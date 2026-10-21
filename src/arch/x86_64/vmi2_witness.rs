// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! VM-INVALIDATION2 — the x86_64 dispatch-gap TLB witness (ACCEPTANCE revision).
//!
//! Built only with `x86-vmi2-witness` and armed only by `yarm.x86_64_vmi2_witness=1`. It reaches
//! the REAL empty-`current`/root-still-loaded window and races a production mapping operation
//! against it, and — this revision — grades the PRODUCTION MAPPING OWNER'S OWN decision for each
//! round, bound to the gap by a shared monotonic sequence rather than by cross-CPU log order.
//!
//! Per round the witness records, from three authorities:
//!
//! * the observer, on its own CPU: the value it read from OBS before the gap (`pre`), the gap's
//!   open/close sequence stamps while its `current` is empty and A's root loaded, and the value it
//!   read after the mutator published the round done (`post`), with a sequence stamp;
//! * the shootdown owner (`complete_unmap_shootdown_from_split`): the ACTUAL target set it computed
//!   and a sequence stamp at that selection — the operation's own decision, never the observer's
//!   separate diagnostic snapshot;
//! * the map transaction (`run_vm_map_transaction`): the mapping actually replaced (old backing),
//!   the backing installed, the completion (acknowledged), and the displaced backing's disposition.
//!
//! The hold is a FIXED, bounded spin inside the yield drain that changes NO interrupt state and
//! never waits on the mutator; it only widens the window so the mutator's replacement overlaps it.
//! The grader (`scripts/grade-vmi2-witness.py`) derives rounds, eligible gap rounds, identities,
//! target inclusion, genuine replacement, required completion and the post-completion bytes from
//! the complete per-round records, and rejects a trusted summary, a missing/duplicated/conflicting
//! record, a refused replacement, stale bytes after completion or a premature reclaim.

#![cfg(feature = "x86-vmi2-witness")]

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::kernel::scheduler::CpuId;
use crate::kernel::vm::{Asid, Mapping, PageFlags, PhysAddr, VirtAddr};
use crate::runtime::SharedKernel;

#[cfg(not(feature = "hosted-dev"))]
core::arch::global_asm!(include_str!("vmi2_witness.S"));

pub const CODE_VA: u64 = 0x2000_0000;
pub const STACK_TOP: u64 = 0x2001_0ff0;
pub const STACK_VA: u64 = 0x2001_0000;
pub const OBS_STK_VA: u64 = 0x2002_0000;
pub const OBS_TLS_VA: u64 = 0x2009_0000;
pub const MBX_VA: u64 = 0x2007_0000;
pub const OBS_VA: u64 = 0x2008_0000;
pub const MUTATOR_TID: u64 = 9_701;
pub const ROUNDS: u64 = 8;
/// The required number of gap-credited rounds for a boot to qualify. Declared here and echoed in
/// the seal; the grader enforces it independently. The remaining rounds must still each finish with
/// a valid, individually graded record — a missing round is a failure, not an uncredited attempt.
pub const GAP_THRESHOLD: u64 = 4;
/// OBS's value before any replace: nonzero, distinct from every per-round sentinel.
pub const OBS_SEED: u64 = 0x5644_4932_5345_4544;
/// Per-round sentinel base (matches `NEWVAL_BASE` in the program); the round is added.
pub const NEWVAL_BASE: u64 = 0x5644_4932_4932_0000;

const M_T1_READY: u64 = 0x00;
const M_MUT_DONE: u64 = 0x08;
const M_T1_POST: u64 = 0x10;
const M_OBS_NEW: u64 = 0x18;
const M_T1_DONE: u64 = 0x20;
const M_T0_DONE: u64 = 0x28;
const M_OBS_TID: u64 = 0x30;
const M_MUT_ROUND: u64 = 0x38;
const M_T1_PRE: u64 = 0x40;

/// The FIXED number of spin iterations the gap-hold keeps the window open, so the mutator's replace
/// on CPU 0 overlaps it. It is a fixed widen, not a wait on the mutator — the hold never blocks on
/// another CPU and touches no interrupt state, so it cannot deadlock and no tick re-enters the
/// drain it is suspended in.
const HOLD_SPIN_BUDGET: u64 = 3_000_000;

const R: usize = ROUNDS as usize;

static ENABLED: AtomicBool = AtomicBool::new(false);
static ASID: AtomicU64 = AtomicU64::new(u64::MAX);
static MBX_PHYS: AtomicU64 = AtomicU64::new(0);
static DUMPED: AtomicBool = AtomicBool::new(false);

