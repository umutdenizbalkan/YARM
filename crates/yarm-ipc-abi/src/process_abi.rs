// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcCodecError {
    Malformed,
}

pub const PROC_SERVER_ABI_VERSION: u16 = 1;
pub const PROC_CODEC_V2_VERSION: u16 = 2;
pub const PROC_CODEC_V3_VERSION: u16 = 3;
pub const PROC_CODEC_V4_VERSION: u16 = 4;

pub const PROC_OP_GETPID: u16 = 1;
pub const PROC_OP_EXIT: u16 = 2;
pub const PROC_OP_GETPPID: u16 = 3;
pub const PROC_OP_SPAWN_V2: u16 = 4;
pub const PROC_OP_WAITPID_V2: u16 = 5;
pub const PROC_OP_SPAWN_V3: u16 = 6;
pub const PROC_OP_SPAWN_V4: u16 = 7;
pub const PROC_OP_TASK_RESTART_TOKEN: u16 = 8;
pub const PROC_OP_REGISTER_SUPERVISED_TASK: u16 = 9;
pub const PROC_OP_EXECUTE_RESTART: u16 = 10;
pub const PROC_OP_SPAWN_V5_CAP: u16 = 11;
/// Query the PM lifecycle table for a given TID.
///
/// Request: [`LifecycleQueryRequest`] (8 bytes).
/// Reply:   [`LifecycleQueryReply`] (19 bytes).
/// PM looks up its `LifecycleTable` and replies found=1 with real metadata, or
/// found=0 if the TID is unknown.  `restart_supported` is always 0 until real
/// restart-token population is wired.
pub const PROC_OP_LIFECYCLE_QUERY: u16 = 12;

/// Stage 76: PM → VFS one-way push notification: a task has exited.
///
/// Payload: [`PmTaskExitedEvent`] (16 bytes, LE).
/// This is a push-only opcode — PM sends it, VFS receives and handles it.
/// No reply is expected or sent.
///
/// Production blocker: PM does not currently receive task-exit events from the
/// kernel, and no PM→VFS send cap exists in the startup handoff.
/// See `VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED` for the two-part blocker record.
pub const PROC_OP_TASK_EXITED: u16 = 13;

/// Stage 76: PM → VFS one-way push notification: a process (all threads) has exited.
///
/// Payload: [`PmProcessExitedEvent`] (16 bytes, LE).
/// Push-only opcode: PM sends, VFS handles.  No reply.
///
/// Semantics: process-level exit covers all threads belonging to the process
/// identified by `process_tid`.  VFS must clean up all lifecycles whose
/// `requester_tid` matches any thread in that process.  In the current helper model
/// (Stage 76) VFS only has per-tid granularity; process-level clean-up is a future
/// extension once a process→tid membership table is available.
///
/// Production blocker: same as [`PROC_OP_TASK_EXITED`].
pub const PROC_OP_PROCESS_EXITED: u16 = 14;

/// SUP-L1 allocated ABI reservation for supervisor → PM restart requests.
///
/// Dispatch is intentionally disabled in SUP-L1: PM must reject/defer this
/// mechanism until later validation/implementation stages (SUP-L2/SUP-L4).
pub const PROC_OP_PM_RESTART_V1: u16 = 15;

/// SUP-L1 allocated ABI reservation for PM → supervisor restart replies.
///
/// No live supervisor send/receive path is wired in SUP-L1. The mechanism is
/// unimplemented; the constants only reserve the reviewed ABI numbers.
pub const PROC_OP_PM_RESTART_REPLY_V1: u16 = 16;

/// Number of globally allocated process IPC opcodes.
///
/// Before SUP-L1 this count was 14. SUP-L1 intentionally raises it to 16 by
/// allocating PM restart request/reply opcodes 15/16 while leaving dispatch off.
pub const PROCESS_IPC_OPCODE_COUNT: usize = 16;

/// Stage 77+78: Kernel → PM one-way push: a tracked task has exited.
///
/// Opcode sent by the kernel on PM's `pm_task_exit_endpoint` when any task exits.
/// Payload: [`KernelPmTaskExitedPayload`] (16 bytes LE).
/// No reply. This is the kernel-push direction; `PROC_OP_TASK_EXITED` (13) is the
/// PM→VFS forwarding direction on a separate IPC endpoint.
pub const KERNEL_OP_PM_TASK_EXITED: u16 = 0xDC;

/// `state` value for a service that was spawned and is running.
pub const LIFECYCLE_STATE_SPAWNED: u8 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleQueryRequest {
    pub tid: u64,
}

impl LifecycleQueryRequest {
    pub const fn new(tid: u64) -> Self {
        Self { tid }
    }
    pub const fn encode(self) -> [u8; 8] {
        self.tid.to_le_bytes()
    }
    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() < 8 {
            return Err(ProcCodecError::Malformed);
        }
        let mut a = [0u8; 8];
        a.copy_from_slice(&payload[..8]);
        Ok(Self::new(u64::from_le_bytes(a)))
    }
}

/// Wire reply for [`PROC_OP_LIFECYCLE_QUERY`].
///
/// Wire layout (19 bytes LE):
/// ```text
/// [0]      found:             1 = record present, 0 = not in lifecycle table
/// [1..9]   tid:               u64 LE (0 when found=0)
/// [9..17]  image_id:          u64 LE (0 when found=0)
/// [17]     state:             LIFECYCLE_STATE_* constant
/// [18]     restart_supported: always 0 (restart not yet wired)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleQueryReply {
    pub found: u8,
    pub tid: u64,
    pub image_id: u64,
    pub state: u8,
    pub restart_supported: u8,
}

impl LifecycleQueryReply {
    pub const ENCODED_LEN: usize = 19;

    pub const fn not_found() -> Self {
        Self {
            found: 0,
            tid: 0,
            image_id: 0,
            state: 0,
            restart_supported: 0,
        }
    }

    pub const fn found(tid: u64, image_id: u64, state: u8) -> Self {
        Self {
            found: 1,
            tid,
            image_id,
            state,
            restart_supported: 0,
        }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[0] = self.found;
        out[1..9].copy_from_slice(&self.tid.to_le_bytes());
        out[9..17].copy_from_slice(&self.image_id.to_le_bytes());
        out[17] = self.state;
        out[18] = self.restart_supported;
        out
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() < Self::ENCODED_LEN {
            return Err(ProcCodecError::Malformed);
        }
        let mut a = [0u8; 8];
        a.copy_from_slice(&payload[1..9]);
        let tid = u64::from_le_bytes(a);
        a.copy_from_slice(&payload[9..17]);
        let image_id = u64::from_le_bytes(a);
        Ok(Self {
            found: payload[0],
            tid,
            image_id,
            state: payload[17],
            restart_supported: payload[18],
        })
    }

