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
pub const PROC_CODEC_V5_VERSION: u16 = 5;

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
pub const PROC_OP_SPAWN_V5: u16 = 11;

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
        Ok(Self::new(u64::from_le_bytes(tid), u64::from_le_bytes(token)))
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
        Self { found: found as u8, token }
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
        Ok(Self { found: payload[0], token: u64::from_le_bytes(token) })
    }
    pub const fn found_token(self) -> Option<u64> {
        if self.found == 1 { Some(self.token) } else { None }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV5Args {
    pub parent_pid: u64,
    pub image_id: u64,
    pub requested_cnode_slots: u64,
    pub task_class_hint: u64,
    pub startup_caps: ServiceStartupCapsV1,
}

/// Structured startup-cap contract for service/task launches.
///
/// This is intentionally decoupled from legacy startup slot overloading so
/// orchestration layers can pass startup capabilities explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceStartupCapsV1 {
    pub version: u16,
    pub role: u16,
    pub request_recv_cap: u64,
    pub control_send_cap: u64,
    pub control_recv_cap: u64,
    pub reserved0: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitOrchestrationCapsV1 {
    pub version: u16,
    pub reserved: u16,
    pub initramfs_request_send_cap: u64,
    pub initramfs_request_recv_cap_for_child: u64,
    pub control0: u64,
    pub control1: u64,
}

impl InitOrchestrationCapsV1 {
    pub const VERSION: u16 = 1;
}

impl ServiceStartupCapsV1 {
    pub const VERSION: u16 = 1;
    pub const ENCODED_LEN: usize = 40;

    pub const fn new(role: u16, request_recv_cap: u64) -> Self {
        Self {
            version: Self::VERSION,
            role,
            request_recv_cap,
            control_send_cap: 0,
            control_recv_cap: 0,
            reserved0: 0,
        }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        let version = self.version.to_le_bytes();
        let role = self.role.to_le_bytes();
        let req = self.request_recv_cap.to_le_bytes();
        let csend = self.control_send_cap.to_le_bytes();
        let crecv = self.control_recv_cap.to_le_bytes();
        let r0 = self.reserved0.to_le_bytes();
        out[0] = version[0];
        out[1] = version[1];
        out[2] = role[0];
        out[3] = role[1];
        out[8..16].copy_from_slice(&req);
        out[16..24].copy_from_slice(&csend);
        out[24..32].copy_from_slice(&crecv);
        out[32..40].copy_from_slice(&r0);
        out
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(ProcCodecError::Malformed);
        }
        let version = u16::from_le_bytes([payload[0], payload[1]]);
        let role = u16::from_le_bytes([payload[2], payload[3]]);
        let mut req = [0u8; 8];
        req.copy_from_slice(&payload[8..16]);
        let mut csend = [0u8; 8];
        csend.copy_from_slice(&payload[16..24]);
        let mut crecv = [0u8; 8];
        crecv.copy_from_slice(&payload[24..32]);
        let mut r0 = [0u8; 8];
        r0.copy_from_slice(&payload[32..40]);
        Ok(Self {
            version,
            role,
            request_recv_cap: u64::from_le_bytes(req),
            control_send_cap: u64::from_le_bytes(csend),
            control_recv_cap: u64::from_le_bytes(crecv),
            reserved0: u64::from_le_bytes(r0),
        })
    }
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

impl SpawnV5Args {
    pub const VERSION: u16 = PROC_CODEC_V5_VERSION;
    pub const ENCODED_LEN: usize = ProcV4Args::ENCODED_LEN + ServiceStartupCapsV1::ENCODED_LEN;

    pub const fn new(
        parent_pid: u64,
        image_id: u64,
        requested_cnode_slots: u64,
        task_class_hint: u64,
        startup_caps: ServiceStartupCapsV1,
    ) -> Self {
        Self { parent_pid, image_id, requested_cnode_slots, task_class_hint, startup_caps }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        let base = SpawnV4Args::new(
            self.parent_pid,
            self.image_id,
            self.requested_cnode_slots,
            self.task_class_hint,
        )
        .encode();
        out[..ProcV4Args::ENCODED_LEN].copy_from_slice(&base);
        out[ProcV4Args::ENCODED_LEN..].copy_from_slice(&self.startup_caps.encode());
        out
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(ProcCodecError::Malformed);
        }
        let v4 = SpawnV4Args::decode(&payload[..ProcV4Args::ENCODED_LEN])?;
        let startup_caps = ServiceStartupCapsV1::decode(&payload[ProcV4Args::ENCODED_LEN..])?;
        Ok(Self::new(v4.parent_pid, v4.image_id, v4.requested_cnode_slots, v4.task_class_hint, startup_caps))
    }
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
    }

    #[test]
    fn task_restart_token_codec_roundtrip() {
        let req = TaskRestartTokenRequest::new(42);
        assert_eq!(TaskRestartTokenRequest::decode(&req.encode()), Ok(req));

        let found = TaskRestartTokenReply::new(true, 0xAA55);
        assert_eq!(TaskRestartTokenReply::decode(&found.encode()), Ok(found));
        assert_eq!(found.found_token(), Some(0xAA55));

        let missing = TaskRestartTokenReply::new(false, 0);
        assert_eq!(TaskRestartTokenReply::decode(&missing.encode()), Ok(missing));
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
    fn service_startup_caps_v1_roundtrip() {
        let caps = ServiceStartupCapsV1 {
            version: ServiceStartupCapsV1::VERSION,
            role: 2,
            request_recv_cap: 11,
            control_send_cap: 22,
            control_recv_cap: 33,
            reserved0: 0,
        };
        let encoded = caps.encode();
        assert_eq!(
            ServiceStartupCapsV1::decode(&encoded).expect("decode"),
            caps
        );
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
    fn spawn_v5_args_roundtrip() {
        let caps = ServiceStartupCapsV1::new(3, 11);
        let args = SpawnV5Args::new(9, 7, 64, 2, caps);
        let encoded = args.encode();
        let decoded = SpawnV5Args::decode(&encoded).expect("decode");
        assert_eq!(decoded, args);
    }
}