/// The shared monotonic order. Every recorded event takes a stamp from it, so causality is bound by
/// stamp comparison, never by the order two CPUs' lines happen to reach the console.
static SEQ: AtomicU64 = AtomicU64::new(1);
fn stamp() -> u64 {
    SEQ.fetch_add(1, Ordering::AcqRel)
}

macro_rules! round_arrays {
    ($($name:ident),+ $(,)?) => {
        $( static $name: [AtomicU64; R] = [const { AtomicU64::new(0) }; R]; )+
    };
}
// Observer-side.
round_arrays!(
    DECLARED, PRE_VAL, GAP_ENTER, GAP_REL, OBS_TARGET, OBS_CR3, POST_VAL, POST_SEQ
);
// Owner-side (the production mapping/shootdown decision).
round_arrays!(
    OWN_SELECT_SEQ,
    OWN_COMPLETE_SEQ,
    OWN_CALLER,
    OWN_CPU,
    OWN_ASID,
    OWN_VA,
    OWN_OLD,
    OWN_NEW,
    OWN_RESULT,
    OWN_TARGET,
    OWN_ACKED,
    OWN_DISP
);
// A per-round single-writer guard: the SECOND owner record for a round marks it conflicting.
static OWN_WRITES: [AtomicU64; R] = [const { AtomicU64::new(0) }; R];
static REFUSED: [AtomicU64; R] = [const { AtomicU64::new(0) }; R];

// OWN_RESULT codes.
const RES_REPLACED: u64 = 1; // installed, displacing an existing mapping (a genuine replacement)
const RES_NO_DISPLACE: u64 = 2; // installed, but nothing was displaced (not a replacement)
// OWN_DISP codes.
const DISP_SETTLED: u64 = 1; // displaced backing reclaimed AFTER an acknowledged shootdown
const DISP_QUARANTINED: u64 = 2; // displaced backing held back (shootdown not acknowledged)

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Release);
}
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

fn witness_asid() -> Option<Asid> {
    match ASID.load(Ordering::Acquire) {
        u64::MAX => None,
        raw => Some(Asid(raw as u16)),
    }
}

#[cfg(not(feature = "hosted-dev"))]
unsafe extern "C" {
    static yarm_vmi2_start: u8;
    static yarm_vmi2_end: u8;
}

fn mbx_read(off: u64) -> u64 {
    let phys = MBX_PHYS.load(Ordering::Acquire);
    if phys == 0 {
        return 0;
    }
    crate::kernel::boot::KernelState::phys_to_direct_map_ptr(phys + off)
        // SAFETY: an aligned u64 inside the witness's own mailbox frame; the direct map covers it.
        .map_or(0, |p| unsafe { core::ptr::read_volatile(p.cast::<u64>()) })
}

#[cfg(not(feature = "hosted-dev"))]
pub fn provision(
    kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    let (asid, _root) = kernel.create_user_address_space()?;

    map_one(kernel, asid, OBS_VA)?;
    map_one(kernel, asid, STACK_VA)?;
    map_one(kernel, asid, OBS_STK_VA)?;
    map_one(kernel, asid, OBS_TLS_VA)?;
    let mbx = map_one(kernel, asid, MBX_VA)?;
    kernel.copy_to_user(asid, VirtAddr(MBX_VA), &[0u8; 0x1000])?;
    kernel.copy_to_user(asid, VirtAddr(OBS_VA), &OBS_SEED.to_le_bytes())?;

    // SAFETY (all `&raw const`): taking an extern static's address reads nothing.
    let start = &raw const yarm_vmi2_start;
    let len = &raw const yarm_vmi2_end as usize - start as usize;
    for i in 0..(len as u64).div_ceil(0x1000) {
        map_one(kernel, asid, CODE_VA + i * 0x1000)?;
    }
    // SAFETY: the program bytes live in this kernel image's text.
    let image = unsafe { core::slice::from_raw_parts(start, len) };
    kernel.copy_to_user(asid, VirtAddr(CODE_VA), image)?;

    kernel.register_task_with_class(MUTATOR_TID, crate::kernel::task::TaskClass::App)?;
    kernel.with_tcbs_mut(|tcbs| {
        let tcb = tcbs
            .iter_mut()
            .flatten()
            .find(|t| t.tid.0 == MUTATOR_TID)
            .ok_or(crate::kernel::boot::KernelError::TaskMissing)?;
        tcb.asid = Some(asid);
        tcb.user_context.instruction_ptr = VirtAddr(CODE_VA);
        tcb.user_context.stack_ptr = VirtAddr(STACK_TOP);
        Ok::<_, crate::kernel::boot::KernelError>(())
    })?;

    ASID.store(u64::from(asid.0), Ordering::Release);
    MBX_PHYS.store(mbx, Ordering::Release);
    ENABLED.store(true, Ordering::Release);
    kernel.enqueue_task(MUTATOR_TID)?;
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMI2_WITNESS_PROVISIONED asid={} boot_tid={} rounds={} gap_threshold={} obs_va=0x{:x} mbx_phys=0x{:x} image={} seed=0x{:x} newval_base=0x{:x}",
        asid.0, MUTATOR_TID, ROUNDS, GAP_THRESHOLD, OBS_VA, mbx, len, OBS_SEED, NEWVAL_BASE
    ));
    Ok(())
}

