// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! AP-SCHED1 — the RISC-V production AP-scheduling witness. Built only with
//! `riscv64-apsched-witness`, and armed only WITHOUT `yarm.ap_user_dispatch=1`: it exercises the
//! production admission of logical CPU 1, never the diagnostic pinned one.
//!
//! It provisions ONE App-class process P (`apsched_witness.S`) and nothing else. P's workers are
//! threads it spawns itself (NR 11) and children it forks (NR 12); the production balanced
//! placement decides where each runs and fixes that CPU as its home. The witness pins nothing and
//! fixes no identity but P's.
//!
//! Its kernel actions are, all labelled:
//!
//! * setup (P's space, pages, three buffered endpoints, P's capabilities) and one ordinary enqueue
//!   of P, at boot, when CPU 0 is the only CPU placement can choose;
//! * PUBLICATION of observation words into P's mailbox frame through the direct map, with no lock
//!   held: CPU 1's admission, this process's timer preemptions per CPU, CPU 1's idle arrivals and
//!   the last reaped child — so P can wait for a fact instead of guessing with a delay;
//! * the REAP of each churn child, through the production NR 31 transaction on the CPU P's
//!   request trapped on. Production's only reaper is the process manager, for supervisor restarts;
//!   the witness stands in for it so an App process's exit and reap can be exercised at all.
//!
//! Every record carries one causal stamp from a single counter, so the grader orders events by
//! stamp and never by console position (the two harts' output interleaves).

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::capabilities::{CapId, CapRights};
use crate::kernel::scheduler::{CpuId, MAX_CPUS};
use crate::kernel::vm::{Asid, CachePolicy, Mapping, PageFlags, PhysAddr, VirtAddr};
use crate::runtime::SharedKernel;

core::arch::global_asm!(include_str!("apsched_witness.S"));

pub const CODE_VA: u64 = 0x2000_0000;
pub const P_STACK_VA: u64 = 0x2001_0000;
pub const P_SP_TOP: u64 = 0x2001_0ff0;
pub const WSTK_VA: u64 = 0x2002_0000;
pub const WSTK_SLOTS: u64 = 8;
pub const MBX_VA: u64 = 0x2007_0000;
pub const BUF_VA: u64 = 0x2008_0000;
pub const P_TID: u64 = 9_600;

const M_AP_READY: u64 = 0x000;
const M_PRE0: u64 = 0x020;
const M_PRE1: u64 = 0x028;
const M_IDLE1: u64 = 0x030;
const M_REAPED: u64 = 0x038;
const M_CPU1_IDLE: u64 = 0x070;

const MAX_CHILDREN: usize = 16;

