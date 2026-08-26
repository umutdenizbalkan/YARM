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
// U9-QA §1: the queue-advance apply convention is named by callers outside this module (the
// pre-lock split route), so it is re-exported alongside the transaction it parameterises.
#[cfg_attr(feature = "hosted-dev", allow(unused_imports))]
pub(crate) use exec_state::{QueueAdvanceApply, QueueAdvanceOutcome};
mod fault_endpoint_state;
mod fault_state;
// U9-FT2 §2: the ONE PageFault classification evaluator and its named fact types are
// re-exported so the OFF-LOCK `SharedKernel::classify_page_fault_shared` twin in
// `crate::runtime` can call the SAME evaluator the broad form calls, rather than
// re-deriving the policy against the split seams.
#[cfg_attr(feature = "hosted-dev", allow(unused_imports))]
pub(crate) use fault_state::{
    BufferedFaultAdmission, BufferedFaultCommit, FaultReportTarget, PageFaultClass, PageFaultFacts,
    SUPERVISOR_FAULT_REPORT_WIRE_LEN, SupervisorFaultReportWire, TerminalFaultPolicyRefusal,
    TerminalFaultPolicySnapshot, TerminalFaultTransition, evaluate_cow_marked,
    evaluate_demand_backed_region, evaluate_fault_policy, evaluate_fault_report_route,
    evaluate_page_fault_class, page_fault_addr_is_kernel_space,
};
mod ipc_state;
// Stage 200C2B: the reply-timeout completion transaction abstraction + single generic
// body are re-exported so the OFF-LOCK drain in `crate::runtime` can run the SAME body
// through the `SharedKernel` split-mut seams (no duplicated transaction).
pub(crate) use ipc_state::{
    BlockingSendProducerOutcome, DetachOutcome, ReplyTimeoutDomains, complete_reply_timeout_over,
    complete_server_death_over,
};
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
/// Stage 199D-WA2A-R1 — helper-only endpoint-waiter ownership primitive, private to the
/// boot/IPC domain (zero production callers).
mod waiter_ownership;

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

/// The length of the authoritative endpoint receive-waiter table
/// ([`IpcSubsystem::endpoint_waiters`]), and the single place that length is named.
///
/// This is the **structural bound** on simultaneously outstanding direct-IPC acknowledgement
/// leases: a lease exists exactly while an endpoint receive-waiter does, so there can never be
/// more of them than there are waiter slots.
/// [`crate::kernel::direct_ack_store::DIRECT_ACK_STORE_CAPACITY`] is defined as this constant,
/// and a compile-time assertion in that module pins the relationship — so the acknowledgement
/// store cannot be under-sized by editing one of them.
pub(crate) const ENDPOINT_WAITER_SLOTS: usize = MAX_ENDPOINTS;

#[cfg(feature = "hosted-dev")]
pub(crate) const MAX_ENDPOINT_SENDER_WAITERS: usize = 8;
#[cfg(not(feature = "hosted-dev"))]
pub(crate) const MAX_ENDPOINT_SENDER_WAITERS: usize = 4;

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
    ReceivedWithSenderWake(Message, crate::kernel::ipc::SenderWakeTarget),
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
    WakeSender(crate::kernel::ipc::SenderWakeTarget),
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

    /// U6 §5 — `true` iff an item is already stashed.
    ///
    /// A producer checks this before storing so a publication can never displace another
    /// handler's post-work (the stash holds exactly one item, and `store` would overwrite).
    ///
    /// # Safety
    /// Caller must ensure no concurrent access (interrupts disabled, single CPU).
    pub(crate) unsafe fn is_occupied(&self) -> bool {
        unsafe { (*self.inner.get()).is_some() }
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

/// U4 — the CANONICAL production predicate for **queue-advancing blocking dispatch**: may this
/// architecture run the authoritative `dispatch_next_on` for a blocking IpcRecv / IpcSend
/// OUTSIDE the broad `SpinLock<KernelState>`?
///
/// This is deliberately a DIFFERENT question from [`d6_genuine_enabled`], which stays
/// x86_64-only and byte-identical. That predicate gates the queue-NEUTRAL D6 observation slice
/// and three `exec_state` decisions whose x86_64 semantics must not change; widening it would
/// silently alter AArch64 FutexWait/Yield behavior and x86-specific `exec_state` paths. U4
/// widens only the queue-ADVANCING blocking-dispatch question, which is what the D2 recv/send
/// deferral protocol actually asks.
///
/// Semantics:
///
/// * **true on x86_64, AArch64 and RISC-V** in an ordinary production build. On x86_64 the
///   result is byte-equivalent to the accepted `d6_genuine_enabled()` it replaces at the D2
///   sites: both reduce to "no D6 switch diagnostic owns the switch path".
/// * **false while a mutually exclusive controlled D6 diagnostic owns the switch path**
///   (`d6_switch_proof` / `d6_switch_a`), on every architecture — those category-D knobs are
///   x86_64-only in practice, and reading them here keeps the exclusion uniform rather than
///   arch-conditional.
/// * **no runtime fallback or opt-out knob** — there is no production route back to the old
///   in-lock queue-advancing dispatch on an eligible path.
/// * **no dependency on direct-IpcCall admission.** It deliberately does NOT go through
///   [`offlock_authoritative_dispatch_enabled`], whose AArch64 arm is
///   `ipccall_direct_admission_enabled()`: that belongs to the proof-gated direct NR6/NR7
///   transaction, and ordinary blocking IpcRecv/IpcSend must not become coupled to direct
///   production. `ipccall_direct_production_enabled()` remains an unconditional `false` and
///   cannot influence this predicate.
/// * **no dependency on waiter ownership.**
///
/// The remaining per-CPU eligibility (a trap-entry drainer is active, and this is the single
/// dispatching CPU) is unchanged and still applied at each publication site.
pub(crate) fn queue_advancing_dispatch_enabled() -> bool {
    !d6_controlled_switch_proof_enabled() && !d6_switch_a_enabled()
}

/// Stage 199D — the CANONICAL replacement for [`d6_genuine_enabled`] as the "may this
/// architecture run the authoritative queue-advancing dispatch outside the broad lock?"
/// question, now that AArch64 readiness blocker 3 is structurally closed.
///
/// `d6_genuine_enabled` deliberately stays byte-identical and x86_64-only: it gates the D6
/// queue-NEUTRAL dispatch slice and three `exec_state` decisions whose x86_64 semantics must
/// not change, and widening it would silently alter AArch64 FutexWait/Yield behaviour. This
/// predicate is the wider question, answered per architecture:
///
/// * **x86_64** — unchanged: exactly `d6_genuine_enabled()`.
/// * **AArch64** — admitted, but only through the same `ipccall_direct_admission_enabled()`
///   the direct NR6/NR7 path already uses, and only when no D6-switch diagnostic owns the
///   switch path. Since Stage 199D-WA1-GATE `ipccall_direct_production_enabled()` is `false` on
///   every architecture, so that resolves to the armed proof/oracle gate everywhere: **the
///   AArch64 production default is OFF** — as is x86_64's — and an ordinary boot on either
///   publishes no work item and drains nothing.
/// * **RISC-V** — not admitted (no RISC-V work in this increment).
// The only production caller is the AArch64 publication site; a hosted (x86_64) `lib` build
// compiles no route to it, exactly like the sibling arch-gated predicates.
#[allow(dead_code)]
pub(crate) fn offlock_authoritative_dispatch_enabled() -> bool {
    if cfg!(target_arch = "aarch64") {
        return ipccall_direct_admission_enabled()
            && !d6_controlled_switch_proof_enabled()
            && !d6_switch_a_enabled();
    }
    d6_genuine_enabled()
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
/// U4: delegates to the canonical [`queue_advancing_dispatch_enabled`], which admits AArch64
/// and RISC-V as well. On x86_64 the value is unchanged — both reduce to "no D6 switch
/// diagnostic owns the switch path".
pub(crate) fn d2_recv_genuine_enabled() -> bool {
    queue_advancing_dispatch_enabled()
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

// ─── Canonical 199E-R1D: the RISC-V ASYNC-PREEMPTED resume decision ───────────────────
//
// The sixth member of the `YIELD_DISPATCH_*` / `FUTEX_WAIT_*` / `D2_RECV_*` per-CPU flag family,
// and exactly as narrow. It carries ONE bit from the phase that knows the answer to the phase
// that needs it.
//
// The classification happens where the TCB is reachable — the in-lock restore
// (`restore_arch_thread_state`) and the post-lock switch drains
// (`direct_dispatch_resume_incoming`) — because only there can the tag's exact `{tid, asid,
// generation}` incarnation be validated and consumed. The DECISION is needed later and
// elsewhere: in the RISC-V trap bridge's register write-back, which owns the choice between
// "install the startup/result ABI lanes" and "restore a0..a7 verbatim", and which holds only a
// `SharedKernel` rather than a `&mut KernelState`.
//
// Routing it through this flag rather than a new broad-lock acquisition is what keeps the
// Stage 204A census delta at zero, and it reuses a pattern this port already relies on for five
// other cross-phase decisions.
//
// The flag is set only when a tag was actually CONSUMED, so it cannot be observed without a
// matching one-shot authorization, and the bridge takes it (clearing it) so a single snapshot
// authorizes exactly one verbatim restore.
pub(crate) static RISCV_ASYNC_RESUME_PENDING: [core::sync::atomic::AtomicBool;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; crate::kernel::scheduler::MAX_CPUS];

/// Canonical 199E-R1D: record that the task about to be resumed on `cpu_idx` had a VALID
/// async-preemption tag, which the caller has already consumed.
pub(crate) fn riscv_async_resume_publish(cpu_idx: usize) {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return;
    }
    RISCV_ASYNC_RESUME_PENDING[cpu_idx].store(true, core::sync::atomic::Ordering::Release);
}

/// Canonical 199E-R1D: TAKE the async-resume decision for `cpu_idx`, clearing it.
///
/// Take-once by construction: the bridge's write-back is the sole consumer, so a decision can
/// never survive into a later, unrelated trap and silently suppress a startup-argument install.
pub(crate) fn riscv_async_resume_take(cpu_idx: usize) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    RISCV_ASYNC_RESUME_PENDING[cpu_idx].swap(false, core::sync::atomic::Ordering::AcqRel)
}

/// Canonical 199E-R1D: clear any stale async-resume decision for `cpu_idx` without consuming
/// it as an authorization.
///
/// Called at the START of every RISC-V trap, so a decision published on a trap that then
/// diverged (fatal halt, idle) cannot be inherited by the next one.
pub(crate) fn riscv_async_resume_clear(cpu_idx: usize) {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return;
    }
    RISCV_ASYNC_RESUME_PENDING[cpu_idx].store(false, core::sync::atomic::Ordering::Release);
}

// ─── Canonical 199E-R2: the SYSCALL-CONTINUATION resume decision (RISC-V) ────────────────────
//
// The exact sibling of `RISCV_ASYNC_RESUME_PENDING`, and it exists for the same reason: a
// write-back must choose ONE of three conventions, and the two it cannot infer must be told to
// it explicitly.
//
// A blocked caller that was completed remotely gets its canonical result encoded into the
// outgoing frame's result lanes by a completion consumer, which runs while the trap is still
// inside the trap-entry wrapper. The write-back that happens afterwards must then install that
// RESULT lane — not the startup argument lane. Without this flag the S-mode-idle timer dispatch
// had no way to know, so it installed the startup lane over a delivered `TimedOut` and the
// caller observed its own stale endpoint capability as a syscall result: measured live as
// `RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid=1 ... final_a0=9` followed immediately by
// `PAGE_FAULT_UNHANDLED tid=1 addr=0x20006`.
//
// Published only where a completion was actually CONSUMED and its lanes encoded, taken once by
// the write-back, and cleared at every trap entry so no decision is inherited.
pub(crate) static RISCV_SYSCALL_CONTINUATION_PENDING: [core::sync::atomic::AtomicBool;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; crate::kernel::scheduler::MAX_CPUS];

/// Canonical 199E-R2: record that a blocked-syscall completion was consumed on `cpu_idx` and its
/// canonical result is already encoded in the outgoing frame's result lanes.
pub(crate) fn riscv_syscall_continuation_publish(cpu_idx: usize) {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return;
    }
    RISCV_SYSCALL_CONTINUATION_PENDING[cpu_idx].store(true, core::sync::atomic::Ordering::Release);
}

/// Canonical 199E-R2: TAKE the syscall-continuation decision for `cpu_idx`, clearing it.
pub(crate) fn riscv_syscall_continuation_take(cpu_idx: usize) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    RISCV_SYSCALL_CONTINUATION_PENDING[cpu_idx].swap(false, core::sync::atomic::Ordering::AcqRel)
}

/// Canonical 199E-R2: clear any stale syscall-continuation decision for `cpu_idx` without
/// consuming it as an authorization. Called at the START of every RISC-V trap.
pub(crate) fn riscv_syscall_continuation_clear(cpu_idx: usize) {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return;
    }
    RISCV_SYSCALL_CONTINUATION_PENDING[cpu_idx].store(false, core::sync::atomic::Ordering::Release);
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
#[cfg(any(
    all(feature = "x86-shared-region-direct-oracle", target_arch = "x86_64"),
    all(
        feature = "aarch64-shared-region-direct-oracle",
        target_arch = "aarch64"
    ),
    all(feature = "riscv-shared-region-direct-oracle", target_arch = "riscv64")
))]
static IPC_SEND_SHARED_REGION_DIRECT_RETIRE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 198E3C1: emit the `IpcSendSharedRegionDirect` retirement markers exactly once — only from
/// the x86_64 armed-oracle build, only after a real off-lock direct completion (called solely by the
/// gated live-marker helper). The queued class and all non-x86 architectures never compile a
/// shared-region retirement literal.
#[cfg(any(
    all(feature = "x86-shared-region-direct-oracle", target_arch = "x86_64"),
    all(
        feature = "aarch64-shared-region-direct-oracle",
        target_arch = "aarch64"
    ),
    all(feature = "riscv-shared-region-direct-oracle", target_arch = "riscv64")
))]
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
            "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch={} class=IpcSendSharedRegionDirect",
            SHARED_REGION_ORACLE_ARCH
        );
        crate::yarm_log!(
            "GLOBAL_LOCK_RETIRE_CLASS_DONE arch={} class=IpcSendSharedRegionDirect result=ok",
            SHARED_REGION_ORACLE_ARCH
        );
    }
}

/// Stage 198E3C1: origin-gated shared-region LIVE markers (attestations + retirement), emitted ONLY
/// from the drain's success arm AFTER the transaction finalized, the waiter was cleared, and the
/// receiver enqueued exactly once. Every marker LITERAL is confined here and gated on the x86_64
/// armed-oracle build, so a NORMAL artifact — and every non-x86 artifact — is marker-clean. Only the
/// DIRECT class emits (the queued class is forbidden this stage); a non-Direct origin is a no-op.
#[cfg(any(
    all(feature = "x86-shared-region-direct-oracle", target_arch = "x86_64"),
    all(
        feature = "aarch64-shared-region-direct-oracle",
        target_arch = "aarch64"
    ),
    all(feature = "riscv-shared-region-direct-oracle", target_arch = "riscv64")
))]
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
        "IPCSEND_SHARED_REGION_OBJECT_OK arch={} class={} object_match={} fresh_cap=1 pin_transfer=1",
        SHARED_REGION_ORACLE_ARCH,
        class,
        object_match
    );
    let map_right = u8::from(snapshot.rights.contains(CapRights::MAP));
    let write_ok = u8::from(!snapshot.map_write || snapshot.rights.contains(CapRights::WRITE));
    crate::yarm_log!(
        "IPCSEND_SHARED_REGION_MAP_OK arch={} class={} map_right={} write_right_ok={} nx=1 cleanup_token=1",
        SHARED_REGION_ORACLE_ARCH,
        class,
        map_right,
        write_ok
    );
    crate::yarm_log!(
        "IPCSEND_SHARED_REGION_LIFECYCLE_OK arch={} class={} transaction_published=1 receiver_wakes={} leaked_state=0",
        SHARED_REGION_ORACLE_ARCH,
        class,
        u8::from(woke_receiver)
    );
    maybe_log_ipc_send_shared_region_direct_retired();
}

