// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

mod bootstrap_state;
mod cap_memory_mint_split;
mod cap_transfer_delegation_split;
mod cap_transfer_materialize_split;
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
mod reply_cap_rank_split;
mod restart_state;
mod scheduler_state;
pub(crate) mod shared_region_txn;
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
    /// Stage 4F pre-screen: found a plain receiver waiter with this COMPLETE identity (tid + ASID,
    /// Stage 198E3B2B2) and no sender waiters. The identity came from a locked ipc_state_lock read in
    /// ipc_try_send_queued_plain_endpoint_only. Caller checks is_task_recv_v2_blocked then calls
    /// ipc_try_send_to_plain_receiver_endpoint_only, which re-verifies the FULL identity (never
    /// numeric TID alone) before clearing the waiter slot.
    ReceiverWaiterFound(ReceiverWaiterIdentity),
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

/// Stage 188A: per-CPU stash cell for a [`crate::kernel::dispatch_post_work::DispatchPostWork`]
/// item that a syscall/IPC handler produced under the broad `with_cpu` /
/// `&mut KernelState` borrow, to be drained and executed by runtime AFTER the
/// borrow is dropped. Mirrors [`PerCpuSwitchPlanStash`] exactly.
///
/// # Safety
///
/// Accessed only from the trap path on the local CPU with interrupts disabled
/// (same discipline as `PerCpuSwitchPlanStash`). No cross-CPU sharing; at most
/// one item stashed per CPU per trap.
pub(crate) struct PerCpuDispatchPostWorkStash {
    inner: core::cell::UnsafeCell<Option<crate::kernel::dispatch_post_work::DispatchPostWork>>,
}

// SAFETY: Accessed only from the local CPU's trap path with interrupts disabled.
unsafe impl Sync for PerCpuDispatchPostWorkStash {}

impl PerCpuDispatchPostWorkStash {
    pub(crate) const fn new() -> Self {
        Self {
            inner: core::cell::UnsafeCell::new(None),
        }
    }

    /// Store post-work in the stash.
    ///
    /// # Safety
    /// Caller must ensure no concurrent access (interrupts disabled, single CPU).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) unsafe fn store(&self, work: crate::kernel::dispatch_post_work::DispatchPostWork) {
        unsafe { *self.inner.get() = Some(work) }
    }

    /// Take the post-work from the stash (consumes it), leaving the slot empty.
    ///
    /// # Safety
    /// Caller must ensure no concurrent access (interrupts disabled, single CPU).
    pub(crate) unsafe fn take(
        &self,
    ) -> Option<crate::kernel::dispatch_post_work::DispatchPostWork> {
        unsafe { (*self.inner.get()).take() }
    }
}

/// Per-CPU dispatch-return work stash (Stage 188A). Index by `CpuId.0`. Accessed
/// only from the trap path on the local CPU with interrupts disabled. Empty on
/// every production trap in Stage 188A (no live producer) → drain is a no-op.
///
/// VALIDATION: DISPATCH_RETURN_CHANNEL (helper-only in Stage 188A)
pub(crate) static DISPATCH_POST_WORK_STASH: [PerCpuDispatchPostWorkStash;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { PerCpuDispatchPostWorkStash::new() }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 188A one-shot flag: emit `DISPATCH_RETURN_CHANNEL_READY mode=helper_only`
/// exactly once (first post-`with_cpu` drain) as honest boot-log evidence the
/// channel is present and inert.
pub(crate) static DISPATCH_RETURN_CHANNEL_READY_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

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
///
/// Stage 182 (REMOVE-FALLBACKS): the graduated D6 seam is now the production path on
/// x86_64 `-smp 1` and is no longer runtime-toggleable — the `yarm.d6_genuine` /
/// `yarm.unlock_graduated` knobs and their `AtomicBool`/setter plumbing were deleted
/// (not hard-disabled). This is a compile-time constant reproducing the accepted
/// enabling condition exactly: graduated on x86_64 UNLESS a D6-switch diagnostic
/// (`d6_switch_proof` / `d6_switch_a`, category-D debug knobs) owns the switch path.
/// On AArch64/RISC-V it is compile-time `false` (in-lock path only — Stage 184), and
/// the runtime `single_cpu` eligibility guard keeps SMP>1 on the in-lock path
/// (Stage 183). There is NO production opt-out back to the old global-lock path.
pub(crate) fn d6_genuine_enabled() -> bool {
    cfg!(target_arch = "x86_64") && !d6_controlled_switch_proof_enabled() && !d6_switch_a_enabled()
}

/// Stage 167: per-CPU count of genuine scheduler-seam dispatch observations.
pub(crate) static D6_GENUINE_SEAM_COUNT: [core::sync::atomic::AtomicU64;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; crate::kernel::scheduler::MAX_CPUS];

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
///
/// Stage 182 (REMOVE-FALLBACKS): compile-time production gate (see
/// [`d6_genuine_enabled`]). The `yarm.d2_recv_genuine` knob + `AtomicBool`/setter were
/// deleted; the graduated blocking-recv seam is the only x86_64 `-smp 1` path, with no
/// runtime opt-out to the old in-lock production path.
pub(crate) fn d2_recv_genuine_enabled() -> bool {
    d6_genuine_enabled()
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

// ── Stage 192A (QUEUE-ADVANCING OUT-OF-LOCK DISPATCH for FutexWait) ─────────────────
//
// FutexWait's blocking wait is structurally identical to blocking IPC recv/send: the
// in-lock path publishes `Blocked(Futex(addr))` + `block_current` (removes the caller
// from `current`), then DEFERS the queue-advancing dispatch out of the global lock to the
// trap-entry drain — exactly the Stage 168B/169 D2-GENUINE recv/send model (default-on on
// x86_64 single-dispatcher). Same per-CPU deferral discipline: one intent at a time; the
// outgoing (blocked) TID is recorded so the drain re-verifies `Blocked(Futex)` before the
// out-of-lock `dispatch_next_on`.

/// Stage 192A: global count of FutexWait queue-advancing dispatches run through the
/// scheduler seam OUTSIDE the global lock. Emitted as `FUTEX_WAIT_SPLIT_DISPATCH_OK`.
pub(crate) static FUTEX_WAIT_DISPATCH_COUNT: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Stage 192A: per-CPU "FutexWait dispatch deferred" flag. Set by the in-lock
/// `futex_wait_current` when it commits the block and defers the queue-advancing dispatch;
/// cleared by the trap-entry drain.
pub(crate) static FUTEX_WAIT_DISPATCH_DEFERRED: [core::sync::atomic::AtomicBool;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 192A: per-CPU blocked (outgoing) FutexWait TID recorded with the deferral, so the
/// drain can re-verify the task is still `Blocked(Futex)` before dispatching (`u64::MAX`
/// sentinel = unset).
pub(crate) static FUTEX_WAIT_DISPATCH_OUTGOING: [core::sync::atomic::AtomicU64;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(u64::MAX) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 192A: record a deferred FutexWait dispatch intent for `cpu`. Returns false
/// (decline; caller falls back to the in-lock dispatch) if an intent is already pending.
pub(crate) fn futex_wait_dispatch_try_defer(cpu_idx: usize, outgoing: u64) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    if FUTEX_WAIT_DISPATCH_DEFERRED[cpu_idx]
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
    FUTEX_WAIT_DISPATCH_OUTGOING[cpu_idx].store(outgoing, core::sync::atomic::Ordering::Release);
    true
}

/// Stage 192A: is a deferred FutexWait dispatch pending for `cpu`?
pub(crate) fn futex_wait_dispatch_is_deferred(cpu_idx: usize) -> bool {
    cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && FUTEX_WAIT_DISPATCH_DEFERRED[cpu_idx].load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 192A: read the deferred FutexWait outgoing TID for `cpu` (`None` if unset).
pub(crate) fn futex_wait_dispatch_outgoing(cpu_idx: usize) -> Option<u64> {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    let v = FUTEX_WAIT_DISPATCH_OUTGOING[cpu_idx].load(core::sync::atomic::Ordering::Acquire);
    if v == u64::MAX { None } else { Some(v) }
}

/// Stage 192A: clear the FutexWait dispatch deferral for `cpu`.
pub(crate) fn futex_wait_dispatch_clear(cpu_idx: usize) {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return;
    }
    FUTEX_WAIT_DISPATCH_OUTGOING[cpu_idx].store(u64::MAX, core::sync::atomic::Ordering::Release);
    FUTEX_WAIT_DISPATCH_DEFERRED[cpu_idx].store(false, core::sync::atomic::Ordering::Release);
}

/// Stage 192A: one-shot latch for the FutexWait retirement markers (queue-advancing
/// dispatch now runs off the global lock; the block-publish stays in-lock, mirroring the
/// accepted D2-GENUINE recv/send out-of-lock dispatch model).
static FUTEX_WAIT_RETIRE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 195F: one-shot latch for the AArch64 FutexWait default-on attestation.
static FUTEX_WAIT_DEFAULT_ON_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 195F: emit `AARCH64_FUTEX_WAIT_RETIRE_DEFAULT_ON` exactly once, at the first eligible
/// AArch64 FutexWait deferral — proving the out-of-lock retirement mechanism is the default
/// production path (no oracle/enable knob required).
pub(crate) fn maybe_log_futex_wait_default_on() {
    if FUTEX_WAIT_DEFAULT_ON_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        crate::yarm_log!("AARCH64_FUTEX_WAIT_RETIRE_DEFAULT_ON result=ok");
    }
}

/// Stage 192A: emit the FutexWait retirement markers exactly once (first off-global-lock
/// queue-advancing dispatch).
pub(crate) fn maybe_log_futex_wait_retired() {
    if FUTEX_WAIT_RETIRE_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        // Stage 197 (FIRST-COHORT SEAL): all architectures emit the canonical arch-tagged
        // retirement marker `arch=<arch> class=FutexWait`. (This helper is called only by the
        // x86_64 + AArch64 drains in `arch/trap_entry.rs`; the RISC-V drain emits its own
        // `arch=riscv64` markers inline.)
        #[cfg(target_arch = "aarch64")]
        {
            crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=FutexWait");
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=FutexWait result=ok"
            );
        }
        #[cfg(target_arch = "x86_64")]
        {
            crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=x86_64 class=FutexWait");
            crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=FutexWait result=ok");
        }
    }
}

// ── Stage 192B (QUEUE-ADVANCING OUT-OF-LOCK DISPATCH for Yield) ─────────────────────
//
// Yield is the preempt sibling of FutexWait: instead of blocking the caller, it
// RE-ENQUEUES the caller as Runnable then dispatches the next task. The in-lock path sets
// the caller Runnable + re-enqueues it + clears `current` (the re-enqueue half of
// on_preempt), records a per-CPU deferral, and declines the in-lock dispatch; the
// trap-entry drain runs the authoritative `dispatch_next_on` out of the global lock. Same
// per-CPU deferral discipline as the Stage 168B/192A models.

