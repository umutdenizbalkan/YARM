// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

mod bootstrap_state;
mod capability_lifecycle_state;
mod capability_service_state;
mod capability_state;
mod capacity_state;
mod cnode_state;
mod defs;
mod delegation_state;
mod driver_state;
mod exec_state;
mod fault_endpoint_state;
mod fault_state;
mod ipc_state;
mod memory_lifecycle_state;
mod memory_state;
mod orchestrator_state;
mod restart_state;
mod scheduler_state;
mod task_core_state;
mod task_policy_state;
mod thread_state;
mod tid_allocation_policy;
mod transfer_state;
mod types;
mod user_memory_state;

use super::capabilities::{
    CNodeId, CapId, CapObject, CapRights, Capability, CapabilityDeriveError, CapabilitySpace,
};
#[cfg(test)]
use super::ipc::EndpointMode;
use super::ipc::{Endpoint, IpcError, Message};
use super::scheduler::{CpuId, SchedulerError, SmpScheduler};
use super::scheduler_timer::Timer;
use super::smp::SmpMailbox;
#[cfg(test)]
use super::smp::WorkItem;
use super::syscall::SyscallError;
use super::task::{FaultPolicy, RobustFutexState, TaskClass, TaskStatus, ThreadControlBlock};
#[cfg(test)]
use super::task::{ThreadGroupId, UserRegisterContext, WaitReason};
use super::trap::FaultInfo;
#[cfg(test)]
use super::trap::{FaultAccess, Trap, TrapEvent};
use super::trapframe::TrapFrame;
use super::vm::{
    AddressSpace, AddressSpaceManager, Asid, Mapping, PageFlags, PhysAddr, VirtAddr, VmError,
};
use crate::arch::{platform_constants, topology};
use crate::kernel::frame_allocator::{
    MemoryRegion, PhysicalFrameAllocator, init_pt_frame_allocator,
};
use crate::kernel::ipc::ThreadId;
use crate::kernel::lock::SpinLockIrq;
#[cfg(feature = "hosted-dev")]
use alloc::collections::BTreeMap;
use tid_allocation_policy::{TidAllocationCursor, TidAllocationPolicy};

const MAX_ENDPOINTS: usize = 256;

#[cfg(feature = "hosted-dev")]
const MAX_ENDPOINT_SENDER_WAITERS: usize = 8;
#[cfg(not(feature = "hosted-dev"))]
const MAX_ENDPOINT_SENDER_WAITERS: usize = 4;

// Keep task capacity consistent across hosted-dev and freestanding builds so
// capacity-sensitive tests match deployed behavior.
const MAX_TASKS: usize = 512;

const MAX_MEMORY_OBJECTS: usize = 512;
const MAX_BOOT_MEMORY_REGIONS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaultBookkeepingMode {
    RecordInHandleTrapEvent,
    AlreadyRecordedBySharedSeam,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IpcEndpointSplitRejectReason {
    EndpointIndexOutOfRange,
    EndpointMissing,
    NonBufferedEndpoint,
    EmptyQueue,
    ReceiverWaiterPresent,
    SenderWaiterPresent,
    TransferOrReplyCapMessage,
    EndpointQueueFull,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IpcEndpointRecvResult {
    Received(Message),
    /// Stage 4D: plain recv with sender-waiter refill.
    /// Endpoint mutation (dequeue + refill) already done under ipc_state_lock.
    /// Caller must apply the wake plan outside the lock via apply_split_sender_wake_plan.
    ReceivedWithSenderWake(Message, ThreadId),
    Ineligible(IpcEndpointSplitRejectReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IpcEndpointSendResult {
    Enqueued,
    /// Stage 4F: plain send to a waiting legacy (non-recv-v2) receiver.
    /// Message enqueued and receiver slot cleared under ipc_state_lock.
    /// Caller must apply WakeReceiver outside the lock via apply_split_receiver_wake_plan.
    EnqueuedWakeReceiver(ThreadId),
    /// Stage 4F pre-screen: found a plain receiver waiter with this TID and no sender waiters.
    /// TID came from a locked ipc_state_lock read in ipc_try_send_queued_plain_endpoint_only.
    /// Caller should check is_task_recv_v2_blocked then call ipc_try_send_to_plain_receiver_endpoint_only.
    ReceiverWaiterFound(ThreadId),
    Ineligible(IpcEndpointSplitRejectReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IpcSchedulerPlan {
    None,
    /// Wake a sender whose message was refilled into the endpoint queue under ipc_state_lock.
    /// Apply with apply_split_sender_wake_plan outside any ipc/endpoint lock.
    WakeSender(ThreadId),
    /// Stage 4F: wake a receiver whose waiter slot was cleared under ipc_state_lock.
    /// Apply with apply_split_receiver_wake_plan outside any ipc/endpoint lock.
    WakeReceiver(ThreadId),
}

#[allow(dead_code)]
/// General-purpose deferred scheduler wake plan.
///
/// Separates the *decision* (computed while holding a domain lock) from the
/// *execution* (applied after all domain locks are released).  Analogous to
/// `IpcSchedulerPlan` but intended for non-IPC kernel domains (fault, restart,
/// capability lifecycle, thread join) that need to wake a task as a side effect
/// of a mutation that is itself guarded by a domain lock.
///
/// Usage pattern:
/// ```text
/// // inside a domain-lock closure — compute only, no scheduler mutation:
/// let plan = if some_condition { SchedulerWakePlan::Wake(tid) }
///            else              { SchedulerWakePlan::None };
/// // after releasing the domain lock — execute:
/// kernel.apply_scheduler_wake_plan(plan)?;
/// ```
///
/// See `doc/KERNEL_LOCKING.md §SchedulerWakePlan` for the authoritative
/// lock-ordering rationale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedulerWakePlan {
    /// No scheduling action required.
    None,
    /// Wake the identified task: mark it Runnable and enqueue it on the
    /// appropriate CPU.  Applied via `apply_scheduler_wake_plan`.
    Wake(ThreadId),
}

#[allow(dead_code)]
/// Deferred cooperative-handoff plan for IPC send paths.
///
/// Encodes the intent to yield CPU time to a specific task after an IPC send
/// completes.  Separates the *decision* (which task should receive the CPU next,
/// computed at message-delivery time) from the *execution* (the one-shot direct
/// dispatch, applied after all IPC/cap/VM domain mutations are done).
///
/// **Hosted-dev and freestanding semantics:**
/// `YieldTo(tid)` drives `yield_current_to(tid)`, which calls `on_preempt_prefer`
/// once: the outgoing task is re-enqueued at the tail of its queue, then `tid`
/// is removed from whichever priority queue it is in and made current directly,
/// bypassing FIFO order.  Completes in one scheduler operation (O(P×Q) where
/// P = 3 priority levels, Q ≤ MAX_RUN_QUEUE = 64) — no busy-loop.
///
/// Callers that guarantee `tid` was just enqueued (e.g. via `wake_waiter_for_endpoint`
/// immediately before) will always get `true` back.
///
/// Usage:
/// ```text
/// // At message-delivery time, before any context switch:
/// let plan = if has_receiver { SchedulerHandoffPlan::YieldTo(receiver_tid) }
///            else             { SchedulerHandoffPlan::None };
/// // After all domain mutations:
/// let switched = kernel.apply_scheduler_handoff_plan(plan)?;
/// ```
///
/// See `doc/KERNEL_LOCKING.md §SchedulerHandoffPlan` for the authoritative
/// lock-ordering and hosted-dev constraint documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedulerHandoffPlan {
    /// No cooperative handoff required.
    None,
    /// Yield CPU to the identified task.  Applied via `apply_scheduler_handoff_plan`
    /// → `yield_current_to` → `on_preempt_prefer` (one scheduler operation).
    /// Returns `true` if the target became the current task, `false` otherwise.
    YieldTo(ThreadId),
}

// ── Stage 5B plan-first structs ──────────────────────────────────────────────
//
// Each struct captures the task-domain snapshot (rank 2) produced by the
// plan-read phase. The mutation phase uses only these snapshots, never
// re-acquiring the task lock inside a capability or memory lock.
//
// Lock-domain flow:
//   ControlPlaneCnodePlan: task (rank 2) read → capability (rank 4) mutation
//   VmBrkPlan:             task (rank 2) read → memory    (rank 6) mutation
//   VmAnonMapPlan:         scaffolding only — no live conversion in Stage 5B
//                          (requires x86_64 TLB smoke; see KERNEL_LOCKING.md §17)

/// Stage 5B plan-first snapshot for `ControlPlaneSetCnodeSlots`.
///
/// Captures the requester's task class and process id under the task lock
/// (rank 2) before any capability mutation (rank 4). The mutation phase uses
/// these fields directly, avoiding a second task-domain read inside the
/// capability closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ControlPlaneCnodePlan {
    pub(crate) requester_class: TaskClass,
    pub(crate) requester_pid: u64,
}

/// Stage 5B plan-first snapshot for `VmBrk`.
///
/// Captures whether the calling thread is the thread-group leader under the
/// task lock (rank 2) before any memory mutation (rank 6). The mutation phase
/// uses this flag directly, avoiding a second task-domain read inside the
/// memory closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VmBrkPlan {
    pub(crate) tid: u64,
    pub(crate) is_group_leader: bool,
}

/// Stage 5B scaffolding for `VmAnonMap`, strengthened in Stage 5C — helper-only,
/// no live conversion.
///
/// ## Stage 5C audit summary
///
/// VmAnonMap touches 6 lock domains in the following sequence (no inversions):
/// ```text
/// A  validate_anon_map_args        lock-free (pure computation)
/// B  check_stack_guard              scheduler(1)→task(2)→vm(5) [reads only]
/// C  alloc_anonymous_memory_object  memory(6)→boot_config(11)→memory(6)→capability(4)
///                                   [all released independently, no simultaneous holds]
/// D  map_user_page_in_current_asid  scheduler(1)→task(2)→capability(4)→vm(5)→memory(6)
/// G  rollback: unmap_user_page      scheduler(1)→task(2)→vm(5)→memory(6)→ipc(3)
///    ↳ request_live_asid_shootdown  scheduler(1)→task(2)→ipc(3) [TLB busy-wait]
/// H  frame.set_ok                   TrapFrame write (last)
/// ```
///
/// ## Why live conversion is deferred
///
/// Three blockers, all requiring x86_64 SMP smoke before resolution:
/// 1. **TLB busy-wait in rollback**: `request_live_asid_shootdown` spins on
///    `begin_live_tlb_shootdown_wait` (ipc rank 3) and cross-CPU ACKs. Any change
///    to its invocation context outside the global lock risks TLB coherency races.
/// 2. **Per-page alloc-map-rollback interleaving**: The loop allocates, maps, and
///    conditionally rolls back each page. Splitting this across per-domain lock
///    acquisitions without the global lock requires careful state management not
///    yet designed.
/// 3. **Implicit current-ASID per iteration**: `map_user_page_in_current_asid_with_caps`
///    re-reads `current_tid()`/`task_asid(tid)` on every page. The explicit-ASID
///    helpers (Stage 5C) eliminate this, but live use requires smoke.
///
/// ## Migration path
///
/// When x86_64 smoke is approved:
/// 1. `handle_vm_anon_map` reads `tid` + `asid` once via `VmAnonMapPlan` before
///    the loop (or before `with_cpu()` via `current_tid_split_read` + `task_asid_for_tid_split_read`).
/// 2. The loop uses `map_user_page_in_asid_with_caps` / `unmap_user_page_in_asid`
///    (Stage 5C explicit-ASID helpers) for all per-page work.
/// 3. `check_stack_guard` uses `is_user_page_mapped_in_asid` with the plan ASID.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VmAnonMapPlan {
    /// Validated, rounded syscall arguments (lock-free phase).
    pub(crate) validated: VmAnonMapValidatedArgs,
    /// TID of the calling thread (scheduler snapshot, rank 1).
    pub(crate) tid: u64,
    /// ASID of the calling task's address space (task snapshot, rank 2).
    pub(crate) asid: Asid,
}

/// Stage 5C: Result of `validate_anon_map_args` — pure computation, no locks.
///
/// Captured before any lock acquisition so it can be reused across plan phases
/// without repeating the overflow/alignment arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VmAnonMapValidatedArgs {
    /// Page-aligned start address (same as syscall arg `addr`).
    pub(crate) addr: usize,
    /// Page-rounded mapping length (`≥ len`, multiple of PAGE_SIZE).
    pub(crate) map_len: usize,
    /// `addr + map_len` (guaranteed no overflow).
    pub(crate) end: usize,
    /// Resolved `PageFlags` from the `prot` syscall argument.
    pub(crate) flags: PageFlags,
}

