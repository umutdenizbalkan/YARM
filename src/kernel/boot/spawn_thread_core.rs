// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-SPAWN1 SP-2 — **THE** thread-incarnation lifecycle: allocate, register, undo.
//!
//! `SpawnThread` (NR 11) is the smallest member of the spawn family and the only one whose whole
//! body lives in two lock ranks. It creates no address space, loads no ELF, mints no capability,
//! creates no endpoint and never switches tasks — it allocates a TID, registers a TCB inside the
//! parent's existing process, initialises that TCB from the caller's arguments, and enqueues it.
//! Rank 2, then rank 1, and nothing else.
//!
//! This module owns the rank-2 half, as free functions over borrowed task-domain storage, so the
//! broad `KernelState` path and the pre-lock split route run the *same* policy under their own
//! lock disciplines — the SP-1 pattern.
//!
//! ## Why the TID cursor moved into the task domain
//!
//! `allocate_thread_id`'s correctness rule is "the candidate TID is not currently any task",
//! which it reads from `tcbs` — already under `task_state_lock`. The cursor it advances was a
//! bare `KernelState` field, serialized only by the broad lock, so an off-lock allocator would
//! have raced it. Exposing the cursor under the task lock is a lock-DOMAIN assignment, not a new
//! policy: the state and the invariant that governs it now live behind the same lock.
//!
//! ## The undo obligation
//!
//! Registration is the first irreversible mutation of NR 11, and the enqueue after it can still
//! fail — `SchedulerFull`, `WakeOnly`, a duplicate. Before this module the broad path leaked in
//! exactly that case: `spawn_user_thread` ends `let _ = self.enqueue_task(tid)?;`, so a failed
//! enqueue returned the error and left a registered, `Runnable`, never-queued task behind, whose
//! TID could not be reallocated. [`unregister_thread_incarnation_locked`] is the exact inverse of
//! [`register_thread_incarnation_locked`], and both paths now call it.
//!
//! What it does NOT undo is the process CNode: a thread joins its parent's existing process, so
//! the CNode is the parent's and outlives the child. Registration only ever *ensures* it, and for
//! a thread in a live process that is a no-op — which is why the split route can verify its
//! presence off-lock and decline before mutating anything if it is absent.

use super::thread_state::{
    KERNEL_STACK_GUARD_SIZE, KERNEL_STACK_REGION_BASE, KERNEL_STACK_REGION_SIZE,
};
use super::tid_allocation_policy::{TidAllocationCursor, TidAllocationPolicy};
use crate::kernel::boot::KernelError;
use crate::kernel::ipc::ThreadId;
use crate::kernel::task::{
    TaskClass, TaskStatus, ThreadControlBlock, ThreadGroupId, UserRegisterContext,
};
use crate::kernel::vm::{Asid, VirtAddr};

/// The telemetry the allocator owes, returned rather than written, so the rank-2 owner never
/// reaches into the rank-10 domain from inside the task lock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TidAllocationDelta {
    pub(crate) gap_floor_repairs: u64,
    pub(crate) dynamic_tid_allocations: u64,
    pub(crate) dynamic_tid_wraps: u64,
}

/// The validated, purely-syntactic arguments of one `SpawnThread`.
///
/// Constructing one is the entire pre-mutation gate: `tls_base`, `user_stack_top` and
/// `user_entry` must be non-zero and the stack top must be 16-byte aligned. It reads no kernel
/// state at all, so a refusal here has touched nothing and may still fall back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpawnThreadArgs {
    pub(crate) tls_base: usize,
    pub(crate) user_stack_top: usize,
    pub(crate) user_entry: usize,
}

impl SpawnThreadArgs {
    /// The exact validation `KernelState::spawn_user_thread` performs, in the same order and with
    /// the same typed refusal.
    pub(crate) fn validate(
        tls_base: usize,
        user_stack_top: usize,
        user_entry: usize,
    ) -> Result<Self, KernelError> {
        if tls_base == 0 || user_stack_top == 0 || user_entry == 0 || (user_stack_top & 0xF) != 0 {
            return Err(KernelError::WrongObject);
        }
        Ok(Self {
            tls_base,
            user_stack_top,
            user_entry,
        })
    }
}

/// What the child inherits from its parent — read under rank 2, in one acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParentThreadFacts {
    pub(crate) thread_group_id: ThreadGroupId,
    pub(crate) asid: Option<Asid>,
    pub(crate) class: TaskClass,
}

