// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP3-ACCEPTANCE §2 — the AArch64 provisioning of the overtaken-deferral witness
//! (`crate::kernel::overtaken_witness`). Built only with `aarch64-overtaken-witness` and armed only
//! together with `yarm.ap_user_dispatch=1` on a two-CPU boot.
//!
//! Setup only: two tasks in their own address spaces with ONE shared mailbox frame, W placed on
//! CPU 0 from the bootstrap window and K placed on CPU 1 by the admitted AP (with the one start-up
//! kick the SMP2 witness also takes, because the AP has no timer). Everything they then do is a
//! real syscall through the production owners.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::overtaken_witness as ow;
use crate::kernel::scheduler::CpuId;
use crate::kernel::vm::{Asid, CachePolicy, Mapping, PageFlags, PhysAddr, VirtAddr};

core::arch::global_asm!(include_str!("overtaken_witness.S"));

pub const CODE_VA: u64 = 0x2000_0000;
pub const STACK_VA: u64 = 0x2001_0000;
pub const SP_TOP: u64 = 0x2001_0ff0;

pub const W_TID: u64 = 9_400;
pub const K_TID: u64 = 9_401;

unsafe extern "C" {
    static yarm_ovt_w_start: u8;
    static yarm_ovt_w_end: u8;
    static yarm_ovt_k_start: u8;
    static yarm_ovt_k_end: u8;
}

static PROVISIONED: AtomicBool = AtomicBool::new(false);
static CPU1_STARTED: AtomicBool = AtomicBool::new(false);
static K_ASID: AtomicU64 = AtomicU64::new(0);

const CODE_FLAGS: PageFlags = PageFlags {
    read: true,
    write: true,
    execute: true,
    user: true,
    cache_policy: CachePolicy::WriteBack,
};

fn image(start: *const u8, end: *const u8) -> alloc::vec::Vec<u8> {
    let len = end as usize - start as usize;
    // SAFETY: `start..end` is the program's bytes inside this kernel image's text.
    unsafe { core::slice::from_raw_parts(start, len) }.to_vec()
}

fn map_fresh(
    kernel: &mut KernelState,
    asid: Asid,
    va: u64,
    count: usize,
    flags: PageFlags,
) -> Result<(), KernelError> {
    for i in 0..count {
        let phys = kernel.alloc_user_data_frame()?;
        kernel.map_user_page_in_asid_raw(
            asid,
            VirtAddr(va + (i as u64) * 0x1000),
            Mapping {
                phys: PhysAddr(phys),
                flags,
            },
        )?;
    }
    Ok(())
}

/// One task: its own address space, code, stack, the shared mailbox, TCB and home CPU.
fn build_task(
    kernel: &mut KernelState,
    tid: u64,
    code: &[u8],
    mbx: u64,
    home: CpuId,
) -> Result<Asid, KernelError> {
    let (asid, _) = kernel.create_user_address_space()?;
    map_fresh(
        kernel,
        asid,
        CODE_VA,
        code.len().div_ceil(0x1000),
        CODE_FLAGS,
    )?;
    map_fresh(kernel, asid, STACK_VA, 1, PageFlags::USER_RW)?;
    kernel.map_user_page_in_asid_raw(
        asid,
        VirtAddr(ow::MBX_VA),
        Mapping {
            phys: PhysAddr(mbx),
            flags: PageFlags::USER_RW,
        },
    )?;
    kernel.copy_to_user(asid, VirtAddr(CODE_VA), code)?;
    kernel.register_task_with_class(tid, crate::kernel::task::TaskClass::App)?;
    kernel.with_tcbs_mut(|tcbs| {
        let tcb = tcbs
            .iter_mut()
            .flatten()
            .find(|t| t.tid.0 == tid)
            .ok_or(KernelError::TaskMissing)?;
        tcb.asid = Some(asid);
        tcb.user_context.instruction_ptr = VirtAddr(CODE_VA);
        tcb.user_context.stack_ptr = VirtAddr(SP_TOP);
        Ok::<_, KernelError>(())
    })?;
    kernel.set_task_home_cpu(tid, home)?;
    Ok(asid)
}

/// Provision both tasks from the bootstrap window. W is enqueued on CPU 0 here; K is placed by the
/// AP once it is admitted ([`ap_admitted`]).
pub fn provision(kernel: &mut KernelState) -> Result<(), KernelError> {
    let mbx = kernel.alloc_user_data_frame()?;
    // SAFETY (all `&raw const` below): taking the address of an extern static reads nothing.
    let w_code = image(&raw const yarm_ovt_w_start, &raw const yarm_ovt_w_end);
    let k_code = image(&raw const yarm_ovt_k_start, &raw const yarm_ovt_k_end);
    let w_asid = build_task(kernel, W_TID, &w_code, mbx, CpuId(0))?;
    let k_asid = build_task(kernel, K_TID, &k_code, mbx, CpuId(1))?;
    kernel.copy_to_user(w_asid, VirtAddr(ow::MBX_VA), &[0u8; 0x100])?;
    K_ASID.store(u64::from(k_asid.0), Ordering::Release);
    kernel.enqueue_task(W_TID)?;
    PROVISIONED.store(true, Ordering::Release);
    ow::arm(W_TID, w_asid.0, K_TID, k_asid.0);
    Ok(())
}

/// The admitted AP places K on itself and takes one start-up kick.
pub fn ap_admitted(shared: &crate::runtime::SharedKernel, cpu: CpuId) {
    if !PROVISIONED.load(Ordering::Acquire)
        || cpu.0 != 1
        || CPU1_STARTED.swap(true, Ordering::AcqRel)
    {
        return;
    }
    if let Err(e) = shared.enqueue_on_cpu_split(cpu, K_TID) {
        crate::kernel::printk::printk_emit_sync(format_args!(
            "OVT_WITNESS_AP_PLACEMENT_FAIL cpu={} tid={} err={:?} result=fail",
            cpu.0, K_TID, e
        ));
        return;
    }
    let kicked = crate::arch::aarch64::smp::kick_self(cpu);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "OVT_WITNESS_AP_PLACED cpu={} tid={} asid={} kick={}",
        cpu.0,
        K_TID,
        K_ASID.load(Ordering::Acquire),
        u8::from(kicked)
    ));
}