#[cfg(not(feature = "hosted-dev"))]
fn map_one(
    kernel: &mut crate::kernel::boot::KernelState,
    asid: Asid,
    va: u64,
) -> Result<u64, crate::kernel::boot::KernelError> {
    let phys = kernel.alloc_user_data_frame()?;
    kernel.map_user_page_in_asid_raw(
        asid,
        VirtAddr(va),
        Mapping {
            phys: PhysAddr(phys),
            flags: PageFlags {
                read: true,
                write: true,
                execute: true,
                user: true,
                cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
            },
        },
    )?;
    Ok(phys)
}

fn round_from_ready() -> Option<usize> {
    let round = mbx_read(M_T1_READY);
    if round == 0 || round > ROUNDS {
        return None;
    }
    Some((round - 1) as usize)
}

/// The gap-hold, from the x86 yield drain while `current` is cleared and before the re-selection
/// re-sets it. Records the observer-side gap evidence and widens the window; holds ONCE per round.
#[cfg(not(feature = "hosted-dev"))]
pub fn gap_hold(shared: &SharedKernel, cpu: CpuId, outgoing: u64) {
    if !enabled() {
        return;
    }
    let observer = mbx_read(M_OBS_TID);
    if observer == 0 || outgoing != observer {
        return;
    }
    let Some(asid) = witness_asid() else {
        return;
    };
    if shared.current_tid_split_read(cpu).is_some() {
        return;
    }
    if crate::arch::hal::active_address_space(cpu) != Some(asid) {
        return;
    }
    let Some(idx) = round_from_ready() else {
        return;
    };
    // Hold ONCE per round. The observer reaches this drain not only on its own cooperative NR 0 but
    // also when the LAPIC timer preempts it; without this guard every such preempt would re-open
    // the window for the same round and the observer would never advance through its loop. The
    // first drain entry at an empty `current` for this round records the gap; later ones return.
    // Claim the round's gap exactly once, without overwriting the stamp on a later timer re-entry:
    // a failed compare-exchange means the gap was already recorded for this round.
    if GAP_ENTER[idx]
        .compare_exchange(0, stamp(), Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    DECLARED[idx].store(1, Ordering::Release);
    PRE_VAL[idx].store(mbx_read(M_T1_PRE), Ordering::Release);
    // Observer-side DIAGNOSTIC snapshot of the target set — never the graded authority.
    let diag = shared.live_cpu_bitmap_for_asid_split(asid);
    OBS_TARGET[idx].store(diag, Ordering::Release);
    OBS_CR3[idx].store(
        crate::arch::x86_64::page_table::read_hw_cr3(),
        Ordering::Release,
    );
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMI2_GAP round={} cpu={} asid={} current=empty root=loaded gap_enter={} pre=0x{:x} diag_target=0x{:x}",
        idx + 1,
        cpu.0,
        asid.0,
        GAP_ENTER[idx].load(Ordering::Acquire),
        PRE_VAL[idx].load(Ordering::Acquire),
        diag
    ));
    let mut spins = 0u64;
    while spins < HOLD_SPIN_BUDGET {
        spins += 1;
        core::hint::spin_loop();
    }
    GAP_REL[idx].store(stamp(), Ordering::Release);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMI2_GAP_RELEASED round={} cpu={} gap_release={} spins={}",
        idx + 1,
        cpu.0,
        GAP_REL[idx].load(Ordering::Acquire),
        spins
    ));
}

