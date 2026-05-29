// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

impl KernelState {
    const CAPACITY_NEAR_FULL_PERCENT: usize = 90;

    fn capacity_pool(used: usize, capacity: usize) -> CapacityPoolTelemetry {
        let near_full = if capacity == 0 {
            false
        } else {
            used.saturating_mul(100) >= capacity.saturating_mul(Self::CAPACITY_NEAR_FULL_PERCENT)
        };
        CapacityPoolTelemetry {
            used,
            capacity,
            near_full,
        }
    }

    pub fn capacity_telemetry(&self) -> CapacityTelemetry {
        let limits = self.runtime_capacity_config();
        let cnode_capability_slots_used: usize = self.with_capability_state(|capability| {
            capability
                .cnode_spaces
                .iter()
                .flatten()
                .map(|space| kernel_ref(&space.cspace).occupied_slots())
                .sum()
        });
        let capability_slots_used = cnode_capability_slots_used;
        let endpoint_used = self.with_ipc_state(|ipc| ipc.endpoints.iter().flatten().count());
        let notification_used =
            self.with_ipc_state(|ipc| ipc.notifications.iter().flatten().count());
        let task_used = self.with_tcbs(|tcbs| tcbs.iter().flatten().count());
        let memory_object_used =
            self.with_memory_state(|memory| memory.memory_objects.iter().flatten().count());
        CapacityTelemetry {
            endpoints: Self::capacity_pool(endpoint_used, limits.max_endpoints),
            notifications: Self::capacity_pool(notification_used, limits.max_notifications),
            tasks: Self::capacity_pool(task_used, limits.max_tasks),
            drivers: Self::capacity_pool(
                self.with_driver_state(|driver| driver.driver_records.iter().flatten().count()),
                limits.max_drivers,
            ),
            memory_objects: Self::capacity_pool(memory_object_used, limits.max_memory_objects),
            capability_slots: Self::capacity_pool(
                capability_slots_used,
                limits.max_total_cnode_slots,
            ),
        }
    }

    pub fn capability_space_telemetry(&self) -> CapabilitySpaceTelemetry {
        self.with_capability_state(|capability| {
            capability.cnode_spaces.iter().flatten().fold(
                CapabilitySpaceTelemetry::default(),
                |mut acc, space| {
                    let telemetry = kernel_ref(&space.cspace).revoke_scratch_telemetry();
                    acc.cnode_spaces = acc.cnode_spaces.saturating_add(1);
                    acc.revoke_scratch_cache_hits = acc
                        .revoke_scratch_cache_hits
                        .saturating_add(telemetry.cache_hits);
                    acc.revoke_scratch_cache_misses = acc
                        .revoke_scratch_cache_misses
                        .saturating_add(telemetry.cache_misses);
                    acc.revoke_scratch_cache_drops = acc
                        .revoke_scratch_cache_drops
                        .saturating_add(telemetry.cache_drops);
                    acc
                },
            )
        })
    }

    pub fn capacity_profile(&self) -> KernelCapacityProfile {
        self.with_boot_config(|boot_config| boot_config.capacity_profile)
    }

    pub fn runtime_capacity_config(&self) -> RuntimeCapacityConfig {
        Self::runtime_capacity_config_for_profile(
            self.with_boot_config(|boot_config| boot_config.capacity_profile),
        )
    }

    pub(crate) fn runtime_capacity_config_for_profile(
        profile: KernelCapacityProfile,
    ) -> RuntimeCapacityConfig {
        match profile {
            KernelCapacityProfile::HostedDefault => Self::runtime_capacity_config_with_cnodes(
                MAX_ENDPOINTS,
                MAX_NOTIFICATIONS,
                MAX_TASKS,
                MAX_DRIVERS,
                MAX_MEMORY_OBJECTS,
                MAX_TRANSFER_ENVELOPES,
                crate::kernel::capabilities::MAX_CAPABILITIES_PER_CSPACE,
                crate::kernel::capabilities::MAX_CAPABILITIES_PER_CSPACE,
            ),
            KernelCapacityProfile::Constrained => Self::runtime_capacity_config_with_cnodes(
                core::cmp::max(1, MAX_ENDPOINTS / 2),
                core::cmp::max(1, MAX_NOTIFICATIONS / 2),
                core::cmp::max(2, MAX_TASKS / 2),
                core::cmp::max(1, MAX_DRIVERS / 2),
                core::cmp::max(1, MAX_MEMORY_OBJECTS / 2),
                core::cmp::max(1, MAX_TRANSFER_ENVELOPES / 2),
                core::cmp::max(
                    1,
                    crate::kernel::capabilities::MAX_CAPABILITIES_PER_CSPACE / 2,
                ),
                crate::kernel::capabilities::MAX_CAPABILITIES_PER_CSPACE,
            ),
            KernelCapacityProfile::Throughput => Self::runtime_capacity_config_with_cnodes(
                MAX_ENDPOINTS,
                MAX_NOTIFICATIONS,
                MAX_TASKS,
                MAX_DRIVERS,
                MAX_MEMORY_OBJECTS,
                MAX_TRANSFER_ENVELOPES,
                crate::kernel::capabilities::MAX_CAPABILITIES_PER_CSPACE,
                crate::kernel::capabilities::MAX_CAPABILITIES_PER_CSPACE,
            ),
        }
    }

    fn runtime_capacity_config_with_cnodes(
        max_endpoints: usize,
        max_notifications: usize,
        max_tasks: usize,
        max_drivers: usize,
        max_memory_objects: usize,
        max_transfer_envelopes: usize,
        default_cnode_slot_capacity: usize,
        driver_cnode_slot_capacity: usize,
    ) -> RuntimeCapacityConfig {
        let app_slots = max_tasks
            .saturating_sub(max_drivers)
            .saturating_mul(default_cnode_slot_capacity);
        let driver_slots = max_drivers.saturating_mul(driver_cnode_slot_capacity);
        let max_total_cnode_slots = app_slots.saturating_add(driver_slots);
        RuntimeCapacityConfig {
            max_endpoints,
            max_notifications,
            max_tasks,
            max_drivers,
            max_memory_objects,
            max_transfer_envelopes,
            default_cnode_slot_capacity,
            driver_cnode_slot_capacity,
            max_total_cnode_slots,
            max_capability_slots: max_total_cnode_slots,
        }
    }
}