/// rank 2 — read the parent's inheritance facts, or `TaskMissing`.
///
/// The class comes from the companion `classes` table at the parent's own slot index, which is
/// how `KernelState::task_class` reads it; taking both from one acquisition is what stops the
/// parent from being replaced between the two reads.
pub(crate) fn parent_facts_locked(
    tcbs: &[Option<ThreadControlBlock>],
    classes: &[Option<TaskClass>],
    parent_tid: u64,
) -> Result<ParentThreadFacts, KernelError> {
    let idx = tcbs
        .iter()
        .position(|slot| slot.as_ref().is_some_and(|tcb| tcb.tid.0 == parent_tid))
        .ok_or(KernelError::TaskMissing)?;
    let parent = tcbs[idx].as_ref().ok_or(KernelError::TaskMissing)?;
    Ok(ParentThreadFacts {
        thread_group_id: parent.thread_group_id,
        asid: parent.asid,
        class: classes
            .get(idx)
            .copied()
            .flatten()
            .ok_or(KernelError::TaskMissing)?,
    })
}

/// rank 2 — **THE** dynamic TID allocator.
///
/// Reproduces `allocate_thread_id` exactly: the capacity ceiling, the gap-floor repair count, the
/// bounded probe over at most `max_tasks + 1` candidates, the wrap detection, and the cursor
/// advance. The telemetry it owes is returned rather than applied, because the caller — not this
/// function — decides when the rank-10 domain is safe to enter.
pub(crate) fn allocate_dynamic_tid_locked(
    tcbs: &[Option<ThreadControlBlock>],
    cursor: &mut TidAllocationCursor,
    policy: TidAllocationPolicy,
    max_tasks: usize,
) -> Result<(u64, TidAllocationDelta), KernelError> {
    if tcbs.iter().flatten().count() >= max_tasks {
        return Err(KernelError::TaskTableFull);
    }
    let mut delta = TidAllocationDelta::default();
    if cursor.raw_next_dynamic_tid() < policy.dynamic_tid_floor() {
        delta.gap_floor_repairs = 1;
    }
    let mut candidate = cursor.next_dynamic_tid(policy);
    for _ in 0..=max_tasks {
        debug_assert!(candidate > policy.static_tid_upper_bound());
        let taken = tcbs.iter().flatten().any(|tcb| tcb.tid.0 == candidate);
        if !taken {
            let wraps = policy.advance_dynamic_cursor(candidate) == policy.dynamic_tid_floor();
            cursor.advance_after_allocation(policy, candidate);
            delta.dynamic_tid_allocations = 1;
            if wraps {
                delta.dynamic_tid_wraps = 1;
                crate::yarm_log!(
                    "YARM_TID_ALLOC_WRAP allocated={} reset_cursor_to={}",
                    candidate,
                    policy.dynamic_tid_floor()
                );
            }
            return Ok((candidate, delta));
        }
        candidate = policy.advance_dynamic_cursor(candidate);
    }
    Err(KernelError::TaskTableFull)
}

/// rank 2 — **THE** thread-incarnation registration: TCB slot, class, kernel-stack range.
///
/// This is registration's task-domain half. The rank-4 half — ensuring the process CNode exists
/// and is associated with the PID — stays with the caller, because a thread joining a live
/// process needs it to be a verified no-op while a fresh process genuinely needs it done.
///
/// The kernel "context" is pure TCB state: the stack range is a fixed per-slot region derived
/// from the slot index, not an allocation, which is why registering and undoing a thread
/// incarnation never touches the frame allocator.
///
/// Returns the claimed slot index. Refuses with `TaskTableFull` at the capacity ceiling or when
/// no slot is free, having mutated nothing in either case.
pub(crate) fn register_thread_incarnation_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    classes: &mut [Option<TaskClass>],
    tid: u64,
    class: TaskClass,
    max_tasks: usize,
) -> Result<usize, KernelError> {
    if tcbs.iter().flatten().count() >= max_tasks {
        return Err(KernelError::TaskTableFull);
    }
    let idx = tcbs
        .iter()
        .position(Option::is_none)
        .ok_or(KernelError::TaskTableFull)?;
    // The stack range must be derivable BEFORE the slot is claimed, so an arithmetic refusal
    // leaves the table untouched rather than half-registered.
    let region_base = KERNEL_STACK_REGION_BASE
        .checked_add(idx.saturating_mul(KERNEL_STACK_REGION_SIZE))
        .ok_or(KernelError::VmFull)?;
    let stack_base = region_base
        .checked_add(KERNEL_STACK_GUARD_SIZE)
        .ok_or(KernelError::VmFull)?;
    let stack_top = region_base
        .checked_add(KERNEL_STACK_REGION_SIZE)
        .ok_or(KernelError::VmFull)?;
    if stack_base == 0 || stack_top == 0 || stack_base >= stack_top {
        return Err(KernelError::WrongObject);
    }

    let mut tcb = ThreadControlBlock::new(ThreadId(tid), None);
    tcb.kernel_context.stack_base = Some(VirtAddr(stack_base as u64));
    tcb.kernel_context.stack_top = Some(VirtAddr(stack_top as u64));
    // The switch-frame trampoline, exactly as `provision_default_kernel_context` sets it: a
    // 16-byte-aligned stack pointer at the top of the region and the trampoline entry point.
    // `owns_stack` records that this TCB, not a borrowed context, owns the region — which is
    // what the teardown path reads to decide whether to release it.
    tcb.kernel_context.frame.set_stack_ptr(stack_top & !0xF);
    tcb.kernel_context
        .frame
        .set_instruction_ptr(super::thread_state::kernel_switch_frame_trampoline_ip());
    tcb.kernel_context.initialized = false;
    tcb.kernel_context.owns_stack = true;
    tcbs[idx] = Some(tcb);
    classes[idx] = Some(class);
    crate::yarm_log!(
        "KERNEL_STACK_RANGE tid={} base=0x{:x} top=0x{:x}",
        tid,
        stack_base,
        stack_top
    );
    Ok(idx)
}

