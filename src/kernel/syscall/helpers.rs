// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 149: shared cross-boundary syscall helpers ([S] group).
//!
//! Mechanically extracted from `syscall.rs` with zero behavior change.
//! `syscall.rs` re-exports all items so call sites in sibling modules and
//! external callers (`runtime.rs`) are unaffected.

use super::SyscallError;
use crate::kernel::boot::KernelState;
use crate::kernel::capabilities::{CapId, CapObject, CapRights};
use crate::kernel::trap::{FaultAccess, FaultInfo};
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::{PAGE_SIZE, VirtAddr};

pub(super) fn current_tid(kernel: &KernelState) -> Result<u64, SyscallError> {
    kernel.current_tid().ok_or(SyscallError::Internal)
}

pub(super) fn current_task_has_user_asid(kernel: &KernelState) -> Result<bool, SyscallError> {
    Ok(kernel.task_asid(current_tid(kernel)?).is_some())
}

pub(super) fn record_user_fault(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
    addr: usize,
    access: FaultAccess,
) {
    kernel.record_fault(FaultInfo {
        addr: VirtAddr(addr as u64),
        access,
    });
    frame.set_err(SyscallError::PageFault.code());
}

/// D-NEXT-2 / Stage 114: made `pub(crate)` so the pre-`with_cpu` VmBrk-shrink
/// split path (`SharedKernel::try_split_vm_brk_shrink_into_frame`) can reuse
/// the identical bounds check the global-lock handler uses — no duplicated
/// validation logic, no behavior drift between the two paths.
pub(crate) fn validate_user_region(offset: u64, len: u64) -> Result<(), SyscallError> {
    let user_end_exclusive = crate::arch::vm_layout::KERNEL_SPACE_BASE;
    if offset >= user_end_exclusive {
        return Err(SyscallError::InvalidArgs);
    }
    let end_exclusive = offset.checked_add(len).ok_or(SyscallError::InvalidArgs)?;
    if end_exclusive > user_end_exclusive {
        return Err(SyscallError::InvalidArgs);
    }
    Ok(())
}

pub(super) fn validate_endpoint_right(
    kernel: &KernelState,
    cap: CapId,
    right: CapRights,
) -> Result<(), SyscallError> {
    let tid = kernel.current_tid().unwrap_or(0);
    let cnode = kernel.current_task_cnode();
    let slot_result = cnode.and_then(|cn| kernel.capability_for_cnode_local(cn, cap));
    let live_result = slot_result.and_then(|c| kernel.capability_object_live(c.object).map(|_| c));
    crate::yarm_log!(
        "CAP_LOOKUP tid={} cap={} cnode={} slot_found={} object_live={} type={:?} rights={:?}",
        tid,
        cap.0,
        cnode.map(|c| c.0).unwrap_or(u64::MAX),
        slot_result.is_some(),
        live_result.is_some(),
        live_result.map(|c| c.object),
        live_result.map(|c| c.rights()),
    );
    let endpoint_cap = live_result.ok_or(SyscallError::InvalidCapability)?;
    if !matches!(endpoint_cap.object, CapObject::Endpoint { .. }) {
        return Err(SyscallError::WrongObject);
    }
    if !endpoint_cap.has_right(right) {
        return Err(SyscallError::MissingRight);
    }
    Ok(())
}

/// D-NEXT-2 / Stage 114: made `pub(crate)` for the same reason as
/// `validate_user_region` above — reused verbatim by the split VmBrk-shrink path.
pub(crate) fn round_up_page(value: usize) -> Result<usize, SyscallError> {
    if value.is_multiple_of(PAGE_SIZE) {
        Ok(value)
    } else {
        let rounded = value
            .checked_add(PAGE_SIZE - 1)
            .ok_or(SyscallError::InvalidArgs)?;
        Ok(rounded & !(PAGE_SIZE - 1))
    }
}
