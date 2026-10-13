// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SHARED-REGION-LIFETIME2 — the RISC-V shared-region replacement witness. Built only with
//! `riscv64-sr-replace-witness` (which brings up the secondary hart through the QEMU-SMP3
//! machinery) and armed only by `yarm.ap_user_dispatch=1` on a two-hart boot. When built, it
//! REPLACES the four QEMU-SMP3 tasks; the SMP3 records stay inert.
//!
//! Two kernel-built tasks share ONE address space: the receiver R on CPU 0 and the observer O on
//! CPU 1. O caches the translation of the window W and spins on it with no kernel entry; R then
//! receives a shared region with NR 2 mapped AT W, so the receive's install REPLACES a page whose
//! translation is live on the other hart. That is exactly the path SHARED-REGION-LIFETIME2
//! changed: `map_user_page_raw_split` must hold the displaced page, ask the requester-explicit
//! shootdown to reach O's hart, settle the hold only on the acknowledgement, and report the
//! outcome — and the receive may succeed only if that invalidation completed.
//!
//! The witness performs none of that. Its kernel actions are setup, the secondary's placement and
//! one start-up kick (the SMP3 pattern), and observation: the production shootdown's own target
//! set (`note_shoot_targets`), the firmware fence's result (`note_fence_done`), the install's
//! outcome and the settlement (`note_install`, `note_settle`), and the user steps. The verdict
//! names the observed bytes (both tasks' own loads) and the kernel's completion evidence
//! SEPARATELY.
//!
//! # Residency, on this port
//!
//! QEMU flushes a hart's TLB on every `satp` write and fence, and this kernel writes `satp` on
//! every return to U-mode — so O's observation is credited only when its hart took NO supervisor
//! entry between O_READY (after which O primes W) and the O_NEW step itself. Then the only thing
//! that can have retired O's cached translation of W is the remote fence R's install requested.
//!
//! # Causal order, not console order (AP-SCHED1)
//!
//! Each record that the grader orders carries `seq=`, drawn from ONE witness-only atomic counter
//! at the moment the event happens: the target set, the fence request, the fence's completion,
//! the install's outcome, the settlement, and every user step. One `fetch_add` on one location is
//! a single total order that every hart agrees on, and each stamp is drawn AFTER the event it
//! names (a user step's after the task's own load, the fence request's before the firmware call).
//! So a remote observer's step may legitimately be PRINTED before the requester's install line,
//! which is only printed once the outcome is known, and yet carry a later stamp than the fence
//! request it depends on. The grader orders by `seq`, never by line position. Nothing here
//! waits: a stamp is one `AcqRel` read-modify-write, and the install line keeps its single print.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
use crate::kernel::scheduler::{CpuId, MAX_CPUS};
use crate::kernel::vm::{Asid, CachePolicy, Mapping, PageFlags, PhysAddr, VirtAddr};

core::arch::global_asm!(include_str!("sr_replace_witness.S"));

pub const R_CODE_VA: u64 = 0x2000_0000;
pub const R_STACK_VA: u64 = 0x2001_0000;
pub const O_CODE_VA: u64 = 0x2002_0000;
pub const O_STACK_VA: u64 = 0x2003_0000;
pub const MBX_VA: u64 = 0x2007_0000;
pub const W_VA: u64 = 0x2008_0000;
pub const OLD_BYTE: u8 = 0x11;
pub const NEW_BYTE: u8 = 0x5A;

pub const R_TID: u64 = 9_400;
pub const O_TID: u64 = 9_401;

