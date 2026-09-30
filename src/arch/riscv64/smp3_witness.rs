// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP3 — the RISC-V two-hart IPI / remote-fence / context witness. Built only with
//! `riscv64-smp3-witness` and armed only by `yarm.ap_user_dispatch=1` on a two-hart boot.
//!
//! It drives PRODUCTION owners and records what they did; it performs none of the work it grades:
//!
//! * every wake IPI is the one the direct NR6/NR7 drains send after their enqueue commits; every
//!   consumption and IPI-driven dispatch is the bridge entry owner's and the idle advance's own;
//! * every W replacement is a genuine NR 3 issued by the OTHER task, so the install, the local
//!   fence, the firmware's remote fence, the displaced pin and the settlement are the VM, page-table
//!   and IPI owners';
//! * context is checked by the user programs themselves (`smp3_witness.S`).
//!
//! Its own kernel actions are setup, the secondary's one start-up kick (it has no timer, so a task
//! placed on it outside a trap needs one interrupt to be dispatched), and two labelled witness
//! synchronizations (`wait_until_parked`, `mutual_rendezvous`), both at points where no lock is
//! held and neither waiting for anything the other hart needs from this one.
//!
//! # Residency, on this port
//!
//! QEMU flushes a hart's whole TLB on every `SFENCE.VMA` and every `satp` write, and this kernel
//! writes `satp` (and fences) on every return to U-mode. So an AArch64-style probe (re-point a page
//! without invalidation and see whether the target still reads the old frame) cannot separate the
//! tested remote fence from an incidental flush here. What does separate them is the target hart's
//! SUPERVISOR-ENTRY COUNT: the target primes W after its last kernel entry, and a round is
//! credited only when its next entry — the observation step — is exactly the one after the priming
//! step. Then no kernel code, and so no `satp` write or local fence, ran on that hart in between,
//! and the only thing that can have retired its translation of W is the firmware fence the
//! requester's shootdown asked for. A round with other entries in the window (the boot hart's
//! periodic timer, typically) is recorded as interfered and not credited.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::kernel::boot::smp3_record::{self as rec, Kind, Role, Roles};
use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::capabilities::{CapId, CapRights};
use crate::kernel::scheduler::{CpuId, MAX_CPUS};
use crate::kernel::vm::{Asid, CachePolicy, Mapping, PageFlags, PhysAddr, VirtAddr};

core::arch::global_asm!(include_str!("smp3_witness.S"));

pub const CODE_VA: u64 = 0x2000_0000;
pub const STACK_VA: u64 = 0x2001_0000;
pub const SP_TOP: u64 = 0x2001_0ff0;
pub const PAYLOAD_VA: u64 = 0x2003_0000;
pub const META_VA: u64 = 0x2004_0000;
pub const REPLY_SRC_VA: u64 = 0x2005_0000;
pub const PAT_VA: u64 = 0x2006_0000;
pub const MBX_VA: u64 = 0x2007_0000;
pub const W_VA: u64 = 0x2008_0000;

pub const S_TID: u64 = 9_300;
pub const C_TID: u64 = 9_301;
pub const H1_TID: u64 = 9_302;
pub const H0_TID: u64 = 9_303;

/// The GPR pattern base per context-checked task: `xN` is loaded with `base + N`.
const S_BASE: u64 = 0xA1A1_0000_0000_0000;
const C_BASE: u64 = 0xB2B2_0000_0000_0000;

const REQUEST: &[u8] = &[0x99, 0x01, b'N', b'R', b'6', b'-', b'R', b'E', b'Q', b'!'];
const REPLY: &[u8] = b"RPLY-OK!";

unsafe extern "C" {
    static yarm_smp3_s_start: u8;
    static yarm_smp3_s_end: u8;
    static yarm_smp3_s_cap_es: u8;
    static yarm_smp3_s_cap_eh0: u8;
    static yarm_smp3_s_cap_rs: u8;
    static yarm_smp3_s_cap_as: u8;
    static yarm_smp3_s_win_a_begin: u8;
    static yarm_smp3_s_win_a_end: u8;
    static yarm_smp3_s_tlb_win_begin: u8;
    static yarm_smp3_s_tlb_win_end: u8;
    static yarm_smp3_c_start: u8;
    static yarm_smp3_c_end: u8;
    static yarm_smp3_c_cap_es: u8;
    static yarm_smp3_c_cap_eh1: u8;
    static yarm_smp3_c_cap_rc: u8;
    static yarm_smp3_c_cap_as: u8;
    static yarm_smp3_c_win_b_begin: u8;
    static yarm_smp3_c_win_b_end: u8;
    static yarm_smp3_c_tlb_win_begin: u8;
    static yarm_smp3_c_tlb_win_end: u8;
    static yarm_smp3_h_start: u8;
    static yarm_smp3_h_end: u8;
    static yarm_smp3_h_cap: u8;
}

/// The byte image of one program with every capability literal patched.
fn patched(start: *const u8, end: *const u8, patches: &[(*const u8, u32)]) -> alloc::vec::Vec<u8> {
    let len = end as usize - start as usize;
    // SAFETY: `start..end` is the program's bytes inside this kernel image's text.
    let mut image = unsafe { core::slice::from_raw_parts(start, len) }.to_vec();
    for &(at, value) in patches {
        let off = at as usize - start as usize;
        image[off..off + 4].copy_from_slice(&value.to_le_bytes());
    }
    image
}

