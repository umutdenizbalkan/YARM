// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! VM-INVALIDATION1 — the RISC-V COW / fork invalidation witness. Built only with
//! `riscv64-vmi-witness` (which brings up the secondary hart through the QEMU-SMP3 machinery) and
//! armed only by `yarm.ap_user_dispatch=1` on a two-hart boot. When built, it REPLACES the four
//! QEMU-SMP3 tasks; the SMP3 records stay inert.
//!
//! Two kernel-built tasks share ONE address space: the requester R on CPU 0 and the observer O on
//! CPU 1. It exercises the two VM owners VM-INVALIDATION1 changed that the shared-region
//! replacement witness does not reach:
//!
//! * **COW** — O caches the read-only translation of a copy-on-write page C and spins on it with
//!   no kernel entry; R stores into C. The fault on CPU 0 runs the production split COW recovery,
//!   whose invalidation must name CPU 0 as its requester, reach CPU 1, and be acknowledged before
//!   the fault is reported recovered. O must then read R's byte.
//! * **fork** — O caches a WRITABLE translation of F and stores into it continuously with no
//!   kernel entry; R forks. The fork's write-protection invalidation must name CPU 0, reach CPU 1
//!   and complete before the child is published. The child reads F twice across a delay: the
//!   values are equal only if O's stores stopped reaching the frame the child shares.
//!
//! The witness performs no VM operation. Its kernel actions are setup, the secondary's placement
//! and one start-up kick (the SMP3 pattern), and observation: the production shootdown's own
//! target set and the firmware fence's result for C and for F (the FIRST of each, which is the
//! COW's and the fork's own), each hart's supervisor-entry count, and the user steps. The verdict
//! names the observed bytes and the kernel's completion evidence SEPARATELY; the smoke script
//! additionally requires the production owners' own markers.
//!
//! # Residency, on this port
//!
//! QEMU flushes a hart's TLB on every `satp` write and fence, and this kernel writes `satp` on
//! every return to U-mode — so an observation is credited only when O's hart took NO supervisor
//! entry between its priming and the step that depends on it.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::scheduler::{CpuId, MAX_CPUS};
use crate::kernel::vm::{Asid, CachePolicy, Mapping, PageFlags, PhysAddr, VirtAddr};

core::arch::global_asm!(include_str!("vmi_witness.S"));

pub const R_CODE_VA: u64 = 0x2000_0000;
pub const R_STACK_VA: u64 = 0x2001_0000;
pub const O_CODE_VA: u64 = 0x2002_0000;
pub const O_STACK_VA: u64 = 0x2003_0000;
pub const MBX_VA: u64 = 0x2007_0000;
pub const C_VA: u64 = 0x2008_0000;
pub const F_VA: u64 = 0x2009_0000;
pub const OLD_BYTE: u8 = 0x11;
pub const NEW_BYTE: u8 = 0x5A;

pub const R_TID: u64 = 9_500;
pub const O_TID: u64 = 9_501;

