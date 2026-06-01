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