/// The user VA of a program label.
fn user_va(start: *const u8, label: *const u8) -> u64 {
    CODE_VA + (label as u64 - start as u64)
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static S_ASID: AtomicU64 = AtomicU64::new(0);
static C_ASID: AtomicU64 = AtomicU64::new(0);
static H1_ASID: AtomicU64 = AtomicU64::new(0);
static H0_ASID: AtomicU64 = AtomicU64::new(0);
static S_DONE: AtomicBool = AtomicBool::new(false);
static C_DONE: AtomicBool = AtomicBool::new(false);
static DUMPED: AtomicBool = AtomicBool::new(false);
static CPU1_STARTED: AtomicBool = AtomicBool::new(false);
/// The secondary's admitted state and its placement, for the sealed `SMP3_BRINGUP` line.
static BRINGUP_SIE: AtomicU64 = AtomicU64::new(u64::MAX);
static BRINGUP_SSTATUS: AtomicU64 = AtomicU64::new(u64::MAX);
static BRINGUP_PLACED: AtomicU64 = AtomicU64::new(0);
const PLACED_KICKED: u64 = 1;
const PLACED_KICK_REFUSED: u64 = 2;
const PLACEMENT_FAILED: u64 = 3;
/// Supervisor entries per CPU — every trap the bridge takes on that hart, from either origin.
static ENTRIES: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// Whether each hart is in its idle wait loop now (set there, cleared by any supervisor entry).
static AT_IDLE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

/// The VM and fence witness records are confined to the one page the witness replaces.
pub fn watches(va: u64) -> bool {
    enabled() && va == W_VA
}

fn hart(cpu: u8) -> u64 {
    crate::arch::riscv64::boot::hart_id_of_logical_cpu(cpu as usize).map_or(u64::MAX, |h| h as u64)
}

fn roles() -> Roles {
    let role = |tid, asid: &AtomicU64, cpu: u8| Role {
        tid,
        asid: asid.load(Ordering::Acquire),
        cpu,
        hart: hart(cpu),
    };
    Roles {
        s: role(S_TID, &S_ASID, 1),
        c: role(C_TID, &C_ASID, 0),
        h1: role(H1_TID, &H1_ASID, 1),
        h0: role(H0_TID, &H0_ASID, 0),
        w_va: W_VA,
    }
}

fn this_cpu() -> u8 {
    crate::arch::riscv64::boot::riscv_current_logical_cpu().0
}

/// Map `count` fresh frames from `va` with `flags`; returns the first frame.
fn map_fresh(
    kernel: &mut KernelState,
    asid: Asid,
    va: u64,
    count: usize,
    flags: PageFlags,
) -> Result<u64, KernelError> {
    let mut first = 0;
    for i in 0..count {
        let phys = kernel.alloc_user_data_frame()?;
        if i == 0 {
            first = phys;
        }
        kernel.map_user_page_in_asid_raw(
            asid,
            VirtAddr(va + (i as u64) * 0x1000),
            Mapping {
                phys: PhysAddr(phys),
                flags,
            },
        )?;
    }
    Ok(first)
}

const CODE_FLAGS: PageFlags = PageFlags {
    read: true,
    write: true,
    execute: true,
    user: true,
    cache_policy: CachePolicy::WriteBack,
};

/// One task: its own address space, code, stack, receive/metadata/reply pages, TCB and home.
fn build_task(
    kernel: &mut KernelState,
    tid: u64,
    image_len: usize,
    home: CpuId,
) -> Result<(Asid, CapId), KernelError> {
    let (asid, as_root) = kernel.create_user_address_space()?;
    map_fresh(
        kernel,
        asid,
        CODE_VA,
        image_len.div_ceil(0x1000),
        CODE_FLAGS,
    )?;
    for va in [STACK_VA, PAYLOAD_VA, META_VA, REPLY_SRC_VA] {
        map_fresh(kernel, asid, va, 1, PageFlags::USER_RW)?;
    }
    kernel.copy_to_user(asid, VirtAddr(PAYLOAD_VA + 0x100), REQUEST)?;
    kernel.copy_to_user(asid, VirtAddr(REPLY_SRC_VA), REPLY)?;
    kernel.register_task_with_class(tid, crate::kernel::task::TaskClass::App)?;
    kernel.with_tcbs_mut(|tcbs| {
        let tcb = tcbs
            .iter_mut()
            .flatten()
            .find(|t| t.tid.0 == tid)
            .ok_or(KernelError::TaskMissing)?;
        // Status is left as registration made it (`Runnable`): the witness adds no transition.
        tcb.asid = Some(asid);
        tcb.user_context.instruction_ptr = VirtAddr(CODE_VA);
        tcb.user_context.stack_ptr = VirtAddr(SP_TOP);
        Ok::<_, KernelError>(())
    })?;
    kernel.set_task_home_cpu(tid, home)?;
    Ok((asid, as_root))
}

fn cap32(c: CapId) -> Result<u32, KernelError> {
    u32::try_from(c.0).map_err(|_| KernelError::CapabilityFull)
}

/// Provision the four tasks from the bootstrap window (`&mut KernelState`). CPU 0's two tasks are
/// enqueued here; CPU 1's are placed by the secondary once it has been admitted.
pub fn provision(kernel: &mut KernelState) -> Result<(), KernelError> {
    let (_, _, es) = kernel.create_endpoint(1)?;
    let (_, _, eh1) = kernel.create_endpoint(1)?;
    let (_, _, eh0) = kernel.create_endpoint(1)?;
    let (_, _, rc) = kernel.create_endpoint(1)?;
    let (_, _, rs) = kernel.create_endpoint(1)?;

    // SAFETY (all `&raw const` below): taking the address of an extern static reads nothing.
    let s_len = &raw const yarm_smp3_s_end as usize - &raw const yarm_smp3_s_start as usize;
    let c_len = &raw const yarm_smp3_c_end as usize - &raw const yarm_smp3_c_start as usize;
    let h_len = &raw const yarm_smp3_h_end as usize - &raw const yarm_smp3_h_start as usize;
    let (s_asid, s_as) = build_task(kernel, S_TID, s_len, CpuId(1))?;
    let (c_asid, c_as) = build_task(kernel, C_TID, c_len, CpuId(0))?;
    let (h1_asid, _) = build_task(kernel, H1_TID, h_len, CpuId(1))?;
    let (h0_asid, _) = build_task(kernel, H0_TID, h_len, CpuId(0))?;

    // Minted straight into each task's own CNode from the object the bootstrap root names; the
    // witness is not exercising delegation.
    let grant = |k: &mut KernelState, cap: CapId, dest: u64, rights: CapRights| {
        let object = k
            .current_task_capability(cap)
            .ok_or(KernelError::InvalidCapability)?
            .object;
        let cnode = k.task_cnode(dest).ok_or(KernelError::TaskMissing)?;
        k.mint_capability_in_cnode(
            cnode,
            crate::kernel::capabilities::Capability::new(object, rights),
        )
        .and_then(cap32)
    };
    let map_rights = CapRights::MAP | CapRights::READ | CapRights::WRITE;
    let s_es = grant(kernel, es, S_TID, CapRights::RECEIVE)?;
    let s_eh0 = grant(kernel, eh0, S_TID, CapRights::SEND)?;
    let s_rs = grant(kernel, rs, S_TID, CapRights::RECEIVE)?;
    let s_as_c = grant(kernel, c_as, S_TID, map_rights)?;
    let c_es = grant(kernel, es, C_TID, CapRights::SEND)?;
    let c_eh1 = grant(kernel, eh1, C_TID, CapRights::SEND)?;
    let c_rc = grant(kernel, rc, C_TID, CapRights::RECEIVE)?;
    let c_as_s = grant(kernel, s_as, C_TID, map_rights)?;
    let h1_cap = grant(kernel, eh1, H1_TID, CapRights::RECEIVE)?;
    let h0_cap = grant(kernel, eh0, H0_TID, CapRights::RECEIVE)?;

    let s_image = patched(
        &raw const yarm_smp3_s_start,
        &raw const yarm_smp3_s_end,
        &[
            (&raw const yarm_smp3_s_cap_es, s_es),
            (&raw const yarm_smp3_s_cap_eh0, s_eh0),
            (&raw const yarm_smp3_s_cap_rs, s_rs),
            (&raw const yarm_smp3_s_cap_as, s_as_c),
        ],
    );
    let c_image = patched(
        &raw const yarm_smp3_c_start,
        &raw const yarm_smp3_c_end,
        &[
            (&raw const yarm_smp3_c_cap_es, c_es),
            (&raw const yarm_smp3_c_cap_eh1, c_eh1),
            (&raw const yarm_smp3_c_cap_rc, c_rc),
            (&raw const yarm_smp3_c_cap_as, c_as_s),
        ],
    );
    let h_start = &raw const yarm_smp3_h_start;
    let h_end = &raw const yarm_smp3_h_end;
    let h1_image = patched(h_start, h_end, &[(&raw const yarm_smp3_h_cap, h1_cap)]);
    let h0_image = patched(h_start, h_end, &[(&raw const yarm_smp3_h_cap, h0_cap)]);
    kernel.copy_to_user(s_asid, VirtAddr(CODE_VA), &s_image)?;
    kernel.copy_to_user(c_asid, VirtAddr(CODE_VA), &c_image)?;
    kernel.copy_to_user(h1_asid, VirtAddr(CODE_VA), &h1_image)?;
    kernel.copy_to_user(h0_asid, VirtAddr(CODE_VA), &h0_image)?;

    // The context patterns and the shared mailbox.
    let mbx = kernel.alloc_user_data_frame()?;
    for (asid, base) in [(s_asid, S_BASE), (c_asid, C_BASE)] {
        map_fresh(kernel, asid, PAT_VA, 1, PageFlags::USER_RW)?;
        kernel.copy_to_user(asid, VirtAddr(PAT_VA), &base.to_le_bytes())?;
        kernel.map_user_page_in_asid_raw(
            asid,
            VirtAddr(MBX_VA),
            Mapping {
                phys: PhysAddr(mbx),
                flags: PageFlags::USER_RW,
            },
        )?;
    }
    kernel.copy_to_user(s_asid, VirtAddr(MBX_VA), &[0u8; 0x1000])?;
    S_ASID.store(u64::from(s_asid.0), Ordering::Release);
    C_ASID.store(u64::from(c_asid.0), Ordering::Release);
    H1_ASID.store(u64::from(h1_asid.0), Ordering::Release);
    H0_ASID.store(u64::from(h0_asid.0), Ordering::Release);

    // CPU 0's tasks go to CPU 0 now: the helper first, so it blocks before anyone calls it.
    kernel.enqueue_task(H0_TID)?;
    kernel.enqueue_task(C_TID)?;
    ENABLED.store(true, Ordering::Release);
    rec::arm();
    // QEMU-SMP3-ACCEPTANCE §3: the firmware that will complete the remote fences, as it reports
    // itself. The grader requires it to be the pinned implementation.
    match crate::arch::riscv64::sbi::base_identity() {
        Ok((spec, impl_id, impl_version)) => crate::kernel::printk::printk_emit_sync(format_args!(
            "SMP3_SBI_IDENTITY spec=0x{:x} impl_id={} impl_version=0x{:x}",
            spec, impl_id, impl_version
        )),
        Err(e) => crate::kernel::printk::printk_emit_sync(format_args!(
            "SMP3_SBI_IDENTITY result=fail err={:?}",
            e
        )),
    }
    crate::kernel::printk::printk_emit_sync(format_args!(
        "SMP3_WITNESS_PROVISIONED s_tid={} s_asid={} c_tid={} c_asid={} h1_tid={} h1_asid={} h0_tid={} h0_asid={} mbx_phys=0x{:x} s_image={} c_image={} h_image={}",
        S_TID,
        s_asid.0,
        C_TID,
        c_asid.0,
        H1_TID,
        h1_asid.0,
        H0_TID,
        h0_asid.0,
        mbx,
        s_image.len(),
        c_image.len(),
        h1_image.len()
    ));
    Ok(())
}

/// The admitted secondary places its two tasks on itself and takes one start-up kick. Nothing else
/// can wake it: it has no timer, and a remote wake IPI is raised only by an enqueue made elsewhere.
pub fn secondary_admitted(shared: &crate::runtime::SharedKernel, cpu: CpuId) {
    if !enabled() || cpu.0 != 1 || CPU1_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    // The admitted state, read on the secondary itself before anything is placed: `sie` (SSIE
    // only), `sstatus.SIE` (clear outside the idle `wfi` loop) and `sstatus.SUM`.
    let (sie, sstatus): (usize, usize);
    // SAFETY: two CSR reads.
    unsafe {
        core::arch::asm!("csrr {0}, sie", "csrr {1}, sstatus", out(reg) sie, out(reg) sstatus,
            options(nomem, nostack, preserves_flags));
    }
    BRINGUP_SIE.store(sie as u64, Ordering::Release);
    BRINGUP_SSTATUS.store(sstatus as u64, Ordering::Release);
    for tid in [H1_TID, S_TID] {
        if let Err(e) = shared.enqueue_on_cpu_split(cpu, tid) {
            BRINGUP_PLACED.store(PLACEMENT_FAILED, Ordering::Release);
            crate::kernel::printk::printk_emit_sync(format_args!(
                "SMP3_WITNESS_SECONDARY_PLACEMENT_FAIL cpu={} tid={} err={:?} result=fail",
                cpu.0, tid, e
            ));
            return;
        }
    }
    let kicked = crate::arch::riscv64::ipi::kick_self(cpu);
    BRINGUP_PLACED.store(
        if kicked {
            PLACED_KICKED
        } else {
            PLACED_KICK_REFUSED
        },
        Ordering::Release,
    );
    // Also printed live, but two harts share the console without a line lock, so the grader takes
    // this fact from the sealed `SMP3_BRINGUP` line; the live marker is corroboration only.
    crate::kernel::printk::printk_emit_sync(format_args!(
        "SMP3_WITNESS_SECONDARY_PLACED cpu={} tids={},{} kick={}",
        cpu.0,
        H1_TID,
        S_TID,
        u8::from(kicked)
    ));
}

/// One supervisor entry on `cpu` (observation only). Any entry ends the hart's idle wait.
pub fn note_supervisor_entry(cpu: CpuId) {
    if let Some(n) = ENTRIES.get(cpu.0 as usize) {
        n.fetch_add(1, Ordering::AcqRel);
    }
    if let Some(f) = AT_IDLE.get(cpu.0 as usize) {
        f.store(false, Ordering::Release);
    }
}

/// `cpu` has reached its idle wait loop (observation only; cleared by its next supervisor entry).
pub fn note_idle_reached(cpu: usize) {
    if let Some(f) = AT_IDLE.get(cpu) {
        f.store(true, Ordering::Release);
    }
}

// ─────────────────────────────── the IPI owner's records ───────────────────────────────

pub fn note_ipi_published(sender: CpuId, target: CpuId, hart: usize, merged: bool) {
    note_publication(sender, target, hart, rec::PUB_WAKE, merged);
}

pub fn note_kick_published(cpu: CpuId, hart: usize, merged: bool) {
    note_publication(cpu, cpu, hart, rec::PUB_KICK, merged);
}

pub fn note_release_published(boot: CpuId, target: CpuId, hart: usize) {
    note_publication(boot, target, hart, rec::PUB_RELEASE, false);
}

fn note_publication(sender: CpuId, target: CpuId, hart: usize, kind: u64, merged: bool) {
    rec::push(
        Kind::IpiPublished,
        sender.0,
        [u64::from(target.0), hart as u64, kind, u64::from(merged), 0],
    );
}

pub fn note_ipi_requested(
    sender: CpuId,
    target: CpuId,
    hart: usize,
    kind: u64,
    result: &Result<(), crate::arch::riscv64::sbi::SbiError>,
) {
    rec::push(
        Kind::IpiRequested,
        sender.0,
        [u64::from(target.0), hart as u64, kind, sbi_code(result), 0],
    );
}

fn sbi_code(result: &Result<(), crate::arch::riscv64::sbi::SbiError>) -> u64 {
    use crate::arch::riscv64::sbi::SbiError;
    match result {
        Ok(()) => 0,
        Err(e) => {
            (match *e {
                SbiError::Failed => -1i64,
                SbiError::NotSupported => -2,
                SbiError::InvalidParam => -3,
                SbiError::Denied => -4,
                SbiError::InvalidAddress => -5,
                SbiError::AlreadyAvailable => -6,
                SbiError::AlreadyStarted => -7,
                SbiError::AlreadyStopped => -8,
                SbiError::NoShmem => -9,
                SbiError::Unknown(c) => c as i64,
            }) as u64
        }
    }
}

pub fn note_park_release(cpu: CpuId, hart: usize) {
    rec::push(Kind::ParkReleased, cpu.0, [hart as u64, 0, 0, 0, 0]);
}

/// Which witness window `sepc` lies in, for the task `tid` that was interrupted there.
pub fn window_of(tid: u64, sepc: u64) -> u64 {
    let within = |start: *const u8, begin: *const u8, end: *const u8| {
        (user_va(start, begin)..user_va(start, end)).contains(&sepc)
    };
    let s = &raw const yarm_smp3_s_start;
    let c = &raw const yarm_smp3_c_start;
    match tid {
        S_TID
            if within(
                s,
                &raw const yarm_smp3_s_win_a_begin,
                &raw const yarm_smp3_s_win_a_end,
            ) =>
        {
            rec::WINDOW_S_A
        }
        S_TID
            if within(
                s,
                &raw const yarm_smp3_s_tlb_win_begin,
                &raw const yarm_smp3_s_tlb_win_end,
            ) =>
        {
            rec::WINDOW_S_TLB
        }
        C_TID
            if within(
                c,
                &raw const yarm_smp3_c_win_b_begin,
                &raw const yarm_smp3_c_win_b_end,
            ) =>
        {
            rec::WINDOW_C_B
        }
        C_TID
            if within(
                c,
                &raw const yarm_smp3_c_tlb_win_begin,
                &raw const yarm_smp3_c_tlb_win_end,
            ) =>
        {
            rec::WINDOW_C_TLB
        }
        _ => rec::WINDOW_NONE,
    }
}

/// The entry owner consumed a supervisor software interrupt (observation only).
pub fn note_arrival(
    cpu: CpuId,
    arrival: crate::arch::riscv64::ipi::IpiArrival,
    sepc: usize,
    tid: u64,
    sstatus: u64,
) {
    if !enabled() {
        return;
    }
    let (origin, window) = match arrival.origin {
        crate::arch::riscv64::ipi::ArrivalOrigin::User => {
            (rec::ORIGIN_USER, window_of(tid, sepc as u64))
        }
        crate::arch::riscv64::ipi::ArrivalOrigin::Idle => (rec::ORIGIN_IDLE, rec::WINDOW_NONE),
    };
    // QEMU-SMP3-SEAL: the hart's supervisor-entry count (this entry included) — its entry history
    // — and the address space `satp` names as the interrupt is taken (the interrupted task's).
    let entries = ENTRIES
        .get(cpu.0 as usize)
        .map_or(0, |n| n.load(Ordering::Acquire));
    let satp: u64;
    // SAFETY: one CSR read.
    unsafe {
        core::arch::asm!("csrr {0}, satp", out(reg) satp, options(nomem, nostack, preserves_flags));
    }
    rec::push(
        Kind::IpiArrived,
        cpu.0,
        [
            arrival.sources,
            origin | (window << 8) | (entries << 16),
            sepc as u64,
            (tid & 0xffff_ffff) | (satp_asid(satp) << 32),
            sstatus,
        ],
    );
    if let Some(row) = CONSUMED.get(cpu.0 as usize) {
        for (src, n) in row.iter().enumerate() {
            if arrival.sources & (1u64 << src) != 0 {
                n.fetch_add(1, Ordering::AcqRel);
            }
        }
    }
}

/// The asid a `satp` value names (Sv39: bits 59:44).
fn satp_asid(satp: u64) -> u64 {
    (satp >> 44) & 0xffff
}

pub fn note_idle_dispatch(cpu: CpuId, tid: u64, ipi_trigger: bool) {
    rec::push(
        Kind::IdleDispatch,
        cpu.0,
        [tid, u64::from(ipi_trigger), 0, 0, 0],
    );
}

// ─────────────────────────────── the remote-fence owner's records ───────────────────────────────

/// Recorded after the `fence rw,rw`, immediately before the firmware call. Returns the request's
/// generation (0 when the page is not the witness's).
pub fn note_fence_request(
    requester: CpuId,
    targets: u64,
    hart_mask: usize,
    asid: u16,
    virt: u64,
) -> u64 {
    if !watches(virt) {
        return 0;
    }
    let generation = rec::next_generation();
    rec::push(
        Kind::FenceRequest,
        requester.0,
        [u64::from(asid), virt, hart_mask as u64, generation, targets],
    );
    generation
}

pub fn note_fence_done(
    requester: CpuId,
    hart_mask: usize,
    asid: u16,
    virt: u64,
    generation: u64,
    result: &Result<(), crate::arch::riscv64::sbi::SbiError>,
) {
    if generation == 0 {
        return;
    }
    rec::push(
        Kind::FenceDone,
        requester.0,
        [
            u64::from(asid),
            virt,
            hart_mask as u64,
            generation,
            sbi_code(result),
        ],
    );
}

// ─────────────────────────────── the VM owner's records ───────────────────────────────

/// A witnessed production VM mapping transaction, as its owner's entry recorded it.
#[derive(Clone, Copy, Debug)]
pub struct VmOp {
    asid: u64,
    va: u64,
    generation: u64,
}

fn contention_snapshot(phase: u64, generation: u64) {
    rec::push(
        Kind::Contention,
        this_cpu(),
        [
            crate::kernel::lock::witness_contended_acquisitions(),
            phase,
            generation,
            0,
            0,
        ],
    );
}

/// `run_vm_map_transaction` entered with its target resolved (observation only; for the witness's
/// page only). The generation names this one operation in its completion.
pub fn vm_op_begin(tid: u64, asid: Asid, addr: usize, len: usize) -> Option<VmOp> {
    if !watches(addr as u64) {
        return None;
    }
    let op = VmOp {
        asid: u64::from(asid.0),
        va: addr as u64,
        generation: rec::next_generation(),
    };
    rec::push(
        Kind::VmOpBegin,
        this_cpu(),
        [op.asid, op.va, len as u64, op.generation, tid],
    );
    contention_snapshot(0, op.generation);
    let role = match tid {
        S_TID => Some(0),
        C_TID => Some(1),
        _ => None,
    };
    if let Some(me) = role {
        let round = MUT_ANNOUNCED[me].swap(0, Ordering::AcqRel);
        if round != 0 {
            mutual_rendezvous(me, round);
        }
    }
    Some(op)
}

/// That transaction is returning `result` — exactly what its caller receives.
pub fn vm_op_end(
    op: Option<VmOp>,
    result: &Result<(usize, usize), crate::kernel::syscall::SyscallError>,
) {
    let Some(op) = op else {
        return;
    };
    let (outcome, returned) = match result {
        Ok((addr, _)) => (0, *addr as u64),
        Err(e) => (e.code() as u64, 0),
    };
    contention_snapshot(1, op.generation);
    rec::push(
        Kind::VmOpEnd,
        this_cpu(),
        [op.asid, op.va, op.generation, outcome, returned],
    );
}

pub fn note_vm_displaced(asid: Asid, va: u64, old: u64) {
    if watches(va) {
        rec::push(
            Kind::VmDisplaced,
            this_cpu(),
            [u64::from(asid.0), va, old, 0, 0],
        );
    }
}

pub fn note_vm_shootdown(asid: Asid, va: u64, acknowledged: bool) {
    if watches(va) {
        rec::push(
            Kind::VmShootdown,
            this_cpu(),
            [u64::from(asid.0), va, u64::from(acknowledged), 0, 0],
        );
    }
}

pub fn note_vm_settled(asid: Asid, va: u64, old: u64) {
    if watches(va) {
        rec::push(
            Kind::VmSettled,
            this_cpu(),
            [u64::from(asid.0), va, old, 0, 0],
        );
    }
}

// ─────────────────────────────── labelled witness synchronization ───────────────────────────────

/// Park waits per TARGET CPU: met, and timed out.
static P1_PARK_MET: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static P1_PARK_TIMED_OUT: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
const P1_PARK_SPINS: u64 = 50_000_000;

/// WITNESS SYNCHRONIZATION — labelled, not production behaviour. A P1 round wakes a PARKED target,
/// but the target raises its mailbox flag before its `ecall`, so the waker could act while the
/// target was still on its way into the receive (the race QEMU-SMP2-ACCEPTANCE measured on
/// AArch64), or while its hart was still draining that block. `current == None` alone is not
/// parked: it becomes true at the in-lock block commit, before the post-lock drain has run. So the
/// waker's last step before its production operation — C's `C_P1_CALL` (target CPU 1) and S's
/// `S_P1_REPLY` (target CPU 0) — waits here, bounded, until the target hart is PARKED: in its idle
/// wait loop, with no current task and an empty run queue. The last clause matters on CPU 0, whose
/// idle timer tick can expire another task's receive deadline and resume the idle wait with that
/// task queued (it is dispatched at the next trap); an IPI arriving then drives an idle advance
/// that selects that task first, which is graded apart as `Preceded`, not as a parked wake. This is
/// the off-lock DebugLog path — no lock is held across the wait, each read is one rank-1 scheduler
/// acquisition — and it waits only for the other hart to go idle, never for anything that hart
/// needs from this one. A timeout is counted per target and the round is then graded as whatever
/// it turned out to be.
fn wait_until_parked(target: CpuId) {
    let t = usize::from(target.0 == 1);
    let Some(shared) = crate::arch::riscv64::boot::trap_shared_kernel_riscv() else {
        P1_PARK_TIMED_OUT[t].fetch_add(1, Ordering::AcqRel);
        return;
    };
    let at_idle = &AT_IDLE[target.0 as usize];
    for _ in 0..P1_PARK_SPINS {
        if at_idle.load(Ordering::Acquire)
            && shared.current_tid_split_read(target).unwrap_or(0) == 0
            && shared.runnable_count_on_cpu_split_read(target) == 0
        {
            P1_PARK_MET[t].fetch_add(1, Ordering::AcqRel);
            return;
        }
        core::hint::spin_loop();
    }
    P1_PARK_TIMED_OUT[t].fetch_add(1, Ordering::AcqRel);
}

/// The round each mutual requester (S = 0, C = 1) has announced and not yet entered the VM owner.
static MUT_ANNOUNCED: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
/// The last mutual round whose operation each requester has ENTERED.
static MUT_ENTERED: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static MUT_SYNC_MET: AtomicU64 = AtomicU64::new(0);
static MUT_SYNC_TIMED_OUT: AtomicU64 = AtomicU64::new(0);
const MUT_SYNC_SPINS: u64 = 50_000_000;

/// WITNESS SYNCHRONIZATION — labelled, not production behaviour. The operation that entered the VM
/// owner first waits, bounded, until the other requester's operation has ENTERED too. It runs at
/// the owner's entry point, where no domain lock is held, and waits only for the other hart to
/// enter — never for anything that hart needs from this one — so it cannot deadlock. Nothing about
/// either operation's work, locks or order inside the owner is changed.
fn mutual_rendezvous(me: usize, round: u64) {
    MUT_ENTERED[me].store(round, Ordering::Release);
    let other = &MUT_ENTERED[1 - me];
    for _ in 0..MUT_SYNC_SPINS {
        if other.load(Ordering::Acquire) >= round {
            MUT_SYNC_MET.fetch_add(1, Ordering::AcqRel);
            return;
        }
        core::hint::spin_loop();
    }
    MUT_SYNC_TIMED_OUT.fetch_add(1, Ordering::AcqRel);
}

// ──────────────────────────── QEMU-SMP3-SEAL: readiness and evidence ────────────────────────────

/// Arrivals each CPU consumed from each source CPU: `[dst][src]` (observation only).
static CONSUMED: [[AtomicU64; 2]; MAX_CPUS] =
    [const { [const { AtomicU64::new(0) }; 2] }; MAX_CPUS];
/// Activation history is recorded on a hart only while armed: from a P2 attempt's readiness to its
/// target's window check, and from a shootdown's target computation to its target's observation.
static ACT_ARMED: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
/// Each requesting CPU's last live-ASID snapshot: asid, bitmap, CPU 0's and CPU 1's current tid.
static SNAP_ASID: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(u64::MAX) }; MAX_CPUS];
static SNAP_CUR: [[AtomicU64; 2]; MAX_CPUS] =
    [const { [const { AtomicU64::new(0) }; 2] }; MAX_CPUS];
