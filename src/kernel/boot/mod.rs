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

/// Stage 5B scaffolding for `VmAnonMap` — helper-only, no live conversion.
///
/// VmAnonMap touches all of: scheduler (rank 1), task (rank 2), ipc (rank 3 —
/// TLB shootdown via cross_cpu_work), capability (rank 4), vm (rank 5), and
/// memory (rank 6). Live plan-first conversion requires x86_64 TLB smoke
/// approval per KERNEL_LOCKING.md §17 invariant. This struct exists as
/// scaffolding for that future stage.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VmAnonMapPlan {
    pub(crate) tid: u64,
}

#[cfg(feature = "hosted-dev")]
const MAX_COW_PAGES: usize = 100;
#[cfg(not(feature = "hosted-dev"))]
const MAX_COW_PAGES: usize = 256;

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
