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
    pub queued_recvs: u64,
    pub split_recv_v2_deliveries: u64,
    pub ipc_call_split_deliveries: u64,
    pub ipc_reply_split_deliveries: u64,
    pub cap_transfer_recv_v2_deliveries: u64,
    /// Stage 4E: cap-transfer messages buffered into the endpoint queue via the split
    /// send path (no receiver waiter).  Incremented alongside queued_sends.
    pub cap_transfer_stage4e_enqueued: u64,
    /// Stage 104 / D1: recv-side cap materializations serviced through the
    /// phase-separated split router (`cap_transfer_split`) instead of the
    /// canonical `materialize_received_message_cap` transfer arm.  Counts only
    /// the supported case (FLAG_CAP_TRANSFER / FLAG_CAP_TRANSFER_PLAIN,
    /// non-reply, non-shared-region).  Reply-cap, shared-region, and fallback
    /// materializations do NOT increment this.
    pub d1_split_materializations: u64,
    /// Stage 105 / D5: reply-cap recv-side materializations serviced through
    /// the phase-separated split engine (Phase A → B → B' with
    /// `try_set_reply_cap_waiter_cap` + mint rollback).  Counts only the
    /// FLAG_REPLY_CAP supported case.  Stale-rollback failures still
    /// increment `d5_split_reply_rollbacks` (below).
    pub d5_split_reply_materializations: u64,
    /// Stage 105 / D5: reply-cap split materializations that hit the
    /// mint→record race window and rolled back via
    /// `rollback_materialized_recv_cap`.  Should normally be 0 on a
    /// well-behaved workload; a non-zero count is benign (the reply object
    /// stays live and the receiver gets the same `WrongObject` it would have
    /// gotten if the revoke had landed before the mint), but it is the
    /// signal smoke tests use to confirm the rollback path is exercised.
    pub d5_split_reply_rollbacks: u64,
    /// Stage 106 / D2: blocking-recv waiter publishes through the typed live
    /// primitive (`publish_recv_waiter_live`). Increments once per blocked
    /// receive that parked a waiter.
    pub d2_recv_waiter_publishes: u64,
    /// Stage 106 / D2: no-lost-wakeup unwinds (publish observed a non-empty
    /// queue after the scheduler block). Always 0 under the serialized
    /// global lock; non-zero only after the SharedKernel seam split, where
    /// it indicates the race branch fired and was handled correctly.
    pub d2_publish_race_unwinds: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CapabilitySpaceTelemetry {
    pub cnode_spaces: usize,
    pub revoke_scratch_cache_hits: u64,
    pub revoke_scratch_cache_misses: u64,
    pub revoke_scratch_cache_drops: u64,
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
    pub default_cnode_slot_capacity: usize,
    pub driver_cnode_slot_capacity: usize,
    pub max_total_cnode_slots: usize,
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