unsafe extern "C" {
    static yarm_vmiw_r_start: u8;
    static yarm_vmiw_r_end: u8;
    static yarm_vmiw_o_start: u8;
    static yarm_vmiw_o_end: u8;
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static ASID: AtomicU64 = AtomicU64::new(0);
static CPU1_STARTED: AtomicBool = AtomicBool::new(false);
static ENTRIES: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Per observed page (0 = C, 1 = F): the first production shootdown's requester and target set,
/// the first firmware fence's result (1 ok, 2 failed), and CPU 1's entry count when the
/// shootdown's target set was computed.
static REQUESTER: [AtomicU64; 2] = [const { AtomicU64::new(u64::MAX) }; 2];
static TARGETS: [AtomicU64; 2] = [const { AtomicU64::new(u64::MAX) }; 2];
static FENCE: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static FENCE_MASK: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static CPU1_AT_SHOOT: [AtomicU64; 2] = [const { AtomicU64::new(u64::MAX) }; 2];

/// CPU 1's entry count, and the requester, at the FIRST production shootdown in this space after
/// O primed F: the fork's write-protection invalidation begins there. The fork write-protects
/// every page before its first shootdown, and QEMU retires a hart's whole TLB on any fence, so
/// O's next store may fault at that first fence rather than at F's own.
static FORK_FIRST_SHOOT_CPU1: AtomicU64 = AtomicU64::new(u64::MAX);
static FORK_FIRST_SHOOT_REQUESTER: AtomicU64 = AtomicU64::new(u64::MAX);
static O_COW_READY_ENTRIES: AtomicU64 = AtomicU64::new(u64::MAX);
static O_FORK_READY_ENTRIES: AtomicU64 = AtomicU64::new(u64::MAX);
/// 1 = O read R's byte, 2 = stale / failed, 0 = not yet.
static O_COW: AtomicU64 = AtomicU64::new(0);
static O_COW_CREDITED: AtomicBool = AtomicBool::new(false);
static R_COW: AtomicU64 = AtomicU64::new(0);
static VERDICT: AtomicBool = AtomicBool::new(false);

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

fn page_of(asid: u16, va: u64) -> Option<usize> {
    if !enabled() || u64::from(asid) != ASID.load(Ordering::Acquire) {
        return None;
    }
    match va {
        C_VA => Some(0),
        F_VA => Some(1),
        _ => None,
    }
}

fn entries(cpu: usize) -> u64 {
    ENTRIES.get(cpu).map_or(0, |n| n.load(Ordering::Acquire))
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
    let r_len = &raw const yarm_vmiw_r_end as usize - &raw const yarm_vmiw_r_start as usize;
    let o_len = &raw const yarm_vmiw_o_end as usize - &raw const yarm_vmiw_o_start as usize;
    map_fresh(kernel, asid, R_CODE_VA, r_len.div_ceil(0x1000))?;
    map_fresh(kernel, asid, O_CODE_VA, o_len.div_ceil(0x1000))?;
    map_fresh(kernel, asid, R_STACK_VA, 1)?;
    map_fresh(kernel, asid, O_STACK_VA, 1)?;
    map_fresh(kernel, asid, MBX_VA, 1)?;
    map_fresh(kernel, asid, F_VA, 1)?;
    kernel.copy_to_user(asid, VirtAddr(MBX_VA), &[0u8; 0x100])?;
    kernel.copy_to_user(asid, VirtAddr(F_VA), &[0u8; 0x10])?;

    // C: one page of an allocator-owned object holding OLD_BYTE, mapped read-only in this space
    // AND in a second one, and copy-on-write here — the shape a fork leaves behind.
    let bootstrap = kernel
        .current_task_cnode()
        .ok_or(KernelError::TaskMissing)?;
    let (c_id, c_cap) = kernel.alloc_anonymous_memory_object()?;
    let c_phys = kernel
        .with_memory_state(|m| {
            m.memory_objects
                .iter()
                .flatten()
                .find(|o| o.id == c_id)
                .map(|o| o.phys.0)
        })
        .ok_or(KernelError::MemoryObjectMissing)?;
    fill(c_phys, OLD_BYTE)?;
    let (other, _other_root) = kernel.create_user_address_space()?;
    let ro = PageFlags {
        read: true,
        write: false,
        execute: false,
        user: true,
        cache_policy: CachePolicy::WriteBack,
    };
    for space in [asid, other] {
        kernel.map_user_page_in_asid_raw(
            space,
            VirtAddr(C_VA),
            Mapping {
                phys: PhysAddr(c_phys),
                flags: ro,
            },
        )?;
    }
    kernel.with_memory_state_mut(|m| {
        crate::kernel::boot::cow_clone::mark_cow_page_locked(m, asid, VirtAddr(C_VA))
    })?;
    kernel.revoke_capability_in_cnode(bootstrap, c_cap)?;

    bind(kernel, R_TID, asid, R_CODE_VA, R_STACK_VA + 0xff0)?;
    bind(kernel, O_TID, asid, O_CODE_VA, O_STACK_VA + 0xff0)?;
    kernel.set_task_home_cpu(R_TID, CpuId(0))?;
    kernel.set_task_home_cpu(O_TID, CpuId(1))?;

    // SAFETY: `start..end` is each program's bytes inside this kernel image's text.
    let r_image = unsafe { core::slice::from_raw_parts(&raw const yarm_vmiw_r_start, r_len) };
    let o_image = unsafe { core::slice::from_raw_parts(&raw const yarm_vmiw_o_start, o_len) };
    kernel.copy_to_user(asid, VirtAddr(R_CODE_VA), r_image)?;
    kernel.copy_to_user(asid, VirtAddr(O_CODE_VA), o_image)?;

    ASID.store(u64::from(asid.0), Ordering::Release);
    kernel.enqueue_task(R_TID)?;
    ENABLED.store(true, Ordering::Release);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMIW_WITNESS_PROVISIONED r_tid={} o_tid={} asid={} c_va=0x{:x} c_object={} c_phys=0x{:x} f_va=0x{:x} r_image={} o_image={}",
        R_TID,
        O_TID,
        asid.0,
        C_VA,
        c_id,
        c_phys,
        F_VA,
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
            "VMIW_WITNESS_PLACEMENT_FAIL cpu={} err={:?} result=fail",
            cpu.0, e
        ));
        return;
    }
    let kicked = crate::arch::riscv64::ipi::kick_self(cpu);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMIW_WITNESS_SECONDARY_PLACED cpu={} tid={} kick={}",
        cpu.0,
        O_TID,
        u8::from(kicked)
    ));
}

