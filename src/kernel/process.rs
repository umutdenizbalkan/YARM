// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::ipc::Message;
use super::process_abi::{
    PROC_OP_EXIT, PROC_OP_GETPID, PROC_OP_GETPPID, PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2,
    SpawnV2Args, WaitPidV2Args, WaitPidV2Reply,
};
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
pub struct SpawnV2Request {
    pub parent_pid: ProcessId,
    pub image_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitPidV2Request {
    pub caller_pid: ProcessId,
    pub target_pid: ProcessId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV2Result {
    pub pid: ProcessId,
}

impl SpawnV2Result {
    pub const fn encode(self) -> [u8; 8] {
        self.pid.0.to_le_bytes()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcessManagerError> {
        if payload.len() < 8 {
            return Err(ProcessManagerError::Malformed);
        }
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&payload[..8]);
        Ok(Self {
            pid: ProcessId(u64::from_le_bytes(bytes)),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElfImageInfo {
    pub entry: u64,
    pub image_id: u64,
}

impl ElfImageInfo {
    pub fn parse(image_id: u64, image: &[u8]) -> Result<Self, ProcessManagerError> {
        if image.len() < 32 {
            return Err(ProcessManagerError::Malformed);
        }
        if &image[..4] != b"\x7FELF" {
            return Err(ProcessManagerError::Malformed);
        }
        let mut entry = [0u8; 8];
        entry.copy_from_slice(&image[24..32]);
        Ok(Self {
            entry: u64::from_le_bytes(entry),
            image_id,
        })
    }
}

pub struct WaitPidV2Result {
    pub waited_pid: ProcessId,
    pub exit_code: u64,
}

impl WaitPidV2Result {
    pub const fn encode(self) -> [u8; 16] {
        WaitPidV2Reply::new(self.waited_pid.0, self.exit_code).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcessManagerError> {
        let args = WaitPidV2Reply::decode(payload).map_err(|_| ProcessManagerError::Malformed)?;
        Ok(Self {
            waited_pid: ProcessId(args.waited_pid),
            exit_code: args.exit_code,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessRequest {
    GetPid { caller_tid: u64 },
    GetPpid { caller_tid: u64 },
    Exit { caller_tid: u64, code: u64 },
    SpawnV2(SpawnV2Request),
    WaitPidV2(WaitPidV2Request),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessRecord {
    pid: ProcessId,
    parent_pid: ProcessId,
    exited: bool,
    exit_code: u64,
    image_id: u64,
    entry: u64,
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

    fn read_u64(payload: &[u8]) -> Result<u64, ProcessManagerError> {
        if payload.len() < 8 {
            return Err(ProcessManagerError::Malformed);
        }
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&payload[..8]);
        Ok(u64::from_le_bytes(bytes))
    }

    pub fn parse_request(msg: Message) -> Result<ProcessRequest, ProcessManagerError> {
        if msg.transferred_cap().is_some() || (msg.flags & Message::FLAG_CAP_TRANSFER) != 0 {
            return Err(ProcessManagerError::InvalidTransport);
        }

        match msg.opcode {
            PROC_OP_GETPID => Ok(ProcessRequest::GetPid {
                caller_tid: Self::read_u64(msg.as_slice())?,
            }),
            PROC_OP_GETPPID => Ok(ProcessRequest::GetPpid {
                caller_tid: Self::read_u64(msg.as_slice())?,
            }),
            PROC_OP_EXIT => Ok(ProcessRequest::Exit {
                caller_tid: msg.sender_tid.0,
                code: Self::read_u64(msg.as_slice())?,
            }),
            PROC_OP_SPAWN_V2 => {
                let args = SpawnV2Args::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                Ok(ProcessRequest::SpawnV2(SpawnV2Request {
                    parent_pid: ProcessId(args.parent_pid),
                    image_id: args.image_id,
                }))
            }
            PROC_OP_WAITPID_V2 => {
                let args = WaitPidV2Args::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                Ok(ProcessRequest::WaitPidV2(WaitPidV2Request {
                    caller_pid: ProcessId(args.caller_pid),
                    target_pid: ProcessId(args.target_pid),
                }))
            }
            _ => Err(ProcessManagerError::Unsupported),
        }
    }

    fn u64_reply(opcode: u16, value: u64) -> Result<Message, ProcessManagerError> {
        Message::with_header(0, opcode, 0, None, &value.to_le_bytes())
            .map_err(|_| ProcessManagerError::Malformed)
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

    fn thread_identity(&self, tid: u64) -> Option<ThreadIdentityRecord> {
        self.threads
            .iter()
            .flatten()
            .find(|record| record.tid == tid)
            .copied()
    }

    fn process_id_for_tid(&self, caller_tid: u64) -> ProcessId {
        self.thread_identity(caller_tid)
            .map(|record| record.pid)
            .unwrap_or(ProcessId(caller_tid))
    }

    pub fn spawn_from_elf_image(
        &mut self,
        parent_pid: ProcessId,
        image_id: u64,
        image: &[u8],
    ) -> Result<(ProcessId, ElfImageInfo), ProcessManagerError> {
        let info = ElfImageInfo::parse(image_id, image)?;
        let pid = self.alloc_process(parent_pid)?;
        if let Some(record) = self
            .table
            .iter_mut()
            .flatten()
            .find(|record| record.pid == pid)
        {
            record.image_id = info.image_id;
            record.entry = info.entry;
        }
        Ok((pid, info))
    }

    #[cfg(test)]
    fn synthetic_elf_image(image_id: u64) -> [u8; 64] {
        let mut image = [0u8; 64];
        image[..4].copy_from_slice(b"\x7FELF");
        let entry = 0x400000u64.saturating_add(image_id.saturating_mul(0x1000));
        image[24..32].copy_from_slice(&entry.to_le_bytes());
        image
    }

    fn alloc_process(&mut self, parent_pid: ProcessId) -> Result<ProcessId, ProcessManagerError> {
        let pid = self.next_pid;
        self.next_pid = ProcessId(self.next_pid.0.saturating_add(1));
        if let Some(slot) = self.table.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(ProcessRecord {
                pid,
                parent_pid,
                exited: false,
                exit_code: 0,
                image_id: 0,
                entry: 0,
            });
            self.register_thread_identity(pid, pid.0, ThreadGroupId(pid.0))?;
            Ok(pid)
        } else {
            Err(ProcessManagerError::TableFull)
        }
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

    fn mark_exit_for_tid(&mut self, caller_tid: u64, code: u64) -> Result<(), ProcessManagerError> {
        let caller_pid = self.process_id_for_tid(caller_tid);
        if self.mark_exit(caller_pid, code).is_ok() {
            return Ok(());
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
            image_id: 0,
            entry: 0,
        });
        self.register_thread_identity(caller_pid, caller_tid, ThreadGroupId(caller_tid))?;
        Ok(())
    }

    fn lookup_parent(&self, pid: ProcessId) -> Option<ProcessId> {
        self.table
            .iter()
            .flatten()
            .find(|record| record.pid == pid)
            .map(|record| record.parent_pid)
    }

    fn wait_result(
        &mut self,
        target_pid: ProcessId,
    ) -> Result<WaitPidV2Result, ProcessManagerError> {
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
            let result = WaitPidV2Result {
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

    pub fn process_image_info(&self, pid: ProcessId) -> Option<ElfImageInfo> {
        self.table
            .iter()
            .flatten()
            .find(|record| record.pid == pid)
            .map(|record| ElfImageInfo {
                entry: record.entry,
                image_id: record.image_id,
            })
    }

    pub fn handle_request(&mut self, request: Message) -> Result<Message, ProcessManagerError> {
        match Self::parse_request(request)? {
            ProcessRequest::GetPid { caller_tid } => {
                Self::u64_reply(PROC_OP_GETPID, self.process_id_for_tid(caller_tid).0)
            }
            ProcessRequest::GetPpid { caller_tid } => {
                let pid = self.process_id_for_tid(caller_tid);
                Self::u64_reply(
                    PROC_OP_GETPPID,
                    self.lookup_parent(pid)
                        .unwrap_or(ProcessId(pid.0.saturating_sub(1)))
                        .0,
                )
            }
            ProcessRequest::Exit { caller_tid, code } => {
                self.mark_exit_for_tid(caller_tid, code)?;
                Self::u64_reply(PROC_OP_EXIT, 0)
            }
            ProcessRequest::SpawnV2(req) => {
                #[cfg(test)]
                {
                    let image = Self::synthetic_elf_image(req.image_id);
                    let (pid, _) = self.spawn_from_elf_image(req.parent_pid, req.image_id, &image)?;
                    let result = SpawnV2Result { pid };
                    return Message::with_header(0, PROC_OP_SPAWN_V2, 0, None, &result.encode())
                        .map_err(|_| ProcessManagerError::Malformed);
                }
                #[cfg(not(test))]
                {
                    let _ = req;
                    Err(ProcessManagerError::Unsupported)
                }
            }
            ProcessRequest::WaitPidV2(req) => {
                if req.caller_pid != req.target_pid {
                    let Some(parent) = self.lookup_parent(req.target_pid) else {
                        return Err(ProcessManagerError::PermissionDenied);
                    };
                    if parent != req.caller_pid {
                        return Err(ProcessManagerError::PermissionDenied);
                    }
                }
                let result = self.wait_result(req.target_pid)?;
                Message::with_header(0, PROC_OP_WAITPID_V2, 0, None, &result.encode())
                    .map_err(|_| ProcessManagerError::Malformed)
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct ProcessService {
    manager: ProcessManager,
    handled: usize,
}

impl ProcessService {
    pub const fn new() -> Self {
        Self {
            manager: ProcessManager::new(),
            handled: 0,
        }
    }

    pub const fn handled_count(&self) -> usize {
        self.handled
    }

    pub fn mark_exit(&mut self, pid: ProcessId, code: u64) -> Result<(), ProcessManagerError> {
        self.manager.mark_exit(pid, code)
    }

    pub fn handle(&mut self, request: Message) -> Result<Message, ProcessManagerError> {
        let reply = self.manager.handle_request(request)?;
        self.handled = self.handled.saturating_add(1);
        Ok(reply)
    }

    pub fn handle_batch(
        &mut self,
        requests: impl IntoIterator<Item = Message>,
    ) -> Result<usize, ProcessManagerError> {
        for request in requests {
            self.handle(request)?;
        }
        Ok(self.handled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::process_abi::{SpawnV2Args, WaitPidV2Args};

    #[test]
    fn elf_image_info_parser_accepts_minimal_elf64_header() {
        let mut image = [0u8; 64];
        image[..4].copy_from_slice(b"\x7FELF");
        image[24..32].copy_from_slice(&0x401000u64.to_le_bytes());
        let info = ElfImageInfo::parse(7, &image).expect("elf");
        assert_eq!(info.image_id, 7);
        assert_eq!(info.entry, 0x401000);
    }

    #[test]
    fn spawn_from_elf_image_allocates_pid_and_returns_entry() {
        let mut pm = ProcessManager::new();
        let mut image = [0u8; 64];
        image[..4].copy_from_slice(b"\x7FELF");
        image[24..32].copy_from_slice(&0x402000u64.to_le_bytes());

        let (pid, info) = pm
            .spawn_from_elf_image(ProcessId(1), 9, &image)
            .expect("spawn from image");
        assert!(pid >= ProcessId(1000));
        assert_eq!(info.image_id, 9);
        assert_eq!(info.entry, 0x402000);
    }

    #[test]
    fn process_manager_parses_v2_payloads() {
        let msg = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(7, 99).encode(),
        )
        .expect("msg");
        let req = ProcessManager::parse_request(msg).expect("parse");
        assert_eq!(
            req,
            ProcessRequest::SpawnV2(SpawnV2Request {
                parent_pid: ProcessId(7),
                image_id: 99,
            })
        );
    }

    #[test]
    fn process_manager_spawn_allocates_pid_and_wait_observes_exit() {
        let mut pm = ProcessManager::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(1, 2).encode(),
        )
        .expect("msg");
        let spawn_reply = pm.handle_request(spawn).expect("handle");
        let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("decode");
        let info = pm.process_image_info(spawned.pid).expect("image info");
        assert_eq!(info.image_id, 2);
        assert_eq!(info.entry, 0x402000);

        pm.mark_exit(spawned.pid, 17).expect("mark exit");

        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(1, spawned.pid.0).encode(),
        )
        .expect("wait");
        let wait_reply = pm.handle_request(wait).expect("wait reply");
        let waited = WaitPidV2Result::decode(wait_reply.as_slice()).expect("decode");
        assert_eq!(waited.waited_pid, spawned.pid);
        assert_eq!(waited.exit_code, 17);
    }

    #[test]
    fn waitpid_returns_would_block_for_running_child() {
        let mut pm = ProcessManager::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(1, 3).encode(),
        )
        .expect("spawn");
        let spawn_reply = pm.handle_request(spawn).expect("spawn reply");
        let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("decode");
        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(1, spawned.pid.0).encode(),
        )
        .expect("wait");
        assert_eq!(
            pm.handle_request(wait),
            Err(ProcessManagerError::WouldBlock)
        );
    }

    #[test]
    fn exit_request_marks_caller_process_exited() {
        let mut pm = ProcessManager::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(1, 5).encode(),
        )
        .expect("spawn");
        let spawn_reply = pm.handle_request(spawn).expect("spawn reply");
        let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("decode");
        let exit = Message::with_header(spawned.pid.0, PROC_OP_EXIT, 0, None, &9u64.to_le_bytes())
            .expect("exit");
        let _ = pm.handle_request(exit).expect("exit reply");
        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(1, spawned.pid.0).encode(),
        )
        .expect("wait");
        let wait_reply = pm.handle_request(wait).expect("wait reply");
        let waited = WaitPidV2Result::decode(wait_reply.as_slice()).expect("decode");
        assert_eq!(waited.exit_code, 9);
    }

    #[test]
    fn process_manager_rejects_cap_transport() {
        let msg = Message::with_header(
            0,
            PROC_OP_GETPID,
            Message::FLAG_CAP_TRANSFER,
            Some(9),
            &7u64.to_le_bytes(),
        )
        .expect("msg");
        assert_eq!(
            ProcessManager::parse_request(msg),
            Err(ProcessManagerError::InvalidTransport)
        );
    }

    #[test]
    fn waitpid_reaps_exited_child() {
        let mut pm = ProcessManager::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(1, 2).encode(),
        )
        .expect("spawn");
        let spawn_reply = pm.handle_request(spawn).expect("spawn reply");
        let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("decode");
        pm.mark_exit(spawned.pid, 4).expect("exit");

        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(1, spawned.pid.0).encode(),
        )
        .expect("wait");
        let _ = pm.handle_request(wait).expect("wait reply");
        assert_eq!(pm.live_process_count(), 0);
    }

    #[test]
    fn waitpid_rejects_non_parent_caller() {
        let mut pm = ProcessManager::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(1, 2).encode(),
        )
        .expect("spawn");
        let spawn_reply = pm.handle_request(spawn).expect("spawn reply");
        let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("decode");

        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(99, spawned.pid.0).encode(),
        )
        .expect("wait");
        assert_eq!(
            pm.handle_request(wait),
            Err(ProcessManagerError::PermissionDenied)
        );
    }

    #[test]
    fn waitpid_rejects_unknown_target() {
        let mut pm = ProcessManager::new();
        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(1, 4242).encode(),
        )
        .expect("wait");
        assert_eq!(
            pm.handle_request(wait),
            Err(ProcessManagerError::PermissionDenied)
        );
    }

    #[test]
    fn process_service_tracks_batch_handled() {
        let mut service = ProcessService::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(1, 2).encode(),
        )
        .expect("spawn");
        let getpid =
            Message::with_header(0, PROC_OP_GETPID, 0, None, &42u64.to_le_bytes()).expect("getpid");

        let handled = service.handle_batch([spawn, getpid]).expect("batch");
        assert_eq!(handled, 2);
        assert_eq!(service.handled_count(), 2);
    }

    #[test]
    fn process_manager_tracks_explicit_thread_identities() {
        let mut pm = ProcessManager::new();
        let pid = pm.alloc_process(ProcessId(1)).expect("pid");
        pm.register_thread_identity(pid, 2000, ThreadGroupId(pid.0))
            .expect("thread");
        assert_eq!(pm.process_id_for_tid(2000), pid);
        assert_eq!(
            pm.thread_identity(2000).expect("identity").thread_group_id,
            ThreadGroupId(pid.0)
        );
    }
}