/// Stage 198E3C1: marker-clean stub for every NON-armed build (all normal artifacts + all non-x86
/// architectures). It contains no marker literal, so a normal artifact stays clean and no
/// shared-region retirement/attestation literal is ever compiled into an AArch64/RISC-V binary.
#[cfg(not(any(
    all(feature = "x86-shared-region-direct-oracle", target_arch = "x86_64"),
    all(
        feature = "aarch64-shared-region-direct-oracle",
        target_arch = "aarch64"
    ),
    all(feature = "riscv-shared-region-direct-oracle", target_arch = "riscv64")
)))]
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
/// U4: delegates to the canonical [`queue_advancing_dispatch_enabled`] (see the recv sibling).
pub(crate) fn d2_send_genuine_enabled() -> bool {
    queue_advancing_dispatch_enabled()
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

/// U9-TM §1 — is ANY timer-only proof hook armed?
///
/// Five `maybe_run_*` hooks are called ONLY from the broad `Trap::TimerInterrupt` arm. A split
/// timer route that returned early would stop them running, which is a silent regression rather
/// than a refactor — each is armed by an existing profile and carries a dozen test references.
///
/// U9-TM does not relocate them. Instead the split route REFUSES whenever any of their knobs is
/// armed, before it claims the interrupt, ticks, or mutates anything at all, and the unchanged
/// broad arm then executes the tick and every hook exactly as before. This is a TEMPORARY
/// proof-mode fallback, not TimerInterrupt retirement: while it stands, an armed proof profile
/// still reaches the terminal broad dispatcher through the timer.
///
/// Every term is one of the five existing knob sources — no new flag, no new selector. The
/// disjunction is exhaustive over the timer-only set, and
/// `u9tm_proof_gate::the_gate_covers_every_timer_only_hook` pins that a sixth timer-only hook
/// cannot be added without extending it.
pub(crate) fn timer_proof_hooks_armed() -> bool {
    cap_cnode_enabled()
        || fault_delivery_enabled()
        || spawn_lifecycle_enabled()
        || global_state_enabled()
        || smp_ready_enabled()
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

/// Stage 198E3C2B: default-off AArch64 DIRECT shared-region live-oracle WORKLOAD selector
/// (`yarm.aarch64_shared_region_direct_oracle=1`). Mirror of the x86_64 knob: provisions init startup
/// slot 5 (=6, a free AArch64 selector) so init runs the SAME architecture-neutral parent/child
/// shared-region direct proof, and arms the shared IPC/oracle-proof knob so the DIRECT producer goes
/// live. INERT on a normal boot; does NOT enable the queued class, the x86 oracle, or RISC-V.
pub(crate) static AARCH64_SHARED_REGION_DIRECT_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_aarch64_shared_region_direct_oracle_enabled(enabled: bool) {
    AARCH64_SHARED_REGION_DIRECT_ORACLE_ENABLED
        .store(enabled, core::sync::atomic::Ordering::Release);
    if enabled {
        set_ipc_recv_oracle_proof_enabled(true);
    }
}

pub fn aarch64_shared_region_direct_oracle_enabled() -> bool {
    AARCH64_SHARED_REGION_DIRECT_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 198E3C2C: default-off RISC-V DIRECT shared-region live-oracle WORKLOAD selector
/// (`yarm.riscv_shared_region_direct_oracle=1`). Mirror of the x86_64/AArch64 knobs: provisions init
/// startup slot 5 (=7, the next free RISC-V selector after the six FutexWake/FutexWait/Yield oracles)
/// so init runs the SAME architecture-neutral parent/child shared-region direct proof, and arms the
/// shared IPC/oracle-proof knob so the DIRECT producer goes live. INERT on a normal boot; does NOT
/// enable the queued class, the x86_64 oracle, or the AArch64 oracle.
pub(crate) static RISCV_SHARED_REGION_DIRECT_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_riscv_shared_region_direct_oracle_enabled(enabled: bool) {
    RISCV_SHARED_REGION_DIRECT_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
    if enabled {
        set_ipc_recv_oracle_proof_enabled(true);
    }
}

pub fn riscv_shared_region_direct_oracle_enabled() -> bool {
    RISCV_SHARED_REGION_DIRECT_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 198E3C2B/C: the shared-region DIRECT oracle is live when ANY arch's workload selector is
/// armed. The provisioning, authoritative ack, and direct producer gate all key on this arch-neutral
/// predicate; the per-arch marker literals are additionally `target_arch`-gated at their emitter.
pub fn shared_region_direct_oracle_enabled() -> bool {
    x86_shared_region_direct_oracle_enabled()
        || aarch64_shared_region_direct_oracle_enabled()
        || riscv_shared_region_direct_oracle_enabled()
}

// ─── Stage 199A2B2F: NR6 direct-request proof gate + committed-server ack ───────────────
/// Default-OFF internal proof gate for the x86_64 off-lock `IpcCallDirectRequest` path
/// (trap-entry snapshot publication + production recv-v2 acknowledgement publication).
/// Off the gate the existing NR6 path is unchanged.
pub(crate) static IPCCALL_DIRECT_PROOF_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_ipccall_direct_proof_enabled(enabled: bool) {
    IPCCALL_DIRECT_PROOF_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn ipccall_direct_proof_enabled() -> bool {
    IPCCALL_DIRECT_PROOF_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

// ─── Stage 199D: x86_64 NR6/NR7 PRODUCTION-DEFAULT admission ───────────────────────────
//
// The off-lock direct request/reply path is the DEFAULT on x86_64. No knob, no oracle, no
// runtime selector participates: `cfg!(target_arch = "x86_64")` is the whole condition, so a
// normal feature-off boot takes it for every eligible call. Correctness rests on the Stage
// 199D increments that preceded this flip — the canonical recv-v2 delivery projection, the
// typed disposition contract, return-lane parity, the bounded multi-pair acknowledgement
// stores, and the Buffered-only eligibility contract that sends Synchronous endpoints to the
// legacy rendezvous path.
//
// AArch64 and RISC-V are UNCHANGED: they remain proof-gated and oracle-confined, so their
// admission and publication predicates below still consult the proof gate and the oracle
// endpoints exactly as before.

/// True iff the direct NR6/NR7 path is the production default on this architecture.
/// A compile-time constant, not a runtime knob.
///
/// # DISABLED on every architecture — Stage 199D-WA1-GATE
///
/// Ordinary `IpcCall`/`IpcReply` traffic on **every** architecture, x86_64 included, falls back
/// to the legacy path. Admission and blocked-waiter acknowledgement publication both require an
/// explicit proof/oracle selector; request/reply endpoint confinement is whatever those
/// selectors authorize. The rationale is below the blocker history.
///
/// ## Historical — the x86_64 production default (`0b5ec254`, since RECLASSIFIED)
///
/// The paragraphs and list that follow record the state when the x86_64 production default was
/// ON. They are retained as the blocker history that earned that default, **not** as a
/// description of current behaviour. Seven blockers stood between the mechanism and it; every
/// one was found by a live boot rather than by inspection, and every one is closed:
///
/// 1. **Capability transfer was silently dropped.** NR7 eligibility carries
///    `transfer_cap_present`, asked through the one canonical `transfer_cap_arg_present`
///    predicate the legacy decode is itself built on; a cap-bearing reply declines before any
///    mutation and the legacy path does the transfer.
/// 2. **The acknowledgement store had no production release path.** The lease is owned by the
///    endpoint waiter lifecycle: the three `IpcSubsystem` waiter-removal primitives every
///    canonical closing edge funnels through retire the exact
///    `{endpoint_index, endpoint_generation, waiter_tid, waiter_asid}` lease.
/// 3. **Orphans tripped the overwrite fuse** — 17 trips on the first attempt, 0 now.
/// 4. **Capacity was a magic 8.** [`crate::kernel::direct_ack_store::DIRECT_ACK_STORE_CAPACITY`]
///    is now [`ENDPOINT_WAITER_SLOTS`] — one slot per endpoint index, derived at compile time
///    from the authoritative endpoint receive-waiter table rather than chosen.
/// 5. **The reverse-link CREATION edge was blind to the direct path.** Both installation seams
///    delegate to [`install_server_reply_link`].
/// 6. **The reverse-link CLOSE edge had the same divergence**, permissively. All four closing
///    paths delegate to [`close_server_reply_link`], so `links_created == links_closed` is a
///    meaningful leak invariant on the production path.
/// 7. **Terminal-arbitrated replies lost their race.** A reply whose record is arbitrated by an
///    armed terminal-ownership / reply-timeout cell must reserve the terminal before its caller
///    copy and commit it after; that lease lives only on the legacy path, so servicing one
///    off-lock made the reply reserve, roll back, and lose to the timeout's deferred path. NR7
///    eligibility now carries `terminal_arbitrated`, read from the authoritative cell and exact
///    in record index AND generation, and such a reply declines before any mutation.
///
/// Both reverse-link edges and both installation edges are single shared decisions, and the
/// arbitrated reply population is explicitly ineligible — so the direct path services exactly
/// the traffic whose semantics it reproduces.
///
/// **Future canonical 199E work:** porting the reply-win terminal lease into the direct NR7
/// transaction, which would let the arbitrated population go off-lock too. Until then those
/// replies take the legacy path by design, counted as `declined_terminal_arbitration`.
/// ─── Stage 199D-WA1-GATE: the x86_64 production DEFAULT is DISABLED ────────────────────────
///
/// `WAITER_OWNERSHIP_EXCLUSIVE=no`, and the reachability is REAL, not mechanism-level. The
/// direct NR6/NR7 transactions publish the reply record, the provisional server-local reply cap
/// and the receiver's user memory BEFORE claiming the endpoint waiter, and
/// `process_ipc_timeout_deadlines` wakes a `Blocked(EndpointReceive)` task at task rank before
/// invalidating its waiter at ipc rank — so it cannot lose to a claim it never consults.
///
/// Ordinary recv/send deadlines are armed by `recv_block_phase_b_task`, its send-block twin and
/// the queued-recv block path, all of which set `ipc_timeout_deadline` **without** a
/// `reply_timeout_token`. They are therefore fully independent of the reply-terminal
/// arbitration that gates direct NR7 eligibility. The notification signal wake never consults
/// the endpoint waiter at all. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.30.
///
/// This is a GATE, not a retraction. Every explicit proof/oracle selector is untouched:
/// `ipccall_direct_admission_enabled()` still admits NR6/NR7 whenever
/// `ipccall_direct_proof_enabled()` is armed, so every knob-gated mechanism seal stays
/// reproducible. Ordinary traffic falls back to the accepted legacy path.
pub const fn ipccall_direct_production_enabled() -> bool {
    false
}

/// True iff NR6/NR7 may be admitted to the split dispatcher at all.
///
/// Stage 199D-WA1-GATE: the production term is `false` on every architecture, so this is now
/// exactly the proof gate. Ordinary traffic — on x86_64 as much as anywhere else — is not
/// admitted and takes the legacy path; only an explicitly armed proof/oracle selector admits.
pub fn ipccall_direct_admission_enabled() -> bool {
    ipccall_direct_production_enabled() || ipccall_direct_proof_enabled()
}

/// True iff a blocked-waiter acknowledgement may be published at all.
///
/// Same predicate as [`ipccall_direct_admission_enabled`], and since WA1-GATE that means: the
/// explicit proof/oracle selector, on every architecture. Without it the request path finds
/// nothing to claim and every ordinary call declines to legacy — which is the current default
/// everywhere.
pub fn ipccall_direct_publication_enabled() -> bool {
    ipccall_direct_production_enabled() || ipccall_direct_proof_enabled()
}

/// True iff this REQUEST endpoint index is admitted to the off-lock path.
///
/// Stage 199D-WA1-GATE: with the production term `false` on every architecture, admission is
/// exactly the oracle's provisioned request endpoint — the confinement the explicit selector
/// authorizes, and nothing wider. The `production ||` term is retained so re-enabling the
/// default in WA2 restores the Buffered-only eligibility contract without another edit here.
pub fn ipccall_direct_request_endpoint_admitted(eidx: usize) -> bool {
    ipccall_direct_production_enabled() || ipccall_direct_oracle_request_endpoint_is(eidx)
}

/// True iff this REPLY endpoint index is admitted to the off-lock path. See the request twin —
/// since WA1-GATE this too is exactly the oracle's provisioned reply endpoint.
pub fn ipccall_direct_reply_endpoint_admitted(eidx: usize) -> bool {
    ipccall_direct_production_enabled() || ipccall_direct_oracle_reply_endpoint_is(eidx)
}

// ─── Stage 199A2B4: x86_64 DIRECT IpcCall/IpcReply live round-trip oracle ───────────────
/// Startup slot-5 selector value that tells init to run the x86_64 DIRECT IpcCall/IpcReply
/// live round-trip oracle (mirrors `yarm_user_rt::syscall::IPCCALL_DIRECT_ORACLE_SELECTOR`).
/// Slot 5 is mutually exclusive across all init oracles; value 3 is the next free x86_64
/// selector after FutexWake(1) and shared-region-direct(2).
pub const IPCCALL_DIRECT_ORACLE_SELECTOR: u64 = 3;

/// Stage 199A2C1: AArch64 startup slot-5 selector for the DIRECT IpcCall/IpcReply round-trip oracle.
/// Value 7 is the next free AArch64 selector after FutexWake(1)/FutexWait(2)/idle(3)/yield(4,5)/
/// shared-region-direct(6); the AArch64 init dispatch keys on `Some(7)`.
pub const AARCH64_IPCCALL_DIRECT_ORACLE_SELECTOR: u64 = 7;

/// Stage 199A2C2: RISC-V startup slot-5 selector for the DIRECT IpcCall/IpcReply round-trip oracle.
/// Value 8 is the next free RISC-V selector after FutexWake(1)/queue-switch(2)/FutexWait(3)/
/// FutexWait-idle(4)/yield(5,6)/shared-region-direct(7); the RISC-V init dispatch keys on `Some(8)`.
pub const RISCV_IPCCALL_DIRECT_ORACLE_SELECTOR: u64 = 8;

/// Stage 199A2B4: default-off x86_64 DIRECT IpcCall/IpcReply live-oracle WORKLOAD selector
/// (`yarm.x86_64_ipccall_direct_oracle=1`). Provisions init startup slot 5 (=3) so init runs
/// the parent(client)/child(server) NR6 request + NR7 reply round trip through the accepted
/// off-lock transactions. Selecting the workload also arms the shared NR6/NR7 proof gate so the
/// direct request + reply gates become live. INERT on a normal boot (which never sets this);
/// does NOT enable queued calls, timeouts, notifications, server-death wake, or any non-x86 arch.
pub(crate) static X86_IPCCALL_DIRECT_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_x86_ipccall_direct_oracle_enabled(enabled: bool) {
    if enabled {
        // Mutual exclusion (Stage 199A2D2A): the SMP=1 functional selector and the SMP=2 cross-CPU
        // selector are never both armed. Refuse to arm the functional oracle if the SMP one is on.
        if x86_ipccall_direct_smp_oracle_enabled() {
            crate::yarm_log!("IPCCALL_DIRECT_ORACLE_REFUSED reason=smp_selector_active");
            return;
        }
        // Selecting the workload arms the NR6/NR7 off-lock proof gate for this run.
        set_ipccall_direct_proof_enabled(true);
    }
    X86_IPCCALL_DIRECT_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn x86_ipccall_direct_oracle_enabled() -> bool {
    X86_IPCCALL_DIRECT_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

// ─── Stage 199A2D2A: x86_64 SMP=2 cross-CPU DIRECT IpcCall (NR6 request) oracle ─────────────
/// Startup selector value for the x86_64 SMP=2 cross-CPU DIRECT IpcCall (request-only) oracle.
/// Distinct from the SMP=1 functional selector (`IPCCALL_DIRECT_ORACLE_SELECTOR` = 3) so the two
/// are mutually exclusive: at most one of them may be armed on any boot. Value 9 is the next free
/// x86_64 selector after the functional direct oracle (3).
pub const X86_IPCCALL_DIRECT_SMP_ORACLE_SELECTOR: u64 = 9;

/// Stage 199A2D2A: default-off x86_64 SMP=2 cross-CPU DIRECT IpcCall request oracle
/// (`yarm.x86_64_ipccall_direct_smp_oracle=1`). This oracle runs ONE userspace IPC server on an
/// application processor (CPU 1) blocked in recv-v2, delivers ONE NR6 direct request from a BSP
/// (CPU 0) client, and remotely wakes + resumes the server on CPU 1. It proves ONLY the cross-CPU
/// request direction (no NR7 reply, no complete Stage 199 SMP seal). It is MUTUALLY EXCLUSIVE with
/// the SMP=1 functional selector, requires `target_arch = x86_64`, `QEMU_SMP >= 2`, the
/// `x86-ipccall-direct-smp-oracle` feature, AND this selector. Feature-on without the selector is
/// inert. Selecting it arms the shared NR6 off-lock proof gate but NOT the functional x86 flag, and
/// NOT queued calls, timeouts, notifications, server-death wake, or any non-x86 arch. The single-slot
/// acknowledgement overwrite fuse stays enabled (one outstanding pair).
pub(crate) static X86_IPCCALL_DIRECT_SMP_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_x86_ipccall_direct_smp_oracle_enabled(enabled: bool) {
    if enabled {
        // Mutual exclusion: refuse to arm the SMP oracle if the SMP=1 functional selector is on.
        if x86_ipccall_direct_oracle_enabled() {
            crate::yarm_log!("IPCCALL_DIRECT_SMP_ORACLE_REFUSED reason=functional_selector_active");
            return;
        }
        set_ipccall_direct_proof_enabled(true);
    }
    X86_IPCCALL_DIRECT_SMP_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn x86_ipccall_direct_smp_oracle_enabled() -> bool {
    X86_IPCCALL_DIRECT_SMP_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 199A2D2C2B: true once the cross-CPU NR6 REQUEST path is live (a real CPU-0 client + a real
/// CPU-1 recv-v2 server sharing an endpoint). In that path CPU 1's reschedule-pending flag
/// ORIGINATES from the real CPU-0 remote-wake interrupt, so the AP saved-frame resume must NOT
/// self-arm it. In the Stage 199A2D2C2A Yield-only proof (no client) this stays `false`, and the
/// resume self-arms the flag to exercise the consume path. DEFAULT-OFF until the request path lands.
pub(crate) static X86_IPCCALL_DIRECT_SMP_REQUEST_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub fn x86_ipccall_direct_smp_request_active() -> bool {
    X86_IPCCALL_DIRECT_SMP_REQUEST_ACTIVE.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 199A2D2C2B1: default-off sub-selector of the SMP oracle
/// (`yarm.x86_64_ipccall_direct_smp_recv_v2_server=1`). When set (AND the SMP oracle is armed) the
/// single AP workload becomes a REAL recv-v2 IPC server: it provisions a request endpoint + a RECEIVE
/// cap in the server's own CNode + payload/meta buffers, and its userspace stub issues a genuine
/// recv-v2 syscall that blocks on CPU 1 (committing a saved continuation + publishing the
/// authoritative blocked-server acknowledgement). When UNSET the workload stays the Stage 199A2D2C2A
/// Yield saved-frame proof (so that seal is untouched). This sub-selector does NOT deliver an NR6
/// request, wake the server, or arm the functional x86 flag — it proves the server-block half only.
pub(crate) static X86_IPCCALL_DIRECT_SMP_RECV_V2_SERVER_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_x86_ipccall_direct_smp_recv_v2_server_enabled(enabled: bool) {
    X86_IPCCALL_DIRECT_SMP_RECV_V2_SERVER_ENABLED
        .store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn x86_ipccall_direct_smp_recv_v2_server_enabled() -> bool {
    X86_IPCCALL_DIRECT_SMP_RECV_V2_SERVER_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 199A2D2C2B2: default-off sub-selector of the SMP oracle
/// (`yarm.x86_64_ipccall_direct_smp_request=1`). When set it IMPLIES the recv-v2 server sub-selector
/// (the CPU-1 server half) AND additionally provisions a REAL CPU-0 userspace client that invokes NR6
/// through the normal x86 split-dispatch path (with a bounded WouldBlock retry), delivers ONE
/// cross-CPU direct request via the accepted transaction, sends the canonical reschedule IPI to CPU 1,
/// and resumes the server's recv-v2 continuation on CPU 1 via the sealed saved-frame return. It marks
/// the cross-CPU REQUEST path active so the AP saved-frame resume takes its pending flag from the real
/// IPI (never a self-arm). It does NOT begin NR7/reply. Mutually exclusive with the standalone
/// Stage 199A2D2C2A Yield workload (that runs only when neither server sub-selector is set).
pub(crate) static X86_IPCCALL_DIRECT_SMP_REQUEST_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_x86_ipccall_direct_smp_request_enabled(enabled: bool) {
    if enabled {
        // The request path requires the CPU-1 recv-v2 server, and marks the cross-CPU request active
        // so the AP saved-frame resume does not self-arm CPU 1's pending flag (it comes from the IPI).
        set_x86_ipccall_direct_smp_recv_v2_server_enabled(true);
        X86_IPCCALL_DIRECT_SMP_REQUEST_ACTIVE.store(true, core::sync::atomic::Ordering::Release);
    }
    X86_IPCCALL_DIRECT_SMP_REQUEST_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn x86_ipccall_direct_smp_request_enabled() -> bool {
    X86_IPCCALL_DIRECT_SMP_REQUEST_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 199A2D2C2B2: bounded counters for the CPU-0 client's NR6 attempts. `EARLY_WOULDBLOCK` counts
/// non-mutating early WouldBlock returns (server not yet blocked); `SUCCESS` counts committed
/// deliveries. attempts = early_retries + successes (one success expected). These prove the retry
/// mechanism exists and the early WouldBlock did not mutate.
pub(crate) static X86_IPCCALL_DIRECT_SMP_REQUEST_EARLY_WOULDBLOCK: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

pub(crate) fn ipccall_direct_smp_request_note_early_wouldblock() {
    X86_IPCCALL_DIRECT_SMP_REQUEST_EARLY_WOULDBLOCK
        .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
}

pub fn ipccall_direct_smp_request_early_wouldblock_count() -> u64 {
    X86_IPCCALL_DIRECT_SMP_REQUEST_EARLY_WOULDBLOCK.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 199A2D2C2B2: count of COMMITTED cross-CPU request deliveries (the accepted transaction
/// succeeded: reserve → mint → copy → claim → commit RunnableSaved → record Available → enqueue).
/// Exactly one is expected. Gates the terminal request-OK seal marker.
pub(crate) static X86_IPCCALL_DIRECT_SMP_REQUEST_DELIVERED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

pub(crate) fn ipccall_direct_smp_request_note_delivered() {
    X86_IPCCALL_DIRECT_SMP_REQUEST_DELIVERED.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
    // U3 (canonical 203C): record the delivery HALF of the request-OK rendezvous. See
    // `try_emit_ipccall_direct_smp_request_ok`.
    try_emit_ipccall_direct_smp_request_ok(REQUEST_OK_FACT_DELIVERED);
}

pub fn ipccall_direct_smp_request_delivered_count() -> u64 {
    X86_IPCCALL_DIRECT_SMP_REQUEST_DELIVERED.load(core::sync::atomic::Ordering::Acquire)
}

/// U3 (canonical 203C) — PROOF BOOKKEEPING for the terminal cross-CPU request-OK marker.
/// **This BLOCKS U3**: it exists so the x86_64 BSP saved-resume cohort's live cell is
/// deterministically observable before its two broad acquisitions are retired.
///
/// The marker's two preconditions are produced by DIFFERENT CPUs with no ordering between them:
/// CPU 0's accepted request drain records the committed delivery, and CPU 1's resumed server
/// emits its single `X86_AP_RECV_V2_CONTINUED` DebugLog. The previous one-shot emitter fired only
/// from the continuation side and only if the delivery had *already* been recorded, so the
/// continuation-first interleaving lost the marker permanently — measured as 2 emissions in 6
/// otherwise-identical matched-artifact boots, with every kernel-path marker deterministic.
///
/// This is a RENDEZVOUS, not a retry: each side records its own fact and calls the same helper,
/// and whichever call completes the pair emits. Both orderings, and simultaneous arrival, are
/// equivalent.
///
/// Confinement: purely observational. It is reached only from the x86_64 direct-SMP proof
/// workload's two sites, no production IPC decision consults it, and it cannot delay, suppress or
/// alter any delivery — the emission happens strictly after the fact it reports. It adds no public
/// marker or counter family: the emitted text is the pre-existing marker, unchanged.
static X86_IPCCALL_DIRECT_SMP_REQUEST_OK_FACTS: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

/// CPU 0's accepted request drain recorded one committed cross-CPU delivery.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const REQUEST_OK_FACT_DELIVERED: u32 = 1 << 0;
/// CPU 1's resumed server emitted its single `X86_AP_RECV_V2_CONTINUED` userspace log.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const REQUEST_OK_FACT_CONTINUED: u32 = 1 << 1;
/// The marker has been emitted; the rendezvous is spent and can never emit again.
#[cfg_attr(not(test), allow(dead_code))]
const REQUEST_OK_FACT_EMITTED: u32 = 1 << 2;

/// Record one half of the rendezvous and emit the existing marker iff BOTH halves now hold.
///
/// Returns `true` only for the single call that completed the pair. Repeated or duplicate calls
/// from either side are idempotent, a lone fact never emits, and a concurrent pair emits once:
/// the `EMITTED` bit is set in the SAME compare-exchange that completes the pair, so exactly one
/// caller can observe the transition.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn try_emit_ipccall_direct_smp_request_ok(fact: u32) -> bool {
    use core::sync::atomic::Ordering;
    const BOTH: u32 = REQUEST_OK_FACT_DELIVERED | REQUEST_OK_FACT_CONTINUED;
    let mut observed = X86_IPCCALL_DIRECT_SMP_REQUEST_OK_FACTS.load(Ordering::Acquire);
    loop {
        if observed & REQUEST_OK_FACT_EMITTED != 0 {
            return false;
        }
        let recorded = observed | fact;
        let complete = (recorded & BOTH) == BOTH;
        let next = if complete {
            recorded | REQUEST_OK_FACT_EMITTED
        } else {
            recorded
        };
        match X86_IPCCALL_DIRECT_SMP_REQUEST_OK_FACTS.compare_exchange_weak(
            observed,
            next,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                if complete {
                    emit_ipccall_direct_smp_request_ok_marker();
                    return true;
                }
                return false;
            }
            Err(seen) => observed = seen,
        }
    }
}

/// Clear the rendezvous completely. Setup/reset only — never a production path.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn reset_ipccall_direct_smp_request_ok_rendezvous() {
    X86_IPCCALL_DIRECT_SMP_REQUEST_OK_FACTS.store(0, core::sync::atomic::Ordering::Release);
}

/// The pre-existing marker text, unchanged. Emitted from exactly one place.
///
/// Emitted SYNCHRONOUSLY, for the reason `ap_seal_syscall_begin` already documents: the shared
/// printk ring is drop-prone under concurrent AP+BSP traffic, and this is a REQUIRED proof marker.
/// Whichever CPU completes the rendezvous emits it, and the completing side is frequently CPU 0
/// inside the request drain — the busiest console window of the whole cell. Buffered emission was
/// measured losing it outright in 2 of 6 matched-artifact boots even after the ordering was fixed:
/// no marker text and no fragment of it reached the log, while every other terminal marker did.
/// This changes nothing about WHEN or WHETHER the fact holds, only that reporting it cannot be
/// dropped.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn emit_ipccall_direct_smp_request_ok_marker() {
    crate::kernel::printk::printk_emit_sync(format_args!(
        "IPCCALL_DIRECT_SMP_REQUEST_OK sender_cpu=0 receiver_cpu=1 cross_cpu=1 request_copies=1 server_wakes=1 server_continuations=1 result=ok"
    ));
}

/// Hosted / non-x86: the rendezvous still runs (so it is testable), but emits no kernel marker.
#[cfg(not(all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
fn emit_ipccall_direct_smp_request_ok_marker() {}

/// Stage 199A2D2C2B2: the AP-continuation HALF of the terminal cross-CPU request-OK rendezvous.
/// Called with every DebugLog message; records the continuation fact only for the resumed CPU-1
/// server's `X86_AP_RECV_V2_CONTINUED` log. The marker is emitted by
/// `try_emit_ipccall_direct_smp_request_ok` once the committed delivery is also recorded — in
/// EITHER order. No-op off the C2B2 path.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn maybe_emit_ipccall_direct_smp_request_ok(msg: &str) {
    if !x86_ipccall_direct_smp_request_enabled() {
        return;
    }
    if !msg.starts_with("X86_AP_RECV_V2_CONTINUED") {
        return;
    }
    try_emit_ipccall_direct_smp_request_ok(REQUEST_OK_FACT_CONTINUED);
}

/// Hosted / non-x86 no-op so the DebugLog call site compiles unconditionally.
#[cfg(not(all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
pub(crate) fn maybe_emit_ipccall_direct_smp_request_ok(_msg: &str) {}

/// Stage 199A2D2C2C: default-off sub-selector of the SMP oracle
/// (`yarm.x86_64_ipccall_direct_smp_reply=1`). When set it IMPLIES the request sub-selector (the whole
/// forward direction) AND additionally drives the REVERSE (NR7 reply) direction: after the CPU-0
/// client's NR6 succeeds it issues a genuine recv-v2 on its OWN reply endpoint (blocking on CPU 0,
/// publishing the blocked-caller ack); the resumed CPU-1 server, after validating the request, issues
/// a genuine NR7 with the Reply CapId it read in ring 3, driving the accepted off-lock reply
/// transaction; on success CPU 1 sends the canonical reschedule IPI to CPU 0, whose saved-frame resume
/// wakes the client to validate the reply bytes/metadata in ring 3. It arms the oracle's REPLY
/// endpoint (the request path leaves it `usize::MAX`). Default-off until the reply path lands.
pub(crate) static X86_IPCCALL_DIRECT_SMP_REPLY_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_x86_ipccall_direct_smp_reply_enabled(enabled: bool) {
    if enabled {
        // The reply path requires the whole forward (request) direction.
        set_x86_ipccall_direct_smp_request_enabled(true);
    }
    X86_IPCCALL_DIRECT_SMP_REPLY_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn x86_ipccall_direct_smp_reply_enabled() -> bool {
    X86_IPCCALL_DIRECT_SMP_REPLY_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 199A2D2C2C: count of COMMITTED cross-CPU reply deliveries (the accepted `ipc_reply_direct_txn`
/// succeeded: resolve → reserve → caller-copy off-lock → claim caller waiter → commit → consume →
/// enqueue caller on CPU 0). Exactly one is expected. Gates the terminal reply-OK seal marker.
pub(crate) static X86_IPCREPLY_DIRECT_SMP_REPLY_DELIVERED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

pub(crate) fn ipcreply_direct_smp_reply_note_delivered() {
    X86_IPCREPLY_DIRECT_SMP_REPLY_DELIVERED.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
}

pub fn ipcreply_direct_smp_reply_delivered_count() -> u64 {
    X86_IPCREPLY_DIRECT_SMP_REPLY_DELIVERED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 199A2D2C2C: count of non-mutating early WouldBlock returns from the CPU-1 server's NR7 (the
/// CPU-0 caller has not yet blocked on its reply endpoint, so no blocked-caller ack is published).
pub(crate) static X86_IPCREPLY_DIRECT_SMP_REPLY_EARLY_WOULDBLOCK: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

pub(crate) fn ipcreply_direct_smp_reply_note_early_wouldblock() {
    X86_IPCREPLY_DIRECT_SMP_REPLY_EARLY_WOULDBLOCK
        .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
}

pub fn ipcreply_direct_smp_reply_early_wouldblock_count() -> u64 {
    X86_IPCREPLY_DIRECT_SMP_REPLY_EARLY_WOULDBLOCK.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 199A2D2C2C: count of REFUSED duplicate NR7 replies (the blocked-caller ack was already
/// claimed + the record consumed by the one successful reply; a second NR7 through the same userspace
/// cap is refused with a canonical `WrongObject` and performs ZERO additional copies / claims /
/// enqueues / IPIs / wakes). Exactly one is expected in the sealed flow (the server issues one
/// deliberate duplicate to prove the one-shot barrier).
pub(crate) static X86_IPCREPLY_DIRECT_SMP_DUPLICATE_REFUSED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

pub(crate) fn ipcreply_direct_smp_note_duplicate_refused() {
    X86_IPCREPLY_DIRECT_SMP_DUPLICATE_REFUSED.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
}

pub fn ipcreply_direct_smp_duplicate_refused_count() -> u64 {
    X86_IPCREPLY_DIRECT_SMP_DUPLICATE_REFUSED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 199A2D2C2C: emit the TERMINAL cross-CPU reply-OK marker EXACTLY ONCE, ONLY after the resumed
/// CPU-0 client's userspace `X86_BSP_REPLY_USER_VALIDATED` marker is observed (passed here as `msg`)
/// AND exactly one committed reply delivery is recorded. Kernel marker (not userspace). No-op off the
/// C2C reply path or before the client's ring-3 reply validation.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn maybe_emit_ipcreply_direct_smp_reply_ok(msg: &str) {
    use core::sync::atomic::{AtomicBool, Ordering};
    static EMITTED: AtomicBool = AtomicBool::new(false);
    if !x86_ipccall_direct_smp_reply_enabled() {
        return;
    }
    if !msg.starts_with("X86_BSP_REPLY_USER_VALIDATED") {
        return;
    }
    if ipcreply_direct_smp_reply_delivered_count() < 1 {
        return;
    }
    if EMITTED.swap(true, Ordering::AcqRel) {
        return;
    }
    crate::yarm_log!(
        "IPCREPLY_DIRECT_SMP_REPLY_OK sender_cpu=1 receiver_cpu=0 cross_cpu=1 reply_copies=1 caller_wakes=1 one_shot=1 result=ok"
    );
}

/// Hosted / non-x86 no-op so the DebugLog call site compiles unconditionally.
#[cfg(not(all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
pub(crate) fn maybe_emit_ipcreply_direct_smp_reply_ok(_msg: &str) {}

/// Stage 199A2D2C2C: the CPU-0 oracle client's TID, recorded at provisioning so the BSP dispatch hook
/// (`maybe_emit_bsp_saved_dispatch_ok`) can recognise the client's saved-frame resume without a
/// hardcoded probe TID. 0 = unset.
pub(crate) static X86_C2C_CLIENT_TID: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

pub(crate) fn set_x86_c2c_client_tid(tid: u64) {
    X86_C2C_CLIENT_TID.store(tid, core::sync::atomic::Ordering::Release);
}

pub fn x86_c2c_client_tid() -> u64 {
    X86_C2C_CLIENT_TID.load(core::sync::atomic::Ordering::Acquire)
}

/// The x86_64 SMP cross-CPU request oracle is ACTIVE only when the selector is armed AND the boot is
/// genuinely SMP (`online_cpus >= 2`). Same-CPU (`online_cpus < 2`) can never present as cross-CPU:
/// the caller passes the authoritative online-CPU count so the gate cannot be spoofed by a knob
/// alone. The `x86-ipccall-direct-smp-oracle` feature is enforced at the marker emitters (cfg-gated).
pub fn ipccall_direct_smp_oracle_active(online_cpus: usize) -> bool {
    x86_ipccall_direct_smp_oracle_enabled() && online_cpus >= 2
}

/// Stage 199A2C1: default-off AArch64 DIRECT IpcCall/IpcReply live-oracle WORKLOAD selector
/// (`yarm.aarch64_ipccall_direct_oracle=1`). Mirror of the x86_64 knob: provisions init startup
/// slot 5 (=7, the next free AArch64 selector after FutexWake/FutexWait/Yield/shared-region 1..6) so
/// init runs the SAME arch-neutral parent(client)/child(server) NR6 request + NR7 reply round trip
/// through the accepted off-lock transactions, and arms the shared NR6/NR7 proof gate so both direct
/// gates go live. INERT on a normal boot; does NOT enable queued calls, timeouts, notifications,
/// server-death wake, the x86_64 oracle, or RISC-V.
pub(crate) static AARCH64_IPCCALL_DIRECT_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_aarch64_ipccall_direct_oracle_enabled(enabled: bool) {
    AARCH64_IPCCALL_DIRECT_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
    if enabled {
        // Selecting the workload arms the SHARED NR6/NR7 off-lock proof gate for this run. This is
        // the arch-neutral gate; it does NOT arm the x86-specific oracle-enabled flag.
        set_ipccall_direct_proof_enabled(true);
    }
}

pub fn aarch64_ipccall_direct_oracle_enabled() -> bool {
    AARCH64_IPCCALL_DIRECT_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// Stage 199A2C2: default-off RISC-V DIRECT IpcCall/IpcReply live-oracle WORKLOAD selector
/// (`yarm.riscv_ipccall_direct_oracle=1`). Mirror of the x86_64/AArch64 knobs: provisions init
/// startup slot 5 (=8, the next free RISC-V selector after FutexWake/FutexWait/Yield/shared-region
/// 1..7) so init runs the SAME arch-neutral parent(client)/child(server) NR6 request + NR7 reply
/// round trip, and arms the shared NR6/NR7 proof gate. INERT on a normal boot; does NOT enable
/// queued calls, timeouts, notifications, server-death wake, the x86_64 oracle, or the AArch64 oracle.
pub(crate) static RISCV_IPCCALL_DIRECT_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_riscv_ipccall_direct_oracle_enabled(enabled: bool) {
    RISCV_IPCCALL_DIRECT_ORACLE_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
    if enabled {
        // Selecting the workload arms the SHARED NR6/NR7 off-lock proof gate for this run. It does
        // NOT arm the x86- or AArch64-specific oracle-enabled flags.
        set_ipccall_direct_proof_enabled(true);
    }
}

pub fn riscv_ipccall_direct_oracle_enabled() -> bool {
    RISCV_IPCCALL_DIRECT_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

/// The DIRECT IpcCall/IpcReply oracle is live when ANY arch's workload selector is armed. The
/// provisioning + arch-parameterized live markers key on this arch-neutral predicate; the per-arch
/// marker literals are additionally `target_arch`-gated at their emitter.
pub fn ipccall_direct_oracle_enabled() -> bool {
    x86_ipccall_direct_oracle_enabled()
        || aarch64_ipccall_direct_oracle_enabled()
        || riscv_ipccall_direct_oracle_enabled()
}

/// The compiled architecture tag for the DIRECT IpcCall/IpcReply oracle markers (mirrors the
/// shared-region oracle's `SHARED_REGION_ORACLE_ARCH`). Confined to the armed-oracle build.
#[cfg(all(feature = "ipccall-direct-oracle", target_arch = "x86_64"))]
pub(crate) const IPCCALL_DIRECT_ORACLE_ARCH: &str = "x86_64";
#[cfg(all(feature = "ipccall-direct-oracle", target_arch = "aarch64"))]
pub(crate) const IPCCALL_DIRECT_ORACLE_ARCH: &str = "aarch64";
#[cfg(all(feature = "ipccall-direct-oracle", target_arch = "riscv64"))]
pub(crate) const IPCCALL_DIRECT_ORACLE_ARCH: &str = "riscv64";

/// Stage 199A2B4: the oracle's request + reply endpoint SLOT INDICES. The off-lock NR6/NR7 gates
/// take the direct path ONLY for these exact endpoints, so a NORMAL system IpcCall/IpcReply (the
/// live service chain) always stays on its unchanged legacy path even while the proof gate is armed.
/// This confines the off-lock retirement to the oracle's own round trip — the service chain is never
/// routed through the direct transactions. `usize::MAX` = un-provisioned (no endpoint matches).
pub(crate) static IPCCALL_DIRECT_ORACLE_REQ_EIDX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);
pub(crate) static IPCCALL_DIRECT_ORACLE_REP_EIDX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);

pub(crate) fn set_ipccall_direct_oracle_endpoints(req_eidx: usize, rep_eidx: usize) {
    IPCCALL_DIRECT_ORACLE_REQ_EIDX.store(req_eidx, core::sync::atomic::Ordering::Release);
    IPCCALL_DIRECT_ORACLE_REP_EIDX.store(rep_eidx, core::sync::atomic::Ordering::Release);
}

/// True iff `eidx` is the oracle's request endpoint (the ONLY endpoint whose IpcCall is serviced
/// off-lock). `false` for every other endpoint (legacy path) and when un-provisioned.
pub fn ipccall_direct_oracle_request_endpoint_is(eidx: usize) -> bool {
    IPCCALL_DIRECT_ORACLE_REQ_EIDX.load(core::sync::atomic::Ordering::Acquire) == eidx
}

/// True iff `eidx` is the oracle's reply endpoint (the ONLY reply whose IpcReply is serviced
/// off-lock). `false` for every other endpoint (legacy path) and when un-provisioned.
pub fn ipccall_direct_oracle_reply_endpoint_is(eidx: usize) -> bool {
    IPCCALL_DIRECT_ORACLE_REP_EIDX.load(core::sync::atomic::Ordering::Acquire) == eidx
}

// ─── Stage 200C2A: x86_64 LIVE reply-receive TIMEOUT functional oracle ─────────────────────
//
// Wires the accepted Stage 200C1 reply-timeout transaction into the REAL production recv-v2
// deadline registration + `process_ipc_timeout_deadlines` scan. Two runtime modes prove the two
// live outcomes (timeout wins before NR7; NR7 reply wins before timeout). The activation state is a
// per-boot mode (`0` off, `1` timeout-wins, `2` reply-wins), mutually exclusive with every existing
// slot-5 oracle, plus the oracle's confined reply endpoint index (the ONLY recv-v2 timeout that
// registers a reply-terminal deadline — every ordinary receive stays on its unchanged path).

// Stage 200C2C2C-R2C: the three per-arch selector BASES are the registry — they are all
// visible here because provisioning must name them — but their INTERPRETATION lives in
// `yarm_ipc_abi::ipc_reply_liveness_abi`, which is compiled for the current architecture and
// is the single decoder shared with userspace. These aliases exist so there is exactly one
// numeric source of truth; they must never be decoded by a shared numeric match.

/// Slot-5 selector for the x86_64 reply-timeout oracle. Next free value after the direct/SMP
/// oracles (3/9), so it is mutually exclusive with every other slot-5 oracle.
pub const X86_IPC_REPLY_TIMEOUT_ORACLE_SELECTOR: u64 =
    yarm_ipc_abi::ipc_reply_liveness_abi::X86_64_SELECTOR_BASE as u64;

/// Stage 200C2C1 — slot-5 selector base for the AArch64 reply-timeout oracle. AArch64 slot-5 values
/// 1..=7 are taken (FutexWake=1, FutexWait switch=2, FutexWait idle=3, two-task Yield=4, lone
/// Yield=5, shared-region direct=6, ipccall-direct=7), so this oracle uses the next free PAIR: `8`
/// (timeout-wins) / `9` (reply-wins), mutually exclusive with every other AArch64 slot-5 oracle.
pub const AARCH64_IPC_REPLY_TIMEOUT_ORACLE_SELECTOR: u64 =
    yarm_ipc_abi::ipc_reply_liveness_abi::AARCH64_SELECTOR_BASE as u64;

/// Stage 200C2C2 — slot-5 selector base for the RISC-V reply-timeout oracle. RISC-V slot-5 values
/// 1..=8 are taken (FutexWake=1, FutexWait=2, FutexWait-idle=3, two-task Yield=4, lone Yield=5,
/// queue-switch=6, shared-region direct=7, ipccall-direct=8), so this oracle uses the next free
/// PAIR: `9` (timeout-wins) / `10` (reply-wins), mutually exclusive with every other RISC-V slot-5
/// oracle.
pub const RISCV_IPC_REPLY_TIMEOUT_ORACLE_SELECTOR: u64 =
    yarm_ipc_abi::ipc_reply_liveness_abi::RISCV64_SELECTOR_BASE as u64;

/// Oracle mode discriminator (also written to init startup slot 15 for the userspace scenario):
/// `1` = timeout-wins, `2` = reply-wins.
// ── Stage 200D-0B1: the x86_64 ExitCurrentTask live-oracle activation ───────────────
//
// DEFAULT-OFF and feature-gated. When armed, init spawns ONE disposable, non-essential
// userspace task that calls NR 16 and must never execute another instruction. Every
// literal below lives behind `x86-exit-current-task-oracle`, so a feature-off binary
// contains none of it — while the production syscall, decoder and disposition consumer
// remain compiled unconditionally.
#[cfg(feature = "x86-exit-current-task-oracle")]
static X86_EXIT_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "x86-exit-current-task-oracle")]
pub(crate) fn set_x86_exit_oracle_enabled(on: bool) {
    X86_EXIT_ORACLE_ENABLED.store(on, core::sync::atomic::Ordering::Release);
}

/// `true` only when the feature is built AND the boot knob armed it.
#[must_use]
pub fn x86_exit_oracle_enabled() -> bool {
    #[cfg(feature = "x86-exit-current-task-oracle")]
    {
        X86_EXIT_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
    }
    #[cfg(not(feature = "x86-exit-current-task-oracle"))]
    {
        false
    }
}

/// Startup slot-5 selector for the disposable exit task. Distinct from every
/// reply-liveness selector, so the two oracles remain mutually exclusive.
#[cfg(feature = "x86-exit-current-task-oracle")]
pub const X86_EXIT_CURRENT_TASK_ORACLE_SELECTOR: u64 = 20;

// ── Stage 200D-0C1: the AArch64 ExitCurrentTask live-oracle activation ──────────────
//
// The AArch64 sibling of the block above, with one deliberate difference: the selector
// is NOT hand-written here. It comes from the shared
// `yarm_ipc_abi::exit_current_task_abi` encoder — the exact inverse of the decoder the
// init server applies — so the kernel and userspace ends cannot drift apart the way the
// reply-timeout selectors did in Stage 200C2C2C-R2B.
#[cfg(feature = "aarch64-exit-current-task-oracle")]
static AARCH64_EXIT_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "aarch64-exit-current-task-oracle")]
pub(crate) fn set_aarch64_exit_oracle_enabled(on: bool) {
    AARCH64_EXIT_ORACLE_ENABLED.store(on, core::sync::atomic::Ordering::Release);
}

/// `true` only when the feature is built AND the boot knob armed it.
#[must_use]
pub fn aarch64_exit_oracle_enabled() -> bool {
    #[cfg(feature = "aarch64-exit-current-task-oracle")]
    {
        AARCH64_EXIT_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
    }
    #[cfg(not(feature = "aarch64-exit-current-task-oracle"))]
    {
        false
    }
}

/// The startup slot-5 selector this build's kernel must publish for the disposable exit
/// task, produced by the SHARED ABI encoder rather than a local literal.
#[cfg(feature = "aarch64-exit-current-task-oracle")]
#[must_use]
pub fn aarch64_exit_current_task_selector() -> u64 {
    yarm_ipc_abi::exit_current_task_abi::exit_current_task_selector_for_current_arch(
        yarm_ipc_abi::exit_current_task_abi::ExitCurrentTaskScenario::SelfExit,
    ) as u64
}

// ── Stage 200D-0D1: the RISC-V ExitCurrentTask live-oracle activation ───────────────
//
// The third arch cell, identical in shape to the AArch64 one: the selector is NOT written
// literally here but comes from the shared `yarm_ipc_abi::exit_current_task_abi` encoder,
// the exact inverse of the decoder the init server applies.
#[cfg(feature = "riscv-exit-current-task-oracle")]
static RISCV_EXIT_ORACLE_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "riscv-exit-current-task-oracle")]
pub(crate) fn set_riscv_exit_oracle_enabled(on: bool) {
    RISCV_EXIT_ORACLE_ENABLED.store(on, core::sync::atomic::Ordering::Release);
}

/// `true` only when the feature is built AND the boot knob armed it.
#[must_use]
pub fn riscv_exit_oracle_enabled() -> bool {
    #[cfg(feature = "riscv-exit-current-task-oracle")]
    {
        RISCV_EXIT_ORACLE_ENABLED.load(core::sync::atomic::Ordering::Acquire)
    }
    #[cfg(not(feature = "riscv-exit-current-task-oracle"))]
    {
        false
    }
}

/// The startup slot-5 selector this build's kernel must publish for the disposable exit
/// task, produced by the SHARED ABI encoder rather than a local literal.
#[cfg(feature = "riscv-exit-current-task-oracle")]
#[must_use]
pub fn riscv_exit_current_task_selector() -> u64 {
    yarm_ipc_abi::exit_current_task_abi::exit_current_task_selector_for_current_arch(
        yarm_ipc_abi::exit_current_task_abi::ExitCurrentTaskScenario::SelfExit,
    ) as u64
}

pub const IPC_REPLY_TIMEOUT_MODE_TIMEOUT_WINS: u8 = 1;
pub const IPC_REPLY_TIMEOUT_MODE_REPLY_WINS: u8 = 2;
/// Stage 200D-2B1: the third liveness mode — the authorized replier exits without replying.
pub const IPC_REPLY_TIMEOUT_MODE_SERVER_DIES: u8 = 3;

/// Stage 200C2C2C-R2C — the armed scenario as the TYPED value both sides agree on, or
/// `None` when the oracle is off. The per-boot mode knob is the only input; the numeric
/// slot-5 selector is derived from this by `ipc_reply_timeout_selector_for_current_arch`,
/// so the kernel never hand-writes a selector number.
#[must_use]
pub fn ipc_reply_timeout_scenario()
-> Option<yarm_ipc_abi::ipc_reply_liveness_abi::IpcReplyLivenessScenario> {
    use yarm_ipc_abi::ipc_reply_liveness_abi::IpcReplyLivenessScenario as S;
    match x86_ipc_reply_timeout_oracle_mode() {
        IPC_REPLY_TIMEOUT_MODE_TIMEOUT_WINS => Some(S::TimeoutWins),
        IPC_REPLY_TIMEOUT_MODE_REPLY_WINS => Some(S::ReplyWins),
        IPC_REPLY_TIMEOUT_MODE_SERVER_DIES => Some(S::ServerDies),
        _ => None,
    }
}

/// The slot-5 selector to publish for the armed scenario, decided by the ARCHITECTURE-LOCAL
/// encoder. `None` when the oracle is off.
#[must_use]
pub fn ipc_reply_timeout_selector() -> Option<u64> {
    ipc_reply_timeout_scenario().map(|s| {
        yarm_ipc_abi::ipc_reply_liveness_abi::ipc_reply_liveness_selector_for_current_arch(s) as u64
    })
}

/// Canonical 199E — the slot-5 selector the proof workload runs under.
///
/// With a runtime selector armed this is that selector, unchanged. With NO runtime selector it is
/// the TIMEOUT-WINS scenario, because that is the one scenario whose client blocks on a reply
/// endpoint with a finite timeout and lets the deadline win — i.e. the workload that drives
/// `IpcRecvTimeout → arm_production_reply_deadline → expiry → collector/drain → TimedOut`.
///
/// The encoder is the architecture-local one from the shared ABI, the exact inverse of the
/// decoder userspace applies, so the kernel still never hand-writes a slot-5 number.
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
pub fn ipc_reply_timeout_workload_selector() -> u64 {
    ipc_reply_timeout_selector().unwrap_or_else(|| {
        yarm_ipc_abi::ipc_reply_liveness_abi::ipc_reply_liveness_selector_for_current_arch(
            yarm_ipc_abi::ipc_reply_liveness_abi::IpcReplyLivenessScenario::TimeoutWins,
        ) as u64
    })
}

/// 199E-R1 — the RISC-V proof workload's slot-5 selector.
///
/// Identical to `ipc_reply_timeout_workload_selector` whenever a runtime selector IS armed:
/// the oracle lane is that selector, unchanged, so the accepted `OracleHardware` cell keeps
/// its exact prior encoding and its exact prior userspace behaviour.
///
/// The ONLY difference is the selector-off fallback. There it publishes
/// `ProductionTimeoutWins` instead of `TimeoutWins`, which tells userspace that the deadline
/// under test is the PRODUCTION one — armed by `arm_production_reply_deadline` on the
/// ordinary block path, with no pre-armed confined deadline behind it. That distinction
/// decides the workload's wait strategy: on this port the scheduler tick is driven by the
/// periodic supervisor timer, which is armed at the terminal KERNEL-IDLE boundary, so a
/// yield-spinning server would keep a runnable task on the CPU, never reach idle, never arm
/// the timer and never let the deadline expire. See `IpcReplyLivenessScenario`.
///
/// Deliberately a SEPARATE, RISC-V-only entry point rather than a change to the shared
/// helper: x86_64's provisioning site keeps calling `ipc_reply_timeout_workload_selector`, so
/// its selector-off production cell publishes the same selector it always did and its
/// behaviour is untouched. AArch64 does not call either — its site is unchanged.
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
pub fn riscv_ipc_reply_timeout_workload_selector() -> u64 {
    ipc_reply_timeout_selector().unwrap_or_else(|| {
        yarm_ipc_abi::ipc_reply_liveness_abi::ipc_reply_liveness_selector_for_current_arch(
            yarm_ipc_abi::ipc_reply_liveness_abi::IpcReplyLivenessScenario::ProductionTimeoutWins,
        ) as u64
    })
}

/// Stage 200C2C1 — the monotonic "now" that drives reply-timeout deadlines, per arch.
///
/// U7 (canonical 199E) ungated these: the timeout pipeline they clock is production on every
/// build now, so a deadline timebase that existed only under the oracle feature would leave the
/// promoted scanner with no clock to scan in. The PER-ARCH `cfg`s are untouched — only the
/// current architecture's reader is ever linked.
///
/// x86_64: the periodic LAPIC timer advances the scheduler tick, so the scheduler tick IS the
/// monotonic source (the caller passes it — see `reply_timeout_now_split_read`). AArch64: the port
/// is COOPERATIVE — there is no periodic timer preemption, so the scheduler tick never advances
/// under a user workload. Instead read the always-advancing generic-timer PHYSICAL COUNTER
/// (`CNTPCT_EL0`), scaled down by `REPLY_TIMEOUT_AARCH64_TICK_SHIFT` to a coarse tick. It advances in
/// hardware regardless of IRQ delivery, so the off-lock collector (which runs on EVERY trap, e.g. the
/// oracle's yield loop) observes it advance and finds a DUE deadline. The deadline is armed in the
/// SAME units (`reply_timeout_hw_now() + delta`), so arm and scan share one clock.
#[cfg(target_arch = "aarch64")]
pub(crate) const REPLY_TIMEOUT_AARCH64_TICK_SHIFT: u64 = 16;

/// Stage 200C2C2 — the RISC-V monotonic reply-timeout tick. Like AArch64, the RISC-V port has no
/// reliable periodic scheduler tick under a user workload, so deadlines use the architectural
/// monotonic counter: the `time` CSR (the SBI/`mtime`-backed real-time counter, which is
/// monotonic, never moves backwards, is available before userspace, and is readable from S-mode).
/// It is scaled down by the same shift so the arm and the scan share ONE clock domain. `time` is
/// 64-bit on RV64, so wrap is not reachable in any realistic uptime; the comparison is a plain
/// `now >= deadline` in the scaled domain.
#[cfg(target_arch = "riscv64")]
pub(crate) const REPLY_TIMEOUT_RISCV_TICK_SHIFT: u64 = 13;

#[cfg(target_arch = "riscv64")]
pub(crate) fn reply_timeout_hw_now() -> u64 {
    let t: u64;
    // SAFETY: `time` is a read-only architectural CSR; the read has no side effects.
    unsafe {
        core::arch::asm!("csrr {0}, time", out(reg) t, options(nostack, nomem, preserves_flags));
    }
    t >> REPLY_TIMEOUT_RISCV_TICK_SHIFT
}

/// Read the AArch64 generic-timer physical counter scaled to a coarse reply-timeout tick.
#[cfg(target_arch = "aarch64")]
pub(crate) fn reply_timeout_hw_now() -> u64 {
    let cnt: u64;
    // SAFETY: `CNTPCT_EL0` is a read-only architectural counter register; the read has no side
    // effects and needs no memory/stack clobber.
    unsafe {
        core::arch::asm!("mrs {0}, cntpct_el0", out(reg) cnt, options(nostack, nomem, preserves_flags));
    }
    cnt >> REPLY_TIMEOUT_AARCH64_TICK_SHIFT
}

/// Stage 200C2C1 — the compile-time arch tag stamped into arch-neutral IPC terminal
/// markers (`arch={REPLY_TIMEOUT_ARCH}`), so the SAME emit sites report `x86_64` on the x86
/// build and `aarch64` on the AArch64 build. Only the current architecture's tag is ever
/// linked, so no foreign arch literal leaks into an artifact.
///
/// CLASSIFICATION (Stage 200D-F0): **production mechanism**, not an oracle literal. It is
/// consumed by the ungated server-death completion path, which exists on every build, so
/// gating it on `ipc-reply-timeout-oracle-core` made the feature-off kernel fail to
/// compile on all three architectures. The per-arch `cfg`s are retained; only the feature
/// condition is removed. This widens no oracle marker: the marker STRINGS that embed the
/// tag remain wherever they were already gated.
#[cfg(target_arch = "x86_64")]
pub(crate) const REPLY_TIMEOUT_ARCH: &str = "x86_64";
#[cfg(target_arch = "aarch64")]
pub(crate) const REPLY_TIMEOUT_ARCH: &str = "aarch64";
#[cfg(target_arch = "riscv64")]
pub(crate) const REPLY_TIMEOUT_ARCH: &str = "riscv64";
#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
)))]
pub(crate) const REPLY_TIMEOUT_ARCH: &str = "unknown";

// ── Stage 200C2C2C-R2C: the BOOT-INSTANCE identifier ────────────────────────────────
//
// Every "this marker appears exactly once" assertion in the live runners is only sound if
// the captured log holds exactly ONE boot instance. Banner and kernel-entry counts catch a
// firmware or payload re-entry, but a runner that concatenated two logs, or a guest that
// reset and re-ran an identical boot, would produce identical counts of identical lines.
//
// The nonce closes that: it is read ONCE per boot from the architecture's free-running
// hardware counter and never stored across a reset, so two boot instances cannot produce
// the same value. A runner asserts BOTH that the marker appears once AND that exactly one
// DISTINCT nonce value is present — the second check is what distinguishes "one boot" from
// "two boots that happened to look alike".
static BOOT_INSTANCE_NONCE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static BOOT_INSTANCE_NONCE_EMITTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// The architecture's free-running counter, used only as a boot-instance discriminator.
#[must_use]
fn boot_instance_counter() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: `rdtsc` is unprivileged and has no memory or state effects.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(target_arch = "aarch64")]
    {
        let v: u64;
        // SAFETY: `CNTPCT_EL0` is a readable EL1 system register on every supported core.
        unsafe { core::arch::asm!("mrs {0}, CNTPCT_EL0", out(reg) v, options(nomem, nostack)) };
        v
    }
    #[cfg(target_arch = "riscv64")]
    {
        let v: u64;
        // SAFETY: `time` is an unprivileged read-only CSR.
        unsafe { core::arch::asm!("csrr {0}, time", out(reg) v, options(nomem, nostack)) };
        v
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )))]
    {
        0
    }
}

/// Emit this boot instance's identifier exactly once. Idempotent: later calls are no-ops,
/// so it can be placed on any path that every boot reaches.
pub fn emit_boot_instance_nonce(arch: &str) {
    if BOOT_INSTANCE_NONCE_EMITTED.swap(true, core::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let nonce = boot_instance_counter();
    BOOT_INSTANCE_NONCE.store(nonce, core::sync::atomic::Ordering::Release);
    crate::yarm_log!(
        "YARM_BOOT_INSTANCE arch={} nonce=0x{:016x} result=ok",
        arch,
        nonce
    );
}

static X86_IPC_REPLY_TIMEOUT_ORACLE_MODE: core::sync::atomic::AtomicU8 =
    core::sync::atomic::AtomicU8::new(0);
static IPC_REPLY_TIMEOUT_ORACLE_REP_EIDX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);

/// Arm the reply-timeout oracle mode from the parsed selector (`0` disarms).
pub(crate) fn set_x86_ipc_reply_timeout_oracle_mode(mode: u8) {
    X86_IPC_REPLY_TIMEOUT_ORACLE_MODE.store(mode, core::sync::atomic::Ordering::Release);
}
/// The armed oracle mode (`0` when off).
pub fn x86_ipc_reply_timeout_oracle_mode() -> u8 {
    X86_IPC_REPLY_TIMEOUT_ORACLE_MODE.load(core::sync::atomic::Ordering::Acquire)
}
/// True iff the reply-timeout oracle is armed (either mode).
pub fn x86_ipc_reply_timeout_oracle_enabled() -> bool {
    x86_ipc_reply_timeout_oracle_mode() != 0
}
/// Confine the reply-timeout registration hook to EXACTLY the oracle's reply endpoint.
pub(crate) fn set_ipc_reply_timeout_oracle_reply_endpoint(eidx: usize) {
    IPC_REPLY_TIMEOUT_ORACLE_REP_EIDX.store(eidx, core::sync::atomic::Ordering::Release);
}

/// The reply-wins scenario's armed deadline tick (`0` = unset). Recorded at arm so
/// the production scan can prove it genuinely executed PAST the deadline harmlessly
/// (the reply already disarmed the token, so the late scan wakes nobody).
static IPC_REPLY_TIMEOUT_RW_DEADLINE: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
static IPC_REPLY_TIMEOUT_RW_LATE_EMITTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_ipc_reply_timeout_rw_deadline(tick: u64) {
    IPC_REPLY_TIMEOUT_RW_DEADLINE.store(tick, core::sync::atomic::Ordering::Release);
}
pub(crate) fn ipc_reply_timeout_rw_deadline() -> u64 {
    IPC_REPLY_TIMEOUT_RW_DEADLINE.load(core::sync::atomic::Ordering::Acquire)
}
/// One-shot latch: `true` on the first call, `false` afterwards.
pub(crate) fn ipc_reply_timeout_rw_late_scan_once() -> bool {
    !IPC_REPLY_TIMEOUT_RW_LATE_EMITTED.swap(true, core::sync::atomic::Ordering::AcqRel)
}

// ── Stage 200C2C2C-R2B: the CAUSAL reply-wins collector gate ────────────────────────
//
// The reply-wins scenario must prove that a REPLY wins terminal ownership over a
// TIMEOUT. Before this gate that outcome depended on wall-clock luck: the deadline was
// armed at `now + N` hardware ticks and the server merely had to issue its NR7 "fast
// enough". On the ports without a periodic scheduler tick the collector advances only on
// TRAPS, so the elapsed time between the caller's block and the server's reply is a
// property of QEMU host load, not of the kernel's ownership model — exactly the
// non-determinism Stage 200C2C2C-R1 measured.
//
// The gate replaces that timing dependence with a CAUSAL one. While HELD, the narrow
// collector publishes NO work, so no timeout claimant can reach the terminal cell at
// all. It is armed at reply-wins ARM time — strictly BEFORE any terminal claim is
// possible — and released only when the DebugLog seam observes the oracle client's own
// post-validation marker, i.e. after userspace has compared the delivered reply payload.
// The reply therefore wins because it is the ONLY claimant that could run, and the
// subsequent scan genuinely executes past the (long-since passed) deadline.
//
// STRICTLY TEST-ONLY and doubly confined: the whole mechanism is behind
// `ipc-reply-timeout-oracle-core`, and every entry point additionally requires the
// oracle to be in REPLY-WINS mode. Timeout-wins and every production deadline are
// untouched — the gate is never armed for them.
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
static IPC_REPLY_TIMEOUT_COLLECTOR_HOLD: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

/// Arm the causal gate (reply-wins only). Idempotent; emits the `held` marker once.
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
pub(crate) fn hold_reply_timeout_collector() {
    // Stage 200D-2B1A (§4): the gate now also serves ServerDies. Its job there is the same
    // as in reply-wins — make the winner CAUSAL rather than a timing race — but the winner
    // it protects is `PeerDeath` rather than `Reply`. While held the narrow oracle collector
    // publishes nothing, so no timeout claimant can reach the terminal cell before the
    // server's death has committed. The gate NEVER chooses the winner: it only withholds one
    // claimant's opportunity, and it is released before the stale token is examined, so the
    // late-timeout verdict is reached by the real claim path losing on a real invalidated
    // record. Timeout-wins and every production deadline remain untouched.
    if !matches!(
        x86_ipc_reply_timeout_oracle_mode(),
        IPC_REPLY_TIMEOUT_MODE_REPLY_WINS | IPC_REPLY_TIMEOUT_MODE_SERVER_DIES
    ) {
        return;
    }
    if IPC_REPLY_TIMEOUT_COLLECTOR_HOLD
        .compare_exchange(
            0,
            1,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        crate::yarm_log!(
            "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch={} outcome=held phase=before_terminal_claim result=ok",
            REPLY_TIMEOUT_ARCH
        );
    }
}

/// `true` while the causal gate suppresses collection. Always `false` outside
/// reply-wins, so it can never suppress a production or timeout-wins deadline.
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
pub(crate) fn reply_timeout_collector_held() -> bool {
    IPC_REPLY_TIMEOUT_COLLECTOR_HOLD.load(core::sync::atomic::Ordering::Acquire) == 1
}

/// Stage 200D-2B1A (§5): the armed ServerDies timeout token, recorded so the later scan can
/// prove it examined the SAME token rather than merely observing no wake.
/// `(token_index, token_generation, caller_tid, caller_asid)`; a zero generation means
/// nothing has been armed yet. The generation is what makes the later comparison exact —
/// a slot index alone would be satisfied by a reused registration.
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
static SERVER_DIES_STALE_TOKEN: crate::kernel::lock::SpinLock<(usize, u64, u64, u16)> =
    crate::kernel::lock::SpinLock::new((0, 0, 0, 0));

/// Record the token the ServerDies caller armed. One-shot per boot.
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
pub(crate) fn record_server_dies_stale_token(
    token_index: usize,
    token_generation: u64,
    caller_tid: u64,
    caller_asid: u16,
) {
    if x86_ipc_reply_timeout_oracle_mode() != IPC_REPLY_TIMEOUT_MODE_SERVER_DIES {
        return;
    }
    let mut slot = SERVER_DIES_STALE_TOKEN.lock();
    if slot.1 == 0 {
        *slot = (token_index, token_generation, caller_tid, caller_asid);
        crate::yarm_log!(
            "IPC_SERVER_DEATH_TIMEOUT_ARMED token_index={} token_generation={} caller_tid={} caller_asid={} tokens=1 result=ok",
            token_index,
            token_generation,
            caller_tid,
            caller_asid
        );
    }
}

/// The armed ServerDies token, or `None`.
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
#[must_use]
pub(crate) fn server_dies_stale_token() -> Option<(usize, u64, u64, u16)> {
    let slot = SERVER_DIES_STALE_TOKEN.lock();
    (slot.1 != 0).then_some(*slot)
}

// ── Stage 200D-2B1B-i: exact ServerDies transition counters ─────────────────────────
//
// Nine counters, each incremented BY the real production operation rather than inferred
// from a nearby marker. They observe; they never gate, order or decide anything, and no
// production lock exists for their sake — each is a relaxed atomic add on a path that has
// already committed its transition.
//
// A monotonic SEQ stamps every increment, which is what makes ordering PROVABLE rather
// than assumed: `result_publications` must carry a strictly smaller stamp than
// `caller_enqueues`, so a reordering shows up as a stamp inversion even if both counters
// reach 1. Every class also fails closed on a second increment, because for one armed
// ServerDies instance each transition happens exactly once.
//
// Feature-gated in full. With `ipc-reply-timeout-oracle-core` off, every entry point below
// is absent and the production sites call nothing.
/// Stage 199D — **THE single reverse-link installation decision**, shared by the legacy
/// (`KernelState::register_server_reply_link`) and split
/// (`SharedKernel::register_server_reply_link_split`) paths.
///
/// The two paths used to carry independent copies of this decision, and they drifted: the
/// split twin installed the link but never stamped the system-wide creation edge, while the
/// legacy one did. With the direct NR6 path as the production default, every request then
/// installed a link the ServerDies leak accounting never counted as created, while the close
/// edge still counted — `IPC_SERVER_DEATH_LINK_LEAK created=0 closed=13`. The links were fine;
/// the attestation that would have caught a *real* reverse-link leak was blind.
///
/// Sharing the decision — not merely the stamp — is what makes that unrepeatable: there is one
/// status gate, one set of match arms and one `note_link_created` call, so an installation edge
/// cannot exist without its accounting, and neither can drift from the other.
///
/// Returns whether the server's TCB now holds EXACTLY `link`. The creation edge is stamped
/// **exactly once**, and only on a genuine new installation:
///
/// | case | installs | returns | stamps |
/// | --- | --- | --- | --- |
/// | no link present | yes | `true` | **yes** |
/// | the identical link already present (duplicate retry) | no | `true` | no |
/// | a DIFFERENT live link present | no | `false` | no |
/// | the incarnation has committed to exit | no | `false` | no |
///
/// A missing or foreign TCB never reaches here — both callers resolve the exact
/// `{tid, asid}` incarnation first and return `false` without calling in.
pub(crate) fn install_server_reply_link(
    tcb: &mut crate::kernel::task::ThreadControlBlock,
    link: crate::kernel::task::ServerReplyLink,
) -> bool {
    // Stage 200D-1: an incarnation that has already committed to exit must never be published
    // as an authorized replier. Teardown snapshots the link AFTER the status flips, so a link
    // installed now would never be looked at and the caller would block forever with no death
    // claim. Refusing forces the NR6 publication to roll back, which is the only other
    // permitted outcome of that race.
    if !matches!(
        tcb.status,
        crate::kernel::task::TaskStatus::Runnable
            | crate::kernel::task::TaskStatus::Running
            | crate::kernel::task::TaskStatus::Blocked(_)
    ) {
        return false;
    }
    match tcb.server_reply_link {
        // Idempotent re-registration: the authority already exists, so nothing is installed
        // and nothing is counted. Counting here would double-count a duplicate retry.
        Some(existing) if existing == link => true,
        // A DIFFERENT live link: refuse rather than replace. Replacing would silently retire
        // an authority whose close edge has not run, which is a real leak.
        Some(_) => false,
        None => {
            tcb.server_reply_link = Some(link);
            // The SYSTEM-WIDE creation edge — it fires for every bound `IpcCall`, not just the
            // armed ServerDies one, so it feeds the unscoped leak totals.
            // `note_link_created` adds it to the armed transaction's vector only when the
            // record identities match, which is what stops unrelated calls from being compared
            // against one death's single detach.
            #[cfg(feature = "ipc-reply-timeout-oracle-core")]
            server_dies_counters::note_link_created(
                link.reply_record_index,
                link.reply_record_generation,
            );
            true
        }
    }
}

/// The reply-timeout domain's reverse-link close, re-exported so the fourth closing path has a
/// direct behavioural test rather than only a structural one. It stays a narrow-domain helper
/// with no production callers outside `ipc_state`.
pub(crate) use ipc_state::rt_detach_server_link;
// U9-C: the two rank-3 queued-split recv bodies, re-exported so the `SharedKernel` seams in
// `runtime.rs` drive the SAME implementation the broad `KernelState` methods drive.
pub(crate) use ipc_state::{
    ipc_try_recv_queued_admitted_locked, ipc_try_recv_queued_plain_endpoint_only_locked,
    ipc_try_recv_queued_with_cap_transfer_locked,
};

/// Which reverse link a close is entitled to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkCloseSelector {
    /// Remove the link **only** if it names this exact reply-record incarnation. Every
    /// ordinary terminal close uses this: reply, timeout, caller exit, endpoint destruction,
    /// cancellation and rollback all know which record they are finalizing, and a close that
    /// does not name it has no business removing an authority.
    Exact {
        record_index: usize,
        record_generation: u64,
    },
    /// Remove whatever link is present, whichever record it names.
    ///
    /// Used **only** by the exiting-server take path, which is not finalizing one record —
    /// the whole incarnation is going away, so any authority it still holds must go with it.
    Any,
}

/// The outcome of a reverse-link close.
///
/// Only [`LinkCloseOutcome::Closed`] mutates. Every other variant leaves the slot exactly as
/// it was, so an absent, stale, foreign, repeated or non-matching close is a pure no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkCloseOutcome {
    /// A link was genuinely removed, and is returned. The only mutating result, and the only
    /// one that stamps the close edge.
    Closed(crate::kernel::task::ServerReplyLink),
    /// No link was present — the idempotent repeat case.
    AlreadyAbsent,
    /// The same record slot at a DIFFERENT generation: the slot was reclaimed and reused, so
    /// this close belongs to a previous occupant and must mutate nothing.
    StaleRecordGeneration,
    /// A different outstanding record is live on this server. Removing it would silently
    /// strip a valid authority, so it is refused.
    DifferentLiveLink,
}

impl LinkCloseOutcome {
    /// The removed link, when this call is the one that closed it.
    pub(crate) fn closed(self) -> Option<crate::kernel::task::ServerReplyLink> {
        match self {
            Self::Closed(link) => Some(link),
            Self::AlreadyAbsent | Self::StaleRecordGeneration | Self::DifferentLiveLink => None,
        }
    }
}

/// Stage 199D — **THE single reverse-link CLOSE decision**, shared by all four closing paths:
/// `KernelState::detach_server_reply_link_exact`, `KernelState::take_server_reply_link`,
/// `SharedKernel::unregister_server_reply_link_split` and the reply-timeout domain's
/// `rt_detach_server_link`.
///
/// The exact mirror of [`install_server_reply_link`], and it exists for the same reason. The
/// four paths carried independent copies of this decision and two of them — the direct NR7
/// close and the reply-timeout close — removed links without stamping the close edge, while
/// the other two stamped. Unifying the *creation* edge made that visible: the system totals
/// went to `created=54 closed=13`, the 41 missing closes being exactly the direct NR7
/// completions on that boot, and the leak attestation was wrong on the production path in the
/// permissive direction.
///
/// Sharing the decision — not merely the stamp — is what makes it unrepeatable: one set of
/// match arms and one `note_link_closed` call, on the arm that genuinely removes a link.
///
/// | case | removes | outcome | stamps |
/// | --- | --- | --- | --- |
/// | `Exact`, link matches the record incarnation | yes | `Closed(link)` | **yes** |
/// | `Any`, a link is present | yes | `Closed(link)` | **yes** |
/// | no link present (absent, or a repeated close) | no | `AlreadyAbsent` | no |
/// | `Exact`, same slot at a different generation | no | `StaleRecordGeneration` | no |
/// | `Exact`, a different record is live | no | `DifferentLiveLink` | no |
///
/// A missing or foreign TCB never reaches here — every caller resolves the exact
/// `{tid, asid}` incarnation first and reports its own contract's failure without calling in.
pub(crate) fn close_server_reply_link(
    tcb: &mut crate::kernel::task::ThreadControlBlock,
    selector: LinkCloseSelector,
) -> LinkCloseOutcome {
    let Some(link) = tcb.server_reply_link else {
        return LinkCloseOutcome::AlreadyAbsent;
    };
    if let LinkCloseSelector::Exact {
        record_index,
        record_generation,
    } = selector
        && !link.matches_record(record_index, record_generation)
    {
        // Same slot, later incarnation: the slot was reclaimed and this close belongs to a
        // previous occupant. Distinguished from a wholly different record because the two are
        // very different bugs, and both callers that care report them separately.
        return if link.reply_record_index == record_index {
            LinkCloseOutcome::StaleRecordGeneration
        } else {
            LinkCloseOutcome::DifferentLiveLink
        };
    }
    tcb.server_reply_link = None;
    // The SYSTEM-WIDE close edge, stamped exactly once and only after a genuine
    // `Some(link) -> None` mutation. It is attributed by the REMOVED link's record identity,
    // not by the selector, so the `Any` path reports the record it actually closed and the
    // armed ServerDies vector is moved only when it is that transaction's own link.
    #[cfg(feature = "ipc-reply-timeout-oracle-core")]
    server_dies_counters::note_link_closed(link.reply_record_index, link.reply_record_generation);
    LinkCloseOutcome::Closed(link)
}

#[cfg(feature = "ipc-reply-timeout-oracle-core")]
pub mod server_dies_counters {
    use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    /// The nine transition classes, in the order the successful path must visit them.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Transition {
        LinkCreated = 0,
        LinkDetached = 1,
        DeferredReserved = 2,
        DeferredPublished = 3,
        DeferredConsumed = 4,
        PeerDeathWinner = 5,
        ResultPublication = 6,
        RunnableTransition = 7,
        CallerEnqueue = 8,
    }

    pub const CLASSES: usize = 9;

    impl Transition {
        #[must_use]
        pub const fn name(self) -> &'static str {
            match self {
                Self::LinkCreated => "links_created",
                Self::LinkDetached => "links_detached",
                Self::DeferredReserved => "deferred_reserved",
                Self::DeferredPublished => "deferred_published",
                Self::DeferredConsumed => "deferred_consumed",
                Self::PeerDeathWinner => "peer_death_winners",
                Self::ResultPublication => "result_publications",
                Self::RunnableTransition => "runnable_transitions",
                Self::CallerEnqueue => "caller_enqueues",
            }
        }
    }

    static COUNTS: [AtomicU32; CLASSES] = [const { AtomicU32::new(0) }; CLASSES];
    static STAMPS: [AtomicU64; CLASSES] = [const { AtomicU64::new(0) }; CLASSES];
    static SEQ: AtomicU64 = AtomicU64::new(0);
    /// Instance id: bumped by `reset_instance`, so one armed oracle (or one hosted test)
    /// can never inherit another's counts.
    static INSTANCE: AtomicU64 = AtomicU64::new(0);

    // ── Stage 199D: the two tiers the original single pair was conflating ─────────────
    //
    // `LinkCreated` used to be incremented by EVERY bound `IpcCall` in the system while
    // `LinkDetached` was incremented only by the one dying server's teardown, and
    // `audit_success_path` compared the two. On a real boot that is `created=54
    // detached=1` — the `IPC_SERVER_DEATH_LINK_LEAK` failure. It was never a leak: the
    // ordinary reply path does close its links, through `detach_server_reply_link_exact`,
    // which counted nothing at all. One pair of counters was being asked to answer two
    // different questions, and could answer neither.
    //
    // They are now separate:
    //
    //   TIER 1 — `LINKS_CREATED_TOTAL` / `LINKS_CLOSED_TOTAL`, unscoped, incremented by
    //   EVERY genuine installation and by EVERY genuine removal at BOTH closing edges.
    //   This is the real reverse-link leak invariant and it is what `LINK_LEAK` now
    //   compares. A link created anywhere and never closed still fails the audit.
    //
    //   TIER 2 — the nine-vector, scoped to the ONE armed ServerDies transaction by the
    //   reply record `{index, generation}` it owns. Unrelated earlier or later calls
    //   cannot contaminate it, because they carry a different record identity.

    /// The reply record the armed ServerDies transaction owns: `(index, generation)`.
    /// A zero generation means unarmed — reply-record generations start at 1, so zero is
    /// unambiguous. Bounded, no allocation, no lock: the arm is one-shot per instance.
    static ARMED_RECORD_INDEX: AtomicU64 = AtomicU64::new(0);
    static ARMED_RECORD_GENERATION: AtomicU64 = AtomicU64::new(0);

    /// Tier-1 unscoped link-lifecycle totals.
    static LINKS_CREATED_TOTAL: AtomicU32 = AtomicU32::new(0);
    static LINKS_CLOSED_TOTAL: AtomicU32 = AtomicU32::new(0);

    /// Begin a fresh instance. Deterministic: every class returns to zero, the stamp
    /// clock restarts, the armed record is cleared and both link totals reset, so a test
    /// observes only its own transitions.
    pub fn reset_instance() -> u64 {
        for i in 0..CLASSES {
            COUNTS[i].store(0, Ordering::Release);
            STAMPS[i].store(0, Ordering::Release);
        }
        SEQ.store(0, Ordering::Release);
        ARMED_RECORD_INDEX.store(0, Ordering::Release);
        ARMED_RECORD_GENERATION.store(0, Ordering::Release);
        LINKS_CREATED_TOTAL.store(0, Ordering::Release);
        LINKS_CLOSED_TOTAL.store(0, Ordering::Release);
        INSTANCE.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Arm this instance to the reply record the ServerDies transaction owns. One-shot:
    /// a second arm with a DIFFERENT record is refused and reported, so two overlapping
    /// scenarios can never share a vector. Re-arming the identical record is idempotent.
    ///
    /// Returns `true` when the instance is armed to `{index, generation}` afterwards.
    pub fn arm_record(index: usize, generation: u64) -> bool {
        if generation == 0 {
            return false;
        }
        match armed_record() {
            None => {
                ARMED_RECORD_INDEX.store(index as u64, Ordering::Release);
                ARMED_RECORD_GENERATION.store(generation, Ordering::Release);
                true
            }
            Some((i, g)) if i == index && g == generation => true,
            Some((i, g)) => {
                crate::yarm_log!(
                    "IPC_SERVER_DEATH_SCOPE_CONFLICT armed_index={} armed_generation={} offered_index={} offered_generation={} result=fail",
                    i,
                    g,
                    index,
                    generation
                );
                false
            }
        }
    }

    /// The armed reply record, or `None` while unarmed.
    #[must_use]
    pub fn armed_record() -> Option<(usize, u64)> {
        let g = ARMED_RECORD_GENERATION.load(Ordering::Acquire);
        (g != 0).then(|| (ARMED_RECORD_INDEX.load(Ordering::Acquire) as usize, g))
    }

    /// Whether `{index, generation}` is the armed transaction's record.
    #[must_use]
    pub fn matches_armed(index: usize, generation: u64) -> bool {
        armed_record() == Some((index, generation))
    }

    /// Tier-1 totals: `(created, closed)` across every reverse link in the system.
    #[must_use]
    pub fn link_totals() -> (u32, u32) {
        (
            LINKS_CREATED_TOTAL.load(Ordering::Acquire),
            LINKS_CLOSED_TOTAL.load(Ordering::Acquire),
        )
    }

    /// A reverse link was genuinely installed for `{index, generation}`.
    ///
    /// Always tier-1. Tier-2 only when it is the armed record's link — which, in the
    /// production ordering, it is not: the link is installed while the reply record is
    /// created, and the transaction only arms later, when the caller blocks. The scoped
    /// arm therefore attributes the creation edge itself (see `note_armed_link_present`).
    /// This branch is the safety net if that ordering ever changes, and it is what keeps
    /// the two paths from double-counting.
    pub fn note_link_created(index: usize, generation: u64) {
        LINKS_CREATED_TOTAL.fetch_add(1, Ordering::AcqRel);
        if matches_armed(index, generation) {
            record(Transition::LinkCreated);
        }
    }

    /// The armed record's link was observed present at arm time.
    ///
    /// This is the scoped creation edge. It is a genuine observation of live kernel state
    /// — teardown has not run, and the link is read back from the bound replier's TCB —
    /// not an inference from the detach. If the link is absent the caller does not call
    /// this, `LinkCreated` stays 0 and the audit fails, which is exactly the "armed a
    /// record that owns no reverse link" defect.
    pub fn note_armed_link_present(index: usize, generation: u64) {
        if matches_armed(index, generation) {
            record(Transition::LinkCreated);
        }
    }

    /// A reverse link was genuinely removed for `{index, generation}`, at either closing
    /// edge — the ordinary terminal detach or the exit-path teardown.
    ///
    /// Always tier-1. Tier-2 only for the armed record. A close for a DIFFERENT record
    /// while armed is reported and deliberately not counted: it belongs to another
    /// transaction and must not move this vector.
    pub fn note_link_closed(index: usize, generation: u64) {
        LINKS_CLOSED_TOTAL.fetch_add(1, Ordering::AcqRel);
        if matches_armed(index, generation) {
            record(Transition::LinkDetached);
        } else if let Some((ai, ag)) = armed_record() {
            crate::yarm_log!(
                "IPC_SERVER_DEATH_FOREIGN_LINK_CLOSE armed_index={} armed_generation={} closed_index={} closed_generation={} counted=0 result=ok",
                ai,
                ag,
                index,
                generation
            );
        }
    }

    #[must_use]
    pub fn instance() -> u64 {
        INSTANCE.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn count(t: Transition) -> u32 {
        COUNTS[t as usize].load(Ordering::Acquire)
    }

    #[must_use]
    pub fn stamp(t: Transition) -> u64 {
        STAMPS[t as usize].load(Ordering::Acquire)
    }

    /// The full nine-element vector, for exact assertions.
    #[must_use]
    pub fn vector() -> [u32; CLASSES] {
        let mut v = [0u32; CLASSES];
        for i in 0..CLASSES {
            v[i] = COUNTS[i].load(Ordering::Acquire);
        }
        v
    }

    /// Record one real transition. Called FROM the production operation that performed it.
    ///
    /// A second increment for the same class in one instance is a defect, not a tolerated
    /// duplicate: it emits the class's canonical hard-fail literal and the count is left
    /// visibly >1 so an assertion cannot miss it.
    pub fn record(t: Transition) {
        let idx = t as usize;
        let seq = SEQ.fetch_add(1, Ordering::AcqRel) + 1;
        let prev = COUNTS[idx].fetch_add(1, Ordering::AcqRel);
        STAMPS[idx].store(seq, Ordering::Release);
        if prev != 0 {
            match t {
                Transition::DeferredPublished => crate::yarm_log!(
                    "IPC_SERVER_DEATH_DUPLICATE_DEFERRED class={} count={} result=fail",
                    t.name(),
                    prev + 1
                ),
                Transition::PeerDeathWinner | Transition::ResultPublication => {
                    crate::yarm_log!(
                        "IPC_SERVER_DEATH_DUPLICATE_COMPLETION class={} count={} result=fail",
                        t.name(),
                        prev + 1
                    )
                }
                Transition::RunnableTransition | Transition::CallerEnqueue => crate::yarm_log!(
                    "IPC_SERVER_DEATH_DUPLICATE_WAKE class={} count={} result=fail",
                    t.name(),
                    prev + 1
                ),
                _ => crate::yarm_log!(
                    "IPC_SERVER_DEATH_DUPLICATE_TRANSITION class={} count={} result=fail",
                    t.name(),
                    prev + 1
                ),
            }
        }
    }

    /// `true` when the result publication is stamped strictly before the caller enqueue.
    /// Both must have happened; a missing stamp is never "in order".
    #[must_use]
    pub fn result_before_enqueue() -> bool {
        let r = stamp(Transition::ResultPublication);
        let e = stamp(Transition::CallerEnqueue);
        r != 0 && e != 0 && r < e
    }

    /// Audit the instance against the successful-path contract and emit the canonical
    /// hard-fail literal for whichever invariant broke. Returns `true` when every class is
    /// exactly 1 and the publication/enqueue order holds.
    ///
    /// The leak literals are derived from COUNT RELATIONSHIPS, not from a separate scan:
    /// a link created but never closed IS a link leak, and a deferred item published but
    /// never consumed IS a deferred leak.
    ///
    /// The link-leak literal reads the TIER-1 totals — every reverse link in the system,
    /// closed at either edge — because that, and not the armed transaction's pair, is the
    /// question "did a reverse link leak?" actually asks. The armed transaction's own
    /// `LinkCreated` / `LinkDetached` are checked by the exact-1 loop below like every
    /// other class.
    #[must_use]
    pub fn audit_success_path() -> bool {
        use Transition as T;
        let mut ok = true;
        let (links_created, links_closed) = link_totals();
        if links_created != links_closed {
            crate::yarm_log!(
                "IPC_SERVER_DEATH_LINK_LEAK created={} closed={} scope=system result=fail",
                links_created,
                links_closed
            );
            ok = false;
        }
        if armed_record().is_none() {
            crate::yarm_log!("IPC_SERVER_DEATH_SCOPE_UNARMED result=fail");
            ok = false;
        }
        if count(T::DeferredPublished) != count(T::DeferredConsumed) {
            crate::yarm_log!(
                "IPC_SERVER_DEATH_DEFERRED_LEAK published={} consumed={} result=fail",
                count(T::DeferredPublished),
                count(T::DeferredConsumed)
            );
            ok = false;
        }
        if count(T::DeferredReserved) < count(T::DeferredPublished) {
            crate::yarm_log!(
                "IPC_SERVER_DEATH_DEFERRED_LEAK reserved={} published={} reason=publish_without_reserve result=fail",
                count(T::DeferredReserved),
                count(T::DeferredPublished)
            );
            ok = false;
        }
        // A detached link whose record never reached a terminal winner leaves the reply
        // record owner-less — that is the record leak this literal names.
        if count(T::LinkDetached) != count(T::PeerDeathWinner) {
            crate::yarm_log!(
                "IPC_SERVER_DEATH_RECORD_LEAK detached={} peer_death_winners={} result=fail",
                count(T::LinkDetached),
                count(T::PeerDeathWinner)
            );
            ok = false;
        }
        for t in [
            T::LinkCreated,
            T::LinkDetached,
            T::DeferredReserved,
            T::DeferredPublished,
            T::DeferredConsumed,
            T::PeerDeathWinner,
            T::ResultPublication,
            T::RunnableTransition,
            T::CallerEnqueue,
        ] {
            if count(t) != 1 {
                crate::yarm_log!(
                    "IPC_SERVER_DEATH_TRANSITION_COUNT class={} count={} expected=1 result=fail",
                    t.name(),
                    count(t)
                );
                ok = false;
            }
        }
        if !result_before_enqueue() {
            crate::yarm_log!(
                "IPC_SERVER_DEATH_DUPLICATE_WAKE reason=result_not_before_enqueue result_stamp={} enqueue_stamp={} result=fail",
                stamp(T::ResultPublication),
                stamp(T::CallerEnqueue)
            );
            ok = false;
        }
        ok
    }
}

/// One-shot latch so the stale-token scan attests exactly once.
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
static SERVER_DIES_STALE_SCAN_DONE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "ipc-reply-timeout-oracle-core")]
#[must_use]
pub(crate) fn server_dies_stale_scan_once() -> bool {
    !SERVER_DIES_STALE_SCAN_DONE.swap(true, core::sync::atomic::Ordering::AcqRel)
}