// ── Stage 5D: TLB shootdown / rollback-domain plan types ─────────────────────
//
// These types make TLB shootdown targeting and per-page rollback progress
// explicit so future plan-first decompositions can use them. All are
// helper-only scaffolding; no live conversion is wired in Stage 5D.
//
// See KERNEL_LOCKING.md §19 for the full audit and lock-sequence table.

/// Stage 5D: Computed TLB shootdown target set for a single-page unmap.
///
/// Captured from the scheduler domain (rank 1) + task domain (rank 2) before
/// any vm (rank 5) or ipc (rank 3) domain is touched. In the future plan-first
/// path, this snapshot eliminates the per-page re-computation of `live_cpu_bitmap_for_asid`
/// inside the unmap loop.
///
/// When `target_cpu_bitmap == 0` no cross-CPU notification is needed (the page
/// is only live on the requester CPU) and `request_live_asid_shootdown` returns
/// immediately without acquiring the ipc lock — making per-page unmap fast-path
/// entirely ipc-lock-free in the single-CPU or private-ASID case.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TlbShootdownRequestPlan {
    /// ASID whose TLB entry is being invalidated.
    pub(crate) asid: Asid,
    /// Page-aligned virtual address of the unmapped page.
    pub(crate) virt: VirtAddr,
    /// Bitmask of CPUs that must receive and ACK the shootdown.
    /// Excludes the requester bit. Zero means no cross-CPU work needed.
    pub(crate) target_cpu_bitmap: crate::kernel::topology::CpuBitmap,
    /// The CPU performing the unmap (excluded from targets).
    pub(crate) requester: crate::kernel::scheduler::CpuId,
}

/// Stage 5D: Per-page mapping progress for VmAnonMap rollback tracking.
///
/// Addresses Stage 5C blocker #2: the per-page loop variable `va` was an
/// implicit bare `usize`; this struct makes the progress interval explicit.
///
/// Invariant: `base_addr ≤ mapped_end ≤ end_addr`; all three are multiples
/// of `PAGE_SIZE`. When `mapped_end == base_addr` the rollback range is empty
/// (nothing to unmap). Rollback covers `[base_addr, mapped_end)` only, never
/// the full `[base_addr, end_addr)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VmPageMapProgress {
    /// Page-aligned start of the requested mapping range.
    pub(crate) base_addr: usize,
    /// Exclusive upper bound of pages that have been successfully mapped.
    /// Rollback must cover `[base_addr, mapped_end)` and no more.
    pub(crate) mapped_end: usize,
    /// Page-aligned end of the total requested range.
    pub(crate) end_addr: usize,
}

/// Stage 5D: Progress-aware VmAnonMap plan (strengthens Stage 5C VmAnonMapPlan).
///
/// Replaces the bare `va` loop variable with an explicit `VmPageMapProgress`.
/// This, combined with the explicit-ASID helpers from Stage 5C and the
/// `TlbShootdownRequestPlan` from Stage 5D, resolves Stage 5C blocker #2.
///
/// Stage 9: live-wired in handle_vm_anon_map; all blockers resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VmAnonMapProgressPlan {
    /// Lock-free validated syscall arguments (same as VmAnonMapPlan.validated).
    pub(crate) validated: VmAnonMapValidatedArgs,
    /// TID of the calling thread (scheduler snapshot, rank 1).
    pub(crate) tid: u64,
    /// ASID of the calling task's address space (task snapshot, rank 2).
    pub(crate) asid: Asid,
    /// Explicit per-page mapping progress (addresses Stage 5C blocker #2).
    pub(crate) progress: VmPageMapProgress,
}