unsafe extern "C" {
    static yarm_apsw_start: u8;
    static yarm_apsw_end: u8;
    static yarm_apsw_cap_ep_p: u8;
    static yarm_apsw_cap_ep_t: u8;
    static yarm_apsw_cap_ep_d: u8;
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static ASID: AtomicU64 = AtomicU64::new(0);
static MBX_PHYS: AtomicU64 = AtomicU64::new(0);
static SEQ: AtomicU64 = AtomicU64::new(0);
/// Accepted timer interrupts per CPU, counted at the timer settlement's entry — before, and
/// independently of, whatever the scheduler does with them.
static IRQS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static PRE: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static IDLE1: AtomicU64 = AtomicU64::new(0);
static CHILDREN: [AtomicU64; MAX_CHILDREN] = [const { AtomicU64::new(0) }; MAX_CHILDREN];
static REAPED: [AtomicBool; MAX_CHILDREN] = [const { AtomicBool::new(false) }; MAX_CHILDREN];
static REFUSALS: AtomicU64 = AtomicU64::new(0);
/// The deadline window: (tick, CPU 0 interrupts, CPU 1 interrupts) at D_BEFORE.
static D_TICK: AtomicU64 = AtomicU64::new(u64::MAX);
static D_IRQ0: AtomicU64 = AtomicU64::new(0);
static D_IRQ1: AtomicU64 = AtomicU64::new(0);
static ENDED: AtomicBool = AtomicBool::new(false);

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

fn stamp() -> u64 {
    SEQ.fetch_add(1, Ordering::AcqRel) + 1
}

fn witness_asid() -> u64 {
    ASID.load(Ordering::Acquire)
}

/// Is `tid` P or one of P's threads (they share P's space)?
fn is_process_task(shared: &SharedKernel, tid: u64) -> bool {
    tid != 0 && shared.task_asid_for_tid_split_read(tid) == witness_asid()
}

fn child_slot(tid: u64) -> Option<usize> {
    (0..MAX_CHILDREN).find(|&i| CHILDREN[i].load(Ordering::Acquire) == tid)
}

fn is_witness_task(shared: &SharedKernel, tid: u64) -> bool {
    is_process_task(shared, tid) || (tid != 0 && child_slot(tid).is_some())
}

/// WITNESS PUBLICATION — store one observation word into P's mailbox frame through the direct
/// map. No lock is taken or held; P only ever reads these words.
fn publish(off: u64, value: u64) {
    let phys = MBX_PHYS.load(Ordering::Acquire);
    if phys == 0 {
        return;
    }
    if let Some(ptr) = KernelState::phys_to_direct_map_ptr(phys + off) {
        // SAFETY: an aligned u64 inside the mailbox frame this witness allocated; the direct map
        // covers it for the life of the kernel.
        unsafe { core::ptr::write_volatile(ptr.cast::<u64>(), value) };
    }
}

fn map_fresh(
    kernel: &mut KernelState,
    asid: Asid,
    va: u64,
    count: u64,
) -> Result<u64, KernelError> {
    let mut first = 0;
    for i in 0..count {
        let phys = kernel.alloc_user_data_frame()?;
        if i == 0 {
            first = phys;
        }
        kernel.map_user_page_in_asid_raw(
            asid,
            VirtAddr(va + i * 0x1000),
            Mapping {
                phys: PhysAddr(phys),
                flags: PageFlags {
                    read: true,
                    write: true,
                    execute: true,
                    user: true,
                    cache_policy: CachePolicy::WriteBack,
                },
            },
        )?;
    }
    Ok(first)
}

fn cap32(c: CapId) -> Result<u32, KernelError> {
    u32::try_from(c.0).map_err(|_| KernelError::CapabilityFull)
}

/// Provision P from the bootstrap window (`&mut KernelState`): its space, its pages, its three
/// endpoints, and ONE ordinary enqueue — at boot, CPU 0 is the only CPU placement can choose.
pub fn provision(kernel: &mut KernelState) -> Result<(), KernelError> {
    let (asid, _as_root) = kernel.create_user_address_space()?;
    // SAFETY (all `&raw const` below): taking the address of an extern static reads nothing.
    let start = &raw const yarm_apsw_start;
    let len = &raw const yarm_apsw_end as usize - start as usize;
    map_fresh(kernel, asid, CODE_VA, (len as u64).div_ceil(0x1000))?;
    map_fresh(kernel, asid, P_STACK_VA, 1)?;
    map_fresh(kernel, asid, WSTK_VA, WSTK_SLOTS)?;
    let mbx = map_fresh(kernel, asid, MBX_VA, 1)?;
    map_fresh(kernel, asid, BUF_VA, 1)?;
    kernel.copy_to_user(asid, VirtAddr(MBX_VA), &[0u8; 0x1000])?;
    kernel.copy_to_user(asid, VirtAddr(BUF_VA), &[0u8; 0x1000])?;

    kernel.register_task_with_class(P_TID, crate::kernel::task::TaskClass::App)?;
    kernel.with_tcbs_mut(|tcbs| {
        let tcb = tcbs
            .iter_mut()
            .flatten()
            .find(|t| t.tid.0 == P_TID)
            .ok_or(KernelError::TaskMissing)?;
        tcb.asid = Some(asid);
        tcb.user_context.instruction_ptr = VirtAddr(CODE_VA);
        tcb.user_context.stack_ptr = VirtAddr(P_SP_TOP);
        Ok::<_, KernelError>(())
    })?;

    // Three buffered endpoints, each minted into P's own CNode with both rights: P's threads share
    // it.
    let rights = CapRights::SEND | CapRights::RECEIVE;
    let grant = |k: &mut KernelState| -> Result<u32, KernelError> {
        let (_, _, cap) = k.create_endpoint(1)?;
        let object = k
            .current_task_capability(cap)
            .ok_or(KernelError::InvalidCapability)?
            .object;
        let cnode = k.task_cnode(P_TID).ok_or(KernelError::TaskMissing)?;
        k.mint_capability_in_cnode(
            cnode,
            crate::kernel::capabilities::Capability::new(object, rights),
        )
        .and_then(cap32)
    };
    let ep_p = grant(kernel)?;
    let ep_t = grant(kernel)?;
    let ep_d = grant(kernel)?;

    // SAFETY: `start..start + len` is the program's bytes inside this kernel image's text.
    let mut image = unsafe { core::slice::from_raw_parts(start, len) }.to_vec();
    for (at, value) in [
        (&raw const yarm_apsw_cap_ep_p, ep_p),
        (&raw const yarm_apsw_cap_ep_t, ep_t),
        (&raw const yarm_apsw_cap_ep_d, ep_d),
    ] {
        let off = at as usize - start as usize;
        image[off..off + 4].copy_from_slice(&value.to_le_bytes());
    }
    kernel.copy_to_user(asid, VirtAddr(CODE_VA), &image)?;

    ASID.store(u64::from(asid.0), Ordering::Release);
    MBX_PHYS.store(mbx, Ordering::Release);
    ENABLED.store(true, Ordering::Release);
    kernel.enqueue_task(P_TID)?;
    crate::kernel::printk::printk_emit_sync(format_args!(
        "APSW_PROVISIONED p_tid={} asid={} image={} ep_p={} ep_t={} ep_d={} placement=balanced pinned=0",
        P_TID,
        asid.0,
        image.len(),
        ep_p,
        ep_t,
        ep_d
    ));
    Ok(())
}

// ─────────────────────────────── the record ───────────────────────────────

/// The record is kept, not printed as it happens: the two harts' console output interleaves
/// character by character, and a record torn by the other hart's marker would be lost evidence.
/// P's last step dumps it once, every line twice with a CRC, while nothing else of the witness
/// runs; the grader keeps exactly the lines whose CRC verifies.
const MAX_RECS: usize = 512;

const K_AP_ADMITTED: u64 = 1;
const K_PREEMPT: u64 = 2;
const K_IDLE: u64 = 3;
const K_IDLE_IPI: u64 = 4;
const K_IDLE_DISPATCH: u64 = 5;
const K_USER: u64 = 6;
const K_DEADLINE: u64 = 7;
const K_REAP: u64 = 8;
const K_REAP_GONE: u64 = 9;
const K_ROOT_LEFT: u64 = 10;

const KINDS: [&str; 11] = [
    "NONE",
    "AP_ADMITTED",
    "PREEMPT",
    "IDLE",
    "IDLE_IPI",
    "IDLE_DISPATCH",
    "USER",
    "DEADLINE",
    "REAP",
    "REAP_GONE",
    "ROOT_LEFT",
];

/// The user steps, by code (index + 1); 0 = a step this table does not know.
const STEPS: [&str; 30] = [
    "P_START",
    "P_NO_AP",
    "P_AP_READY",
    "P_PRE_TIMEOUT",
    "P_A_TIMEOUT",
    "P_A_DONE",
    "P_B_TIMEOUT",
    "P_NO_IDLE",
    "P_WAKE",
    "P_NO_ACK",
    "P_ACKED",
    "P_PING",
    "P_PING_RAW",
    "P_C_TIMEOUT",
    "P_FORKED",
    "P_REAP_TIMEOUT",
    "P_END",
    "P_SPAWN_FAIL",
    "P_FORK_FAIL",
    "C_RUN",
    "D_BEFORE",
    "W_START",
    "W_DONE",
    "S_START",
    "S_WOKE",
    "T_START",
    "T_PONG",
    "EXIT_REFUSED",
    "D_AFTER",
    "P_REAP",
];

fn step_code(step: &str) -> u64 {
    STEPS
        .iter()
        .position(|s| *s == step)
        .map_or(0, |i| i as u64 + 1)
}

struct Rec {
    valid: AtomicBool,
    kind: AtomicU64,
    cpu: AtomicU64,
    tid: AtomicU64,
    step: AtomicU64,
    v: [AtomicU64; 5],
}

static RECS: [Rec; MAX_RECS] = [const {
    Rec {
        valid: AtomicBool::new(false),
        kind: AtomicU64::new(0),
        cpu: AtomicU64::new(0),
        tid: AtomicU64::new(0),
        step: AtomicU64::new(0),
        v: [const { AtomicU64::new(0) }; 5],
    }
}; MAX_RECS];
static OVERFLOW: AtomicBool = AtomicBool::new(false);

/// Append one record under the next causal stamp. Lock-free: each stamp owns one slot.
fn record(kind: u64, cpu: u64, tid: u64, step: u64, v: [u64; 5]) {
    let seq = stamp();
    let Some(r) = RECS.get(seq as usize - 1) else {
        OVERFLOW.store(true, Ordering::Release);
        return;
    };
    r.kind.store(kind, Ordering::Relaxed);
    r.cpu.store(cpu, Ordering::Relaxed);
    r.tid.store(tid, Ordering::Relaxed);
    r.step.store(step, Ordering::Relaxed);
    for (slot, value) in r.v.iter().zip(v) {
        slot.store(value, Ordering::Relaxed);
    }
    r.valid.store(true, Ordering::Release);
}

/// CPU `cpu` completed its production admission (called last, by that CPU).
pub fn ap_admitted(cpu: CpuId) {
    if !enabled() || cpu.0 != 1 {
        return;
    }
    record(K_AP_ADMITTED, 1, 0, 0, [0; 5]);
    publish(M_AP_READY, 1);
}

/// Every accepted timer interrupt, at the timer settlement's entry.
pub fn note_timer_irq(cpu: CpuId) {
    if !enabled() {
        return;
    }
    if let Some(n) = IRQS.get(cpu.0 as usize) {
        n.fetch_add(1, Ordering::AcqRel);
    }
}

/// A timer preemption the settlement COMMITTED, with its outgoing task.
pub fn note_preempt(shared: &SharedKernel, cpu: CpuId, outgoing: u64) {
    // P itself waits in yield loops, so it is running at most ticks; only its workers count.
    if !enabled() || cpu.0 > 1 || outgoing == P_TID || !is_witness_task(shared, outgoing) {
        return;
    }
    let n = PRE[cpu.0 as usize].fetch_add(1, Ordering::AcqRel) + 1;
    record(K_PREEMPT, u64::from(cpu.0), outgoing, 0, [n, 0, 0, 0, 0]);
    publish(if cpu.0 == 0 { M_PRE0 } else { M_PRE1 }, n);
}

/// `cpu` arrived at its idle wait from a task (not a return to an interrupted idle).
pub fn note_idle_reached(cpu: usize) {
    if !enabled() || cpu != 1 {
        return;
    }
    let n = IDLE1.fetch_add(1, Ordering::AcqRel) + 1;
    record(K_IDLE, cpu as u64, 0, 0, [n, 0, 0, 0, 0]);
    publish(M_IDLE1, n);
    publish(M_CPU1_IDLE, 1);
}

/// `cpu` left a user root for the kernel's own space (`hal::leave_user_root`), after the hardware
/// change and the record release. CPU 1's only: every task placed there in this boot is the
/// witness's (the core services are bootstrap-pinned), and a child can run and exit before P has
/// even reported forking it, so the grader matches the root against the child's own step.
///
/// Recorded from what the transition LEFT BEHIND, not from the fact that it was called: the live
/// `satp` (must be Bare, 0) and the activation record (must name no space). A transition that did
/// not happen records nothing.
pub fn note_root_left(cpu: CpuId, leaving: Option<crate::kernel::vm::Asid>) {
    if !enabled() || cpu.0 != 1 {
        return;
    }
    let satp: usize;
    // SAFETY: a CSR read.
    unsafe {
        core::arch::asm!("csrr {0}, satp", out(reg) satp, options(nomem, nostack, preserves_flags))
    };
    if satp != 0 || crate::arch::hal::active_address_space(cpu).is_some() {
        return;
    }
    if let Some(asid) = leaving {
        record(
            K_ROOT_LEFT,
            1,
            0,
            0,
            [u64::from(asid.0), satp as u64, 0, 0, 0],
        );
    }
}

/// A reschedule IPI taken at `cpu`'s idle boundary, with the sources its consumption swapped out.
pub fn note_idle_ipi(cpu: CpuId, sources: u64) {
    if !enabled() {
        return;
    }
    record(K_IDLE_IPI, u64::from(cpu.0), 0, 0, [sources, 0, 0, 0, 0]);
}

/// `cpu` leaves idle to run `tid`, by the trigger that woke it.
pub fn note_idle_dispatch(shared: &SharedKernel, cpu: CpuId, tid: u64, by_ipi: bool) {
    if !enabled() {
        return;
    }
    if cpu.0 == 1 {
        publish(M_CPU1_IDLE, 0);
    }
    if !is_witness_task(shared, tid) {
        return;
    }
    record(
        K_IDLE_DISPATCH,
        u64::from(cpu.0),
        tid,
        0,
        [u64::from(by_ipi), 0, 0, 0, 0],
    );
}

/// The DebugLog observation point. Returns `true` when the message was this witness's.
pub fn observe_user_marker(
    shared: &SharedKernel,
    cpu: CpuId,
    tid: u64,
    asid: u64,
    msg: &str,
    a2: u64,
    a3: u64,
) -> bool {
    if !enabled() || !msg.starts_with("APS ") {
        return false;
    }
    let step = msg.trim_end().trim_start_matches("APS ");
    let c = u64::from(cpu.0);
    match step {
        "P_FORKED" => {
            // A churn child: the reaper accepts only TIDs P reported here.
            let _ = (0..MAX_CHILDREN).find(|&i| {
                CHILDREN[i]
                    .compare_exchange(0, a2, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            });
        }
        // Polled: only the outcome is recorded, not every attempt.
        "P_REAP" => {
            reap(shared, cpu, a2, a3);
            return true;
        }
        "D_BEFORE" => {
            D_TICK.store(shared.scheduler_tick_now_split_read(), Ordering::Release);
            D_IRQ0.store(IRQS[0].load(Ordering::Acquire), Ordering::Release);
            D_IRQ1.store(IRQS[1].load(Ordering::Acquire), Ordering::Release);
        }
        "D_AFTER" => {
            let tick = shared.scheduler_tick_now_split_read();
            record(
                K_DEADLINE,
                c,
                tid,
                0,
                [
                    a2,
                    a3,
                    tick.wrapping_sub(D_TICK.load(Ordering::Acquire)),
                    IRQS[0].load(Ordering::Acquire) - D_IRQ0.load(Ordering::Acquire),
                    IRQS[1].load(Ordering::Acquire) - D_IRQ1.load(Ordering::Acquire),
                ],
            );
            return true;
        }
        _ => {}
    }
    record(K_USER, c, tid, step_code(step), [a2, a3, asid, 0, 0]);
    if step == "P_END" && !ENDED.swap(true, Ordering::AcqRel) {
        dump();
    }
    true
}

/// WITNESS REAPER — the churn child's reap, through the production NR 31 transaction on the CPU
/// P's request trapped on. A child that has not exited yet is refused by the transaction itself
/// (`NonTerminal` / `StillScheduled`) having mutated nothing; P asks again.
fn reap(shared: &SharedKernel, cpu: CpuId, target: u64, round: u64) {
    let Some(slot) = child_slot(target) else {
        return;
    };
    if REAPED[slot].load(Ordering::Acquire) {
        return;
    }
    let mut owners = crate::kernel::syscall::reap_txn::SharedReapOwners { shared, cpu };
    match crate::kernel::syscall::reap_txn::run_reap_transaction(&mut owners, target) {
        Ok(outcome) => {
            REAPED[slot].store(true, Ordering::Release);
            record(
                K_REAP,
                u64::from(cpu.0),
                target,
                0,
                [
                    round,
                    u64::from(outcome.process_reaped),
                    outcome.address_spaces_destroyed as u64,
                    outcome.address_spaces_quarantined as u64,
                    0,
                ],
            );
            publish(M_REAPED, target);
        }
        Err(refusal) if refusal.is_already_reaped() => {
            REAPED[slot].store(true, Ordering::Release);
            record(
                K_REAP_GONE,
                u64::from(cpu.0),
                target,
                0,
                [round, 0, 0, 0, 0],
            );
            publish(M_REAPED, target);
        }
        Err(_) => {
            REFUSALS.fetch_add(1, Ordering::AcqRel);
        }
    }
}

// ─────────────────────────────── the dump ───────────────────────────────

struct Line {
    buf: [u8; 256],
    len: usize,
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let b = s.as_bytes();
        let end = self.len + b.len();
        if end > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(b);
        self.len = end;
        Ok(())
    }
}

/// IEEE CRC-32 (the one `zlib.crc32` computes), bitwise.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Print one line twice, each copy carrying the CRC of its content.
fn emit_twice(args: core::fmt::Arguments<'_>) {
    let mut line = Line {
        buf: [0; 256],
        len: 0,
    };
    if core::fmt::write(&mut line, args).is_err() {
        return;
    }
    let body = core::str::from_utf8(&line.buf[..line.len]).unwrap_or("APSW_R torn");
    let crc = crc32(body.as_bytes());
    for _ in 0..2 {
        crate::kernel::printk::printk_emit_sync(format_args!("{} crc={:08x}", body, crc));
    }
}

/// The sealed dump, at P's last step: every record by stamp, then the totals.
fn dump() {
    let n = (SEQ.load(Ordering::Acquire) as usize).min(MAX_RECS);
    for (i, r) in RECS.iter().take(n).enumerate() {
        // A record stamped on the other CPU may still be mid-write: bounded wait for its flag.
        let mut spins = 0u32;
        while !r.valid.load(Ordering::Acquire) && spins < 1_000_000 {
            core::hint::spin_loop();
            spins += 1;
        }
        let kind = r.kind.load(Ordering::Relaxed) as usize;
        let step = r.step.load(Ordering::Relaxed) as usize;
        let v = |j: usize| r.v[j].load(Ordering::Relaxed);
        emit_twice(format_args!(
            "APSW_R seq={} kind={} cpu={} tid={} step={} v0={} v1={} v2={} v3={} v4={}",
            i + 1,
            KINDS.get(kind).copied().unwrap_or("UNKNOWN"),
            r.cpu.load(Ordering::Relaxed),
            r.tid.load(Ordering::Relaxed),
            if step == 0 {
                "-"
            } else {
                STEPS.get(step - 1).copied().unwrap_or("?")
            },
            v(0),
            v(1),
            v(2),
            v(3),
            v(4)
        ));
    }
    emit_twice(format_args!(
        "APSW_DUMP_END count={} overflow={} irq0={} irq1={} pre0={} pre1={} idle1={} reap_retries={}",
        n,
        u8::from(OVERFLOW.load(Ordering::Acquire)),
        IRQS[0].load(Ordering::Acquire),
        IRQS[1].load(Ordering::Acquire),
        PRE[0].load(Ordering::Acquire),
        PRE[1].load(Ordering::Acquire),
        IDLE1.load(Ordering::Acquire),
        REFUSALS.load(Ordering::Acquire)
    ));
}