/// The P2 target hart's consumed-from-waker count at readiness, per target CPU.
static P2_BASE: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
/// Readiness waits (index 0 = P2, 1 = P3) and P2 post-send waits: met, and timed out.
static READY_MET: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static READY_TIMED_OUT: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static SENT_MET: AtomicU64 = AtomicU64::new(0);
static SENT_TIMED_OUT: AtomicU64 = AtomicU64::new(0);
const READY_SPINS: u64 = 50_000_000;
const SENT_SPINS: u64 = 50_000_000;

fn cpu_of_asid(asid: u64) -> Option<usize> {
    if asid == S_ASID.load(Ordering::Acquire) {
        Some(1)
    } else if asid == C_ASID.load(Ordering::Acquire) {
        Some(0)
    } else {
        None
    }
}

/// `write_satp` / `activate_asid` are about to install `satp` on this hart (observation only;
/// recorded before the write, so a recorded activation's fence follows everything before it).
pub fn note_activation(satp: u64) {
    if !enabled() {
        return;
    }
    let cpu = this_cpu();
    if ACT_ARMED
        .get(cpu as usize)
        .is_some_and(|a| a.load(Ordering::Acquire))
    {
        rec::push(Kind::Activation, cpu, [satp_asid(satp), satp, 0, 0, 0]);
    }
}

