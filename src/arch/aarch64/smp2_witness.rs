// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP2 — the AArch64 two-CPU interrupt / TLB / context witness. Built only with
//! `aarch64-smp2-witness` and armed only by `yarm.ap_user_dispatch=1` on a two-CPU boot.
//!
//! It drives PRODUCTION owners and records what they did; it performs none of the work it
//! grades:
//!
//! * every wake SGI is the one the direct NR6/NR7 drains send after their enqueue commits, and
//!   every claim, completion and SGI-driven dispatch is the vector entry's, the vector tail's and
//!   the shared bridge's own;
//! * every W replacement is a genuine NR 3 issued by the OTHER task, so the install, the
//!   break-before-make invalidation, the displaced pin and the settlement are the VM and
//!   page-table owners';
//! * context is checked by the user programs themselves (`smp2_witness.S`).
//!
//! Its own kernel actions are setup, the AP's one start-up kick (the AP has no timer, so a task
//! placed on it outside a trap needs one interrupt to be dispatched), and ONE probe: when a
//! requester arms a round it re-points the target's residency page R at a frame never mapped
//! there before by writing the leaf WITHOUT any invalidation. A target that still reads the value
//! it primed therefore kept its translations across the round, so its view of W can only have
//! changed through the requester's targeted invalidation of W.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::kernel::boot::smp2_record::{self as rec, Kind, Role, Roles};
use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::capabilities::{CapId, CapRights};
use crate::kernel::scheduler::CpuId;
use crate::kernel::vm::{Asid, CachePolicy, Mapping, PageFlags, PhysAddr, VirtAddr};

core::arch::global_asm!(include_str!("smp2_witness.S"));

pub const CODE_VA: u64 = 0x2000_0000;
pub const STACK_VA: u64 = 0x2001_0000;
pub const SP_TOP: u64 = 0x2001_0ff0;
pub const PAYLOAD_VA: u64 = 0x2003_0000;
pub const META_VA: u64 = 0x2004_0000;
pub const REPLY_SRC_VA: u64 = 0x2005_0000;
pub const PAT_VA: u64 = 0x2006_0000;
pub const MBX_VA: u64 = 0x2007_0000;
pub const W_VA: u64 = 0x2008_0000;
pub const R_VA: u64 = 0x2009_0000;
/// Kernel-provisioning aliases of the probe's later frames; the programs never touch them.
pub const R_ALT_VA: u64 = 0x200A_0000;
pub const OLD_R: u64 = 0x5151_5151_5151_5151;
/// Frame `i` (1-based) behind R holds `NEW_R + i`.
pub const NEW_R: u64 = 0x5252_5252_5252_5200;
/// Serial rounds in which each task is the target.
pub const TARGET_ROUNDS: usize = 2;

pub const S_TID: u64 = 9_200;
pub const C_TID: u64 = 9_201;
pub const H1_TID: u64 = 9_202;
pub const H0_TID: u64 = 9_203;

/// `(fp seed, FPCR, FPSR, NZCV, TPIDR_EL0, GPR base)` per context-checked task.
const S_CTX: (u8, u64, u64, u64, u64, u64) = (
    0x3C,
    0x0240_0000,
    0x0800_0015,
    0xA000_0000,
    0x5354_4C53_0000_0001,
    0xA1A1_0000_0000_0000,
);
const C_CTX: (u8, u64, u64, u64, u64, u64) = (
    0xC5,
    0x0180_0000,
    0x0000_008A,
    0x5000_0000,
    0x4354_4C53_0000_0002,
    0xB2B2_0000_0000_0000,
);

const REQUEST: &[u8] = &[0x99, 0x01, b'N', b'R', b'6', b'-', b'R', b'E', b'Q', b'!'];
const REPLY: &[u8] = b"RPLY-OK!";