/// The DebugLog-seam release: the oracle client logs this marker only AFTER comparing
/// the delivered reply payload, so observing it here is proof that userspace validated
/// the reply before any timeout claimant could run. Same idiom as the Stage 199A2D2C
/// cross-CPU seals. One-shot.
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
pub(crate) fn maybe_release_reply_timeout_collector_gate(msg: &str) {
    if !matches!(
        x86_ipc_reply_timeout_oracle_mode(),
        IPC_REPLY_TIMEOUT_MODE_REPLY_WINS | IPC_REPLY_TIMEOUT_MODE_SERVER_DIES
    ) {
        return;
    }
    // Stage 200D-2B1A (§4): each scenario is released by ITS OWN userspace post-validation
    // marker, so the release always means "userspace has already validated the winner".
    // ServerDies releases on the caller's `IPC_SERVER_DEATH_USER_VALIDATED ... code=10`,
    // which the caller logs only after comparing the numeric canonical code.
    let released_by_reply =
        msg.starts_with("IPC_REPLY_TIMEOUT_ORACLE_CLIENT_REPLY_RECV") && msg.contains("reply_ok=1");
    let released_by_server_death =
        msg.starts_with("IPC_SERVER_DEATH_USER_VALIDATED") && msg.contains("code=10");
    if !released_by_reply && !released_by_server_death {
        return;
    }
    if IPC_REPLY_TIMEOUT_COLLECTOR_HOLD
        .compare_exchange(
            1,
            0,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        crate::yarm_log!(
            "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch={} outcome=released trigger=userspace_reply_validated result=ok",
            REPLY_TIMEOUT_ARCH
        );
    }
}