// ── Stage 5E: Two-phase unmap / rank-safe TLB wait plan types ─────────────────
//
// These types implement the rank-safe two-phase unmap design that resolves
// blocker #1 (frame reclamation before TLB shootdown) at the scaffolding level.
//
// ## Background
//
// The current unmap path calls `reclaim_memory_object_for_phys` BEFORE
// `request_live_asid_shootdown`. Under the global lock this is safe: no
// concurrent thread can map the reclaimed frame before shootdown completes.
// For future global-lock removal, the frame MUST NOT be reused until all
// remote CPUs have acknowledged the TLB invalidation.
//
// ## Two-phase design
//
//   Phase 1 — `unmap_page_phase1()` (vm rank 5, memory rank 6, sequential):
//     - Remove page table entry           (vm lock, rank 5)
//     - Clear COW record                  (memory lock, rank 6)
//     - Decrement map_refcount            (memory lock, rank 6)
//     - Return TlbShootdownWaitPlan       (carries asid, virt, phys, target_bitmap)
//     - Does NOT reclaim frame
//
//   Phase 2 — TLB notification (ipc lock, rank 3):
//     - IF plan.target_cpu_bitmap != 0:
//         request_live_asid_shootdown(plan.asid, plan.virt)
//     - ELSE: skip (ipc lock never acquired)
//
//   Phase 3 — Frame reclamation (memory lock, rank 6):
//     - reclaim_memory_object_for_phys(plan.phys)
//
// Under this ordering, ipc(3) is acquired BETWEEN memory(6) uses, never
// simultaneously. The frame (plan.phys) is held until after phase 2, so
// no other mapping can reuse it while remote CPUs still hold stale TLBs.
//
// See KERNEL_LOCKING.md §20 for the full design and blocker analysis.

/// Stage 5E: Two-phase unmap TLB wait plan.
///
/// Extends `TlbShootdownRequestPlan` with the physical frame address, enabling
/// frame reclamation to be deferred until AFTER TLB shootdown completes.
///
/// ## Safety invariant
///
/// The caller of `unmap_page_phase1` must NOT call `reclaim_memory_object_for_phys`
/// on `plan.phys` until EITHER:
/// - `plan.target_cpu_bitmap == 0` (no remote CPUs hold stale TLBs), OR
/// - `request_live_asid_shootdown(plan.asid, plan.virt)` has returned `Ok(())`.
///
/// Violating this ordering under a global-lock-free design would allow stale TLB
/// entries on remote CPUs to point to a reused physical frame.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TlbShootdownWaitPlan {
    /// ASID whose TLB entry was invalidated in phase 1.
    pub(crate) asid: Asid,
    /// Page-aligned virtual address removed in phase 1.
    pub(crate) virt: VirtAddr,
    /// Target CPU bitmap (scheduler+task snapshot). Zero = fast path, no shootdown.
    pub(crate) target_cpu_bitmap: crate::kernel::topology::CpuBitmap,
    /// CPU that performed phase 1 (excluded from targets).
    pub(crate) requester: crate::kernel::scheduler::CpuId,
    /// Physical frame to reclaim in phase 3 (AFTER shootdown in phase 2).
    pub(crate) phys: PhysAddr,
}

/// Stage 5E: Aggregate TLB plan for a VmBrk shrink operation.
///
/// Captures the per-ASID shootdown state for all pages in the shrink range.
/// In the future two-phase design, all pages are unmapped first (phase 1), then
/// a single ASID-wide batch shootdown is issued (phase 2), then all frames are
/// reclaimed (phase 3). This reduces the N-page shrink from N serial IPC waits
/// to one.
///
/// `aggregate_target_bitmap` is the union of per-page target bitmaps from phase 1.
/// If it is zero, no cross-CPU notification is needed and the batch shootdown is
/// skipped entirely.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VmBrkShrinkTlbPlan {
    /// ASID being shrunk.
    pub(crate) asid: Asid,
    /// Page-aligned start of the unmap range.
    pub(crate) unmap_start: usize,
    /// Page-aligned exclusive end of the unmap range.
    pub(crate) unmap_end: usize,
    /// Union of per-page target bitmaps from phase 1.
    /// Zero means no shootdown is needed (all pages were private to requester CPU).
    pub(crate) aggregate_target_bitmap: crate::kernel::topology::CpuBitmap,
}

/// Stage 5E: Aggregate TLB plan for a VmAnonMap rollback operation.
///
/// Captures the rollback range and accumulated shootdown state. In the future
/// two-phase design, all rollback unmaps happen in phase 1, then one shootdown
/// covers all removed pages in phase 2, then frames are reclaimed in phase 3.
///
/// Together with `VmAnonMapProgressPlan` (Stage 5D), this struct closes the
/// last structural gap for plan-first VmAnonMap decomposition. The remaining
/// blocker is x86_64 smoke approval (blocker #3).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VmAnonMapRollbackTlbPlan {
    /// ASID of the task whose pages are being rolled back.
    pub(crate) asid: Asid,
    /// Pages to roll back: [progress.base_addr, progress.mapped_end).
    pub(crate) progress: VmPageMapProgress,
    /// Union of per-page target bitmaps accumulated during rollback phase 1.
    pub(crate) aggregate_target_bitmap: crate::kernel::topology::CpuBitmap,
}

/// Stage 116 / Solution 1: typed context-switch plan for D6 dispatch.
///
/// Built under the `task_state_lock` (rank 2) inside
/// `maybe_switch_kernel_context` and used after that sub-lock is released.
/// Contains only raw pointers into stable `KernelState::tcbs` storage and
/// copied scalar values — no Rust references, no live lock guards, no
/// borrowed scheduler state survive the sub-lock boundary.
///
/// Safety invariant: the raw pointer fields are valid only while the outer
/// global `SpinLock<KernelState>` (from `SharedKernel::with_cpu`) is held
/// on the current CPU, OR while interrupts are disabled (single-CPU, trap
/// path) after the global lock has been dropped for Stage 117 out-of-lock
/// `switch_frames`. No cross-CPU sharing occurs.
///
/// VALIDATION: D6_SWITCH_PLAN_READY / D6_GLOBAL_LOCK_DROP_PLAN_READY
pub(crate) struct DispatchSwitchPlan {
    /// TID of the outgoing (currently-running) task.
    pub(crate) outgoing_tid: u64,
    /// TID of the incoming (next-to-run) task.
    pub(crate) incoming_tid: u64,
    /// Raw pointer to the outgoing task's `ArchSwitchContext` frame.
    ///
    /// Derived from `&mut TCB.kernel_context.frame` under `task_state_lock`.
    /// After lock release, valid because `KernelState::tcbs` is a fixed-size
    /// array (no move/reallocation) and the global lock is still held (Stage 116)
    /// or interrupts are disabled on a single CPU (Stage 117 stash path).
    pub(crate) outgoing_frame_ptr: *mut crate::kernel::task::ArchSwitchContext,
    /// Raw pointer to the incoming task's `ArchSwitchContext` frame.
    ///
    /// Derived from `&mut TCB.kernel_context.frame` under `task_state_lock`.
    /// Stored as `*mut` so that `yarm_kernel_thread_switch_trampoline` can use
    /// it as the `prev` parameter of a switch-back `switch_frames` call on the
    /// first-resume path.
    pub(crate) incoming_frame_ptr: *mut crate::kernel::task::ArchSwitchContext,
    /// Incoming task's kernel stack top (copied scalar, not a reference).
    ///
    /// Copied from `incoming_tcb.kernel_context.stack_top` under the lock;
    /// no reference into TCB storage survives after `task_state_lock` drops.
    pub(crate) incoming_stack_top: Option<u64>,
    /// Outgoing task's kernel stack top (copied scalar, not a reference).
    ///
    /// Used by the first-resume trampoline when switching back to the outgoing
    /// task: passed as `next_kernel_stack_top` to update TSS RSP0 on x86_64.
    pub(crate) outgoing_stack_top: Option<u64>,
}

/// Stage 117: per-CPU stash cell for a `DispatchSwitchPlan` that will be
/// drained (via `switch_frames`) OUTSIDE the global `SharedKernel::with_cpu`
/// lock.
///
/// # Safety
///
/// This cell is only accessed from the trap path on the local CPU, always
/// with interrupts disabled (hardware trap entry disables IRQs; the outer
/// `SpinLock<KernelState>` does not save/restore IRQ state, so IRQs remain
/// disabled after it is dropped). No cross-CPU sharing occurs. Only one plan
/// can be stashed per CPU at a time.
pub(crate) struct PerCpuSwitchPlanStash {
    inner: core::cell::UnsafeCell<Option<DispatchSwitchPlan>>,
}

// SAFETY: Accessed only from the local CPU's trap path with interrupts
// disabled. No concurrent access from other threads/CPUs is possible.
unsafe impl Sync for PerCpuSwitchPlanStash {}

impl PerCpuSwitchPlanStash {
    pub(crate) const fn new() -> Self {
        Self {
            inner: core::cell::UnsafeCell::new(None),
        }
    }

    /// Store a plan in the stash.
    ///
    /// # Safety
    ///
    /// Caller must ensure no concurrent access (interrupts disabled, single
    /// CPU).
    pub(crate) unsafe fn store(&self, plan: DispatchSwitchPlan) {
        unsafe { *self.inner.get() = Some(plan) }
    }