/// `live_cpu_bitmap_for_asid_split` computed `bitmap` for `asid` from `current` (observation only).
pub fn note_live_snapshot(asid: u16, _bitmap: u64, current: &[Option<u64>]) {
    if !enabled() {
        return;
    }
    let cpu = this_cpu() as usize;
    let (Some(a), Some(cur)) = (SNAP_ASID.get(cpu), SNAP_CUR.get(cpu)) else {
        return;
    };
    for (slot, tid) in cur.iter().zip(current.iter()) {
        slot.store(tid.unwrap_or(0), Ordering::Release);
    }
    a.store(u64::from(asid), Ordering::Release);
}

/// The shootdown owner is about to compute its targets for the witness page: arm activation
/// history on the witness target's hart first, so no activation after the snapshot goes unseen.
pub fn note_shoot_begin(asid: u16, va: u64) {
    if !watches(va) {
        return;
    }
    if let Some(cpu) = cpu_of_asid(u64::from(asid)) {
        ACT_ARMED[cpu].store(true, Ordering::Release);
    }
    if let Some(a) = SNAP_ASID.get(this_cpu() as usize) {
        a.store(u64::MAX, Ordering::Release);
    }
}

/// ... and computed `targets` from the snapshot it just kept.
pub fn note_shoot_targets(requester: CpuId, asid: u16, va: u64, targets: u64) {
    if !watches(va) {
        return;
    }
    let cpu = requester.0 as usize;
    let fresh = SNAP_ASID
        .get(cpu)
        .is_some_and(|a| a.load(Ordering::Acquire) == u64::from(asid));
    let cur = |i: usize| {
        if fresh {
            SNAP_CUR[cpu][i].load(Ordering::Acquire)
        } else {
            u64::MAX
        }
    };
    rec::push(
        Kind::ShootTargets,
        requester.0,
        [u64::from(asid), va, targets, cur(0), cur(1)],
    );
}