unsafe extern "C" {
    static yarm_srr_r_start: u8;
    static yarm_srr_r_end: u8;
    static yarm_srr_r_cap_ep: u8;
    static yarm_srr_r_cap_src: u8;
    static yarm_srr_o_start: u8;
    static yarm_srr_o_end: u8;
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static ASID: AtomicU64 = AtomicU64::new(0);
static OLD_OBJECT: AtomicU64 = AtomicU64::new(u64::MAX);
static OLD_PHYS: AtomicU64 = AtomicU64::new(0);
static CPU1_STARTED: AtomicBool = AtomicBool::new(false);
static ENTRIES: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static O_READY_ENTRIES: AtomicU64 = AtomicU64::new(u64::MAX);
/// The production shootdown's own target set for W, as `complete_unmap_shootdown_from_split`
/// computed it; `u64::MAX` until it ran.
static TARGETS: AtomicU64 = AtomicU64::new(u64::MAX);
static REQUESTER: AtomicU64 = AtomicU64::new(u64::MAX);
/// 1 = the firmware fence returned success, 2 = it failed, 0 = none was asked for.
static FENCE: AtomicU64 = AtomicU64::new(0);
static FENCE_MASK: AtomicU64 = AtomicU64::new(0);
/// 1 = complete, 2 = invalidation incomplete, 0 = no displacing install seen.
static INSTALL: AtomicU64 = AtomicU64::new(0);
/// 1 = the displaced page's hold settled and its object was reclaimed, 2 = settled, not
/// reclaimed, 0 = not settled.
static SETTLE: AtomicU64 = AtomicU64::new(0);
static R_RESULT: AtomicU64 = AtomicU64::new(0);
static O_RESULT: AtomicU64 = AtomicU64::new(0);
static O_CREDITED: AtomicBool = AtomicBool::new(false);
static VERDICT: AtomicBool = AtomicBool::new(false);
/// AP-SCHED1 — the causal stamp source, and the stamps of the kernel events the install line
/// reports (0 = the event did not happen).
static SEQ: AtomicU64 = AtomicU64::new(0);
static TARGETS_SEQ: AtomicU64 = AtomicU64::new(0);
static FENCE_REQ_SEQ: AtomicU64 = AtomicU64::new(0);
static FENCE_REQ_MASK: AtomicU64 = AtomicU64::new(0);
static FENCE_DONE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Draw the next causal stamp (1, 2, ...).
fn stamp() -> u64 {
    SEQ.fetch_add(1, Ordering::AcqRel) + 1
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

fn watched(asid: u16, va: u64) -> bool {
    enabled() && u64::from(asid) == ASID.load(Ordering::Acquire) && va == W_VA
}

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

fn map_fresh(
    kernel: &mut KernelState,
    asid: Asid,
    va: u64,
    count: usize,
) -> Result<(), KernelError> {
    for i in 0..count {
        let phys = kernel.alloc_user_data_frame()?;
        kernel.map_user_page_in_asid_raw(
            asid,
            VirtAddr(va + (i as u64) * 0x1000),
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
    Ok(())
}

fn object_phys(kernel: &KernelState, id: u64) -> Result<u64, KernelError> {
    kernel
        .with_memory_state(|m| {
            m.memory_objects
                .iter()
                .flatten()
                .find(|o| o.id == id)
                .map(|o| o.phys.0)
        })
        .ok_or(KernelError::MemoryObjectMissing)
}

fn fill(phys: u64, byte: u8) -> Result<(), KernelError> {
    for off in 0..0x1000u64 {
        let ptr =
            KernelState::phys_to_direct_map_ptr(phys + off).ok_or(KernelError::UserMemoryFault)?;
        // SAFETY: the frame belongs to an object this provisioning just allocated; nothing maps it
        // yet, and the direct map covers it.
        unsafe { core::ptr::write_volatile(ptr, byte) };
    }
    Ok(())
}

fn bind(
    kernel: &mut KernelState,
    tid: u64,
    asid: Asid,
    ip: u64,
    sp: u64,
) -> Result<(), KernelError> {
    kernel.register_task_with_class(tid, crate::kernel::task::TaskClass::App)?;
    kernel.with_tcbs_mut(|tcbs| {
        let tcb = tcbs
            .iter_mut()
            .flatten()
            .find(|t| t.tid.0 == tid)
            .ok_or(KernelError::TaskMissing)?;
        // Status is left as registration made it (`Runnable`): the witness adds no transition.
        tcb.asid = Some(asid);
        tcb.user_context.instruction_ptr = VirtAddr(ip);
        tcb.user_context.stack_ptr = VirtAddr(sp);
        Ok::<_, KernelError>(())
    })
}

/// Provision R and O from the bootstrap window (`&mut KernelState`). R is enqueued here; O is
/// placed by the secondary once it has been admitted.
pub fn provision(kernel: &mut KernelState) -> Result<(), KernelError> {
    let (asid, _as_root) = kernel.create_user_address_space()?;
    // SAFETY (all `&raw const` below): taking the address of an extern static reads nothing.
    let r_len = &raw const yarm_srr_r_end as usize - &raw const yarm_srr_r_start as usize;
    let o_len = &raw const yarm_srr_o_end as usize - &raw const yarm_srr_o_start as usize;
    map_fresh(kernel, asid, R_CODE_VA, r_len.div_ceil(0x1000))?;
    map_fresh(kernel, asid, O_CODE_VA, o_len.div_ceil(0x1000))?;
    map_fresh(kernel, asid, R_STACK_VA, 1)?;
    map_fresh(kernel, asid, O_STACK_VA, 1)?;
    map_fresh(kernel, asid, MBX_VA, 1)?;
    kernel.copy_to_user(asid, VirtAddr(MBX_VA), &[0u8; 0x100])?;

    // W: one page of an allocator-owned object holding OLD_BYTE, living by this mapping alone, so
    // its hold's settlement is the one reclaim that may free it.
    let bootstrap = kernel
        .current_task_cnode()
        .ok_or(KernelError::TaskMissing)?;
    let (old_id, old_cap) = kernel.alloc_anonymous_memory_object()?;
    let old_phys = object_phys(kernel, old_id)?;
    fill(old_phys, OLD_BYTE)?;
    kernel.map_user_page_in_asid_with_caps(asid, old_cap, VirtAddr(W_VA), PageFlags::USER_RW)?;
    kernel.revoke_capability_in_cnode(bootstrap, old_cap)?;

    // The granted object: one page of NEW_BYTE.
    let (new_id, _new_cap) = kernel.alloc_anonymous_memory_object()?;
    let new_phys = object_phys(kernel, new_id)?;
    fill(new_phys, NEW_BYTE)?;

    bind(kernel, R_TID, asid, R_CODE_VA, R_STACK_VA + 0xff0)?;
    bind(kernel, O_TID, asid, O_CODE_VA, O_STACK_VA + 0xff0)?;
    kernel.set_task_home_cpu(R_TID, CpuId(0))?;
    kernel.set_task_home_cpu(O_TID, CpuId(1))?;

    let (_, _, ep) = kernel.create_endpoint(1)?;
    let ep_object = kernel
        .current_task_capability(ep)
        .ok_or(KernelError::InvalidCapability)?
        .object;
    let r_cnode = kernel.task_cnode(R_TID).ok_or(KernelError::TaskMissing)?;
    let cap32 = |c: CapId| u32::try_from(c.0).map_err(|_| KernelError::CapabilityFull);
    let r_ep = kernel.mint_capability_in_cnode(
        r_cnode,
        Capability::new(ep_object, CapRights::SEND | CapRights::RECEIVE),
    )?;
    let r_src = kernel.mint_capability_in_cnode(
        r_cnode,
        Capability::new(
            CapObject::MemoryObject { id: new_id },
            CapRights::READ | CapRights::MAP,
        ),
    )?;
    let r_image = patched(
        &raw const yarm_srr_r_start,
        &raw const yarm_srr_r_end,
        &[
            (&raw const yarm_srr_r_cap_ep, cap32(r_ep)?),
            (&raw const yarm_srr_r_cap_src, cap32(r_src)?),
        ],
    );
    let o_image = patched(&raw const yarm_srr_o_start, &raw const yarm_srr_o_end, &[]);
    kernel.copy_to_user(asid, VirtAddr(R_CODE_VA), &r_image)?;
    kernel.copy_to_user(asid, VirtAddr(O_CODE_VA), &o_image)?;

    ASID.store(u64::from(asid.0), Ordering::Release);
    OLD_OBJECT.store(old_id, Ordering::Release);
    OLD_PHYS.store(old_phys, Ordering::Release);
    kernel.enqueue_task(R_TID)?;
    ENABLED.store(true, Ordering::Release);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "SRR_WITNESS_PROVISIONED r_tid={} o_tid={} asid={} w_va=0x{:x} old_object={} old_phys=0x{:x} new_object={} new_phys=0x{:x} r_image={} o_image={}",
        R_TID,
        O_TID,
        asid.0,
        W_VA,
        old_id,
        old_phys,
        new_id,
        new_phys,
        r_image.len(),
        o_image.len()
    ));
    Ok(())
}

/// The admitted secondary places O on itself and takes one start-up kick (the SMP3 pattern: it
/// has no timer, so a task placed on it outside a trap needs one interrupt to be dispatched).
pub fn secondary_admitted(shared: &crate::runtime::SharedKernel, cpu: CpuId) {
    if !enabled() || cpu.0 != 1 || CPU1_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Err(e) = shared.enqueue_on_cpu_split(cpu, O_TID) {
        crate::kernel::printk::printk_emit_sync(format_args!(
            "SRR_WITNESS_PLACEMENT_FAIL cpu={} err={:?} result=fail",
            cpu.0, e
        ));
        return;
    }
    let kicked = crate::arch::riscv64::ipi::kick_self(cpu);
    // The hart this logical CPU really is: the fence's hart mask is graded against it.
    let hart =
        crate::arch::riscv64::boot::hart_id_of_logical_cpu(cpu.0 as usize).map_or(-1, |h| h as i64);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "SRR_WITNESS_SECONDARY_PLACED cpu={} tid={} kick={} hart={}",
        cpu.0,
        O_TID,
        u8::from(kicked),
        hart
    ));
}