/// Every supervisor entry on `cpu`, from either origin.
pub fn note_supervisor_entry(cpu: CpuId) {
    if let Some(n) = ENTRIES.get(cpu.0 as usize) {
        n.fetch_add(1, Ordering::AcqRel);
    }
}

/// The production shootdown's target set for C or F — the first one only (observation only).
pub fn note_shoot_targets(requester: CpuId, asid: u16, va: u64, targets: u64) {
    if enabled()
        && u64::from(asid) == ASID.load(Ordering::Acquire)
        && O_FORK_READY_ENTRIES.load(Ordering::Acquire) != u64::MAX
        && FORK_FIRST_SHOOT_CPU1
            .compare_exchange(u64::MAX, entries(1), Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    {
        FORK_FIRST_SHOOT_REQUESTER.store(u64::from(requester.0), Ordering::Release);
        crate::kernel::printk::printk_emit_sync(format_args!(
            "VMIW_KERNEL_FORK_FIRST_SHOOT requester_cpu={} asid={} va=0x{:x} targets=0x{:x} cpu1_entries={}",
            requester.0,
            asid,
            va,
            targets,
            entries(1)
        ));
    }
    let Some(page) = page_of(asid, va) else {
        return;
    };
    if TARGETS[page]
        .compare_exchange(u64::MAX, targets, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        REQUESTER[page].store(u64::from(requester.0), Ordering::Release);
        CPU1_AT_SHOOT[page].store(entries(1), Ordering::Release);
        crate::kernel::printk::printk_emit_sync(format_args!(
            "VMIW_KERNEL_SHOOT page={} requester_cpu={} asid={} va=0x{:x} targets=0x{:x} cpu1_entries={}",
            if page == 0 { "C" } else { "F" },
            requester.0,
            asid,
            va,
            targets,
            entries(1)
        ));
    }
}

/// The firmware remote fence's result for C or F — the first one only (observation only).
pub fn note_fence_done(asid: u16, va: u64, hart_mask: usize, ok: bool) {
    let Some(page) = page_of(asid, va) else {
        return;
    };
    if FENCE[page]
        .compare_exchange(
            0,
            if ok { 1 } else { 2 },
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        FENCE_MASK[page].store(hart_mask as u64, Ordering::Release);
    }
}

/// The DebugLog observation point. Returns `true` when the message was this witness's.
pub fn observe_user_marker(cpu: CpuId, tid: u64, msg: &str, round: u64, aux: u64) -> bool {
    if !enabled() || !msg.starts_with("VMIW ") {
        return false;
    }
    let here = entries(cpu.0 as usize);
    let step = msg.trim_end().trim_start_matches("VMIW ");
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMIW_USER cpu={} tid={} step={} round=0x{:x} aux=0x{:x} entries={}",
        cpu.0, tid, step, round, aux, here
    ));
    match step {
        "O_COW_READY" => O_COW_READY_ENTRIES.store(here, Ordering::Release),
        "O_FORK_READY" => O_FORK_READY_ENTRIES.store(here, Ordering::Release),
        "O_COW_NEW" | "O_COW_STALE" | "O_PRIME_FAIL" => {
            // This step's own entry is the one after O_COW_READY's: nothing else ran on O's hart.
            let ready = O_COW_READY_ENTRIES.load(Ordering::Acquire);
            O_COW_CREDITED.store(ready != u64::MAX && here == ready + 1, Ordering::Release);
            O_COW.store(
                if step == "O_COW_NEW" && aux == u64::from(NEW_BYTE) {
                    1
                } else {
                    2
                },
                Ordering::Release,
            );
        }
        "R_COW_OK" => R_COW.store(1, Ordering::Release),
        "R_COW_FAIL" | "R_NO_OBSERVER" | "R_FORK_FAIL" => R_COW.store(2, Ordering::Release),
        "C_F" if !VERDICT.swap(true, Ordering::AcqRel) => verdict(round, aux),
        _ => {}
    }
    true
}