/// Off-feature no-op so the DebugLog seams stay unconditional call sites.
#[cfg(not(feature = "ipc-reply-timeout-oracle-core"))]
pub(crate) fn maybe_release_reply_timeout_collector_gate(_msg: &str) {}

/// One-shot latch for the quiescent link-balance attestation.
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
static SERVER_DIES_LINK_BALANCE_DONE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 199D — the ServerDies scenario's QUIESCENT reverse-link balance attestation.
///
/// Read-only and one-shot. It reports; it repairs nothing, claims nothing and mutates no
/// accounting state. The completion-time `IPC_SERVER_DEATH_TRANSITION_AUDIT` already proves
/// the armed transaction's own 1/1 pair; this answers the separate, system-wide question at
/// a point where the scenario is finished.
///
/// **"After no audited call remains open" is enforced, not assumed.** The trigger is the
/// surviving caller's own final marker, which it logs only after validating `ServerDied` and
/// completing its survivor loop; and the reading is additionally gated on
/// `live_server_reply_link_count() == 0`, so a still-outstanding reverse link defers the
/// attestation rather than reporting a balance that a later close would change. If links are
/// still live the latch is NOT consumed, so a later trigger can still produce the reading.
///
/// Emitted from the off-lock DebugLog split path, alongside the existing collector-gate
/// release, so it costs an ordinary boot nothing.
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
pub(crate) fn maybe_emit_server_dies_link_balance(msg: &str, live_links: usize) {
    if x86_ipc_reply_timeout_oracle_mode() != IPC_REPLY_TIMEOUT_MODE_SERVER_DIES {
        return;
    }
    if !(msg.starts_with("IPC_SERVER_DEATH_SCENARIO_DONE") && msg.contains("server_died=1")) {
        return;
    }
    if SERVER_DIES_LINK_BALANCE_DONE.load(core::sync::atomic::Ordering::Acquire) {
        return;
    }
    if live_links != 0 {
        // Not quiescent yet: an audited call is still open. Report the deferral rather than
        // publishing a balance that is not final, and leave the latch unconsumed.
        crate::yarm_log!(
            "IPC_SERVER_DEATH_LINK_BALANCE_DEFERRED live_links={} reason=audited_call_open result=ok",
            live_links
        );
        return;
    }
    if SERVER_DIES_LINK_BALANCE_DONE.swap(true, core::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let (created, closed) = server_dies_counters::link_totals();
    crate::yarm_log!(
        "IPC_SERVER_DEATH_LINK_BALANCE_QUIESCENT created={} closed={} live_links=0 scope=system result={}",
        created,
        closed,
        if created == closed { "ok" } else { "fail" }
    );
}

#[cfg(not(feature = "ipc-reply-timeout-oracle-core"))]
pub(crate) fn maybe_emit_server_dies_link_balance(_msg: &str, _live_links: usize) {}

// ── Stage 200D-2A: per-CPU bounded DEFERRED SERVER-DEATH work queue ─────────────────
//
// The sibling of the Stage 200C2B reply-timeout queue below, with the SAME ownership
// model — a per-CPU bounded array under its own IRQ-safe lock, never the broad
// `SpinLock<KernelState>` — but deliberately NOT behind the oracle feature: server death
// is production behaviour on every build, whereas the reply-timeout collector is an
// oracle-gated proof path.
//
// Why a deferred queue at all: `exit_task` runs under the broad lock. Claiming PeerDeath,
// publishing the caller's result and enqueueing it there is correct but keeps the whole
// completion inside the broad lock, which is exactly the unlocking this stage removes.
// Teardown now only RESERVES a slot, detaches the exact link and publishes an immutable
// generation-bearing item; the post-lock drain does all the authority work.
//
// Reservation precedes the irreversible link detach on purpose: a detached link with no
// deferred owner would strand the caller blocked forever, so capacity is proven available
// before the link stops being the owner.

/// One owned unit of deferred server-death completion work.
///
/// Everything here is immutable and generation-bearing: no raw pointer, no borrowed TCB
/// reference, no userspace pointer, and no second authoritative completion state — the
/// terminal cell remains the only authority. `exiting_server` is a full `{tid, asid}`
/// identity, so a replacement incarnation reusing the numeric TID can never inherit this
/// item's authority, and `reply_record_generation` makes a reclaimed-and-reused record
/// slot detectable at drain time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeferredServerDeathCompletion {
    /// The exact exiting replier incarnation, captured while its TCB was still stable.
    pub exiting_server: ReceiverWaiterIdentity,
    /// The reply record it owed, by slot.
    pub reply_record_index: usize,
    /// …and by generation, so a reused slot is a stale item rather than a wrong target.
    pub reply_record_generation: u64,
}

