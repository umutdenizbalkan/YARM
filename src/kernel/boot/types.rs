// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelError {
    VmFull,
    SchedulerFull,
    CapabilityFull,
    EndpointFull,
    InvalidCapability,
    MissingRight,
    WrongObject,
    StaleCapability,
    EndpointQueueFull,
    TaskTableFull,
    TaskMissing,
    MemoryObjectFull,
    MemoryObjectMissing,
    Vm(VmError),
    UserMemoryFault,
    WouldBlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapHandleError {
    MissingTrapFrame,
    Syscall(SyscallError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserImageSpec {
    pub tid: u64,
    pub entry: usize,
    pub asid: Option<Asid>,
    pub class: TaskClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnedUserTask {
    pub tid: u64,
    pub entry: usize,
    pub asid: Option<Asid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceServerDelegation {
    pub server_tid: ThreadId,
    pub irq_line: u16,
    pub mem_cap: CapId,
    pub dma_offset: usize,
    pub dma_len: usize,
    pub iova_cap: CapId,
    pub iova_base: usize,
    pub iova_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverDelegationBundle {
    pub irq_cap: CapId,
    pub dma_cap: CapId,
    pub iova_cap: CapId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverBundlePlan {
    pub server_tid: ThreadId,
    pub irq_line: u16,
    pub mem_cap: CapId,
    pub dma_len: usize,
    pub iova_cap: CapId,
    pub iova_base: usize,
    pub iova_len: usize,
}

impl DriverBundlePlan {
    pub const fn standard(
        server_tid: ThreadId,
        irq_line: u16,
        mem_cap: CapId,
        dma_len: usize,
        iova_cap: CapId,
        iova_base: usize,
        iova_len: usize,
    ) -> Self {
        Self {
            server_tid,
            irq_line,
            mem_cap,
            dma_len,
            iova_cap,
            iova_base,
            iova_len,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpcFastpathResult {
    pub switched_to_waiter: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IpcPathTelemetry {
    pub fastpath_attempts: u64,
    pub fastpath_switches: u64,
    pub queued_sends: u64,
    pub blocked_sends: u64,
    pub rendezvous_handoffs: u64,
    pub transfer_records_created: u64,
    pub transfer_records_materialized: u64,
    pub transfer_records_revoked: u64,
    pub transfer_record_failures: u64,
    pub shared_mem_bytes_mapped: u64,
    pub shared_mem_bytes_released: u64,
    pub transfer_release_calls: u64,
    pub scheduler_dispatch_calls: u64,
    pub scheduler_yield_calls: u64,
    pub scheduler_context_switches: u64,
    pub scheduler_fastpath_handoffs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityPoolTelemetry {
    pub used: usize,
    pub capacity: usize,
    pub near_full: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityTelemetry {
    pub endpoints: CapacityPoolTelemetry,
    pub notifications: CapacityPoolTelemetry,
    pub tasks: CapacityPoolTelemetry,
    pub drivers: CapacityPoolTelemetry,
    pub memory_objects: CapacityPoolTelemetry,
    pub capability_slots: CapacityPoolTelemetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelCapacityProfile {
    HostedDefault,
    Constrained,
    Throughput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeCapacityConfig {
    pub max_endpoints: usize,
    pub max_notifications: usize,
    pub max_tasks: usize,
    pub max_drivers: usize,
    pub max_memory_objects: usize,
    pub max_transfer_envelopes: usize,
    pub max_capability_slots: usize,
}