/// Stage 192B: global count of Yield queue-advancing dispatches run through the scheduler
/// seam OUTSIDE the global lock. Emitted as `YIELD_DISPATCH_DONE`.
pub(crate) static YIELD_DISPATCH_COUNT: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Stage 192B: per-CPU "Yield dispatch deferred" flag.
pub(crate) static YIELD_DISPATCH_DEFERRED: [core::sync::atomic::AtomicBool;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 192B: per-CPU re-enqueued (outgoing) Yield TID recorded with the deferral
/// (`u64::MAX` sentinel = unset).
pub(crate) static YIELD_DISPATCH_OUTGOING: [core::sync::atomic::AtomicU64;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(u64::MAX) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 192B: record a deferred Yield dispatch intent for `cpu`. Returns false (decline;
/// caller falls back to the in-lock dispatch) if an intent is already pending.
pub(crate) fn yield_dispatch_try_defer(cpu_idx: usize, outgoing: u64) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    if YIELD_DISPATCH_DEFERRED[cpu_idx]
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
    YIELD_DISPATCH_OUTGOING[cpu_idx].store(outgoing, core::sync::atomic::Ordering::Release);
    true
}

/// Stage 192B: is a deferred Yield dispatch pending for `cpu`?
pub(crate) fn yield_dispatch_is_deferred(cpu_idx: usize) -> bool {
    cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && YIELD_DISPATCH_DEFERRED[cpu_idx].load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 192B: read the deferred Yield outgoing TID for `cpu` (`None` if unset).
pub(crate) fn yield_dispatch_outgoing(cpu_idx: usize) -> Option<u64> {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    let v = YIELD_DISPATCH_OUTGOING[cpu_idx].load(core::sync::atomic::Ordering::Acquire);
    if v == u64::MAX { None } else { Some(v) }
}

/// Stage 192B: clear the Yield dispatch deferral for `cpu`.
pub(crate) fn yield_dispatch_clear(cpu_idx: usize) {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return;
    }
    YIELD_DISPATCH_OUTGOING[cpu_idx].store(u64::MAX, core::sync::atomic::Ordering::Release);
    YIELD_DISPATCH_DEFERRED[cpu_idx].store(false, core::sync::atomic::Ordering::Release);
}

// ─── Stage 196D: RISC-V queue-advancing context-switch FOUNDATION deferral ───
// A SEPARATE, default-off, one-shot deferral used ONLY by the RISC-V queue-switch
// foundation oracle. It is deliberately distinct from `YIELD_DISPATCH_*` so it can
// NEVER be confused with (or accidentally enable) Yield retirement: it emits only
// RISCV_QUEUE_SWITCH_FOUNDATION_* markers and is gated by its own default-off knob.
pub(crate) static RISCV_QUEUE_SWITCH_FOUNDATION_DEFERRED: [core::sync::atomic::AtomicBool;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; crate::kernel::scheduler::MAX_CPUS];
pub(crate) static RISCV_QUEUE_SWITCH_FOUNDATION_OUTGOING: [core::sync::atomic::AtomicU64;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(u64::MAX) }; crate::kernel::scheduler::MAX_CPUS];
/// One-shot latch: the foundation switch fires exactly once per boot (the oracle needs
/// a single proven switch; every later yield takes the unchanged legacy path).
static RISCV_QUEUE_SWITCH_FOUNDATION_DONE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Default-off selector (`yarm.riscv64_queue_switch_foundation_oracle=1`). Arms the
/// one-shot RISC-V post-lock context-switch foundation (publish/re-enqueue outgoing,
/// clear current, defer the dispatch; post-lock drain switches to the incoming task
/// with a real SATP/sfence.vma + frame restore + sret). Enables NO syscall retirement.
pub(crate) static RISCV_QUEUE_SWITCH_FOUNDATION_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_riscv_queue_switch_foundation_oracle_enabled(enabled: bool) {
    RISCV_QUEUE_SWITCH_FOUNDATION_ORACLE_ENABLED
        .store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn riscv_queue_switch_foundation_oracle_enabled() -> bool {
    RISCV_QUEUE_SWITCH_FOUNDATION_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// True only while the one-shot foundation switch has not yet fired (armed by the knob).
pub(crate) fn riscv_queue_switch_foundation_armed() -> bool {
    riscv_queue_switch_foundation_oracle_enabled()
        && !RISCV_QUEUE_SWITCH_FOUNDATION_DONE.load(core::sync::atomic::Ordering::Acquire)
}

/// Record the one-shot foundation switch deferral for `cpu`. Returns false (decline;
/// caller keeps the legacy path) if one is already pending OR the one-shot already fired.
pub(crate) fn riscv_queue_switch_foundation_try_defer(cpu_idx: usize, outgoing: u64) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    // Claim the one-shot first so a second yield can never re-arm the foundation.
    if RISCV_QUEUE_SWITCH_FOUNDATION_DONE
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
    if RISCV_QUEUE_SWITCH_FOUNDATION_DEFERRED[cpu_idx]
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
    RISCV_QUEUE_SWITCH_FOUNDATION_OUTGOING[cpu_idx]
        .store(outgoing, core::sync::atomic::Ordering::Release);
    true
}

pub(crate) fn riscv_queue_switch_foundation_is_deferred(cpu_idx: usize) -> bool {
    cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && RISCV_QUEUE_SWITCH_FOUNDATION_DEFERRED[cpu_idx]
            .load(core::sync::atomic::Ordering::Acquire)
}

pub(crate) fn riscv_queue_switch_foundation_outgoing(cpu_idx: usize) -> Option<u64> {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    let v =
        RISCV_QUEUE_SWITCH_FOUNDATION_OUTGOING[cpu_idx].load(core::sync::atomic::Ordering::Acquire);
    if v == u64::MAX { None } else { Some(v) }
}

/// Clear the per-CPU deferral (does NOT reset the one-shot DONE latch — the foundation
/// fires exactly once per boot).
pub(crate) fn riscv_queue_switch_foundation_clear(cpu_idx: usize) {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return;
    }
    RISCV_QUEUE_SWITCH_FOUNDATION_OUTGOING[cpu_idx]
        .store(u64::MAX, core::sync::atomic::Ordering::Release);
    RISCV_QUEUE_SWITCH_FOUNDATION_DEFERRED[cpu_idx]
        .store(false, core::sync::atomic::Ordering::Release);
}

// ── Stage 196E/196F: RISC-V FutexWait queue-advancing retirement ──
//
// As of Stage 196F the retirement MECHANISM is DEFAULT-ON for eligible RISC-V traps: there is NO
// oracle knob and NO one-shot consume latch in the kernel eligibility path (both removed). The
// generic per-CPU `FUTEX_WAIT_DISPATCH_*` deferral state drives the in-lock publish + post-lock
// drain. Two userspace WORKLOAD knobs remain default-off (they create the two-task switch scenario
// / the last-task idle scenario; they do NOT arm kernel retirement):
//   * `yarm.riscv64_futex_wait_oracle`      → switch oracle workload (slot-5 = 3)
//   * `yarm.riscv64_futex_wait_idle_oracle` → no-incoming idle oracle workload (slot-5 = 4)

/// Default-off SWITCH-oracle WORKLOAD selector (`yarm.riscv64_futex_wait_oracle=1`). Provisions
/// init slot 5 (=3) so init runs the two-task FutexWait switch workload. Does NOT arm kernel
/// retirement (that is default-on) — only creates the workload.
pub(crate) static RISCV_FUTEX_WAIT_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Default-off IDLE-oracle WORKLOAD selector (`yarm.riscv64_futex_wait_idle_oracle=1`). Provisions
/// init slot 5 (=4) so init (the last runnable user task) blocks on a never-woken futex, driving
/// the production default-on drain to its post-lock IDLE outcome. Also gates the kernel-side
/// idle-oracle attestation marker.
pub(crate) static RISCV_FUTEX_WAIT_IDLE_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_riscv_futex_wait_oracle_enabled(enabled: bool) {
    RISCV_FUTEX_WAIT_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn riscv_futex_wait_oracle_enabled() -> bool {
    RISCV_FUTEX_WAIT_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

pub(crate) fn set_riscv_futex_wait_idle_oracle_enabled(enabled: bool) {
    RISCV_FUTEX_WAIT_IDLE_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn riscv_futex_wait_idle_oracle_enabled() -> bool {
    RISCV_FUTEX_WAIT_IDLE_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 196F: one-shot latch for the DEFAULT-ON informational marker. Records that the
/// production (default-on) FutexWait retirement mechanism was exercised — NOT that an oracle knob
/// was enabled.
static RISCV_FUTEX_WAIT_DEFAULT_ON_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Emit `RISCV_FUTEX_WAIT_RETIRE_DEFAULT_ON result=ok` exactly once, on the first eligible
/// production FutexWait retirement.
pub(crate) fn maybe_log_riscv_futex_wait_retire_default_on() {
    if RISCV_FUTEX_WAIT_DEFAULT_ON_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        crate::yarm_log!("RISCV_FUTEX_WAIT_RETIRE_DEFAULT_ON result=ok");
    }
}

/// Stage 192B: one-shot latch for the Yield retirement markers.
static YIELD_RETIRE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 192B: emit the Yield retirement markers exactly once.
pub(crate) fn maybe_log_yield_retired() {
    if YIELD_RETIRE_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        // Stage 197 (FIRST-COHORT SEAL): all architectures emit the canonical arch-tagged
        // retirement marker `arch=<arch> class=Yield`. (This helper is called only by the x86_64 +
        // AArch64 drains in `arch/trap_entry.rs`; the RISC-V drain emits its own `arch=riscv64`
        // markers inline.)
        #[cfg(target_arch = "aarch64")]
        {
            crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=Yield");
            crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=Yield result=ok");
        }
        #[cfg(target_arch = "x86_64")]
        {
            crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=x86_64 class=Yield");
            crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=Yield result=ok");
        }
    }
}

/// Stage 195G: one-shot latch for the AArch64 Yield default-on attestation.
static YIELD_DEFAULT_ON_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 195G: emit `AARCH64_YIELD_RETIRE_DEFAULT_ON` exactly once, at the first eligible
/// AArch64 Yield deferral — proving the out-of-lock retirement mechanism is the default
/// production path (no oracle/enable knob required).
pub(crate) fn maybe_log_yield_default_on() {
    if YIELD_DEFAULT_ON_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        crate::yarm_log!("AARCH64_YIELD_RETIRE_DEFAULT_ON result=ok");
    }
}

// ── Stage 196G: RISC-V Yield (NR 0) DEFAULT-ON out-of-lock retirement ──
//
// Production Yield reuses the generic per-CPU `YIELD_DISPATCH_*` deferral + `preempt_reenqueue`
// re-enqueue seam + `yield_dispatch_step_mut` dequeue — the SAME seams x86_64 (192B) / AArch64
// (195G) use — plus the 196D–196F RISC-V SATP/sfence/frame switch machinery. It is DEFAULT-ON for
// eligible NR 0 traps (no oracle knob, no consume latch); the 196D foundation oracle stays a
// SEPARATE default-off mechanism. Two userspace WORKLOAD knobs (two-task + lone-task) stay
// default-off.

/// Stage 196G: one-shot latch for the RISC-V Yield default-on informational marker. The
/// mechanism itself is NOT one-shot (it retires every eligible Yield); only this attestation fires
/// once.
static RISCV_YIELD_DEFAULT_ON_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Emit `RISCV_YIELD_RETIRE_DEFAULT_ON result=ok` exactly once, on the first eligible production
/// Yield retirement. Records that the production (default-on) mechanism ran — NOT a knob.
pub(crate) fn maybe_log_riscv_yield_retire_default_on() {
    if RISCV_YIELD_DEFAULT_ON_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        crate::yarm_log!("RISCV_YIELD_RETIRE_DEFAULT_ON result=ok");
    }
}

/// Default-off two-task Yield oracle WORKLOAD selector (`yarm.riscv64_yield_two_task_oracle=1`,
/// slot-5 = 5). Does NOT arm kernel retirement (default-on) — only creates the workload.
pub(crate) static RISCV_YIELD_TWO_TASK_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
/// Default-off lone-task Yield oracle WORKLOAD selector (`yarm.riscv64_yield_lone_task_oracle=1`,
/// slot-5 = 6).
pub(crate) static RISCV_YIELD_LONE_TASK_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_riscv_yield_two_task_oracle_enabled(enabled: bool) {
    RISCV_YIELD_TWO_TASK_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}
pub fn riscv_yield_two_task_oracle_enabled() -> bool {
    RISCV_YIELD_TWO_TASK_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}
pub(crate) fn set_riscv_yield_lone_task_oracle_enabled(enabled: bool) {
    RISCV_YIELD_LONE_TASK_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}
pub fn riscv_yield_lone_task_oracle_enabled() -> bool {
    RISCV_YIELD_LONE_TASK_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

// ── Stage 193A (BROAD-IPC DECOMPOSITION — IpcSend plain waiting-receiver slice) ─────
//
// IpcSend of a PLAIN message to an already-recv-v2-blocked receiver reuses the 188
// dispatch-return channel (the same producer + drain `ipc_reply` uses): Phase A snapshots
// the payload/meta by value under the broad borrow (NO user copy, NO cap materialization),
// and the trap-entry drain does Phase B (user copy + slot-clear + wake) AFTER the broad
// borrow drops. This per-CPU flag tags the stashed plain delivery as originating from
// `ipc_send` so the drain can emit the IpcSend-specific boundary markers (the plain snapshot
// arm is shared with `ipc_reply`, which leaves the flag unset).

/// Stage 193A: per-CPU "the pending plain delivery originated from ipc_send" flag.
pub(crate) static IPC_SEND_BOUNDARY_ORIGIN: [core::sync::atomic::AtomicBool;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 193A: tag the just-stashed plain delivery on `cpu` as an ipc_send boundary split.
pub(crate) fn ipc_send_boundary_origin_set(cpu_idx: usize) {
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        IPC_SEND_BOUNDARY_ORIGIN[cpu_idx].store(true, core::sync::atomic::Ordering::Release);
    }
}

/// Stage 193A: is the pending plain delivery on `cpu` an ipc_send boundary split? (peek)
pub(crate) fn ipc_send_boundary_origin_is_set(cpu_idx: usize) -> bool {
    cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && IPC_SEND_BOUNDARY_ORIGIN[cpu_idx].load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 193A: consume the ipc_send boundary origin flag for `cpu` (clear + return prior).
pub(crate) fn ipc_send_boundary_origin_take(cpu_idx: usize) -> bool {
    cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && IPC_SEND_BOUNDARY_ORIGIN[cpu_idx].swap(false, core::sync::atomic::Ordering::AcqRel)
}

// ── Stage 198A1 / 198B: authoritative blocking-syscall idle provenance ────────────────────
//
// The RISC-V trap wrapper must NOT infer intentional idle from scheduler state alone
// (`Ok` result + `current == None` + zero runnable). Instead, the canonical blocking path
// (`handle_trap_event`'s `blocking_syscall && caller_blocked` branch — IpcRecv / IpcCall /
// IpcSend) publishes an AUTHORITATIVE per-CPU token recording the tid it just blocked and
// dispatched away from, plus the exact blocking syscall CLASS. The wrapper CONSUMES that token:
// token present + terminal scheduler state → typed `EnterKernelIdle { BlockedIpcNoRunnable }`;
// terminal state WITHOUT the token is a bug and takes a defensive error path, never silent idle.
// x86_64 / AArch64 set the token too (arch-neutral seam) but never read it — they own their own
// idle bridges. (Stage 198B generalized the reason name from the recv-only `BlockedRecvNoRunnable`
// to `BlockedIpcNoRunnable` + a separately recorded blocking class, since the producer covers
// IpcRecv / IpcCall / IpcSend, not recv alone.)

/// Stage 198B: the authoritative blocking syscall class that produced idle provenance. Recorded
/// separately from the reason so `RiscvIdleReason` stays a plain (non-payload) enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockingSyscallClass {
    IpcRecv,
    IpcCall,
    IpcSend,
}

impl BlockingSyscallClass {
    pub fn as_str(self) -> &'static str {
        match self {
            BlockingSyscallClass::IpcRecv => "IpcRecv",
            BlockingSyscallClass::IpcCall => "IpcCall",
            BlockingSyscallClass::IpcSend => "IpcSend",
        }
    }
    fn to_code(self) -> u8 {
        match self {
            BlockingSyscallClass::IpcRecv => 1,
            BlockingSyscallClass::IpcCall => 2,
            BlockingSyscallClass::IpcSend => 3,
        }
    }
    fn from_code(code: u8) -> Option<BlockingSyscallClass> {
        match code {
            1 => Some(BlockingSyscallClass::IpcRecv),
            2 => Some(BlockingSyscallClass::IpcCall),
            3 => Some(BlockingSyscallClass::IpcSend),
            _ => None,
        }
    }
}

/// Stage 198A1: per-CPU "a canonical blocking syscall blocked+dispatched-away this trap" token.
/// Stores `tid + 1` (0 = unset); consumed once by the RISC-V trap-entry wrapper.
pub(crate) static BLOCKED_SYSCALL_IDLE_PROVENANCE: [core::sync::atomic::AtomicU64;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 198B: per-CPU blocking-syscall class code paired with the provenance token above (only
/// meaningful while the token is set; single-CPU IRQ-off trap path, so the pair is consistent).
static BLOCKED_SYSCALL_IDLE_CLASS: [core::sync::atomic::AtomicU8;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU8::new(0) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 198A1/198B: publish authoritative idle provenance — a canonical blocking syscall of
/// `class` blocked `tid` and dispatched away from it on `cpu`. Called from the arch-neutral seam.
pub(crate) fn blocked_syscall_idle_provenance_set(
    cpu_idx: usize,
    tid: u64,
    class: BlockingSyscallClass,
) {
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        BLOCKED_SYSCALL_IDLE_CLASS[cpu_idx]
            .store(class.to_code(), core::sync::atomic::Ordering::Release);
        BLOCKED_SYSCALL_IDLE_PROVENANCE[cpu_idx]
            .store(tid.wrapping_add(1), core::sync::atomic::Ordering::Release);
    }
}

/// Stage 198A1/198B: consume the blocking-syscall idle provenance for `cpu` (clear + return the
/// `(tid, class)`, or `None` if no canonical blocking syscall published provenance this trap).
pub(crate) fn blocked_syscall_idle_provenance_take(
    cpu_idx: usize,
) -> Option<(u64, BlockingSyscallClass)> {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    let raw =
        BLOCKED_SYSCALL_IDLE_PROVENANCE[cpu_idx].swap(0, core::sync::atomic::Ordering::AcqRel);
    if raw == 0 {
        return None;
    }
    let code = BLOCKED_SYSCALL_IDLE_CLASS[cpu_idx].swap(0, core::sync::atomic::Ordering::AcqRel);
    // A set token always co-recorded a valid class; default to IpcRecv only if somehow absent.
    let class = BlockingSyscallClass::from_code(code).unwrap_or(BlockingSyscallClass::IpcRecv);
    Some((raw - 1, class))
}

// ── Stage 198E3 (SHARED-REGION LIVE) ──────────────────────────────────────────────────────
//
// The accepted post-lock shared-region transaction (`shared_region_execute`) is wired LIVE into
// the direct (blocked-receiver) and queued (dequeue) receive boundaries, gated behind the
// oracle-proof knob (`ipc_recv_oracle_proof_enabled`) so it is INERT on a normal boot (the legacy
// path runs and no shared-region live class markers are emitted). A per-CPU origin tag records the
// pending shared-region post-work's CLASS (direct vs enqueue) so the drain executor emits the
// class-correct attestations + retirement markers ONLY from a real post-lock completion — never
// from ordinary-cap, reply-cap, plain, hosted-test, or fallback paths.

/// Stage 198E3: the running architecture tag for runtime markers (arch-neutral drain executor).
pub(crate) const fn current_arch_tag() -> &'static str {
    #[cfg(target_arch = "aarch64")]
    {
        "aarch64"
    }
    #[cfg(target_arch = "x86_64")]
    {
        "x86_64"
    }
    #[cfg(target_arch = "riscv64")]
    {
        "riscv64"
    }
    #[cfg(not(any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "riscv64"
    )))]
    {
        "host"
    }
}

