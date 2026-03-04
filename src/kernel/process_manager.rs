use super::ipc::Message;
use super::linux_compat::{
    PROC_OP_EXIT, PROC_OP_GETPID, PROC_OP_GETPPID, PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2, ProcV2Args,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessManagerError {
    Malformed,
    Unsupported,
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
pub enum ProcessRequest {
    GetPid { tid: u64 },
    GetPpid { tid: u64 },
    Exit { code: u64 },
    SpawnV2(SpawnV2Request),
    WaitPidV2(WaitPidV2Request),
}

#[derive(Debug)]
pub struct ProcessManagerLite {
    next_pid: u64,
}

impl Default for ProcessManagerLite {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessManagerLite {
    pub const fn new() -> Self {
        Self { next_pid: 1000 }
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

    pub fn handle_request(&mut self, request: Message) -> Result<Message, ProcessManagerError> {
        match Self::parse_request(request)? {
            ProcessRequest::GetPid { tid } => Self::u64_reply(PROC_OP_GETPID, tid),
            ProcessRequest::GetPpid { tid } => {
                Self::u64_reply(PROC_OP_GETPPID, tid.saturating_sub(1))
            }
            ProcessRequest::Exit { .. } => Self::u64_reply(PROC_OP_EXIT, 0),
            ProcessRequest::SpawnV2(_) => {
                let pid = self.next_pid;
                self.next_pid = self.next_pid.saturating_add(1);
                Self::u64_reply(PROC_OP_SPAWN_V2, pid)
            }
            ProcessRequest::WaitPidV2(req) => Self::u64_reply(PROC_OP_WAITPID_V2, req.target_tid),
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
    fn process_manager_spawn_allocates_pid() {
        let mut pm = ProcessManagerLite::new();
        let msg = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &ProcV2Args::new(1, 2).encode(),
        )
        .expect("msg");
        let rep = pm.handle_request(msg).expect("handle");
        assert_eq!(rep.opcode, PROC_OP_SPAWN_V2);
    }
}
