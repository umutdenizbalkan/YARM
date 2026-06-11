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
    /// Startup entry ABI arguments passed to userspace `_start`.
    pub startup_args: [u64; 18],
    /// TID of the spawning task (0 = no cap delegation needed).
    pub spawner_tid: u64,
    /// Recv cap ID in the spawner's cnode to delegate into startup slot 12.
    pub service_recv_cap: u64,
    /// Recv cap ID in the spawner's cnode to delegate into startup slot 2
    /// (service-local reply recv endpoint for outbound ipc_call replies).
    pub service_reply_recv_cap: u64,
    /// Up to 4 send cap IDs in the spawner's cnode to delegate into slots 13-16.
    pub extra_send_caps: [u64; 4],
}

impl UserImageSpec {
    pub const DEFAULT_STARTUP_ARGS: [u64; 18] = [0; 18];
}

impl Default for UserImageSpec {
    fn default() -> Self {
        Self {
            tid: 0,
            entry: 0,
            asid: None,
            class: TaskClass::App,
            startup_args: Self::DEFAULT_STARTUP_ARGS,
            spawner_tid: 0,
            service_recv_cap: 0,
            service_reply_recv_cap: 0,
            extra_send_caps: [0; 4],
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

/// Stage 105 / D5: outcome of [`super::KernelState::try_set_reply_cap_waiter_cap`].
///
/// The D5 reply-cap split treats any non-[`Set`](ReplyRecordSetOutcome::Set)
/// outcome as a stale window between the rank-4 mint and the rank-3 record
/// write and rolls back the mint. The wrapper
/// `KernelState::set_reply_cap_waiter_cap` discards the outcome and is used
/// only by the canonical global-lock path, where staleness is unreachable
/// because the global lock spans the mint→set sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyRecordSetOutcome {
    /// Record was updated; D5 split can finalize Phase C.
    Set,
    /// `reply_index >= MAX_REPLY_CAPS`. Indicates a corrupted reply object —
    /// in practice unreachable because the source envelope guarantees the
    /// index is valid, but the variant exists so the typed outcome is total.
    IndexOutOfRange,
    /// Generation on the slot no longer matches the one captured before the
    /// rank-4 mint: the record was revoked and reused between mint and set.
    /// D5 split rolls back the mint.
    GenerationMismatch,
    /// Generation matched but the slot itself is `None`: the record was
    /// revoked between mint and set without yet being reused. D5 split rolls
    /// back the mint.
    SlotEmpty,
}

impl ReplyRecordSetOutcome {
    /// True only for [`Set`](Self::Set).
    pub fn is_set(self) -> bool {
        matches!(self, Self::Set)
    }
    /// Smoke-log reason tag matching the `D5_REPLY_RECORD_SET_STALE reason=`
    /// values emitted by [`super::KernelState::try_set_reply_cap_waiter_cap`].
    pub fn stale_reason(self) -> Option<&'static str> {
        match self {
            Self::Set => None,
            Self::IndexOutOfRange => Some("index_out_of_range"),
            Self::GenerationMismatch => Some("generation_mismatch"),
            Self::SlotEmpty => Some("slot_empty"),
        }
    }
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
