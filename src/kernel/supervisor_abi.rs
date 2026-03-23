use super::capabilities::CapId;
use super::ipc::Message;

pub const SUPERVISOR_OP_TASK_EXITED: u16 = 0xEE;
pub const SUPERVISOR_OP_INIT_ALERT: u16 = 0xEF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskExitedEvent {
    pub tid: u64,
    pub exit_code: u64,
    pub restart_token: u64,
}

impl TaskExitedEvent {
    pub const ENCODED_LEN: usize = 24;

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        let tid = self.tid.to_le_bytes();
        let code = self.exit_code.to_le_bytes();
        let token = self.restart_token.to_le_bytes();
        let mut i = 0;
        while i < 8 {
            out[i] = tid[i];
            out[8 + i] = code[i];
            out[16 + i] = token[i];
            i += 1;
        }
        out
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < Self::ENCODED_LEN {
            return None;
        }
        let mut tid = [0u8; 8];
        let mut code = [0u8; 8];
        let mut token = [0u8; 8];
        tid.copy_from_slice(&payload[..8]);
        code.copy_from_slice(&payload[8..16]);
        token.copy_from_slice(&payload[16..24]);
        Some(Self {
            tid: u64::from_le_bytes(tid),
            exit_code: u64::from_le_bytes(code),
            restart_token: u64::from_le_bytes(token),
        })
    }
}

pub fn task_exited_message(sender_tid: u64, event: TaskExitedEvent) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_TASK_EXITED,
        0,
        None,
        &event.encode(),
    )
    .map_err(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitAlertKind {
    RedelegationRequired = 1,
    ServiceDegraded = 2,
    SupervisorRestarted = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitAlert {
    pub tid: u64,
    pub kind: InitAlertKind,
}

impl InitAlert {
    pub const ENCODED_LEN: usize = 16;

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        let tid = self.tid.to_le_bytes();
        let kind = (self.kind as u64).to_le_bytes();
        let mut i = 0;
        while i < 8 {
            out[i] = tid[i];
            out[8 + i] = kind[i];
            i += 1;
        }
        out
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < Self::ENCODED_LEN {
            return None;
        }
        let mut tid = [0u8; 8];
        let mut kind = [0u8; 8];
        tid.copy_from_slice(&payload[..8]);
        kind.copy_from_slice(&payload[8..16]);
        let kind = match u64::from_le_bytes(kind) {
            1 => InitAlertKind::RedelegationRequired,
            2 => InitAlertKind::ServiceDegraded,
            3 => InitAlertKind::SupervisorRestarted,
            _ => return None,
        };
        Some(Self {
            tid: u64::from_le_bytes(tid),
            kind,
        })
    }
}

pub fn init_alert_message(
    sender_tid: u64,
    send_cap: Option<CapId>,
    alert: InitAlert,
) -> Result<Message, ()> {
    let (flags, cap) = if let Some(cap) = send_cap {
        (Message::FLAG_CAP_TRANSFER, Some(cap.0))
    } else {
        (0, None)
    };
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_INIT_ALERT,
        flags,
        cap,
        &alert.encode(),
    )
    .map_err(|_| ())
}