/// Per-CPU deferred server-death slots, bounded by the reply-record store itself: there
/// cannot be more outstanding records than slots, so the queue cannot overflow through
/// legitimate use. No allocation happens on the teardown path.
pub(crate) const SD_POST_WORK_SLOTS: usize = MAX_REPLY_CAPS;

static SERVER_DEATH_POST_WORK: [crate::kernel::lock::SpinLockIrq<
    [Option<DeferredServerDeathCompletion>; SD_POST_WORK_SLOTS],
>; crate::kernel::scheduler::MAX_CPUS] =
    [const { crate::kernel::lock::SpinLockIrq::new([None; SD_POST_WORK_SLOTS]) };
        crate::kernel::scheduler::MAX_CPUS];

static SERVER_DEATH_WORK_PUBLISHED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
static SERVER_DEATH_WORK_DRAINED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// A proof that one queue slot is available for `cpu_idx`. Held across the link detach so
/// the handoff can never lose the work; consuming it publishes, dropping it releases.
#[derive(Debug)]
#[must_use = "a reservation must be published or explicitly released"]
pub(crate) struct ServerDeathWorkReservation {
    cpu_idx: usize,
    slot: usize,
}

impl ServerDeathWorkReservation {
    pub(crate) fn slot(&self) -> usize {
        self.slot
    }
}

/// Reserve one slot BEFORE the reverse link is detached. `None` when the queue is full —
/// teardown then leaves the link attached and does not detach, so the record keeps an
/// exact owner rather than being stranded.
pub(crate) fn server_death_work_reserve(cpu_idx: usize) -> Option<ServerDeathWorkReservation> {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    let mut q = SERVER_DEATH_POST_WORK[cpu_idx].lock();
    let slot = q.iter().position(|s| s.is_none())?;
    // Mark the slot taken with a placeholder the drain skips: a reserved-but-unpublished
    // slot must not be drainable, and must not be handed out twice.
    q[slot] = Some(DeferredServerDeathCompletion {
        exiting_server: ReceiverWaiterIdentity::new(crate::kernel::ipc::ThreadId(0), Asid(0)),
        reply_record_index: usize::MAX,
        reply_record_generation: 0,
    });
    // Stage 200D-2B1B-i (class 3): a slot was really taken. Recorded here and NOT at the
    // call site, so a reservation that fails (queue full) records nothing.
    #[cfg(feature = "ipc-reply-timeout-oracle-core")]
    server_dies_counters::record(server_dies_counters::Transition::DeferredReserved);
    Some(ServerDeathWorkReservation { cpu_idx, slot })
}

/// Publish into a held reservation. A DUPLICATE (an item for the same record slot and
/// generation already queued and not yet drained) collapses to ONE owner: the reservation
/// is released and `false` is returned, so a repeated task-exit notification cannot
/// produce two deferred items. A different record can never overwrite a pending item
/// because each reservation owns its own slot.
pub(crate) fn server_death_work_publish(
    reservation: ServerDeathWorkReservation,
    work: DeferredServerDeathCompletion,
) -> bool {
    let mut q = SERVER_DEATH_POST_WORK[reservation.cpu_idx].lock();
    let duplicate = q.iter().enumerate().any(|(i, s)| {
        i != reservation.slot
            && s.is_some_and(|w| {
                w.reply_record_index == work.reply_record_index
                    && w.reply_record_generation == work.reply_record_generation
            })
    });
    if duplicate {
        q[reservation.slot] = None;
        return false;
    }
    q[reservation.slot] = Some(work);
    SERVER_DEATH_WORK_PUBLISHED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    // Stage 200D-2B1B-i (class 4): the item is now queued. The duplicate branch above
    // returned early, so a collapsed duplicate never reaches this counter.
    #[cfg(feature = "ipc-reply-timeout-oracle-core")]
    server_dies_counters::record(server_dies_counters::Transition::DeferredPublished);
    true
}

/// Release a reservation without publishing (a failpoint or an aborted handoff).
pub(crate) fn server_death_work_release(reservation: ServerDeathWorkReservation) {
    SERVER_DEATH_POST_WORK[reservation.cpu_idx].lock()[reservation.slot] = None;
}

/// Drain the next PUBLISHED item for `cpu_idx`. Reserved-but-unpublished placeholders are
/// skipped, so a reservation in flight is never drained as if it were work.
pub(crate) fn server_death_work_drain_next(
    cpu_idx: usize,
) -> Option<DeferredServerDeathCompletion> {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    let mut q = SERVER_DEATH_POST_WORK[cpu_idx].lock();
    let idx = q
        .iter()
        .position(|s| s.is_some_and(|w| w.reply_record_index != usize::MAX))?;
    let taken = q[idx].take();
    if taken.is_some() {
        SERVER_DEATH_WORK_DRAINED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        // Stage 200D-2B1B-i (class 5): one item left the queue. A second drain of the same
        // record finds nothing here, so consumption cannot be double-counted.
        #[cfg(feature = "ipc-reply-timeout-oracle-core")]
        server_dies_counters::record(server_dies_counters::Transition::DeferredConsumed);
    }
    taken
}

/// Number of PUBLISHED (drainable) items queued for `cpu_idx`.
pub(crate) fn server_death_work_len(cpu_idx: usize) -> usize {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return 0;
    }
    SERVER_DEATH_POST_WORK[cpu_idx]
        .lock()
        .iter()
        .filter(|s| s.is_some_and(|w| w.reply_record_index != usize::MAX))
        .count()
}

/// Clear every slot for `cpu_idx` (hosted test isolation only).
pub(crate) fn server_death_work_clear(cpu_idx: usize) {
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        for slot in SERVER_DEATH_POST_WORK[cpu_idx].lock().iter_mut() {
            *slot = None;
        }
    }
}

/// Stage 200D-0 — is at least one deferred server-death slot free for `cpu_idx`?
///
/// A non-mutating probe used by `ExitCurrentTask` BEFORE anything irreversible happens: a
/// task that still owes a reply must not be allowed to reach the point of no return unless
/// its deferred completion can be handed off, or the blocked caller would be stranded.
pub fn server_death_work_capacity_available(cpu_idx: usize) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    SERVER_DEATH_POST_WORK[cpu_idx]
        .lock()
        .iter()
        .any(|s| s.is_none())
}

// ── Stage 200D-0A: the TYPED non-returning trap disposition ─────────────────────────
//
// A successful `ExitCurrentTask` abandons its own syscall frame. Rather than have each
// architecture infer that from missing scheduler state — which is how divergent,
// duplicated lifecycle logic creeps into trap paths — the syscall publishes an explicit
// per-CPU typed disposition and every arch return path will consume it the same way:
//
//   do not restore the exiting frame
//   drain post-lock work
//   dispatch the audited replacement, or enter idle
//
// This is CONTROL-FLOW OWNERSHIP, not a second lifecycle authority: `exit_task` remains
// the only teardown authority, and nothing here mutates task, record or terminal state.
//
// It is generation-bearing (`{tid, asid}`, not a bare TID or a global boolean), per-CPU,
// and single-shot. A duplicate publication is REJECTED rather than overwriting, so a
// second exit on the same CPU cannot silently displace a pending one.

/// What the post-lock trap path must do with the frame that just trapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostLockTrapDisposition {
    /// The ordinary case: finalize the syscall result and restore the same frame.
    ReturnNormally,
    /// The task that trapped has exited. Its frame must never be restored; the post-lock
    /// path drains deferred work and dispatches a replacement or idles.
    CurrentTaskExited { tid: u64, asid: Asid },
}

/// Per-CPU slot. `None` means `ReturnNormally` — the overwhelmingly common case costs a
/// single relaxed load and no allocation.
static POST_LOCK_TRAP_DISPOSITION: [crate::kernel::lock::SpinLockIrq<Option<(u64, Asid)>>;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { crate::kernel::lock::SpinLockIrq::new(None) }; crate::kernel::scheduler::MAX_CPUS];

/// Publish `CurrentTaskExited` for `cpu_idx`. Returns `false` WITHOUT overwriting when a
/// disposition is already pending — a duplicate publication is a bug, not a last-writer-wins
/// race, and the caller must be able to see that it lost.
#[must_use]
pub fn publish_current_task_exited(cpu_idx: usize, tid: u64, asid: Asid) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    let mut slot = POST_LOCK_TRAP_DISPOSITION[cpu_idx].lock();
    if slot.is_some() {
        return false;
    }
    *slot = Some((tid, asid));
    true
}

/// Consume the disposition for `cpu_idx`. One-shot: the second caller sees
/// `ReturnNormally`, so a normally returning syscall can never act on a stale exit.
#[must_use]
pub fn take_post_lock_trap_disposition(cpu_idx: usize) -> PostLockTrapDisposition {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return PostLockTrapDisposition::ReturnNormally;
    }
    match POST_LOCK_TRAP_DISPOSITION[cpu_idx].lock().take() {
        Some((tid, asid)) => PostLockTrapDisposition::CurrentTaskExited { tid, asid },
        None => PostLockTrapDisposition::ReturnNormally,
    }
}

/// Non-consuming peek (assertions/telemetry only).
#[must_use]
pub fn post_lock_trap_disposition_pending(cpu_idx: usize) -> bool {
    cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && POST_LOCK_TRAP_DISPOSITION[cpu_idx].lock().is_some()
}

/// Clear the slot (hosted test isolation only).
#[cfg(any(test, feature = "hosted-dev"))]
pub fn clear_post_lock_trap_disposition(cpu_idx: usize) {
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        *POST_LOCK_TRAP_DISPOSITION[cpu_idx].lock() = None;
    }
}

// ── Stage 200D-0B3: the bounded exit-attestation latch ──────────────────────────────
//
// Stage 200D-0B1 attested "broad lock released" and "post-lock drain done" from INSIDE
// `SharedKernel::with_cpu`, where neither was true, and Stage 200D-0B2 sealed a live boot
// against those claims. The correction is not a rewording: each of the three claims now has
// to be emitted from the place that actually performs it, and those places are three
// different stack frames spanning the lock boundary.
//
// This latch is what lets a later frame know that THIS trap accepted an exit. It carries no
// authority whatsoever: no scheduling, teardown, frame selection or terminal-claim decision
// reads it. Its only consumers are markers and one fail-closed stage check, so an
// attestation can never run ahead of the operation it describes.
//
// `stage` is monotonic within one trap:
//   0 = idle           no accepted exit on this CPU
//   1 = consumed       the in-lock consumer validated the identity and prepared the owner
//   2 = lock_released  `with_cpu` has returned; the broad guard is dropped
//   3 = drained        every shared post-lock drain has completed
// The vector epilogue consumes the latch back to 0. A frame that finds an unexpected stage
// emits `EXIT_TASK_TRAP_DEPTH_ERROR` rather than a reassuring marker.
pub const EXIT_ATTEST_IDLE: u8 = 0;
pub const EXIT_ATTEST_CONSUMED: u8 = 1;
pub const EXIT_ATTEST_LOCK_RELEASED: u8 = 2;
pub const EXIT_ATTEST_DRAINED: u8 = 3;

static EXIT_ATTEST_STAGE: [core::sync::atomic::AtomicU8; crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU8::new(EXIT_ATTEST_IDLE) };
        crate::kernel::scheduler::MAX_CPUS];
static EXIT_ATTEST_TID: [core::sync::atomic::AtomicU64; crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; crate::kernel::scheduler::MAX_CPUS];
static EXIT_ATTEST_ASID: [core::sync::atomic::AtomicU16; crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU16::new(0) }; crate::kernel::scheduler::MAX_CPUS];

/// Arm the latch from the in-lock consumer. Returns `false` if one is already armed for this
/// CPU — a second accepted exit in one trap is a defect, not a last-writer-wins race.
#[must_use]
pub fn arm_exit_attestation(cpu_idx: usize, tid: u64, asid: Asid) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    use core::sync::atomic::Ordering;
    if EXIT_ATTEST_STAGE[cpu_idx]
        .compare_exchange(
            EXIT_ATTEST_IDLE,
            EXIT_ATTEST_CONSUMED,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return false;
    }
    EXIT_ATTEST_TID[cpu_idx].store(tid, Ordering::Release);
    EXIT_ATTEST_ASID[cpu_idx].store(asid.0, Ordering::Release);
    true
}

/// Advance the latch exactly one stage. Returns the armed `{tid, asid}` on success, or `None`
/// when this CPU has no armed attestation at `expected` — which is the ordinary case for every
/// trap that is not an accepted exit, and makes the marker sites strict no-ops there.
#[must_use]
pub fn advance_exit_attestation(cpu_idx: usize, expected: u8, next: u8) -> Option<(u64, Asid)> {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    use core::sync::atomic::Ordering;
    EXIT_ATTEST_STAGE[cpu_idx]
        .compare_exchange(expected, next, Ordering::AcqRel, Ordering::Acquire)
        .ok()?;
    Some((
        EXIT_ATTEST_TID[cpu_idx].load(Ordering::Acquire),
        Asid(EXIT_ATTEST_ASID[cpu_idx].load(Ordering::Acquire)),
    ))
}

/// Read the current stage without changing it.
#[must_use]
pub fn exit_attestation_stage(cpu_idx: usize) -> u8 {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return EXIT_ATTEST_IDLE;
    }
    EXIT_ATTEST_STAGE[cpu_idx].load(core::sync::atomic::Ordering::Acquire)
}

/// Consume the latch in the vector epilogue, returning the armed identity and the stage it
/// had reached. `None` when nothing was armed.
#[must_use]
pub fn take_exit_attestation(cpu_idx: usize) -> Option<(u64, Asid, u8)> {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    use core::sync::atomic::Ordering;
    let stage = EXIT_ATTEST_STAGE[cpu_idx].swap(EXIT_ATTEST_IDLE, Ordering::AcqRel);
    if stage == EXIT_ATTEST_IDLE {
        return None;
    }
    Some((
        EXIT_ATTEST_TID[cpu_idx].load(Ordering::Acquire),
        Asid(EXIT_ATTEST_ASID[cpu_idx].load(Ordering::Acquire)),
        stage,
    ))
}

/// Clear the latch (hosted test isolation only).
#[cfg(any(test, feature = "hosted-dev"))]
pub fn clear_exit_attestation(cpu_idx: usize) {
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        EXIT_ATTEST_STAGE[cpu_idx].store(EXIT_ATTEST_IDLE, core::sync::atomic::Ordering::Release);
    }
}

