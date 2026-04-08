// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TidAllocationTelemetry {
    pub dynamic_tid_allocations: u64,
    pub dynamic_tid_wraps: u64,
    pub gap_floor_repairs: u64,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_defaults_to_zero() {
        let telemetry = IpcPathTelemetry::default();
        assert_eq!(telemetry.fastpath_attempts, 0);
        assert_eq!(telemetry.scheduler_fastpath_handoffs, 0);
        let tid = TidAllocationTelemetry::default();
        assert_eq!(tid.dynamic_tid_allocations, 0);
        assert_eq!(tid.dynamic_tid_wraps, 0);
        assert_eq!(tid.gap_floor_repairs, 0);
    }

    #[test]
    fn capacity_profile_enum_is_stable() {
        assert_eq!(KernelCapacityProfile::HostedDefault as u8, 0);
        assert_eq!(KernelCapacityProfile::Constrained as u8, 1);
        assert_eq!(KernelCapacityProfile::Throughput as u8, 2);
    }
}