/// Every supervisor entry on `cpu`, from either origin.
pub fn note_supervisor_entry(cpu: CpuId) {
    if let Some(n) = ENTRIES.get(cpu.0 as usize) {
        n.fetch_add(1, Ordering::AcqRel);
    }
}

/// The production shootdown's target set for W (observation only).
pub fn note_shoot_targets(requester: CpuId, asid: u16, va: u64, targets: u64) {
    if !watched(asid, va) {
        return;
    }
    TARGETS_SEQ.store(stamp(), Ordering::Release);
    TARGETS.store(targets, Ordering::Release);
    REQUESTER.store(u64::from(requester.0), Ordering::Release);
}

/// The firmware remote fence for W is about to be requested (after the requester's `fence`).
pub fn note_fence_request(asid: u16, va: u64, hart_mask: usize) {
    if !watched(asid, va) {
        return;
    }
    FENCE_REQ_MASK.store(hart_mask as u64, Ordering::Release);
    FENCE_REQ_SEQ.store(stamp(), Ordering::Release);
}

/// The firmware remote fence's result for W (observation only).
pub fn note_fence_done(asid: u16, va: u64, hart_mask: usize, ok: bool) {
    if !watched(asid, va) {
        return;
    }
    FENCE_DONE_SEQ.store(stamp(), Ordering::Release);
    FENCE_MASK.store(hart_mask as u64, Ordering::Release);
    FENCE.store(if ok { 1 } else { 2 }, Ordering::Release);
}