/// WITNESS SYNCHRONIZATION — labelled, not production behaviour. Before a waker (P2) or requester
/// (P3) issues its production operation against `target`, wait, bounded, until `target` is current
/// on its hart and not at its idle boundary, and record what was seen — the target's current tid
/// and its hart's entry count, read as one consistent snapshot (the count unchanged across the
/// read). The operation itself is never delayed past the bound and nothing is suppressed: an
/// unmet wait is recorded as such and the attempt is graded for what it then was. This is the
/// off-lock DebugLog path; each read is one rank-1 scheduler acquisition; it waits only for the
/// other hart to run its own task.
fn establish_ready(phase: u64, target_cpu: u8, target_tid: u64, round: u64) {
    let t = target_cpu as usize;
    if phase == rec::PHASE_P2 {
        // Activation history on the target's hart from before the snapshot to its window check.
        ACT_ARMED[t].store(true, Ordering::Release);
    }
    let idx = usize::from(phase == rec::PHASE_P3);
    let shared = crate::arch::riscv64::boot::trap_shared_kernel_riscv();
    let (mut cur, mut entries, mut met) = (0u64, 0u64, false);
    for _ in 0..READY_SPINS {
        let e0 = ENTRIES[t].load(Ordering::Acquire);
        cur = shared.map_or(0, |k| {
            k.current_tid_split_read(CpuId(target_cpu)).unwrap_or(0)
        });
        let idle = AT_IDLE[t].load(Ordering::Acquire);
        let e1 = ENTRIES[t].load(Ordering::Acquire);
        entries = e1;
        if e0 == e1 && cur == target_tid && !idle {
            met = true;
            break;
        }
        core::hint::spin_loop();
    }
    if met {
        READY_MET[idx].fetch_add(1, Ordering::AcqRel);
    } else {
        READY_TIMED_OUT[idx].fetch_add(1, Ordering::AcqRel);
    }
    rec::push(
        Kind::Ready,
        this_cpu(),
        [
            u64::from(target_cpu) | (phase << 8) | (u64::from(met) << 16),
            cur,
            entries,
            round,
            rec::next_generation(),
        ],
    );
    if phase == rec::PHASE_P2 {
        P2_BASE[t].store(
            CONSUMED[t][usize::from(this_cpu())].load(Ordering::Acquire),
            Ordering::Release,
        );
    }
}

