// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! VM/MM syscall handlers (VmMap, VmAnonMap, VmBrk).
//!
//! Stage 145: mechanically split from the parent `syscall.rs` module with zero
//! behavior change. `syscall.rs` keeps minimal delegation shims so dispatch
//! routing remains explicit while VM mapping semantics stay owned by the
//! existing `KernelState` VM methods.

use super::{
    SYSCALL_ARG_CAP, SYSCALL_ARG_INLINE_PAYLOAD0, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR,
    SYSCALL_VM_MAP_PROT_EXEC, SYSCALL_VM_MAP_PROT_READ, SYSCALL_VM_MAP_PROT_WRITE, SyscallError,
    current_tid, round_up_page, validate_user_region,
};
use crate::kernel::boot::{
    KernelError, KernelState, VmAnonMapProgressPlan, VmAnonMapValidatedArgs, VmBrkPlan,
    VmPageMapProgress,
};
use crate::kernel::capabilities::CapId;
use crate::kernel::capabilities::CapObject;
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::{Asid, PAGE_SIZE, PageFlags, VirtAddr};

fn vm_map_page_flags(prot: usize) -> Result<PageFlags, SyscallError> {
    let unknown =
        prot & !(SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE | SYSCALL_VM_MAP_PROT_EXEC);
    if unknown != 0 {
        return Err(SyscallError::InvalidArgs);
    }
    Ok(PageFlags {
        read: (prot & SYSCALL_VM_MAP_PROT_READ) != 0,
        write: (prot & SYSCALL_VM_MAP_PROT_WRITE) != 0,
        execute: (prot & SYSCALL_VM_MAP_PROT_EXEC) != 0,
        user: true,
        cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
    })
}

/// Validates the (addr, len, prot) triple shared by VmMap and VmAnonMap.
/// Returns `(map_len, end, flags)` where `map_len` is rounded up to `PAGE_SIZE`
/// and `end = addr + map_len` is guaranteed not to overflow.
fn validate_anon_map_args(
    addr: usize,
    len: usize,
    prot: usize,
) -> Result<(usize, usize, PageFlags), SyscallError> {
    if len == 0 || !addr.is_multiple_of(PAGE_SIZE) {
        return Err(SyscallError::InvalidArgs);
    }
    let map_len = round_up_page(len)?;
    let end = addr.checked_add(map_len).ok_or(SyscallError::InvalidArgs)?;
    let flags = vm_map_page_flags(prot)?;
    Ok((map_len, end, flags))
}

/// Undo physical mappings for [addr, mapped_end) on partial VmAnonMap failure.
/// Stage 9: also revokes capability slots for rolled-back pages so physical
/// frames are fully reclaimed. `unmapped_cap` carries the cap that was allocated
/// for the failing page but never mapped (only set on map failure, not alloc failure).
fn rollback_anon_map(
    kernel: &mut KernelState,
    asid: Asid,
    addr: usize,
    mapped_end: usize,
    unmapped_cap: Option<CapId>,
) {
    // Revoke the un-mapped cap first (case: map_user_page_in_asid_with_caps failed).
    // The cap was allocated but the page was never inserted into the address space,
    // so there is no phase-1 unmap — we revoke it directly.
    if let Some(cap) = unmapped_cap {
        if let Some(cnode) = kernel.current_task_cnode() {
            let _ = kernel.revoke_capability_in_cnode(cnode, cap);
        }
    }
    // Stage 6: two-phase unmap for mapped pages; Stage 9: also revoke their caps.
    // After unmap_page_phase1, map_refcount=0 and we have the physical address.
    // Revoking the cap decrements cap_refcount to 0; execute_tlb_shootdown_wait_plan
    // then frees the physical frame (reclaim_memory_object_if_unreferenced sees both=0).
    // Absent pages (Ok(None)) are silently skipped — unmap_page_phase1 tolerates them.
    let mut va = addr;
    while va < mapped_end {
        if let Ok(Some(wait_plan)) = kernel.unmap_page_phase1(asid, VirtAddr(va as u64)) {
            if let Some((cnode, cap_id)) =
                kernel.find_current_task_cap_for_memory_object_phys(wait_plan.phys)
            {
                let _ = kernel.revoke_capability_in_cnode(cnode, cap_id);
            }
            let _ = kernel.execute_tlb_shootdown_wait_plan(wait_plan);
        }
        va += PAGE_SIZE;
    }
}

