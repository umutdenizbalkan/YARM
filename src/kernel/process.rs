// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::task::ThreadGroupId;

const MAX_PROCESSES: usize = 64;
const MAX_THREADS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessManagerError {
    Malformed,
    Unsupported,
    TableFull,
    UnknownProcess,
    InvalidTransport,
    PermissionDenied,
    WouldBlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitResult {
    pub waited_pid: ProcessId,
    pub exit_code: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessRecord {
    pid: ProcessId,
    parent_pid: ProcessId,
    exited: bool,
    exit_code: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ThreadIdentityRecord {
    tid: u64,
    pid: ProcessId,
    thread_group_id: ThreadGroupId,
}

#[derive(Debug)]
pub struct ProcessManager {
    next_pid: ProcessId,
    table: [Option<ProcessRecord>; MAX_PROCESSES],
    threads: [Option<ThreadIdentityRecord>; MAX_THREADS],
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessManager {
    pub const fn new() -> Self {
        Self {
            next_pid: ProcessId(1000),
            table: [None; MAX_PROCESSES],
            threads: [None; MAX_THREADS],
        }
    }

    pub fn allocate_process(
        &mut self,
        parent_pid: ProcessId,
    ) -> Result<ProcessId, ProcessManagerError> {
        let pid = self.next_pid;
        self.next_pid = ProcessId(self.next_pid.0.saturating_add(1));
        if let Some(slot) = self.table.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(ProcessRecord {
                pid,
                parent_pid,
                exited: false,
                exit_code: 0,
            });
            self.register_thread_identity(pid, pid.0, ThreadGroupId(pid.0))?;
            Ok(pid)
        } else {
            Err(ProcessManagerError::TableFull)
        }
    }

    pub fn register_thread_identity(
        &mut self,
        pid: ProcessId,
        tid: u64,
        thread_group_id: ThreadGroupId,
    ) -> Result<(), ProcessManagerError> {
        if self
            .threads
            .iter()
            .flatten()
            .any(|record| record.tid == tid)
        {
            return Ok(());
        }
        let slot = self
            .threads
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(ProcessManagerError::TableFull)?;
        *slot = Some(ThreadIdentityRecord {
            tid,
            pid,
            thread_group_id,
        });
        Ok(())
    }

    pub fn thread_identity(&self, tid: u64) -> Option<(ProcessId, ThreadGroupId)> {
        self.threads
            .iter()
            .flatten()
            .find(|record| record.tid == tid)
            .map(|record| (record.pid, record.thread_group_id))
    }

    pub fn process_id_for_tid(&self, caller_tid: u64) -> ProcessId {
        self.thread_identity(caller_tid)
            .map(|record| record.0)
            .unwrap_or(ProcessId(caller_tid))
    }

    pub fn parent_of(&self, pid: ProcessId) -> Option<ProcessId> {
        self.table
            .iter()
            .flatten()
            .find(|record| record.pid == pid)
            .map(|record| record.parent_pid)
    }

    pub fn mark_exit(&mut self, pid: ProcessId, code: u64) -> Result<(), ProcessManagerError> {
        let record = self
            .table
            .iter_mut()
            .flatten()
            .find(|record| record.pid == pid)
            .ok_or(ProcessManagerError::UnknownProcess)?;
        record.exited = true;
        record.exit_code = code;
        Ok(())
    }

    pub fn insert_synthetic_exit_for_tid(
        &mut self,
        caller_tid: u64,
        code: u64,
    ) -> Result<ProcessId, ProcessManagerError> {
        let caller_pid = self.process_id_for_tid(caller_tid);
        if self.mark_exit(caller_pid, code).is_ok() {
            return Ok(caller_pid);
        }
        let slot = self
            .table
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(ProcessManagerError::TableFull)?;
        *slot = Some(ProcessRecord {
            pid: caller_pid,
            parent_pid: ProcessId(caller_pid.0.saturating_sub(1)),
            exited: true,
            exit_code: code,
        });
        self.register_thread_identity(caller_pid, caller_tid, ThreadGroupId(caller_tid))?;
        Ok(caller_pid)
    }

    pub fn wait_exited(
        &mut self,
        target_pid: ProcessId,
    ) -> Result<WaitResult, ProcessManagerError> {
        if let Some((idx, record)) = self
            .table
            .iter()
            .enumerate()
            .find_map(|(idx, slot)| slot.map(|record| (idx, record)))
            .filter(|(_, record)| record.pid == target_pid)
        {
            if !record.exited {
                return Err(ProcessManagerError::WouldBlock);
            }
            let result = WaitResult {
                waited_pid: target_pid,
                exit_code: record.exit_code,
            };
            self.table[idx] = None;
            Ok(result)
        } else {
            Err(ProcessManagerError::UnknownProcess)
        }
    }

    pub fn live_process_count(&self) -> usize {
        self.table.iter().flatten().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_process_assigns_pid_and_tracks_parent() {
        let mut pm = ProcessManager::new();
        let pid = pm.allocate_process(ProcessId(7)).expect("alloc");
        assert_eq!(pid, ProcessId(1000));
        assert_eq!(pm.parent_of(pid), Some(ProcessId(7)));
    }

    #[test]
    fn wait_exited_reaps_and_removes_process() {
        let mut pm = ProcessManager::new();
        let pid = pm.allocate_process(ProcessId(1)).expect("alloc");
        pm.mark_exit(pid, 9).expect("exit");
        let waited = pm.wait_exited(pid).expect("wait");
        assert_eq!(waited.waited_pid, pid);
        assert_eq!(waited.exit_code, 9);
        assert_eq!(pm.live_process_count(), 0);
    }

    #[test]
    fn wait_exited_returns_would_block_for_running_process() {
        let mut pm = ProcessManager::new();
        let pid = pm.allocate_process(ProcessId(1)).expect("alloc");
        assert_eq!(pm.wait_exited(pid), Err(ProcessManagerError::WouldBlock));
    }

    #[test]
    fn synthetic_exit_supports_unregistered_callers() {
        let mut pm = ProcessManager::new();
        let pid = pm
            .insert_synthetic_exit_for_tid(2121, 5)
            .expect("synthetic exit");
        assert_eq!(pid, ProcessId(2121));
        let waited = pm.wait_exited(pid).expect("wait");
        assert_eq!(waited.exit_code, 5);
    }

    #[test]
    fn process_manager_tracks_explicit_thread_identities() {
        let mut pm = ProcessManager::new();
        let pid = pm.allocate_process(ProcessId(1)).expect("pid");
        pm.register_thread_identity(pid, 2000, ThreadGroupId(pid.0))
            .expect("thread");
        assert_eq!(pm.process_id_for_tid(2000), pid);
        assert_eq!(pm.thread_identity(2000), Some((pid, ThreadGroupId(pid.0))));
    }
}
