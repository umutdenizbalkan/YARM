// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Capability-domain syscall handlers.
//!
//! D4 step 4: mechanically split from the parent `syscall.rs` module with zero
//! behavior change. `syscall.rs` keeps minimal delegation shims so dispatch
//! routing remains explicit while capability/CNode semantics stay owned by the
//! existing `KernelState` capability methods.

use super::{
    SYSCALL_ARG_CAP, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR, SyscallError, current_task_has_user_asid,
    current_tid, round_up_page,
};
use crate::kernel::boot::{ControlPlaneCnodePlan, KernelError, KernelState};
use crate::kernel::capabilities::CapId;
use crate::kernel::ipc::ThreadId;
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::{PAGE_SIZE, VirtAddr};

pub(super) fn handle_transfer_release(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    if !current_task_has_user_asid(kernel)? {
        return Err(SyscallError::InvalidArgs);
    }
    let transfer_cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let owner = ThreadId(current_tid(kernel)?);
    let (base, map_len) = {
        let base_arg = frame.arg(SYSCALL_ARG_PTR);
        let len_arg = frame.arg(SYSCALL_ARG_LEN);
        if base_arg == 0 && len_arg == 0 {
            kernel
                .active_transfer_mapping_for(owner, transfer_cap)
                .map(|(base, len)| (base.0 as usize, len))
                .ok_or(SyscallError::InvalidArgs)?
        } else {
            if len_arg == 0 || !base_arg.is_multiple_of(PAGE_SIZE) {
                return Err(SyscallError::InvalidArgs);
            }
            (base_arg, round_up_page(len_arg)?)
        }
    };
    let end = base.checked_add(map_len).ok_or(SyscallError::InvalidArgs)?;
    // Stage 7: plan-first ASID resolution before the two-phase unmap loop
    // (rank-2 task read before vm/memory mutation in loop body).
    // current_task_has_user_asid (checked above) guarantees task_asid returns Some.
    let asid = kernel
        .task_asid(owner.0)
        .ok_or(SyscallError::from(KernelError::UserMemoryFault))?;
    let mut va = base;
    while va < end {
        // Stage 7: two-phase unmap — reclaim only after shootdown wait/fast path.
        // Ok(None) means the page was never mapped; preserve old InvalidArgs behavior.
        let plan = kernel
            .unmap_page_phase1(asid, VirtAddr(va as u64))
            .map_err(SyscallError::from)?;
        let Some(plan) = plan else {
            return Err(SyscallError::InvalidArgs);
        };
        kernel
            .execute_tlb_shootdown_wait_plan(plan)
            .map_err(SyscallError::from)?;
        va += PAGE_SIZE;
    }

    let cnode = kernel.current_task_cnode().ok_or(SyscallError::Internal)?;
    kernel
        .revoke_capability_in_cnode(cnode, transfer_cap)
        .map_err(SyscallError::from)?;
    if kernel.remove_active_transfer_mapping(owner, transfer_cap) {
        kernel.note_shared_mem_released(map_len);
    }
    frame.set_ok(map_len, 0, 0);
    Ok(())
}

pub(super) fn handle_control_plane_set_cnode_slots(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let requester_tid = current_tid(kernel)?;
    let target_pid = frame.arg(SYSCALL_ARG_CAP) as u64;
    let slot_capacity = frame.arg(SYSCALL_ARG_PTR);
    if target_pid == 0 || slot_capacity == 0 {
        return Err(SyscallError::InvalidArgs);
    }
    // Stage 5B plan-first: snapshot task domain (rank 2) before capability
    // mutation (rank 4). When the global lock is removed, this read moves to
    // before the with_cpu() call via split-read on SharedKernel.
    let plan = ControlPlaneCnodePlan {
        requester_class: kernel
            .task_class(requester_tid)
            .ok_or(SyscallError::from(KernelError::TaskMissing))?,
        requester_pid: kernel.process_id(requester_tid).unwrap_or(requester_tid),
    };
    kernel
        .control_plane_set_process_cnode_slots_planned(&plan, target_pid, slot_capacity)
        .map_err(SyscallError::from)?;
    frame.set_ok(slot_capacity, target_pid as usize, 0);
    Ok(())
}