/// A displacing install at W, with the outcome `map_user_page_raw_split` reports.
pub fn note_install(
    requester: CpuId,
    asid: u16,
    va: u64,
    old_phys: u64,
    new_phys: u64,
    hold: Option<u64>,
    complete: bool,
) {
    if !watched(asid, va) {
        return;
    }
    let seq = stamp();
    INSTALL.store(if complete { 1 } else { 2 }, Ordering::Release);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "SRR_KERNEL_INSTALL route=queued requester_cpu={} asid={} va=0x{:x} old_phys=0x{:x} new_phys=0x{:x} hold={} targets=0x{:x} fence={} fence_hart_mask=0x{:x} outcome={}",
        requester.0,
        asid,
        va,
        old_phys,
        new_phys,
        hold.map_or(-1, |h| h as i64),
        TARGETS.load(Ordering::Acquire),
        FENCE.load(Ordering::Acquire),
        FENCE_MASK.load(Ordering::Acquire),
        if complete {
            "complete"
        } else {
            "invalidation_incomplete"
        },
    ));
    // A second record: one printk record is bounded, and the stamps do not fit beside the facts.
    crate::kernel::printk::printk_emit_sync(format_args!(
        "SRR_KERNEL_INSTALL_ORDER seq={} targets_seq={} fence_req_seq={} fence_req_mask=0x{:x} fence_done_seq={}",
        seq,
        TARGETS_SEQ.load(Ordering::Acquire),
        FENCE_REQ_SEQ.load(Ordering::Acquire),
        FENCE_REQ_MASK.load(Ordering::Acquire),
        FENCE_DONE_SEQ.load(Ordering::Acquire),
    ));
}

