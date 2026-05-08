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
#[cfg(feature = "hosted-dev")]
const MAX_COW_PAGES: usize = 1024;
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
    cpu_apic_ids: [Option<u8>; platform_constants::MAX_CPUS],
}

pub struct Bootstrap;

pub fn install_init_orchestration_caps_for_first_user(
    kernel: &mut KernelState,
    init_tid: u64,
    startup_args: &mut [u64; UserImageSpec::STARTUP_SLOT_COUNT],
) -> Result<(), KernelError> {
    crate::yarm_log!("INIT_ORCH_WRITER_BEGIN init_tid={}", init_tid);
    if init_tid == 0 {
        crate::yarm_log!("INIT_ORCH_WRITER_SKIPPED reason=invalid_init_tid");
        return Err(KernelError::TaskMissing);
    }
    let (_, request_send_root, request_recv_root) = match kernel.create_endpoint(16) {
        Ok(v) => v,
        Err(err) => {
            crate::yarm_log!("INIT_ORCH_WRITER_SKIPPED reason=create_request_endpoint_failed err={:?}", err);
            return Err(err);
        }
    };
    let request_send = kernel.grant_capability_task_to_task_with_rights(
        0,
        request_send_root,
        init_tid,
        CapRights::SEND,
    )
    .map_err(|err| {
        crate::yarm_log!("INIT_ORCH_WRITER_SKIPPED reason=grant_request_send_failed err={:?}", err);
        err
    })?;
    let request_recv = kernel.grant_capability_task_to_task_with_rights(
        0,
        request_recv_root,
        init_tid,
        CapRights::RECEIVE,
    )
    .map_err(|err| {
        crate::yarm_log!("INIT_ORCH_WRITER_SKIPPED reason=grant_request_recv_failed err={:?}", err);
        err
    })?;
    let (_, readiness_send_root, readiness_recv_root) = match kernel.create_endpoint(16) {
        Ok(v) => v,
        Err(err) => {
            crate::yarm_log!("INIT_ORCH_WRITER_SKIPPED reason=create_readiness_endpoint_failed err={:?}", err);
            return Err(err);
        }
    };
    let readiness_send = kernel.grant_capability_task_to_task_with_rights(
        0,
        readiness_send_root,
        init_tid,
        CapRights::SEND,
    )
    .map_err(|err| {
        crate::yarm_log!("INIT_ORCH_WRITER_SKIPPED reason=grant_readiness_send_failed err={:?}", err);
        err
    })?;
    let readiness_recv = kernel.grant_capability_task_to_task_with_rights(
        0,
        readiness_recv_root,
        init_tid,
        CapRights::RECEIVE,
    )
    .map_err(|err| {
        crate::yarm_log!("INIT_ORCH_WRITER_SKIPPED reason=grant_readiness_recv_failed err={:?}", err);
        err
    })?;
    startup_args[12] = yarm_ipc_abi::process_abi::InitOrchestrationCapsV1::VERSION as u64;
    startup_args[13] = request_send.0;
    startup_args[14] = request_recv.0;
    startup_args[15] = readiness_recv.0;
    startup_args[16] = readiness_send.0;
    crate::yarm_log!(
        "INIT_ORCH_WRITER_DONE slot13={} slot14={} slot15={} slot16={}",
        startup_args[13],
        startup_args[14],
        startup_args[15],
        startup_args[16]
    );
    Ok(())
}

#[cfg(test)]
mod tests;