/// The verdict, at the fork child's report: both phases' observed bytes and kernel evidence,
/// graded separately.
fn verdict(v1: u64, v2: u64) {
    let fork_ready = O_FORK_READY_ENTRIES.load(Ordering::Acquire);
    // COW
    let cow_bytes = R_COW.load(Ordering::Acquire) == 1
        && O_COW.load(Ordering::Acquire) == 1
        && O_COW_CREDITED.load(Ordering::Acquire);
    let cow_kernel = REQUESTER[0].load(Ordering::Acquire) == 0
        && TARGETS[0].load(Ordering::Acquire) != u64::MAX
        && TARGETS[0].load(Ordering::Acquire) & 0b10 != 0
        && FENCE[0].load(Ordering::Acquire) == 1;
    // fork: O stored with no supervisor entry from its priming to the fork's first shootdown (a
    // requester-0 one), the child saw F frozen, and O's next store faulted afterwards (CPU 1
    // entered once more).
    let fork_credited = fork_ready != u64::MAX
        && FORK_FIRST_SHOOT_CPU1.load(Ordering::Acquire) == fork_ready
        && FORK_FIRST_SHOOT_REQUESTER.load(Ordering::Acquire) == 0;
    let o_faulted = fork_ready != u64::MAX && entries(1) > fork_ready;
    let fork_bytes = v1 == v2 && v1 != 0 && fork_credited;
    let fork_kernel = REQUESTER[1].load(Ordering::Acquire) == 0
        && TARGETS[1].load(Ordering::Acquire) != u64::MAX
        && TARGETS[1].load(Ordering::Acquire) & 0b10 != 0
        && FENCE[1].load(Ordering::Acquire) == 1;
    // Three lines: one printk record is bounded, and the full detail does not fit in one.
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMIW_DETAIL_COW cow_bytes={} cow_credited={} cow_kernel={} cow_requester={} cow_targets=0x{:x} cow_fence={}",
        u8::from(cow_bytes),
        u8::from(O_COW_CREDITED.load(Ordering::Acquire)),
        u8::from(cow_kernel),
        REQUESTER[0].load(Ordering::Acquire) as i64,
        TARGETS[0].load(Ordering::Acquire),
        FENCE[0].load(Ordering::Acquire),
    ));
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMIW_DETAIL_FORK fork_bytes={} child_f=0x{:x}/0x{:x} fork_credited={} o_faulted={} fork_kernel={} fork_requester={} fork_targets=0x{:x} fork_fence={}",
        u8::from(fork_bytes),
        v1,
        v2,
        u8::from(fork_credited),
        u8::from(o_faulted),
        u8::from(fork_kernel),
        REQUESTER[1].load(Ordering::Acquire) as i64,
        TARGETS[1].load(Ordering::Acquire),
        FENCE[1].load(Ordering::Acquire),
    ));
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMIW_VERDICT cow={} fork={} o_faulted={} result={}",
        u8::from(cow_bytes && cow_kernel),
        u8::from(fork_bytes && fork_kernel),
        u8::from(o_faulted),
        if cow_bytes && cow_kernel && fork_bytes && fork_kernel && o_faulted {
            "ok"
        } else {
            "fail"
        }
    ));
}