/// The settlement of the displaced page's hold (after an acknowledged invalidation).
pub fn note_settle(object: u64, phys: u64, reclaimed: bool) {
    if !enabled() || phys != OLD_PHYS.load(Ordering::Acquire) {
        return;
    }
    let seq = stamp();
    SETTLE.store(if reclaimed { 1 } else { 2 }, Ordering::Release);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "SRR_KERNEL_SETTLE object={} expected_object={} phys=0x{:x} reclaimed={} seq={}",
        object,
        OLD_OBJECT.load(Ordering::Acquire),
        phys,
        u8::from(reclaimed),
        seq
    ));
}

/// The DebugLog observation point. Returns `true` when the message was this witness's.
pub fn observe_user_marker(cpu: CpuId, tid: u64, msg: &str, round: u64, aux: u64) -> bool {
    if !enabled() || !msg.starts_with("SRR ") {
        return false;
    }
    // Drawn at the step's own supervisor entry, after the task's load it reports.
    let seq = stamp();
    let entries = ENTRIES
        .get(cpu.0 as usize)
        .map_or(0, |n| n.load(Ordering::Acquire));
    let step = msg.trim_end();
    crate::kernel::printk::printk_emit_sync(format_args!(
        "SRR_USER cpu={} tid={} step={} round={} aux=0x{:x} entries={} seq={}",
        cpu.0,
        tid,
        step.trim_start_matches("SRR "),
        round,
        aux,
        entries,
        seq
    ));
    match step {
        "SRR O_READY" => O_READY_ENTRIES.store(entries, Ordering::Release),
        "SRR O_NEW" | "SRR O_STALE" | "SRR O_PRIME_FAIL" => {
            // This step's own entry is the one after O_READY's: nothing else ran on O's hart.
            let ready = O_READY_ENTRIES.load(Ordering::Acquire);
            O_CREDITED.store(ready != u64::MAX && entries == ready + 1, Ordering::Release);
            O_RESULT.store(
                match step {
                    "SRR O_NEW" if aux == u64::from(NEW_BYTE) => 1,
                    _ => 2,
                },
                Ordering::Release,
            );
        }
        "SRR R_OK" => R_RESULT.store(1, Ordering::Release),
        "SRR R_SEND_FAIL" | "SRR R_RECV_FAIL" | "SRR R_BYTES_FAIL" | "SRR R_NO_OBSERVER" => {
            R_RESULT.store(2, Ordering::Release)
        }
        _ => {}
    }
    let (r, o) = (
        R_RESULT.load(Ordering::Acquire),
        O_RESULT.load(Ordering::Acquire),
    );
    if r != 0 && o != 0 && !VERDICT.swap(true, Ordering::AcqRel) {
        let targets = TARGETS.load(Ordering::Acquire);
        let remote_included = targets != u64::MAX && targets & 0b10 != 0;
        let bytes_ok = r == 1 && o == 1 && O_CREDITED.load(Ordering::Acquire);
        let kernel_ok = remote_included
            && REQUESTER.load(Ordering::Acquire) == 0
            && FENCE.load(Ordering::Acquire) == 1
            && INSTALL.load(Ordering::Acquire) == 1
            && SETTLE.load(Ordering::Acquire) == 1;
        crate::kernel::printk::printk_emit_sync(format_args!(
            "SRR_VERDICT bytes={} receiver={} observer={} observer_credited={} kernel={} requester={} targets=0x{:x} fence={} install={} settle={} result={}",
            u8::from(bytes_ok),
            r,
            o,
            u8::from(O_CREDITED.load(Ordering::Acquire)),
            u8::from(kernel_ok),
            REQUESTER.load(Ordering::Acquire) as i64,
            targets,
            FENCE.load(Ordering::Acquire),
            INSTALL.load(Ordering::Acquire),
            SETTLE.load(Ordering::Acquire),
            if bytes_ok && kernel_ok { "ok" } else { "fail" }
        ));
    }
    true
}
