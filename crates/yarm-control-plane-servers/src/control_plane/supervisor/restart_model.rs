// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Non-live supervisor↔PM restart contract/model home.
//!
//! This module is intentionally compiled only for hosted development and tests.
//! SUP-12 mechanically isolates SUP-2..SUP-10 restart contract/model/readiness
//! code from the production supervisor hot path. It performs no PM IPC, runtime
//! dispatch, restart/spawn/teardown, capability mint/revoke, resource grants, or
//! MMIO; it exists only for hosted-dev/test/docs guardrails until future SUP-L1
//! live work deliberately wires a real contract-compliant PM client.

#![cfg(any(test, feature = "hosted-dev"))]
#![allow(dead_code)]

use super::*;

/// Source guard marker documenting that restart contract/model code is non-live.
pub const SUPERVISOR_RESTART_MODEL_NON_LIVE: &str = "SUPERVISOR_RESTART_MODEL_NON_LIVE";

const MAX_RESTART_REQUESTS: usize = MAX_MANAGED_SERVICES;
const MAX_PM_RESTART_RESERVATIONS: usize = 7;
const MAX_PM_RESTART_ROLLBACK_STEPS: usize = MAX_PM_RESTART_RESERVATIONS;
pub const SUPERVISOR_PM_RESTART_REQUEST_VERSION: u16 = 1;
const SUPERVISOR_PM_RESTART_AUTHORITY_MARKER: u32 = 0x5355_5052;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorRestartReason {
    Fault,
    NormalExit,
    CrashLoop,
    DependencyFailed { failed_tid: u64 },
    ManualPolicy,
    HealthTimeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorRestartTokenRef {
    pub owner_tid: u64,
    pub redacted_fingerprint: u16,
}

impl SupervisorRestartTokenRef {
    fn from_token(owner_tid: u64, token: u64) -> Self {
        Self {
            owner_tid,
            redacted_fingerprint: ((token >> 48) as u16) ^ (token as u16),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPmHandleRef {
    pub mock_request_id: u32,
    pub authority_marker: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorRestartBlocker {
    NoRestartPolicy,
    BlockedNoDependentToken { dependent_tid: u64, failed_tid: u64 },
    BlockedRestartLimit,
    MissingRestartToken,
    ManualStopNoRestart,
    PmAuthorityUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorRestartRequestStatus {
    WouldRequestPmRestart,
    Blocked(SupervisorRestartBlocker),
    NoAction,
    AlreadyPending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorRestartRequestFailure {
    BundleFull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorRestartPolicy {
    pub request_version: u16,
    pub pm_authority_available: bool,
    pub fail_closed: bool,
    pub include_no_action_entries: bool,
}

impl Default for SupervisorRestartPolicy {
    fn default() -> Self {
        Self {
            request_version: SUPERVISOR_PM_RESTART_REQUEST_VERSION,
            pm_authority_available: true,
            fail_closed: true,
            include_no_action_entries: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorRestartRequest {
    pub request_version: u16,
    pub tid: u64,
    pub service_kind: ManagedServiceKind,
    pub service_name: &'static str,
    pub restart_token: Option<SupervisorRestartTokenRef>,
    pub restart_owner: RestartOwner,
    pub reason: SupervisorRestartReason,
    pub backoff_due_tick: u64,
    pub attempt_count: u8,
    pub dependency_cause: Option<u64>,
    pub degraded: bool,
    pub pm_handle: SupervisorPmHandleRef,
    pub status: SupervisorRestartRequestStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorRestartRequestBundle {
    pub entries: [Option<SupervisorRestartRequest>; MAX_RESTART_REQUESTS],
    pub len: usize,
}

impl SupervisorRestartRequestBundle {
    const fn empty() -> Self {
        Self {
            entries: [None; MAX_RESTART_REQUESTS],
            len: 0,
        }
    }

    fn push(
        &mut self,
        request: SupervisorRestartRequest,
    ) -> Result<(), SupervisorRestartRequestFailure> {
        if self.len >= self.entries.len() {
            return Err(SupervisorRestartRequestFailure::BundleFull);
        }
        self.entries[self.len] = Some(request);
        self.len += 1;
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = SupervisorRestartRequest> + '_ {
        self.entries[..self.len].iter().flatten().copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorPmRestartValidationStatus {
    WouldAccept,
    WouldReject,
    Deferred,
    NoAction,
    AlreadyPending,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorPmRestartValidationFailure {
    MissingVerifiedSupervisorIdentity,
    MissingRestartToken,
    RestartTokenWrongOwner,
    MissingTargetRecord,
    RestartLimitExceeded,
    DependencyBlocked,
    PmAuthorityUnavailable,
    UnsupportedVersion,
    FailClosedPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPmRestartValidationPolicy {
    pub verified_supervisor_tid: Option<u64>,
    pub pm_authority_available: bool,
    pub supported_version: u16,
    pub fail_closed: bool,
}

impl Default for SupervisorPmRestartValidationPolicy {
    fn default() -> Self {
        Self {
            verified_supervisor_tid: Some(0),
            pm_authority_available: true,
            supported_version: SUPERVISOR_PM_RESTART_REQUEST_VERSION,
            fail_closed: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPmRestartValidationEntry {
    pub tid: u64,
    pub request_id: u32,
    pub status: SupervisorPmRestartValidationStatus,
    pub failure: Option<SupervisorPmRestartValidationFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPmRestartValidationReport {
    pub entries: [Option<SupervisorPmRestartValidationEntry>; MAX_RESTART_REQUESTS],
    pub len: usize,
}

impl SupervisorPmRestartValidationReport {
    const fn empty() -> Self {
        Self {
            entries: [None; MAX_RESTART_REQUESTS],
            len: 0,
        }
    }
    fn push(&mut self, entry: SupervisorPmRestartValidationEntry) {
        if self.len < self.entries.len() {
            self.entries[self.len] = Some(entry);
            self.len += 1;
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = SupervisorPmRestartValidationEntry> + '_ {
        self.entries[..self.len].iter().flatten().copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorPmRestartReservation {
    RestartSlot,
    ReplacementTaskSlot,
    AddressSpaceSlot,
    CNodeSlot,
    StartupCapDeliverySlot,
    HealthMonitorSlot,
    InitAlertSlot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorPmRestartAccountingStatus {
    Reserved,
    RolledBack,
    Deferred,
    NoAction,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorPmRestartFailureInjectionPoint {
    None,
    AfterReplacementTaskSlot,
    AfterStartupCapSlot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPmRestartAccountingPolicy {
    pub failure_injection: SupervisorPmRestartFailureInjectionPoint,
}

impl Default for SupervisorPmRestartAccountingPolicy {
    fn default() -> Self {
        Self {
            failure_injection: SupervisorPmRestartFailureInjectionPoint::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPmRestartRollbackStep {
    pub tid: u64,
    pub reservation: SupervisorPmRestartReservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPmRestartAccountingEntry {
    pub tid: u64,
    pub request_id: u32,
    pub status: SupervisorPmRestartAccountingStatus,
    pub reservations: [Option<SupervisorPmRestartReservation>; MAX_PM_RESTART_RESERVATIONS],
    pub reservation_len: usize,
    pub rollback: [Option<SupervisorPmRestartRollbackStep>; MAX_PM_RESTART_ROLLBACK_STEPS],
    pub rollback_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPmRestartAccountingReport {
    pub entries: [Option<SupervisorPmRestartAccountingEntry>; MAX_RESTART_REQUESTS],
    pub len: usize,
}

impl SupervisorPmRestartAccountingReport {
    const fn empty() -> Self {
        Self {
            entries: [None; MAX_RESTART_REQUESTS],
            len: 0,
        }
    }
    fn push(&mut self, entry: SupervisorPmRestartAccountingEntry) {
        if self.len < self.entries.len() {
            self.entries[self.len] = Some(entry);
            self.len += 1;
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = SupervisorPmRestartAccountingEntry> + '_ {
        self.entries[..self.len].iter().flatten().copied()
    }
}

pub type SupervisorPmRestartContractVersion = u16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPmRestartWireLimits {
    pub max_requests: usize,
    pub max_service_name_bytes: usize,
    pub max_reply_entries: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPmRestartContract {
    pub version: SupervisorPmRestartContractVersion,
    pub wire_limits: SupervisorPmRestartWireLimits,
    pub requires_verified_supervisor_identity: bool,
    pub token_must_be_scoped_to_target: bool,
    pub mock_only: bool,
}

impl Default for SupervisorPmRestartContract {
    fn default() -> Self {
        Self {
            version: SUPERVISOR_PM_RESTART_REQUEST_VERSION,
            wire_limits: SupervisorPmRestartWireLimits {
                max_requests: MAX_RESTART_REQUESTS,
                max_service_name_bytes: 32,
                max_reply_entries: MAX_RESTART_REQUESTS,
            },
            requires_verified_supervisor_identity: true,
            token_must_be_scoped_to_target: true,
            mock_only: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorPmRestartDescriptorStatus {
    Sendable,
    NonSendable(SupervisorRestartBlocker),
    Deferred(SupervisorRestartBlocker),
    NoAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorStartupCapabilityBehavior {
    PreserveExisting,
    RequestPmDelivery,
    DeferredNoCaps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorHealthMonitorBehavior {
    PreserveExisting,
    RequestRegistration,
    Deferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorRollbackExpectation {
    PmRollbackRequired,
    SupervisorNoRollbackAuthority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPmRestartRequestV1 {
    pub version: SupervisorPmRestartContractVersion,
    pub descriptor_status: SupervisorPmRestartDescriptorStatus,
    pub requires_verified_supervisor_identity: bool,
    pub target_tid: u64,
    pub service_kind: ManagedServiceKind,
    pub service_name: &'static str,
    pub restart_token: Option<SupervisorRestartTokenRef>,
    pub restart_reason: SupervisorRestartReason,
    pub attempt_count: u8,
    pub due_tick: u64,
    pub dependency_cause: Option<u64>,
    pub degraded_hint: bool,
    pub policy_flags: u32,
    pub startup_capability_behavior: SupervisorStartupCapabilityBehavior,
    pub health_monitor_behavior: SupervisorHealthMonitorBehavior,
    pub rollback_expectation: SupervisorRollbackExpectation,
    pub mock_request_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorPmRestartReplyStatus {
    Accepted,
    Rejected,
    Deferred,
    RolledBack,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorPmRestartReplyFailure {
    None,
    InvalidVersion,
    MissingSupervisorIdentity,
    TokenRejected,
    TargetUnknown,
    RestartLimitExceeded,
    AccountingFailed,
    StartupCapDeliveryFailed,
    HealthMonitorFailed,
    RollbackFailed,
    PmUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPmReplacementHandleRef {
    pub mock_generation: u32,
    pub mock_pm_slot: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPmRestartReplyV1 {
    pub version: SupervisorPmRestartContractVersion,
    pub request_id: u32,
    pub status: SupervisorPmRestartReplyStatus,
    pub replacement: Option<SupervisorPmReplacementHandleRef>,
    pub old_task_cleanup: SupervisorPmRestartAccountingStatus,
    pub accounting_result: SupervisorPmRestartAccountingStatus,
    pub startup_cap_delivery: SupervisorPmRestartAccountingStatus,
    pub health_monitor_registration: SupervisorPmRestartAccountingStatus,
    pub rollback_result: SupervisorPmRestartAccountingStatus,
    pub failure: SupervisorPmRestartReplyFailure,
    pub next_retry_tick: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorTimerMode {
    LogicalTickOnly,
    FutureTimerEndpoint,
    FuturePmTimerSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorBackoffSchedule {
    pub base_ticks: u64,
    pub max_ticks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorBackoffDecision {
    DueAt(TickInstant),
    DeferredNoTimer,
    OverflowCapped(TickInstant),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorTimerEvent {
    pub mode: SupervisorTimerMode,
    pub tick: TickInstant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorTimerFailure {
    EndpointUnavailable,
    TickOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorPmRestartReplyOutcomeStatus {
    AcceptedRecorded,
    RejectedBlocked,
    DeferredRetryScheduled,
    RollbackMarkedDegraded,
    InvalidVersionRejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPmRestartReplyOutcome {
    pub tid: u64,
    pub request_id: u32,
    pub status: SupervisorPmRestartReplyOutcomeStatus,
    pub replacement: Option<SupervisorPmReplacementHandleRef>,
    pub retry_tick: Option<TickInstant>,
    pub degraded: bool,
    pub failure: SupervisorPmRestartReplyFailure,
}

impl SupervisorService {
    fn restart_owner_for_kind(kind: ManagedServiceKind) -> RestartOwner {
        match kind {
            ManagedServiceKind::Core(core) => CoreServicePolicyTable::restart_owner_for(core),
            ManagedServiceKind::Driver => RestartOwner::Supervisor,
        }
    }

    fn mock_restart_request_id(tid: u64, due_tick: TickInstant, attempt_count: u8) -> u32 {
        ((tid as u32) << 8) ^ (due_tick.0 as u32) ^ attempt_count as u32
    }

    fn build_restart_request_for_record(
        &self,
        record: ManagedServiceRecord,
        policy: SupervisorRestartPolicy,
        reason: SupervisorRestartReason,
    ) -> SupervisorRestartRequest {
        let restart_policy = self.policy_for(record);
        let due_tick = record.pending_restart_due.map(|tick| tick.0).unwrap_or(0);
        let dependency_cause = match reason {
            SupervisorRestartReason::DependencyFailed { failed_tid } => Some(failed_tid),
            _ => None,
        };
        let status = if record.pending_restart_due.is_none() {
            SupervisorRestartRequestStatus::NoAction
        } else if record.restart_attempts > restart_policy.max_restarts {
            SupervisorRestartRequestStatus::Blocked(SupervisorRestartBlocker::BlockedRestartLimit)
        } else if !policy.pm_authority_available {
            SupervisorRestartRequestStatus::Blocked(
                SupervisorRestartBlocker::PmAuthorityUnavailable,
            )
        } else if record.pending_restart_token.is_none() {
            SupervisorRestartRequestStatus::Blocked(SupervisorRestartBlocker::MissingRestartToken)
        } else {
            SupervisorRestartRequestStatus::WouldRequestPmRestart
        };
        SupervisorRestartRequest {
            request_version: policy.request_version,
            tid: record.tid,
            service_kind: record.kind,
            service_name: Self::service_name(record.kind),
            restart_token: record
                .pending_restart_token
                .map(|token| SupervisorRestartTokenRef::from_token(record.tid, token)),
            restart_owner: Self::restart_owner_for_kind(record.kind),
            reason,
            backoff_due_tick: due_tick,
            attempt_count: record.restart_attempts,
            dependency_cause,
            degraded: self.degraded,
            pm_handle: SupervisorPmHandleRef {
                mock_request_id: Self::mock_restart_request_id(
                    record.tid,
                    record.pending_restart_due.unwrap_or(TickInstant(0)),
                    record.restart_attempts,
                ),
                authority_marker: SUPERVISOR_PM_RESTART_AUTHORITY_MARKER,
            },
            status,
        }
    }

    pub fn build_restart_request_bundle(
        &self,
        policy: SupervisorRestartPolicy,
    ) -> Result<SupervisorRestartRequestBundle, SupervisorRestartRequestFailure> {
        let mut bundle = SupervisorRestartRequestBundle::empty();
        for record in self.managed.iter().flatten().copied() {
            if record.pending_restart_due.is_some() || policy.include_no_action_entries {
                bundle.push(self.build_restart_request_for_record(
                    record,
                    policy,
                    SupervisorRestartReason::Fault,
                ))?;
            }
        }
        Ok(bundle)
    }

    pub fn build_dependency_blocked_restart_request(
        &self,
        dependent_tid: u64,
        failed_tid: u64,
    ) -> Option<SupervisorRestartRequest> {
        let record = self.find_record(dependent_tid)?;
        let mut request = self.build_restart_request_for_record(
            record,
            SupervisorRestartPolicy::default(),
            SupervisorRestartReason::DependencyFailed { failed_tid },
        );
        request.status = SupervisorRestartRequestStatus::Blocked(
            SupervisorRestartBlocker::BlockedNoDependentToken {
                dependent_tid,
                failed_tid,
            },
        );
        request.restart_token = None;
        Some(request)
    }

    pub fn validate_restart_request_bundle(
        &self,
        bundle: SupervisorRestartRequestBundle,
        policy: SupervisorPmRestartValidationPolicy,
    ) -> SupervisorPmRestartValidationReport {
        let mut report = SupervisorPmRestartValidationReport::empty();
        for request in bundle.iter() {
            let (status, failure) = if request.request_version != policy.supported_version {
                (
                    SupervisorPmRestartValidationStatus::Unsupported,
                    Some(SupervisorPmRestartValidationFailure::UnsupportedVersion),
                )
            } else if policy.fail_closed {
                (
                    SupervisorPmRestartValidationStatus::WouldReject,
                    Some(SupervisorPmRestartValidationFailure::FailClosedPolicy),
                )
            } else if policy.verified_supervisor_tid.is_none() {
                (
                    SupervisorPmRestartValidationStatus::WouldReject,
                    Some(SupervisorPmRestartValidationFailure::MissingVerifiedSupervisorIdentity),
                )
            } else if !policy.pm_authority_available {
                (
                    SupervisorPmRestartValidationStatus::Deferred,
                    Some(SupervisorPmRestartValidationFailure::PmAuthorityUnavailable),
                )
            } else if matches!(request.status, SupervisorRestartRequestStatus::NoAction) {
                (SupervisorPmRestartValidationStatus::NoAction, None)
            } else if matches!(
                request.status,
                SupervisorRestartRequestStatus::AlreadyPending
            ) {
                (SupervisorPmRestartValidationStatus::AlreadyPending, None)
            } else if let SupervisorRestartRequestStatus::Blocked(blocker) = request.status {
                let failure = match blocker {
                    SupervisorRestartBlocker::BlockedRestartLimit => {
                        SupervisorPmRestartValidationFailure::RestartLimitExceeded
                    }
                    SupervisorRestartBlocker::BlockedNoDependentToken { .. } => {
                        SupervisorPmRestartValidationFailure::DependencyBlocked
                    }
                    SupervisorRestartBlocker::MissingRestartToken => {
                        SupervisorPmRestartValidationFailure::MissingRestartToken
                    }
                    SupervisorRestartBlocker::PmAuthorityUnavailable => {
                        SupervisorPmRestartValidationFailure::PmAuthorityUnavailable
                    }
                    _ => SupervisorPmRestartValidationFailure::DependencyBlocked,
                };
                (
                    SupervisorPmRestartValidationStatus::WouldReject,
                    Some(failure),
                )
            } else {
                match (self.find_record(request.tid), request.restart_token) {
                    (None, _) => (
                        SupervisorPmRestartValidationStatus::WouldReject,
                        Some(SupervisorPmRestartValidationFailure::MissingTargetRecord),
                    ),
                    (_, None) => (
                        SupervisorPmRestartValidationStatus::WouldReject,
                        Some(SupervisorPmRestartValidationFailure::MissingRestartToken),
                    ),
                    (Some(_), Some(token_ref)) if token_ref.owner_tid != request.tid => (
                        SupervisorPmRestartValidationStatus::WouldReject,
                        Some(SupervisorPmRestartValidationFailure::RestartTokenWrongOwner),
                    ),
                    (Some(record), Some(_))
                        if request.attempt_count > self.policy_for(record).max_restarts =>
                    {
                        (
                            SupervisorPmRestartValidationStatus::WouldReject,
                            Some(SupervisorPmRestartValidationFailure::RestartLimitExceeded),
                        )
                    }
                    (Some(_), Some(_)) => (SupervisorPmRestartValidationStatus::WouldAccept, None),
                }
            };
            report.push(SupervisorPmRestartValidationEntry {
                tid: request.tid,
                request_id: request.pm_handle.mock_request_id,
                status,
                failure,
            });
        }
        report
    }

    pub fn account_restart_validation_report(
        &self,
        validation: SupervisorPmRestartValidationReport,
        policy: SupervisorPmRestartAccountingPolicy,
    ) -> SupervisorPmRestartAccountingReport {
        let mut report = SupervisorPmRestartAccountingReport::empty();
        for entry in validation.iter() {
            let mut accounting = SupervisorPmRestartAccountingEntry {
                tid: entry.tid,
                request_id: entry.request_id,
                status: SupervisorPmRestartAccountingStatus::NoAction,
                reservations: [None; MAX_PM_RESTART_RESERVATIONS],
                reservation_len: 0,
                rollback: [None; MAX_PM_RESTART_ROLLBACK_STEPS],
                rollback_len: 0,
            };
            if entry.status != SupervisorPmRestartValidationStatus::WouldAccept {
                accounting.status = match entry.status {
                    SupervisorPmRestartValidationStatus::Deferred => {
                        SupervisorPmRestartAccountingStatus::Deferred
                    }
                    SupervisorPmRestartValidationStatus::NoAction => {
                        SupervisorPmRestartAccountingStatus::NoAction
                    }
                    _ => SupervisorPmRestartAccountingStatus::Rejected,
                };
                report.push(accounting);
                continue;
            }

            let reservations = [
                SupervisorPmRestartReservation::RestartSlot,
                SupervisorPmRestartReservation::ReplacementTaskSlot,
                SupervisorPmRestartReservation::AddressSpaceSlot,
                SupervisorPmRestartReservation::CNodeSlot,
                SupervisorPmRestartReservation::StartupCapDeliverySlot,
                SupervisorPmRestartReservation::HealthMonitorSlot,
                SupervisorPmRestartReservation::InitAlertSlot,
            ];
            let failure_after = match policy.failure_injection {
                SupervisorPmRestartFailureInjectionPoint::None => reservations.len(),
                SupervisorPmRestartFailureInjectionPoint::AfterReplacementTaskSlot => 2,
                SupervisorPmRestartFailureInjectionPoint::AfterStartupCapSlot => 5,
            };
            let reserve_len = failure_after.min(reservations.len());
            let mut idx = 0;
            while idx < reserve_len {
                accounting.reservations[idx] = Some(reservations[idx]);
                accounting.reservation_len += 1;
                idx += 1;
            }
            if reserve_len == reservations.len()
                && policy.failure_injection == SupervisorPmRestartFailureInjectionPoint::None
            {
                accounting.status = SupervisorPmRestartAccountingStatus::Reserved;
            } else {
                accounting.status = SupervisorPmRestartAccountingStatus::RolledBack;
                let mut rollback_idx = 0;
                while rollback_idx < reserve_len {
                    let reservation = reservations[reserve_len - 1 - rollback_idx];
                    accounting.rollback[rollback_idx] = Some(SupervisorPmRestartRollbackStep {
                        tid: entry.tid,
                        reservation,
                    });
                    accounting.rollback_len += 1;
                    rollback_idx += 1;
                }
            }
            report.push(accounting);
        }
        report
    }

    pub fn map_restart_request_to_pm_descriptor(
        &self,
        request: SupervisorRestartRequest,
        contract: SupervisorPmRestartContract,
    ) -> SupervisorPmRestartRequestV1 {
        let descriptor_status = match request.status {
            SupervisorRestartRequestStatus::WouldRequestPmRestart
                if request.restart_token.is_some() && contract.mock_only =>
            {
                SupervisorPmRestartDescriptorStatus::Sendable
            }
            SupervisorRestartRequestStatus::WouldRequestPmRestart => {
                SupervisorPmRestartDescriptorStatus::NonSendable(
                    SupervisorRestartBlocker::MissingRestartToken,
                )
            }
            SupervisorRestartRequestStatus::Blocked(
                SupervisorRestartBlocker::PmAuthorityUnavailable,
            ) => SupervisorPmRestartDescriptorStatus::Deferred(
                SupervisorRestartBlocker::PmAuthorityUnavailable,
            ),
            SupervisorRestartRequestStatus::Blocked(blocker) => {
                SupervisorPmRestartDescriptorStatus::NonSendable(blocker)
            }
            SupervisorRestartRequestStatus::NoAction => {
                SupervisorPmRestartDescriptorStatus::NoAction
            }
            SupervisorRestartRequestStatus::AlreadyPending => {
                SupervisorPmRestartDescriptorStatus::NonSendable(
                    SupervisorRestartBlocker::ManualStopNoRestart,
                )
            }
        };
        SupervisorPmRestartRequestV1 {
            version: contract.version,
            descriptor_status,
            requires_verified_supervisor_identity: contract.requires_verified_supervisor_identity,
            target_tid: request.tid,
            service_kind: request.service_kind,
            service_name: request.service_name,
            restart_token: request.restart_token,
            restart_reason: request.reason,
            attempt_count: request.attempt_count,
            due_tick: request.backoff_due_tick,
            dependency_cause: request.dependency_cause,
            degraded_hint: request.degraded,
            policy_flags: (request.restart_owner == RestartOwner::Supervisor) as u32,
            startup_capability_behavior: SupervisorStartupCapabilityBehavior::RequestPmDelivery,
            health_monitor_behavior: SupervisorHealthMonitorBehavior::RequestRegistration,
            rollback_expectation: SupervisorRollbackExpectation::PmRollbackRequired,
            mock_request_id: request.pm_handle.mock_request_id,
        }
    }

    pub fn compute_backoff_decision(
        current_tick: TickInstant,
        attempt_count: u8,
        schedule: SupervisorBackoffSchedule,
        timer_mode: SupervisorTimerMode,
    ) -> SupervisorBackoffDecision {
        if matches!(timer_mode, SupervisorTimerMode::FutureTimerEndpoint)
            && schedule.base_ticks == 0
        {
            return SupervisorBackoffDecision::DeferredNoTimer;
        }
        let multiplier_shift = attempt_count.min(16) as u32;
        let multiplier = 1u64.checked_shl(multiplier_shift).unwrap_or(u64::MAX);
        let uncapped = schedule.base_ticks.saturating_mul(multiplier);
        let capped = uncapped.min(schedule.max_ticks);
        match current_tick.0.checked_add(capped) {
            Some(tick) if uncapped <= schedule.max_ticks => {
                SupervisorBackoffDecision::DueAt(TickInstant(tick))
            }
            Some(tick) => SupervisorBackoffDecision::OverflowCapped(TickInstant(tick)),
            None => SupervisorBackoffDecision::OverflowCapped(TickInstant(u64::MAX)),
        }
    }

    pub fn due_restart_ready(timer: SupervisorTimerEvent, due_tick: TickInstant) -> bool {
        timer.tick.0 >= due_tick.0
    }

    pub fn apply_pm_restart_reply_model(
        &self,
        request: SupervisorPmRestartRequestV1,
        reply: SupervisorPmRestartReplyV1,
    ) -> SupervisorPmRestartReplyOutcome {
        if reply.version != request.version {
            return SupervisorPmRestartReplyOutcome {
                tid: request.target_tid,
                request_id: request.mock_request_id,
                status: SupervisorPmRestartReplyOutcomeStatus::InvalidVersionRejected,
                replacement: None,
                retry_tick: None,
                degraded: true,
                failure: SupervisorPmRestartReplyFailure::InvalidVersion,
            };
        }
        match reply.status {
            SupervisorPmRestartReplyStatus::Accepted => SupervisorPmRestartReplyOutcome {
                tid: request.target_tid,
                request_id: reply.request_id,
                status: SupervisorPmRestartReplyOutcomeStatus::AcceptedRecorded,
                replacement: reply.replacement,
                retry_tick: None,
                degraded: false,
                failure: reply.failure,
            },
            SupervisorPmRestartReplyStatus::Deferred => SupervisorPmRestartReplyOutcome {
                tid: request.target_tid,
                request_id: reply.request_id,
                status: SupervisorPmRestartReplyOutcomeStatus::DeferredRetryScheduled,
                replacement: None,
                retry_tick: reply.next_retry_tick.map(TickInstant),
                degraded: false,
                failure: reply.failure,
            },
            SupervisorPmRestartReplyStatus::RolledBack => SupervisorPmRestartReplyOutcome {
                tid: request.target_tid,
                request_id: reply.request_id,
                status: SupervisorPmRestartReplyOutcomeStatus::RollbackMarkedDegraded,
                replacement: reply.replacement,
                retry_tick: None,
                degraded: true,
                failure: reply.failure,
            },
            SupervisorPmRestartReplyStatus::Rejected
            | SupervisorPmRestartReplyStatus::Unsupported => SupervisorPmRestartReplyOutcome {
                tid: request.target_tid,
                request_id: reply.request_id,
                status: SupervisorPmRestartReplyOutcomeStatus::RejectedBlocked,
                replacement: None,
                retry_tick: None,
                degraded: true,
                failure: reply.failure,
            },
        }
    }

    #[cfg(test)]
    fn recv_with_budget(
        &self,
        kernel: &mut KernelState,
        recv_cap: CapId,
    ) -> Result<Option<Message>, KernelError> {
        match kernel.try_ipc_recv(recv_cap) {
            Ok(Some(msg)) => return Ok(Some(msg)),
            Ok(None) => {}
            Err(KernelError::TaskMissing) => return Ok(None),
            Err(other) => return Err(other),
        }

        if !kernel.current_task_capability_has_right(recv_cap, CapRights::RECEIVE) {
            return Ok(None);
        }

        #[cfg(test)]
        {
            if self
                .test_disable_budgeted_receive_for_tracked_tid
                .is_some_and(|tid| {
                    self.find_record(tid)
                        .is_some_and(|record| record.pending_restart_due.is_some())
                })
            {
                return Ok(None);
            }
        }

        match kernel.ipc_recv_with_deadline(recv_cap, SUPERVISOR_RECV_BUDGET_TICKS) {
            Ok(msg) => Ok(msg),
            Err(KernelError::TaskMissing)
            | Err(KernelError::InvalidCapability)
            | Err(KernelError::MissingRight) => Ok(None),
            Err(other) => Err(other),
        }
    }
}
