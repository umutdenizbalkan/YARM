// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;
pub use yarm_kernel::boot::{
    CapabilitySpaceTelemetry, CapacityPoolTelemetry, CapacityTelemetry, IpcFastpathResult,
    IpcPathTelemetry, KernelCapacityProfile, RuntimeCapacityConfig, TidAllocationTelemetry,
};

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
    /// Canonical startup slot block copied to userspace.
    ///
    /// Slot contract:
    /// - slot0 => task_id / tid
    /// - slot1 => process-manager request-send cap
    /// - slot2 => process-manager reply-recv cap
    /// - slot3.. => extended startup metadata/caps
    ///
    /// Startup register ABI derives from these slots:
    /// - arg0/arg1/arg2 mirror slot0/slot1/slot2.
    /// - arg3 carries startup-slot-block userspace pointer.
    /// - arg4 carries startup-slot count.
    /// - arg5 is reserved (0).
    pub startup_args: [u64; Self::STARTUP_SLOT_COUNT],
}

impl UserImageSpec {
    pub const STARTUP_SLOT_COUNT: usize = 17;
    pub const STARTUP_ARGS_BYTES: usize = Self::STARTUP_SLOT_COUNT * core::mem::size_of::<u64>();
    pub const DEFAULT_STARTUP_ARGS: [u64; Self::STARTUP_SLOT_COUNT] = [0; Self::STARTUP_SLOT_COUNT];
}

impl Default for UserImageSpec {
    fn default() -> Self {
        Self {
            tid: 0,
            entry: 0,
            asid: None,
            class: TaskClass::App,
            startup_args: Self::DEFAULT_STARTUP_ARGS,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem;

    #[test]
    fn pass_c_boot_telemetry_types_are_reexported_from_yarm_kernel() {
        assert_eq!(
            mem::size_of::<IpcPathTelemetry>(),
            mem::size_of::<yarm_kernel::boot::IpcPathTelemetry>()
        );
        assert_eq!(
            mem::size_of::<CapacityTelemetry>(),
            mem::size_of::<yarm_kernel::boot::CapacityTelemetry>()
        );
        assert_eq!(
            mem::size_of::<CapabilitySpaceTelemetry>(),
            mem::size_of::<yarm_kernel::boot::CapabilitySpaceTelemetry>()
        );
        assert_eq!(
            KernelCapacityProfile::HostedDefault as u8,
            yarm_kernel::boot::KernelCapacityProfile::HostedDefault as u8
        );
        let _cfg: yarm_kernel::boot::RuntimeCapacityConfig = RuntimeCapacityConfig {
            max_endpoints: 1,
            max_notifications: 1,
            max_tasks: 1,
            max_drivers: 1,
            max_memory_objects: 1,
            max_transfer_envelopes: 1,
            default_cnode_slot_capacity: 1,
            driver_cnode_slot_capacity: 1,
            max_total_cnode_slots: 1,
            max_capability_slots: 1,
        };
    }
}