/// WITNESS SYNCHRONIZATION — labelled, not production behaviour. After a P2 waker's production
/// send returned, wait, bounded, until the target hart has CONSUMED an arrival from this CPU since
/// readiness — so the target's window closes on the arrival, never on a fixed delay that a slow
/// delivery can outlast. A suppressed IPI makes this time out; the attempt then fails on its own
/// missing arrival (bounded progress), never on this wait.
fn await_consumed(target_cpu: u8) {
    let t = target_cpu as usize;
    let me = usize::from(this_cpu());
    let base = P2_BASE[t].load(Ordering::Acquire);
    for _ in 0..SENT_SPINS {
        if CONSUMED[t][me].load(Ordering::Acquire) > base {
            SENT_MET.fetch_add(1, Ordering::AcqRel);
            return;
        }
        core::hint::spin_loop();
    }
    SENT_TIMED_OUT.fetch_add(1, Ordering::AcqRel);
}

// ─────────────────────────────── the user steps and the dump ───────────────────────────────

/// The DebugLog route saw a user step. Every `SMP3 ` step becomes one record; the residency steps
/// also record the stepping hart's supervisor-entry count; a failure is echoed at once.
pub fn observe_user_marker(cpu: CpuId, tid: u64, asid: u64, msg: &str, round: u64, aux: u64) {
    if !enabled() {
        return;
    }
    let Some(code) = rec::step_code(msg) else {
        return;
    };
    rec::push(Kind::User, cpu.0, [tid, asid, code, round, aux]);
    let step = rec::step_name(code);
    if matches!(step, "S_PRE" | "C_PRE" | "S_OBSERVED" | "C_OBSERVED") {
        let entries = ENTRIES
            .get(cpu.0 as usize)
            .map_or(0, |n| n.load(Ordering::Acquire));
        rec::push(Kind::Residency, cpu.0, [tid, entries, code, round, 0]);
    }
    if code >= rec::STEP_FAIL {
        crate::kernel::printk::printk_emit_sync(format_args!(
            "SMP3_USER_FAIL cpu={} tid={} msg={} round={} aux=0x{:x} result=fail",
            cpu.0,
            tid,
            msg.trim_end(),
            round,
            aux
        ));
    }
    // QEMU-SMP3-SEAL: readiness before the production operation, the P2 post-send wait, and the
    // end of each armed activation-history interval (the target's own check on its own hart).
    match step {
        "C_P2A_CALL" => return establish_ready(rec::PHASE_P2, 1, S_TID, round),
        "S_P2B_CALL" => return establish_ready(rec::PHASE_P2, 0, C_TID, round),
        "C_REQ" => return establish_ready(rec::PHASE_P3, 1, S_TID, round),
        "S_REQ" => return establish_ready(rec::PHASE_P3, 0, C_TID, round),
        "C_P2A_SENT" => return await_consumed(1),
        "S_P2B_SENT" => return await_consumed(0),
        "S_WIN_A_OK" | "C_WIN_B_OK" | "S_OBSERVED" | "C_OBSERVED" | "S_MUT_OK" | "C_MUT_OK" => {
            if let Some(a) = ACT_ARMED.get(cpu.0 as usize) {
                a.store(false, Ordering::Release);
            }
        }
        _ => {}
    }
    // The P1 waker is about to wake a PARKED target: see `wait_until_parked`.
    if step == "C_P1_CALL" {
        wait_until_parked(CpuId(1));
        return;
    }
    if step == "S_P1_REPLY" {
        wait_until_parked(CpuId(0));
        return;
    }
    // The mutual requester's announcement arms its next VM operation for the rendezvous.
    if step == "S_MUT_NR3" || step == "C_MUT_NR3" {
        MUT_ANNOUNCED[usize::from(step == "C_MUT_NR3")].store(round, Ordering::Release);
        return;
    }
    if step == "S_DONE" {
        S_DONE.store(true, Ordering::Release);
    } else if step == "C_DONE" {
        C_DONE.store(true, Ordering::Release);
    } else {
        return;
    }
    if S_DONE.load(Ordering::Acquire)
        && C_DONE.load(Ordering::Acquire)
        && !DUMPED.swap(true, Ordering::AcqRel)
    {
        dump();
    }
}

