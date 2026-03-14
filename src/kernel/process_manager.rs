use super::ipc::Message;
use super::linux_compat::{
    PROC_OP_EXIT, PROC_OP_GETPID, PROC_OP_GETPPID, PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2, ProcV2Args,
};

const MAX_PROCESSES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessManagerError {
    Malformed,
    Unsupported,
    TableFull,
    UnknownProcess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV2Request {
    pub parent_tid: u64,
    pub image_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitPidV2Request {
    pub caller_tid: u64,
    pub target_tid: u64,
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
    GetPid { tid: u64 },
    GetPpid { tid: u64 },
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

#[derive(Debug)]
pub struct ProcessManagerLite {
    next_pid: u64,
    table: [Option<ProcessRecord>; MAX_PROCESSES],
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
        match msg.opcode {
            PROC_OP_GETPID => Ok(ProcessRequest::GetPid {
                tid: Self::read_u64(msg.as_slice())?,
            }),
            PROC_OP_GETPPID => Ok(ProcessRequest::GetPpid {
                tid: Self::read_u64(msg.as_slice())?,
            }),
            PROC_OP_EXIT => Ok(ProcessRequest::Exit {
                code: Self::read_u64(msg.as_slice())?,
            }),
            PROC_OP_SPAWN_V2 => {
                let args = ProcV2Args::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                Ok(ProcessRequest::SpawnV2(SpawnV2Request {
                    parent_tid: args.arg0,
                    image_id: args.arg1,
                }))
            }
            PROC_OP_WAITPID_V2 => {
                let args = ProcV2Args::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                Ok(ProcessRequest::WaitPidV2(WaitPidV2Request {
                    caller_tid: args.arg0,
                    target_tid: args.arg1,
                }))
            }
            _ => Err(ProcessManagerError::Unsupported),
        }
    }

    fn u64_reply(opcode: u16, value: u64) -> Result<Message, ProcessManagerError> {
        Message::with_header(0, opcode, 0, None, &value.to_le_bytes())
            .map_err(|_| ProcessManagerError::Malformed)
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

    fn lookup_parent(&self, pid: u64) -> u64 {
        self.table
            .iter()
            .flatten()
            .find(|record| record.pid == pid)
            .map(|record| record.parent_pid)
            .unwrap_or(pid.saturating_sub(1))
    }

    fn wait_result(&self, target_pid: u64) -> WaitPidV2Result {
        if let Some(record) = self
            .table
            .iter()
            .flatten()
            .find(|record| record.pid == target_pid)
        {
            WaitPidV2Result {
                waited_pid: target_pid,
                exit_code: if record.exited {
                    record.exit_code
                } else {
                    u64::MAX
                },
            }
        } else {
            WaitPidV2Result {
                waited_pid: target_pid,
                exit_code: u64::MAX,
            }
        }
    }

    pub fn handle_request(&mut self, request: Message) -> Result<Message, ProcessManagerError> {
        match Self::parse_request(request)? {
            ProcessRequest::GetPid { tid } => Self::u64_reply(PROC_OP_GETPID, tid),
            ProcessRequest::GetPpid { tid } => {
                Self::u64_reply(PROC_OP_GETPPID, self.lookup_parent(tid))
            }
            ProcessRequest::Exit { .. } => Self::u64_reply(PROC_OP_EXIT, 0),
            ProcessRequest::SpawnV2(req) => {
                let _ = req.image_id;
                let pid = self.alloc_process(req.parent_tid)?;
                let result = SpawnV2Result { pid };
                Message::with_header(0, PROC_OP_SPAWN_V2, 0, None, &result.encode())
                    .map_err(|_| ProcessManagerError::Malformed)
            }
            ProcessRequest::WaitPidV2(req) => {
                let _ = req.caller_tid;
                let result = self.wait_result(req.target_tid);
                Message::with_header(0, PROC_OP_WAITPID_V2, 0, None, &result.encode())
                    .map_err(|_| ProcessManagerError::Malformed)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                parent_tid: 7,
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
}