/// rank 2 — the EXACT inverse of [`register_thread_incarnation_locked`].
///
/// Clears the kernel context, the class entry and the TCB slot, in that order, and only for a
/// slot that still names this exact TID. Idempotent and inert on a stale or already-cleaned
/// identity: an undo that runs twice, or names a TID some later task now occupies, mutates
/// nothing and reports `false`.
///
/// It deliberately does not touch the process CNode. A thread joins its parent's process; the
/// CNode is the parent's and outlives the child.
pub(crate) fn unregister_thread_incarnation_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    classes: &mut [Option<TaskClass>],
    tid: u64,
    expected_group: ThreadGroupId,
) -> bool {
    let Some(idx) = tcbs.iter().position(|slot| {
        slot.as_ref()
            .is_some_and(|tcb| tcb.tid.0 == tid && tcb.thread_group_id.0 == expected_group.0)
    }) else {
        return false;
    };
    if let Some(tcb) = tcbs[idx].as_mut() {
        tcb.kernel_context.stack_base = None;
        tcb.kernel_context.stack_top = None;
        tcb.kernel_context.frame = Default::default();
        tcb.kernel_context.initialized = false;
        tcb.kernel_context.owns_stack = false;
    }
    classes[idx] = None;
    // Clearing a TCB slot has ONE owner. `spawn_reservation::clear_reservation_slot` is it, and
    // the WA2A closure argument — "no production path removes a TCB from the array" — rests on
    // that being true, so an undo that wrote the slot itself would be a second slot-clearing
    // owner outside the census.
    crate::kernel::spawn_reservation::clear_reservation_slot(tcbs, idx);
    crate::yarm_log!(
        "SPAWN_THREAD_INCARNATION_UNDONE tid={} group={} result=ok",
        tid,
        expected_group.0
    );
    true
}

/// rank 2 — initialise a freshly registered thread from its parent's facts and the caller's
/// arguments, and make it `Runnable`.
///
/// This is the point of no return in the task domain: after it the TCB is a live, dispatchable
/// task in every respect except that it is not yet on a run queue. The `Runnable` status is what
/// the enqueue seam requires, so it must be set before rank 1 — which is exactly why a failed
/// enqueue has to undo the whole incarnation rather than leaving it.
pub(crate) fn initialize_thread_incarnation_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    idx: usize,
    parent: &ParentThreadFacts,
    args: &SpawnThreadArgs,
) -> Result<(), KernelError> {
    let tcb = tcbs
        .get_mut(idx)
        .and_then(Option::as_mut)
        .ok_or(KernelError::TaskMissing)?;
    tcb.thread_group_id = parent.thread_group_id;
    tcb.asid = parent.asid;
    tcb.tls_ptr = Some(VirtAddr(args.tls_base as u64));
    tcb.user_entry = Some(VirtAddr(args.user_entry as u64));
    tcb.user_stack_top = Some(VirtAddr(args.user_stack_top as u64));
    tcb.user_context = UserRegisterContext {
        instruction_ptr: VirtAddr(args.user_entry as u64),
        stack_ptr: VirtAddr(args.user_stack_top as u64),
        user_gprs: [0; 32],
        arg0: 0,
        arg1: 0,
        arg2: 0,
        arg3: 0,
        arg4: 0,
        arg5: 0,
    };
    tcb.status = TaskStatus::Runnable;
    Ok(())
}

/// rank 2 — claim the child's TLS-restore slot, as `spawn_user_thread` does.
///
/// Reuses a slot already naming this TID, else the first free one. A full table is not an error:
/// the broad path has always treated it as best-effort, and the restore is a convenience rather
/// than a correctness precondition.
pub(crate) fn claim_tls_restore_slot_locked(slots: &mut [Option<ThreadId>], tid: u64) {
    if let Some(slot) = slots
        .iter_mut()
        .find(|slot| slot.is_some_and(|pending| pending.0 == tid) || slot.is_none())
    {
        *slot = Some(ThreadId(tid));
    }
}