pub(crate) fn server_death_work_published_count() -> u64 {
    SERVER_DEATH_WORK_PUBLISHED.load(core::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn server_death_work_drained_count() -> u64 {
    SERVER_DEATH_WORK_DRAINED.load(core::sync::atomic::Ordering::Relaxed)
}

// ── Stage 200C2B: per-CPU bounded DEFERRED reply-timeout work queue ─────────────────
//
// The narrow collector (`SharedKernel::collect_due_reply_timeout_work`) publishes one
// owned work item per DUE token-bearing reply-receive deadline into this queue OFF the
// broad lock; the off-lock drain (`SharedKernel::drain_reply_timeout_post_work`) then
// runs the Stage 200C1 completion transaction for each. The queue is a per-CPU bounded
// array guarded by its own IRQ-safe lock — NOT the broad `SpinLock<KernelState>` — so
// neither collection nor completion holds a broad lock. A full queue leaves the DUE
// deadline armed (the collector does not clear the TCB entry), to be retried on a later
// scan. Duplicate collection of the same exact token yields only ONE work owner.

/// One owned unit of deferred reply-timeout completion work. The `handle` embeds the
/// full generation-bearing identity (token slot+generation, terminal epoch, caller
/// `{tid,asid}`, reply record index+generation, reply endpoint index+generation, and
/// blocked-recv generation); `deadline` is the DUE tick that selected it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReplyTimeoutPostWork {
    pub handle: crate::kernel::deadline_token::DeadlineTokenHandle,
    pub deadline: u64,
    /// Canonical 199E — the clock domain `deadline` is expressed in, carried from the
    /// registration so the drain's own re-check compares against the SAME clock the collector
    /// used. Without it the re-check would have to pick a domain, and on a selector-on boot
    /// (where both domains coexist) it would pick wrongly for one of them.
    pub clock: crate::kernel::deadline_token::ReplyDeadlineClock,
}

/// Per-CPU deferred-work slots — bounded by the whole deadline-token store, so a
/// full store cannot overflow it.
pub(crate) const RT_POST_WORK_SLOTS: usize = MAX_DEADLINE_TOKENS;

static REPLY_TIMEOUT_POST_WORK: [crate::kernel::lock::SpinLockIrq<
    [Option<ReplyTimeoutPostWork>; RT_POST_WORK_SLOTS],
>; crate::kernel::scheduler::MAX_CPUS] =
    [const { crate::kernel::lock::SpinLockIrq::new([None; RT_POST_WORK_SLOTS]) };
        crate::kernel::scheduler::MAX_CPUS];

/// Live counters (retirement-seal evidence): total deferred-work items published by the
/// collector and total drained by the off-lock drain.
static REPLY_TIMEOUT_WORK_PUBLISHED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
static REPLY_TIMEOUT_WORK_DRAINED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Publish one owned work item for `cpu_idx`. A DUPLICATE (a work item for the exact
/// same token slot + generation already queued and not yet drained) yields only ONE
/// owner — the collector's re-publication returns `true` without adding a second. A
/// FULL queue returns `false`; the collector then leaves the DUE deadline armed to be
/// retried on a later scan. Returns `true` iff the item is now published (or already
/// present).
pub(crate) fn reply_timeout_work_publish(cpu_idx: usize, work: ReplyTimeoutPostWork) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    let mut q = REPLY_TIMEOUT_POST_WORK[cpu_idx].lock();
    // One owner per exact token: a re-published duplicate is not added twice.
    for slot in q.iter().flatten() {
        if slot.handle.token_index() == work.handle.token_index()
            && slot.handle.token_generation() == work.handle.token_generation()
        {
            return true;
        }
    }
    if let Some(free) = q.iter_mut().find(|s| s.is_none()) {
        *free = Some(work);
        REPLY_TIMEOUT_WORK_PUBLISHED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        true
    } else {
        false // queue full — leave the deadline armed + due for a later scan
    }
}

/// Drain (remove + return) the next published work item for `cpu_idx`, or `None` when
/// the queue is empty.
pub(crate) fn reply_timeout_work_drain_next(cpu_idx: usize) -> Option<ReplyTimeoutPostWork> {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    let mut q = REPLY_TIMEOUT_POST_WORK[cpu_idx].lock();
    let taken = q.iter_mut().find(|s| s.is_some()).and_then(|s| s.take());
    if taken.is_some() {
        REPLY_TIMEOUT_WORK_DRAINED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
    taken
}

/// `true` iff the per-CPU deferred-work queue for `cpu_idx` holds no items (assertions).
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
pub(crate) fn reply_timeout_work_is_empty(cpu_idx: usize) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return true;
    }
    REPLY_TIMEOUT_POST_WORK[cpu_idx]
        .lock()
        .iter()
        .all(|s| s.is_none())
}

/// Total deferred-work items published by the collector (retirement-seal evidence).
pub(crate) fn reply_timeout_work_published_count() -> u64 {
    REPLY_TIMEOUT_WORK_PUBLISHED.load(core::sync::atomic::Ordering::Relaxed)
}
/// Total deferred-work items drained by the off-lock drain (retirement-seal evidence).
pub(crate) fn reply_timeout_work_drained_count() -> u64 {
    REPLY_TIMEOUT_WORK_DRAINED.load(core::sync::atomic::Ordering::Relaxed)
}

/// One-shot latch guarding the retired lock-status + class retirement seal: `true` on
/// the FIRST off-lock completion (proving the class scan runs with the broad lock
/// retired), `false` afterwards.
static REPLY_TIMEOUT_LOCK_STATUS_EMITTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
pub(crate) fn reply_timeout_lock_status_once() -> bool {
    // Stage 200C2C2B: an explicit compare-exchange is the single authority for this attestation.
    // Exactly one caller can observe the false -> true transition, so the marker is emitted once
    // per boot even if several trap paths race the drain. The latch is a `static` with no reset
    // path anywhere (asserted by a guard), so it cannot be re-armed mid-boot.
    REPLY_TIMEOUT_LOCK_STATUS_EMITTED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
}

/// Stage 200C2C1B — the class RETIREMENT marker is authorized only AFTER a resumed caller has
/// consumed its exact pending completion and had the canonical result encoded. On x86_64 the
/// completion transaction itself IS the delivery point (saved-frame return), so the drain arms
/// this immediately; on AArch64 the drain only ARMS it and the resume boundary fires it, so a
/// committed-but-never-delivered completion can never claim the class retired.
static REPLY_TIMEOUT_RETIRE_ARMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
static REPLY_TIMEOUT_RETIRE_EMITTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Arm the class-retirement marker (called by the off-lock completion transaction once it has
/// COMMITTED a timeout terminal). Arming alone never emits.
pub(crate) fn arm_reply_timeout_class_retired() {
    REPLY_TIMEOUT_RETIRE_ARMED.store(true, core::sync::atomic::Ordering::Release);
}

/// Emit `GLOBAL_LOCK_RETIRE_CLASS_DONE` exactly once, and ONLY if a committed completion armed
/// it. Called from the delivery point (the resume boundary), so the marker attests an
/// end-to-end retirement: collected off-lock, completed off-lock, and actually delivered.
pub(crate) fn maybe_emit_reply_timeout_class_retired() {
    if !REPLY_TIMEOUT_RETIRE_ARMED.load(core::sync::atomic::Ordering::Acquire) {
        return;
    }
    if REPLY_TIMEOUT_RETIRE_EMITTED.swap(true, core::sync::atomic::Ordering::AcqRel) {
        return;
    }
    crate::yarm_log!(
        "GLOBAL_LOCK_RETIRE_CLASS_DONE arch={} class=IpcReplyTimeout result=ok",
        REPLY_TIMEOUT_ARCH
    );
}

// U7 (canonical 199E) deleted the `REPLY_TIMEOUT_ARMED_ANY` latch that used to live here. Its
// only reader was the gate on the retired-scan attestation, and the promotion moved that
// attestation to the FIRST production drain — which necessarily runs before anything has armed a
// deadline. A latch that can never be true at its only read site is not evidence, so it went with
// the gate rather than being kept as a field that would always report zero.

// ── U7 (canonical 199E): per-CPU bounded DEFERRED blocking-SEND timeout work ────────
//
// The IpcSend timeout class rides the SAME shape as the reply class: the arch-neutral
// scanner publishes one owned work item per DUE blocking-send deadline off the broad
// lock, and the off-lock drain settles it through the U6 blocking-send lifecycle
// (completion publication → waiter removal → envelope settle → scheduler enqueue). The
// identity carried is the U6 one — `{tid, asid, send_generation}` — never a bare TID, so
// a replacement incarnation or a re-blocked sender can never be timed out by a stale
// item. `deadline` is the DUE value that selected it and is re-checked at settle.

/// One owned unit of deferred blocking-send timeout work.
///
/// `asid` is `Option` for exactly the reason U6's `SenderWakeTarget::asid` is: a task with no
/// bound address space has no incarnation to name. Such a sender is still timed out and woken —
/// the retired in-lock scan woke it too — but nothing is published for it, because the U6
/// completion contract is keyed on `{tid, asid, generation}` and cannot name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SendTimeoutPostWork {
    pub tid: u64,
    pub asid: Option<crate::kernel::vm::Asid>,
    pub send_generation: u64,
    pub deadline: u64,
}

/// Per-CPU deferred blocking-send work slots. Bounded by the endpoint sender-waiter
/// capacity so one scan pass cannot publish more than the system can hold blocked.
pub(crate) const ST_POST_WORK_SLOTS: usize = 8;

static SEND_TIMEOUT_POST_WORK: [crate::kernel::lock::SpinLockIrq<
    [Option<SendTimeoutPostWork>; ST_POST_WORK_SLOTS],
>; crate::kernel::scheduler::MAX_CPUS] =
    [const { crate::kernel::lock::SpinLockIrq::new([None; ST_POST_WORK_SLOTS]) };
        crate::kernel::scheduler::MAX_CPUS];

static SEND_TIMEOUT_WORK_PUBLISHED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
static SEND_TIMEOUT_WORK_DRAINED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Publish one owned send-timeout work item for `cpu_idx`. A DUPLICATE (the exact
/// `{tid, asid, send_generation}` cycle already queued and not yet drained) yields only
/// ONE owner. A FULL queue returns `false` and the collector leaves the DUE deadline
/// armed on the TCB, to be retried on a later scan — no due deadline is ever dropped.
pub(crate) fn send_timeout_work_publish(cpu_idx: usize, work: SendTimeoutPostWork) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    let mut q = SEND_TIMEOUT_POST_WORK[cpu_idx].lock();
    for slot in q.iter().flatten() {
        if slot.tid == work.tid
            && slot.asid == work.asid
            && slot.send_generation == work.send_generation
        {
            return true;
        }
    }
    if let Some(free) = q.iter_mut().find(|s| s.is_none()) {
        *free = Some(work);
        SEND_TIMEOUT_WORK_PUBLISHED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// Drain (remove + return) the next published send-timeout work item for `cpu_idx`.
pub(crate) fn send_timeout_work_drain_next(cpu_idx: usize) -> Option<SendTimeoutPostWork> {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    let mut q = SEND_TIMEOUT_POST_WORK[cpu_idx].lock();
    let taken = q.iter_mut().find(|s| s.is_some()).and_then(|s| s.take());
    if taken.is_some() {
        SEND_TIMEOUT_WORK_DRAINED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
    taken
}

/// One-shot latch for the send-timeout deferred-work publish/drain evidence marker.
static SEND_TIMEOUT_DEFERRED_EMITTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
pub(crate) fn send_timeout_deferred_once() -> bool {
    !SEND_TIMEOUT_DEFERRED_EMITTED.swap(true, core::sync::atomic::Ordering::AcqRel)
}

/// The IpcSendTimeout class-retirement seal, on the SAME discipline as the reply class: the
/// off-lock settle only ARMS it, and it is emitted from the U6 delivery point once a resumed
/// sender has actually consumed its parked completion. A committed-but-never-delivered
/// completion can therefore never claim the class retired.
static SEND_TIMEOUT_RETIRE_ARMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
static SEND_TIMEOUT_RETIRE_EMITTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn arm_send_timeout_class_retired() {
    SEND_TIMEOUT_RETIRE_ARMED.store(true, core::sync::atomic::Ordering::Release);
}

pub(crate) fn maybe_emit_send_timeout_class_retired() {
    if !SEND_TIMEOUT_RETIRE_ARMED.load(core::sync::atomic::Ordering::Acquire) {
        return;
    }
    if SEND_TIMEOUT_RETIRE_EMITTED.swap(true, core::sync::atomic::Ordering::AcqRel) {
        return;
    }
    crate::yarm_log!(
        "GLOBAL_LOCK_RETIRE_CLASS_DONE arch={} class=IpcSendTimeout result=ok",
        REPLY_TIMEOUT_ARCH
    );
}

pub(crate) fn send_timeout_work_published_count() -> u64 {
    SEND_TIMEOUT_WORK_PUBLISHED.load(core::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn send_timeout_work_drained_count() -> u64 {
    SEND_TIMEOUT_WORK_DRAINED.load(core::sync::atomic::Ordering::Relaxed)
}

/// Test-only: empty a CPU's deferred send-timeout queue WITHOUT settling anything.
#[cfg(test)]
pub(crate) fn send_timeout_work_clear(cpu_idx: usize) {
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        for slot in SEND_TIMEOUT_POST_WORK[cpu_idx].lock().iter_mut() {
            *slot = None;
        }
    }
}

// Canonical 199E: per-CPU bounded DEFERRED ordinary RECEIVE timeout work.
//
// The third and last class to leave the broad-lock scan. It rides the identical shape as the
// other two, and carries the identity the in-lock scan carried — the exact `{tid, asid}`
// incarnation plus the blocked-RECEIVE generation minted for this block — so a replacement task
// that reused the numeric TID, or the same task blocking a second time, can never be timed out
// by a stale item. `endpoint_idx` is the endpoint whose waiter must be removed.

/// One owned unit of deferred receive-timeout work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecvTimeoutPostWork {
    pub tid: u64,
    pub asid: Option<crate::kernel::vm::Asid>,
    pub wait_generation: u64,
    pub deadline: u64,
}

/// Per-CPU deferred receive-timeout slots.
pub(crate) const CT_POST_WORK_SLOTS: usize = 8;

static RECV_TIMEOUT_POST_WORK: [crate::kernel::lock::SpinLockIrq<
    [Option<RecvTimeoutPostWork>; CT_POST_WORK_SLOTS],
>; crate::kernel::scheduler::MAX_CPUS] =
    [const { crate::kernel::lock::SpinLockIrq::new([None; CT_POST_WORK_SLOTS]) };
        crate::kernel::scheduler::MAX_CPUS];

static RECV_TIMEOUT_WORK_PUBLISHED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
static RECV_TIMEOUT_WORK_DRAINED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Publish one owned receive-timeout item. A duplicate `{tid, asid, wait_generation}` yields one
/// owner; a full queue returns `false` and the collector leaves the deadline armed for a later
/// pass, so no due registration is dropped.
pub(crate) fn recv_timeout_work_publish(cpu_idx: usize, work: RecvTimeoutPostWork) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return false;
    }
    let mut q = RECV_TIMEOUT_POST_WORK[cpu_idx].lock();
    for slot in q.iter().flatten() {
        if slot.tid == work.tid
            && slot.asid == work.asid
            && slot.wait_generation == work.wait_generation
        {
            return true;
        }
    }
    if let Some(free) = q.iter_mut().find(|s| s.is_none()) {
        *free = Some(work);
        RECV_TIMEOUT_WORK_PUBLISHED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        true
    } else {
        false
    }
}

pub(crate) fn recv_timeout_work_drain_next(cpu_idx: usize) -> Option<RecvTimeoutPostWork> {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    let mut q = RECV_TIMEOUT_POST_WORK[cpu_idx].lock();
    let taken = q.iter_mut().find(|s| s.is_some()).and_then(|s| s.take());
    if taken.is_some() {
        RECV_TIMEOUT_WORK_DRAINED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
    taken
}

pub(crate) fn recv_timeout_work_published_count() -> u64 {
    RECV_TIMEOUT_WORK_PUBLISHED.load(core::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn recv_timeout_work_drained_count() -> u64 {
    RECV_TIMEOUT_WORK_DRAINED.load(core::sync::atomic::Ordering::Relaxed)
}

/// One-shot latch for the receive-timeout deferred-work evidence marker.
static RECV_TIMEOUT_DEFERRED_EMITTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
pub(crate) fn recv_timeout_deferred_once() -> bool {
    !RECV_TIMEOUT_DEFERRED_EMITTED.swap(true, core::sync::atomic::Ordering::AcqRel)
}

/// The IpcRecvTimeout class-retirement seal, on the same discipline as the other two: the settle
/// only ARMS it, and it is emitted once a resumed receiver has actually consumed its result.
static RECV_TIMEOUT_RETIRE_ARMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
static RECV_TIMEOUT_RETIRE_EMITTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) fn arm_recv_timeout_class_retired() {
    RECV_TIMEOUT_RETIRE_ARMED.store(true, core::sync::atomic::Ordering::Release);
}

pub(crate) fn maybe_emit_recv_timeout_class_retired() {
    if !RECV_TIMEOUT_RETIRE_ARMED.load(core::sync::atomic::Ordering::Acquire) {
        return;
    }
    if RECV_TIMEOUT_RETIRE_EMITTED.swap(true, core::sync::atomic::Ordering::AcqRel) {
        return;
    }
    crate::yarm_log!(
        "GLOBAL_LOCK_RETIRE_CLASS_DONE arch={} class=IpcRecvTimeout result=ok",
        REPLY_TIMEOUT_ARCH
    );
}

/// Test-only: empty a CPU's deferred receive-timeout queue. Gated exactly like its
/// `reply_timeout_work_clear` sibling: every caller is a retirement-cell test, and those modules
/// compile only under the oracle feature, so a plain hosted build has no use for it.
#[cfg(all(test, feature = "ipc-reply-timeout-oracle-core"))]
pub(crate) fn recv_timeout_work_clear(cpu_idx: usize) {
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        for slot in RECV_TIMEOUT_POST_WORK[cpu_idx].lock().iter_mut() {
            *slot = None;
        }
    }
}

/// U7 (canonical 199E) — the per-CPU cursor of the arch-neutral IPC-timeout scanner.
///
/// The scanner examines a bounded WINDOW of TCB slots per trap instead of the whole
/// array, so its cost is O(window) regardless of `MAX_TASKS`. The cursor advances by the
/// number of entries **scanned**, never by the number published: a window that publishes
/// nothing still advances, and a window whose publications were refused by a full queue
/// still advances, so no slot can starve behind a permanently-full queue. It wraps at
/// `MAX_TASKS`, so every slot is visited within `ceil(MAX_TASKS / window)` traps.
static IPC_TIMEOUT_SCAN_CURSOR: [core::sync::atomic::AtomicUsize;
    crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; crate::kernel::scheduler::MAX_CPUS];

/// The number of TCB slots one scan pass examines. Bounded and arch-neutral.
pub(crate) const IPC_TIMEOUT_SCAN_WINDOW: usize = 64;

/// Read this CPU's scan cursor (already reduced modulo `MAX_TASKS`).
pub(crate) fn ipc_timeout_scan_cursor(cpu_idx: usize) -> usize {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return 0;
    }
    IPC_TIMEOUT_SCAN_CURSOR[cpu_idx].load(core::sync::atomic::Ordering::Relaxed) % MAX_TASKS
}

/// Advance this CPU's scan cursor by the number of entries the pass SCANNED.
pub(crate) fn advance_ipc_timeout_scan_cursor(cpu_idx: usize, scanned: usize) {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return;
    }
    let cur = IPC_TIMEOUT_SCAN_CURSOR[cpu_idx].load(core::sync::atomic::Ordering::Relaxed);
    IPC_TIMEOUT_SCAN_CURSOR[cpu_idx].store(
        (cur.wrapping_add(scanned)) % MAX_TASKS,
        core::sync::atomic::Ordering::Relaxed,
    );
}

/// Test-only: rewind a CPU's scan cursor so a focused test starts from slot 0.
#[cfg(test)]
pub(crate) fn reset_ipc_timeout_scan_cursor(cpu_idx: usize) {
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        IPC_TIMEOUT_SCAN_CURSOR[cpu_idx].store(0, core::sync::atomic::Ordering::Relaxed);
    }
}

/// One-shot latch for the deferred-work publish/drain evidence marker.
static REPLY_TIMEOUT_DEFERRED_EMITTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
pub(crate) fn reply_timeout_deferred_once() -> bool {
    !REPLY_TIMEOUT_DEFERRED_EMITTED.swap(true, core::sync::atomic::Ordering::AcqRel)
}

/// Test-only: empty a CPU's deferred-work queue WITHOUT running completions, so a test
/// starts from a known-clean per-CPU queue (the queue statics are process-global).
#[cfg(all(test, feature = "ipc-reply-timeout-oracle-core"))]
pub(crate) fn reply_timeout_work_clear(cpu_idx: usize) {
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        for slot in REPLY_TIMEOUT_POST_WORK[cpu_idx].lock().iter_mut() {
            *slot = None;
        }
    }
}

/// Test-only: count queued (not-yet-drained) work items for `cpu_idx`.
#[cfg(all(test, feature = "ipc-reply-timeout-oracle-core"))]
pub(crate) fn reply_timeout_work_len(cpu_idx: usize) -> usize {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return 0;
    }
    REPLY_TIMEOUT_POST_WORK[cpu_idx]
        .lock()
        .iter()
        .filter(|s| s.is_some())
        .count()
}
/// True iff `eidx` is the oracle's reply endpoint (the ONLY recv-v2 timeout that registers a
/// reply-terminal deadline). `false` for every other endpoint and when un-provisioned.
pub fn ipc_reply_timeout_oracle_reply_endpoint_is(eidx: usize) -> bool {
    IPC_REPLY_TIMEOUT_ORACLE_REP_EIDX.load(core::sync::atomic::Ordering::Acquire) == eidx
}

/// The init-local caps a provisioned reply-timeout oracle hands to init (mirrors the direct oracle).
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
pub struct IpcReplyTimeoutOracleCaps {
    /// init-local request endpoint cap (`SEND | RECEIVE`) → startup slot 13.
    pub request_ep_cap: u32,
    /// init-local reply endpoint cap (`SEND | RECEIVE`) → startup slot 14.
    pub reply_ep_cap: u32,
    pub request_endpoint_idx: usize,
    pub reply_endpoint_idx: usize,
}

/// Stage 200C2A: provision the x86_64 reply-timeout oracle TRANSACTIONALLY — a request endpoint + a
/// reply endpoint, each minted `SEND | RECEIVE` into init's CNode, and the reply endpoint confined as
/// the ONLY registration endpoint. Fail-closed: on any step failure it emits a precise marker and
/// returns `None` (the oracle stays un-armed). Provisions NO MemoryObject, NO queued/notification
/// authority, and NO second deadline queue.
#[cfg(feature = "ipc-reply-timeout-oracle-core")]
pub fn provision_init_ipc_reply_timeout_oracle(
    kernel: &mut KernelState,
    init_tid: u64,
) -> Option<IpcReplyTimeoutOracleCaps> {
    // Canonical 199E: provisioning is gated by the COMPILE-TIME proof feature alone — the `cfg`
    // on this function — and no longer by the runtime selector.
    //
    // That separation is what makes the production path observable live. The runtime selector
    // decides whether the ORACLE pre-arms a confined, synthetic deadline
    // (`maybe_arm_reply_timeout_oracle` returns immediately when the mode is 0). While it was
    // also the gate on provisioning, the two were welded together: the only way to get a
    // userspace client that blocks on a reply endpoint with a finite timeout was to enable the
    // selector, and the selector's own pre-arm then won the one-registration race, so
    // `arm_production_reply_deadline` never fired on any live boot. With the selector off the
    // same client now runs against the PRODUCTION registration, which is the path that ships.
    //
    // This adds no knob: a build without the feature provisions nothing, exactly as before.
    use crate::kernel::capabilities::{CapObject, CapRights, Capability};
    let init_cnode = kernel.task_cnode(init_tid)?;
    let mint = |kernel: &mut KernelState, recv_root: crate::kernel::capabilities::CapId| {
        let object = kernel.current_task_capability(recv_root)?.object;
        debug_assert!(matches!(object, CapObject::Endpoint { .. }));
        kernel
            .mint_capability_in_cnode(
                init_cnode,
                Capability::new(object, CapRights::SEND | CapRights::RECEIVE),
            )
            .ok()
    };
    let (req_idx, _req_send, req_recv_root) = kernel.create_endpoint(8).ok()?;
    let Some(req_cap) = mint(kernel, req_recv_root) else {
        crate::yarm_log!("IPC_REPLY_TIMEOUT_ORACLE_PROVISION_FAIL step=mint_req");
        return None;
    };
    let (rep_idx, _rep_send, rep_recv_root) = kernel.create_endpoint(8).ok()?;
    let Some(rep_cap) = mint(kernel, rep_recv_root) else {
        crate::yarm_log!("IPC_REPLY_TIMEOUT_ORACLE_PROVISION_FAIL step=mint_rep");
        return None;
    };
    set_ipc_reply_timeout_oracle_reply_endpoint(rep_idx);
    crate::yarm_log!(
        "IPC_REPLY_TIMEOUT_ORACLE_PROVISION_OK init_tid={} req_cap={} rep_cap={} req_eidx={} rep_eidx={} mode={}",
        init_tid,
        req_cap.0,
        rep_cap.0,
        req_idx,
        rep_idx,
        x86_ipc_reply_timeout_oracle_mode(),
    );
    Some(IpcReplyTimeoutOracleCaps {
        request_ep_cap: req_cap.0 as u32,
        reply_ep_cap: rep_cap.0 as u32,
        request_endpoint_idx: req_idx,
        reply_endpoint_idx: rep_idx,
    })
}

/// Stage 199A2B4/199A2C1: emit the NR6 `IpcCallDirectRequest` success + retirement markers EXACTLY
/// ONCE, from the production off-lock request drain, ONLY under the umbrella oracle feature (x86_64
/// or aarch64) + the arch's selector. A normal (feature-off) artifact never compiles the class
/// literal; a feature-on-but-selector-off boot never reaches it (the gate is not armed and the drain
/// never runs a live delivery). ARCH-PARAMETERIZED via `IPCCALL_DIRECT_ORACLE_ARCH` — one emitter
/// serves both arches; the literal is `target_arch`-gated so an artifact carries only its arch tag.
#[cfg(all(
    feature = "ipccall-direct-oracle",
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ),
    not(feature = "hosted-dev")
))]
pub(crate) fn emit_ipccall_direct_request_live_markers() {
    if !ipccall_direct_oracle_enabled() {
        return;
    }
    static ONCE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    if ONCE.swap(true, core::sync::atomic::Ordering::AcqRel) {
        return;
    }
    crate::yarm_log!(
        "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch={} class=IpcCallDirectRequest",
        IPCCALL_DIRECT_ORACLE_ARCH
    );
    crate::yarm_log!(
        "IPCCALL_DIRECT_REQUEST_OK arch={} source_copy_offlock=1 reply_cap=1 server_wakes=1",
        IPCCALL_DIRECT_ORACLE_ARCH
    );
    crate::yarm_log!(
        "GLOBAL_LOCK_RETIRE_CLASS_DONE arch={} class=IpcCallDirectRequest result=ok",
        IPCCALL_DIRECT_ORACLE_ARCH
    );
}