    /// Take the plan from the stash (consumes it), leaving the slot empty.
    ///
    /// # Safety
    ///
    /// Caller must ensure no concurrent access (interrupts disabled, single
    /// CPU).
    pub(crate) unsafe fn take(&self) -> Option<DispatchSwitchPlan> {
        unsafe { (*self.inner.get()).take() }
    }

    /// Return `true` if a plan is currently stashed without consuming it.
    ///
    /// # Safety
    ///
    /// Caller must ensure no concurrent access.
    pub(crate) unsafe fn has_plan(&self) -> bool {
        unsafe { (*self.inner.get()).is_some() }
    }
}

/// Per-CPU stash for `DispatchSwitchPlan` used by the Stage 117 global-lock
/// drop path. Index by `CpuId.0`. Accessed only from the trap path on the
/// local CPU with interrupts disabled.
///
/// VALIDATION: D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH
pub(crate) static DISPATCH_SWITCH_PLAN_STASH: [PerCpuSwitchPlanStash;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { PerCpuSwitchPlanStash::new() }; crate::kernel::scheduler::MAX_CPUS];

/// Per-CPU flag indicating that `handle_trap_entry_shared` is active and will
/// drain the stash AFTER `with_cpu` returns. When `false`, code calling
/// `dispatch_next_task` directly (e.g., unit tests) must not stash — there
/// would be no external drainer and the context switch would be lost.
///
/// Set to `true` by `handle_trap_entry_shared` before `with_cpu`, cleared
/// after the stash drain completes.
///
/// VALIDATION: D6_GLOBAL_LOCK_DROP_PLAN_BEGIN
pub(crate) static GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE: [core::sync::atomic::AtomicBool;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 120: x86_64-only controlled one-shot unlocked `switch_frames` proof
/// harness gate. This is diagnostic/smoke-only, default-off, single-CPU-only,
/// and does not alter scheduler policy. VALIDATION: D6_CONTROLLED_SWITCH_PROOF_BEGIN
pub(crate) static D6_CONTROLLED_SWITCH_PROOF_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
pub(crate) static D6_CONTROLLED_SWITCH_PROOF_STARTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
pub(crate) static D6_CONTROLLED_SWITCH_PROOF_PENDING_DONE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
pub(crate) static D6_CONTROLLED_SWITCH_PROOF_DONE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
/// Stage 132: per-CPU one-shot flag set after D6 proof CLEANUP_DONE.
/// Consumed by the x86_64 trap handler on the first post-cleanup trap entry to
/// emit D6_POST_CLEANUP_FIRST_TRAP_* diagnostic markers capturing vector, error
/// code, CR2, RSP (derived), R14 (kernel ptr), TID, ASID, TSS RSP0, and stack
/// classification (cr2_below_mapped_stack / cr2_inside_mapped_stack / unknown).
pub(crate) static D6_POST_CLEANUP_DIAG_PENDING: [core::sync::atomic::AtomicBool;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; crate::kernel::scheduler::MAX_CPUS];
/// Stage 133: per-CPU one-shot flag set after D6 proof CLEANUP_DONE.
/// Consumed by the x86_64 trap dispatcher on the first post-cleanup #PF,
/// BEFORE acquiring any KernelState lock, to emit D6_PRE_LOCK_PF_DIAG_*
/// markers with raw trap register values: actual RIP, RSP (hardware-saved),
/// R14 (from the trap stub push), RSP-8, computed lock pointer, and a
/// classification label (stack_push / r14_lockptr / other).
pub(crate) static D6_PRE_LOCK_PF_DIAG_PENDING: [core::sync::atomic::AtomicBool;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; crate::kernel::scheduler::MAX_CPUS];

pub(crate) fn set_d6_controlled_switch_proof_enabled(enabled: bool) {
    D6_CONTROLLED_SWITCH_PROOF_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
    if !enabled {
        D6_CONTROLLED_SWITCH_PROOF_STARTED.store(false, core::sync::atomic::Ordering::Release);
        D6_CONTROLLED_SWITCH_PROOF_PENDING_DONE.store(false, core::sync::atomic::Ordering::Release);
        D6_CONTROLLED_SWITCH_PROOF_DONE.store(false, core::sync::atomic::Ordering::Release);
    }
}

pub(crate) fn d6_controlled_switch_proof_enabled() -> bool {
    D6_CONTROLLED_SWITCH_PROOF_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 166 (D6-SWITCH-A): x86_64-only, default-off gate that opts a real
/// production `switch_frames` context switch into the unlocked (global-lock-
/// dropped) path proven by D6-SWITCH-SMOKE.  Separate from the diagnostic
/// `d6_switch_proof` knob.  When OFF (default), production initialized-pair
/// switches use the proven Stage 116 lock-held fallback (no behavior change);
/// when ON, the first such production switch drops the global `SpinLock<KernelState>`
/// before `switch_frames` and emits `D6_SWITCH_A_*` markers.  This is the first
/// narrow production Outcome A; it is not scheduler policy and is reversible by
/// dropping the knob.  VALIDATION: D6_SWITCH_A_ENABLED.
pub(crate) static D6_SWITCH_A_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_d6_switch_a_enabled(enabled: bool) {
    D6_SWITCH_A_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub(crate) fn d6_switch_a_enabled() -> bool {
    D6_SWITCH_A_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 167 (D6-GENUINE-A): x86_64-only, default-off gate that turns the
/// rank-1 scheduler split seam (`SharedKernel::with_scheduler_split_mut`) into
/// its first live production caller.  When OFF (default) the seam stays
/// helper-only and the authoritative dispatch decision is taken exclusively by
/// the in-lock `local_dispatch_step_split` (`self.scheduler_state()` under the
/// global `with_cpu` borrow) — no behavior change.  When ON, after `with_cpu`
/// has returned and the global `SpinLock<KernelState>` guard is dropped, the
/// trap-entry path runs one genuine `local_dispatch_step_split` observation
/// through the seam holding ONLY the rank-1 scheduler lock, proving the
/// scheduler dispatch step can execute outside the global lock.  The
/// observation is non-mutating (it reads the committed dispatch decision), so
/// it never double-advances the run queue, and the in-lock path remains the
/// authoritative fallback.  This is the narrow Outcome A for the scheduler
/// seam; it is not scheduler policy and is reversible by dropping the knob.
/// VALIDATION: D6_GENUINE_ENABLED.
pub(crate) static D6_GENUINE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 167: per-CPU count of genuine scheduler-seam dispatch observations.
pub(crate) static D6_GENUINE_SEAM_COUNT: [core::sync::atomic::AtomicU64;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; crate::kernel::scheduler::MAX_CPUS];

pub(crate) fn set_d6_genuine_enabled(enabled: bool) {
    D6_GENUINE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub(crate) fn d6_genuine_enabled() -> bool {
    D6_GENUINE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 168 (D6-GENUINE-B): global count of authoritative mutating dispatch
/// steps that ran through the scheduler seam OUTSIDE the global KernelState
/// lock. Emitted as `D6_GENUINE_MUT_DISPATCH_COUNT value=<n>`.
pub(crate) static D6_GENUINE_MUT_DISPATCH_COUNT: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Stage 168 (D6-GENUINE-B): per-CPU "authoritative dispatch deferred" flag.
/// Set by the in-lock `dispatch_next_task` when it declines to perform the
/// authoritative mutating dispatch (eligible, queue-neutral d6_genuine case)
/// and instead defers it to the out-of-global-lock seam drained by the trap
/// entry. Cleared by the drain (or by any in-lock fallback dispatch that
/// supersedes the deferral). VALIDATION: D6_GENUINE_MUT_DISPATCH_PREPARED.
pub(crate) static D6_GENUINE_DISPATCH_DEFERRED: [core::sync::atomic::AtomicBool;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 168: per-CPU outgoing TID recorded when a dispatch is deferred
/// (`u64::MAX` sentinel for "no current task / idle"). Diagnostic only.
pub(crate) static D6_GENUINE_DISPATCH_OUTGOING: [core::sync::atomic::AtomicU64;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(u64::MAX) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 168: record a deferred authoritative dispatch intent for `cpu`.
/// Returns false (declining to defer) if an intent is already pending — the
/// caller must then fall back to the in-lock dispatch (no nested deferral).
pub(crate) fn d6_genuine_dispatch_try_defer(cpu_idx: usize, outgoing: Option<u64>) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    if D6_GENUINE_DISPATCH_DEFERRED[cpu_idx]
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        return false;
    }
    D6_GENUINE_DISPATCH_OUTGOING[cpu_idx].store(
        outgoing.unwrap_or(u64::MAX),
        core::sync::atomic::Ordering::Release,
    );
    true
}

/// Stage 168: is a deferred authoritative dispatch pending for `cpu`?
pub(crate) fn d6_genuine_dispatch_is_deferred(cpu_idx: usize) -> bool {
    cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && D6_GENUINE_DISPATCH_DEFERRED[cpu_idx].load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 168: clear the deferred flag for `cpu` (drain complete, or an in-lock
/// fallback dispatch superseded the deferral). Returns the prior state.
pub(crate) fn d6_genuine_dispatch_clear_deferred(cpu_idx: usize) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    D6_GENUINE_DISPATCH_OUTGOING[cpu_idx].store(u64::MAX, core::sync::atomic::Ordering::Release);
    D6_GENUINE_DISPATCH_DEFERRED[cpu_idx].swap(false, core::sync::atomic::Ordering::AcqRel)
}

/// Stage 168 (D2-GENUINE-RECV): x86_64-only, default-off gate that runs the
/// blocking-receive path through explicit rank-clean scheduler/task/IPC phase
/// markers and uses the Stage 168 out-of-global-lock dispatch seam where the
/// resulting dispatch is queue-neutral-eligible. When OFF (default) the recv
/// path is byte-identical to Stage 163P (no behavior change). Immediate /
/// NoWait / timeout / rollback semantics are preserved on both paths.
/// VALIDATION: D2_RECV_GENUINE_ENABLED.
pub(crate) static D2_RECV_GENUINE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_d2_recv_genuine_enabled(enabled: bool) {
    D2_RECV_GENUINE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub(crate) fn d2_recv_genuine_enabled() -> bool {
    D2_RECV_GENUINE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 168B (D2-GENUINE-RECV completion): global count of blocking-recv
/// queue-advancing dispatches that ran through the scheduler seam OUTSIDE the
/// global KernelState lock. Emitted as `D2_RECV_GENUINE_DISPATCH_DONE`.
pub(crate) static D2_RECV_DISPATCH_COUNT: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Stage 168B: per-CPU "blocking-recv dispatch deferred" flag. Set by the
/// in-lock `block_current_on_receive_with_deadline` when it commits the block
/// (waiter published, current task `Blocked`) and defers the queue-advancing
/// dispatch to the out-of-global-lock trap-entry drain instead of running the
/// authoritative dispatch in-lock. Cleared by the drain.
/// VALIDATION: D2_RECV_GENUINE_DISPATCH_DEFERRED.
pub(crate) static D2_RECV_DISPATCH_DEFERRED: [core::sync::atomic::AtomicBool;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 168B: per-CPU blocked (outgoing) recv TID recorded with the deferral,
/// so the drain can re-verify the task is still `Blocked(EndpointReceive)`
/// before running the queue-advancing dispatch (`u64::MAX` sentinel = unset).
pub(crate) static D2_RECV_DISPATCH_OUTGOING: [core::sync::atomic::AtomicU64;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(u64::MAX) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 168B: record a deferred blocking-recv dispatch intent for `cpu`.
/// Returns false (declining to defer, caller must fall back to the in-lock
/// dispatch) if an intent is already pending — no nested deferral.
pub(crate) fn d2_recv_dispatch_try_defer(cpu_idx: usize, outgoing: u64) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    if D2_RECV_DISPATCH_DEFERRED[cpu_idx]
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        return false;
    }
    D2_RECV_DISPATCH_OUTGOING[cpu_idx].store(outgoing, core::sync::atomic::Ordering::Release);
    true
}

/// Stage 168B: is a deferred blocking-recv dispatch pending for `cpu`?
pub(crate) fn d2_recv_dispatch_is_deferred(cpu_idx: usize) -> bool {
    cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && D2_RECV_DISPATCH_DEFERRED[cpu_idx].load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 168B: read the deferred blocking-recv outgoing TID for `cpu`
/// (`None` if unset).
pub(crate) fn d2_recv_dispatch_outgoing(cpu_idx: usize) -> Option<u64> {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    let v = D2_RECV_DISPATCH_OUTGOING[cpu_idx].load(core::sync::atomic::Ordering::Acquire);
    if v == u64::MAX { None } else { Some(v) }
}

/// Stage 168B: clear the blocking-recv dispatch deferral for `cpu`.
pub(crate) fn d2_recv_dispatch_clear(cpu_idx: usize) {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return;
    }
    D2_RECV_DISPATCH_OUTGOING[cpu_idx].store(u64::MAX, core::sync::atomic::Ordering::Release);
    D2_RECV_DISPATCH_DEFERRED[cpu_idx].store(false, core::sync::atomic::Ordering::Release);
}

/// Stage 169 (D2-GENUINE-SEND): x86_64-only, default-off gate that runs the
/// blocking-SEND path (endpoint full / synchronous no-waiter) through explicit
/// rank-clean scheduler/task/IPC phase markers and relocates its queue-advancing
/// dispatch OUT of the global lock, exactly as Stage 168B did for recv. When OFF
/// (default) the send path is byte-identical to Stage 168B (no behavior change);
/// the Stage 163P sender-wake oracle is preserved on both paths.
/// VALIDATION: D2_SEND_GENUINE_ENABLED.
pub(crate) static D2_SEND_GENUINE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_d2_send_genuine_enabled(enabled: bool) {
    D2_SEND_GENUINE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub(crate) fn d2_send_genuine_enabled() -> bool {
    D2_SEND_GENUINE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 169: global count of blocking-send queue-advancing dispatches that ran
/// through the scheduler seam OUTSIDE the global lock.
pub(crate) static D2_SEND_DISPATCH_COUNT: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Stage 169: per-CPU "blocking-send dispatch deferred" flag (mirrors the
/// Stage 168B recv deferral). Set by the in-lock
/// `block_current_on_send_with_deadline` after the sender-waiter is published
/// and the sender is `Blocked(EndpointSend)`; drained out of the global lock by
/// the trap entry. VALIDATION: D2_SEND_GENUINE_DISPATCH_DEFERRED.
pub(crate) static D2_SEND_DISPATCH_DEFERRED: [core::sync::atomic::AtomicBool;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 169: per-CPU blocked (outgoing) sender TID recorded with the deferral
/// so the drain can re-verify `Blocked(EndpointSend)` before dispatching
/// (`u64::MAX` sentinel = unset).
pub(crate) static D2_SEND_DISPATCH_OUTGOING: [core::sync::atomic::AtomicU64;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(u64::MAX) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 169: record a deferred blocking-send dispatch intent for `cpu`.
/// Returns false (caller must fall back to the in-lock dispatch) if an intent
/// is already pending — no nested deferral.
pub(crate) fn d2_send_dispatch_try_defer(cpu_idx: usize, outgoing: u64) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    if D2_SEND_DISPATCH_DEFERRED[cpu_idx]
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        return false;
    }
    D2_SEND_DISPATCH_OUTGOING[cpu_idx].store(outgoing, core::sync::atomic::Ordering::Release);
    true
}

/// Stage 169: is a deferred blocking-send dispatch pending for `cpu`?
pub(crate) fn d2_send_dispatch_is_deferred(cpu_idx: usize) -> bool {
    cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && D2_SEND_DISPATCH_DEFERRED[cpu_idx].load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 169: read the deferred blocking-send outgoing TID for `cpu`.
pub(crate) fn d2_send_dispatch_outgoing(cpu_idx: usize) -> Option<u64> {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    let v = D2_SEND_DISPATCH_OUTGOING[cpu_idx].load(core::sync::atomic::Ordering::Acquire);
    if v == u64::MAX { None } else { Some(v) }
}

/// Stage 169: clear the blocking-send dispatch deferral for `cpu`.
pub(crate) fn d2_send_dispatch_clear(cpu_idx: usize) {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return;
    }
    D2_SEND_DISPATCH_OUTGOING[cpu_idx].store(u64::MAX, core::sync::atomic::Ordering::Release);
    D2_SEND_DISPATCH_DEFERRED[cpu_idx].store(false, core::sync::atomic::Ordering::Release);
}

/// Stage 171 (SCHED-TIMEOUT): arch-neutral, default-off DIAGNOSTIC gate for the
/// scheduler timeout/deadline hardening markers. When OFF (default) the timeout
/// scan runs byte-identically (only the always-on chunked-scan hardening applies)
/// and emits none of the `SCHED_TIMEOUT_*` / `SCHED_IDLE_*` markers. When ON, the
/// per-tick timeout scan and the idle-entry path emit rank-clean phase markers so
/// a QEMU acceptance profile can prove no stranded waiters, exactly-once wake, and
/// idle-with-pending-timeout safety. It changes NO scheduling behavior and no ABI.
/// VALIDATION: SCHED_TIMEOUT_ENABLED.
pub(crate) static SCHED_TIMEOUT_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_sched_timeout_enabled(enabled: bool) {
    SCHED_TIMEOUT_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub(crate) fn sched_timeout_enabled() -> bool {
    SCHED_TIMEOUT_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 171: rate-limit for the (frequent) idle-entry timeout markers so the
/// diagnostic profile does not flood the UART. Returns true for the first
/// `SCHED_IDLE_MARKER_BUDGET` idle entries after the knob is enabled.
pub(crate) static SCHED_IDLE_MARKER_SEQ: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
pub(crate) const SCHED_IDLE_MARKER_BUDGET: u64 = 8;

pub(crate) fn sched_idle_marker_budget_remaining() -> bool {
    SCHED_IDLE_MARKER_SEQ.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
        < SCHED_IDLE_MARKER_BUDGET
}

/// Stage 172 (VM-COW): arch-neutral, default-off DIAGNOSTIC gate for the
/// VM/COW/page-table/fork phase-boundary markers. When OFF (default) the VM/COW
/// paths run byte-identically and emit none of the `VM_COW_*` / `VM_MAP_*` /
/// `VM_UNMAP_*` / `VM_TLB_*` markers. When ON, the COW fault handler, the fork COW
/// clone + rollback, and the map/unmap syscall handlers emit rank-clean phase
/// markers so a QEMU acceptance profile can prove phase boundaries, rollback, and
/// TLB-shootdown prep. It changes NO VM behavior and no ABI (the existing
/// transactional rollback and `PAGE_FAULT_HANDLED_COW` handling are untouched).
/// VALIDATION: VM_COW_ENABLED.
pub(crate) static VM_COW_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_vm_cow_enabled(enabled: bool) {
    VM_COW_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub(crate) fn vm_cow_enabled() -> bool {
    VM_COW_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 173 (CAP-CNODE): arch-neutral, default-off DIAGNOSTIC gate for the
/// capability/CNode phase-boundary markers + a one-shot self-contained proof.
/// When OFF (default) the cap/CNode paths run byte-identically and emit none of
/// the `CAP_CNODE_*` markers. When ON, the reply-cap consume and cap-transfer
/// production paths emit phase markers, and a bounded one-shot proof
/// (`maybe_run_cap_cnode_proof`) deterministically exercises reserve →
/// materialize → lookup → release → stale-lookup-rejected → double-release-
/// rejected → invariant-check. It changes NO cap/CNode behavior and no ABI.
/// VALIDATION: CAP_CNODE_ENABLED.
pub(crate) static CAP_CNODE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 173: one-shot latch so the cap/CNode proof runs exactly once.
pub(crate) static CAP_CNODE_PROOF_STARTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_cap_cnode_enabled(enabled: bool) {
    CAP_CNODE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub(crate) fn cap_cnode_enabled() -> bool {
    CAP_CNODE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 173: try to claim the one-shot cap/CNode proof (true exactly once).
pub(crate) fn cap_cnode_proof_try_start() -> bool {
    CAP_CNODE_PROOF_STARTED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
}

/// Stage 174 (FAULT-DELIVERY): arch-neutral, default-off DIAGNOSTIC gate for the
/// kernel-fault → supervisor delivery / fault-channel lifecycle markers + the
/// one-shot fault-delivery proof. It changes NO fault/IPC/ABI behavior — only
/// emits FAULT_DELIVERY_* markers. VALIDATION: FAULT_DELIVERY_ENABLED.
pub(crate) static FAULT_DELIVERY_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 174: one-shot latch so the fault-delivery proof runs exactly once.
pub(crate) static FAULT_DELIVERY_PROOF_STARTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_fault_delivery_enabled(enabled: bool) {
    FAULT_DELIVERY_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub(crate) fn fault_delivery_enabled() -> bool {
    FAULT_DELIVERY_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 174: try to claim the one-shot fault-delivery proof (true exactly once).
pub(crate) fn fault_delivery_proof_try_start() -> bool {
    FAULT_DELIVERY_PROOF_STARTED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
}

/// Stage 175 (SPAWN-LIFECYCLE): arch-neutral, default-off DIAGNOSTIC gate for the
/// spawn / image-loading / lifecycle-metadata phase markers + the one-shot
/// spawn-lifecycle rollback proof. It changes NO spawn/PM/ABI behavior — only emits
/// SPAWN_LIFECYCLE_* markers. VALIDATION: SPAWN_LIFECYCLE_ENABLED.
pub(crate) static SPAWN_LIFECYCLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 175: one-shot latch so the spawn-lifecycle proof runs exactly once.
pub(crate) static SPAWN_LIFECYCLE_PROOF_STARTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_spawn_lifecycle_enabled(enabled: bool) {
    SPAWN_LIFECYCLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub(crate) fn spawn_lifecycle_enabled() -> bool {
    SPAWN_LIFECYCLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 175: try to claim the one-shot spawn-lifecycle proof (true exactly once).
pub(crate) fn spawn_lifecycle_proof_try_start() -> bool {
    SPAWN_LIFECYCLE_PROOF_STARTED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
}

/// Stage 176 (GLOBAL-STATE): arch-neutral, default-off DIAGNOSTIC gate for the
/// remaining direct global-`KernelState` mutation audit + lock-rank discipline
/// markers + the one-shot global-state audit. It changes NO state/ABI behavior —
/// only emits GLOBAL_STATE_* markers. VALIDATION: GLOBAL_STATE_ENABLED.
pub(crate) static GLOBAL_STATE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 176: one-shot latch so the global-state audit runs exactly once.
pub(crate) static GLOBAL_STATE_AUDIT_STARTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_global_state_enabled(enabled: bool) {
    GLOBAL_STATE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub(crate) fn global_state_enabled() -> bool {
    GLOBAL_STATE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 176: try to claim the one-shot global-state audit (true exactly once).
pub(crate) fn global_state_audit_try_start() -> bool {
    GLOBAL_STATE_AUDIT_STARTED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
}

pub(crate) fn d6_controlled_switch_proof_done() -> bool {
    D6_CONTROLLED_SWITCH_PROOF_DONE.load(core::sync::atomic::Ordering::Acquire)
}

pub(crate) fn d6_controlled_switch_proof_try_start() -> bool {
    D6_CONTROLLED_SWITCH_PROOF_STARTED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
}

pub(crate) fn d6_controlled_switch_proof_mark_pending_done() {
    D6_CONTROLLED_SWITCH_PROOF_PENDING_DONE.store(true, core::sync::atomic::Ordering::Release);
}

pub(crate) fn d6_controlled_switch_proof_take_pending_done() -> bool {
    D6_CONTROLLED_SWITCH_PROOF_PENDING_DONE.swap(false, core::sync::atomic::Ordering::AcqRel)
}

pub(crate) fn d6_controlled_switch_proof_mark_done() {
    D6_CONTROLLED_SWITCH_PROOF_DONE.store(true, core::sync::atomic::Ordering::Release);
}

/// Stage 159: `yarm.ipc_recv_proof=1` gate for the default-off userspace IPC
/// recv-v2 oracle exercise client. When set, the control-plane bootstrap
/// provisions a dedicated loopback endpoint into the exercise workload, which
/// then deterministically drives the three recv-v2 delivery markers that a
/// normal boot does not reliably exercise on every arch:
/// `IPC_RECV_V2_META_QUEUED_SPLIT_OK`, `IPC_RECV_V2_SENDER_WAKE_ORDER_OK`, and
/// `IPC_RECV_V2_ROLLBACK_OK`. Diagnostic/smoke-only, arch-neutral, default-off;
/// it provisions nothing and runs nothing unless explicitly enabled.
pub(crate) static IPC_RECV_ORACLE_PROOF_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_ipc_recv_oracle_proof_enabled(enabled: bool) {
    IPC_RECV_ORACLE_PROOF_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn ipc_recv_oracle_proof_enabled() -> bool {
    IPC_RECV_ORACLE_PROOF_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 163A: buffered capacity (max queue depth) of the proof loopback endpoint
/// E1. Communicated to init (startup slot 14) so the sender-wake workload can fill
/// E1 to EXACTLY full with non-blocking sends and never become a sender-waiter
/// itself — a buffered send on a full endpoint blocks the sender even with a zero
/// timeout, so init must never attempt the (capacity+1)-th send.
pub const IPC_RECV_PROOF_E1_DEPTH: usize = 8;

/// Stage 163: `yarm.ipc_recv_proof_sender_wake=1` SUB-knob, layered on top of
/// `yarm.ipc_recv_proof=1`. Default-off and independent: the sender-wake
/// coordination hook and workload run ONLY when BOTH knobs are set, so the
/// already-green queued-split + rollback proof boots (which set only
/// `yarm.ipc_recv_proof=1`) are completely unaffected. When enabled, the
/// bootstrap additionally provisions a second proof "coordination" endpoint (E2)
/// and the sender-waiter-enqueue path emits a deterministic, race-free
/// waiter-present signal into E2 (see `proof_sender_wake_*` below).
pub(crate) static IPC_RECV_PROOF_SENDER_WAKE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Endpoint INDEX of the proof loopback endpoint E1 (the fill/drain channel), and
/// of the proof coordination endpoint E2 (the waiter-present signal channel),
/// captured at provision time when the sender-wake sub-knob is set. `usize::MAX`
/// means "not provisioned" so the enqueue-waiter hook is a no-op. Only the
/// kernel reads these (to recognize E1 in the sender-waiter-enqueue path and to
/// push the coordination message into E2).
pub(crate) static IPC_RECV_PROOF_SENDER_WAKE_E1_IDX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);
pub(crate) static IPC_RECV_PROOF_SENDER_WAKE_E2_IDX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);

pub(crate) fn set_ipc_recv_proof_sender_wake_enabled(enabled: bool) {
    IPC_RECV_PROOF_SENDER_WAKE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn ipc_recv_proof_sender_wake_enabled() -> bool {
    IPC_RECV_PROOF_SENDER_WAKE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// True only when BOTH the base proof knob and the sender-wake sub-knob are set —
/// the precondition for any sender-wake coordination/workload behavior.
pub fn ipc_recv_proof_sender_wake_active() -> bool {
    ipc_recv_oracle_proof_enabled() && ipc_recv_proof_sender_wake_enabled()
}

/// If `endpoint_idx` is the provisioned proof loopback E1 (and the sender-wake
/// sub-knob is active), return the coordination endpoint E2's index so the caller
/// can push the deterministic waiter-present signal. Returns `None` otherwise —
/// so this is a strict no-op on every endpoint except the proof E1, and only
/// under the sub-knob.
pub(crate) fn proof_sender_wake_coordination_target(endpoint_idx: usize) -> Option<usize> {
    if !ipc_recv_proof_sender_wake_active() {
        return None;
    }
    let e1 = IPC_RECV_PROOF_SENDER_WAKE_E1_IDX.load(core::sync::atomic::Ordering::Acquire);
    let e2 = IPC_RECV_PROOF_SENDER_WAKE_E2_IDX.load(core::sync::atomic::Ordering::Acquire);
    if e1 != usize::MAX && e2 != usize::MAX && endpoint_idx == e1 {
        Some(e2)
    } else {
        None
    }
}

/// Stage 159BC/D: provision the userspace IPC recv-v2 oracle loopback endpoint.
///
/// When (and ONLY when) `yarm.ipc_recv_proof=1` is set, mint a fresh buffered
/// endpoint and grant the init server (TID 1) BOTH a SEND and a RECV capability
/// to it, returning `(send_cap, recv_cap)`. The caller wires these into init's
/// startup-arg slots 6/7 (the otherwise-unused `init_alert_send_ep` /
/// `init_alert_recv_ep` slots — init never receives an alert endpoint in the
/// first-user bootstrap today, so reusing them needs no ABI/slot change). Their
/// PRESENCE is what gates the proof workload in init: a normal boot leaves both
/// slots zero and init behaves byte-identically.
///
/// Holding both caps in one process lets init drive the queued-split and
/// rollback recv-v2 paths deterministically with a single thread
/// (send-to-self enqueues because no receiver is blocked, then recv-from-self
/// drains via the queued-split delivery path) — no cross-process/thread timing
/// race. This is the architecture-native way to obtain an endpoint: userspace
/// cannot mint endpoints, so the kernel bootstrap provisions it, exactly like
/// every other control-plane endpoint.
///
/// Returns `None` when the knob is off (normal boot) or if endpoint/cap
/// provisioning fails (the proof workload is then simply skipped — never fatal).
pub fn provision_init_ipc_recv_proof_loopback(
    kernel: &mut KernelState,
    init_tid: u64,
) -> Option<(u32, u32)> {
    if !ipc_recv_oracle_proof_enabled() {
        return None;
    }
    let (e1_idx, send_root, recv_root) = match kernel.create_endpoint(IPC_RECV_PROOF_E1_DEPTH) {
        Ok(triple) => triple,
        Err(e) => {
            crate::yarm_log!(
                "IPC_RECV_PROOF_LOOPBACK_FAIL step=create_endpoint err={:?}",
                e
            );
            return None;
        }
    };
    // Stage 163: remember E1's endpoint index so the (sub-knob-gated)
    // sender-waiter-enqueue hook can recognize it. Stored unconditionally here;
    // the hook is still inert unless the sender-wake sub-knob is also set.
    IPC_RECV_PROOF_SENDER_WAKE_E1_IDX.store(e1_idx, core::sync::atomic::Ordering::Release);
    let send_cap = match kernel.grant_capability_task_to_task_with_rights(
        0,
        send_root,
        init_tid,
        crate::kernel::capabilities::CapRights::SEND,
    ) {
        Ok(c) => c,
        Err(e) => {
            crate::yarm_log!("IPC_RECV_PROOF_LOOPBACK_FAIL step=grant_send err={:?}", e);
            return None;
        }
    };
    let recv_cap = match kernel.grant_capability_task_to_task_with_rights(
        0,
        recv_root,
        init_tid,
        crate::kernel::capabilities::CapRights::RECEIVE,
    ) {
        Ok(c) => c,
        Err(e) => {
            crate::yarm_log!("IPC_RECV_PROOF_LOOPBACK_FAIL step=grant_recv err={:?}", e);
            return None;
        }
    };
    crate::yarm_log!(
        "IPC_RECV_PROOF_LOOPBACK_OK init_tid={} send_cap={} recv_cap={}",
        init_tid,
        send_cap.0,
        recv_cap.0
    );
    Some((send_cap.0 as u32, recv_cap.0 as u32))
}

/// Stage 163: provision the second proof "coordination" endpoint E2 for the
/// sender-wake proof, and grant init (TID 1) a RECEIVE cap to it. Returns the
/// recv cap, which the caller wires into init's startup slot 13
/// (`service_extra_cap_0`, unused by init). Active ONLY when BOTH the base proof
/// knob and the sender-wake sub-knob are set — so queued-split + rollback proof
/// boots (base knob only) never get E2 and the sender-waiter-enqueue hook stays
/// inert (E2 index left unset).
///
/// E2 carries the deterministic, race-free "sender is a waiter" signal: the
/// kernel pushes a coordination message into E2 from inside the same
/// `enqueue_sender_waiter` critical section that makes the proof sender a waiter
/// on E1, so init (which non-blocking-polls E2) drains E1 only after the sender
/// is provably blocked.
pub fn provision_init_ipc_recv_proof_sender_wake_e2(
    kernel: &mut KernelState,
    init_tid: u64,
) -> Option<u32> {
    if !ipc_recv_proof_sender_wake_active() {
        return None;
    }
    let (e2_idx, _send_root, recv_root) = match kernel.create_endpoint(8) {
        Ok(triple) => triple,
        Err(e) => {
            crate::yarm_log!("IPC_RECV_PROOF_SW_E2_FAIL step=create_endpoint err={:?}", e);
            return None;
        }
    };
    let recv_cap = match kernel.grant_capability_task_to_task_with_rights(
        0,
        recv_root,
        init_tid,
        crate::kernel::capabilities::CapRights::RECEIVE,
    ) {
        Ok(c) => c,
        Err(e) => {
            crate::yarm_log!("IPC_RECV_PROOF_SW_E2_FAIL step=grant_recv err={:?}", e);
            return None;
        }
    };
    IPC_RECV_PROOF_SENDER_WAKE_E2_IDX.store(e2_idx, core::sync::atomic::Ordering::Release);
    crate::yarm_log!(
        "IPC_RECV_PROOF_SW_E2_OK init_tid={} e1_idx={} e2_idx={} recv_cap={}",
        init_tid,
        IPC_RECV_PROOF_SENDER_WAKE_E1_IDX.load(core::sync::atomic::Ordering::Acquire),
        e2_idx,
        recv_cap.0
    );
    Some(recv_cap.0 as u32)
}

/// Stage 163: push the deterministic waiter-present coordination message into the
/// proof coordination endpoint E2. Called from the sender-waiter-enqueue path
/// (which already holds `ipc_state_lock`), so E2's queue — in the same IPC
/// domain — is mutated within the SAME critical section as the waiter enqueue,
/// making "E2 has the signal" an atomic proxy for "the sender is a waiter on E1".
/// No scheduler/cap/user-copy work is done here (init non-blocking-polls E2, so no
/// wake is needed), so there is no lock-order hazard. Best-effort: a full E2 queue
/// (already signalled) is harmless.
pub(crate) fn proof_sender_wake_push_coordination_locked(
    ipc: &mut defs::IpcSubsystem,
    e2_idx: usize,
    waiter_tid: u64,
) {
    if let Some(Some(endpoint_storage)) = ipc.endpoints.get_mut(e2_idx) {
        let endpoint = defs::kernel_mut(endpoint_storage);
        if let Ok(msg) = Message::with_header(waiter_tid, 0, 0, None, &[0xE2u8]) {
            let _ = endpoint.send(msg);
        }
    }
}

/// Stage 118: context for the first-resume trampoline (`yarm_kernel_thread_switch_trampoline`).
///
/// Set by the Stage 117 stash drain in `handle_trap_entry_shared` immediately
/// before calling `switch_frames` for a first-resume incoming task. Consumed by
/// the trampoline on the incoming task's first kernel-context-switch resume.
///
/// # Safety
///
/// Accessed only from the trap path on the local CPU with interrupts disabled.
/// No cross-CPU sharing occurs. Only one context can be stashed per CPU at a time.
pub(crate) struct FirstResumeContext {
    /// CPU ID of the CPU on which the switch is occurring.
    pub(crate) cpu_id: crate::kernel::scheduler::CpuId,
    /// TID of the incoming (first-resuming) task.
    pub(crate) incoming_tid: u64,
    /// Pointer to the outgoing task's frame (for the switch-back `next` arg).
    pub(crate) outgoing_frame_ptr: *const crate::kernel::task::ArchSwitchContext,
    /// Pointer to the incoming task's frame (for the switch-back `prev` arg).
    pub(crate) incoming_frame_ptr: *mut crate::kernel::task::ArchSwitchContext,
    /// Outgoing task's kernel stack top for TSS RSP0 update on switch-back.
    pub(crate) outgoing_stack_top: Option<u64>,
}

/// Stage 118: per-CPU stash for `FirstResumeContext`.
///
/// # Safety
///
/// Accessed only from the local CPU's trap path with interrupts disabled.
/// No concurrent access from other threads or CPUs is possible.
pub(crate) struct PerCpuFirstResumeStash {
    inner: core::cell::UnsafeCell<Option<FirstResumeContext>>,
}

// SAFETY: Accessed only from the local CPU's trap path with interrupts
// disabled. No concurrent access from other threads/CPUs is possible.
unsafe impl Sync for PerCpuFirstResumeStash {}

impl PerCpuFirstResumeStash {
    pub(crate) const fn new() -> Self {
        Self {
            inner: core::cell::UnsafeCell::new(None),
        }
    }

    /// Store a first-resume context in the stash.
    ///
    /// # Safety
    ///
    /// Caller must ensure no concurrent access (interrupts disabled, single CPU).
    pub(crate) unsafe fn store(&self, ctx: FirstResumeContext) {
        unsafe { *self.inner.get() = Some(ctx) }
    }

    /// Take the first-resume context from the stash (consumes it), leaving the slot empty.
    ///
    /// # Safety
    ///
    /// Caller must ensure no concurrent access (interrupts disabled, single CPU).
    pub(crate) unsafe fn take(&self) -> Option<FirstResumeContext> {
        unsafe { (*self.inner.get()).take() }
    }
}

/// Stage 118: per-CPU stash for the first-resume context. Populated by the
/// stash drain in `handle_trap_entry_shared` before the first `switch_frames`
/// for a task whose entry point is `yarm_kernel_thread_switch_trampoline`.
/// Consumed by the trampoline on the incoming task's kernel stack.
///
/// VALIDATION: D6_FIRST_RESUME_ENTER / D6_FIRST_RESUME_POST_SWITCH_RESTORE_DONE
pub(crate) static FIRST_RESUME_STASH: [PerCpuFirstResumeStash; crate::kernel::scheduler::MAX_CPUS] =
    [const { PerCpuFirstResumeStash::new() }; crate::kernel::scheduler::MAX_CPUS];

#[cfg(feature = "hosted-dev")]
const MAX_NOTIFICATIONS: usize = 64;
#[cfg(not(feature = "hosted-dev"))]
const MAX_NOTIFICATIONS: usize = 32;
const MAX_IRQ_LINES: usize = platform_constants::MAX_IRQ_LINES;
#[cfg(feature = "hosted-dev")]
const MAX_DRIVERS: usize = 64;
#[cfg(not(feature = "hosted-dev"))]
const MAX_DRIVERS: usize = 32;

#[cfg(feature = "hosted-dev")]
const MAX_DRIVER_IRQ_CAPS: usize = 16;
#[cfg(not(feature = "hosted-dev"))]
const MAX_DRIVER_IRQ_CAPS: usize = 8;

#[cfg(feature = "hosted-dev")]
const MAX_DRIVER_DMA_CAPS: usize = 16;
#[cfg(not(feature = "hosted-dev"))]
const MAX_DRIVER_DMA_CAPS: usize = 8;

#[cfg(feature = "hosted-dev")]
const MAX_TRANSFER_ENVELOPES: usize = 256;
#[cfg(not(feature = "hosted-dev"))]
const MAX_TRANSFER_ENVELOPES: usize = 64;
const MAX_REPLY_CAPS: usize = MAX_TASKS;
#[cfg(feature = "hosted-dev")]
const MAX_DELEGATED_CAPABILITY_LINKS: usize = 4096;
#[cfg(not(feature = "hosted-dev"))]
const MAX_DELEGATED_CAPABILITY_LINKS: usize = 2048;
const INITIAL_DYNAMIC_TID: u64 = 10_000;
const STATIC_TID_UPPER_BOUND: u64 = INITIAL_DYNAMIC_TID - 1;

pub(crate) use defs::*;
pub use types::*;

#[derive(Debug)]
pub struct KernelState {
    // Lock ordering is documented in doc/KERNEL_LOCKING.md.
    // Any new SpinLockIrq field or multi-lock path must update that document.
    pub kernel_aspace: AddressSpace,
    hal: crate::arch::hal::SelectedIsaHal,
    pub user_spaces: KernelStorage<AddressSpaceManager>,
    scheduler_state: SpinLockIrq<SchedulerState>,
    ipc_state_lock: SpinLockIrq<()>,
    driver_state_lock: SpinLockIrq<()>,
    fault_state_lock: SpinLockIrq<()>,
    restart_state_lock: SpinLockIrq<()>,
    capability_state_lock: SpinLockIrq<()>,
    telemetry_state_lock: SpinLockIrq<()>,
    boot_config_state_lock: SpinLockIrq<()>,
    vm_state_lock: SpinLockIrq<()>,
    task_state_lock: SpinLockIrq<()>,
    memory_state_lock: SpinLockIrq<()>,
    ipc: KernelStorage<IpcSubsystem>,
    capability: CapabilitySubsystem,
    tid_allocation_policy: TidAllocationPolicy,
    tid_allocation_cursor: TidAllocationCursor,
    tcbs: KernelStorage<[Option<ThreadControlBlock>; MAX_TASKS]>,
    task_classes: KernelStorage<[Option<TaskClass>; MAX_TASKS]>,
    tls_restore_pending: KernelStorage<[Option<ThreadId>; MAX_TASKS]>,
    robust_futex: KernelStorage<[Option<RobustFutexRecord>; MAX_TASKS]>,
    memory: KernelStorage<MemorySubsystem>,
    drivers: KernelStorage<DriverSubsystem>,
    telemetry: KernelStorage<TelemetrySubsystem>,
    boot_config: KernelStorage<BootConfigSubsystem>,
    faults: KernelStorage<FaultSubsystem>,
    restart: KernelStorage<RestartSubsystem>,
}

pub struct Bootstrap;

#[cfg(test)]
mod tests;
