use super::ipc::Message;

pub const SUPERVISOR_OP_TASK_EXITED: u16 = 0xEE;
pub const SUPERVISOR_OP_INIT_ALERT: u16 = 0xEF;
pub const SUPERVISOR_OP_REGISTER_CORE_SERVICE: u16 = 0x40;
pub const SUPERVISOR_OP_REGISTER_DRIVER: u16 = 0x41;
pub const SUPERVISOR_OP_QUERY_STATUS: u16 = 0x42;
pub const SUPERVISOR_OP_ACK_REDELEGATION: u16 = 0x43;

pub const DEP_PROCESS_MANAGER: u8 = 1 << 0;
pub const DEP_VFS: u8 = 1 << 1;
pub const DEP_SUPERVISOR: u8 = 1 << 2;

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
    CoreServiceRestartRequired = 4,
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
            4 => InitAlertKind::CoreServiceRestartRequired,
            _ => return None,
        };
        Some(Self {
            tid: u64::from_le_bytes(tid),
            kind,
        })
    }
}

pub fn init_alert_message(sender_tid: u64, alert: InitAlert) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_INIT_ALERT,
        0,
        None,
        &alert.encode(),
    )
    .map_err(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreServiceRegistrationKind {
    ProcessManager = 1,
    Vfs = 2,
    Supervisor = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterCoreServiceRequest {
    pub tid: u64,
    pub kind: CoreServiceRegistrationKind,
    pub max_restarts: u8,
    pub restart_group: u8,
    pub dependency_mask: u8,
    pub backoff_ticks: u64,
}

impl RegisterCoreServiceRequest {
    pub const ENCODED_LEN: usize = 24;

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        let tid = self.tid.to_le_bytes();
        let backoff = self.backoff_ticks.to_le_bytes();
        let mut i = 0;
        while i < 8 {
            out[i] = tid[i];
            out[16 + i] = backoff[i];
            i += 1;
        }
        out[8] = self.kind as u8;
        out[9] = self.max_restarts;
        out[10] = self.restart_group;
        out[11] = self.dependency_mask;
        out
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < Self::ENCODED_LEN {
            return None;
        }
        let mut tid = [0u8; 8];
        let mut backoff = [0u8; 8];
        tid.copy_from_slice(&payload[..8]);
        backoff.copy_from_slice(&payload[16..24]);
        let kind = match payload[8] {
            1 => CoreServiceRegistrationKind::ProcessManager,
            2 => CoreServiceRegistrationKind::Vfs,
            3 => CoreServiceRegistrationKind::Supervisor,
            _ => return None,
        };
        Some(Self {
            tid: u64::from_le_bytes(tid),
            kind,
            max_restarts: payload[9],
            restart_group: payload[10],
            dependency_mask: payload[11],
            backoff_ticks: u64::from_le_bytes(backoff),
        })
    }
}

pub fn register_core_service_message(
    sender_tid: u64,
    request: RegisterCoreServiceRequest,
) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_REGISTER_CORE_SERVICE,
        0,
        None,
        &request.encode(),
    )
    .map_err(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterDriverRequest {
    pub tid: u64,
    pub max_restarts: u8,
    pub restart_group: u8,
    pub dependency_mask: u8,
    pub backoff_ticks: u64,
    pub irq_line: u16,
    pub mem_cap: u64,
    pub iova_cap: u64,
    pub iova_base: u64,
    pub dma_len: u64,
    pub iova_len: u64,
}

impl RegisterDriverRequest {
    pub const ENCODED_LEN: usize = 64;

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        let tid = self.tid.to_le_bytes();
        let backoff = self.backoff_ticks.to_le_bytes();
        let irq = self.irq_line.to_le_bytes();
        let mem_cap = self.mem_cap.to_le_bytes();
        let iova_cap = self.iova_cap.to_le_bytes();
        let iova_base = self.iova_base.to_le_bytes();
        let dma_len = self.dma_len.to_le_bytes();
        let iova_len = self.iova_len.to_le_bytes();
        let mut i = 0;
        while i < 8 {
            out[i] = tid[i];
            out[16 + i] = backoff[i];
            out[24 + i] = mem_cap[i];
            out[32 + i] = iova_cap[i];
            out[40 + i] = iova_base[i];
            out[48 + i] = dma_len[i];
            out[56 + i] = iova_len[i];
            i += 1;
        }
        out[8] = self.max_restarts;
        out[9] = self.restart_group;
        out[10] = irq[0];
        out[11] = irq[1];
        out[12] = self.dependency_mask;
        out
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < 56 {
            return None;
        }
        let mut tid = [0u8; 8];
        let mut backoff = [0u8; 8];
        let mut mem_cap = [0u8; 8];
        let mut iova_cap = [0u8; 8];
        let mut iova_base = [0u8; 8];
        let mut dma_len = [0u8; 8];
        let mut iova_len = [0u8; 8];
        tid.copy_from_slice(&payload[..8]);
        backoff.copy_from_slice(&payload[16..24]);
        mem_cap.copy_from_slice(&payload[24..32]);
        iova_cap.copy_from_slice(&payload[32..40]);
        iova_base.copy_from_slice(&payload[40..48]);
        if payload.len() >= Self::ENCODED_LEN {
            dma_len.copy_from_slice(&payload[48..56]);
            iova_len.copy_from_slice(&payload[56..64]);
        } else {
            // Backward-compat decode for pre-split format.
            dma_len.copy_from_slice(&payload[48..56]);
            iova_len.copy_from_slice(&payload[48..56]);
        }
        Some(Self {
            tid: u64::from_le_bytes(tid),
            max_restarts: payload[8],
            restart_group: payload[9],
            dependency_mask: payload[12],
            backoff_ticks: u64::from_le_bytes(backoff),
            irq_line: u16::from_le_bytes([payload[10], payload[11]]),
            mem_cap: u64::from_le_bytes(mem_cap),
            iova_cap: u64::from_le_bytes(iova_cap),
            iova_base: u64::from_le_bytes(iova_base),
            dma_len: u64::from_le_bytes(dma_len),
            iova_len: u64::from_le_bytes(iova_len),
        })
    }
}