    pub const fn is_found(self) -> bool {
        self.found == 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecuteRestartRequest {
    pub tid: u64,
    pub restart_token: u64,
}

impl ExecuteRestartRequest {
    pub const fn new(tid: u64, restart_token: u64) -> Self {
        Self { tid, restart_token }
    }
    pub fn encode(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&self.tid.to_le_bytes());
        out[8..16].copy_from_slice(&self.restart_token.to_le_bytes());
        out
    }
    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() < 16 {
            return Err(ProcCodecError::Malformed);
        }
        let mut tid = [0u8; 8];
        let mut token = [0u8; 8];
        tid.copy_from_slice(&payload[..8]);
        token.copy_from_slice(&payload[8..16]);
        Ok(Self::new(
            u64::from_le_bytes(tid),
            u64::from_le_bytes(token),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecuteRestartReply {
    pub status: u8,
}

impl ExecuteRestartReply {
    pub const STATUS_OK: u8 = 0;
    pub const STATUS_NOT_FOUND: u8 = 1;
    pub const STATUS_TOKEN_MISMATCH: u8 = 2;
    pub const STATUS_PERMISSION_DENIED: u8 = 3;
    pub const STATUS_INTERNAL_UNSUPPORTED: u8 = 255;

    pub const fn new(status: u8) -> Self {
        Self { status }
    }
    pub const fn encode(self) -> [u8; 1] {
        [self.status]
    }
    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.is_empty() {
            return Err(ProcCodecError::Malformed);
        }
        Ok(Self::new(payload[0]))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterSupervisedTask {
    pub tid: u64,
    pub restart_token: u64,
}

impl RegisterSupervisedTask {
    pub const fn new(tid: u64, restart_token: u64) -> Self {
        Self { tid, restart_token }
    }
    pub fn encode(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        let tid = self.tid.to_le_bytes();
        let tok = self.restart_token.to_le_bytes();
        out[..8].copy_from_slice(&tid);
        out[8..16].copy_from_slice(&tok);
        out
    }
    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() < 16 {
            return Err(ProcCodecError::Malformed);
        }
        let mut tid = [0u8; 8];
        let mut tok = [0u8; 8];
        tid.copy_from_slice(&payload[..8]);
        tok.copy_from_slice(&payload[8..16]);
        Ok(Self::new(u64::from_le_bytes(tid), u64::from_le_bytes(tok)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskRestartTokenRequest {
    pub tid: u64,
}

impl TaskRestartTokenRequest {
    pub const fn new(tid: u64) -> Self {
        Self { tid }
    }
    pub const fn encode(self) -> [u8; 8] {
        self.tid.to_le_bytes()
    }
    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() < 8 {
            return Err(ProcCodecError::Malformed);
        }
        let mut tid = [0u8; 8];
        tid.copy_from_slice(&payload[..8]);
        Ok(Self::new(u64::from_le_bytes(tid)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskRestartTokenReply {
    pub found: u8,
    pub token: u64,
}

impl TaskRestartTokenReply {
    pub const fn new(found: bool, token: u64) -> Self {
        Self {
            found: found as u8,
            token,
        }
    }
    pub const fn encode(self) -> [u8; 9] {
        let mut out = [0u8; 9];
        out[0] = self.found;
        let bytes = self.token.to_le_bytes();
        out[1] = bytes[0];
        out[2] = bytes[1];
        out[3] = bytes[2];
        out[4] = bytes[3];
        out[5] = bytes[4];
        out[6] = bytes[5];
        out[7] = bytes[6];
        out[8] = bytes[7];
        out
    }
    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() < 9 {
            return Err(ProcCodecError::Malformed);
        }
        let mut token = [0u8; 8];
        token.copy_from_slice(&payload[1..9]);
        Ok(Self {
            found: payload[0],
            token: u64::from_le_bytes(token),
        })
    }
    pub const fn found_token(self) -> Option<u64> {
        if self.found == 1 {
            Some(self.token)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV2Args {
    pub parent_pid: u64,
    pub image_id: u64,
}

impl SpawnV2Args {
    pub const VERSION: u16 = PROC_CODEC_V2_VERSION;

    pub const fn new(parent_pid: u64, image_id: u64) -> Self {
        Self {
            parent_pid,
            image_id,
        }
    }

    pub const fn encode(self) -> [u8; ProcV2Args::ENCODED_LEN] {
        ProcV2Args::new(self.parent_pid, self.image_id).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        let args = ProcV2Args::decode(payload)?;
        Ok(Self::new(args.arg0, args.arg1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV3Args {
    pub parent_pid: u64,
    pub image_id: u64,
    pub requested_cnode_slots: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV4Args {
    pub parent_pid: u64,
    pub image_id: u64,
    pub requested_cnode_slots: u64,
    pub task_class_hint: u64,
}

impl SpawnV4Args {
    pub const VERSION: u16 = PROC_CODEC_V4_VERSION;

    pub const fn new(
        parent_pid: u64,
        image_id: u64,
        requested_cnode_slots: u64,
        task_class_hint: u64,
    ) -> Self {
        Self {
            parent_pid,
            image_id,
            requested_cnode_slots,
            task_class_hint,
        }
    }

    pub const fn encode(self) -> [u8; ProcV4Args::ENCODED_LEN] {
        ProcV4Args::new(
            self.parent_pid,
            self.image_id,
            self.requested_cnode_slots,
            self.task_class_hint,
        )
        .encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        let args = ProcV4Args::decode(payload)?;
        Ok(Self::new(args.arg0, args.arg1, args.arg2, args.arg3))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV5CapArgs {
    pub parent_pid: u64,
    pub image_id: u64,
    pub service_caps: [u64; 4],
}

impl SpawnV5CapArgs {
    pub const ENCODED_LEN: usize = 48;

    pub const fn new(parent_pid: u64, image_id: u64, service_caps: [u64; 4]) -> Self {
        Self {
            parent_pid,
            image_id,
            service_caps,
        }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[..8].copy_from_slice(&self.parent_pid.to_le_bytes());
        out[8..16].copy_from_slice(&self.image_id.to_le_bytes());
        out[16..24].copy_from_slice(&self.service_caps[0].to_le_bytes());
        out[24..32].copy_from_slice(&self.service_caps[1].to_le_bytes());
        out[32..40].copy_from_slice(&self.service_caps[2].to_le_bytes());
        out[40..48].copy_from_slice(&self.service_caps[3].to_le_bytes());
        out
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(ProcCodecError::Malformed);
        }
        let mut a = [0u8; 8];
        a.copy_from_slice(&payload[..8]);
        let parent_pid = u64::from_le_bytes(a);
        a.copy_from_slice(&payload[8..16]);
        let image_id = u64::from_le_bytes(a);
        let mut caps = [0u64; 4];
        for i in 0..4 {
            a.copy_from_slice(&payload[16 + i * 8..24 + i * 8]);
            caps[i] = u64::from_le_bytes(a);
        }
        Ok(Self {
            parent_pid,
            image_id,
            service_caps: caps,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV5CapResult {
    pub pid: u64,
    pub service_send_cap: u64,
}

impl SpawnV5CapResult {
    pub const ENCODED_LEN: usize = 16;

    pub const fn new(pid: u64, service_send_cap: u64) -> Self {
        Self {
            pid,
            service_send_cap,
        }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[..8].copy_from_slice(&self.pid.to_le_bytes());
        out[8..16].copy_from_slice(&self.service_send_cap.to_le_bytes());
        out
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(ProcCodecError::Malformed);
        }
        let mut a = [0u8; 8];
        a.copy_from_slice(&payload[..8]);
        let pid = u64::from_le_bytes(a);
        a.copy_from_slice(&payload[8..16]);
        let service_send_cap = u64::from_le_bytes(a);
        Ok(Self {
            pid,
            service_send_cap,
        })
    }
}

#[inline]
pub fn encode_spawn_v5_reply(
    child_tid: u64,
    service_send_cap: u64,
) -> [u8; SpawnV5CapResult::ENCODED_LEN] {
    SpawnV5CapResult::new(child_tid, service_send_cap).encode()
}

#[inline]
pub fn decode_spawn_v5_reply(payload: &[u8]) -> Result<SpawnV5CapResult, ProcCodecError> {
    SpawnV5CapResult::decode(payload)
}

impl SpawnV3Args {
    pub const VERSION: u16 = PROC_CODEC_V3_VERSION;

    pub const fn new(parent_pid: u64, image_id: u64, requested_cnode_slots: u64) -> Self {
        Self {
            parent_pid,
            image_id,
            requested_cnode_slots,
        }
    }

    pub const fn encode(self) -> [u8; ProcV3Args::ENCODED_LEN] {
        ProcV3Args::new(self.parent_pid, self.image_id, self.requested_cnode_slots).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        let args = ProcV3Args::decode(payload)?;
        Ok(Self::new(args.arg0, args.arg1, args.arg2))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitPidV2Args {
    pub caller_pid: u64,
    pub target_pid: u64,
}

impl WaitPidV2Args {
    pub const VERSION: u16 = PROC_CODEC_V2_VERSION;

    pub const fn new(caller_pid: u64, target_pid: u64) -> Self {
        Self {
            caller_pid,
            target_pid,
        }
    }

    pub const fn encode(self) -> [u8; ProcV2Args::ENCODED_LEN] {
        ProcV2Args::new(self.caller_pid, self.target_pid).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        let args = ProcV2Args::decode(payload)?;
        Ok(Self::new(args.arg0, args.arg1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitPidV2Reply {
    pub waited_pid: u64,
    pub exit_code: u64,
}

impl WaitPidV2Reply {
    pub const VERSION: u16 = PROC_CODEC_V2_VERSION;

    pub const fn new(waited_pid: u64, exit_code: u64) -> Self {
        Self {
            waited_pid,
            exit_code,
        }
    }

    pub const fn encode(self) -> [u8; ProcV2Args::ENCODED_LEN] {
        ProcV2Args::new(self.waited_pid, self.exit_code).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        let args = ProcV2Args::decode(payload)?;
        Ok(Self::new(args.arg0, args.arg1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcV2Args {
    pub arg0: u64,
    pub arg1: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcV3Args {
    pub arg0: u64,
    pub arg1: u64,
    pub arg2: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcV4Args {
    pub arg0: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
}

impl ProcV4Args {
    pub const VERSION: u16 = PROC_CODEC_V4_VERSION;
    pub const ENCODED_LEN: usize = 32;

    pub const fn new(arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> Self {
        Self {
            arg0,
            arg1,
            arg2,
            arg3,
        }
    }

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut payload = [0u8; Self::ENCODED_LEN];
        let a0 = self.arg0.to_le_bytes();
        let a1 = self.arg1.to_le_bytes();
        let a2 = self.arg2.to_le_bytes();
        let a3 = self.arg3.to_le_bytes();
        let mut i = 0;
        while i < 8 {
            payload[i] = a0[i];
            payload[8 + i] = a1[i];
            payload[16 + i] = a2[i];
            payload[24 + i] = a3[i];
            i += 1;
        }
        payload
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(ProcCodecError::Malformed);
        }
        let mut a0 = [0u8; 8];
        let mut a1 = [0u8; 8];
        let mut a2 = [0u8; 8];
        let mut a3 = [0u8; 8];
        a0.copy_from_slice(&payload[..8]);
        a1.copy_from_slice(&payload[8..16]);
        a2.copy_from_slice(&payload[16..24]);
        a3.copy_from_slice(&payload[24..Self::ENCODED_LEN]);
        Ok(Self {
            arg0: u64::from_le_bytes(a0),
            arg1: u64::from_le_bytes(a1),
            arg2: u64::from_le_bytes(a2),
            arg3: u64::from_le_bytes(a3),
        })
    }
}

impl ProcV3Args {
    pub const VERSION: u16 = PROC_CODEC_V3_VERSION;
    pub const ENCODED_LEN: usize = 24;

    pub const fn new(arg0: u64, arg1: u64, arg2: u64) -> Self {
        Self { arg0, arg1, arg2 }
    }

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut payload = [0u8; Self::ENCODED_LEN];
        let a0 = self.arg0.to_le_bytes();
        let a1 = self.arg1.to_le_bytes();
        let a2 = self.arg2.to_le_bytes();
        let mut i = 0;
        while i < 8 {
            payload[i] = a0[i];
            payload[8 + i] = a1[i];
            payload[16 + i] = a2[i];
            i += 1;
        }
        payload
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(ProcCodecError::Malformed);
        }
        let mut a0 = [0u8; 8];
        let mut a1 = [0u8; 8];
        let mut a2 = [0u8; 8];
        a0.copy_from_slice(&payload[..8]);
        a1.copy_from_slice(&payload[8..16]);
        a2.copy_from_slice(&payload[16..Self::ENCODED_LEN]);
        Ok(Self {
            arg0: u64::from_le_bytes(a0),
            arg1: u64::from_le_bytes(a1),
            arg2: u64::from_le_bytes(a2),
        })
    }
}

impl ProcV2Args {
    pub const VERSION: u16 = PROC_CODEC_V2_VERSION;
    pub const ENCODED_LEN: usize = 16;

    pub const fn new(arg0: u64, arg1: u64) -> Self {
        Self { arg0, arg1 }
    }

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut payload = [0u8; Self::ENCODED_LEN];
        let a0 = self.arg0.to_le_bytes();
        let a1 = self.arg1.to_le_bytes();
        let mut i = 0;
        while i < 8 {
            payload[i] = a0[i];
            payload[8 + i] = a1[i];
            i += 1;
        }
        payload
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(ProcCodecError::Malformed);
        }
        let mut a0 = [0u8; 8];
        let mut a1 = [0u8; 8];
        a0.copy_from_slice(&payload[..8]);
        a1.copy_from_slice(&payload[8..Self::ENCODED_LEN]);
        Ok(Self {
            arg0: u64::from_le_bytes(a0),
            arg1: u64::from_le_bytes(a1),
        })
    }
}

// ── Stage 77+78: Kernel → PM task-exit payload ────────────────────────────────

/// Stage 77+78: payload for [`KERNEL_OP_PM_TASK_EXITED`] — kernel → PM push.
///
/// Wire layout (16 bytes LE):
/// ```text
/// [0..8]   tid:       u64 LE — TID of the exited task
/// [8..16]  exit_code: u64 LE — task exit code
/// ```
///
/// The kernel sends this to PM's `pm_task_exit_endpoint` whenever any task exits.
/// PM decodes it and, if it is a tracked task, forwards `PROC_OP_TASK_EXITED` to VFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelPmTaskExitedPayload {
    pub tid: u64,
    pub exit_code: u64,
}

impl KernelPmTaskExitedPayload {
    pub const ENCODED_LEN: usize = 16;

    pub const fn new(tid: u64, exit_code: u64) -> Self {
        Self { tid, exit_code }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[..8].copy_from_slice(&self.tid.to_le_bytes());
        out[8..16].copy_from_slice(&self.exit_code.to_le_bytes());
        out
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() < Self::ENCODED_LEN {
            return Err(ProcCodecError::Malformed);
        }
        let mut tid = [0u8; 8];
        let mut code = [0u8; 8];
        tid.copy_from_slice(&payload[..8]);
        code.copy_from_slice(&payload[8..16]);
        Ok(Self {
            tid: u64::from_le_bytes(tid),
            exit_code: u64::from_le_bytes(code),
        })
    }
}

// ── Stage 76: PM-owned lifecycle push notifications ────────────────────────────

/// Stage 76: payload for [`PROC_OP_TASK_EXITED`] — PM → VFS push notification.
///
/// Wire layout (16 bytes LE):
/// ```text
/// [0..8]   tid:       u64 LE — TID of the exited task
/// [8..16]  exit_code: u64 LE — task exit code
/// ```
///
/// PM sends this to VFS when a tracked task exits. VFS calls
/// `deliver_requester_exit_if_tid_matches(tid, handles)` on each active lifecycle.
/// No reply is sent by VFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmTaskExitedEvent {
    pub tid: u64,
    pub exit_code: u64,
}

impl PmTaskExitedEvent {
    pub const ENCODED_LEN: usize = 16;

    pub const fn new(tid: u64, exit_code: u64) -> Self {
        Self { tid, exit_code }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[..8].copy_from_slice(&self.tid.to_le_bytes());
        out[8..16].copy_from_slice(&self.exit_code.to_le_bytes());
        out
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() < Self::ENCODED_LEN {
            return Err(ProcCodecError::Malformed);
        }
        let mut tid = [0u8; 8];
        let mut code = [0u8; 8];
        tid.copy_from_slice(&payload[..8]);
        code.copy_from_slice(&payload[8..16]);
        Ok(Self {
            tid: u64::from_le_bytes(tid),
            exit_code: u64::from_le_bytes(code),
        })
    }
}

/// Stage 76: payload for [`PROC_OP_PROCESS_EXITED`] — PM → VFS push notification.
///
/// Wire layout (16 bytes LE):
/// ```text
/// [0..8]   process_tid: u64 LE — root TID identifying the process
/// [8..16]  exit_code:   u64 LE — process exit code
/// ```
///
/// Sent when an entire process (all threads) has exited.  VFS must clean up all
/// active lifecycles whose `requester_tid` matches `process_tid`.  In Stage 76
/// this is handled at per-tid granularity; a process→thread membership table is
/// a future extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmProcessExitedEvent {
    pub process_tid: u64,
    pub exit_code: u64,
}

impl PmProcessExitedEvent {
    pub const ENCODED_LEN: usize = 16;

    pub const fn new(process_tid: u64, exit_code: u64) -> Self {
        Self {
            process_tid,
            exit_code,
        }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[..8].copy_from_slice(&self.process_tid.to_le_bytes());
        out[8..16].copy_from_slice(&self.exit_code.to_le_bytes());
        out
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() < Self::ENCODED_LEN {
            return Err(ProcCodecError::Malformed);
        }
        let mut ptid = [0u8; 8];
        let mut code = [0u8; 8];
        ptid.copy_from_slice(&payload[..8]);
        code.copy_from_slice(&payload[8..16]);
        Ok(Self {
            process_tid: u64::from_le_bytes(ptid),
            exit_code: u64::from_le_bytes(code),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_v2_roundtrip() {
        let args = ProcV2Args::new(9, 11);
        let enc = args.encode();
        assert_eq!(ProcV2Args::decode(&enc), Ok(args));
    }

    #[test]
    fn proc_v2_rejects_non_exact_payload_lengths() {
        let short = [0u8; ProcV2Args::ENCODED_LEN - 1];
        assert_eq!(ProcV2Args::decode(&short), Err(ProcCodecError::Malformed));

        let long = [0u8; ProcV2Args::ENCODED_LEN + 1];
        assert_eq!(ProcV2Args::decode(&long), Err(ProcCodecError::Malformed));
    }

    #[test]
    fn proc_v2_constants_are_stable() {
        assert_eq!(PROC_SERVER_ABI_VERSION, 1);
        assert_eq!(PROC_CODEC_V2_VERSION, 2);
        assert_eq!(ProcV2Args::VERSION, PROC_CODEC_V2_VERSION);
        assert_eq!(ProcV2Args::ENCODED_LEN, 16);
        assert_eq!(SpawnV2Args::VERSION, PROC_CODEC_V2_VERSION);
        assert_eq!(WaitPidV2Args::VERSION, PROC_CODEC_V2_VERSION);
        assert_eq!(WaitPidV2Reply::VERSION, PROC_CODEC_V2_VERSION);
        assert_eq!(PROC_OP_SPAWN_V2, 4);
        assert_eq!(PROC_OP_WAITPID_V2, 5);
        assert_eq!(PROC_CODEC_V3_VERSION, 3);
        assert_eq!(ProcV3Args::VERSION, PROC_CODEC_V3_VERSION);
        assert_eq!(ProcV3Args::ENCODED_LEN, 24);
        assert_eq!(SpawnV3Args::VERSION, PROC_CODEC_V3_VERSION);
        assert_eq!(PROC_OP_SPAWN_V3, 6);
        assert_eq!(PROC_CODEC_V4_VERSION, 4);
        assert_eq!(ProcV4Args::VERSION, PROC_CODEC_V4_VERSION);
        assert_eq!(ProcV4Args::ENCODED_LEN, 32);
        assert_eq!(SpawnV4Args::VERSION, PROC_CODEC_V4_VERSION);
        assert_eq!(PROC_OP_SPAWN_V4, 7);
        assert_eq!(PROC_OP_TASK_RESTART_TOKEN, 8);
        assert_eq!(PROC_OP_REGISTER_SUPERVISED_TASK, 9);
        assert_eq!(PROC_OP_EXECUTE_RESTART, 10);
        assert_eq!(PROC_OP_LIFECYCLE_QUERY, 12);
        assert_eq!(LIFECYCLE_STATE_SPAWNED, 0);
        assert_eq!(LifecycleQueryReply::ENCODED_LEN, 19);
    }

    #[test]
    fn task_restart_token_codec_roundtrip() {
        let req = TaskRestartTokenRequest::new(42);
        assert_eq!(TaskRestartTokenRequest::decode(&req.encode()), Ok(req));

        let found = TaskRestartTokenReply::new(true, 0xAA55);
        assert_eq!(TaskRestartTokenReply::decode(&found.encode()), Ok(found));
        assert_eq!(found.found_token(), Some(0xAA55));

        let missing = TaskRestartTokenReply::new(false, 0);
        assert_eq!(
            TaskRestartTokenReply::decode(&missing.encode()),
            Ok(missing)
        );
        assert_eq!(missing.found_token(), None);
    }

    #[test]
    fn register_supervised_task_codec_roundtrip() {
        let reg = RegisterSupervisedTask::new(7, 99);
        assert_eq!(RegisterSupervisedTask::decode(&reg.encode()), Ok(reg));
    }

    #[test]
    fn execute_restart_codec_roundtrip() {
        let req = ExecuteRestartRequest::new(9, 44);
        assert_eq!(ExecuteRestartRequest::decode(&req.encode()), Ok(req));
        let rep = ExecuteRestartReply::new(ExecuteRestartReply::STATUS_NOT_FOUND);
        assert_eq!(ExecuteRestartReply::decode(&rep.encode()), Ok(rep));
    }

    #[test]
    fn typed_proc_v2_wrappers_roundtrip_via_frozen_codec() {
        let spawn = SpawnV2Args::new(7, 9);
        assert_eq!(SpawnV2Args::decode(&spawn.encode()), Ok(spawn));

        let wait = WaitPidV2Args::new(3, 4);
        assert_eq!(WaitPidV2Args::decode(&wait.encode()), Ok(wait));

        let reply = WaitPidV2Reply::new(4, 255);
        assert_eq!(WaitPidV2Reply::decode(&reply.encode()), Ok(reply));

        let spawn_v3 = SpawnV3Args::new(7, 9, 64);
        assert_eq!(SpawnV3Args::decode(&spawn_v3.encode()), Ok(spawn_v3));

        let spawn_v4 = SpawnV4Args::new(7, 9, 64, 2);
        assert_eq!(SpawnV4Args::decode(&spawn_v4.encode()), Ok(spawn_v4));
    }

    #[test]
    fn proc_v2_golden_vector_is_stable() {
        let args = ProcV2Args::new(0x1122_3344_5566_7788, 0x99aa_bbcc_ddee_ff00);
        let expected = [
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, // arg0 LE
            0x00, 0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, // arg1 LE
        ];
        assert_eq!(args.encode(), expected);
        assert_eq!(ProcV2Args::decode(&expected), Ok(args));
    }

    #[test]
    fn proc_v3_golden_vector_is_stable() {
        let args = ProcV3Args::new(
            0x1122_3344_5566_7788,
            0x99aa_bbcc_ddee_ff00,
            0x0102_0304_0506_0708,
        );
        let expected = [
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, // arg0 LE
            0x00, 0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, // arg1 LE
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // arg2 LE
        ];
        assert_eq!(args.encode(), expected);
        assert_eq!(ProcV3Args::decode(&expected), Ok(args));
    }

    #[test]
    fn proc_v4_golden_vector_is_stable() {
        let args = ProcV4Args::new(
            0x1122_3344_5566_7788,
            0x99aa_bbcc_ddee_ff00,
            0x0102_0304_0506_0708,
            0x0a0b_0c0d_0e0f_1011,
        );
        let expected = [
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, // arg0
            0x00, 0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, // arg1
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // arg2
            0x11, 0x10, 0x0f, 0x0e, 0x0d, 0x0c, 0x0b, 0x0a, // arg3
        ];
        assert_eq!(args.encode(), expected);
        assert_eq!(ProcV4Args::decode(&expected), Ok(args));
    }

    #[test]
    fn lifecycle_query_codec_roundtrip() {
        let req = LifecycleQueryRequest::new(42);
        assert_eq!(LifecycleQueryRequest::decode(&req.encode()), Ok(req));

        let found = LifecycleQueryReply::found(42, 6, LIFECYCLE_STATE_SPAWNED);
        assert_eq!(found.is_found(), true);
        assert_eq!(found.restart_supported, 0);
        let dec = LifecycleQueryReply::decode(&found.encode()).unwrap();
        assert_eq!(dec.found, 1);
        assert_eq!(dec.tid, 42);
        assert_eq!(dec.image_id, 6);
        assert_eq!(dec.state, LIFECYCLE_STATE_SPAWNED);
        assert_eq!(dec.restart_supported, 0);

        let not = LifecycleQueryReply::not_found();
        assert_eq!(not.is_found(), false);
        let dec2 = LifecycleQueryReply::decode(&not.encode()).unwrap();
        assert_eq!(dec2.found, 0);
    }

    #[test]
    fn lifecycle_query_reply_rejects_short_payload() {
        let short = [0u8; LifecycleQueryReply::ENCODED_LEN - 1];
        assert_eq!(
            LifecycleQueryReply::decode(&short),
            Err(ProcCodecError::Malformed)
        );
    }

    #[test]
    fn spawn_v5_reply_layout_is_stable_and_exact_len() {
        assert_eq!(SpawnV5CapResult::ENCODED_LEN, 16);
        let encoded = encode_spawn_v5_reply(10_000, 65_540);
        assert_eq!(
            encoded,
            [
                0x10, 0x27, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00,
                0x00, 0x00
            ]
        );
        let decoded = decode_spawn_v5_reply(&encoded).expect("decode");
        assert_eq!(decoded.pid, 10_000);
        assert_eq!(decoded.service_send_cap, 65_540);
    }

    #[test]
    fn spawn_v5_reply_decode_rejects_non_exact_payload_len() {
        let short = [0u8; SpawnV5CapResult::ENCODED_LEN - 1];
        let long = [0u8; SpawnV5CapResult::ENCODED_LEN + 1];
        assert_eq!(
            decode_spawn_v5_reply(&short),
            Err(ProcCodecError::Malformed)
        );
        assert_eq!(decode_spawn_v5_reply(&long), Err(ProcCodecError::Malformed));
    }
}

// SUP-L1 promoted PM restart ABI constants/codecs.
// Dispatch and supervisor send remain intentionally disabled.

pub const PM_RESTART_VERSION_V1: u16 = 1;
pub const PM_RESTART_SERVICE_NAME_MAX: usize = 32;
pub const PM_RESTART_REQUEST_V1_LEN: usize = 110;
pub const PM_RESTART_REPLY_V1_LEN: usize = 50;

pub const PM_RESTART_REQUEST_VERSION_OFFSET: usize = 0;
pub const PM_RESTART_REQUEST_ID_OFFSET: usize = 2;
pub const PM_RESTART_REQUEST_TARGET_TID_OFFSET: usize = 18;
pub const PM_RESTART_REQUEST_SERVICE_NAME_LEN_OFFSET: usize = 28;
pub const PM_RESTART_REQUEST_SERVICE_NAME_OFFSET: usize = 29;
pub const PM_RESTART_REQUEST_REASON_OFFSET: usize = 61;
pub const PM_RESTART_REQUEST_TOKEN_OWNER_OFFSET: usize = 86;
pub const PM_RESTART_REQUEST_TOKEN_FINGERPRINT_OFFSET: usize = 94;
/// Reserved byte after token scope. Must encode as zero; decoders reject nonzero.
pub const PM_RESTART_REQUEST_TOKEN_RESERVED_OFFSET: usize = 97;

pub const PM_RESTART_REPLY_VERSION_OFFSET: usize = 0;
pub const PM_RESTART_REPLY_REQUEST_ID_OFFSET: usize = 2;
pub const PM_RESTART_REPLY_STATUS_OFFSET: usize = 18;
pub const PM_RESTART_REPLY_FAILURE_OFFSET: usize = 20;
pub const PM_RESTART_REPLY_RETRY_TICK_OFFSET: usize = 42;

// SUP-8 reserved-field policy: every byte named reserved in this review codec
// must encode as zero, and decode must reject nonzero values. Future extension
// requires a version bump or an explicit compatibility rule in the ABI signoff.
// `policy_flags` are descriptive review flags only; live authority must come
// from verified sender/token capability state, not from payload flags alone.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmRestartCodecError {
    Malformed,
    UnsupportedVersion,
    InvalidEnum,
    OversizedServiceName,
    RawOrUnscopedToken,
    NonzeroReserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmRestartReason {
    Fault = 1,
    NormalExit = 2,
    CrashLoop = 3,
    DependencyFailed = 4,
    ManualPolicy = 5,
    HealthTimeout = 6,
}

impl PmRestartReason {
    fn from_u16(value: u16) -> Result<Self, PmRestartCodecError> {
        match value {
            1 => Ok(Self::Fault),
            2 => Ok(Self::NormalExit),
            3 => Ok(Self::CrashLoop),
            4 => Ok(Self::DependencyFailed),
            5 => Ok(Self::ManualPolicy),
            6 => Ok(Self::HealthTimeout),
            _ => Err(PmRestartCodecError::InvalidEnum),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmRestartReplyStatus {
    Accepted = 1,
    Rejected = 2,
    Deferred = 3,
    RolledBack = 4,
    UnsupportedVersion = 5,
    AlreadyRestarting = 6,
    NoSuchTarget = 7,
}

impl PmRestartReplyStatus {
    fn from_u16(value: u16) -> Result<Self, PmRestartCodecError> {
        match value {
            1 => Ok(Self::Accepted),
            2 => Ok(Self::Rejected),
            3 => Ok(Self::Deferred),
            4 => Ok(Self::RolledBack),
            5 => Ok(Self::UnsupportedVersion),
            6 => Ok(Self::AlreadyRestarting),
            7 => Ok(Self::NoSuchTarget),
            _ => Err(PmRestartCodecError::InvalidEnum),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmRestartFailure {
    None = 0,
    MissingRight = 1,
    WrongTokenOwner = 2,
    RawTokenUnsupported = 3,
    RestartLimitExceeded = 4,
    DependencyBlocked = 5,
    ResourceUnavailable = 6,
    StartupCapLayoutUnsupported = 7,
    RollbackFailed = 8,
    TimerUnavailable = 9,
    UnsupportedVersion = 10,
}

impl PmRestartFailure {
    fn from_u16(value: u16) -> Result<Self, PmRestartCodecError> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::MissingRight),
            2 => Ok(Self::WrongTokenOwner),
            3 => Ok(Self::RawTokenUnsupported),
            4 => Ok(Self::RestartLimitExceeded),
            5 => Ok(Self::DependencyBlocked),
            6 => Ok(Self::ResourceUnavailable),
            7 => Ok(Self::StartupCapLayoutUnsupported),
            8 => Ok(Self::RollbackFailed),
            9 => Ok(Self::TimerUnavailable),
            10 => Ok(Self::UnsupportedVersion),
            _ => Err(PmRestartCodecError::InvalidEnum),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmRestartTokenDescriptor {
    pub owner_tid: u64,
    pub redacted_fingerprint: u16,
    pub scoped: bool,
}

impl PmRestartTokenDescriptor {
    pub const fn scoped(owner_tid: u64, redacted_fingerprint: u16) -> Self {
        Self {
            owner_tid,
            redacted_fingerprint,
            scoped: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmRestartRequestV1 {
    pub version: u16,
    pub request_id: u64,
    pub supervisor_tid: u64,
    pub target_tid: u64,
    pub service_kind: u16,
    pub service_name_len: u8,
    pub service_name: [u8; PM_RESTART_SERVICE_NAME_MAX],
    pub restart_reason: PmRestartReason,
    pub attempt_count: u16,
    pub due_tick: u64,
    pub dependency_cause_tid: u64,
    pub degraded_hint: bool,
    pub policy_flags: u32,
    pub token: PmRestartTokenDescriptor,
    pub startup_cap_policy: u32,
    pub rollback_policy: u32,
    pub health_monitor_policy: u32,
}

impl PmRestartRequestV1 {
    pub fn new(
        request_id: u64,
        supervisor_tid: u64,
        target_tid: u64,
        service_kind: u16,
        service_name: &[u8],
        restart_reason: PmRestartReason,
        token: PmRestartTokenDescriptor,
    ) -> Result<Self, PmRestartCodecError> {
        if service_name.len() > PM_RESTART_SERVICE_NAME_MAX {
            return Err(PmRestartCodecError::OversizedServiceName);
        }
        if !token.scoped {
            return Err(PmRestartCodecError::RawOrUnscopedToken);
        }
        let mut name = [0u8; PM_RESTART_SERVICE_NAME_MAX];
        name[..service_name.len()].copy_from_slice(service_name);
        Ok(Self {
            version: PM_RESTART_VERSION_V1,
            request_id,
            supervisor_tid,
            target_tid,
            service_kind,
            service_name_len: service_name.len() as u8,
            service_name: name,
            restart_reason,
            attempt_count: 1,
            due_tick: 0,
            dependency_cause_tid: 0,
            degraded_hint: false,
            policy_flags: 0,
            token,
            startup_cap_policy: 0,
            rollback_policy: 0,
            health_monitor_policy: 0,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmRestartReplyV1 {
    pub version: u16,
    pub request_id: u64,
    pub target_tid: u64,
    pub status: PmRestartReplyStatus,
    pub failure: PmRestartFailure,
    pub replacement_handle_kind: u16,
    pub replacement_handle_value: u64,
    pub cleanup_status: u16,
    pub accounting_status: u16,
    pub startup_cap_status: u16,
    pub health_monitor_status: u16,
    pub rollback_status: u16,
    pub next_retry_tick: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sup4PmRestartOracleDescriptor {
    pub request_id: u32,
    pub target_tid: u64,
    pub restart_reason: PmRestartReason,
    pub attempt_count: u8,
    pub due_tick: u64,
    pub dependency_cause_tid: u64,
    pub token_owner_tid: u64,
    pub token_fingerprint: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sup4PmRestartOracleReplyDescriptor {
    pub request_id: u32,
    pub target_tid: u64,
    pub status: PmRestartReplyStatus,
    pub failure: PmRestartFailure,
    pub retry_tick: u64,
}

pub fn request_from_sup4_oracle(
    oracle: Sup4PmRestartOracleDescriptor,
) -> Result<PmRestartRequestV1, PmRestartCodecError> {
    let mut request = PmRestartRequestV1::new(
        oracle.request_id as u64,
        4,
        oracle.target_tid,
        1,
        b"oracle-service",
        oracle.restart_reason,
        PmRestartTokenDescriptor::scoped(oracle.token_owner_tid, oracle.token_fingerprint),
    )?;
    request.attempt_count = oracle.attempt_count as u16;
    request.due_tick = oracle.due_tick;
    request.dependency_cause_tid = oracle.dependency_cause_tid;
    Ok(request)
}

pub fn oracle_from_request(request: PmRestartRequestV1) -> Sup4PmRestartOracleDescriptor {
    Sup4PmRestartOracleDescriptor {
        request_id: request.request_id as u32,
        target_tid: request.target_tid,
        restart_reason: request.restart_reason,
        attempt_count: request.attempt_count as u8,
        due_tick: request.due_tick,
        dependency_cause_tid: request.dependency_cause_tid,
        token_owner_tid: request.token.owner_tid,
        token_fingerprint: request.token.redacted_fingerprint,
    }
}

pub fn reply_from_sup4_oracle(oracle: Sup4PmRestartOracleReplyDescriptor) -> PmRestartReplyV1 {
    PmRestartReplyV1 {
        version: PM_RESTART_VERSION_V1,
        request_id: oracle.request_id as u64,
        target_tid: oracle.target_tid,
        status: oracle.status,
        failure: oracle.failure,
        replacement_handle_kind: (oracle.status == PmRestartReplyStatus::Accepted) as u16,
        replacement_handle_value: if oracle.status == PmRestartReplyStatus::Accepted {
            0x504d_5355_5037
        } else {
            0
        },
        cleanup_status: 0,
        accounting_status: 0,
        startup_cap_status: 0,
        health_monitor_status: 0,
        rollback_status: (oracle.status == PmRestartReplyStatus::RolledBack) as u16,
        next_retry_tick: oracle.retry_tick,
    }
}

pub fn oracle_from_reply(reply: PmRestartReplyV1) -> Sup4PmRestartOracleReplyDescriptor {
    Sup4PmRestartOracleReplyDescriptor {
        request_id: reply.request_id as u32,
        target_tid: reply.target_tid,
        status: reply.status,
        failure: reply.failure,
        retry_tick: reply.next_retry_tick,
    }
}

pub fn encode_pm_restart_request_v1(
    request: &PmRestartRequestV1,
) -> Result<[u8; PM_RESTART_REQUEST_V1_LEN], PmRestartCodecError> {
    if request.version != PM_RESTART_VERSION_V1 {
        return Err(PmRestartCodecError::UnsupportedVersion);
    }
    if request.service_name_len as usize > PM_RESTART_SERVICE_NAME_MAX {
        return Err(PmRestartCodecError::OversizedServiceName);
    }
    if !request.token.scoped {
        return Err(PmRestartCodecError::RawOrUnscopedToken);
    }
    let mut out = [0u8; PM_RESTART_REQUEST_V1_LEN];
    put_u16(&mut out, PM_RESTART_REQUEST_VERSION_OFFSET, request.version);
    put_u64(&mut out, PM_RESTART_REQUEST_ID_OFFSET, request.request_id);
    put_u64(&mut out, 10, request.supervisor_tid);
    put_u64(
        &mut out,
        PM_RESTART_REQUEST_TARGET_TID_OFFSET,
        request.target_tid,
    );
    put_u16(&mut out, 26, request.service_kind);
    out[PM_RESTART_REQUEST_SERVICE_NAME_LEN_OFFSET] = request.service_name_len;
    out[PM_RESTART_REQUEST_SERVICE_NAME_OFFSET..PM_RESTART_REQUEST_SERVICE_NAME_OFFSET + 32]
        .copy_from_slice(&request.service_name);
    put_u16(
        &mut out,
        PM_RESTART_REQUEST_REASON_OFFSET,
        request.restart_reason as u16,
    );
    put_u16(&mut out, 63, request.attempt_count);
    put_u64(&mut out, 65, request.due_tick);
    put_u64(&mut out, 73, request.dependency_cause_tid);
    out[81] = request.degraded_hint as u8;
    put_u32(&mut out, 82, request.policy_flags);
    put_u64(
        &mut out,
        PM_RESTART_REQUEST_TOKEN_OWNER_OFFSET,
        request.token.owner_tid,
    );
    put_u16(
        &mut out,
        PM_RESTART_REQUEST_TOKEN_FINGERPRINT_OFFSET,
        request.token.redacted_fingerprint,
    );
    out[96] = request.token.scoped as u8;
    out[97] = 0;
    put_u32(&mut out, 98, request.startup_cap_policy);
    put_u32(&mut out, 102, request.rollback_policy);
    put_u32(&mut out, 106, request.health_monitor_policy);
    Ok(out)
}

pub fn decode_pm_restart_request_v1(
    bytes: &[u8],
) -> Result<PmRestartRequestV1, PmRestartCodecError> {
    if bytes.len() != PM_RESTART_REQUEST_V1_LEN {
        return Err(PmRestartCodecError::Malformed);
    }
    let version = get_u16(bytes, PM_RESTART_REQUEST_VERSION_OFFSET);
    if version != PM_RESTART_VERSION_V1 {
        return Err(PmRestartCodecError::UnsupportedVersion);
    }
    let service_name_len = bytes[PM_RESTART_REQUEST_SERVICE_NAME_LEN_OFFSET];
    if service_name_len as usize > PM_RESTART_SERVICE_NAME_MAX {
        return Err(PmRestartCodecError::OversizedServiceName);
    }
    let restart_reason =
        PmRestartReason::from_u16(get_u16(bytes, PM_RESTART_REQUEST_REASON_OFFSET))?;
    let token_scoped = bytes[96] == 1;
    if bytes[96] > 1 || !token_scoped {
        return Err(PmRestartCodecError::RawOrUnscopedToken);
    }
    if bytes[PM_RESTART_REQUEST_TOKEN_RESERVED_OFFSET] != 0 {
        return Err(PmRestartCodecError::NonzeroReserved);
    }
    let mut service_name = [0u8; PM_RESTART_SERVICE_NAME_MAX];
    service_name.copy_from_slice(
        &bytes[PM_RESTART_REQUEST_SERVICE_NAME_OFFSET..PM_RESTART_REQUEST_SERVICE_NAME_OFFSET + 32],
    );
    Ok(PmRestartRequestV1 {
        version,
        request_id: get_u64(bytes, PM_RESTART_REQUEST_ID_OFFSET),
        supervisor_tid: get_u64(bytes, 10),
        target_tid: get_u64(bytes, PM_RESTART_REQUEST_TARGET_TID_OFFSET),
        service_kind: get_u16(bytes, 26),
        service_name_len,
        service_name,
        restart_reason,
        attempt_count: get_u16(bytes, 63),
        due_tick: get_u64(bytes, 65),
        dependency_cause_tid: get_u64(bytes, 73),
        degraded_hint: bytes[81] != 0,
        policy_flags: get_u32(bytes, 82),
        token: PmRestartTokenDescriptor {
            owner_tid: get_u64(bytes, PM_RESTART_REQUEST_TOKEN_OWNER_OFFSET),
            redacted_fingerprint: get_u16(bytes, PM_RESTART_REQUEST_TOKEN_FINGERPRINT_OFFSET),
            scoped: true,
        },
        startup_cap_policy: get_u32(bytes, 98),
        rollback_policy: get_u32(bytes, 102),
        health_monitor_policy: get_u32(bytes, 106),
    })
}

pub fn encode_pm_restart_reply_v1(
    reply: &PmRestartReplyV1,
) -> Result<[u8; PM_RESTART_REPLY_V1_LEN], PmRestartCodecError> {
    if reply.version != PM_RESTART_VERSION_V1 {
        return Err(PmRestartCodecError::UnsupportedVersion);
    }
    let mut out = [0u8; PM_RESTART_REPLY_V1_LEN];
    put_u16(&mut out, PM_RESTART_REPLY_VERSION_OFFSET, reply.version);
    put_u64(
        &mut out,
        PM_RESTART_REPLY_REQUEST_ID_OFFSET,
        reply.request_id,
    );
    put_u64(&mut out, 10, reply.target_tid);
    put_u16(
        &mut out,
        PM_RESTART_REPLY_STATUS_OFFSET,
        reply.status as u16,
    );
    put_u16(
        &mut out,
        PM_RESTART_REPLY_FAILURE_OFFSET,
        reply.failure as u16,
    );
    put_u16(&mut out, 22, reply.replacement_handle_kind);
    put_u64(&mut out, 24, reply.replacement_handle_value);
    put_u16(&mut out, 32, reply.cleanup_status);
    put_u16(&mut out, 34, reply.accounting_status);
    put_u16(&mut out, 36, reply.startup_cap_status);
    put_u16(&mut out, 38, reply.health_monitor_status);
    put_u16(&mut out, 40, reply.rollback_status);
    put_u64(
        &mut out,
        PM_RESTART_REPLY_RETRY_TICK_OFFSET,
        reply.next_retry_tick,
    );
    Ok(out)
}

pub fn decode_pm_restart_reply_v1(bytes: &[u8]) -> Result<PmRestartReplyV1, PmRestartCodecError> {
    if bytes.len() != PM_RESTART_REPLY_V1_LEN {
        return Err(PmRestartCodecError::Malformed);
    }
    let version = get_u16(bytes, PM_RESTART_REPLY_VERSION_OFFSET);
    if version != PM_RESTART_VERSION_V1 {
        return Err(PmRestartCodecError::UnsupportedVersion);
    }
    Ok(PmRestartReplyV1 {
        version,
        request_id: get_u64(bytes, PM_RESTART_REPLY_REQUEST_ID_OFFSET),
        target_tid: get_u64(bytes, 10),
        status: PmRestartReplyStatus::from_u16(get_u16(bytes, PM_RESTART_REPLY_STATUS_OFFSET))?,
        failure: PmRestartFailure::from_u16(get_u16(bytes, PM_RESTART_REPLY_FAILURE_OFFSET))?,
        replacement_handle_kind: get_u16(bytes, 22),
        replacement_handle_value: get_u64(bytes, 24),
        cleanup_status: get_u16(bytes, 32),
        accounting_status: get_u16(bytes, 34),
        startup_cap_status: get_u16(bytes, 36),
        health_monitor_status: get_u16(bytes, 38),
        rollback_status: get_u16(bytes, 40),
        next_retry_tick: get_u64(bytes, PM_RESTART_REPLY_RETRY_TICK_OFFSET),
    })
}

pub fn accepted_reply(request_id: u64, target_tid: u64) -> PmRestartReplyV1 {
    PmRestartReplyV1 {
        version: PM_RESTART_VERSION_V1,
        request_id,
        target_tid,
        status: PmRestartReplyStatus::Accepted,
        failure: PmRestartFailure::None,
        replacement_handle_kind: 1,
        replacement_handle_value: 0x504d_5355_5037,
        cleanup_status: 1,
        accounting_status: 1,
        startup_cap_status: 1,
        health_monitor_status: 1,
        rollback_status: 0,
        next_retry_tick: 0,
    }
}

const fn get_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

fn put_u16(out: &mut [u8], offset: usize, value: u16) {
    out[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut [u8], offset: usize, value: u64) {
    out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod pm_restart_abi_tests {
    use super::*;

    #[test]
    fn pm_restart_opcodes_and_count_are_sup_l1_allocated() {
        assert_eq!(PROC_OP_PM_RESTART_V1, 15);
        assert_eq!(PROC_OP_PM_RESTART_REPLY_V1, 16);
        assert_eq!(PROCESS_IPC_OPCODE_COUNT, 16);
    }

    #[test]
    fn pm_restart_request_v1_fixed_size_offsets_and_rejections() {
        let request = PmRestartRequestV1::new(
            0x0102_0304_0506_0708,
            4,
            77,
            3,
            b"vfs",
            PmRestartReason::Fault,
            PmRestartTokenDescriptor::scoped(77, 0xBEEF),
        )
        .expect("valid request");
        let encoded = encode_pm_restart_request_v1(&request).expect("encode");
        assert_eq!(encoded.len(), 110);
        assert_eq!(PM_RESTART_REQUEST_VERSION_OFFSET, 0);
        assert_eq!(PM_RESTART_REQUEST_ID_OFFSET, 2);
        assert_eq!(PM_RESTART_REQUEST_TARGET_TID_OFFSET, 18);
        assert_eq!(PM_RESTART_REQUEST_SERVICE_NAME_LEN_OFFSET, 28);
        assert_eq!(PM_RESTART_REQUEST_SERVICE_NAME_OFFSET, 29);
        assert_eq!(PM_RESTART_REQUEST_REASON_OFFSET, 61);
        assert_eq!(PM_RESTART_REQUEST_TOKEN_OWNER_OFFSET, 86);
        assert_eq!(PM_RESTART_REQUEST_TOKEN_FINGERPRINT_OFFSET, 94);
        assert_eq!(PM_RESTART_REQUEST_TOKEN_RESERVED_OFFSET, 97);
        assert_eq!(decode_pm_restart_request_v1(&encoded), Ok(request));
        assert_eq!(
            decode_pm_restart_request_v1(&encoded[..109]),
            Err(PmRestartCodecError::Malformed)
        );
        let mut bad = encoded;
        bad[PM_RESTART_REQUEST_TOKEN_RESERVED_OFFSET] = 1;
        assert_eq!(
            decode_pm_restart_request_v1(&bad),
            Err(PmRestartCodecError::NonzeroReserved)
        );
    }

    #[test]
    fn pm_restart_reply_v1_fixed_size_offsets_and_rejections() {
        let reply = accepted_reply(7, 77);
        let encoded = encode_pm_restart_reply_v1(&reply).expect("encode");
        assert_eq!(encoded.len(), 50);
        assert_eq!(PM_RESTART_REPLY_VERSION_OFFSET, 0);
        assert_eq!(PM_RESTART_REPLY_REQUEST_ID_OFFSET, 2);
        assert_eq!(PM_RESTART_REPLY_STATUS_OFFSET, 18);
        assert_eq!(PM_RESTART_REPLY_FAILURE_OFFSET, 20);
        assert_eq!(PM_RESTART_REPLY_RETRY_TICK_OFFSET, 42);
        assert_eq!(decode_pm_restart_reply_v1(&encoded), Ok(reply));
        assert_eq!(
            decode_pm_restart_reply_v1(&encoded[..49]),
            Err(PmRestartCodecError::Malformed)
        );
        let mut bad = encoded;
        bad[PM_RESTART_REPLY_STATUS_OFFSET..PM_RESTART_REPLY_STATUS_OFFSET + 2]
            .copy_from_slice(&99u16.to_le_bytes());
        assert_eq!(
            decode_pm_restart_reply_v1(&bad),
            Err(PmRestartCodecError::InvalidEnum)
        );
    }
}