/// Stage 198E3: per-CPU shared-region live post-work origin (0 = none, 1 = direct, 2 = enqueue).
pub(crate) static SHARED_REGION_LIVE_ORIGIN: [core::sync::atomic::AtomicU8;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU8::new(0) }; crate::kernel::scheduler::MAX_CPUS];

/// Origin class for a pending shared-region live delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SharedRegionLiveOrigin {
    Direct,
    Enqueue,
}

/// Stage 198E3: tag the just-stashed shared-region delivery on `cpu` with its class origin.
pub(crate) fn shared_region_live_origin_set(cpu_idx: usize, origin: SharedRegionLiveOrigin) {
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        let code = match origin {
            SharedRegionLiveOrigin::Direct => 1,
            SharedRegionLiveOrigin::Enqueue => 2,
        };
        SHARED_REGION_LIVE_ORIGIN[cpu_idx].store(code, core::sync::atomic::Ordering::Release);
    }
}

/// Stage 198E3: consume the shared-region live origin for `cpu` (clear + return prior class).
pub(crate) fn shared_region_live_origin_take(cpu_idx: usize) -> Option<SharedRegionLiveOrigin> {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    match SHARED_REGION_LIVE_ORIGIN[cpu_idx].swap(0, core::sync::atomic::Ordering::AcqRel) {
        1 => Some(SharedRegionLiveOrigin::Direct),
        2 => Some(SharedRegionLiveOrigin::Enqueue),
        _ => None,
    }
}

/// Stage 198E3: one-shot latch for the fail-closed cancellation-fuse diagnostic.
static SHARED_REGION_CANCEL_FUSE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 198E3: emit the fail-closed cancellation-fuse diagnostic EXACTLY ONCE, when the fuse
/// transitions clear → set (a cancellation request that could not be recorded). The fuse is never
/// auto-cleared; a normal live oracle run must show `count=0` of this marker. Returns `true` iff
/// this call actually emitted (won the one-shot latch).
pub(crate) fn maybe_log_shared_region_cancel_fuse_set() -> bool {
    if SHARED_REGION_CANCEL_FUSE_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        crate::yarm_log!(
            "SHARED_REGION_CANCEL_FUSE_SET reason=capacity_exhausted result=fail_closed"
        );
        true
    } else {
        false
    }
}

/// Test-only: reset the one-shot fuse-diagnostic latch so the "emitted exactly once" contract can
/// be exercised deterministically without cross-test contamination from the shared static.
#[cfg(test)]
pub(crate) fn reset_shared_region_cancel_fuse_log() {
    SHARED_REGION_CANCEL_FUSE_LOGGED.store(false, core::sync::atomic::Ordering::Release);
}

/// Stage 198E3C1: one-shot latch for the `IpcSendSharedRegionDirect` retirement markers. Compiled
/// ONLY for the explicitly-armed x86_64 live-oracle build (`feature = "x86-shared-region-direct-
/// oracle"`); a normal artifact and every non-x86 artifact contain none of these literals.
#[cfg(all(feature = "x86-shared-region-direct-oracle", target_arch = "x86_64"))]
static IPC_SEND_SHARED_REGION_DIRECT_RETIRE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 198E3C1: emit the `IpcSendSharedRegionDirect` retirement markers exactly once — only from
/// the x86_64 armed-oracle build, only after a real off-lock direct completion (called solely by the
/// gated live-marker helper). The queued class and all non-x86 architectures never compile a
/// shared-region retirement literal.
#[cfg(all(feature = "x86-shared-region-direct-oracle", target_arch = "x86_64"))]
pub(crate) fn maybe_log_ipc_send_shared_region_direct_retired() {
    if IPC_SEND_SHARED_REGION_DIRECT_RETIRE_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        crate::yarm_log!(
            "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=x86_64 class=IpcSendSharedRegionDirect"
        );
        crate::yarm_log!(
            "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcSendSharedRegionDirect result=ok"
        );
    }
}

/// Stage 198E3C1: origin-gated shared-region LIVE markers (attestations + retirement), emitted ONLY
/// from the drain's success arm AFTER the transaction finalized, the waiter was cleared, and the
/// receiver enqueued exactly once. Every marker LITERAL is confined here and gated on the x86_64
/// armed-oracle build, so a NORMAL artifact — and every non-x86 artifact — is marker-clean. Only the
/// DIRECT class emits (the queued class is forbidden this stage); a non-Direct origin is a no-op.
#[cfg(all(feature = "x86-shared-region-direct-oracle", target_arch = "x86_64"))]
pub(crate) fn maybe_emit_shared_region_direct_live_markers(
    class: &str,
    snapshot: &crate::kernel::boot::shared_region_txn::RecvBoundarySharedRegionSnapshot,
    woke_receiver: bool,
    origin: Option<SharedRegionLiveOrigin>,
) {
    use crate::kernel::capabilities::{CapObject, CapRights};
    if !matches!(origin, Some(SharedRegionLiveOrigin::Direct)) {
        return;
    }
    let object_match = u8::from(matches!(
        snapshot.object,
        CapObject::MemoryObject { .. } | CapObject::DmaRegion { .. }
    ));
    crate::yarm_log!(
        "IPCSEND_SHARED_REGION_OBJECT_OK arch=x86_64 class={} object_match={} fresh_cap=1 pin_transfer=1",
        class,
        object_match
    );
    let map_right = u8::from(snapshot.rights.contains(CapRights::MAP));
    let write_ok = u8::from(!snapshot.map_write || snapshot.rights.contains(CapRights::WRITE));
    crate::yarm_log!(
        "IPCSEND_SHARED_REGION_MAP_OK arch=x86_64 class={} map_right={} write_right_ok={} nx=1 cleanup_token=1",
        class,
        map_right,
        write_ok
    );
    crate::yarm_log!(
        "IPCSEND_SHARED_REGION_LIFECYCLE_OK arch=x86_64 class={} transaction_published=1 receiver_wakes={} leaked_state=0",
        class,
        u8::from(woke_receiver)
    );
    maybe_log_ipc_send_shared_region_direct_retired();
}

/// Stage 198E3C1: marker-clean stub for every NON-armed build (all normal artifacts + all non-x86
/// architectures). It contains no marker literal, so a normal artifact stays clean and no
/// shared-region retirement/attestation literal is ever compiled into an AArch64/RISC-V binary.
#[cfg(not(all(feature = "x86-shared-region-direct-oracle", target_arch = "x86_64")))]
pub(crate) fn maybe_emit_shared_region_direct_live_markers(
    _class: &str,
    _snapshot: &crate::kernel::boot::shared_region_txn::RecvBoundarySharedRegionSnapshot,
    _woke_receiver: bool,
    _origin: Option<SharedRegionLiveOrigin>,
) {
}

/// Stage 193A: one-shot latch for the IpcSendPlain boundary retirement markers.
static IPC_SEND_PLAIN_RETIRE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 193A: emit the IpcSendPlain retirement markers exactly once (first plain
/// waiting-receiver delivery completed through the out-of-broad-lock boundary drain).
pub(crate) fn maybe_log_ipc_send_plain_retired() {
    if IPC_SEND_PLAIN_RETIRE_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        // Stage 198A (SECOND-COHORT PLAIN PARITY): all architectures emit the canonical
        // arch-tagged retirement marker `arch=<arch> class=IpcSendPlain`. The drain executor
        // (`execute_dispatch_post_work` in runtime.rs) is arch-neutral and reached from all
        // three trap-entry drains, so the arch string is selected here by `cfg`.
        #[cfg(target_arch = "aarch64")]
        {
            crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=IpcSendPlain");
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=IpcSendPlain result=ok"
            );
        }
        #[cfg(target_arch = "x86_64")]
        {
            crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=x86_64 class=IpcSendPlain");
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcSendPlain result=ok"
            );
        }
        #[cfg(target_arch = "riscv64")]
        {
            crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=riscv64 class=IpcSendPlain");
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcSendPlain result=ok"
            );
        }
    }
}

// ── Stage 193C (BROAD-IPC DECOMPOSITION — IpcSend ordinary cap-transfer slice) ──────
//
// IpcSend of an ORDINARY cap-transfer message (exactly one transferred cap, not a reply
// cap, not a shared-region) to an already-recv-v2-blocked receiver reuses the SAME 188C
// producer + executor `ipc_reply` uses: Phase A snapshots object/rights/delegation-parent
// + payload/meta by value (NO mint, NO user copy, NO wake) and consumes the transfer
// envelope ONCE under the broad borrow; the trap-entry drain materializes the fresh
// receiver-local cap through the 186D2/186D3 seam, copies payload/meta through the 186E
// seam, and wakes the receiver once — all AFTER the broad borrow drops. This per-CPU flag
// tags the stashed ordinary-cap delivery as originating from `ipc_send` so the drain emits
// the IpcSend-cap-specific boundary markers (the ordinary-cap executor arm is shared with
// `ipc_reply`, which leaves the flag unset).

/// Stage 193C: per-CPU "the pending ordinary-cap delivery originated from ipc_send" flag.
pub(crate) static IPC_SEND_CAP_BOUNDARY_ORIGIN: [core::sync::atomic::AtomicBool;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 193C: tag the just-stashed ordinary-cap delivery on `cpu` as an ipc_send split.
pub(crate) fn ipc_send_cap_boundary_origin_set(cpu_idx: usize) {
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        IPC_SEND_CAP_BOUNDARY_ORIGIN[cpu_idx].store(true, core::sync::atomic::Ordering::Release);
    }
}

/// Stage 193C: is the pending ordinary-cap delivery on `cpu` an ipc_send split? (peek)
pub(crate) fn ipc_send_cap_boundary_origin_is_set(cpu_idx: usize) -> bool {
    cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && IPC_SEND_CAP_BOUNDARY_ORIGIN[cpu_idx].load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 193C: consume the ipc_send ordinary-cap origin flag for `cpu` (clear + return prior).
pub(crate) fn ipc_send_cap_boundary_origin_take(cpu_idx: usize) -> bool {
    cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && IPC_SEND_CAP_BOUNDARY_ORIGIN[cpu_idx].swap(false, core::sync::atomic::Ordering::AcqRel)
}

/// Stage 193C: one-shot latch for the IpcSendOrdinaryCap boundary retirement markers.
static IPC_SEND_ORDINARY_CAP_RETIRE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 193C: emit the IpcSendOrdinaryCap retirement markers exactly once (first
/// ordinary cap-transfer waiting-receiver delivery completed through the out-of-broad-lock
/// boundary drain).
pub(crate) fn maybe_log_ipc_send_ordinary_cap_retired() {
    if IPC_SEND_ORDINARY_CAP_RETIRE_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        // Stage 198B (ORDINARY-CAP PARITY): arch-tagged on all three arches. The drain executor
        // (`execute_dispatch_post_work` in runtime.rs) is arch-neutral and reached from all three
        // trap-entry drains, so the arch string is selected here by `cfg`.
        #[cfg(target_arch = "aarch64")]
        {
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=IpcSendOrdinaryCap"
            );
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=IpcSendOrdinaryCap result=ok"
            );
        }
        #[cfg(target_arch = "x86_64")]
        {
            crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=x86_64 class=IpcSendOrdinaryCap");
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcSendOrdinaryCap result=ok"
            );
        }
        #[cfg(target_arch = "riscv64")]
        {
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=riscv64 class=IpcSendOrdinaryCap"
            );
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcSendOrdinaryCap result=ok"
            );
        }
    }
}