pub(super) fn handle_vm_map(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let aspace_map_cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let addr = frame.arg(SYSCALL_ARG_PTR);
    let len = frame.arg(SYSCALL_ARG_LEN);
    let prot = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0);
    let (map_len, end, flags) = validate_anon_map_args(addr, len, prot)?;
    // Stage 7: extract ASID from aspace_map_cap (the capability target) so that the
    // stack guard check looks at the same address space as the map loop. The old
    // check_stack_guard used is_user_page_mapped_in_current_asid, which would differ
    // from the map target if aspace_map_cap refers to a different address space.
    let map_asid = {
        let cap = kernel
            .capability_service()
            .resolve_current_task_capability(aspace_map_cap)
            .ok_or(SyscallError::from(KernelError::InvalidCapability))?;
        match cap.object {
            CapObject::AddressSpace { asid } => Asid(asid),
            _ => return Err(SyscallError::from(KernelError::WrongObject)),
        }
    };
    // Explicit-ASID guard check (same condition as check_stack_guard / handle_vm_anon_map):
    // reject write-only mappings when the page immediately below addr is already mapped.
    if flags.write
        && !flags.execute
        && let Some(guard_page) = addr.checked_sub(PAGE_SIZE)
        && kernel
            .is_user_page_mapped_in_asid(map_asid, VirtAddr(guard_page as u64))
            .map_err(SyscallError::from)?
    {
        return Err(SyscallError::InvalidArgs);
    }
    // Stage 10: use map_asid (resolved plan-first above) directly instead of
    // re-resolving from aspace_map_cap on every page. Track mapped_end for
    // rollback symmetry: on alloc or map failure, rollback_anon_map revokes
    // caps and reclaims frames for [addr, mapped_end) — same two-phase pattern
    // as handle_vm_anon_map. Anonymous memory is always allocated in the
    // current task's cnode regardless of which address space it is mapped into.
    // Stage 172 (VM-COW): default-off map phase markers. Diagnostic only — the
    // two-phase `rollback_anon_map` transaction below is UNCHANGED.
    let vm_cow = crate::kernel::boot::vm_cow_enabled();
    if vm_cow {
        crate::yarm_log!(
            "VM_MAP_PHASE_METADATA asid={} addr=0x{:x} len={}",
            map_asid.0,
            addr,
            map_len
        );
    }
    let mut mapped_end = addr;
    while mapped_end < end {
        let (_, mem_cap) = match kernel.alloc_anonymous_memory_object() {
            Ok(pair) => pair,
            Err(e) => {
                rollback_anon_map(kernel, map_asid, addr, mapped_end, None);
                if vm_cow {
                    crate::yarm_log!(
                        "VM_MAP_ROLLBACK_OK asid={} addr=0x{:x} reason=frame_alloc",
                        map_asid.0,
                        addr
                    );
                }
                return Err(SyscallError::from(e));
            }
        };
        if let Err(e) = kernel.map_user_page_in_asid_with_caps(
            map_asid,
            mem_cap,
            VirtAddr(mapped_end as u64),
            flags,
        ) {
            rollback_anon_map(kernel, map_asid, addr, mapped_end, Some(mem_cap));
            if vm_cow {
                crate::yarm_log!(
                    "VM_MAP_ROLLBACK_OK asid={} addr=0x{:x} reason=pt_update",
                    map_asid.0,
                    addr
                );
            }
            return Err(SyscallError::from(e));
        }
        mapped_end += PAGE_SIZE;
    }
    if vm_cow {
        crate::yarm_log!(
            "VM_MAP_PHASE_FRAME_ALLOC asid={} addr=0x{:x} len={}",
            map_asid.0,
            addr,
            map_len
        );
        crate::yarm_log!(
            "VM_MAP_PHASE_PT_UPDATE asid={} addr=0x{:x} len={}",
            map_asid.0,
            addr,
            map_len
        );
    }
    frame.set_ok(addr, map_len, 0);
    Ok(())
}