/// Seal, print every record, every graded attempt and the verdict — all synchronously, so no line
/// can be lost. Each line is formatted and emitted on its own (twice, each copy with its own
/// checksum): the console is shared with raw markers another hart may write mid-line, so a grader
/// takes each line from whichever copy is intact.
fn dump() {
    let mut out = [None; rec::SLOTS];
    let Some((n, overflowed)) = rec::seal(&mut out, 50_000_000) else {
        crate::kernel::printk::printk_emit_sync(format_args!(
            "SMP3_VERDICT result=fail reason=record_not_sealed"
        ));
        return;
    };
    let mut recs: alloc::vec::Vec<rec::Rec> = alloc::vec::Vec::with_capacity(n);
    recs.extend(out.iter().take(n).flatten().copied());
    let roles = roles();
    let (v, attempts) = rec::verify_attempts(&recs, &roles, overflowed);
    for pass in 1..=2u32 {
        let emit = |line: alloc::string::String| {
            crate::kernel::printk::printk_emit_sync(format_args!(
                "{} pass={} crc=0x{:08x}",
                line,
                pass,
                fnv1a(line.as_bytes())
            ));
        };
        emit(alloc::format!(
            "SMP3_ROLES s={}:{}:{}:{} c={}:{}:{}:{} h1={}:{} h0={}:{} w_va=0x{:x} dump_cpu={}",
            roles.s.tid,
            roles.s.asid,
            roles.s.cpu,
            roles.s.hart,
            roles.c.tid,
            roles.c.asid,
            roles.c.cpu,
            roles.c.hart,
            roles.h1.tid,
            roles.h1.asid,
            roles.h0.tid,
            roles.h0.asid,
            roles.w_va,
            this_cpu()
        ));
        for r in &recs {
            emit(alloc::format!(
                "SMP3_REC seq={} kind={} cpu={} f0=0x{:x} f1=0x{:x} f2=0x{:x} f3=0x{:x} f4=0x{:x}{}{}",
                r.seq,
                r.kind.name(),
                r.cpu,
                r.f[0],
                r.f[1],
                r.f[2],
                r.f[3],
                r.f[4],
                if r.kind == Kind::User || r.kind == Kind::Residency {
                    " step="
                } else {
                    ""
                },
                match r.kind {
                    Kind::User | Kind::Residency => rec::step_name(r.f[2]),
                    _ => "",
                }
            ));
        }
        for a in attempts.iter() {
            emit(alloc::format!(
                "SMP3_ATTEMPT phase={} n={} target_cpu={} gen=0x{:x} eligible={} reason={} outcome={}",
                a.phase,
                a.n,
                a.target_cpu,
                a.generation,
                u8::from(a.eligible),
                a.reason,
                a.outcome.name()
            ));
        }
        let (sie, sstatus) = (
            BRINGUP_SIE.load(Ordering::Acquire),
            BRINGUP_SSTATUS.load(Ordering::Acquire),
        );
        emit(alloc::format!(
            "SMP3_BRINGUP cpu=1 hart={} sie=0x{:x} sstatus_sie={} sum={} placed={} tids={},{} kick={}",
            hart(1),
            sie,
            (sstatus >> 1) & 1,
            (sstatus >> 18) & 1,
            match BRINGUP_PLACED.load(Ordering::Acquire) {
                PLACED_KICKED | PLACED_KICK_REFUSED => "ok",
                PLACEMENT_FAILED => "fail",
                _ => "none",
            },
            H1_TID,
            S_TID,
            u8::from(BRINGUP_PLACED.load(Ordering::Acquire) == PLACED_KICKED)
        ));
        for cpu in [CpuId(0), CpuId(1)] {
            let (p, m, q, qf) = crate::arch::riscv64::ipi::sent_counters(cpu);
            let (a, au, ai, ac, ae, ar) = crate::arch::riscv64::ipi::taken_counters(cpu);
            let (fr, fo, ff) = crate::arch::riscv64::ipi::fence_counters(cpu);
            emit(alloc::format!(
                "SMP3_CPU_IPI cpu={} hart={} published={} merged={} fw_requests={} fw_refused={} arrivals={} from_user={} at_idle={} consumed={} empty={}",
                cpu.0,
                hart(cpu.0),
                p,
                m,
                q,
                qf,
                a,
                au,
                ai,
                ac,
                ae
            ));
            emit(alloc::format!(
                "SMP3_CPU_FENCE cpu={} park_releases={} fences={} fences_ok={} fences_refused={} supervisor_entries={}",
                cpu.0,
                ar,
                fr,
                fo,
                ff,
                ENTRIES[cpu.0 as usize].load(Ordering::Acquire)
            ));
        }
        emit(alloc::format!(
            "SMP3_COUNTS_IPI records={} arrivals={} consumed={} empty={} merged={} user_fp_vs_off={}",
            v.records,
            v.ipi_arrivals,
            v.ipi_consumed,
            v.ipi_empty,
            v.ipi_merged,
            v.user_fp_vs_off
        ));
        emit(alloc::format!(
            "SMP3_COUNTS_WAKE p1_parked={} p1_ipi_to_s={} p1_ipi_to_c={} p1_timer_first={} p1_busy={} p1_preceded={}",
            v.p1_parked,
            v.p1_ipi_to_s,
            v.p1_ipi_to_c,
            v.p1_timer_first,
            v.p1_busy,
            v.p1_preceded
        ));
        emit(alloc::format!(
            "SMP3_COUNTS_P2 p2_attempts={} p2_credited_s={} p2_credited_c={} p2_uncredited={} p2_in_window={} p2_outside={} p2_displaced={}",
            v.p2_attempts,
            v.p2_credited_s,
            v.p2_credited_c,
            v.p2_uncredited,
            v.p2_in_window,
            v.p2_outside,
            v.p2_displaced
        ));
        emit(alloc::format!(
            "SMP3_COUNTS_TLB tlb_rounds={} credited_s={} credited_c={} interfered={} off_cpu={} not_ready={}",
            v.tlb_rounds,
            v.tlb_credited_s,
            v.tlb_credited_c,
            v.tlb_interfered,
            v.tlb_off_cpu,
            v.tlb_not_ready
        ));
        emit(alloc::format!(
            "SMP3_COUNTS_MUT mutual_rounds={} overlapped={} contended={} mutual_off_cpu={} settled_after_completion={} settled_local_only={}",
            v.mutual_rounds,
            v.mutual_overlapped,
            v.mutual_contended,
            v.mutual_off_cpu,
            v.settled_after_completion,
            v.settled_local_only
        ));
        emit(alloc::format!(
            "SMP3_SYNC mutual_rendezvous_met={} mutual_rendezvous_timed_out={} p1_parked_met_cpu0={} p1_parked_timed_out_cpu0={} p1_parked_met_cpu1={} p1_parked_timed_out_cpu1={}",
            MUT_SYNC_MET.load(Ordering::Acquire),
            MUT_SYNC_TIMED_OUT.load(Ordering::Acquire),
            P1_PARK_MET[0].load(Ordering::Acquire),
            P1_PARK_TIMED_OUT[0].load(Ordering::Acquire),
            P1_PARK_MET[1].load(Ordering::Acquire),
            P1_PARK_TIMED_OUT[1].load(Ordering::Acquire)
        ));
        emit(alloc::format!(
            "SMP3_SEAL_SYNC p2_ready_met={} p2_ready_timed_out={} p2_sent_met={} p2_sent_timed_out={} p3_ready_met={} p3_ready_timed_out={}",
            READY_MET[0].load(Ordering::Acquire),
            READY_TIMED_OUT[0].load(Ordering::Acquire),
            SENT_MET.load(Ordering::Acquire),
            SENT_TIMED_OUT.load(Ordering::Acquire),
            READY_MET[1].load(Ordering::Acquire),
            READY_TIMED_OUT[1].load(Ordering::Acquire)
        ));
        emit(alloc::format!(
            "SMP3_VERDICT result={} reason={} at={}",
            if v.ok() { "ok" } else { "fail" },
            v.failure.unwrap_or("none"),
            v.failure_at
        ));
    }
}

/// FNV-1a over a dump line's text (everything before ` pass=`).
pub fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5u32, |h, &b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}
