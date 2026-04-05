// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

impl KernelState {
    const CAPACITY_NEAR_FULL_PERCENT: usize = 90;
    const MAX_CAPABILITY_SLOTS_ACROSS_CNODES: usize =
        MAX_TASKS * crate::kernel::capabilities::MAX_CAPABILITIES_PER_CSPACE;

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
                limits.max_capability_slots,
            ),
        }
    }

    pub fn capacity_profile(&self) -> KernelCapacityProfile {
        self.with_boot_config(|boot_config| boot_config.capacity_profile)
    }

    pub fn runtime_capacity_config(&self) -> RuntimeCapacityConfig {
        match self.with_boot_config(|boot_config| boot_config.capacity_profile) {
            KernelCapacityProfile::HostedDefault => RuntimeCapacityConfig {
                max_endpoints: MAX_ENDPOINTS,
                max_notifications: MAX_NOTIFICATIONS,
                max_tasks: MAX_TASKS,
                max_drivers: MAX_DRIVERS,
                max_memory_objects: MAX_MEMORY_OBJECTS,
                max_transfer_envelopes: MAX_TRANSFER_ENVELOPES,
                max_capability_slots: Self::MAX_CAPABILITY_SLOTS_ACROSS_CNODES,
            },
            KernelCapacityProfile::Constrained => RuntimeCapacityConfig {
                max_endpoints: core::cmp::max(1, MAX_ENDPOINTS / 2),
                max_notifications: core::cmp::max(1, MAX_NOTIFICATIONS / 2),
                max_tasks: core::cmp::max(2, MAX_TASKS / 2),
                max_drivers: core::cmp::max(1, MAX_DRIVERS / 2),
                max_memory_objects: core::cmp::max(1, MAX_MEMORY_OBJECTS / 2),
                max_transfer_envelopes: core::cmp::max(1, MAX_TRANSFER_ENVELOPES / 2),
                max_capability_slots: core::cmp::max(
                    1,
                    Self::MAX_CAPABILITY_SLOTS_ACROSS_CNODES / 2,
                ),
            },
            KernelCapacityProfile::Throughput => RuntimeCapacityConfig {
                max_endpoints: MAX_ENDPOINTS,
                max_notifications: MAX_NOTIFICATIONS,
                max_tasks: MAX_TASKS,
                max_drivers: MAX_DRIVERS,
                max_memory_objects: MAX_MEMORY_OBJECTS,
                max_transfer_envelopes: MAX_TRANSFER_ENVELOPES,
                max_capability_slots: Self::MAX_CAPABILITY_SLOTS_ACROSS_CNODES,
            },
        }
    }
}