// ── Stage 193D (BROAD-IPC DECOMPOSITION — IpcSend reply-cap transfer slice) ─────────
//
// IpcSend of a REPLY-CAP transfer message (FLAG_REPLY_CAP + exactly one transferred cap)
// to an already-recv-v2-blocked receiver reuses the SAME 188D reply-cap producer +
// executor `ipc_reply` carries: Phase A snapshots the reply object's registry
// coordinates (reply_index, reply_generation) + payload/meta by value (NO mint, NO IPC
// record, NO user copy, NO wake) and consumes the reply-cap transfer envelope ONCE under
// the broad borrow; the trap-entry drain mints the fresh receiver-local one-shot reply
// cap through the rank-4 seam, records the waiter-cap through the rank-3 IPC seam, copies
// payload/meta through the 186E seam, and wakes the receiver once — all AFTER the broad
// borrow drops. This per-CPU flag tags the stashed reply-cap delivery as originating from
// `ipc_send` so the drain emits the IpcSend-reply-cap-specific boundary markers (the
// reply-cap executor arm is shared with `ipc_reply`, which leaves the flag unset).

/// Stage 193D: per-CPU "the pending reply-cap delivery originated from ipc_send" flag.
pub(crate) static IPC_SEND_REPLY_CAP_BOUNDARY_ORIGIN: [core::sync::atomic::AtomicBool;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; crate::kernel::scheduler::MAX_CPUS];

/// Stage 193D: tag the just-stashed reply-cap delivery on `cpu` as an ipc_send split.
pub(crate) fn ipc_send_reply_cap_boundary_origin_set(cpu_idx: usize) {
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        IPC_SEND_REPLY_CAP_BOUNDARY_ORIGIN[cpu_idx]
            .store(true, core::sync::atomic::Ordering::Release);
    }
}

/// Stage 193D: is the pending reply-cap delivery on `cpu` an ipc_send split? (peek)
pub(crate) fn ipc_send_reply_cap_boundary_origin_is_set(cpu_idx: usize) -> bool {
    cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && IPC_SEND_REPLY_CAP_BOUNDARY_ORIGIN[cpu_idx].load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 193D: consume the ipc_send reply-cap origin flag for `cpu` (clear + return prior).
pub(crate) fn ipc_send_reply_cap_boundary_origin_take(cpu_idx: usize) -> bool {
    cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && IPC_SEND_REPLY_CAP_BOUNDARY_ORIGIN[cpu_idx]
            .swap(false, core::sync::atomic::Ordering::AcqRel)
}

/// Stage 193D: one-shot latch for the IpcSendReplyCap boundary retirement markers.
static IPC_SEND_REPLY_CAP_RETIRE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 193D: emit the IpcSendReplyCap retirement markers exactly once (first reply-cap
/// waiting-receiver delivery completed through the out-of-broad-lock boundary drain).
pub(crate) fn maybe_log_ipc_send_reply_cap_retired() {
    if IPC_SEND_REPLY_CAP_RETIRE_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        // Stage 198C2 (REPLY-CAP DIRECT PARITY): arch-tagged on all three arches, matching the
        // IpcSendPlain / IpcSendOrdinaryCap vocabulary. The drain executor
        // (`execute_dispatch_post_work` in runtime.rs) is arch-neutral and reached from all three
        // trap-entry drains, so the arch string is selected here by `cfg`. Emitted ONLY from the
        // real reply-cap direct-delivery boundary drain (gated by the boundary-origin flag), never
        // from ordinary-cap / plain / enqueue / test-only paths.
        #[cfg(target_arch = "aarch64")]
        {
            crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=IpcSendReplyCap");
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=IpcSendReplyCap result=ok"
            );
        }
        #[cfg(target_arch = "x86_64")]
        {
            crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=x86_64 class=IpcSendReplyCap");
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcSendReplyCap result=ok"
            );
        }
        #[cfg(target_arch = "riscv64")]
        {
            crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=riscv64 class=IpcSendReplyCap");
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcSendReplyCap result=ok"
            );
        }
    }
}

// ── Stage 193E (BROAD-IPC DECOMPOSITION — IpcSend plain no-waiter enqueue slice) ────
//
// IpcSend of a PLAIN message to a buffered endpoint with NO blocked receiver enqueues
// the message via the endpoint-only Stage 4E seam (`ipc_try_send_queued_plain_endpoint_only`,
// rank-4 IPC lock only): NO user copy, NO cap materialization, NO receiver wake, NO sender
// block (the sender returns Ok and continues; the message waits in the queue for a later
// receiver's dequeue). Unlike the 193A–D blocked-waiter slices, there is NO deferred Phase
// B/C work — the whole slice is the in-lock endpoint enqueue. This class formalizes the
// PLAIN no-waiter enqueue (cap-transfer / reply-cap / shared-region enqueue stay on the
// legacy Stage 4E path, NOT retired).

/// Stage 193E: one-shot latch for the IpcSendPlainEnqueue boundary retirement markers.
static IPC_SEND_PLAIN_ENQUEUE_RETIRE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 193E: emit the IpcSendPlainEnqueue retirement markers exactly once (first plain
/// no-waiter enqueue completed through the endpoint-only boundary seam).
pub(crate) fn maybe_log_ipc_send_plain_enqueue_retired() {
    if IPC_SEND_PLAIN_ENQUEUE_RETIRE_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        // Stage 198A (SECOND-COHORT PLAIN PARITY): all architectures emit the canonical
        // arch-tagged retirement marker `arch=<arch> class=IpcSendPlainEnqueue`. This helper
        // is called from the arch-neutral in-lock enqueue seam (ipc_state.rs), so the arch
        // string is selected here by `cfg`.
        #[cfg(target_arch = "aarch64")]
        {
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=IpcSendPlainEnqueue"
            );
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=IpcSendPlainEnqueue result=ok"
            );
        }
        #[cfg(target_arch = "x86_64")]
        {
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=x86_64 class=IpcSendPlainEnqueue"
            );
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcSendPlainEnqueue result=ok"
            );
        }
        #[cfg(target_arch = "riscv64")]
        {
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=riscv64 class=IpcSendPlainEnqueue"
            );
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcSendPlainEnqueue result=ok"
            );
        }
    }
}

// ── Stage 193F (BROAD-IPC DECOMPOSITION — IpcSend ordinary-cap no-waiter enqueue slice) ─
//
// IpcSend of an ORDINARY cap-transfer message (FLAG_CAP_TRANSFER / FLAG_CAP_TRANSFER_PLAIN,
// exactly one transferred cap whose OBJECT is ordinary — not a Reply, not a shared-region)
// to a buffered endpoint with NO blocked receiver enqueues via the endpoint-only Stage 4E
// seam. Like 193E there is NO deferred Phase B/C work and NO receiver user-copy / cap
// materialization / wake / sender block AT ENQUEUE TIME: the transfer envelope is PRESERVED
// in the envelope table (the queued message carries only its numeric handle), and the
// receiver's LATER recv_v2 consumes the envelope + materializes a fresh receiver-local cap
// (`IPC_TRANSFER_CAP_MATERIALIZE_OK`). This class formalizes the ORDINARY-object no-waiter
// cap enqueue (reply-cap / shared-region enqueue stay on the legacy path, NOT retired).

/// Stage 193F: one-shot latch for the IpcSendOrdinaryCapEnqueue retirement markers.
static IPC_SEND_ORDINARY_CAP_ENQUEUE_RETIRE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 193F: emit the IpcSendOrdinaryCapEnqueue retirement markers exactly once (first
/// ordinary-cap no-waiter enqueue completed through the endpoint-only boundary seam).
pub(crate) fn maybe_log_ipc_send_ordinary_cap_enqueue_retired() {
    if IPC_SEND_ORDINARY_CAP_ENQUEUE_RETIRE_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        // Stage 198B (ORDINARY-CAP PARITY): arch-tagged on all three arches. Emitted from the
        // arch-neutral in-lock enqueue seam (ipc_state.rs), so `cfg` selects the arch string.
        #[cfg(target_arch = "aarch64")]
        {
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=IpcSendOrdinaryCapEnqueue"
            );
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=IpcSendOrdinaryCapEnqueue result=ok"
            );
        }
        #[cfg(target_arch = "x86_64")]
        {
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=x86_64 class=IpcSendOrdinaryCapEnqueue"
            );
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcSendOrdinaryCapEnqueue result=ok"
            );
        }
        #[cfg(target_arch = "riscv64")]
        {
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=riscv64 class=IpcSendOrdinaryCapEnqueue"
            );
            crate::yarm_log!(
                "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcSendOrdinaryCapEnqueue result=ok"
            );
        }
    }
}