/// The production shootdown owner's OWN target-set decision, recorded from inside
/// `complete_unmap_shootdown_from_split`. `targets` is the actual set the shootdown will drive
/// (requester already excluded); `virt` is the displaced page. A no-op unless this is the witness
/// ASID and OBS. Stamps the selection order and (on the ACK outcome) the completion order.
#[cfg(not(feature = "hosted-dev"))]
pub fn owner_target_select(requester: CpuId, asid: Asid, virt: VirtAddr, targets: u64) {
    if !enabled() || witness_asid() != Some(asid) || virt.0 != OBS_VA {
        return;
    }
    let round = mbx_read(M_MUT_ROUND);
    if round == 0 || round > ROUNDS {
        return;
    }
    let idx = (round - 1) as usize;
    OWN_SELECT_SEQ[idx].store(stamp(), Ordering::Release);
    OWN_CALLER[idx].store(mbx_read(M_OBS_TID).wrapping_add(0), Ordering::Release);
    let _ = requester;
    OWN_CPU[idx].store(u64::from(requester.0), Ordering::Release);
    OWN_ASID[idx].store(u64::from(asid.0), Ordering::Release);
    OWN_VA[idx].store(virt.0, Ordering::Release);
    OWN_TARGET[idx].store(targets, Ordering::Release);
}

/// The map transaction's displaced-page outcome for the witness OBS: the backing actually replaced,
/// the backing installed, whether the shootdown acknowledged, and the displaced backing's
/// disposition. The authoritative "a genuine replacement occurred and completed" record. A second
/// write for the same round marks it conflicting (the grader fails a conflicting record).
#[cfg(not(feature = "hosted-dev"))]
pub fn owner_displaced(
    asid: Asid,
    virt: VirtAddr,
    old_phys: u64,
    new_phys: u64,
    acked: bool,
    settled: bool,
) {
    if !enabled() || witness_asid() != Some(asid) || virt.0 != OBS_VA {
        return;
    }
    let round = mbx_read(M_MUT_ROUND);
    if round == 0 || round > ROUNDS {
        return;
    }
    let idx = (round - 1) as usize;
    let writes = OWN_WRITES[idx].fetch_add(1, Ordering::AcqRel);
    OWN_OLD[idx].store(old_phys, Ordering::Release);
    OWN_NEW[idx].store(new_phys, Ordering::Release);
    OWN_RESULT[idx].store(RES_REPLACED, Ordering::Release);
    OWN_ACKED[idx].store(u64::from(acked), Ordering::Release);
    OWN_DISP[idx].store(
        if settled {
            DISP_SETTLED
        } else {
            DISP_QUARANTINED
        },
        Ordering::Release,
    );
    OWN_COMPLETE_SEQ[idx].store(stamp(), Ordering::Release);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMI2_OWNER round={} cpu={} asid={} va=0x{:x} old=0x{:x} new=0x{:x} result=replaced target=0x{:x} acked={} disp={} select={} complete={} writes={}",
        round,
        OWN_CPU[idx].load(Ordering::Acquire),
        asid.0,
        virt.0,
        old_phys,
        new_phys,
        OWN_TARGET[idx].load(Ordering::Acquire),
        u8::from(acked),
        if settled { "settled" } else { "quarantined" },
        OWN_SELECT_SEQ[idx].load(Ordering::Acquire),
        OWN_COMPLETE_SEQ[idx].load(Ordering::Acquire),
        writes + 1
    ));
}

/// The map transaction installed OBS but displaced NOTHING — not a replacement. Recorded so the
/// grader can distinguish a genuine replacement from a fresh install that changed no old backing.
#[cfg(not(feature = "hosted-dev"))]
pub fn owner_no_displacement(asid: Asid, virt: VirtAddr, new_phys: u64) {
    if !enabled() || witness_asid() != Some(asid) || virt.0 != OBS_VA {
        return;
    }
    let round = mbx_read(M_MUT_ROUND);
    if round == 0 || round > ROUNDS {
        return;
    }
    let idx = (round - 1) as usize;
    OWN_WRITES[idx].fetch_add(1, Ordering::AcqRel);
    OWN_NEW[idx].store(new_phys, Ordering::Release);
    OWN_RESULT[idx].store(RES_NO_DISPLACE, Ordering::Release);
    OWN_COMPLETE_SEQ[idx].store(stamp(), Ordering::Release);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMI2_OWNER round={} asid={} va=0x{:x} result=no_displacement new=0x{:x}",
        round, asid.0, virt.0, new_phys
    ));
}