/// Stage 199A2B4/199A2C1: emit the NR7 `IpcReplyDirect` success + retirement markers EXACTLY ONCE,
/// from the production off-lock reply drain, ONLY under the umbrella oracle feature + selector.
/// Arch-parameterized; one-shot.
#[cfg(all(
    feature = "ipccall-direct-oracle",
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ),
    not(feature = "hosted-dev")
))]
pub(crate) fn emit_ipcreply_direct_live_markers() {
    if !ipccall_direct_oracle_enabled() {
        return;
    }
    static ONCE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    if ONCE.swap(true, core::sync::atomic::Ordering::AcqRel) {
        return;
    }
    crate::yarm_log!(
        "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch={} class=IpcReplyDirect",
        IPCCALL_DIRECT_ORACLE_ARCH
    );
    crate::yarm_log!(
        "IPCREPLY_DIRECT_OK arch={} source_copy_offlock=1 caller_wakes=1 one_shot=1",
        IPCCALL_DIRECT_ORACLE_ARCH
    );
    crate::yarm_log!(
        "GLOBAL_LOCK_RETIRE_CLASS_DONE arch={} class=IpcReplyDirect result=ok",
        IPCCALL_DIRECT_ORACLE_ARCH
    );
}

/// No-op stubs so the production drain call sites compile unconditionally (normal artifacts +
/// hosted builds never emit the class literals).
#[cfg(not(all(
    feature = "ipccall-direct-oracle",
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ),
    not(feature = "hosted-dev")
)))]
pub(crate) fn emit_ipccall_direct_request_live_markers() {}

#[cfg(not(all(
    feature = "ipccall-direct-oracle",
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ),
    not(feature = "hosted-dev")
)))]
pub(crate) fn emit_ipcreply_direct_live_markers() {}

/// Authoritative committed blocked-server acknowledgements for the NR6 direct request
/// transaction, published ONLY from the fully-committed recv-v2 path.
///
/// # Concurrency classification (Stage 199D)
/// This is the endpoint-keyed view of a bounded, generation-bearing MULTI-PAIR
/// [`crate::kernel::direct_ack_store::DirectAckStore`] — the replacement the Stage 199A2D1
/// race model specified for the former single-outstanding-pair slot. Independent
/// `(endpoint_index, endpoint_generation)` pairs now coexist up to
/// [`crate::kernel::direct_ack_store::DIRECT_ACK_STORE_CAPACITY`], each published and
/// consumed exactly once, with capacity refused at reservation time — before any
/// irreversible publication. The former overwrite fuse survives as the fail-closed
/// [`overwrite_fuse_count`]: a SECOND live pair on the SAME endpoint is refused and the
/// already-published acknowledgement is preserved untouched.
///
/// Endpoint confinement and the NR6/NR7 proof gate are unchanged and still decide WHICH
/// endpoints may use the off-lock path at all; this store no longer constrains how many
/// pairs may be outstanding while they do.
pub mod ipccall_direct_ack {
    use crate::kernel::boot::ReceiverWaiterIdentity;
    use crate::kernel::direct_ack_store::{
        AckConsume, AckEndpoint, AckFields, AckReservation, AckReserveError, AckWaiter,
        DirectAckStore,
    };
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::ipccall_direct::BlockedServerAck;
    use crate::kernel::vm::Asid;

    static STORE: DirectAckStore = DirectAckStore::new();

    /// The shared multi-pair store backing the blocked-SERVER acknowledgements.
    pub(crate) fn store() -> &'static DirectAckStore {
        &STORE
    }

    fn endpoint_of(ack: &BlockedServerAck) -> AckEndpoint {
        AckEndpoint::new(ack.endpoint_index, ack.endpoint_generation)
    }

    fn waiter_of(ack: &BlockedServerAck) -> AckWaiter {
        AckWaiter::new(ack.server.tid.0, ack.server.asid.0)
    }

    fn fields_of(ack: &BlockedServerAck) -> AckFields {
        AckFields {
            endpoint: endpoint_of(ack),
            waiter: waiter_of(ack),
            payload_user_ptr: ack.payload_user_ptr,
            payload_user_len: ack.payload_user_len,
            meta_user_ptr: ack.meta_user_ptr,
            meta_user_len: ack.meta_user_len,
        }
    }

    fn ack_of(fields: AckFields) -> BlockedServerAck {
        BlockedServerAck {
            server: ReceiverWaiterIdentity::new(
                ThreadId(fields.waiter.tid),
                Asid(fields.waiter.asid),
            ),
            endpoint_index: fields.endpoint.index,
            endpoint_generation: fields.endpoint.generation,
            recv_v2_committed: true,
            payload_user_ptr: fields.payload_user_ptr,
            payload_user_len: fields.payload_user_len,
            meta_user_ptr: fields.meta_user_ptr,
            meta_user_len: fields.meta_user_len,
        }
    }

    /// Reset every pair and counter (test setup / between boots).
    pub fn reset() {
        STORE.reset();
    }

    /// Reserve the pair slot for one blocked server BEFORE anything is published, so
    /// capacity (or a still-live pair on the same endpoint) is refused while the caller can
    /// still abandon the pair with nothing to unwind.
    pub(crate) fn reserve(
        endpoint_index: usize,
        endpoint_generation: u64,
        server: ReceiverWaiterIdentity,
    ) -> Result<AckReservation, AckReserveError> {
        STORE.reserve(
            AckEndpoint::new(endpoint_index, endpoint_generation),
            AckWaiter::new(server.tid.0, server.asid.0),
        )
    }

    /// Publish the reserved acknowledgement — the single irreversible step. Returns the
    /// publication sequence, or hands the reservation back so the caller can cancel it.
    pub(crate) fn commit(
        reservation: AckReservation,
        ack: BlockedServerAck,
    ) -> Result<u64, AckReservation> {
        let fields = fields_of(&ack);
        STORE
            .commit(reservation, fields)
            .map_err(|err| err.into_reservation())
    }

    /// Abandon an uncommitted reservation, leaving no slot occupied and no server identity
    /// readable.
    pub(crate) fn cancel(reservation: AckReservation) -> bool {
        STORE.cancel(reservation)
    }

    /// Reserve + commit in one step. Used by the hosted wiring fixtures and by tests; the
    /// production publish site drives the explicit reserve → commit lifecycle so a rollback
    /// between the two is expressible. Returns 0 when the store refuses (capacity or a
    /// still-live pair on the same endpoint) — nothing is published in that case.
    ///
    /// Hosted builds first release any spent-or-live pair on the endpoint so the wiring
    /// fixtures, which share this process-global store across cases, keep the
    /// last-writer-wins behaviour they were written against. Real builds are fail-closed:
    /// a live pair is preserved and the refusal is counted.
    pub(crate) fn publish(ack: BlockedServerAck) -> u64 {
        #[cfg(feature = "hosted-dev")]
        STORE.release_endpoint_index(ack.endpoint_index);
        let reservation = match reserve(ack.endpoint_index, ack.endpoint_generation, ack.server) {
            Ok(reservation) => reservation,
            Err(AckReserveError::EndpointAlreadyLive) => {
                crate::yarm_log!("IPCCALL_DIRECT_ACK_OVERWRITE_FUSE slot=server");
                return 0;
            }
            Err(AckReserveError::CapacityExhausted) => {
                crate::yarm_log!("IPCCALL_DIRECT_ACK_CAPACITY_REFUSED slot=server");
                return 0;
            }
        };
        match commit(reservation, ack) {
            Ok(seq) => seq,
            Err(reservation) => {
                cancel(reservation);
                0
            }
        }
    }

    /// The acknowledgement published for EXACTLY this endpoint incarnation, or `None`.
    pub fn snapshot(endpoint_index: usize, endpoint_generation: u64) -> Option<BlockedServerAck> {
        STORE
            .snapshot(AckEndpoint::new(endpoint_index, endpoint_generation))
            .map(ack_of)
    }

    /// The publication sequence held for this endpoint incarnation, or 0.
    pub fn commit_seq(endpoint_index: usize, endpoint_generation: u64) -> u64 {
        STORE.commit_seq(AckEndpoint::new(endpoint_index, endpoint_generation))
    }

    /// Consume the acknowledgement published for EXACTLY this endpoint incarnation, at most
    /// once. `None` on an absent, stale, still-reserved or already-consumed pair — a
    /// duplicate trap/drain can never consume the same acknowledgement twice.
    pub fn claim(
        endpoint_index: usize,
        endpoint_generation: u64,
    ) -> Option<(BlockedServerAck, u64)> {
        claim_exact(endpoint_index, endpoint_generation, None)
    }

    /// Endpoint-keyed consume that additionally pins the SERVER incarnation: an
    /// acknowledgement belonging to a different `{tid, asid}` is foreign and refused.
    pub fn claim_exact(
        endpoint_index: usize,
        endpoint_generation: u64,
        expect_server: Option<ReceiverWaiterIdentity>,
    ) -> Option<(BlockedServerAck, u64)> {
        match STORE.consume(
            AckEndpoint::new(endpoint_index, endpoint_generation),
            expect_server.map(|id| AckWaiter::new(id.tid.0, id.asid.0)),
        ) {
            AckConsume::Consumed(fields, seq) => Some((ack_of(fields), seq)),
            _ => None,
        }
    }

    /// End the lease held for EXACTLY this endpoint incarnation and server incarnation —
    /// the NON-DIRECT terminal edge, driven by the endpoint waiter's own lifecycle. Called
    /// from the one centralized waiter-removal primitive, never from a terminal branch.
    pub fn release(
        endpoint_index: usize,
        endpoint_generation: u64,
        server: ReceiverWaiterIdentity,
    ) -> crate::kernel::direct_ack_store::AckRelease {
        STORE.release(
            AckEndpoint::new(endpoint_index, endpoint_generation),
            AckWaiter::new(server.tid.0, server.asid.0),
        )
    }

    /// Re-arm (restore) a consumed ack for a retryable rollback of the SAME publication
    /// (matching seq). A superseded publication cannot be restored — it stays consumed.
    pub fn restore(seq: u64) -> bool {
        STORE.restore(seq)
    }

    /// True iff an unconsumed published ack is present for this endpoint incarnation.
    pub fn is_claimable(endpoint_index: usize, endpoint_generation: u64) -> bool {
        STORE.is_claimable(AckEndpoint::new(endpoint_index, endpoint_generation))
    }

    /// Reservations refused because the endpoint already owned a LIVE pair — the
    /// fail-closed successor of the single-slot overwrite fuse. Must be 0 in the sealed
    /// oracle boot.
    pub fn overwrite_fuse_count() -> u64 {
        STORE.endpoint_live_refusal_count()
    }

    /// Reservations refused because every slot held a live pair. Must be 0 in the sealed
    /// oracle boot.
    pub fn capacity_refusal_count() -> u64 {
        STORE.capacity_refusal_count()
    }

    /// Simultaneously outstanding (reserved or committed) blocked-server pairs.
    pub fn live_pair_count() -> usize {
        STORE.live_pair_count()
    }

    /// Test support: the endpoint incarnation of the store's SINGLE pair, when exactly one
    /// exists. Lets a wiring fixture that publishes one acknowledgement address it through
    /// the ordinary endpoint-keyed API. `None` for zero or more than one pair.
    pub fn sole_pair_endpoint() -> Option<(usize, u64)> {
        STORE
            .sole_pair_endpoint()
            .map(|endpoint| (endpoint.index, endpoint.generation))
    }

    // ── Test-support wrappers ────────────────────────────────────────────────────────
    //
    // The recv-v2 publication / claim / restore WIRING fixtures publish exactly one
    // acknowledgement each; these address that single pair through the ordinary
    // endpoint-keyed API above so the wiring assertions stay readable. They are NOT a
    // production affordance: every production consumer names the endpoint incarnation it
    // is entitled to, and the endpoint-keying itself is proven by the store's own tests
    // and by the multi-pair hosted races.

    #[cfg(test)]
    pub fn sole_snapshot() -> Option<BlockedServerAck> {
        let (index, generation) = sole_pair_endpoint()?;
        snapshot(index, generation)
    }

    #[cfg(test)]
    pub fn sole_claim() -> Option<(BlockedServerAck, u64)> {
        let (index, generation) = sole_pair_endpoint()?;
        claim(index, generation)
    }

    #[cfg(test)]
    pub fn sole_is_claimable() -> bool {
        match sole_pair_endpoint() {
            Some((index, generation)) => is_claimable(index, generation),
            None => false,
        }
    }

    #[cfg(test)]
    pub fn sole_commit_seq() -> u64 {
        match sole_pair_endpoint() {
            Some((index, generation)) => commit_seq(index, generation),
            None => 0,
        }
    }
}

/// Publish the NR6 committed blocked-server acknowledgement from the recv-v2 commit
/// point — ONLY when the proof gate is armed and the FULLY-committed recv-v2 identity
/// exists. It does NOT wake, mint, copy user memory, mutate scheduler state, or emit a
/// retirement marker. A strict no-op off the gate or on any partial/stale state.
pub(crate) fn maybe_publish_ipccall_direct_blocked_server_ack(
    kernel: &KernelState,
    receiver_tid: u64,
    endpoint: crate::kernel::capabilities::CapObject,
    state: &crate::kernel::task::BlockedRecvState,
) {
    use crate::kernel::capabilities::CapObject;
    if !ipccall_direct_publication_enabled() {
        return;
    }
    let CapObject::Endpoint { index, generation } = endpoint else {
        return;
    };
    // Stage 199D-WA1-GATE: with the production term `false` on every architecture, publication
    // is confined to the oracle's provisioned request endpoint EVERYWHERE — x86_64 included —
    // so every normal boot stays byte-identical to the legacy path. The `production ||` term is
    // retained so WA2 can restore the eligibility-contract behaviour without editing here.
    // Hosted wiring
    // tests have no service chain and no provisioned oracle endpoint, so they keep the
    // unconfined publish the fixtures rely on.
    #[cfg(not(feature = "hosted-dev"))]
    if !ipccall_direct_request_endpoint_admitted(index) {
        return;
    }
    // Complete-commit contract: recv-v2, valid payload dest, non-null meta dest.
    if state.recv_abi != crate::kernel::task::RecvAbiVariant::RecvV2
        || state.payload_user_ptr == 0
        || state.meta_user_ptr == 0
    {
        return;
    }
    let Some(asid) = kernel.task_asid(receiver_tid) else {
        return;
    };
    let server = ReceiverWaiterIdentity::new(crate::kernel::ipc::ThreadId(receiver_tid), asid);
    // Stage 199D reserve → commit → cancel. RESERVE first: capacity (and a still-live pair
    // on this endpoint) is refused here, before ANY irreversible publication, so a refusal
    // costs nothing to unwind. A reservation is invisible to every consumer until commit.
    let reservation = match ipccall_direct_ack::reserve(index, generation, server) {
        Ok(reservation) => reservation,
        Err(crate::kernel::direct_ack_store::AckReserveError::EndpointAlreadyLive) => {
            crate::yarm_log!("IPCCALL_DIRECT_ACK_OVERWRITE_FUSE slot=server");
            return;
        }
        Err(crate::kernel::direct_ack_store::AckReserveError::CapacityExhausted) => {
            crate::yarm_log!("IPCCALL_DIRECT_ACK_CAPACITY_REFUSED slot=server");
            return;
        }
    };
    // Re-read the endpoint waiter identity under the IPC lock immediately before
    // publication and require an EXACT match (else the record is not fully committed
    // for this endpoint — publish nothing). CANCEL returns the reserved slot to vacant
    // with no server identity readable: a rollback with no slot or waiter leak.
    let waiter = kernel.with_ipc_state(|ipc| ipc.endpoint_waiter_identity(index));
    if waiter != Some(server) {
        ipccall_direct_ack::cancel(reservation);
        return;
    }
    let ack = crate::kernel::ipccall_direct::BlockedServerAck {
        server,
        endpoint_index: index,
        endpoint_generation: generation,
        recv_v2_committed: true,
        payload_user_ptr: state.payload_user_ptr,
        payload_user_len: state.payload_user_len,
        meta_user_ptr: state.meta_user_ptr,
        meta_user_len: state.meta_user_len,
    };
    let seq = match ipccall_direct_ack::commit(reservation, ack) {
        Ok(seq) => seq,
        Err(reservation) => {
            ipccall_direct_ack::cancel(reservation);
            return;
        }
    };
    // Stage 199A2D2C2B1: emit the AUTHORITATIVE cross-CPU blocked-server marker EXACTLY ONCE, only
    // for the x86_64 SMP oracle's CPU-1 recv-v2 server, and ONLY once every authoritative
    // blocking-order condition has committed: the saved continuation is captured, the exact endpoint
    // waiter equals the server, the server is absent from every runqueue, its home CPU is 1, and the
    // ack has published. Not a userspace log — a kernel marker. Never emits IPCCALL_DIRECT_SMP_
    // REQUEST_OK (this stage does not deliver a request).
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    maybe_emit_ipccall_direct_smp_server_blocked(kernel, receiver_tid, index, generation, seq);
    let _ = seq;
}

/// Stage 199A2D2C2B1: one-shot authoritative `IPCCALL_DIRECT_SMP_SERVER_BLOCKED` marker for the
/// x86_64 SMP oracle CPU-1 recv-v2 server. Fires at most once per boot, only when the SMP oracle is
/// armed and every blocking-order invariant is independently re-verified here against authoritative
/// committed state (never trusting the caller): the server has a committed saved frame, it is absent
/// from all runqueues (BlockedUnfinalized / not re-selectable), its home CPU is 1, the exact endpoint
/// waiter identity still equals the server, and the ack sequence is live. A strict no-op otherwise.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn maybe_emit_ipccall_direct_smp_server_blocked(
    kernel: &KernelState,
    receiver_tid: u64,
    endpoint_index: usize,
    endpoint_generation: u64,
    ack_seq: u64,
) {
    use core::sync::atomic::{AtomicBool, Ordering};
    static EMITTED: AtomicBool = AtomicBool::new(false);
    if !x86_ipccall_direct_smp_oracle_enabled() {
        return;
    }
    // Independent re-verification of every authoritative condition.
    let saved_frame = kernel.task_has_saved_frame(receiver_tid);
    let absent_from_runqueue = !kernel.task_present_in_any_runqueue(receiver_tid);
    let home_cpu_1 = kernel.task_home_cpu(receiver_tid) == Some(crate::kernel::scheduler::CpuId(1));
    let Some(asid) = kernel.task_asid(receiver_tid) else {
        return;
    };
    let server = ReceiverWaiterIdentity::new(crate::kernel::ipc::ThreadId(receiver_tid), asid);
    let waiter_exact =
        kernel.with_ipc_state(|ipc| ipc.endpoint_waiter_identity(endpoint_index)) == Some(server);
    let ack_published = ipccall_direct_ack::commit_seq(endpoint_index, endpoint_generation)
        == ack_seq
        && ack_seq != 0;
    if !(saved_frame && absent_from_runqueue && home_cpu_1 && waiter_exact && ack_published) {
        return;
    }
    if EMITTED.swap(true, Ordering::AcqRel) {
        return;
    }
    crate::yarm_log!(
        "IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1 recv_v2_committed=1 saved_frame=1 waiter_exact=1 ack_published=1 absent_from_runqueue=1 server_tid={} server_asid={} endpoint_index={} endpoint_generation={} result=ok",
        receiver_tid,
        asid.0,
        endpoint_index,
        endpoint_generation,
    );
}

/// Committed blocked-CALLER acknowledgements for the NR7 direct reply transaction.
/// Mirrors [`ipccall_direct_ack`] with the same reserve → commit → consume/cancel
/// lifecycle over its own bounded multi-pair store.
///
/// # Concurrency classification (Stage 199D)
/// The endpoint-keyed view of a second, independent bounded
/// [`crate::kernel::direct_ack_store::DirectAckStore`]. Independent
/// `(endpoint_index, endpoint_generation)` reply pairs coexist, each consumed exactly
/// once; capacity is refused at reservation time, before any irreversible publication.
/// The former overwrite fuse survives as the fail-closed [`overwrite_fuse_count`]: a
/// second LIVE pair on the SAME reply endpoint is refused and the already-published
/// acknowledgement is preserved untouched.
pub mod ipcreply_direct_ack {
    use crate::kernel::boot::ReceiverWaiterIdentity;
    use crate::kernel::direct_ack_store::{
        AckConsume, AckEndpoint, AckFields, AckReservation, AckReserveError, AckWaiter,
        DirectAckStore,
    };
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::ipccall_direct::BlockedCallerAck;
    use crate::kernel::vm::Asid;

    static STORE: DirectAckStore = DirectAckStore::new();

    /// The shared multi-pair store backing the blocked-CALLER acknowledgements.
    pub(crate) fn store() -> &'static DirectAckStore {
        &STORE
    }

    fn fields_of(ack: &BlockedCallerAck) -> AckFields {
        AckFields {
            endpoint: AckEndpoint::new(ack.endpoint_index, ack.endpoint_generation),
            waiter: AckWaiter::new(ack.caller.tid.0, ack.caller.asid.0),
            payload_user_ptr: ack.payload_user_ptr,
            payload_user_len: ack.payload_user_len,
            meta_user_ptr: ack.meta_user_ptr,
            meta_user_len: ack.meta_user_len,
        }
    }

    fn ack_of(fields: AckFields) -> BlockedCallerAck {
        BlockedCallerAck {
            caller: ReceiverWaiterIdentity::new(
                ThreadId(fields.waiter.tid),
                Asid(fields.waiter.asid),
            ),
            endpoint_index: fields.endpoint.index,
            endpoint_generation: fields.endpoint.generation,
            recv_v2_committed: true,
            payload_user_ptr: fields.payload_user_ptr,
            payload_user_len: fields.payload_user_len,
            meta_user_ptr: fields.meta_user_ptr,
            meta_user_len: fields.meta_user_len,
        }
    }

    /// Reset every pair and counter (test setup / between boots).
    pub fn reset() {
        STORE.reset();
    }

    /// Reserve the pair slot for one blocked caller BEFORE anything is published.
    pub(crate) fn reserve(
        endpoint_index: usize,
        endpoint_generation: u64,
        caller: ReceiverWaiterIdentity,
    ) -> Result<AckReservation, AckReserveError> {
        STORE.reserve(
            AckEndpoint::new(endpoint_index, endpoint_generation),
            AckWaiter::new(caller.tid.0, caller.asid.0),
        )
    }

    /// Publish the reserved acknowledgement — the single irreversible step.
    pub(crate) fn commit(
        reservation: AckReservation,
        ack: BlockedCallerAck,
    ) -> Result<u64, AckReservation> {
        let fields = fields_of(&ack);
        STORE
            .commit(reservation, fields)
            .map_err(|err| err.into_reservation())
    }

    /// Abandon an uncommitted reservation, leaving no slot occupied and no caller identity
    /// readable.
    pub(crate) fn cancel(reservation: AckReservation) -> bool {
        STORE.cancel(reservation)
    }

    /// Reserve + commit in one step (hosted wiring fixtures and tests). Returns 0 when the
    /// store refuses — nothing is published in that case. See
    /// [`super::ipccall_direct_ack::publish`] for the hosted last-writer-wins note.
    pub(crate) fn publish(ack: BlockedCallerAck) -> u64 {
        #[cfg(feature = "hosted-dev")]
        STORE.release_endpoint_index(ack.endpoint_index);
        let reservation = match reserve(ack.endpoint_index, ack.endpoint_generation, ack.caller) {
            Ok(reservation) => reservation,
            Err(AckReserveError::EndpointAlreadyLive) => {
                crate::yarm_log!("IPCREPLY_DIRECT_ACK_OVERWRITE_FUSE slot=caller");
                return 0;
            }
            Err(AckReserveError::CapacityExhausted) => {
                crate::yarm_log!("IPCREPLY_DIRECT_ACK_CAPACITY_REFUSED slot=caller");
                return 0;
            }
        };
        match commit(reservation, ack) {
            Ok(seq) => seq,
            Err(reservation) => {
                cancel(reservation);
                0
            }
        }
    }

    /// The acknowledgement published for EXACTLY this reply-endpoint incarnation.
    pub fn snapshot(endpoint_index: usize, endpoint_generation: u64) -> Option<BlockedCallerAck> {
        STORE
            .snapshot(AckEndpoint::new(endpoint_index, endpoint_generation))
            .map(ack_of)
    }

    /// Consume the acknowledgement for EXACTLY this reply-endpoint incarnation, at most
    /// once.
    pub fn claim(
        endpoint_index: usize,
        endpoint_generation: u64,
    ) -> Option<(BlockedCallerAck, u64)> {
        claim_exact(endpoint_index, endpoint_generation, None)
    }

    /// Endpoint-keyed consume that additionally pins the CALLER incarnation.
    pub fn claim_exact(
        endpoint_index: usize,
        endpoint_generation: u64,
        expect_caller: Option<ReceiverWaiterIdentity>,
    ) -> Option<(BlockedCallerAck, u64)> {
        match STORE.consume(
            AckEndpoint::new(endpoint_index, endpoint_generation),
            expect_caller.map(|id| AckWaiter::new(id.tid.0, id.asid.0)),
        ) {
            AckConsume::Consumed(fields, seq) => Some((ack_of(fields), seq)),
            _ => None,
        }
    }

    /// End the lease held for EXACTLY this reply-endpoint incarnation and caller
    /// incarnation — the NON-DIRECT terminal edge. See the request twin.
    pub fn release(
        endpoint_index: usize,
        endpoint_generation: u64,
        caller: ReceiverWaiterIdentity,
    ) -> crate::kernel::direct_ack_store::AckRelease {
        STORE.release(
            AckEndpoint::new(endpoint_index, endpoint_generation),
            AckWaiter::new(caller.tid.0, caller.asid.0),
        )
    }

    /// Re-arm a consumed ack for a retryable rollback of the SAME publication.
    pub fn restore(seq: u64) -> bool {
        STORE.restore(seq)
    }

    /// True iff an unconsumed published ack is present for this endpoint incarnation.
    pub fn is_claimable(endpoint_index: usize, endpoint_generation: u64) -> bool {
        STORE.is_claimable(AckEndpoint::new(endpoint_index, endpoint_generation))
    }

    /// Reservations refused because the reply endpoint already owned a LIVE pair. Must be 0
    /// in the sealed oracle boot.
    pub fn overwrite_fuse_count() -> u64 {
        STORE.endpoint_live_refusal_count()
    }

    /// Reservations refused because every slot held a live pair. Must be 0 in the sealed
    /// oracle boot.
    pub fn capacity_refusal_count() -> u64 {
        STORE.capacity_refusal_count()
    }

    /// Simultaneously outstanding (reserved or committed) blocked-caller pairs.
    pub fn live_pair_count() -> usize {
        STORE.live_pair_count()
    }

    /// Test support: the endpoint incarnation of the store's SINGLE pair, when exactly one
    /// exists. `None` for zero or more than one pair.
    pub fn sole_pair_endpoint() -> Option<(usize, u64)> {
        STORE
            .sole_pair_endpoint()
            .map(|endpoint| (endpoint.index, endpoint.generation))
    }

    // Test-support wrappers for the single-pair reply wiring fixtures; see the NR6 twin.

    #[cfg(test)]
    pub fn sole_snapshot() -> Option<BlockedCallerAck> {
        let (index, generation) = sole_pair_endpoint()?;
        snapshot(index, generation)
    }

    #[cfg(test)]
    pub fn sole_claim() -> Option<(BlockedCallerAck, u64)> {
        let (index, generation) = sole_pair_endpoint()?;
        claim(index, generation)
    }

    #[cfg(test)]
    pub fn sole_is_claimable() -> bool {
        match sole_pair_endpoint() {
            Some((index, generation)) => is_claimable(index, generation),
            None => false,
        }
    }

    #[cfg(test)]
    pub fn sole_commit_seq() -> u64 {
        match sole_pair_endpoint() {
            Some((index, generation)) => commit_seq(index, generation),
            None => 0,
        }
    }

    /// The publication sequence held for this reply-endpoint incarnation, or 0.
    pub fn commit_seq(endpoint_index: usize, endpoint_generation: u64) -> u64 {
        STORE.commit_seq(AckEndpoint::new(endpoint_index, endpoint_generation))
    }
}