/// Stage 169 (D2-GENUINE-SEND): x86_64-only, default-off gate that runs the
/// blocking-SEND path (endpoint full / synchronous no-waiter) through explicit
/// rank-clean scheduler/task/IPC phase markers and relocates its queue-advancing
/// dispatch OUT of the global lock, exactly as Stage 168B did for recv. When OFF
/// (default) the send path is byte-identical to Stage 168B (no behavior change);
/// the Stage 163P sender-wake oracle is preserved on both paths.
/// VALIDATION: D2_SEND_GENUINE_ENABLED.
///
/// Stage 182 (REMOVE-FALLBACKS): compile-time production gate (see
/// [`d6_genuine_enabled`]). The `yarm.d2_send_genuine` knob + `AtomicBool`/setter were
/// deleted; the graduated blocking-send seam is the only x86_64 `-smp 1` path, with no
/// runtime opt-out to the old in-lock production path.
pub(crate) fn d2_send_genuine_enabled() -> bool {
    d6_genuine_enabled()
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

/// Stage 177 (SMP-READY): arch-neutral, default-off DIAGNOSTIC gate for the x86_64
/// SMP-readiness audit (AP bring-up / per-CPU state / remote-wake + IPI readiness)
/// markers + the one-shot SMP-readiness audit. It changes NO state/ABI/SMP behavior
/// — only emits SMP_READY_* markers and does NOT bring APs into the production
/// scheduler (BSP-only stays BSP-only). VALIDATION: SMP_READY_ENABLED.
pub(crate) static SMP_READY_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 177: one-shot latch so the SMP-readiness audit runs exactly once.
pub(crate) static SMP_READY_AUDIT_STARTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub fn set_smp_ready_enabled(enabled: bool) {
    SMP_READY_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn smp_ready_enabled() -> bool {
    SMP_READY_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 189C6 (LIVE-AP-DISPATCH): x86_64-only, DEFAULT-OFF gate that arms the
/// FIRST live application-processor user dispatch. When OFF (default) the AP
/// idle-loop live hook is an inert single-load-and-branch — the AP stays in its
/// wake-only managed idle loop and the accepted smp2/smp4 baseline is byte-for-byte
/// preserved. When ON (`yarm.ap_user_dispatch=1`), after the audited wake-only
/// clear the BSP builds a self-contained AP ring3 probe task, posts the per-CPU
/// dispatch request, wakes the AP, and the AP's live hook enters ring 3 and issues
/// the probe syscall — proving `X86_AP_RING3_ENTER` + `X86_AP_USER_SYSCALL_REENTRY_OK`
/// on a real second CPU. VALIDATION: AP_USER_DISPATCH_ENABLED.
pub(crate) static AP_USER_DISPATCH_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub fn set_ap_user_dispatch_enabled(enabled: bool) {
    AP_USER_DISPATCH_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn ap_user_dispatch_enabled() -> bool {
    AP_USER_DISPATCH_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 177: try to claim the one-shot SMP-readiness audit (true exactly once).
pub(crate) fn smp_ready_audit_try_start() -> bool {
    SMP_READY_AUDIT_STARTED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
}

/// Stage 178 (CROSS-ARCH-D6): arch-neutral, default-off DIAGNOSTIC gate for the
/// AArch64/RISC-V D6 restore-path audit (user trapframe / exception-return / dispatch
/// / lock-drop readiness) markers + the one-shot per-arch restore-readiness audit. It
/// changes NO state/ABI/dispatch behavior and does NOT live-wire any cross-arch D6
/// restore — only emits CROSS_ARCH_D6_* markers. VALIDATION: CROSS_ARCH_D6_ENABLED.
pub(crate) static CROSS_ARCH_D6_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 178: one-shot latch so the cross-arch D6 audit runs exactly once.
pub(crate) static CROSS_ARCH_D6_AUDIT_STARTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 184 (CROSS-ARCH-LIVE): one-shot latch for the cross-arch live audit. This
/// audit is DEFAULT-ON (no knob) and runs on every arch: it attests the honest
/// per-arch topology (dispatching_cpu_count) and that the accepted graduated
/// D2/D6/D3 correctness invariants + syscall-error parity are live for this arch's
/// topology. It live-wires nothing and changes no dispatch/ABI behavior.
pub(crate) static CROSS_ARCH_LIVE_AUDIT_STARTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 184: try to claim the one-shot cross-arch live audit (true exactly once).
pub(crate) fn cross_arch_live_audit_try_start() -> bool {
    CROSS_ARCH_LIVE_AUDIT_STARTED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
}

pub(crate) fn set_cross_arch_d6_enabled(enabled: bool) {
    CROSS_ARCH_D6_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub(crate) fn cross_arch_d6_enabled() -> bool {
    CROSS_ARCH_D6_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 178: try to claim the one-shot cross-arch D6 audit (true exactly once).
pub(crate) fn cross_arch_d6_audit_try_start() -> bool {
    CROSS_ARCH_D6_AUDIT_STARTED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
}

/// Stage 179 (D3-FULL): arch-neutral, default-off gate for the D3 VM anonymous
/// map/unmap two-phase diagnostic markers + the one-shot self-contained D3 proof
/// (drives the REAL VM primitives on a scratch address space; local TLB flush live,
/// remote shootdown prepped/deferred). It changes NO production VM ABI and claims NO
/// real SMP shootdown — only emits D3_* markers. VALIDATION: D3_FULL_ENABLED.
pub(crate) static D3_FULL_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 179: one-shot latch so the D3 proof runs exactly once.
pub(crate) static D3_FULL_PROOF_STARTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_d3_full_enabled(enabled: bool) {
    D3_FULL_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub(crate) fn d3_full_enabled() -> bool {
    D3_FULL_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 179: try to claim the one-shot D3 proof (true exactly once).
pub(crate) fn d3_full_proof_try_start() -> bool {
    D3_FULL_PROOF_STARTED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
}

/// Stage 181 (GRADUATE-KNOBS) → Stage 182 (REMOVE-FALLBACKS): the graduated x86_64
/// `-smp 1` unlock seams (D2-RECV/D2-SEND/D6 out-of-global-lock dispatch) are the
/// production path. Stage 182 DELETED the `yarm.unlock_graduated` umbrella knob and its
/// `AtomicBool`/setter (including the `=0` emergency opt-out that ran the old
/// global-lock production path) — there is no runtime toggle back to the fallback.
/// This is now a compile-time constant identical to the individual seam gate: the
/// verification proof runs wherever the graduated seams are the production path.
pub(crate) fn unlock_graduated_enabled() -> bool {
    d6_genuine_enabled()
}

/// Stage 181: one-shot latch so the graduation verification proof runs exactly once.
pub(crate) static UNLOCK_GRADUATED_PROOF_STARTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 181: try to claim the one-shot graduation proof (true exactly once).
pub(crate) fn unlock_graduated_proof_try_start() -> bool {
    UNLOCK_GRADUATED_PROOF_STARTED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
}

/// Stage 183.5: set once the graduated one-shot proof has emitted its verdict
/// (any result). The AP scheduler-online admission is sequenced AFTER this so
/// the accepted graduated evidence still runs on the BSP with `online == 1`
/// (the proof's out-of-lock seam slices require the single-CPU topology until
/// 183.6 proves them under SMP).
pub(crate) static UNLOCK_GRADUATED_PROOF_COMPLETED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn unlock_graduated_proof_completed() -> bool {
    UNLOCK_GRADUATED_PROOF_COMPLETED.load(core::sync::atomic::Ordering::Acquire)
}

pub(crate) fn set_unlock_graduated_proof_completed() {
    UNLOCK_GRADUATED_PROOF_COMPLETED.store(true, core::sync::atomic::Ordering::Release);
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

// ── Stage 193B (IPCSEND-PLAIN LIVE ORACLE) ──────────────────────────────────
//
// `yarm.ipc_send_plain_oracle=1` SUB-knob, layered on `yarm.ipc_recv_proof=1`.
// Default-off and INDEPENDENT of the sender-wake sub-knob. When active, the
// bootstrap provisions a coordination endpoint E2 (init's RECV cap goes to
// startup slot 14, and slot 13 stays empty — the presence pattern that lets init
// pick the send-plain oracle over sender-wake), and the receiver-block publish
// path (`publish_recv_waiter_live`) pushes a deterministic "receiver blocked on
// E1" signal into E2 within the SAME `ipc_state_lock` section that registers the
// waiter — an atomic proxy for "a receiver is a waiter on E1". init polls E2 and
// plain-`ipc_send`s to E1 only after the forked child receiver is provably
// blocked, so the send takes the 193A plain boundary split (no enqueue race).
//
// The coordination endpoint index reuses `IPC_RECV_PROOF_SENDER_WAKE_E2_IDX`
// (it is just "the proof coordination endpoint index"); the two oracles never run
// together (mutually exclusive sub-knobs), so there is no cross-firing.
pub(crate) static IPC_SEND_PLAIN_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_ipc_send_plain_oracle_enabled(enabled: bool) {
    IPC_SEND_PLAIN_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn ipc_send_plain_oracle_enabled() -> bool {
    IPC_SEND_PLAIN_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// True only when BOTH the base proof knob and the send-plain-oracle sub-knob are
/// set — the precondition for any 193B coordination/workload behavior.
pub fn ipc_send_plain_oracle_active() -> bool {
    ipc_recv_oracle_proof_enabled() && ipc_send_plain_oracle_enabled()
}

/// Stage 193C: `yarm.ipc_send_cap_oracle=1` SUB-knob (layered on the base proof
/// knob, independent of the plain oracle). Gates the IpcSend ordinary cap-transfer
/// live oracle, which shares the SAME receiver-block coordination mechanism as the
/// plain oracle (mutually exclusive coordination-slot pattern: cap oracle uses init
/// startup slot 13, plain oracle uses slot 14).
pub(crate) static IPC_SEND_CAP_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_ipc_send_cap_oracle_enabled(enabled: bool) {
    IPC_SEND_CAP_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn ipc_send_cap_oracle_enabled() -> bool {
    IPC_SEND_CAP_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// True only when BOTH the base proof knob and the send-cap-oracle sub-knob are set.
pub fn ipc_send_cap_oracle_active() -> bool {
    ipc_recv_oracle_proof_enabled() && ipc_send_cap_oracle_enabled()
}

/// Stage 193D: `yarm.ipc_send_reply_cap_oracle=1` SUB-knob (layered on the base proof
/// knob, independent of the plain + ordinary-cap oracles). Gates the IpcSend reply-cap
/// transfer live oracle, which shares the SAME receiver-block coordination mechanism.
/// Coordination-slot pattern: reply-cap oracle uses init startup slots 13 (coord) + 14
/// (a kernel-provisioned transferable reply cap) + 17 (a discriminator that separates it
/// from sender-wake, which also uses slots 13+14).
pub(crate) static IPC_SEND_REPLY_CAP_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_ipc_send_reply_cap_oracle_enabled(enabled: bool) {
    IPC_SEND_REPLY_CAP_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn ipc_send_reply_cap_oracle_enabled() -> bool {
    IPC_SEND_REPLY_CAP_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// True only when BOTH the base proof knob and the send-reply-cap-oracle sub-knob are set.
pub fn ipc_send_reply_cap_oracle_active() -> bool {
    ipc_recv_oracle_proof_enabled() && ipc_send_reply_cap_oracle_enabled()
}

/// Stage 193E: `yarm.ipc_send_enqueue_oracle=1` SUB-knob (layered on the base proof
/// knob). Gates the IpcSend plain no-waiter enqueue live oracle. Unlike the blocked-waiter
/// oracles it needs NO fork / coordination endpoint — a plain send to the loopback E1 with
/// no blocked receiver simply enqueues — so it is signalled by init startup slot 17 alone
/// (slots 13 + 14 empty), distinct from every other oracle's slot pattern.
pub(crate) static IPC_SEND_ENQUEUE_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_ipc_send_enqueue_oracle_enabled(enabled: bool) {
    IPC_SEND_ENQUEUE_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn ipc_send_enqueue_oracle_enabled() -> bool {
    IPC_SEND_ENQUEUE_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// True only when BOTH the base proof knob and the send-enqueue-oracle sub-knob are set.
pub fn ipc_send_enqueue_oracle_active() -> bool {
    ipc_recv_oracle_proof_enabled() && ipc_send_enqueue_oracle_enabled()
}

/// Stage 193F: `yarm.ipc_send_cap_enqueue_oracle=1` SUB-knob (layered on the base proof
/// knob). Gates the IpcSend ordinary-cap no-waiter enqueue live oracle. Like the 193E plain
/// enqueue oracle it needs NO fork / coordination endpoint — init sends a cap-transfer to
/// the loopback with no blocked receiver, then recv-drains it to materialize a fresh cap. It
/// shares the slot-17 discriminator with 193E: slot 17 == 1 selects the plain enqueue oracle,
/// slot 17 == 2 selects this ordinary-cap enqueue oracle (slots 13 + 14 empty for both).
pub(crate) static IPC_SEND_CAP_ENQUEUE_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_ipc_send_cap_enqueue_oracle_enabled(enabled: bool) {
    IPC_SEND_CAP_ENQUEUE_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn ipc_send_cap_enqueue_oracle_enabled() -> bool {
    IPC_SEND_CAP_ENQUEUE_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 195C: default-off AArch64 FutexWake live-oracle knob (`yarm.aarch64_futex_wake_oracle=1`).
/// When set, the AArch64 boot signals init (startup slot 5, unused by init) to run a controlled
/// parent/child FutexWake oracle: a child thread blocks via legacy FutexWait, the parent wakes it
/// once through the split path (count must be 1), then wakes again (count must be 0).
pub(crate) static AARCH64_FUTEX_WAKE_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_aarch64_futex_wake_oracle_enabled(enabled: bool) {
    AARCH64_FUTEX_WAKE_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn aarch64_futex_wake_oracle_enabled() -> bool {
    AARCH64_FUTEX_WAKE_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 195E: default-off AArch64 FutexWait (NR 9) queue-advancing out-of-lock retirement.
/// When set, an eligible in-lock `futex_wait_current` (BSP, shared trap drain active,
/// dispatching_cpu_count()<=1, no outstanding deferral) publishes `Blocked(Futex)`, clears
/// `current`, records a one-shot per-CPU deferral, and skips the in-lock dispatch — the
/// trap-entry drain then performs the authoritative queue-advancing dispatch + EL0 restore
/// off the global lock. Every ineligible case keeps the unchanged in-lock `dispatch_next_task`
/// fallback. Default-off keeps the proven in-lock FutexWait path as the production default.
pub(crate) static AARCH64_FUTEX_WAIT_RETIRE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_aarch64_futex_wait_retire_enabled(enabled: bool) {
    AARCH64_FUTEX_WAIT_RETIRE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn aarch64_futex_wait_retire_enabled() -> bool {
    AARCH64_FUTEX_WAIT_RETIRE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 195F: default-off AArch64 FutexWait NO-INCOMING idle-oracle WORKLOAD selector. The
/// retirement MECHANISM is default-on (no knob); this flag only selects the narrowly-gated
/// idle-oracle init workload (a final FutexWait with no other runnable user task) and the
/// `AARCH64_FUTEX_WAIT_IDLE_ORACLE_DONE` attestation emitted by the post-lock idle drain.
pub(crate) static AARCH64_FUTEX_WAIT_IDLE_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_aarch64_futex_wait_idle_oracle_enabled(enabled: bool) {
    AARCH64_FUTEX_WAIT_IDLE_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn aarch64_futex_wait_idle_oracle_enabled() -> bool {
    AARCH64_FUTEX_WAIT_IDLE_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 195G: default-off AArch64 Yield TWO-TASK oracle WORKLOAD selector. The Yield retirement
/// MECHANISM is default-on (no knob); this flag only selects the init two-task oracle workload
/// (slot 5 = 4).
pub(crate) static AARCH64_YIELD_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_aarch64_yield_oracle_enabled(enabled: bool) {
    AARCH64_YIELD_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn aarch64_yield_oracle_enabled() -> bool {
    AARCH64_YIELD_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 195G: default-off AArch64 Yield LONE-TASK oracle WORKLOAD selector (slot 5 = 5).
pub(crate) static AARCH64_YIELD_LONE_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_aarch64_yield_lone_oracle_enabled(enabled: bool) {
    AARCH64_YIELD_LONE_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn aarch64_yield_lone_oracle_enabled() -> bool {
    AARCH64_YIELD_LONE_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 196A: default-off RISC-V post-lock-drain FOUNDATION oracle selector.
/// When enabled, the RISC-V shared trap wrapper (`handle_riscv_trap_entry_shared`)
/// publishes a one-shot post-work token during its broad-lock (`with_cpu`) phase
/// and consumes it AFTER the outer `SpinLock<KernelState>` guard drops, proving
/// genuine post-lock-drain ordering: the lock-dropped proof re-acquires
/// `with_cpu` (which would deadlock if the guard were still held). It enables
/// ZERO retirement classes and mutates no scheduler / capability / user-copy /
/// task-switch state — it only reads `current_tid` and drives log markers.
pub(crate) static RISCV_POST_LOCK_FOUNDATION_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_riscv_post_lock_foundation_oracle_enabled(enabled: bool) {
    RISCV_POST_LOCK_FOUNDATION_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn riscv_post_lock_foundation_oracle_enabled() -> bool {
    RISCV_POST_LOCK_FOUNDATION_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 196C: default-off RISC-V FutexWake (NR 10) live-oracle selector
/// (`yarm.riscv64_futex_wake_oracle=1`). When enabled, the RISC-V boot provisions init
/// startup slot 5 (=1) so init runs the parent/child split-FutexWake proof: the child
/// blocks on the LEGACY global-lock FutexWait, the parent wakes it through the SPLIT path
/// and verifies the authoritative wake counts (1 then 0). It enables NO additional
/// retirement class (FutexWake retirement is the split MECHANISM, live by default once the
/// class is enabled); this flag only selects the proof workload.
pub(crate) static RISCV_FUTEX_WAKE_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_riscv_futex_wake_oracle_enabled(enabled: bool) {
    RISCV_FUTEX_WAKE_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn riscv_futex_wake_oracle_enabled() -> bool {
    RISCV_FUTEX_WAKE_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 197A: default-off x86_64 FutexWake live-oracle WORKLOAD selector
/// (`yarm.x86_64_futex_wake_oracle=1`). Provisions init startup slot 5 (=1) so init runs the
/// parent/child split-FutexWake proof (counts 1 then 0, waiter resumes once) — closing the
/// first-cohort matrix at 12/12 LIVE. Selects the proof workload only; the FutexWake retirement
/// MECHANISM is already live by default.
pub(crate) static X86_FUTEX_WAKE_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_x86_futex_wake_oracle_enabled(enabled: bool) {
    X86_FUTEX_WAKE_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn x86_futex_wake_oracle_enabled() -> bool {
    X86_FUTEX_WAKE_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 198E3C1: default-off x86_64 DIRECT shared-region live-oracle WORKLOAD selector
/// (`yarm.x86_64_shared_region_direct_oracle=1`). Provisions init startup slot 5 (=2) so init runs
/// the parent/child shared-region (`IpcSendSharedRegionDirect`) delivery proof — a receiver blocks
/// first in recv-v2, the parent transfers a multi-page shared `MemoryObject`, and the accepted
/// off-lock post-lock drain maps it + wakes the receiver exactly once. Selecting this workload also
/// arms the shared IPC/oracle-proof knob so the direct producer becomes live (INERT on a normal
/// boot, which never sets this). Does NOT enable the queued shared-region class or any non-x86 arch.
pub(crate) static X86_SHARED_REGION_DIRECT_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_x86_shared_region_direct_oracle_enabled(enabled: bool) {
    X86_SHARED_REGION_DIRECT_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
    if enabled {
        // Selecting the workload arms the shared IPC/oracle-proof knob so the DIRECT shared-region
        // producer is live for this run (the queued class stays disabled).
        set_ipc_recv_oracle_proof_enabled(true);
    }
}

pub fn x86_shared_region_direct_oracle_enabled() -> bool {
    X86_SHARED_REGION_DIRECT_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

// ─── Stage 197A-C: mandatory init ELF loading (no synthetic fallback) ──────────────────
//
// Why an init load can be fatal. There is NO synthetic/placeholder init ELF fallback anymore
// (Stage 197A removed it): a missing initramfs, a malformed CPIO, a missing `/init`, an
// oversized/malformed init ELF, or a forced-fault ZC load MUST halt boot with an explicit
// `BOOT_FATAL_*` diagnostic instead of silently limping on (or booting a fake init).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitLoadFatal {
    /// No boot initramfs blob was provided (`boot_initrd_bytes()` is None).
    InitramfsMissing,
    /// The initramfs blob is not a parseable CPIO archive.
    CpioInvalid,
    /// The CPIO archive parses but contains no `/init` (or `init`) entry.
    InitNotFound,
    /// The `/init` entry exceeds the maximum init ELF size.
    TooLarge,
}

/// Default-off fault-injection knob (`yarm.force_init_zc_load_fail=1`): forces the required init
/// ELF load to fail so the fatal `BOOT_FATAL_INIT_ZC_LOAD_FAILED` halt path can be exercised
/// under QEMU without corrupting the initramfs. NEVER set on a normal boot.
pub(crate) static FORCE_INIT_ZC_LOAD_FAIL: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_force_init_zc_load_fail(enabled: bool) {
    FORCE_INIT_ZC_LOAD_FAIL.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn force_init_zc_load_fail() -> bool {
    FORCE_INIT_ZC_LOAD_FAIL.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 197B default-off NEGATIVE oracle knob (`yarm.riscv_typed_outcome_internal_error_oracle=1`):
/// forces the RISC-V trap wrapper to return a GENUINE internal trap-handling error on the first
/// syscall from a LIVE current task, proving the bridge takes the fatal `RISCV_TRAP_HANDLE_FAILED`
/// path and NEVER the FutexWait typed-idle-success path. NEVER set on a normal boot.
pub(crate) static RISCV_TYPED_OUTCOME_INTERNAL_ERROR_ORACLE_ENABLED:
    core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_riscv_typed_outcome_internal_error_oracle_enabled(enabled: bool) {
    RISCV_TYPED_OUTCOME_INTERNAL_ERROR_ORACLE_ENABLED
        .store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn riscv_typed_outcome_internal_error_oracle_enabled() -> bool {
    RISCV_TYPED_OUTCOME_INTERNAL_ERROR_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Maximum accepted init ELF size (16 MiB) — shared across architectures.
pub const INIT_ELF_MAX_SIZE: usize = 16 * 1024 * 1024;

/// Load the REQUIRED `/init` ELF bytes from the boot initramfs, distinguishing every fatal
/// failure reason. Arch-neutral: it reads the immutable boot CPIO blob and locates `/init`
/// (or `init`). There is no fallback — a `None`/error here is a boot-fatal condition.
pub fn load_required_init_elf_bytes() -> Result<alloc::vec::Vec<u8>, InitLoadFatal> {
    let bytes = Bootstrap::boot_initrd_bytes().ok_or(InitLoadFatal::InitramfsMissing)?;
    let archive = yarm_srv_common::cpio::CpioArchive::new(bytes);
    // A CPIO parse error (bad magic / truncated header) is distinct from "parsed but no /init".
    let entry = match archive.find("/init") {
        Ok(Some(e)) => e,
        Ok(None) => match yarm_srv_common::cpio::CpioArchive::new(bytes).find("init") {
            Ok(Some(e)) => e,
            Ok(None) => return Err(InitLoadFatal::InitNotFound),
            Err(_) => return Err(InitLoadFatal::CpioInvalid),
        },
        Err(_) => return Err(InitLoadFatal::CpioInvalid),
    };
    let file_data = entry.file_data();
    crate::yarm_log!("YARM_INITRD_INIT_FOUND len={}", file_data.len());
    if file_data.len() > INIT_ELF_MAX_SIZE {
        return Err(InitLoadFatal::TooLarge);
    }
    Ok(alloc::vec::Vec::from(file_data))
}

/// Emit the canonical `BOOT_FATAL_*` diagnostic(s) for an init-load fatal reason. The caller
/// follows this with the arch's fatal halt (reused, not a new mechanism), so boot stops here
/// rather than continuing with no user tasks.
pub fn log_init_load_fatal(reason: InitLoadFatal) {
    match reason {
        InitLoadFatal::InitramfsMissing => {
            crate::yarm_log!("BOOT_FATAL_INITRAMFS_MISSING");
            crate::yarm_log!("BOOT_FATAL_NO_CPIO");
        }
        InitLoadFatal::CpioInvalid => {
            crate::yarm_log!("BOOT_FATAL_CPIO_INVALID");
        }
        InitLoadFatal::InitNotFound => {
            crate::yarm_log!("BOOT_FATAL_INIT_NOT_FOUND path=/init");
        }
        InitLoadFatal::TooLarge => {
            crate::yarm_log!("BOOT_FATAL_INIT_ELF_INVALID reason=too_large");
        }
    }
}

/// True only when BOTH the base proof knob and the send-cap-enqueue-oracle sub-knob are set.
pub fn ipc_send_cap_enqueue_oracle_active() -> bool {
    ipc_recv_oracle_proof_enabled() && ipc_send_cap_enqueue_oracle_enabled()
}

/// True when ANY blocked-waiter IpcSend live oracle (plain 193B / ordinary-cap 193C /
/// reply-cap 193D) is active — the precondition for the shared receiver-block coordination
/// hook to fire. The 193E enqueue oracle is NOT here: it has no blocked receiver, so it
/// never uses the receiver-block coordination hook.
pub fn ipc_send_oracle_coordination_active() -> bool {
    ipc_send_plain_oracle_active()
        || ipc_send_cap_oracle_active()
        || ipc_send_reply_cap_oracle_active()
}

/// If `endpoint_idx` is the provisioned proof loopback E1 (and EITHER IpcSend live
/// oracle sub-knob is active), return the coordination endpoint E2's index so the
/// receiver-block publish path can push the deterministic "receiver blocked"
/// signal. Returns `None` otherwise — a strict no-op on every endpoint except the
/// proof E1, and only under a sub-knob.
pub(crate) fn proof_send_plain_oracle_coordination_target(endpoint_idx: usize) -> Option<usize> {
    if !ipc_send_oracle_coordination_active() {
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

/// Stage 193B: provision the coordination endpoint E2 for the send-plain live
/// oracle, and grant init (TID 1) a RECEIVE cap to it. Returns the recv cap, which
/// the caller wires into init's startup slot 14 (`service_extra_cap_1`) WITH slot
/// 13 left empty — the presence pattern init uses to select the send-plain oracle.
/// Active ONLY when BOTH the base proof knob and the send-plain-oracle sub-knob are
/// set. Stores E2's index into the shared coordination-index static so the
/// receiver-block push hook can find it.
pub fn provision_init_ipc_send_plain_oracle_coord(
    kernel: &mut KernelState,
    init_tid: u64,
) -> Option<u32> {
    if !ipc_send_plain_oracle_active() {
        return None;
    }
    let (e2_idx, _send_root, recv_root) = match kernel.create_endpoint(8) {
        Ok(triple) => triple,
        Err(e) => {
            crate::yarm_log!(
                "IPC_SEND_PLAIN_ORACLE_COORD_FAIL step=create_endpoint err={:?}",
                e
            );
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
            crate::yarm_log!(
                "IPC_SEND_PLAIN_ORACLE_COORD_FAIL step=grant_recv err={:?}",
                e
            );
            return None;
        }
    };
    IPC_RECV_PROOF_SENDER_WAKE_E2_IDX.store(e2_idx, core::sync::atomic::Ordering::Release);
    crate::yarm_log!(
        "IPC_SEND_PLAIN_ORACLE_COORD_OK init_tid={} e1_idx={} e2_idx={} recv_cap={}",
        init_tid,
        IPC_RECV_PROOF_SENDER_WAKE_E1_IDX.load(core::sync::atomic::Ordering::Acquire),
        e2_idx,
        recv_cap.0
    );
    Some(recv_cap.0 as u32)
}

/// Stage 193C: provision the coordination endpoint for the ordinary cap-transfer
/// live oracle, and grant init (TID 1) a RECEIVE cap to it. Returns the recv cap,
/// which the caller wires into init's startup slot 13 (`service_extra_cap_0`) WITH
/// slot 14 left empty — the presence pattern init uses to select the cap oracle
/// (slot 13 only), distinct from the plain oracle (slot 14 only) and sender-wake
/// (slots 13 + 14). Active ONLY when BOTH the base proof knob and the send-cap
/// sub-knob are set. Stores the endpoint's index into the shared coordination-index
/// static so the receiver-block push hook can find it.
pub fn provision_init_ipc_send_cap_oracle_coord(
    kernel: &mut KernelState,
    init_tid: u64,
) -> Option<u32> {
    if !ipc_send_cap_oracle_active() {
        return None;
    }
    let (e2_idx, _send_root, recv_root) = match kernel.create_endpoint(8) {
        Ok(triple) => triple,
        Err(e) => {
            crate::yarm_log!(
                "IPC_SEND_CAP_ORACLE_COORD_FAIL step=create_endpoint err={:?}",
                e
            );
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
            crate::yarm_log!("IPC_SEND_CAP_ORACLE_COORD_FAIL step=grant_recv err={:?}", e);
            return None;
        }
    };
    IPC_RECV_PROOF_SENDER_WAKE_E2_IDX.store(e2_idx, core::sync::atomic::Ordering::Release);
    crate::yarm_log!(
        "IPC_SEND_CAP_ORACLE_COORD_OK init_tid={} e1_idx={} e2_idx={} recv_cap={}",
        init_tid,
        IPC_RECV_PROOF_SENDER_WAKE_E1_IDX.load(core::sync::atomic::Ordering::Acquire),
        e2_idx,
        recv_cap.0
    );
    Some(recv_cap.0 as u32)
}

/// Stage 198E3C1: the DIRECT shared-region oracle's fixed region size (two pages, so the live path
/// exercises multi-page mapping progress).
pub(crate) const SHARED_REGION_ORACLE_PAGES: usize = 2;

/// Stage 198E3C1B-H: the dedicated unmapped two-page oracle receive window VA, mirroring
/// `yarm_user_rt::syscall::SHARED_REGION_ORACLE_VA`. The authoritative blocked-recv acknowledgement
/// requires the receiver's `payload_user_ptr` to equal this exact VA (a wrong-VA recv is rejected).
#[cfg(feature = "x86-shared-region-direct-oracle")]
pub const SHARED_REGION_ORACLE_USER_VA: usize = 0x4000_0000;

/// Stage 198E3C1B: the init startup-slot-5 selector value that names the DIRECT shared-region
/// oracle (mirrors `yarm_user_rt::syscall::SHARED_REGION_ORACLE_SELECTOR`). Slot 5 is a mutually
/// exclusive selector: 1 = x86_64 FutexWake oracle, 2 = shared-region direct oracle. Only ONE may
/// be armed per boot (the boot caller fails closed if both knobs are set).
#[cfg(feature = "x86-shared-region-direct-oracle")]
pub const SHARED_REGION_ORACLE_SELECTOR: u64 = 2;

// ── Stage 198E3C1B-H: authoritative blocked-recv acknowledgement ─────────────────────────────────
// The pre-recv futex signal (child wakes parent BEFORE entering recv) is NOT proof that the child is
// a committed recv-v2 waiter: the wake precedes waiter registration, so a valid interleaving lets the
// parent send while the child is still runnable, taking the immediate/no-waiter path. The
// authoritative acknowledgement below is published by the RECEIVER's own recv path ONLY after the
// blocked-recv record is fully committed (endpoint waiter linked + task Blocked + BlockedRecvState
// payload/meta stored), and it records the exact committed identity so the send side (and hosted
// tests) can prove the direct blocked path is reachable ONLY for the expected receiver/endpoint/VA.
//
// It is oracle-only (feature-gated), reads authoritative committed state, and does NOT wake, mint a
// capability, copy user memory, add a kernel lock, or emit any retirement success.
#[cfg(feature = "x86-shared-region-direct-oracle")]
pub mod shared_region_blocked_recv {
    use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

    /// Complete authoritative snapshot of a fully-committed shared-region blocked recv-v2 waiter.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct SharedRegionBlockedRecvAck {
        pub receiver_tid: u64,
        /// ASID = the receiver's incarnation/generation discriminator.
        pub receiver_generation: u32,
        pub endpoint_idx: usize,
        pub endpoint_generation: u64,
        pub payload_va: usize,
        pub meta_ptr: usize,
        pub map_len: usize,
        pub recv_v2: bool,
        /// Monotonic commit sequence — a fresh publish always advances it, so a stale reader that
        /// cached an earlier ack can detect it changed. Duplicate consumption is rejected separately.
        pub commit_seq: u64,
    }

    static VALID: AtomicBool = AtomicBool::new(false);
    static CONSUMED: AtomicBool = AtomicBool::new(false);
    static SEQ: AtomicU64 = AtomicU64::new(0);
    static RECEIVER_TID: AtomicU64 = AtomicU64::new(0);
    static RECEIVER_GEN: AtomicU32 = AtomicU32::new(0);
    static ENDPOINT_IDX: AtomicUsize = AtomicUsize::new(usize::MAX);
    static ENDPOINT_GEN: AtomicU64 = AtomicU64::new(0);
    static PAYLOAD_VA: AtomicUsize = AtomicUsize::new(0);
    static META_PTR: AtomicUsize = AtomicUsize::new(0);
    static MAP_LEN: AtomicUsize = AtomicUsize::new(0);
    static RECV_V2: AtomicBool = AtomicBool::new(false);

    /// Reset the ack (test setup + between boots). Clears everything to the unpublished state.
    pub fn reset() {
        VALID.store(false, Ordering::Release);
        CONSUMED.store(false, Ordering::Release);
    }

    /// Publish the authoritative ack. Fields are stored first, then `VALID` is released last, so a
    /// reader that observes `VALID` via `snapshot()` sees a complete record (no torn read). The
    /// caller is the receiver's own recv path AFTER full commit; it must have already validated the
    /// oracle contract. Advances the commit sequence and clears the consumed flag (a fresh commit is
    /// independently consumable exactly once).
    pub(crate) fn publish(ack: SharedRegionBlockedRecvAck) {
        RECEIVER_TID.store(ack.receiver_tid, Ordering::Relaxed);
        RECEIVER_GEN.store(ack.receiver_generation, Ordering::Relaxed);
        ENDPOINT_IDX.store(ack.endpoint_idx, Ordering::Relaxed);
        ENDPOINT_GEN.store(ack.endpoint_generation, Ordering::Relaxed);
        PAYLOAD_VA.store(ack.payload_va, Ordering::Relaxed);
        META_PTR.store(ack.meta_ptr, Ordering::Relaxed);
        MAP_LEN.store(ack.map_len, Ordering::Relaxed);
        RECV_V2.store(ack.recv_v2, Ordering::Relaxed);
        SEQ.store(ack.commit_seq, Ordering::Relaxed);
        CONSUMED.store(false, Ordering::Relaxed);
        VALID.store(true, Ordering::Release);
    }

    /// Monotonic next commit sequence (for the publisher).
    pub(crate) fn next_commit_seq() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    /// Authoritative snapshot, or `None` if no ack has been committed since the last reset.
    pub fn snapshot() -> Option<SharedRegionBlockedRecvAck> {
        if !VALID.load(Ordering::Acquire) {
            return None;
        }
        Some(SharedRegionBlockedRecvAck {
            receiver_tid: RECEIVER_TID.load(Ordering::Relaxed),
            receiver_generation: RECEIVER_GEN.load(Ordering::Relaxed),
            endpoint_idx: ENDPOINT_IDX.load(Ordering::Relaxed),
            endpoint_generation: ENDPOINT_GEN.load(Ordering::Relaxed),
            payload_va: PAYLOAD_VA.load(Ordering::Relaxed),
            meta_ptr: META_PTR.load(Ordering::Relaxed),
            map_len: MAP_LEN.load(Ordering::Relaxed),
            recv_v2: RECV_V2.load(Ordering::Relaxed),
            commit_seq: SEQ.load(Ordering::Relaxed),
        })
    }

    /// Authoritative gate for the parent's DIRECT send: returns `true` ONLY if a committed,
    /// unconsumed ack matches the EXACT expected receiver identity, generation, endpoint identity,
    /// recv-v2 contract, payload VA, metadata pointer, and two-page map length. Rejects a wrong
    /// receiver / stale generation / wrong endpoint / wrong VA / wrong meta ptr / plain (non-v2)
    /// waiter / absent ack. Does NOT consume — see `consume_if_matches`.
    #[allow(clippy::too_many_arguments)]
    pub fn matches(
        receiver_tid: u64,
        receiver_generation: u32,
        endpoint_idx: usize,
        endpoint_generation: u64,
        payload_va: usize,
        meta_ptr: usize,
        map_len: usize,
    ) -> bool {
        matches!(
            snapshot(),
            Some(a) if !CONSUMED.load(Ordering::Acquire)
                && a.recv_v2
                && a.receiver_tid == receiver_tid
                && a.receiver_generation == receiver_generation
                && a.endpoint_idx == endpoint_idx
                && a.endpoint_generation == endpoint_generation
                && a.payload_va == payload_va
                && a.meta_ptr == meta_ptr
                && a.map_len == map_len
        )
    }

    /// Consume the ack exactly once if it matches — a DUPLICATE consume (already consumed) returns
    /// `false`, so a duplicate parent send cannot re-satisfy the gate on a stale ack.
    #[allow(clippy::too_many_arguments)]
    pub fn consume_if_matches(
        receiver_tid: u64,
        receiver_generation: u32,
        endpoint_idx: usize,
        endpoint_generation: u64,
        payload_va: usize,
        meta_ptr: usize,
        map_len: usize,
    ) -> bool {
        if !matches(
            receiver_tid,
            receiver_generation,
            endpoint_idx,
            endpoint_generation,
            payload_va,
            meta_ptr,
            map_len,
        ) {
            return false;
        }
        // Claim exactly once.
        CONSUMED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Send-side authoritative gate for the DIRECT blocked-recv delivery: consume the ack exactly
    /// once iff a committed, unconsumed, recv-v2, oracle-VA ack exists for THIS receiver + endpoint.
    /// A wrong receiver, wrong endpoint, non-oracle VA, plain (non-v2) waiter, absent ack, or an
    /// already-consumed (duplicate) ack all return `false`, so the direct path declines and no
    /// duplicate send can re-satisfy the gate.
    pub(crate) fn consume_for_delivery(receiver_tid: u64, endpoint_idx: usize) -> bool {
        let matches_now = matches!(
            snapshot(),
            Some(a) if !CONSUMED.load(Ordering::Acquire)
                && a.recv_v2
                && a.receiver_tid == receiver_tid
                && a.endpoint_idx == endpoint_idx
                && a.payload_va == super::SHARED_REGION_ORACLE_USER_VA
        );
        if !matches_now {
            return false;
        }
        CONSUMED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

/// Stage 198E3C1B-H: publish the authoritative blocked-recv acknowledgement from the RECEIVER's own
/// recv path, called ONLY after the blocked-recv record is fully committed (endpoint waiter linked,
/// task Blocked, `BlockedRecvState` payload/meta stored). Feature + runtime-knob gated; a strict
/// no-op otherwise. It re-reads authoritative committed state (the endpoint waiter identity must
/// equal this receiver) and validates the oracle contract (recv-v2, payload VA = the oracle window,
/// two-page length, non-null metadata pointer) before recording the ack. It does NOT wake, mint,
/// copy user memory, add a lock, or emit any retirement marker.
#[cfg(feature = "x86-shared-region-direct-oracle")]
pub(crate) fn maybe_publish_shared_region_blocked_recv_ack(
    kernel: &KernelState,
    receiver_tid: u64,
    endpoint: crate::kernel::capabilities::CapObject,
    state: &crate::kernel::task::BlockedRecvState,
) {
    use crate::kernel::capabilities::CapObject;
    if !x86_shared_region_direct_oracle_enabled() {
        return;
    }
    // Oracle contract: recv-v2, the dedicated two-page oracle window, a real metadata buffer.
    let two_pages = SHARED_REGION_ORACLE_PAGES * crate::kernel::vm::PAGE_SIZE;
    let recv_v2 = state.recv_abi == crate::kernel::task::RecvAbiVariant::RecvV2;
    let CapObject::Endpoint { index, generation } = endpoint else {
        return;
    };
    if !recv_v2
        || state.payload_user_ptr != SHARED_REGION_ORACLE_USER_VA
        || state.payload_user_len < two_pages
        || state.meta_user_ptr == 0
    {
        return;
    }
    // Authoritative commit check: the endpoint waiter slot must already hold THIS receiver (the
    // Phase-C publish committed it before BlockedRecvState was stored). If it does not, the record is
    // not fully committed for this endpoint — do not acknowledge.
    let waiter = kernel.with_ipc_state(|ipc| ipc.endpoint_waiter_identity(index));
    let receiver_generation = kernel
        .task_asid(receiver_tid)
        .map(|a| a.0 as u32)
        .unwrap_or(0);
    match waiter {
        Some(w) if w.tid.0 == receiver_tid && w.asid.0 as u32 == receiver_generation => {}
        _ => return,
    }
    let ack = shared_region_blocked_recv::SharedRegionBlockedRecvAck {
        receiver_tid,
        receiver_generation,
        endpoint_idx: index,
        endpoint_generation: generation,
        payload_va: state.payload_user_ptr,
        meta_ptr: state.meta_user_ptr,
        map_len: two_pages,
        recv_v2: true,
        commit_seq: shared_region_blocked_recv::next_commit_seq(),
    };
    shared_region_blocked_recv::publish(ack);
    crate::yarm_log!(
        "SHARED_REGION_BLOCKED_RECV_ACK tid={} gen={} endpoint={} ep_gen={} payload_va=0x{:x} meta_ptr=0x{:x} map_len={} recv_v2=1 seq={}",
        ack.receiver_tid,
        ack.receiver_generation,
        ack.endpoint_idx,
        ack.endpoint_generation,
        ack.payload_va,
        ack.meta_ptr,
        ack.map_len,
        ack.commit_seq
    );
}

/// Stage 198E3C1: the deterministic known byte the oracle writes at object offset `off`. It varies
/// with `off`, so the pattern SPANS BOTH pages (page 0 and page 1 hold different bytes), and it is a
/// pure formula the userspace child recomputes to validate the mapped contents. No secret, no ABI.
pub(crate) const fn shared_region_oracle_pattern_byte(off: usize) -> u8 {
    // Vary with BOTH the in-page offset and the page index (`off >> 12`) so consecutive pages hold
    // distinct bytes at the same in-page offset — the pattern genuinely spans both pages.
    (off as u8)
        .wrapping_add((off >> 12) as u8)
        .wrapping_add(0x5A)
}

/// Stage 198E3C1B: the two init-local caps a fully-provisioned shared-region oracle hands to init.
///
/// The report's original three-slot plan (`mem_cap` / `send_cap` / `recv_cap` in startup slots
/// 13/14/15) does NOT fit: startup slot 15 is `STARTUP_SLOT_INITRD_PTR` (already occupied by the
/// boot initrd pointer), leaving only the two free service-extra slots 13/14. So the send and recv
/// authorities collapse into ONE endpoint cap carrying `SEND | RECEIVE`: the parent sends on it and
/// the forked child receives on the SAME cap through init's shared CSpace. Two caps, two slots.
#[cfg(feature = "x86-shared-region-direct-oracle")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedRegionOracleCaps {
    /// init-local `MemoryObject` source cap (`READ | MAP`, no `WRITE`/execute) → startup slot 13.
    pub mem_cap: u32,
    /// init-local endpoint cap carrying `SEND | RECEIVE` for the oracle rendezvous → startup slot 14.
    pub endpoint_cap: u32,
    /// The endpoint's slot index (for the drain/waiter bookkeeping and hosted assertions).
    pub endpoint_idx: usize,
}

/// Stage 198E3C1B: TEST-ONLY fault-injection selector for the provisioning transaction. Names the
/// step at which provisioning is forced to fail, so hosted tests can assert the rollback leaves NO
/// leaked cap, NO leaked object, and NO occupied startup slot at EVERY failure point. Defaults to 0
/// (no injection) so a real build's provisioning is byte-identical.
#[cfg(feature = "x86-shared-region-direct-oracle")]
pub(crate) static SHARED_REGION_ORACLE_FAULT_INJECT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

/// Fault-injection step codes (0 = none). Each names the step FORCED to fail; rollback must then
/// reclaim everything created by the strictly-earlier steps.
#[cfg(feature = "x86-shared-region-direct-oracle")]
pub(crate) mod shared_region_fault {
    pub const NONE: u32 = 0;
    pub const ALLOC: u32 = 1;
    pub const RESOLVE: u32 = 2;
    pub const GRANT_MEM: u32 = 3;
    pub const CREATE_EP: u32 = 4;
    pub const RESOLVE_EP: u32 = 5;
    pub const MINT_EP: u32 = 6;
    /// Highest injectable step (inclusive) — hosted tests sweep `1..=LAST`.
    pub const LAST: u32 = MINT_EP;
}

#[cfg(feature = "x86-shared-region-direct-oracle")]
pub(crate) fn set_shared_region_oracle_fault_inject(step: u32) {
    SHARED_REGION_ORACLE_FAULT_INJECT.store(step, core::sync::atomic::Ordering::Release);
}

#[cfg(feature = "x86-shared-region-direct-oracle")]
fn shared_region_oracle_fault_inject() -> u32 {
    SHARED_REGION_ORACLE_FAULT_INJECT.load(core::sync::atomic::Ordering::Acquire)
}

/// Rollback scratch: every reclaimable resource the transaction has created so far, in creation
/// order. Rollback undoes them in REVERSE order.
#[cfg(feature = "x86-shared-region-direct-oracle")]
#[derive(Default, Clone, Copy)]
struct SharedRegionProvisionScratch {
    /// Source `MemoryObject` cap in task 0's CNode — revoking it cascades to init's delegated mem
    /// cap and reclaims the object (refcount → 0).
    obj_src_cap: Option<crate::kernel::capabilities::CapId>,
    /// Endpoint SEND/RECEIVE root caps in task 0's CNode (from `create_endpoint`).
    ep_send_root: Option<crate::kernel::capabilities::CapId>,
    ep_recv_root: Option<crate::kernel::capabilities::CapId>,
    /// The endpoint object slot.
    endpoint_idx: Option<usize>,
    /// The `SEND | RECEIVE` cap minted into init's CNode.
    init_ep_cap: Option<crate::kernel::capabilities::CapId>,
}

/// Undo a partial provisioning transaction. Idempotent and total: every present resource is
/// reclaimed (best-effort `let _`), leaving NO leaked cap, NO leaked object, and NO startup slot
/// written (the caller only writes slots on a `Some` return).
#[cfg(feature = "x86-shared-region-direct-oracle")]
fn rollback_shared_region_provision(
    kernel: &mut KernelState,
    init_tid: u64,
    scratch: &SharedRegionProvisionScratch,
) {
    // 1. init-local endpoint cap.
    if let (Some(cap), Some(cnode)) = (scratch.init_ep_cap, kernel.task_cnode(init_tid)) {
        let _ = kernel.revoke_capability_in_cnode(cnode, cap);
    }
    // 2. endpoint root caps in task 0's CNode.
    if let Some(cnode) = kernel.task_cnode(0) {
        for root in [scratch.ep_send_root, scratch.ep_recv_root]
            .into_iter()
            .flatten()
        {
            let _ = kernel.revoke_capability_in_cnode(cnode, root);
        }
    }
    // 3. endpoint object.
    if let Some(eidx) = scratch.endpoint_idx {
        let _ = kernel.destroy_endpoint(eidx);
    }
    // 4. source MemoryObject cap (cascades to init's delegated mem cap + reclaims the object).
    if let (Some(cap), Some(cnode)) = (scratch.obj_src_cap, kernel.task_cnode(0)) {
        let _ = kernel.revoke_capability_in_cnode(cnode, cap);
    }
}

/// Stage 198E3C1B: provision the DIRECT shared-region live oracle TRANSACTIONALLY. Under BOTH the
/// compile-time feature and the runtime selector, it (1) allocates ONE fresh two-page
/// `MemoryObject`, fills both backing pages with the deterministic pattern (kernel direct map — NO
/// userspace WRITE cap is minted for init), (2) grants init a `READ | MAP` (no `WRITE`/execute)
/// source cap with canonical transfer authority, (3) creates the rendezvous endpoint, and (4) mints
/// a `SEND | RECEIVE` endpoint cap into init's CNode. Returns `SharedRegionOracleCaps` for the
/// startup slots. Fail-closed AND leak-free: on ANY step failure it emits a precise failure marker,
/// rolls back every resource created so far (no leaked cap / object / occupied slot), and returns
/// `None` (the caller then leaves the oracle un-armed) — it never partially arms.
#[cfg(feature = "x86-shared-region-direct-oracle")]
pub fn provision_init_shared_region_oracle(
    kernel: &mut KernelState,
    init_tid: u64,
) -> Option<SharedRegionOracleCaps> {
    if !x86_shared_region_direct_oracle_enabled() {
        return None;
    }
    use crate::kernel::capabilities::{CapObject, CapRights, Capability};
    let inject = shared_region_oracle_fault_inject();
    let len = SHARED_REGION_ORACLE_PAGES * crate::kernel::vm::PAGE_SIZE;
    let mut scratch = SharedRegionProvisionScratch::default();

    // Step 1: allocate the source object.
    if inject == shared_region_fault::ALLOC {
        crate::yarm_log!("SHARED_REGION_ORACLE_PROVISION_FAIL step=alloc err=Injected");
        rollback_shared_region_provision(kernel, init_tid, &scratch);
        return None;
    }
    let (obj_id, src_cap0) = match kernel.alloc_anonymous_memory_object_with_len(len) {
        Ok(t) => t,
        Err(e) => {
            crate::yarm_log!("SHARED_REGION_ORACLE_PROVISION_FAIL step=alloc err={:?}", e);
            return None;
        }
    };
    scratch.obj_src_cap = Some(src_cap0);

    // Step 2: resolve the object handle.
    if inject == shared_region_fault::RESOLVE {
        crate::yarm_log!("SHARED_REGION_ORACLE_PROVISION_FAIL step=resolve err=Injected");
        rollback_shared_region_provision(kernel, init_tid, &scratch);
        return None;
    }
    let _object = match kernel.current_task_capability(src_cap0) {
        Some(c) => c.object,
        None => {
            crate::yarm_log!(
                "SHARED_REGION_ORACLE_PROVISION_FAIL step=resolve obj_id={}",
                obj_id
            );
            rollback_shared_region_provision(kernel, init_tid, &scratch);
            return None;
        }
    };
    // Initialize both pages with the deterministic pattern through the kernel direct map (bare-metal
    // only; a normal boot never reaches this, and no userspace WRITE authority is created).
    #[cfg(not(feature = "hosted-dev"))]
    {
        let phys = match kernel
            .with_memory_state(|m| KernelState::shared_region_phys_base_locked(m, _object))
        {
            Some(p) => p.0,
            None => {
                crate::yarm_log!(
                    "SHARED_REGION_ORACLE_PROVISION_FAIL step=phys obj_id={}",
                    obj_id
                );
                rollback_shared_region_provision(kernel, init_tid, &scratch);
                return None;
            }
        };
        for off in 0..len {
            match KernelState::phys_to_direct_map_ptr(phys + off as u64) {
                Some(ptr) => unsafe {
                    core::ptr::write_volatile(ptr, shared_region_oracle_pattern_byte(off));
                },
                None => {
                    crate::yarm_log!("SHARED_REGION_ORACLE_PROVISION_FAIL step=fill off={}", off);
                    rollback_shared_region_provision(kernel, init_tid, &scratch);
                    return None;
                }
            }
        }
    }

    // Step 3: grant init the read-only source cap (READ | MAP; canonical transfer authority).
    if inject == shared_region_fault::GRANT_MEM {
        crate::yarm_log!("SHARED_REGION_ORACLE_PROVISION_FAIL step=grant err=Injected");
        rollback_shared_region_provision(kernel, init_tid, &scratch);
        return None;
    }
    let init_mem_cap = match kernel.grant_capability_task_to_task_with_rights(
        0,
        src_cap0,
        init_tid,
        CapRights::READ | CapRights::MAP,
    ) {
        Ok(c) => c,
        Err(e) => {
            crate::yarm_log!("SHARED_REGION_ORACLE_PROVISION_FAIL step=grant err={:?}", e);
            rollback_shared_region_provision(kernel, init_tid, &scratch);
            return None;
        }
    };

    // Step 4: create the rendezvous endpoint.
    if inject == shared_region_fault::CREATE_EP {
        crate::yarm_log!("SHARED_REGION_ORACLE_PROVISION_FAIL step=create_ep err=Injected");
        rollback_shared_region_provision(kernel, init_tid, &scratch);
        return None;
    }
    let (endpoint_idx, ep_send_root, ep_recv_root) = match kernel.create_endpoint(8) {
        Ok(t) => t,
        Err(e) => {
            crate::yarm_log!(
                "SHARED_REGION_ORACLE_PROVISION_FAIL step=create_ep err={:?}",
                e
            );
            rollback_shared_region_provision(kernel, init_tid, &scratch);
            return None;
        }
    };
    scratch.ep_send_root = Some(ep_send_root);
    scratch.ep_recv_root = Some(ep_recv_root);
    scratch.endpoint_idx = Some(endpoint_idx);

    // Step 5: resolve the endpoint object (for its generation) so the combined cap is well-formed.
    if inject == shared_region_fault::RESOLVE_EP {
        crate::yarm_log!("SHARED_REGION_ORACLE_PROVISION_FAIL step=resolve_ep err=Injected");
        rollback_shared_region_provision(kernel, init_tid, &scratch);
        return None;
    }
    let ep_object = match kernel.current_task_capability(ep_recv_root) {
        Some(c) => c.object,
        None => {
            crate::yarm_log!(
                "SHARED_REGION_ORACLE_PROVISION_FAIL step=resolve_ep eidx={}",
                endpoint_idx
            );
            rollback_shared_region_provision(kernel, init_tid, &scratch);
            return None;
        }
    };
    debug_assert!(matches!(ep_object, CapObject::Endpoint { .. }));

    // Step 6: mint the SEND | RECEIVE endpoint cap into init's CNode.
    if inject == shared_region_fault::MINT_EP {
        crate::yarm_log!("SHARED_REGION_ORACLE_PROVISION_FAIL step=mint_ep err=Injected");
        rollback_shared_region_provision(kernel, init_tid, &scratch);
        return None;
    }
    let init_cnode = match kernel.task_cnode(init_tid) {
        Some(c) => c,
        None => {
            crate::yarm_log!("SHARED_REGION_ORACLE_PROVISION_FAIL step=init_cnode");
            rollback_shared_region_provision(kernel, init_tid, &scratch);
            return None;
        }
    };
    let init_ep_cap = match kernel.mint_capability_in_cnode(
        init_cnode,
        Capability::new(ep_object, CapRights::SEND | CapRights::RECEIVE),
    ) {
        Ok(c) => c,
        Err(e) => {
            crate::yarm_log!(
                "SHARED_REGION_ORACLE_PROVISION_FAIL step=mint_ep err={:?}",
                e
            );
            rollback_shared_region_provision(kernel, init_tid, &scratch);
            return None;
        }
    };
    scratch.init_ep_cap = Some(init_ep_cap);

    crate::yarm_log!(
        "SHARED_REGION_ORACLE_PROVISION_OK init_tid={} obj_id={} pages={} mem_cap={} endpoint_cap={} eidx={}",
        init_tid,
        obj_id,
        SHARED_REGION_ORACLE_PAGES,
        init_mem_cap.0,
        init_ep_cap.0,
        endpoint_idx
    );
    Some(SharedRegionOracleCaps {
        mem_cap: init_mem_cap.0 as u32,
        endpoint_cap: init_ep_cap.0 as u32,
        endpoint_idx,
    })
}

/// Stage 193D: provision the reply-cap live oracle. Under BOTH the base proof knob and
/// the send-reply-cap sub-knob, this (a) creates the coordination endpoint + grants init
/// a RECV cap (slot 13), and (b) mints a transferable one-shot Reply cap directly into
/// init's cnode (slot 14) via the EXISTING `create_reply_cap_for_caller_in_cnode` seam —
/// so init can transfer it to the recv-v2-blocked child, exercising the 193D reply-cap
/// boundary split. Returns `(coord_recv_cap, reply_cap)`. The reply cap's reply endpoint
/// is a fresh endpoint whose RECV cap stays with task 0 (the synthetic caller); the
/// oracle only needs the fresh receiver-local reply cap to be materialized + observed,
/// not actually replied through.
pub fn provision_init_ipc_send_reply_cap_oracle(
    kernel: &mut KernelState,
    init_tid: u64,
) -> Option<(u32, u32, u32)> {
    if !ipc_send_reply_cap_oracle_active() {
        return None;
    }
    // (a) Coordination endpoint (init RECV cap → slot 13).
    let (e2_idx, _e2_send, e2_recv_root) = match kernel.create_endpoint(8) {
        Ok(t) => t,
        Err(e) => {
            crate::yarm_log!(
                "IPC_SEND_REPLY_CAP_ORACLE_FAIL step=create_coord err={:?}",
                e
            );
            return None;
        }
    };
    let coord_recv = match kernel.grant_capability_task_to_task_with_rights(
        0,
        e2_recv_root,
        init_tid,
        crate::kernel::capabilities::CapRights::RECEIVE,
    ) {
        Ok(c) => c,
        Err(e) => {
            crate::yarm_log!(
                "IPC_SEND_REPLY_CAP_ORACLE_FAIL step=grant_coord err={:?}",
                e
            );
            return None;
        }
    };
    IPC_RECV_PROOF_SENDER_WAKE_E2_IDX.store(e2_idx, core::sync::atomic::Ordering::Release);

    // (b) Reply endpoint + a transferable Reply cap minted DIRECTLY into init's cnode.
    //
    // Stage 198C2B (ONE-SHOT LIVE): init is the REAL wakeable caller (not a synthetic
    // task-0 caller). init is granted the reply endpoint RECV cap (returned as the third
    // value → slot 17, which also serves as the reply-cap-oracle discriminator: a real
    // non-zero cap is `Some`), so after transferring the reply cap init blocks on this
    // endpoint in the canonical waiting-for-reply state and the receiving child's
    // `ipc_reply` wakes it exactly once. `responder = None` so the child (a distinct TID)
    // is permitted to invoke the transferred reply cap — the record is bound to the
    // caller, NOT to a responder identity that would reject the child. Caller = init so
    // the reply is delivered back to init's reply endpoint on invocation.
    let (_reply_eidx, _reply_send, reply_recv_root) = match kernel.create_endpoint(2) {
        Ok(t) => t,
        Err(e) => {
            crate::yarm_log!(
                "IPC_SEND_REPLY_CAP_ORACLE_FAIL step=create_reply_ep err={:?}",
                e
            );
            return None;
        }
    };
    let init_cnode = match kernel.task_cnode(init_tid) {
        Some(c) => c,
        None => {
            crate::yarm_log!("IPC_SEND_REPLY_CAP_ORACLE_FAIL step=init_cnode");
            return None;
        }
    };
    // Grant init the reply endpoint RECV cap so init can be the wakeable caller.
    let reply_recv_init = match kernel.grant_capability_task_to_task_with_rights(
        0,
        reply_recv_root,
        init_tid,
        crate::kernel::capabilities::CapRights::RECEIVE,
    ) {
        Ok(c) => c,
        Err(e) => {
            crate::yarm_log!(
                "IPC_SEND_REPLY_CAP_ORACLE_FAIL step=grant_reply_recv err={:?}",
                e
            );
            return None;
        }
    };
    // caller = init (holds the reply endpoint RECV cap, the wakeable caller); responder =
    // None (the receiving child may invoke); mint the one-shot Reply cap into init's cnode
    // so init can transfer it.
    let reply_cap = match kernel.create_reply_cap_for_caller_in_cnode(
        crate::kernel::ipc::ThreadId(init_tid),
        reply_recv_init,
        None,
        Some(init_cnode),
    ) {
        Ok(c) => c,
        Err(e) => {
            crate::yarm_log!(
                "IPC_SEND_REPLY_CAP_ORACLE_FAIL step=mint_reply_cap err={:?}",
                e
            );
            return None;
        }
    };
    crate::yarm_log!(
        "IPC_SEND_REPLY_CAP_ORACLE_PROVISION_OK init_tid={} e1_idx={} e2_idx={} coord_recv={} reply_cap={} reply_recv={} responder=none",
        init_tid,
        IPC_RECV_PROOF_SENDER_WAKE_E1_IDX.load(core::sync::atomic::Ordering::Acquire),
        e2_idx,
        coord_recv.0,
        reply_cap.0,
        reply_recv_init.0
    );
    Some((
        coord_recv.0 as u32,
        reply_cap.0 as u32,
        reply_recv_init.0 as u32,
    ))
}

/// Stage 193B: push the deterministic "receiver blocked on E1" coordination
/// message into the coordination endpoint E2. Called from the receiver-waiter
/// publish path (`publish_recv_waiter_live`) which already holds `ipc_state_lock`,
/// so E2's queue — in the same IPC domain — is mutated within the SAME critical
/// section as the waiter publish, making "E2 has the signal" an atomic proxy for
/// "a receiver is a waiter on E1". No scheduler/cap/user-copy work is done here
/// (init non-blocking-polls E2, so no wake is needed) → no lock-order hazard.
/// Best-effort: a full E2 queue (already signalled) is harmless.
pub(crate) fn proof_send_plain_oracle_push_coordination_locked(
    ipc: &mut defs::IpcSubsystem,
    e2_idx: usize,
    waiter_tid: u64,
) {
    if let Some(Some(endpoint_storage)) = ipc.endpoints.get_mut(e2_idx) {
        let endpoint = defs::kernel_mut(endpoint_storage);
        if let Ok(msg) = Message::with_header(waiter_tid, 0, 0, None, &[0xB3u8]) {
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

// Stage 187B: re-export the cap-transfer seam value types so the recv delivery
// boundary (runtime.rs, post-`with_cpu`) can build a snapshot and call the
// 186D2/186D3 seam. The seam *methods* on `SharedKernel` are already
// `pub(crate)`; these re-exports only surface the by-value input/output types.
pub(crate) use cap_transfer_delegation_split::TransferCapDelegation;
pub(crate) use cap_transfer_materialize_split::{
    CapTransferMaterializeOutcome, TransferCapSnapshot,
};

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