pub fn register_driver_message(
    sender_tid: u64,
    request: RegisterDriverRequest,
) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_REGISTER_DRIVER,
        0,
        None,
        &request.encode(),
    )
    .map_err(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorStatusRequest {
    pub tid: u64,
}

impl SupervisorStatusRequest {
    pub const ENCODED_LEN: usize = 8;

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        self.tid.to_le_bytes()
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < Self::ENCODED_LEN {
            return None;
        }
        let mut tid = [0u8; 8];
        tid.copy_from_slice(&payload[..8]);
        Some(Self {
            tid: u64::from_le_bytes(tid),
        })
    }
}

pub fn query_status_message(
    sender_tid: u64,
    request: SupervisorStatusRequest,
) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_QUERY_STATUS,
        0,
        None,
        &request.encode(),
    )
    .map_err(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedelegationAckRequest {
    pub tid: u64,
}

impl RedelegationAckRequest {
    pub const ENCODED_LEN: usize = 8;

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        self.tid.to_le_bytes()
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        SupervisorStatusRequest::decode(payload).map(|req| Self { tid: req.tid })
    }
}

pub fn redelegation_ack_message(
    sender_tid: u64,
    request: RedelegationAckRequest,
) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_ACK_REDELEGATION,
        0,
        None,
        &request.encode(),
    )
    .map_err(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorStatusReply {
    pub tid: u64,
    pub degraded: bool,
    pub pending_redelegation: bool,
    pub restart_attempts: u8,
    pub restart_group: u8,
    pub max_restarts: u8,
    pub restart_owner: u8,
    pub last_exit_code: u64,
    pub last_exit_tick: u64,
    pub pending_restart_due: u64,
    pub last_restart_tick: u64,
}

impl SupervisorStatusReply {
    pub const ENCODED_LEN: usize = 48;

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        let tid = self.tid.to_le_bytes();
        let last_exit_code = self.last_exit_code.to_le_bytes();
        let last_exit_tick = self.last_exit_tick.to_le_bytes();
        let pending_restart_due = self.pending_restart_due.to_le_bytes();
        let last_restart_tick = self.last_restart_tick.to_le_bytes();
        let mut i = 0;
        while i < 8 {
            out[i] = tid[i];
            out[16 + i] = last_exit_code[i];
            out[24 + i] = last_exit_tick[i];
            out[32 + i] = pending_restart_due[i];
            out[40 + i] = last_restart_tick[i];
            i += 1;
        }
        out[8] = self.degraded as u8;
        out[9] = self.pending_redelegation as u8;
        out[10] = self.restart_attempts;
        out[11] = self.restart_group;
        out[12] = self.max_restarts;
        out[13] = self.restart_owner;
        out
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < Self::ENCODED_LEN {
            return None;
        }
        let mut tid = [0u8; 8];
        let mut last_exit_code = [0u8; 8];
        let mut last_exit_tick = [0u8; 8];
        let mut pending_restart_due = [0u8; 8];
        let mut last_restart_tick = [0u8; 8];
        tid.copy_from_slice(&payload[..8]);
        last_exit_code.copy_from_slice(&payload[16..24]);
        last_exit_tick.copy_from_slice(&payload[24..32]);
        pending_restart_due.copy_from_slice(&payload[32..40]);
        last_restart_tick.copy_from_slice(&payload[40..48]);
        Some(Self {
            tid: u64::from_le_bytes(tid),
            degraded: payload[8] != 0,
            pending_redelegation: payload[9] != 0,
            restart_attempts: payload[10],
            restart_group: payload[11],
            max_restarts: payload[12],
            restart_owner: payload[13],
            last_exit_code: u64::from_le_bytes(last_exit_code),
            last_exit_tick: u64::from_le_bytes(last_exit_tick),
            pending_restart_due: u64::from_le_bytes(pending_restart_due),
            last_restart_tick: u64::from_le_bytes(last_restart_tick),
        })
    }
}

pub fn status_reply_message(sender_tid: u64, reply: SupervisorStatusReply) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_QUERY_STATUS,
        0,
        None,
        &reply.encode(),
    )
    .map_err(|_| ())
}
