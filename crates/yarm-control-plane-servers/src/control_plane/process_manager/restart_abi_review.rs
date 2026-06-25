// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

// SUP-7 non-live PM restart ABI codec review helpers.
//
// This module is compiled only for tests/`hosted-dev` review builds via
// `process_manager::mod.rs`. It intentionally does not define live process IPC
// opcodes and is not referenced by PM runtime dispatch.

pub const PM_RESTART_REVIEW_VERSION_V1: u16 = 1;
pub const PM_RESTART_REVIEW_SERVICE_NAME_MAX: usize = 32;
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

pub const PM_RESTART_REPLY_VERSION_OFFSET: usize = 0;
pub const PM_RESTART_REPLY_REQUEST_ID_OFFSET: usize = 2;
pub const PM_RESTART_REPLY_STATUS_OFFSET: usize = 18;
pub const PM_RESTART_REPLY_FAILURE_OFFSET: usize = 20;
pub const PM_RESTART_REPLY_RETRY_TICK_OFFSET: usize = 42;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmRestartReviewCodecError {
    Malformed,
    UnsupportedVersion,
    InvalidEnum,
    OversizedServiceName,
    RawOrUnscopedToken,
    NonzeroReserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmRestartReviewReason {
    Fault = 1,
    NormalExit = 2,
    CrashLoop = 3,
    DependencyFailed = 4,
    ManualPolicy = 5,
    HealthTimeout = 6,
}

impl PmRestartReviewReason {
    fn from_u16(value: u16) -> Result<Self, PmRestartReviewCodecError> {
        match value {
            1 => Ok(Self::Fault),
            2 => Ok(Self::NormalExit),
            3 => Ok(Self::CrashLoop),
            4 => Ok(Self::DependencyFailed),
            5 => Ok(Self::ManualPolicy),
            6 => Ok(Self::HealthTimeout),
            _ => Err(PmRestartReviewCodecError::InvalidEnum),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmRestartReviewReplyStatus {
    Accepted = 1,
    Rejected = 2,
    Deferred = 3,
    RolledBack = 4,
    UnsupportedVersion = 5,
    AlreadyRestarting = 6,
    NoSuchTarget = 7,
}

impl PmRestartReviewReplyStatus {
    fn from_u16(value: u16) -> Result<Self, PmRestartReviewCodecError> {
        match value {
            1 => Ok(Self::Accepted),
            2 => Ok(Self::Rejected),
            3 => Ok(Self::Deferred),
            4 => Ok(Self::RolledBack),
            5 => Ok(Self::UnsupportedVersion),
            6 => Ok(Self::AlreadyRestarting),
            7 => Ok(Self::NoSuchTarget),
            _ => Err(PmRestartReviewCodecError::InvalidEnum),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmRestartReviewFailure {
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

impl PmRestartReviewFailure {
    fn from_u16(value: u16) -> Result<Self, PmRestartReviewCodecError> {
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
            _ => Err(PmRestartReviewCodecError::InvalidEnum),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmRestartReviewTokenDescriptor {
    pub owner_tid: u64,
    pub redacted_fingerprint: u16,
    pub scoped: bool,
}

impl PmRestartReviewTokenDescriptor {
    pub const fn scoped(owner_tid: u64, redacted_fingerprint: u16) -> Self {
        Self {
            owner_tid,
            redacted_fingerprint,
            scoped: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmRestartRequestV1Review {
    pub version: u16,
    pub request_id: u64,
    pub supervisor_tid: u64,
    pub target_tid: u64,
    pub service_kind: u16,
    pub service_name_len: u8,
    pub service_name: [u8; PM_RESTART_REVIEW_SERVICE_NAME_MAX],
    pub restart_reason: PmRestartReviewReason,
    pub attempt_count: u16,
    pub due_tick: u64,
    pub dependency_cause_tid: u64,
    pub degraded_hint: bool,
    pub policy_flags: u32,
    pub token: PmRestartReviewTokenDescriptor,
    pub startup_cap_policy: u32,
    pub rollback_policy: u32,
    pub health_monitor_policy: u32,
}

impl PmRestartRequestV1Review {
    pub fn new(
        request_id: u64,
        supervisor_tid: u64,
        target_tid: u64,
        service_kind: u16,
        service_name: &[u8],
        restart_reason: PmRestartReviewReason,
        token: PmRestartReviewTokenDescriptor,
    ) -> Result<Self, PmRestartReviewCodecError> {
        if service_name.len() > PM_RESTART_REVIEW_SERVICE_NAME_MAX {
            return Err(PmRestartReviewCodecError::OversizedServiceName);
        }
        if !token.scoped {
            return Err(PmRestartReviewCodecError::RawOrUnscopedToken);
        }
        let mut name = [0u8; PM_RESTART_REVIEW_SERVICE_NAME_MAX];
        name[..service_name.len()].copy_from_slice(service_name);
        Ok(Self {
            version: PM_RESTART_REVIEW_VERSION_V1,
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
pub struct PmRestartReplyV1Review {
    pub version: u16,
    pub request_id: u64,
    pub target_tid: u64,
    pub status: PmRestartReviewReplyStatus,
    pub failure: PmRestartReviewFailure,
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
    pub restart_reason: PmRestartReviewReason,
    pub attempt_count: u8,
    pub due_tick: u64,
    pub dependency_cause_tid: u64,
    pub token_owner_tid: u64,
    pub token_fingerprint: u16,
}

pub fn request_from_sup4_oracle(
    oracle: Sup4PmRestartOracleDescriptor,
) -> Result<PmRestartRequestV1Review, PmRestartReviewCodecError> {
    let mut request = PmRestartRequestV1Review::new(
        oracle.request_id as u64,
        4,
        oracle.target_tid,
        1,
        b"oracle-service",
        oracle.restart_reason,
        PmRestartReviewTokenDescriptor::scoped(oracle.token_owner_tid, oracle.token_fingerprint),
    )?;
    request.attempt_count = oracle.attempt_count as u16;
    request.due_tick = oracle.due_tick;
    request.dependency_cause_tid = oracle.dependency_cause_tid;
    Ok(request)
}

pub fn oracle_from_request(request: PmRestartRequestV1Review) -> Sup4PmRestartOracleDescriptor {
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

pub fn encode_pm_restart_request_v1(
    request: &PmRestartRequestV1Review,
) -> Result<[u8; PM_RESTART_REQUEST_V1_LEN], PmRestartReviewCodecError> {
    if request.version != PM_RESTART_REVIEW_VERSION_V1 {
        return Err(PmRestartReviewCodecError::UnsupportedVersion);
    }
    if request.service_name_len as usize > PM_RESTART_REVIEW_SERVICE_NAME_MAX {
        return Err(PmRestartReviewCodecError::OversizedServiceName);
    }
    if !request.token.scoped {
        return Err(PmRestartReviewCodecError::RawOrUnscopedToken);
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
) -> Result<PmRestartRequestV1Review, PmRestartReviewCodecError> {
    if bytes.len() != PM_RESTART_REQUEST_V1_LEN {
        return Err(PmRestartReviewCodecError::Malformed);
    }
    let version = get_u16(bytes, PM_RESTART_REQUEST_VERSION_OFFSET);
    if version != PM_RESTART_REVIEW_VERSION_V1 {
        return Err(PmRestartReviewCodecError::UnsupportedVersion);
    }
    let service_name_len = bytes[PM_RESTART_REQUEST_SERVICE_NAME_LEN_OFFSET];
    if service_name_len as usize > PM_RESTART_REVIEW_SERVICE_NAME_MAX {
        return Err(PmRestartReviewCodecError::OversizedServiceName);
    }
    let restart_reason =
        PmRestartReviewReason::from_u16(get_u16(bytes, PM_RESTART_REQUEST_REASON_OFFSET))?;
    let token_scoped = bytes[96] == 1;
    if bytes[96] > 1 || !token_scoped {
        return Err(PmRestartReviewCodecError::RawOrUnscopedToken);
    }
    if bytes[97] != 0 {
        return Err(PmRestartReviewCodecError::NonzeroReserved);
    }
    let mut service_name = [0u8; PM_RESTART_REVIEW_SERVICE_NAME_MAX];
    service_name.copy_from_slice(
        &bytes[PM_RESTART_REQUEST_SERVICE_NAME_OFFSET..PM_RESTART_REQUEST_SERVICE_NAME_OFFSET + 32],
    );
    Ok(PmRestartRequestV1Review {
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
        token: PmRestartReviewTokenDescriptor {
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
    reply: &PmRestartReplyV1Review,
) -> Result<[u8; PM_RESTART_REPLY_V1_LEN], PmRestartReviewCodecError> {
    if reply.version != PM_RESTART_REVIEW_VERSION_V1 {
        return Err(PmRestartReviewCodecError::UnsupportedVersion);
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

pub fn decode_pm_restart_reply_v1(
    bytes: &[u8],
) -> Result<PmRestartReplyV1Review, PmRestartReviewCodecError> {
    if bytes.len() != PM_RESTART_REPLY_V1_LEN {
        return Err(PmRestartReviewCodecError::Malformed);
    }
    let version = get_u16(bytes, PM_RESTART_REPLY_VERSION_OFFSET);
    if version != PM_RESTART_REVIEW_VERSION_V1 {
        return Err(PmRestartReviewCodecError::UnsupportedVersion);
    }
    Ok(PmRestartReplyV1Review {
        version,
        request_id: get_u64(bytes, PM_RESTART_REPLY_REQUEST_ID_OFFSET),
        target_tid: get_u64(bytes, 10),
        status: PmRestartReviewReplyStatus::from_u16(get_u16(
            bytes,
            PM_RESTART_REPLY_STATUS_OFFSET,
        ))?,
        failure: PmRestartReviewFailure::from_u16(get_u16(bytes, PM_RESTART_REPLY_FAILURE_OFFSET))?,
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

pub fn accepted_reply(request_id: u64, target_tid: u64) -> PmRestartReplyV1Review {
    PmRestartReplyV1Review {
        version: PM_RESTART_REVIEW_VERSION_V1,
        request_id,
        target_tid,
        status: PmRestartReviewReplyStatus::Accepted,
        failure: PmRestartReviewFailure::None,
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