/// Publish the NR7 committed blocked-CALLER acknowledgement from the recv-v2 commit
/// point (the caller blocking on its reply endpoint) — proof-gated, and only when the
/// FULLY-committed identity exists. No wake / mint / copy / scheduler mutation /
/// retirement marker; a strict no-op off the gate or on any partial/stale state.
pub(crate) fn maybe_publish_ipcreply_direct_blocked_caller_ack(
    kernel: &KernelState,
    receiver_tid: u64,
    endpoint: crate::kernel::capabilities::CapObject,
    state: &crate::kernel::task::BlockedRecvState,
) {
    use crate::kernel::capabilities::CapObject;
    if !ipccall_direct_publication_enabled() {
        return;
    }
    let CapObject::Endpoint { index, generation } = endpoint else {
        return;
    };
    // Stage 199D-WA1-GATE: with the production term `false` on every architecture, reply
    // publication is confined to the oracle's provisioned reply endpoint EVERYWHERE — x86_64
    // included. Hosted wiring tests keep the unconfined publish their fixtures rely on.
    #[cfg(not(feature = "hosted-dev"))]
    if !ipccall_direct_reply_endpoint_admitted(index) {
        return;
    }
    if state.recv_abi != crate::kernel::task::RecvAbiVariant::RecvV2
        || state.payload_user_ptr == 0
        || state.meta_user_ptr == 0
    {
        return;
    }
    let Some(asid) = kernel.task_asid(receiver_tid) else {
        return;
    };
    let caller = ReceiverWaiterIdentity::new(crate::kernel::ipc::ThreadId(receiver_tid), asid);
    // Stage 199D reserve → commit → cancel: capacity is refused here, before any
    // irreversible publication (see the NR6 twin).
    let reservation = match ipcreply_direct_ack::reserve(index, generation, caller) {
        Ok(reservation) => reservation,
        Err(crate::kernel::direct_ack_store::AckReserveError::EndpointAlreadyLive) => {
            crate::yarm_log!("IPCREPLY_DIRECT_ACK_OVERWRITE_FUSE slot=caller");
            return;
        }
        Err(crate::kernel::direct_ack_store::AckReserveError::CapacityExhausted) => {
            crate::yarm_log!("IPCREPLY_DIRECT_ACK_CAPACITY_REFUSED slot=caller");
            return;
        }
    };
    // Re-read the endpoint waiter identity under the IPC lock immediately before publish.
    let waiter = kernel.with_ipc_state(|ipc| ipc.endpoint_waiter_identity(index));
    if waiter != Some(caller) {
        ipcreply_direct_ack::cancel(reservation);
        return;
    }
    let ack = crate::kernel::ipccall_direct::BlockedCallerAck {
        caller,
        endpoint_index: index,
        endpoint_generation: generation,
        recv_v2_committed: true,
        payload_user_ptr: state.payload_user_ptr,
        payload_user_len: state.payload_user_len,
        meta_user_ptr: state.meta_user_ptr,
        meta_user_len: state.meta_user_len,
    };
    let seq = match ipcreply_direct_ack::commit(reservation, ack) {
        Ok(seq) => seq,
        Err(reservation) => {
            ipcreply_direct_ack::cancel(reservation);
            return;
        }
    };
    // Stage 199A2D2C2C: emit the AUTHORITATIVE cross-CPU blocked-CALLER marker EXACTLY ONCE, only for
    // the x86_64 SMP oracle's CPU-0 client blocking on its reply endpoint, and ONLY once every
    // authoritative blocking-order condition has committed (saved continuation captured, the exact
    // reply-endpoint waiter equals the caller, the caller is absent from every runqueue, its home CPU
    // is 0, and the ack has published). The reply-side analog of `IPCCALL_DIRECT_SMP_SERVER_BLOCKED`.
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    maybe_emit_ipcreply_direct_smp_caller_blocked(kernel, receiver_tid, index, generation, seq);
    let _ = seq;
}

/// Stage 199A2D2C2C: one-shot authoritative `IPCREPLY_DIRECT_SMP_CALLER_BLOCKED` marker for the x86_64
/// SMP oracle CPU-0 client. Fires at most once per boot, only when the reply sub-selector is armed and
/// every blocking-order invariant is independently re-verified here against authoritative committed
/// state (never trusting the caller): the caller has a committed saved frame, it is absent from all
/// runqueues, its home CPU is 0, the exact reply-endpoint waiter identity still equals the caller, and
/// the ack sequence is live. A strict no-op otherwise.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn maybe_emit_ipcreply_direct_smp_caller_blocked(
    kernel: &KernelState,
    receiver_tid: u64,
    endpoint_index: usize,
    endpoint_generation: u64,
    ack_seq: u64,
) {
    use core::sync::atomic::{AtomicBool, Ordering};
    static EMITTED: AtomicBool = AtomicBool::new(false);
    if !x86_ipccall_direct_smp_reply_enabled() {
        return;
    }
    let saved_frame = kernel.task_has_saved_frame(receiver_tid);
    let absent_from_runqueue = !kernel.task_present_in_any_runqueue(receiver_tid);
    let home_cpu_0 = kernel.task_home_cpu(receiver_tid) == Some(crate::kernel::scheduler::CpuId(0));
    let Some(asid) = kernel.task_asid(receiver_tid) else {
        return;
    };
    let caller = ReceiverWaiterIdentity::new(crate::kernel::ipc::ThreadId(receiver_tid), asid);
    let waiter_exact =
        kernel.with_ipc_state(|ipc| ipc.endpoint_waiter_identity(endpoint_index)) == Some(caller);
    let ack_published = ipcreply_direct_ack::commit_seq(endpoint_index, endpoint_generation)
        == ack_seq
        && ack_seq != 0;
    if !(saved_frame && absent_from_runqueue && home_cpu_0 && waiter_exact && ack_published) {
        return;
    }
    if EMITTED.swap(true, Ordering::AcqRel) {
        return;
    }
    crate::yarm_log!(
        "IPCREPLY_DIRECT_SMP_CALLER_BLOCKED arch=x86_64 caller_cpu=0 recv_v2_committed=1 saved_frame=1 waiter_exact=1 ack_published=1 absent_from_runqueue=1 caller_tid={} caller_asid={} endpoint_index={} endpoint_generation={} ack_seq={} result=ok",
        receiver_tid,
        asid.0,
        endpoint_index,
        endpoint_generation,
        ack_seq,
    );
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

/// Stage 198E3C1B-H / 198E3C2C: the dedicated unmapped two-page oracle receive window VA, mirroring
/// `yarm_user_rt::syscall::SHARED_REGION_ORACLE_VA` EXACTLY (same per-arch value). The authoritative
/// blocked-recv acknowledgement requires the receiver's `payload_user_ptr` to equal this exact VA (a
/// wrong-VA recv is rejected), and the transaction maps the two pages here. It MUST be a currently
/// UNMAPPED user VA on the target, below its kernel boundary and clear of image/initrd/heap/stack:
/// - AArch64 user half ends at `KERNEL_SPACE_BASE = 0x4000_0000`, so the window is 512 MiB (1 GiB
///   faults `PrivilegeViolation`);
/// - RISC-V places the default `brk` at `USER_BRK_DEFAULT_BASE = 0x4000_0000`, so 1 GiB is the HEAP
///   base there — the window is 512 MiB, in the image↔heap gap and below `KERNEL_SPACE_BASE`
///   (`0x8000_0000`);
/// - x86_64 uses 1 GiB (well above the image/initrd, far below the multi-TB user stack).
#[cfg(all(
    feature = "shared-region-direct-oracle",
    any(target_arch = "aarch64", target_arch = "riscv64")
))]
pub const SHARED_REGION_ORACLE_USER_VA: usize = 0x2000_0000;
#[cfg(all(
    feature = "shared-region-direct-oracle",
    not(any(target_arch = "aarch64", target_arch = "riscv64"))
))]
pub const SHARED_REGION_ORACLE_USER_VA: usize = 0x4000_0000;

/// Stage 198E3C1B: the init startup-slot-5 selector value that names the DIRECT shared-region
/// oracle (mirrors `yarm_user_rt::syscall::SHARED_REGION_ORACLE_SELECTOR`). Slot 5 is a mutually
/// exclusive selector: 1 = x86_64 FutexWake oracle, 2 = shared-region direct oracle. Only ONE may
/// be armed per boot (the boot caller fails closed if both knobs are set).
#[cfg(feature = "x86-shared-region-direct-oracle")]
pub const SHARED_REGION_ORACLE_SELECTOR: u64 = 2;

/// Stage 198E3C2B: the AArch64 init startup-slot-5 selector for the DIRECT shared-region oracle. On
/// AArch64 slot-5 values 1–5 are already claimed (FutexWake/FutexWait-switch/idle/yield oracles), so
/// the shared-region oracle uses the next FREE value, 6. (x86_64 uses 2, where it is free there.)
#[cfg(feature = "aarch64-shared-region-direct-oracle")]
pub const AARCH64_SHARED_REGION_ORACLE_SELECTOR: u64 = 6;

/// Stage 198E3C2C: the RISC-V init startup-slot-5 selector for the DIRECT shared-region oracle. On
/// RISC-V slot-5 values 1–6 are already claimed (FutexWake / queue-switch / FutexWait-switch /
/// FutexWait-idle / Yield-two / Yield-lone oracles), so the shared-region oracle uses the next FREE
/// value, 7. (x86_64 uses 2, AArch64 uses 6, each free on its own arch.)
#[cfg(feature = "riscv-shared-region-direct-oracle")]
pub const RISCV_SHARED_REGION_ORACLE_SELECTOR: u64 = 7;

/// Stage 198E3C2B/C: the architecture tag baked into the shared-region DIRECT live markers. Fixed by
/// `target_arch` (the emitter is compiled only for the armed arch), so the exact `arch=<a>` literal
/// appears at runtime without duplicating the emitter per arch.
#[cfg(any(
    all(feature = "x86-shared-region-direct-oracle", target_arch = "x86_64"),
    all(
        feature = "aarch64-shared-region-direct-oracle",
        target_arch = "aarch64"
    ),
    all(feature = "riscv-shared-region-direct-oracle", target_arch = "riscv64")
))]
pub(crate) const SHARED_REGION_ORACLE_ARCH: &str = if cfg!(target_arch = "aarch64") {
    "aarch64"
} else if cfg!(target_arch = "riscv64") {
    "riscv64"
} else {
    "x86_64"
};

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
#[cfg(feature = "shared-region-direct-oracle")]
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

    /// Non-consuming send-side match: a committed, unconsumed, recv-v2, oracle-VA ack exists for THIS
    /// receiver + endpoint. Used to DECIDE the direct path (and, on no-match, fail closed) WITHOUT
    /// consuming the ack — so a pre-publication failure leaves the ack available for a retry.
    pub(crate) fn matches_for_delivery(receiver_tid: u64, endpoint_idx: usize) -> bool {
        matches!(
            snapshot(),
            Some(a) if !CONSUMED.load(Ordering::Acquire)
                && a.recv_v2
                && a.receiver_tid == receiver_tid
                && a.endpoint_idx == endpoint_idx
                && a.payload_va == super::SHARED_REGION_ORACLE_USER_VA
        )
    }

    /// Send-side authoritative gate for the DIRECT blocked-recv delivery: consume the ack exactly
    /// once iff a committed, unconsumed, recv-v2, oracle-VA ack exists for THIS receiver + endpoint.
    /// A wrong receiver, wrong endpoint, non-oracle VA, plain (non-v2) waiter, absent ack, or an
    /// already-consumed (duplicate) ack all return `false`, so the direct path declines and no
    /// duplicate send can re-satisfy the gate. Called ONLY after the post-work item is published, so
    /// the consume is the atomic commit of a successful delivery (never a pre-publication loss).
    pub(crate) fn consume_for_delivery(receiver_tid: u64, endpoint_idx: usize) -> bool {
        if !matches_for_delivery(receiver_tid, endpoint_idx) {
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
#[cfg(feature = "shared-region-direct-oracle")]
pub(crate) fn maybe_publish_shared_region_blocked_recv_ack(
    kernel: &KernelState,
    receiver_tid: u64,
    endpoint: crate::kernel::capabilities::CapObject,
    state: &crate::kernel::task::BlockedRecvState,
) {
    use crate::kernel::capabilities::CapObject;
    if !shared_region_direct_oracle_enabled() {
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
#[cfg(feature = "shared-region-direct-oracle")]
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
#[cfg(feature = "shared-region-direct-oracle")]
pub(crate) static SHARED_REGION_ORACLE_FAULT_INJECT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

/// Fault-injection step codes (0 = none). Each names the step FORCED to fail; rollback must then
/// reclaim everything created by the strictly-earlier steps.
#[cfg(feature = "shared-region-direct-oracle")]
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

#[cfg(feature = "shared-region-direct-oracle")]
pub(crate) fn set_shared_region_oracle_fault_inject(step: u32) {
    SHARED_REGION_ORACLE_FAULT_INJECT.store(step, core::sync::atomic::Ordering::Release);
}

#[cfg(feature = "shared-region-direct-oracle")]
fn shared_region_oracle_fault_inject() -> u32 {
    SHARED_REGION_ORACLE_FAULT_INJECT.load(core::sync::atomic::Ordering::Acquire)
}

/// Rollback scratch: every reclaimable resource the transaction has created so far, in creation
/// order. Rollback undoes them in REVERSE order.
#[cfg(feature = "shared-region-direct-oracle")]
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
#[cfg(feature = "shared-region-direct-oracle")]
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
#[cfg(feature = "shared-region-direct-oracle")]
pub fn provision_init_shared_region_oracle(
    kernel: &mut KernelState,
    init_tid: u64,
) -> Option<SharedRegionOracleCaps> {
    if !shared_region_direct_oracle_enabled() {
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

/// Stage 199A2B4: caps provisioned for the x86_64 DIRECT IpcCall/IpcReply live round-trip
/// oracle. Both endpoint caps carry `SEND | RECEIVE` and live in init's (shared) CNode — the
/// client uses the request SEND side + the reply RECEIVE side, and the spawned server child
/// (sharing init's CSpace) uses the request RECEIVE side.
#[cfg(feature = "ipccall-direct-oracle")]
pub struct IpccallDirectOracleCaps {
    /// init-local request endpoint cap (`SEND | RECEIVE`) → startup slot 13.
    pub request_ep_cap: u32,
    /// init-local reply endpoint cap (`SEND | RECEIVE`) → startup slot 14.
    pub reply_ep_cap: u32,
    /// The request endpoint's slot index (bookkeeping / hosted assertions).
    pub request_endpoint_idx: usize,
    /// The reply endpoint's slot index (bookkeeping / hosted assertions).
    pub reply_endpoint_idx: usize,
}

/// Rollback scratch for the IpcCall/IpcReply oracle provisioning transaction.
#[cfg(feature = "ipccall-direct-oracle")]
#[derive(Default, Clone, Copy)]
struct IpccallDirectProvisionScratch {
    req_send_root: Option<crate::kernel::capabilities::CapId>,
    req_recv_root: Option<crate::kernel::capabilities::CapId>,
    req_endpoint_idx: Option<usize>,
    init_req_cap: Option<crate::kernel::capabilities::CapId>,
    rep_send_root: Option<crate::kernel::capabilities::CapId>,
    rep_recv_root: Option<crate::kernel::capabilities::CapId>,
    rep_endpoint_idx: Option<usize>,
    init_rep_cap: Option<crate::kernel::capabilities::CapId>,
}

#[cfg(feature = "ipccall-direct-oracle")]
fn rollback_ipccall_direct_provision(
    kernel: &mut KernelState,
    init_tid: u64,
    scratch: &IpccallDirectProvisionScratch,
) {
    if let Some(cnode) = kernel.task_cnode(init_tid) {
        for cap in [scratch.init_rep_cap, scratch.init_req_cap]
            .into_iter()
            .flatten()
        {
            let _ = kernel.revoke_capability_in_cnode(cnode, cap);
        }
    }
    if let Some(cnode) = kernel.task_cnode(0) {
        for root in [
            scratch.rep_send_root,
            scratch.rep_recv_root,
            scratch.req_send_root,
            scratch.req_recv_root,
        ]
        .into_iter()
        .flatten()
        {
            let _ = kernel.revoke_capability_in_cnode(cnode, root);
        }
    }
    for eidx in [scratch.rep_endpoint_idx, scratch.req_endpoint_idx]
        .into_iter()
        .flatten()
    {
        let _ = kernel.destroy_endpoint(eidx);
    }
}

/// Stage 199A2B4: provision the x86_64 DIRECT IpcCall/IpcReply live round-trip oracle
/// TRANSACTIONALLY. Under BOTH the compile-time feature and the runtime selector, it creates
/// a request endpoint + a reply endpoint and mints a `SEND | RECEIVE` cap for each into init's
/// CNode. Returns `IpccallDirectOracleCaps` for the startup slots. Fail-closed AND leak-free:
/// on ANY step failure it emits a precise failure marker, rolls back every resource created so
/// far, and returns `None` (the caller then leaves the oracle un-armed). Provisions NO
/// MemoryObject and NO queued/timeout/notification authority.
#[cfg(feature = "ipccall-direct-oracle")]
pub fn provision_init_ipccall_direct_oracle(
    kernel: &mut KernelState,
    init_tid: u64,
) -> Option<IpccallDirectOracleCaps> {
    if !ipccall_direct_oracle_enabled() {
        return None;
    }
    use crate::kernel::capabilities::{CapObject, CapRights, Capability};
    let mut scratch = IpccallDirectProvisionScratch::default();

    // Step 1: request endpoint.
    let (req_idx, req_send_root, req_recv_root) = match kernel.create_endpoint(8) {
        Ok(t) => t,
        Err(e) => {
            crate::yarm_log!(
                "IPCCALL_DIRECT_ORACLE_PROVISION_FAIL step=create_req err={:?}",
                e
            );
            return None;
        }
    };
    scratch.req_send_root = Some(req_send_root);
    scratch.req_recv_root = Some(req_recv_root);
    scratch.req_endpoint_idx = Some(req_idx);

    let req_object = match kernel.current_task_capability(req_recv_root) {
        Some(c) => c.object,
        None => {
            crate::yarm_log!(
                "IPCCALL_DIRECT_ORACLE_PROVISION_FAIL step=resolve_req eidx={}",
                req_idx
            );
            rollback_ipccall_direct_provision(kernel, init_tid, &scratch);
            return None;
        }
    };
    debug_assert!(matches!(req_object, CapObject::Endpoint { .. }));

    let init_cnode = match kernel.task_cnode(init_tid) {
        Some(c) => c,
        None => {
            crate::yarm_log!("IPCCALL_DIRECT_ORACLE_PROVISION_FAIL step=init_cnode");
            rollback_ipccall_direct_provision(kernel, init_tid, &scratch);
            return None;
        }
    };
    let init_req_cap = match kernel.mint_capability_in_cnode(
        init_cnode,
        Capability::new(req_object, CapRights::SEND | CapRights::RECEIVE),
    ) {
        Ok(c) => c,
        Err(e) => {
            crate::yarm_log!(
                "IPCCALL_DIRECT_ORACLE_PROVISION_FAIL step=mint_req err={:?}",
                e
            );
            rollback_ipccall_direct_provision(kernel, init_tid, &scratch);
            return None;
        }
    };
    scratch.init_req_cap = Some(init_req_cap);

    // Step 2: reply endpoint.
    let (rep_idx, rep_send_root, rep_recv_root) = match kernel.create_endpoint(8) {
        Ok(t) => t,
        Err(e) => {
            crate::yarm_log!(
                "IPCCALL_DIRECT_ORACLE_PROVISION_FAIL step=create_rep err={:?}",
                e
            );
            rollback_ipccall_direct_provision(kernel, init_tid, &scratch);
            return None;
        }
    };
    scratch.rep_send_root = Some(rep_send_root);
    scratch.rep_recv_root = Some(rep_recv_root);
    scratch.rep_endpoint_idx = Some(rep_idx);

    let rep_object = match kernel.current_task_capability(rep_recv_root) {
        Some(c) => c.object,
        None => {
            crate::yarm_log!(
                "IPCCALL_DIRECT_ORACLE_PROVISION_FAIL step=resolve_rep eidx={}",
                rep_idx
            );
            rollback_ipccall_direct_provision(kernel, init_tid, &scratch);
            return None;
        }
    };
    debug_assert!(matches!(rep_object, CapObject::Endpoint { .. }));

    let init_rep_cap = match kernel.mint_capability_in_cnode(
        init_cnode,
        Capability::new(rep_object, CapRights::SEND | CapRights::RECEIVE),
    ) {
        Ok(c) => c,
        Err(e) => {
            crate::yarm_log!(
                "IPCCALL_DIRECT_ORACLE_PROVISION_FAIL step=mint_rep err={:?}",
                e
            );
            rollback_ipccall_direct_provision(kernel, init_tid, &scratch);
            return None;
        }
    };
    scratch.init_rep_cap = Some(init_rep_cap);

    // Confine the off-lock NR6/NR7 gates to EXACTLY these two endpoints — every other
    // IpcCall/IpcReply in the running system stays on its unchanged legacy path.
    set_ipccall_direct_oracle_endpoints(req_idx, rep_idx);
    crate::yarm_log!(
        "IPCCALL_DIRECT_ORACLE_PROVISION_OK init_tid={} req_cap={} rep_cap={} req_eidx={} rep_eidx={}",
        init_tid,
        init_req_cap.0,
        init_rep_cap.0,
        req_idx,
        rep_idx
    );
    Some(IpccallDirectOracleCaps {
        request_ep_cap: init_req_cap.0 as u32,
        reply_ep_cap: init_rep_cap.0 as u32,
        request_endpoint_idx: req_idx,
        reply_endpoint_idx: rep_idx,
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
pub(crate) const MAX_TRANSFER_ENVELOPES: usize = 256;
#[cfg(not(feature = "hosted-dev"))]
pub(crate) const MAX_TRANSFER_ENVELOPES: usize = 64;
pub(crate) const MAX_REPLY_CAPS: usize = MAX_TASKS;
/// Stage 200B — bounded capacity of the single deadline-registration store
/// (`IpcSubsystem::reply_deadline_tokens`). Small on purpose: this stage supports
/// at most one active deadline registration per reply receive in a single-pair
/// era, and a bounded store makes the fail-closed "store full" path testable. This
/// is a registration/ownership store, NOT a terminal-result authority store.
pub(crate) const MAX_DEADLINE_TOKENS: usize = 4;
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
    /// Stage 199D-WA3B: monotonic source of spawn-reservation generations.
    ///
    /// Never derived from the TID, so a token minted for an earlier occupant of numeric TID `T`
    /// can never match a later occupant of `T`. Advanced once per reservation and never reset.
    spawn_reservation_generation: u64,
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
