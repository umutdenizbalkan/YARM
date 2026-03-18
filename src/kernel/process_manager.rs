use super::ipc::Message;
use super::proc_proto::{
    ProcV2Args, PROC_OP_EXIT, PROC_OP_GETPID, PROC_OP_GETPPID, PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2,
};

const MAX_PROCESSES: usize = 64;
const MAX_THREADS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessManagerError {
    Malformed,
    Unsupported,
    TableFull,
    UnknownProcess,
    InvalidTransport,
    PermissionDenied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV2Request {
    pub parent_pid: u64,
    pub image_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitPidV2Request {
    pub caller_pid: u64,
    pub target_pid: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV2Result {
    pub pid: u64,
}

impl SpawnV2Result {
    pub const fn encode(self) -> [u8; 8] {
        self.pid.to_le_bytes()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcessManagerError> {
        if payload.len() < 8 {
            return Err(ProcessManagerError::Malformed);
        }
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&payload[..8]);
        Ok(Self {
            pid: u64::from_le_bytes(bytes),
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
        if &image[..4] != b"ELF" {
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
    pub waited_pid: u64,
    pub exit_code: u64,
}

impl WaitPidV2Result {
    pub const fn encode(self) -> [u8; 16] {
        ProcV2Args::new(self.waited_pid, self.exit_code).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcessManagerError> {
        let args = ProcV2Args::decode(payload).map_err(|_| ProcessManagerError::Malformed)?;
        Ok(Self {
            waited_pid: args.arg0,
            exit_code: args.arg1,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessRequest {
    GetPid { caller_tid: u64 },
    GetPpid { caller_tid: u64 },
    Exit { code: u64 },
    SpawnV2(SpawnV2Request),
    WaitPidV2(WaitPidV2Request),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessRecord {
    pid: u64,
    parent_pid: u64,
    exited: bool,
    exit_code: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ThreadIdentityRecord {
    tid: u64,
    pid: u64,
    thread_group_id: u64,
}

#[derive(Debug)]
pub struct ProcessManagerLite {
    next_pid: u64,
    table: [Option<ProcessRecord>; MAX_PROCESSES],
    threads: [Option<ThreadIdentityRecord>; MAX_THREADS],
}

impl Default for ProcessManagerLite {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessManagerLite {
    pub const fn new() -> Self {
        Self {
            next_pid: 1000,
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
                code: Self::read_u64(msg.as_slice())?,
            }),
            PROC_OP_SPAWN_V2 => {
                let args = ProcV2Args::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                Ok(ProcessRequest::SpawnV2(SpawnV2Request {
                    parent_pid: args.arg0,
                    image_id: args.arg1,
                }))
            }
            PROC_OP_WAITPID_V2 => {
                let args = ProcV2Args::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                Ok(ProcessRequest::WaitPidV2(WaitPidV2Request {
                    caller_pid: args.arg0,
                    target_pid: args.arg1,
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
        pid: u64,
        tid: u64,
        thread_group_id: u64,
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

    fn process_id_for_tid(&self, caller_tid: u64) -> u64 {
        self.thread_identity(caller_tid)
            .map(|record| record.pid)
            .unwrap_or(caller_tid)
    }

    pub fn spawn_from_elf_image(
        &mut self,
        parent_pid: u64,
        image_id: u64,
        image: &[u8],
    ) -> Result<(u64, ElfImageInfo), ProcessManagerError> {
        let info = ElfImageInfo::parse(image_id, image)?;
        let pid = self.alloc_process(parent_pid)?;
        Ok((pid, info))
    }

    fn alloc_process(&mut self, parent_pid: u64) -> Result<u64, ProcessManagerError> {
        let pid = self.next_pid;
        self.next_pid = self.next_pid.saturating_add(1);
        if let Some(slot) = self.table.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(ProcessRecord {
                pid,
                parent_pid,
                exited: false,
                exit_code: 0,
            });
            self.register_thread_identity(pid, pid, pid)?;
            Ok(pid)
        } else {
            Err(ProcessManagerError::TableFull)
        }
    }

    pub fn mark_exit(&mut self, pid: u64, code: u64) -> Result<(), ProcessManagerError> {
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

    fn lookup_parent(&self, pid: u64) -> Option<u64> {
        self.table
            .iter()
            .flatten()
            .find(|record| record.pid == pid)
            .map(|record| record.parent_pid)
    }

    fn wait_result(&mut self, target_pid: u64) -> WaitPidV2Result {
        if let Some((idx, record)) = self
            .table
            .iter()
            .enumerate()
            .find_map(|(idx, slot)| slot.map(|record| (idx, record)))
            .filter(|(_, record)| record.pid == target_pid)
        {
            let result = WaitPidV2Result {
                waited_pid: target_pid,
                exit_code: if record.exited {
                    record.exit_code
                } else {
                    u64::MAX
                },
            };
            if record.exited {
                self.table[idx] = None;
            }
            result
        } else {
            WaitPidV2Result {
                waited_pid: target_pid,
                exit_code: u64::MAX,
            }
        }
    }

    pub fn live_process_count(&self) -> usize {
        self.table.iter().flatten().count()
    }

    pub fn handle_request(&mut self, request: Message) -> Result<Message, ProcessManagerError> {
        match Self::parse_request(request)? {
            ProcessRequest::GetPid { caller_tid } => {
                Self::u64_reply(PROC_OP_GETPID, self.process_id_for_tid(caller_tid))
            }
            ProcessRequest::GetPpid { caller_tid } => {
                let pid = self.process_id_for_tid(caller_tid);
                Self::u64_reply(
                    PROC_OP_GETPPID,
                    self.lookup_parent(pid).unwrap_or(pid.saturating_sub(1)),
                )
            }
            ProcessRequest::Exit { .. } => Self::u64_reply(PROC_OP_EXIT, 0),
            ProcessRequest::SpawnV2(req) => {
                let _ = req.image_id;
                let pid = self.alloc_process(req.parent_pid)?;
                let result = SpawnV2Result { pid };
                Message::with_header(0, PROC_OP_SPAWN_V2, 0, None, &result.encode())
                    .map_err(|_| ProcessManagerError::Malformed)
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
                let result = self.wait_result(req.target_pid);
                Message::with_header(0, PROC_OP_WAITPID_V2, 0, None, &result.encode())
                    .map_err(|_| ProcessManagerError::Malformed)
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct ProcessService {
    manager: ProcessManagerLite,
    handled: usize,
}

impl ProcessService {
    pub const fn new() -> Self {
        Self {
            manager: ProcessManagerLite::new(),
            handled: 0,
        }
    }

    pub const fn handled_count(&self) -> usize {
        self.handled
    }

    pub fn mark_exit(&mut self, pid: u64, code: u64) -> Result<(), ProcessManagerError> {
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

    #[test]
    fn elf_image_info_parser_accepts_minimal_elf64_header() {
        let mut image = [0u8; 64];
        image[..4].copy_from_slice(b"ELF");
        image[24..32].copy_from_slice(&0x401000u64.to_le_bytes());
        let info = ElfImageInfo::parse(7, &image).expect("elf");
        assert_eq!(info.image_id, 7);
        assert_eq!(info.entry, 0x401000);
    }

    #[test]
    fn spawn_from_elf_image_allocates_pid_and_returns_entry() {
        let mut pm = ProcessManagerLite::new();
        let mut image = [0u8; 64];
        image[..4].copy_from_slice(b"ELF");
        image[24..32].copy_from_slice(&0x402000u64.to_le_bytes());

        let (pid, info) = pm
            .spawn_from_elf_image(1, 9, &image)
            .expect("spawn from image");
        assert!(pid >= 1000);
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
            &ProcV2Args::new(7, 99).encode(),
        )
        .expect("msg");
        let req = ProcessManagerLite::parse_request(msg).expect("parse");
        assert_eq!(
            req,
            ProcessRequest::SpawnV2(SpawnV2Request {
                parent_pid: 7,
                image_id: 99,
            })
        );
    }

    #[test]
    fn process_manager_spawn_allocates_pid_and_wait_observes_exit() {
        let mut pm = ProcessManagerLite::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &ProcV2Args::new(1, 2).encode(),
        )
        .expect("msg");
        let spawn_reply = pm.handle_request(spawn).expect("handle");
        let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("decode");

        pm.mark_exit(spawned.pid, 17).expect("mark exit");

        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &ProcV2Args::new(1, spawned.pid).encode(),
        )
        .expect("wait");
        let wait_reply = pm.handle_request(wait).expect("wait reply");
        let waited = WaitPidV2Result::decode(wait_reply.as_slice()).expect("decode");
        assert_eq!(waited.waited_pid, spawned.pid);
        assert_eq!(waited.exit_code, 17);
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
            ProcessManagerLite::parse_request(msg),
            Err(ProcessManagerError::InvalidTransport)
        );
    }

    #[test]
    fn waitpid_reaps_exited_child() {
        let mut pm = ProcessManagerLite::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &ProcV2Args::new(1, 2).encode(),
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
            &ProcV2Args::new(1, spawned.pid).encode(),
        )
        .expect("wait");
        let _ = pm.handle_request(wait).expect("wait reply");
        assert_eq!(pm.live_process_count(), 0);
    }

    #[test]
    fn waitpid_rejects_non_parent_caller() {
        let mut pm = ProcessManagerLite::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &ProcV2Args::new(1, 2).encode(),
        )
        .expect("spawn");
        let spawn_reply = pm.handle_request(spawn).expect("spawn reply");
        let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("decode");

        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &ProcV2Args::new(99, spawned.pid).encode(),
        )
        .expect("wait");
        assert_eq!(
            pm.handle_request(wait),
            Err(ProcessManagerError::PermissionDenied)
        );
    }

    #[test]
    fn waitpid_rejects_unknown_target() {
        let mut pm = ProcessManagerLite::new();
        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &ProcV2Args::new(1, 4242).encode(),
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
            &ProcV2Args::new(1, 2).encode(),
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
        let mut pm = ProcessManagerLite::new();
        let pid = pm.alloc_process(1).expect("pid");
        pm.register_thread_identity(pid, 2000, pid).expect("thread");
        assert_eq!(pm.process_id_for_tid(2000), pid);
        assert_eq!(
            pm.thread_identity(2000).expect("identity").thread_group_id,
            pid
        );
    }
}
