// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

pub(crate) struct CapabilityService<'a> {
    pub(crate) kernel: &'a KernelState,
}

pub(crate) struct CapabilityServiceMut<'a> {
    pub(crate) kernel: &'a mut KernelState,
}

impl KernelState {
    pub(crate) fn capability_service(&self) -> CapabilityService<'_> {
        CapabilityService { kernel: self }
    }

    pub(crate) fn capability_service_mut(&mut self) -> CapabilityServiceMut<'_> {
        CapabilityServiceMut { kernel: self }
    }
}

impl CapabilityService<'_> {
    pub(crate) fn resolve_current_task_capability(&self, cap: CapId) -> Option<Capability> {
        self.kernel.current_task_capability(cap)
    }

    pub(crate) fn resolve_task_capability(&self, tid: u64, cap: CapId) -> Option<Capability> {
        self.kernel.task_capability(tid, cap)
    }

    #[cfg(test)]
    pub(crate) fn current_task_capability_has_right(&self, cap: CapId, right: CapRights) -> bool {
        self.resolve_current_task_capability(cap)
            .map(|capability| capability.has_right(right))
            .unwrap_or(false)
    }
}

impl CapabilityServiceMut<'_> {
    pub(crate) fn grant_task_to_task_with_rights(
        &mut self,
        source_tid: u64,
        source_cap: CapId,
        dest_tid: u64,
        rights: CapRights,
    ) -> Result<CapId, KernelError> {
        self.kernel
            .grant_capability_task_to_task_with_rights(source_tid, source_cap, dest_tid, rights)
    }
}