unsafe extern "C" {
    static yarm_smp2_s_start: u8;
    static yarm_smp2_s_end: u8;
    static yarm_smp2_s_cap_es: u8;
    static yarm_smp2_s_cap_eh0: u8;
    static yarm_smp2_s_cap_rs: u8;
    static yarm_smp2_s_cap_as: u8;
    static yarm_smp2_s_win_a_begin: u8;
    static yarm_smp2_s_win_a_end: u8;
    static yarm_smp2_s_tlb_win_begin: u8;
    static yarm_smp2_s_tlb_win_end: u8;
    static yarm_smp2_c_start: u8;
    static yarm_smp2_c_end: u8;
    static yarm_smp2_c_cap_es: u8;
    static yarm_smp2_c_cap_eh1: u8;
    static yarm_smp2_c_cap_rc: u8;
    static yarm_smp2_c_cap_as: u8;
    static yarm_smp2_c_win_b_begin: u8;
    static yarm_smp2_c_win_b_end: u8;
    static yarm_smp2_c_tlb_win_begin: u8;
    static yarm_smp2_c_tlb_win_end: u8;
    static yarm_smp2_h_start: u8;
    static yarm_smp2_h_end: u8;
    static yarm_smp2_h_cap: u8;
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

/// The context pattern image: q0..q31, FPCR, FPSR, NZCV, TPIDR_EL0, then the GPR base at 0x240.
pub fn pattern(ctx: (u8, u64, u64, u64, u64, u64)) -> [u8; 0x248] {
    let (seed, fpcr, fpsr, nzcv, tpidr, base) = ctx;
    let mut b = [0u8; 0x248];
    for i in 0..32 {
        for k in 0..16 {
            b[16 * i + k] = seed.wrapping_add((i * 16 + k) as u8).wrapping_mul(0x9D) ^ 0x5A;
        }
    }
    b[512..520].copy_from_slice(&fpcr.to_le_bytes());
    b[520..528].copy_from_slice(&fpsr.to_le_bytes());
    b[528..536].copy_from_slice(&nzcv.to_le_bytes());
    b[536..544].copy_from_slice(&tpidr.to_le_bytes());
    b[0x240..0x248].copy_from_slice(&base.to_le_bytes());
    b
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static S_ASID: AtomicU64 = AtomicU64::new(0);
static C_ASID: AtomicU64 = AtomicU64::new(0);
static H1_ASID: AtomicU64 = AtomicU64::new(0);
static H0_ASID: AtomicU64 = AtomicU64::new(0);
static S_R_FRAMES: [AtomicU64; TARGET_ROUNDS] = [const { AtomicU64::new(0) }; TARGET_ROUNDS];
static C_R_FRAMES: [AtomicU64; TARGET_ROUNDS] = [const { AtomicU64::new(0) }; TARGET_ROUNDS];
static S_R_NEXT: AtomicU64 = AtomicU64::new(0);
static C_R_NEXT: AtomicU64 = AtomicU64::new(0);
static S_DONE: AtomicBool = AtomicBool::new(false);
static C_DONE: AtomicBool = AtomicBool::new(false);
static DUMPED: AtomicBool = AtomicBool::new(false);
static CPU1_STARTED: AtomicBool = AtomicBool::new(false);

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

/// The VM and page-table witness records are confined to the one page the witness replaces.
pub fn watches(va: u64) -> bool {
    enabled() && va == W_VA
}

fn roles() -> Roles {
    let role = |tid, asid: &AtomicU64, cpu| Role {
        tid,
        asid: asid.load(Ordering::Acquire),
        cpu,
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
    (crate::arch::aarch64::read_mpidr_el1() & 0xff) as u8
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
    image: &[u8],
    home: CpuId,
) -> Result<(Asid, CapId), KernelError> {
    let (asid, as_root) = kernel.create_user_address_space()?;
    map_fresh(
        kernel,
        asid,
        CODE_VA,
        image.len().div_ceil(0x1000),
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
        tcb.asid = Some(asid);
        tcb.status = crate::kernel::task::TaskStatus::Runnable;
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

/// Provision the four tasks from the bootstrap window (`&mut KernelState`, the first user task
/// current). CPU 0's two tasks are enqueued here; CPU 1's are placed by the AP once it has been
/// admitted (`ap_admitted`).
pub fn provision(kernel: &mut KernelState) -> Result<(), KernelError> {
    let (_, _, es) = kernel.create_endpoint(1)?;
    let (_, _, eh1) = kernel.create_endpoint(1)?;
    let (_, _, eh0) = kernel.create_endpoint(1)?;
    let (_, _, rc) = kernel.create_endpoint(1)?;
    let (_, _, rs) = kernel.create_endpoint(1)?;

    // SAFETY (all `&raw const` below): taking the address of an extern static reads nothing.
    let s_len = &raw const yarm_smp2_s_end as usize - &raw const yarm_smp2_s_start as usize;
    let c_len = &raw const yarm_smp2_c_end as usize - &raw const yarm_smp2_c_start as usize;
    let h_len = &raw const yarm_smp2_h_end as usize - &raw const yarm_smp2_h_start as usize;
    let placeholder = |n: usize| alloc::vec![0u8; n];
    let (s_asid, s_as) = build_task(kernel, S_TID, &placeholder(s_len), CpuId(1))?;
    let (c_asid, c_as) = build_task(kernel, C_TID, &placeholder(c_len), CpuId(0))?;
    let (h1_asid, _) = build_task(kernel, H1_TID, &placeholder(h_len), CpuId(1))?;
    let (h0_asid, _) = build_task(kernel, H0_TID, &placeholder(h_len), CpuId(0))?;

    // Minted straight into each task's own CNode from the object the bootstrap root names, the
    // way the CONTEXT1 witness provisions its park endpoint: the roots carry no grant right, and
    // the witness is not exercising delegation.
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
        &raw const yarm_smp2_s_start,
        &raw const yarm_smp2_s_end,
        &[
            (&raw const yarm_smp2_s_cap_es, s_es),
            (&raw const yarm_smp2_s_cap_eh0, s_eh0),
            (&raw const yarm_smp2_s_cap_rs, s_rs),
            (&raw const yarm_smp2_s_cap_as, s_as_c),
        ],
    );
    let c_image = patched(
        &raw const yarm_smp2_c_start,
        &raw const yarm_smp2_c_end,
        &[
            (&raw const yarm_smp2_c_cap_es, c_es),
            (&raw const yarm_smp2_c_cap_eh1, c_eh1),
            (&raw const yarm_smp2_c_cap_rc, c_rc),
            (&raw const yarm_smp2_c_cap_as, c_as_s),
        ],
    );
    let h_start = &raw const yarm_smp2_h_start;
    let h_end = &raw const yarm_smp2_h_end;
    let h1_image = patched(h_start, h_end, &[(&raw const yarm_smp2_h_cap, h1_cap)]);
    let h0_image = patched(h_start, h_end, &[(&raw const yarm_smp2_h_cap, h0_cap)]);
    kernel.copy_to_user(s_asid, VirtAddr(CODE_VA), &s_image)?;
    kernel.copy_to_user(c_asid, VirtAddr(CODE_VA), &c_image)?;
    kernel.copy_to_user(h1_asid, VirtAddr(CODE_VA), &h1_image)?;
    kernel.copy_to_user(h0_asid, VirtAddr(CODE_VA), &h0_image)?;

    // The context patterns, the shared mailbox and each target's residency probe frames.
    let mbx = kernel.alloc_user_data_frame()?;
    for (asid, ctx, frames) in [(s_asid, S_CTX, &S_R_FRAMES), (c_asid, C_CTX, &C_R_FRAMES)] {
        map_fresh(kernel, asid, PAT_VA, 1, PageFlags::USER_RW)?;
        kernel.copy_to_user(asid, VirtAddr(PAT_VA), &pattern(ctx))?;
        kernel.map_user_page_in_asid_raw(
            asid,
            VirtAddr(MBX_VA),
            Mapping {
                phys: PhysAddr(mbx),
                flags: PageFlags::USER_RW,
            },
        )?;
        map_fresh(kernel, asid, R_VA, 1, PageFlags::USER_RW)?;
        kernel.copy_to_user(asid, VirtAddr(R_VA), &OLD_R.to_le_bytes())?;
        for (k, slot) in frames.iter().enumerate() {
            let alias = R_ALT_VA + (k as u64) * 0x1000;
            let frame = map_fresh(kernel, asid, alias, 1, PageFlags::USER_RW)?;
            kernel.copy_to_user(asid, VirtAddr(alias), &(NEW_R + k as u64 + 1).to_le_bytes())?;
            slot.store(frame, Ordering::Release);
        }
    }
    kernel.copy_to_user(s_asid, VirtAddr(MBX_VA), &[0u8; 0x400])?;
    S_ASID.store(u64::from(s_asid.0), Ordering::Release);
    C_ASID.store(u64::from(c_asid.0), Ordering::Release);
    H1_ASID.store(u64::from(h1_asid.0), Ordering::Release);
    H0_ASID.store(u64::from(h0_asid.0), Ordering::Release);

    // CPU 0's tasks go to CPU 0 now: the helper first, so it blocks before anyone calls it.
    kernel.enqueue_task(H0_TID)?;
    kernel.enqueue_task(C_TID)?;
    ENABLED.store(true, Ordering::Release);
    rec::arm();
    crate::kernel::printk::printk_emit_sync(format_args!(
        "SMP2_WITNESS_PROVISIONED s_tid={} s_asid={} c_tid={} c_asid={} h1_tid={} h1_asid={} h0_tid={} h0_asid={} mbx_phys=0x{:x} s_image={} c_image={} h_image={}",
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

/// The admitted AP places its two tasks on itself and takes one start-up kick. Nothing else can
/// wake it: it has no timer, and a remote wake SGI is raised only by an enqueue made elsewhere.
pub fn ap_admitted(shared: &crate::runtime::SharedKernel, cpu: CpuId) {
    if !enabled() || cpu.0 != 1 || CPU1_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    for tid in [H1_TID, S_TID] {
        if let Err(e) = shared.enqueue_on_cpu_split(cpu, tid) {
            crate::kernel::printk::printk_emit_sync(format_args!(
                "SMP2_WITNESS_AP_PLACEMENT_FAIL cpu={} tid={} err={:?} result=fail",
                cpu.0, tid, e
            ));
            return;
        }
    }
    let kicked = crate::arch::aarch64::smp::kick_self(cpu);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "SMP2_WITNESS_AP_PLACED cpu={} tids={},{} kick={}",
        cpu.0,
        H1_TID,
        S_TID,
        u8::from(kicked)
    ));
}

/// Which witness window `elr` lies in, for the task `tid` that was interrupted there.
pub fn window_of(tid: u64, elr: u64) -> u64 {
    let within = |start: *const u8, begin: *const u8, end: *const u8| {
        (user_va(start, begin)..user_va(start, end)).contains(&elr)
    };
    let s = &raw const yarm_smp2_s_start;
    let c = &raw const yarm_smp2_c_start;
    match tid {
        S_TID
            if within(
                s,
                &raw const yarm_smp2_s_win_a_begin,
                &raw const yarm_smp2_s_win_a_end,
            ) =>
        {
            rec::WINDOW_S_A
        }
        S_TID
            if within(
                s,
                &raw const yarm_smp2_s_tlb_win_begin,
                &raw const yarm_smp2_s_tlb_win_end,
            ) =>
        {
            rec::WINDOW_S_TLB
        }
        C_TID
            if within(
                c,
                &raw const yarm_smp2_c_win_b_begin,
                &raw const yarm_smp2_c_win_b_end,
            ) =>
        {
            rec::WINDOW_C_B
        }
        C_TID
            if within(
                c,
                &raw const yarm_smp2_c_tlb_win_begin,
                &raw const yarm_smp2_c_tlb_win_end,
            ) =>
        {
            rec::WINDOW_C_TLB
        }
        _ => rec::WINDOW_NONE,
    }
}

/// The vector entry's claim returned the reschedule SGI (observation only).
pub fn note_arrival(cpu: CpuId, raw: u32, source: u64, origin: u64, elr: u64) {
    if !enabled() {
        return;
    }
    let tid = crate::arch::aarch64::boot::trap_shared_kernel()
        .and_then(|s| s.current_tid_split_read(cpu))
        .unwrap_or(0);
    let window = if origin == rec::ORIGIN_USER {
        window_of(tid, elr)
    } else {
        rec::WINDOW_NONE
    };
    rec::push(
        Kind::SgiArrived,
        cpu.0,
        [u64::from(raw), source, origin | (window << 8), elr, tid],
    );
}

/// The DebugLog route saw a user step. Every `SMP2 ` step becomes one record; a failure is also
/// echoed at once, synchronously. `SMP2 x_ARM_R` re-points the target's probe (the witness's one
/// kernel action); both tasks' `DONE` seal and dump the record.
pub fn observe_user_marker(cpu: CpuId, tid: u64, asid: u64, msg: &str, round: u64, aux: u64) {
    if !enabled() {
        return;
    }
    let Some(code) = rec::step_code(msg) else {
        return;
    };
    rec::push(Kind::User, cpu.0, [tid, asid, code, round, aux]);
    if code >= rec::STEP_FAIL {
        crate::kernel::printk::printk_emit_sync(format_args!(
            "SMP2_USER_FAIL cpu={} tid={} msg={} round={} aux=0x{:x} result=fail",
            cpu.0,
            tid,
            msg.trim_end(),
            round,
            aux
        ));
    }
    let step = rec::step_name(code);
    if step == "S_ARM_R" || step == "C_ARM_R" {
        // The requester names no target; the target is the other context-checked task.
        let (target_asid, frames, next) = if step == "C_ARM_R" {
            (S_ASID.load(Ordering::Acquire), &S_R_FRAMES, &S_R_NEXT)
        } else {
            (C_ASID.load(Ordering::Acquire), &C_R_FRAMES, &C_R_NEXT)
        };
        let index = next.fetch_add(1, Ordering::AcqRel) as usize;
        let Some(frame) = frames.get(index) else {
            crate::kernel::printk::printk_emit_sync(format_args!(
                "SMP2_WITNESS_R_EXHAUSTED asid={} result=fail",
                target_asid
            ));
            return;
        };
        let phys = frame.load(Ordering::Acquire);
        let ok = crate::arch::aarch64::page_table::witness_repoint_without_invalidation(
            Asid(target_asid as u16),
            VirtAddr(R_VA),
            PhysAddr(phys),
            PageFlags::USER_RW,
        );
        rec::push(
            Kind::RRepoint,
            cpu.0,
            [target_asid, index as u64 + 1, phys, round, u64::from(ok)],
        );
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

/// Seal, print every record and the verdict — all synchronously, so no line can be lost.
fn dump() {
    let mut out = [None; rec::SLOTS];
    let Some((n, overflowed)) = rec::seal(&mut out, 50_000_000) else {
        crate::kernel::printk::printk_emit_sync(format_args!(
            "SMP2_VERDICT result=fail reason=record_not_sealed"
        ));
        return;
    };
    let mut recs: alloc::vec::Vec<rec::Rec> = alloc::vec::Vec::with_capacity(n);
    recs.extend(out.iter().take(n).flatten().copied());
    let roles = roles();
    let v = rec::verify(&recs, &roles, overflowed);
    let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::with_capacity(n + 6);
    lines.push(alloc::format!(
        "SMP2_ROLES s_tid={} s_asid={} s_cpu={} c_tid={} c_asid={} c_cpu={} h1_tid={} h0_tid={} w_va=0x{:x} dump_cpu={}",
        roles.s.tid,
        roles.s.asid,
        roles.s.cpu,
        roles.c.tid,
        roles.c.asid,
        roles.c.cpu,
        roles.h1.tid,
        roles.h0.tid,
        roles.w_va,
        this_cpu()
    ));
    for r in &recs {
        lines.push(alloc::format!(
            "SMP2_REC seq={} kind={} cpu={} f0=0x{:x} f1=0x{:x} f2=0x{:x} f3=0x{:x} f4=0x{:x}{}{}",
            r.seq,
            r.kind.name(),
            r.cpu,
            r.f[0],
            r.f[1],
            r.f[2],
            r.f[3],
            r.f[4],
            if r.kind == Kind::User { " step=" } else { "" },
            if r.kind == Kind::User {
                rec::step_name(r.f[2])
            } else {
                ""
            }
        ));
    }
    for cpu in [CpuId(0), CpuId(1)] {
        let (total, el0, idle, kernel, sent) = crate::arch::aarch64::smp::sgi_counters(cpu);
        lines.push(alloc::format!(
            "SMP2_CPU cpu={} sgi_arrivals={} from_el0={} at_idle={} in_kernel={} sgi_sent={}",
            cpu.0,
            total,
            el0,
            idle,
            kernel,
            sent
        ));
    }
    // Every dump line stays well inside `printk_emit_sync`'s 192-byte line, checksum included.
    lines.push(alloc::format!(
        "SMP2_COUNTS records={} sgi_arrivals={} p1_parked={} p1_sgi_to_s={} p1_sgi_to_c={} p1_timer_first={} p2_el0={}",
        v.records,
        v.sgi_arrivals,
        v.p1_parked,
        v.p1_sgi_to_s,
        v.p1_sgi_to_c,
        v.p1_timer_first,
        v.p2_el0
    ));
    lines.push(alloc::format!(
        "SMP2_COUNTS tlb_rounds={} mutual_rounds={} mutual_overlapped={} settled_after_ack={}",
        v.tlb_rounds,
        v.mutual_rounds,
        v.mutual_overlapped,
        v.settled_after_ack
    ));
    lines.push(alloc::format!(
        "SMP2_VERDICT result={} reason={} at={}",
        if v.ok() { "ok" } else { "fail" },
        v.failure.unwrap_or("none"),
        v.failure_at
    ));
    // Twice, each line with its own checksum. The console is shared with raw, lock-free UART
    // markers another CPU may write mid-line (measured: one stray byte inside a record), so a
    // grader takes each line from whichever copy is intact and fails when neither is.
    for pass in 1..=2 {
        for line in &lines {
            crate::kernel::printk::printk_emit_sync(format_args!(
                "{} pass={} crc=0x{:08x}",
                line,
                pass,
                fnv1a(line.as_bytes())
            ));
        }
    }
}

/// FNV-1a over a dump line's text (everything before ` pass=`).
pub fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5u32, |h, &b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}