pub(super) fn handle_vm_anon_map(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let addr = frame.arg(SYSCALL_ARG_PTR);
    let len = frame.arg(SYSCALL_ARG_LEN);
    let prot = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0);
    let (map_len, end, flags) = validate_anon_map_args(addr, len, prot)?;

    // Stage 6 plan-first; Stage 9: captured in VmAnonMapProgressPlan so all fields
    // (tid, asid, validated args, mapped_end progress) are explicit in one struct.
    let tid = current_tid(kernel)?;
    let asid = kernel
        .task_asid(tid)
        .ok_or(SyscallError::from(KernelError::UserMemoryFault))?;
    let mut plan = VmAnonMapProgressPlan {
        validated: VmAnonMapValidatedArgs {
            addr,
            map_len,
            end,
            flags,
        },
        tid,
        asid,
        progress: VmPageMapProgress {
            base_addr: addr,
            mapped_end: addr,
            end_addr: end,
        },
    };

    // Stage 6: explicit-ASID stack guard check using the plan-first ASID.
    // Guard fires iff flags.write && !flags.execute && the page below addr is mapped.
    if plan.validated.flags.write
        && !plan.validated.flags.execute
        && let Some(guard_page) = plan.validated.addr.checked_sub(PAGE_SIZE)
        && kernel
            .is_user_page_mapped_in_asid(plan.asid, VirtAddr(guard_page as u64))
            .map_err(SyscallError::from)?
    {
        return Err(SyscallError::InvalidArgs);
    }

    while plan.progress.mapped_end < plan.progress.end_addr {
        let va = plan.progress.mapped_end;
        let (_, mem_cap) = match kernel.alloc_anonymous_memory_object() {
            Ok(pair) => pair,
            Err(e) => {
                // Stage 9: alloc failure — no unmapped cap (alloc itself failed).
                rollback_anon_map(
                    kernel,
                    plan.asid,
                    plan.progress.base_addr,
                    plan.progress.mapped_end,
                    None,
                );
                return Err(SyscallError::from(e));
            }
        };
        if let Err(e) = kernel.map_user_page_in_asid_with_caps(
            plan.asid,
            mem_cap,
            VirtAddr(va as u64),
            plan.validated.flags,
        ) {
            // Stage 9: map failure — mem_cap was allocated but not mapped; pass it for revoke.
            rollback_anon_map(
                kernel,
                plan.asid,
                plan.progress.base_addr,
                plan.progress.mapped_end,
                Some(mem_cap),
            );
            return Err(SyscallError::from(e));
        }
        plan.progress.mapped_end += PAGE_SIZE;
    }
    frame.set_ok(plan.validated.addr, plan.validated.map_len, 0);
    Ok(())
}

pub(super) fn handle_vm_brk(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let tid = current_tid(kernel)?;
    // Stage 5B plan-first: snapshot task domain (rank 2) before memory
    // mutation (rank 6). When the global lock is removed, this read moves to
    // before the with_cpu() call via split-read on SharedKernel.
    let plan = VmBrkPlan {
        tid,
        is_group_leader: kernel.is_thread_group_leader(tid),
    };
    if !plan.is_group_leader {
        return Err(SyscallError::InvalidArgs);
    }

    let requested = frame.arg(SYSCALL_ARG_CAP);
    if requested == 0 {
        let current_end = kernel
            .task_brk_bounds(plan.tid)
            .map(|(_, end)| end)
            .unwrap_or(0);
        frame.set_ok(current_end, 0, 0);
        return Ok(());
    }

    validate_user_region(requested as u64, 1)?;
    let (base, current_end) = kernel
        .task_brk_bounds(plan.tid)
        .ok_or(SyscallError::InvalidArgs)?;
    if requested < base {
        return Err(SyscallError::InvalidArgs);
    }

    if requested < current_end {
        let unmap_start = round_up_page(requested)?;
        let unmap_end = round_up_page(current_end)?;
        if unmap_start < unmap_end {
            // VALIDATION: D3_LIVE_SPLIT (Stage 107)
            // Stage 5F two-phase shrink: resolve ASID once before the helper
            // call (plan-first: snapshot task rank 2 before vm+memory
            // mutation). Stage 107 routes the per-page two-phase loop into
            // the typed `vm_brk_shrink_two_phase` helper in memory_state.rs
            // — observability + future SharedKernel seam anchor. The per-page
            // ordering (Phase 1 PTE remove → Phase 2 TLB shootdown wait →
            // Phase 3 frame reclaim, via execute_tlb_shootdown_wait_plan)
            // is byte-identical to the pre-Stage-107 inline loop. See
            // doc/KERNEL_UNLOCKING.md
            let asid = kernel
                .task_asid(plan.tid)
                .ok_or(SyscallError::from(KernelError::UserMemoryFault))?;
            kernel
                .vm_brk_shrink_two_phase(asid, unmap_start, unmap_end)
                .map_err(SyscallError::from)?;
        }
    }

    // Staged VM_BRK behavior: tracked per-task. Growth requires
    // pre-initialized brk bounds to avoid creating an empty [base,end) window
    // from unset state. Heap pages are still allocated lazily by demand-fault
    // mapping in [base, end). Shrink updates the byte-granular brk after all
    // page-granular unmap bookkeeping succeeds.
    kernel
        .set_task_brk_bounds(tid, base, requested)
        .map_err(SyscallError::from)?;
    frame.set_ok(requested, 0, 0);
    Ok(())
}