/// The DebugLog observation point. Records the observer's post-resume byte per round with a stamp,
/// a refusal the program reports, and — once both tasks are done — dumps.
pub fn observe_user_marker(cpu: CpuId, msg: &str) {
    if !enabled() || !msg.starts_with("VMI2_") {
        return;
    }
    // The round is read from the mailbox, not the marker string: the observer emits VMI2_T1_POST
    // while M_T1_READY still names its current round (it increments only afterwards), and the
    // mutator emits VMI2_MUT_REFUSED while M_MUT_ROUND names the refused round.
    if msg.starts_with("VMI2_T1_POST") {
        if let Some(idx) = round_from_ready() {
            POST_VAL[idx].store(mbx_read(M_T1_POST), Ordering::Release);
            POST_SEQ[idx].store(stamp(), Ordering::Release);
            crate::kernel::printk::printk_emit_sync(format_args!(
                "VMI2_POST round={} cpu={} post=0x{:x} post_seq={}",
                idx + 1,
                cpu.0,
                POST_VAL[idx].load(Ordering::Acquire),
                POST_SEQ[idx].load(Ordering::Acquire)
            ));
        }
        return;
    }
    if msg.starts_with("VMI2_MUT_REFUSED") {
        let round = mbx_read(M_MUT_ROUND);
        if round >= 1 && round <= ROUNDS {
            let idx = (round - 1) as usize;
            REFUSED[idx].store(1, Ordering::Release);
            crate::kernel::printk::printk_emit_sync(format_args!(
                "VMI2_REFUSED round={} cpu={}",
                round, cpu.0
            ));
        }
        return;
    }
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMI2_USER cpu={} {}",
        cpu.0,
        msg.trim_end()
    ));
    let both_done = mbx_read(M_T1_DONE) != 0 && mbx_read(M_T0_DONE) != 0;
    if both_done && !DUMPED.swap(true, Ordering::AcqRel) {
        dump();
    }
}

fn dump() {
    // The console truncates a line at PRB_MSG_MAX, so a whole-record line cannot carry its CRC.
    // Emit ONE SHORT line per field instead — `VMI2_F r=<round> k=<key> v=<u64> crc=<8hex>` — each
    // twice. The grader reassembles a round from its CRC-valid field lines: a torn copy fails its
    // CRC and is dropped, a missing field fails the round, and two CRC-valid copies of one field
    // that disagree fail as conflicting. The seal that follows is a cross-check, never the source
    // of truth.
    use core::fmt::Write;
    for i in 0..R {
        let fields = [
            ("declared", DECLARED[i].load(Ordering::Acquire)),
            ("pre", PRE_VAL[i].load(Ordering::Acquire)),
            ("gap_enter", GAP_ENTER[i].load(Ordering::Acquire)),
            ("gap_release", GAP_REL[i].load(Ordering::Acquire)),
            ("diag_target", OBS_TARGET[i].load(Ordering::Acquire)),
            ("own_cpu", OWN_CPU[i].load(Ordering::Acquire)),
            ("own_asid", OWN_ASID[i].load(Ordering::Acquire)),
            ("own_va", OWN_VA[i].load(Ordering::Acquire)),
            ("own_old", OWN_OLD[i].load(Ordering::Acquire)),
            ("own_new", OWN_NEW[i].load(Ordering::Acquire)),
            ("own_result", OWN_RESULT[i].load(Ordering::Acquire)),
            ("own_target", OWN_TARGET[i].load(Ordering::Acquire)),
            ("own_acked", OWN_ACKED[i].load(Ordering::Acquire)),
            ("own_disp", OWN_DISP[i].load(Ordering::Acquire)),
            ("own_select", OWN_SELECT_SEQ[i].load(Ordering::Acquire)),
            ("own_complete", OWN_COMPLETE_SEQ[i].load(Ordering::Acquire)),
            ("own_writes", OWN_WRITES[i].load(Ordering::Acquire)),
            ("refused", REFUSED[i].load(Ordering::Acquire)),
            ("post", POST_VAL[i].load(Ordering::Acquire)),
            ("post_seq", POST_SEQ[i].load(Ordering::Acquire)),
        ];
        for (k, v) in fields {
            let mut payload = RecLine::new();
            let _ = write!(payload, "r={} k={k} v={v}", i + 1);
            let crc = crc32(&payload.buf[..payload.len]);
            for _ in 0..2 {
                crate::kernel::printk::printk_emit_sync(format_args!(
                    "VMI2_F {} crc={:08x}",
                    payload.as_str(),
                    crc
                ));
            }
        }
    }
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMI2_WITNESS_SEAL arch=x86_64 rounds={} gap_threshold={} seed=0x{:x} newval_base=0x{:x} obs_va=0x{:x}",
        ROUNDS, GAP_THRESHOLD, OBS_SEED, NEWVAL_BASE, OBS_VA
    ));
    crate::kernel::printk::printk_emit_sync(format_args!("VMI2_DUMP_END"));
}

struct RecLine {
    buf: [u8; 1024],
    len: usize,
}
impl RecLine {
    fn new() -> Self {
        Self {
            buf: [0; 1024],
            len: 0,
        }
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}
impl core::fmt::Write for RecLine {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let m = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & m);
        }
    }
    !crc
}
