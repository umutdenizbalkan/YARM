// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

impl KernelState {
    pub(crate) fn switch_to_runnable_tid(&mut self, tid: ThreadId) -> Result<bool, KernelError> {
        let mut spins = 0usize;
        while spins < MAX_TASKS {
            if self.current_tid() == Some(tid.0) {
                return Ok(true);
            }
            self.yield_current()?;
            spins += 1;
        }
        Ok(self.current_tid() == Some(tid.0))
    }

    pub(crate) fn tcb_mut(&mut self, tid: u64) -> Option<&mut ThreadControlBlock> {
        self.tcbs.iter_mut().flatten().find(|tcb| tcb.tid.0 == tid)
    }

    pub fn task_status(&self, tid: u64) -> Option<TaskStatus> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| tcb.status)
    }

    pub fn task_restart_token(&self, tid: u64) -> Option<u64> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .and_then(|tcb| tcb.restart.token.map(|token| token.0))
    }

    #[cfg(test)]
    pub(crate) fn cspace_for_cnode(&self, cnode: CNodeId) -> Option<&CapabilitySpace> {
        self.capability
            .cnode_spaces
            .iter()
            .flatten()
            .find(|space| space.id == cnode)
            .map(|space| kernel_ref(&space.cspace))
    }

    #[cfg(test)]
    pub(crate) fn cspace_for_cnode_mut(&mut self, cnode: CNodeId) -> Option<&mut CapabilitySpace> {
        self.capability
            .cnode_spaces
            .iter_mut()
            .flatten()
            .find(|space| space.id == cnode)
            .map(|space| kernel_mut(&mut space.cspace))
    }
}
