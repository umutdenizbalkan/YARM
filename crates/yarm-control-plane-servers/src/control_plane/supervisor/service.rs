// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::control_plane::init::{
    CoreServiceKind, CoreServicePolicyTable, InitFaultHandoff, RestartOwner, ServiceRestartPolicy,
};
#[cfg(test)]
use yarm::kernel::boot::{DriverBundlePlan, KernelError, KernelState};
#[cfg(not(test))]
use yarm_ipc_abi::process_abi::{
    ExecuteRestartReply, ExecuteRestartRequest, LifecycleQueryReply, LifecycleQueryRequest,
    PROC_OP_EXECUTE_RESTART, PROC_OP_LIFECYCLE_QUERY, PROC_OP_PM_RESTART_V1,
    PROC_OP_TASK_RESTART_TOKEN, TaskRestartTokenReply, TaskRestartTokenRequest,
    decode_pm_restart_reply_v1, encode_pm_restart_request_v1,
};
use yarm_ipc_abi::process_abi::{
    PM_RESTART_REPLY_V1_LEN, PROC_OP_PM_RESTART_REPLY_V1, PmRestartFailure, PmRestartReason,
    PmRestartReplyStatus, PmRestartRequestV1, PmRestartTokenDescriptor,
};
#[cfg(not(test))]
use yarm_ipc_abi::recv_shared_v3_abi::RecvSharedV3Output;
use yarm_ipc_abi::supervisor_abi::{
    CoreServiceRegistrationKind, RedelegationAckRequest, RegisterCoreServiceRequest,
    RegisterDriverRequest, SUPERVISOR_OP_ACK_REDELEGATION, SUPERVISOR_OP_QUERY_STATUS,
    SUPERVISOR_OP_REGISTER_CORE_SERVICE, SUPERVISOR_OP_REGISTER_DRIVER,
    SUPERVISOR_OP_TRANSFER_REVOKED, SupervisorStatusRequest, TransferRevokedEvent,
};
use yarm_ipc_abi::supervisor_abi::{
    DEP_PROCESS_MANAGER, DEP_SUPERVISOR, DEP_VFS, InitAlert, InitAlertKind,
    SUPERVISOR_OP_TASK_EXITED, SupervisorStatusReply, TaskExitedEvent,
};
use yarm_user_rt::capability::CapId;
#[cfg(test)]
use yarm_user_rt::capability::CapRights;
use yarm_user_rt::ipc::Message;
#[cfg(test)]
use yarm_user_rt::ipc::ThreadId;
#[cfg(not(test))]
use yarm_user_rt::runtime::{KernelIpcError as KernelError, StartupContext, startup_context};
#[cfg(not(test))]
use yarm_user_rt::syscall::recv_v3::ipc_recv_shared_v3_nonblocking;
#[cfg(not(test))]
use yarm_user_rt::syscall::{IpcTransport, SyscallIpcTransport};
#[cfg(test)]
use yarm_user_rt::task::{TaskClass, TaskStatus};
use yarm_user_rt::time::{TickDuration, TickInstant};

#[cfg(any(test, feature = "hosted-dev"))]
#[path = "restart_model.rs"]
mod restart_model;
#[cfg(any(test, feature = "hosted-dev"))]
#[allow(unused_imports)]
pub(crate) use restart_model::*;

const SUPERVISOR_FAULT_REPORT_WIRE_LEN: usize = 17;
#[cfg(not(test))]
const SUPERVISOR_SHORT_RECV_TIMEOUT_TICKS: u64 = 1;
const SUPERVISOR_FAULT_REPORT_TID_START: usize = 0;
const SUPERVISOR_FAULT_REPORT_TID_END: usize = 8;
const SUPERVISOR_FAULT_REPORT_ADDR_START: usize = 8;
const SUPERVISOR_FAULT_REPORT_ADDR_END: usize = 16;
const SUPERVISOR_FAULT_REPORT_ACCESS_INDEX: usize = 16;
const SUPERVISOR_FAULT_EXIT_CODE_TAG: u64 = 0xF000_0000_0000_0000u64;
const SUPERVISOR_FAULT_EXIT_CODE_ACCESS_SHIFT: u64 = 56;
const SUPERVISOR_FAULT_EXIT_CODE_ADDR_MASK: u64 = 0x00FF_FFFF_FFFF_FFFF;

fn supervisor_restart_test_build_gate_enabled() -> bool {
    option_env!("YARM_SUPERVISOR_RESTART_TEST") == Some("1")
        || option_env!("SUPERVISOR_RESTART_TEST") == Some("1")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultAccess {
    Read = 0,
    Write = 1,
    Execute = 2,
}

impl FaultAccess {
    fn decode(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Read),
            1 => Some(Self::Write),
            2 => Some(Self::Execute),
            _ => None,
        }
    }

    const fn wire(self) -> u8 {
        self as u8
    }
}
/// Kernel-originated supervisor fault-report notification opcode.
///
/// The kernel fault path uses `Message::new(0, payload)` for the 17-byte fault report wire payload.
const SUPERVISOR_OP_FAULT_REPORT_WIRE: u16 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SupervisorFaultReportWire {
    tid: u64,
    fault_addr: u64,
    access: FaultAccess,
}

impl SupervisorFaultReportWire {
    fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != SUPERVISOR_FAULT_REPORT_WIRE_LEN {
            return None;
        }
        let mut tid = [0u8; 8];
        let mut fault_addr = [0u8; 8];
        tid.copy_from_slice(
            &bytes[SUPERVISOR_FAULT_REPORT_TID_START..SUPERVISOR_FAULT_REPORT_TID_END],
        );
        fault_addr.copy_from_slice(
            &bytes[SUPERVISOR_FAULT_REPORT_ADDR_START..SUPERVISOR_FAULT_REPORT_ADDR_END],
        );
        let access = FaultAccess::decode(bytes[SUPERVISOR_FAULT_REPORT_ACCESS_INDEX])?;
        Some(Self {
            tid: u64::from_le_bytes(tid),
            fault_addr: u64::from_le_bytes(fault_addr),
            access,
        })
    }

    fn encode(self) -> [u8; SUPERVISOR_FAULT_REPORT_WIRE_LEN] {
        let mut bytes = [0u8; SUPERVISOR_FAULT_REPORT_WIRE_LEN];
        bytes[SUPERVISOR_FAULT_REPORT_TID_START..SUPERVISOR_FAULT_REPORT_TID_END]
            .copy_from_slice(&self.tid.to_le_bytes());
        bytes[SUPERVISOR_FAULT_REPORT_ADDR_START..SUPERVISOR_FAULT_REPORT_ADDR_END]
            .copy_from_slice(&self.fault_addr.to_le_bytes());
        bytes[SUPERVISOR_FAULT_REPORT_ACCESS_INDEX] = self.access.wire();
        bytes
    }

    fn synthetic_exit_code(self) -> u64 {
        // Preserve existing supervisor restart flow by translating fault reports into
        // a stable synthetic exit code domain.
        SUPERVISOR_FAULT_EXIT_CODE_TAG
            | ((self.access.wire() as u64) << SUPERVISOR_FAULT_EXIT_CODE_ACCESS_SHIFT)
            | (self.fault_addr & SUPERVISOR_FAULT_EXIT_CODE_ADDR_MASK)
    }
}

const MAX_MANAGED_SERVICES: usize = 8;
const SUPERVISOR_PENDING_FAULT_CAPACITY: usize = 4;
#[cfg(not(test))]
const SUPERVISOR_PENDING_FAULT_MAX_AGE_TICKS: u64 = 8;
const MAX_DEPENDENTS: usize = 8;
#[cfg(test)]
const SUPERVISOR_RECV_BUDGET_TICKS: u64 = 1;
const SUPERVISOR_QUERY_STATUS_CALL_RECV_TIMEOUT_TICKS: u64 = 1;
#[cfg(not(test))]
const SUPERVISOR_RUNTIME_DEFAULT_RESTART_WINDOW_TICKS: u64 = 100;
#[cfg(not(test))]
const SUPERVISOR_RUNTIME_IDLE_RECV_TIMEOUT_TICKS: u64 = 1;
const SUPERVISOR_PM_RESTART_REPLACEMENT_HANDLE_KIND_TASK_TID: u16 = 1;
const SUPERVISOR_PM_RESTART_REPLY_CAP_OPCODE: u16 = 0;
const SUPERVISOR_PM_RESOURCE_UNAVAILABLE_RETRY_MAX: u8 = 3;
const SUPERVISOR_CRASH_TEST_RESTART_TOKEN_TAG: u64 = 0x4352_4153_4854_0000;

fn supervisor_crash_test_restart_token_for_tid(tid: u64) -> u64 {
    SUPERVISOR_CRASH_TEST_RESTART_TOKEN_TAG | (tid & 0xffff)
}

#[cfg(not(test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SupervisorRuntimeHandoff {
    pub supervisor_tid: Option<u64>,
    pub init_tid: Option<u64>,
    pub supervisor_fault_recv_ep: Option<u32>,
    pub supervisor_control_send_ep: Option<u32>,
    pub supervisor_control_recv_ep: Option<u32>,
    pub init_alert_send_ep: Option<u32>,
    pub init_alert_recv_ep: Option<u32>,
    pub restart_window_ticks: u64,
}

#[cfg(not(test))]
impl SupervisorRuntimeHandoff {
    pub fn from_startup_context(ctx: StartupContext) -> Self {
        yarm_user_rt::user_log!(
            "SUP_HANDOFF_FAULT_RECV cap={:?}",
            ctx.supervisor_fault_recv_ep
        );
        yarm_user_rt::user_log!(
            "SUP_HANDOFF_CONTROL_SEND cap={:?}",
            ctx.supervisor_control_send_ep
        );
        yarm_user_rt::user_log!(
            "SUP_HANDOFF_CONTROL_RECV cap={:?}",
            ctx.supervisor_control_recv_ep
        );
        Self {
            supervisor_tid: ctx.supervisor_tid.or(Some(ctx.task_id)),
            init_tid: ctx.init_tid,
            supervisor_fault_recv_ep: ctx.supervisor_fault_recv_ep,
            supervisor_control_send_ep: ctx.supervisor_control_send_ep,
            supervisor_control_recv_ep: ctx.supervisor_control_recv_ep,
            init_alert_send_ep: ctx.init_alert_send_ep,
            init_alert_recv_ep: ctx.init_alert_recv_ep,
            restart_window_ticks: ctx
                .supervisor_restart_window_ticks
                .unwrap_or(SUPERVISOR_RUNTIME_DEFAULT_RESTART_WINDOW_TICKS),
        }
    }

    fn into_fault_handoff(self) -> Result<(u64, InitFaultHandoff), KernelError> {
        let supervisor_tid = self.supervisor_tid.ok_or(KernelError::InvalidCapability)?;
        let init_tid = self.init_tid.ok_or(KernelError::InvalidCapability)?;
        let fault_recv = self
            .supervisor_fault_recv_ep
            .ok_or(KernelError::InvalidCapability)?;
        let control_send = self
            .supervisor_control_send_ep
            .ok_or(KernelError::InvalidCapability)?;
        let control_recv = self
            .supervisor_control_recv_ep
            .ok_or(KernelError::InvalidCapability)?;
        let init_alert_send = self.init_alert_send_ep.unwrap_or(0);
        let init_alert_recv = self.init_alert_recv_ep.unwrap_or(0);
        Ok((
            init_tid,
            InitFaultHandoff::new(
                supervisor_tid,
                CapId(fault_recv as u64),
                CapId(control_send as u64),
                CapId(control_recv as u64),
                CapId(init_alert_send as u64),
                CapId(init_alert_recv as u64),
                self.restart_window_ticks,
            ),
        ))
    }
}

#[cfg(test)]
fn map_task_status(status: yarm::kernel::task::TaskStatus) -> TaskStatus {
    match status {
        yarm::kernel::task::TaskStatus::Runnable => TaskStatus::Runnable,
        yarm::kernel::task::TaskStatus::Running => TaskStatus::Running,
        yarm::kernel::task::TaskStatus::Blocked(_) => TaskStatus::Blocked,
        yarm::kernel::task::TaskStatus::Faulted => TaskStatus::Faulted,
        yarm::kernel::task::TaskStatus::Exited(code) => TaskStatus::Exited(code),
        yarm::kernel::task::TaskStatus::Dead => TaskStatus::Dead,
    }
}

#[cfg(test)]
fn map_task_class(class: yarm::kernel::task::TaskClass) -> TaskClass {
    match class {
        yarm::kernel::task::TaskClass::App => TaskClass::App,
        yarm::kernel::task::TaskClass::Driver => TaskClass::Driver,
        yarm::kernel::task::TaskClass::SystemServer => TaskClass::SystemServer,
    }
}

#[cfg(test)]
fn to_kernel_task_class(class: TaskClass) -> yarm::kernel::task::TaskClass {
    match class {
        TaskClass::App => yarm::kernel::task::TaskClass::App,
        TaskClass::Driver => yarm::kernel::task::TaskClass::Driver,
        TaskClass::SystemServer => yarm::kernel::task::TaskClass::SystemServer,
    }
}

fn init_alert_message(sender_tid: u64, alert: InitAlert) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        yarm_ipc_abi::supervisor_abi::SUPERVISOR_OP_INIT_ALERT,
        0,
        None,
        &alert.encode(),
    )
    .map_err(|_| ())
}

fn status_reply_message(sender_tid: u64, reply: SupervisorStatusReply) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_QUERY_STATUS,
        0,
        None,
        &reply.encode(),
    )
    .map_err(|_| ())
}

#[cfg(test)]
fn query_status_message(sender_tid: u64, request: SupervisorStatusRequest) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_QUERY_STATUS,
        0,
        None,
        &request.encode(),
    )
    .map_err(|_| ())
}

#[cfg(test)]
fn transfer_revoked_message(sender_tid: u64, event: TransferRevokedEvent) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_TRANSFER_REVOKED,
        0,
        None,
        &event.encode(),
    )
    .map_err(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedServiceKind {
    Core(CoreServiceKind),
    Driver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorDecision {
    ScheduledRestart {
        tid: u64,
        kind: ManagedServiceKind,
        due_tick: TickInstant,
        redelegated: bool,
    },
    MarkedDead {
        tid: u64,
        kind: ManagedServiceKind,
    },
    Ignored {
        tid: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorEvent {
    Control(Message),
    Fault(Message),
    Tick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SupervisorStepOutcome {
    pub handled: usize,
    pub restarts_executed: usize,
    pub tick_advanced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DriverRecoveryPlan {
    irq_line: u16,
    mem_cap: CapId,
    dma_len: usize,
    iova_cap: CapId,
    iova_base: usize,
    iova_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum SupervisorRestartReason {
    Fault,
    NormalExit,
    Dependency { cause_tid: u64 },
    ManualPolicy,
}

impl SupervisorRestartReason {
    const fn dependency_cause_tid(self) -> u64 {
        match self {
            Self::Dependency { cause_tid } => cause_tid,
            _ => 0,
        }
    }

    const fn as_pm_reason(self) -> PmRestartReason {
        match self {
            Self::Fault => PmRestartReason::Fault,
            Self::NormalExit => PmRestartReason::NormalExit,
            Self::Dependency { .. } => PmRestartReason::DependencyFailed,
            Self::ManualPolicy => PmRestartReason::ManualPolicy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorPmRestartState {
    PendingDue,
    BlockedNoPmClient,
    PmDeferred,
    PmRejected,
    PmClientSendFailed,
    ProtocolViolation,
    AwaitingMechanismUnavailable,
    RestartAccepted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SupervisorPmRestartClientRequest {
    request_id: u64,
    supervisor_tid: u64,
    target_tid: u64,
    service_kind: u16,
    service_name: &'static [u8],
    reason: SupervisorRestartReason,
    attempt_count: u16,
    due_tick: u64,
    degraded_hint: bool,
    policy_flags: u32,
    token_owner_tid: u64,
    token_fingerprint: u16,
    accepted_reply_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SupervisorPmRestartClientResult {
    Deferred {
        failure: PmRestartFailure,
        retry_tick: u64,
    },
    Rejected {
        failure: PmRestartFailure,
    },
    Accepted {
        replacement_handle_kind: u16,
        replacement_handle_value: u64,
    },
    ProtocolViolationAccepted,
    MalformedReply,
    SendFailed,
    NoPmClient,
    BuildFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManagedServiceRecord {
    tid: u64,
    kind: ManagedServiceKind,
    restart_attempts: u8,
    restart_group: u8,
    dependency_mask: u8,
    last_exit_code: u64,
    last_exit_tick: TickInstant,
    last_restart_tick: TickInstant,
    window_start_tick: TickInstant,
    pending_restart_due: Option<TickInstant>,
    pending_restart_token: Option<u64>,
    restart_blocked_no_pm_client: bool,
    restart_deferred_by_pm: bool,
    restart_rejected_by_pm: bool,
    restart_deferred_no_pm_client_logged: bool,
    pm_restart_state: SupervisorPmRestartState,
    pending_restart_reason: SupervisorRestartReason,
    pending_pm_request_id: Option<u64>,
    pm_resource_unavailable_retries: u8,
    pending_redelegation: bool,
    driver_policy: Option<ServiceRestartPolicy>,
    driver_plan: Option<DriverRecoveryPlan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingSupervisorFault {
    fault: SupervisorFaultReportWire,
    stashed_tick: TickInstant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorService {
    init_tid: u64,
    handoff: InitFaultHandoff,
    policies: CoreServicePolicyTable,
    managed: [Option<ManagedServiceRecord>; MAX_MANAGED_SERVICES],
    pending_faults: [Option<PendingSupervisorFault>; SUPERVISOR_PENDING_FAULT_CAPACITY],
    degraded: bool,
    current_tick: TickInstant,
    next_pm_restart_request_id: u64,
    pm_restart_acceptance_enabled: bool,
    #[cfg(test)]
    test_disable_budgeted_receive_for_tracked_tid: Option<u64>,
}

pub trait SupervisorOutboundMessageOps {
    fn ipc_send(&mut self, cap: CapId, msg: Message) -> Result<(), KernelError>;
    fn ipc_reply(&mut self, cap: CapId, msg: Message) -> Result<(), KernelError>;
}

pub(crate) trait SupervisorRestartRedelegationOps: SupervisorOutboundMessageOps {
    fn restart_task(
        &mut self,
        request: SupervisorPmRestartClientRequest,
    ) -> SupervisorPmRestartClientResult;
    #[allow(dead_code)]
    fn delegate_driver_bundle(
        &mut self,
        server_tid: u64,
        plan: DriverRecoveryPlan,
    ) -> Result<(), KernelError>;
}

pub trait SupervisorTaskExitOps: SupervisorOutboundMessageOps {
    fn mark_task_dead(&mut self, tid: u64) -> Result<(), KernelError>;
    fn task_restart_token(&self, tid: u64) -> Option<u64>;
}

#[cfg(test)]
struct KernelSupervisorOutboundMessageOps<'a> {
    kernel: &'a mut KernelState,
}

#[cfg(test)]
impl<'a> KernelSupervisorOutboundMessageOps<'a> {
    fn new(kernel: &'a mut KernelState) -> Self {
        Self { kernel }
    }
}

#[cfg(test)]
impl SupervisorOutboundMessageOps for KernelSupervisorOutboundMessageOps<'_> {
    fn ipc_send(&mut self, cap: CapId, msg: Message) -> Result<(), KernelError> {
        self.kernel.ipc_send(cap, msg)
    }

    fn ipc_reply(&mut self, cap: CapId, msg: Message) -> Result<(), KernelError> {
        self.kernel.ipc_reply(cap, msg)
    }
}

#[cfg(test)]
impl SupervisorRestartRedelegationOps for KernelSupervisorOutboundMessageOps<'_> {
    fn restart_task(
        &mut self,
        _request: SupervisorPmRestartClientRequest,
    ) -> SupervisorPmRestartClientResult {
        SupervisorPmRestartClientResult::SendFailed
    }

    fn delegate_driver_bundle(
        &mut self,
        server_tid: u64,
        plan: DriverRecoveryPlan,
    ) -> Result<(), KernelError> {
        let _ = self.kernel.delegate_driver_bundle(DriverBundlePlan {
            server_tid: ThreadId(server_tid),
            irq_line: plan.irq_line,
            mem_cap: plan.mem_cap,
            dma_len: plan.dma_len,
            iova_cap: plan.iova_cap,
            iova_base: plan.iova_base,
            iova_len: plan.iova_len,
        })?;
        Ok(())
    }
}

#[cfg(test)]
impl SupervisorTaskExitOps for KernelSupervisorOutboundMessageOps<'_> {
    fn mark_task_dead(&mut self, tid: u64) -> Result<(), KernelError> {
        self.kernel.mark_task_dead(tid)
    }

    fn task_restart_token(&self, tid: u64) -> Option<u64> {
        self.kernel.task_restart_token(tid)
    }
}

impl SupervisorService {
    pub const fn new(
        init_tid: u64,
        handoff: InitFaultHandoff,
        policies: CoreServicePolicyTable,
    ) -> Self {
        Self {
            init_tid,
            handoff,
            policies,
            managed: [None; MAX_MANAGED_SERVICES],
            pending_faults: [None; SUPERVISOR_PENDING_FAULT_CAPACITY],
            degraded: false,
            current_tick: TickInstant(0),
            next_pm_restart_request_id: 1,
            pm_restart_acceptance_enabled: false,
            #[cfg(test)]
            test_disable_budgeted_receive_for_tracked_tid: None,
        }
    }

    #[cfg(not(test))]
    pub fn new_from_runtime_handoff(
        runtime_handoff: SupervisorRuntimeHandoff,
    ) -> Result<Self, KernelError> {
        let (init_tid, handoff) = runtime_handoff.into_fault_handoff()?;
        let mut service = Self::new(init_tid, handoff, CoreServicePolicyTable::baseline());
        if supervisor_restart_test_build_gate_enabled() {
            service.pm_restart_acceptance_enabled = true;
            yarm_user_rt::user_log!("SUPERVISOR_RESTART_TEST_GATE_ON");
        }
        Ok(service)
    }

    pub const fn degraded(&self) -> bool {
        self.degraded
    }

    pub const fn current_tick(&self) -> TickInstant {
        self.current_tick
    }

    #[cfg(test)]
    fn enable_sup_l4_pm_restart_acceptance_for_tests(&mut self) {
        self.pm_restart_acceptance_enabled = true;
    }

    pub fn advance_ticks(&mut self, delta: TickDuration) {
        self.current_tick = self.current_tick + delta;
    }

    fn send_init_message(
        &mut self,
        outbound_ops: &mut impl SupervisorOutboundMessageOps,
        msg: Message,
    ) -> Result<(), KernelError> {
        outbound_ops.ipc_send(self.handoff.init_alert_send_cap, msg)
    }

    fn send_init_alert(
        &mut self,
        outbound_ops: &mut impl SupervisorOutboundMessageOps,
        alert: InitAlert,
    ) -> Result<(), KernelError> {
        let msg = init_alert_message(self.init_tid, alert).map_err(|_| KernelError::WrongObject)?;
        self.send_init_message(outbound_ops, msg)
    }

    fn send_status_reply(
        &mut self,
        outbound_ops: &mut impl SupervisorOutboundMessageOps,
        reply: SupervisorStatusReply,
        reply_cap: Option<CapId>,
    ) -> Result<(), KernelError> {
        let msg =
            status_reply_message(self.init_tid, reply).map_err(|_| KernelError::WrongObject)?;
        if let Some(reply_cap) = reply_cap {
            outbound_ops.ipc_reply(reply_cap, msg)
        } else {
            self.send_init_message(outbound_ops, msg)
        }
    }

    fn register_record(&mut self, record: ManagedServiceRecord) -> Result<(), KernelError> {
        if let Some(slot) = self.managed.iter_mut().find(|slot| {
            slot.is_none()
                || slot
                    .as_ref()
                    .map(|existing| existing.tid == record.tid)
                    .unwrap_or(false)
        }) {
            *slot = Some(record);
            Ok(())
        } else {
            Err(KernelError::TaskTableFull)
        }
    }

    pub fn register_core_service(
        &mut self,
        kind: CoreServiceKind,
        tid: u64,
        restart_group: u8,
        dependency_mask: u8,
    ) -> Result<(), KernelError> {
        #[cfg(not(test))]
        yarm_user_rt::user_log!("SUPERVISOR_MANAGED_RECORD_REGISTER_BEGIN tid={}", tid);
        self.register_record(ManagedServiceRecord {
            tid,
            kind: ManagedServiceKind::Core(kind),
            restart_attempts: 0,
            restart_group,
            dependency_mask,
            last_exit_code: 0,
            last_exit_tick: TickInstant(0),
            last_restart_tick: TickInstant(0),
            window_start_tick: TickInstant(0),
            pending_restart_due: None,
            pending_restart_token: None,
            restart_blocked_no_pm_client: false,
            restart_deferred_by_pm: false,
            restart_rejected_by_pm: false,
            restart_deferred_no_pm_client_logged: false,
            pm_restart_state: SupervisorPmRestartState::BlockedNoPmClient,
            pending_restart_reason: SupervisorRestartReason::Fault,
            pending_pm_request_id: None,
            pm_resource_unavailable_retries: 0,
            pending_redelegation: false,
            driver_policy: None,
            driver_plan: None,
        })?;
        #[cfg(not(test))]
        yarm_user_rt::user_log!("SUPERVISOR_MANAGED_RECORD_REGISTER_OK tid={}", tid);
        Ok(())
    }

    pub(crate) fn register_driver(
        &mut self,
        tid: u64,
        policy: ServiceRestartPolicy,
        restart_group: u8,
        dependency_mask: u8,
        plan: DriverRecoveryPlan,
    ) -> Result<(), KernelError> {
        #[cfg(not(test))]
        yarm_user_rt::user_log!("SUPERVISOR_MANAGED_RECORD_REGISTER_BEGIN tid={}", tid);
        self.register_record(ManagedServiceRecord {
            tid,
            kind: ManagedServiceKind::Driver,
            restart_attempts: 0,
            restart_group,
            dependency_mask,
            last_exit_code: 0,
            last_exit_tick: TickInstant(0),
            last_restart_tick: TickInstant(0),
            window_start_tick: TickInstant(0),
            pending_restart_due: None,
            pending_restart_token: None,
            restart_blocked_no_pm_client: false,
            restart_deferred_by_pm: false,
            restart_rejected_by_pm: false,
            restart_deferred_no_pm_client_logged: false,
            pm_restart_state: SupervisorPmRestartState::BlockedNoPmClient,
            pending_restart_reason: SupervisorRestartReason::Fault,
            pending_pm_request_id: None,
            pm_resource_unavailable_retries: 0,
            pending_redelegation: false,
            driver_policy: Some(policy),
            driver_plan: Some(plan),
        })?;
        #[cfg(not(test))]
        yarm_user_rt::user_log!("SUPERVISOR_MANAGED_RECORD_REGISTER_OK tid={}", tid);
        Ok(())
    }

    fn find_record(&self, tid: u64) -> Option<ManagedServiceRecord> {
        self.managed
            .iter()
            .flatten()
            .find(|record| record.tid == tid)
            .copied()
    }

    fn find_record_mut(&mut self, tid: u64) -> Option<&mut ManagedServiceRecord> {
        self.managed
            .iter_mut()
            .flatten()
            .find(|record| record.tid == tid)
    }

    #[cfg(not(test))]
    fn has_managed_records(&self) -> bool {
        self.managed.iter().any(Option::is_some)
    }

    fn stash_pending_fault(&mut self, fault: SupervisorFaultReportWire) -> bool {
        if self
            .pending_faults
            .iter()
            .flatten()
            .any(|pending| pending.fault.tid == fault.tid)
        {
            #[cfg(not(test))]
            yarm_user_rt::user_log!(
                "SUPERVISOR_FAULT_PENDING_STASH tid={} reason=record_not_ready",
                fault.tid
            );
            return true;
        }
        if let Some(slot) = self.pending_faults.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(PendingSupervisorFault {
                fault,
                stashed_tick: self.current_tick,
            });
            #[cfg(not(test))]
            yarm_user_rt::user_log!(
                "SUPERVISOR_FAULT_PENDING_STASH tid={} reason=record_not_ready",
                fault.tid
            );
            true
        } else {
            #[cfg(not(test))]
            yarm_user_rt::user_log!(
                "SUPERVISOR_FAULT_PENDING_DROP tid={} reason=overflow",
                fault.tid
            );
            false
        }
    }

    fn take_pending_fault_for_registered_tid(
        &mut self,
        tid: u64,
    ) -> Option<SupervisorFaultReportWire> {
        if self.find_record(tid).is_none() {
            return None;
        }
        let slot = self.pending_faults.iter_mut().find(|slot| {
            slot.map(|pending| pending.fault.tid == tid)
                .unwrap_or(false)
        })?;
        slot.take().map(|pending| pending.fault)
    }

    #[cfg(not(test))]
    fn drop_stale_pending_faults(&mut self) {
        for idx in 0..self.pending_faults.len() {
            let Some(pending) = self.pending_faults[idx] else {
                continue;
            };
            if TickDuration(SUPERVISOR_PENDING_FAULT_MAX_AGE_TICKS)
                .has_elapsed_since(pending.stashed_tick, self.current_tick)
                && self.find_record(pending.fault.tid).is_none()
            {
                yarm_user_rt::user_log!(
                    "SUPERVISOR_FAULT_PENDING_DROP tid={} reason=stale",
                    pending.fault.tid
                );
                self.pending_faults[idx] = None;
            }
        }
    }

    fn dependency_bit(kind: ManagedServiceKind) -> u8 {
        match kind {
            ManagedServiceKind::Core(CoreServiceKind::ProcessManager) => DEP_PROCESS_MANAGER,
            ManagedServiceKind::Core(CoreServiceKind::Vfs) => DEP_VFS,
            ManagedServiceKind::Core(CoreServiceKind::Supervisor) => DEP_SUPERVISOR,
            ManagedServiceKind::Driver => 0,
        }
    }

    fn policy_for(&self, record: ManagedServiceRecord) -> ServiceRestartPolicy {
        match record.kind {
            ManagedServiceKind::Core(kind) => self.policies.policy_for(kind),
            ManagedServiceKind::Driver => record.driver_policy.unwrap_or(ServiceRestartPolicy {
                max_restarts: 2,
                backoff_ticks: 5,
            }),
        }
    }

    fn dependent_tids(&self, failed: ManagedServiceRecord) -> [Option<u64>; MAX_DEPENDENTS] {
        let mut out = [None; MAX_DEPENDENTS];
        let failed_bit = Self::dependency_bit(failed.kind);
        let mut count = 0usize;
        if failed_bit == 0 {
            return out;
        }
        let mut idx = 0;
        while idx < self.managed.len() && count < out.len() {
            if let Some(record) = self.managed[idx] {
                if record.tid != failed.tid
                    && record.restart_group == failed.restart_group
                    && (record.dependency_mask & failed_bit) != 0
                {
                    out[count] = Some(record.tid);
                    count += 1;
                }
            }
            idx += 1;
        }
        out
    }

    fn next_pm_restart_request_id(&mut self) -> Result<u64, KernelError> {
        let request_id = self.next_pm_restart_request_id;
        self.next_pm_restart_request_id = self
            .next_pm_restart_request_id
            .checked_add(1)
            .ok_or(KernelError::WrongObject)?;
        Ok(request_id)
    }

    const fn service_kind_code(kind: ManagedServiceKind) -> u16 {
        match kind {
            ManagedServiceKind::Core(CoreServiceKind::ProcessManager) => 1,
            ManagedServiceKind::Core(CoreServiceKind::Vfs) => 2,
            ManagedServiceKind::Core(CoreServiceKind::Supervisor) => 3,
            ManagedServiceKind::Driver => 100,
        }
    }

    const fn service_name_bytes(kind: ManagedServiceKind) -> &'static [u8] {
        match kind {
            ManagedServiceKind::Core(CoreServiceKind::ProcessManager) => b"process_manager",
            ManagedServiceKind::Core(CoreServiceKind::Vfs) => b"vfs",
            ManagedServiceKind::Core(CoreServiceKind::Supervisor) => b"supervisor",
            ManagedServiceKind::Driver => b"driver",
        }
    }

    fn schedule_restart_with_reason(
        &mut self,
        tid: u64,
        token: u64,
        reason: SupervisorRestartReason,
    ) -> Result<TickInstant, KernelError> {
        let snapshot = self.find_record(tid).ok_or(KernelError::TaskMissing)?;
        let policy = self.policy_for(snapshot);
        let restart_window_ticks = self.handoff.restart_window_ticks;
        let current_tick = self.current_tick;
        let record = self.find_record_mut(tid).ok_or(KernelError::TaskMissing)?;
        if TickDuration(restart_window_ticks)
            .has_elapsed_since(record.window_start_tick, current_tick)
        {
            record.window_start_tick = current_tick;
            record.restart_attempts = 0;
        }
        let old_attempt = record.restart_attempts;
        record.restart_attempts = record.restart_attempts.saturating_add(1);
        #[cfg(not(test))]
        yarm_user_rt::user_log!(
            "SUPERVISOR_RESTART_ATTEMPT_ADVANCE old={} new={}",
            old_attempt,
            record.restart_attempts
        );
        record.pending_restart_due = Some(current_tick + TickDuration(policy.backoff_ticks));
        record.pending_restart_token = Some(token);
        record.restart_blocked_no_pm_client = false;
        record.restart_deferred_by_pm = false;
        record.restart_rejected_by_pm = false;
        record.restart_deferred_no_pm_client_logged = false;
        record.pm_restart_state = SupervisorPmRestartState::PendingDue;
        record.pending_restart_reason = reason;
        record.pending_pm_request_id = None;
        record.pm_resource_unavailable_retries = 0;
        #[cfg(not(test))]
        yarm_user_rt::user_log!(
            "SUPERVISOR_RESTART_SCHEDULED tid={} due_tick={} attempt={} reason={:?} dependency_cause_tid={}",
            tid,
            record.pending_restart_due.expect("due set").0,
            record.restart_attempts,
            record.pending_restart_reason.as_pm_reason(),
            record.pending_restart_reason.dependency_cause_tid()
        );
        #[cfg(not(test))]
        yarm_user_rt::user_log!(
            "SUPERVISOR_RESTART_SCHEDULED attempt={} max={}",
            record.restart_attempts,
            policy.max_restarts
        );
        Ok(record.pending_restart_due.expect("due set"))
    }

    fn handle_control_request(
        &mut self,
        outbound_ops: &mut impl SupervisorOutboundMessageOps,
        request: Message,
    ) -> Result<(), KernelError> {
        self.validate_control_sender(&request)?;
        #[cfg(not(test))]
        yarm_user_rt::user_log!(
            "SUPERVISOR_CONTROL_SENDER_OK sender={}",
            request.sender_tid.0
        );
        #[cfg(not(test))]
        yarm_user_rt::user_log!("SUPERVISOR_CONTROL_DISPATCH opcode={}", request.opcode);
        match request.opcode {
            SUPERVISOR_OP_REGISTER_CORE_SERVICE => {
                let req = RegisterCoreServiceRequest::decode(request.as_slice())
                    .ok_or(KernelError::WrongObject)?;
                let kind = match req.kind {
                    CoreServiceRegistrationKind::ProcessManager => CoreServiceKind::ProcessManager,
                    CoreServiceRegistrationKind::Vfs => CoreServiceKind::Vfs,
                    CoreServiceRegistrationKind::Supervisor => CoreServiceKind::Supervisor,
                };
                self.register_core_service(kind, req.tid, req.restart_group, req.dependency_mask)?;
                match kind {
                    CoreServiceKind::ProcessManager => {
                        self.policies.process_manager = ServiceRestartPolicy {
                            max_restarts: req.max_restarts,
                            backoff_ticks: req.backoff_ticks,
                        };
                    }
                    CoreServiceKind::Vfs => {
                        self.policies.vfs = ServiceRestartPolicy {
                            max_restarts: req.max_restarts,
                            backoff_ticks: req.backoff_ticks,
                        };
                    }
                    CoreServiceKind::Supervisor => {
                        self.policies.supervisor = ServiceRestartPolicy {
                            max_restarts: req.max_restarts,
                            backoff_ticks: req.backoff_ticks,
                        };
                    }
                }
            }
            SUPERVISOR_OP_REGISTER_DRIVER => {
                let req = match RegisterDriverRequest::decode(request.as_slice()) {
                    Some(req) => req,
                    None => {
                        #[cfg(not(test))]
                        if supervisor_restart_test_build_gate_enabled() {
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_CRASH_TEST_REGISTER_FAIL tid=0 reason=decode"
                            );
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_CONTROL_WRONG_OBJECT site=register-driver-decode opcode={} reason=payload-decode",
                                request.opcode
                            );
                        }
                        return Err(KernelError::WrongObject);
                    }
                };
                let crash_test_registration = supervisor_restart_test_build_gate_enabled()
                    && req.max_restarts == 3
                    && req.restart_group == 13;
                #[cfg(not(test))]
                if crash_test_registration {
                    yarm_user_rt::user_log!("SUPERVISOR_CRASH_TEST_REGISTER_BEGIN tid={}", req.tid);
                }
                if let Err(err) = self.register_driver(
                    req.tid,
                    ServiceRestartPolicy {
                        max_restarts: req.max_restarts,
                        backoff_ticks: req.backoff_ticks,
                    },
                    req.restart_group,
                    req.dependency_mask,
                    DriverRecoveryPlan {
                        irq_line: req.irq_line,
                        mem_cap: CapId(req.mem_cap),
                        dma_len: req.dma_len as usize,
                        iova_cap: CapId(req.iova_cap),
                        iova_base: req.iova_base as usize,
                        iova_len: req.iova_len as usize,
                    },
                ) {
                    #[cfg(not(test))]
                    if crash_test_registration {
                        yarm_user_rt::user_log!(
                            "SUPERVISOR_CRASH_TEST_REGISTER_FAIL tid={} reason={:?}",
                            req.tid,
                            err
                        );
                    }
                    return Err(err);
                }
                if crash_test_registration {
                    let restart_token = supervisor_crash_test_restart_token_for_tid(req.tid);
                    if let Some(record) = self.find_record_mut(req.tid) {
                        record.pending_restart_token = Some(restart_token);
                    }
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_CRASH_TEST_REGISTER_OK tid={} max_restarts=3",
                        req.tid
                    );
                    yarm_user_rt::user_log!("SUPERVISOR_CRASH_TEST_RECORD_READY tid={}", req.tid);
                    yarm_user_rt::user_log!("SUPERVISOR_CRASH_TEST_POLICY max_restarts=3");
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_CRASH_TEST_RESTART_TOKEN_READY tid={} fingerprint={}",
                        req.tid,
                        (restart_token & 0xffff) as u16
                    );
                }
            }
            SUPERVISOR_OP_QUERY_STATUS => {
                let req = SupervisorStatusRequest::decode(request.as_slice())
                    .ok_or(KernelError::WrongObject)?;
                let record = self.find_record(req.tid).ok_or(KernelError::TaskMissing)?;
                let reply_cap = request.transferred_cap().map(|cap| CapId(cap.0));
                self.send_status_reply(outbound_ops, self.status_reply(record), reply_cap)?;
            }
            SUPERVISOR_OP_ACK_REDELEGATION => {
                let req = RedelegationAckRequest::decode(request.as_slice())
                    .ok_or(KernelError::WrongObject)?;
                let _ = self.complete_redelegation(req.tid);
            }
            _ => {
                #[cfg(not(test))]
                yarm_user_rt::user_log!(
                    "SUPERVISOR_CONTROL_WRONG_OBJECT site=dispatch opcode={} reason=unknown-opcode",
                    request.opcode
                );
                return Err(KernelError::WrongObject);
            }
        }
        Ok(())
    }

    fn execute_due_restarts(
        &mut self,
        restart_ops: &mut impl SupervisorRestartRedelegationOps,
    ) -> Result<usize, KernelError> {
        let restarted = 0usize;
        let mut idx = 0usize;
        while idx < self.managed.len() {
            let Some(mut record) = self.managed[idx] else {
                idx += 1;
                continue;
            };
            let Some(due) = record.pending_restart_due else {
                idx += 1;
                continue;
            };
            if due.0 > self.current_tick.0 {
                idx += 1;
                continue;
            }
            if !matches!(
                record.pm_restart_state,
                SupervisorPmRestartState::PendingDue
            ) {
                self.managed[idx] = Some(record);
                idx += 1;
                continue;
            }
            let Some(restart_token) = record.pending_restart_token else {
                record.pm_restart_state = SupervisorPmRestartState::BlockedNoPmClient;
                record.restart_blocked_no_pm_client = true;
                self.managed[idx] = Some(record);
                idx += 1;
                continue;
            };
            let request_id = self.next_pm_restart_request_id()?;
            record.pending_pm_request_id = Some(request_id);
            let client_request = SupervisorPmRestartClientRequest {
                request_id,
                supervisor_tid: self.handoff.supervisor_tid,
                target_tid: record.tid,
                service_kind: Self::service_kind_code(record.kind),
                service_name: Self::service_name_bytes(record.kind),
                reason: record.pending_restart_reason,
                attempt_count: record.restart_attempts as u16,
                due_tick: due.0,
                degraded_hint: self.degraded,
                policy_flags: 0,
                token_owner_tid: record.tid,
                token_fingerprint: (restart_token & 0xffff) as u16,
                accepted_reply_enabled: self.pm_restart_acceptance_enabled,
            };
            #[cfg(not(test))]
            yarm_user_rt::user_log!(
                "SUPERVISOR_RESTART_DUE_CHECK tid={} service={} request_id={} reason={:?} dependency_cause_tid={} due_tick={} attempt={} state={:?}",
                record.tid,
                Self::service_name(record.kind),
                request_id,
                record.pending_restart_reason.as_pm_reason(),
                record.pending_restart_reason.dependency_cause_tid(),
                due.0,
                record.restart_attempts,
                record.pm_restart_state
            );
            let client_result = restart_ops.restart_task(client_request);
            match client_result {
                SupervisorPmRestartClientResult::Accepted {
                    replacement_handle_kind,
                    replacement_handle_value,
                } => {
                    record.last_restart_tick = self.current_tick;
                    record.pending_restart_due = None;
                    record.pending_restart_token = None;
                    record.pending_pm_request_id = None;
                    record.pm_resource_unavailable_retries = 0;
                    record.restart_blocked_no_pm_client = false;
                    record.restart_deferred_by_pm = false;
                    record.restart_rejected_by_pm = false;
                    record.restart_deferred_no_pm_client_logged = false;
                    let old_tid = record.tid;
                    if replacement_handle_value != 0 {
                        #[cfg(not(test))]
                        yarm_user_rt::user_log!(
                            "SUPERVISOR_RESTART_LINEAGE_UPDATE_BEGIN old_tid={} replacement_tid={} attempt={}",
                            old_tid,
                            replacement_handle_value,
                            record.restart_attempts
                        );
                        record.tid = replacement_handle_value;
                        if self.pm_restart_acceptance_enabled
                            && matches!(record.kind, ManagedServiceKind::Driver)
                            && record.restart_group == 13
                        {
                            record.pending_restart_token =
                                Some(supervisor_crash_test_restart_token_for_tid(
                                    replacement_handle_value,
                                ));
                        }
                        #[cfg(not(test))]
                        {
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_RESTART_LINEAGE_UPDATE_OK old_tid={} replacement_tid={}",
                                old_tid,
                                replacement_handle_value
                            );
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_RESTART_LINEAGE_INDEX_OK replacement_tid={}",
                                replacement_handle_value
                            );
                        }
                    } else {
                        #[cfg(not(test))]
                        yarm_user_rt::user_log!(
                            "SUPERVISOR_RESTART_LINEAGE_INDEX_FAIL replacement_tid={} reason=zero",
                            replacement_handle_value
                        );
                    }
                    record.pm_restart_state = SupervisorPmRestartState::RestartAccepted;
                    self.degraded = false;
                    #[cfg(not(test))]
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_PM_RESTART_STATE_UPDATED tid={} replacement_tid={} attempt={} request_id={} replacement_handle_kind={} replacement_handle_value={}",
                        record.tid,
                        replacement_handle_value,
                        record.restart_attempts,
                        request_id,
                        replacement_handle_kind,
                        replacement_handle_value
                    );
                }
                SupervisorPmRestartClientResult::Deferred {
                    failure,
                    retry_tick,
                } => {
                    record.restart_deferred_by_pm = true;
                    record.pm_restart_state = if failure == PmRestartFailure::ResourceUnavailable {
                        SupervisorPmRestartState::AwaitingMechanismUnavailable
                    } else {
                        SupervisorPmRestartState::PmDeferred
                    };
                    if failure == PmRestartFailure::ResourceUnavailable
                        && record.pm_resource_unavailable_retries
                            < SUPERVISOR_PM_RESOURCE_UNAVAILABLE_RETRY_MAX
                    {
                        record.pm_resource_unavailable_retries =
                            record.pm_resource_unavailable_retries.saturating_add(1);
                        let bounded_retry_tick = if retry_tick > self.current_tick.0 {
                            TickInstant(retry_tick)
                        } else {
                            self.current_tick + TickDuration(1)
                        };
                        record.pending_restart_due = Some(bounded_retry_tick);
                        record.pending_pm_request_id = None;
                        #[cfg(not(test))]
                        yarm_user_rt::user_log!(
                            "SUPERVISOR_RESTART_RESCHEDULED tid={} attempt={} retry_tick={} reason=pm_resource_unavailable retry_count={} retry_max={}",
                            record.tid,
                            record.restart_attempts,
                            bounded_retry_tick.0,
                            record.pm_resource_unavailable_retries,
                            SUPERVISOR_PM_RESOURCE_UNAVAILABLE_RETRY_MAX
                        );
                    } else if failure == PmRestartFailure::ResourceUnavailable {
                        record.pending_restart_due = None;
                        record.restart_blocked_no_pm_client = true;
                        #[cfg(not(test))]
                        yarm_user_rt::user_log!(
                            "SUPERVISOR_RESTART_RETRY_EXHAUSTED tid={} attempt={} reason=pm_resource_unavailable retry_count={} retry_max={}",
                            record.tid,
                            record.restart_attempts,
                            record.pm_resource_unavailable_retries,
                            SUPERVISOR_PM_RESOURCE_UNAVAILABLE_RETRY_MAX
                        );
                    }
                    #[cfg(not(test))]
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_PM_RESTART_REPLY_DEFERRED_STATE tid={} request_id={} failure={:?} retry_tick={} state={:?}",
                        record.tid,
                        request_id,
                        failure,
                        retry_tick,
                        record.pm_restart_state
                    );
                }
                SupervisorPmRestartClientResult::Rejected { failure } => {
                    record.restart_rejected_by_pm = true;
                    record.pm_restart_state = SupervisorPmRestartState::PmRejected;
                    #[cfg(not(test))]
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_PM_RESTART_REPLY_REJECTED_STATE tid={} request_id={} failure={:?}",
                        record.tid,
                        request_id,
                        failure
                    );
                }
                SupervisorPmRestartClientResult::NoPmClient => {
                    record.restart_blocked_no_pm_client = true;
                    record.pm_restart_state = SupervisorPmRestartState::BlockedNoPmClient;
                    if !record.restart_deferred_no_pm_client_logged {
                        #[cfg(not(test))]
                        yarm_user_rt::user_log!(
                            "SUPERVISOR_RESTART_EXEC_DEFERRED_NO_PM_CLIENT tid={} service={} reason=no-pm-client due_tick={} attempt={} state=RestartBlockedNoPmClient",
                            record.tid,
                            Self::service_name(record.kind),
                            due.0,
                            record.restart_attempts
                        );
                        record.restart_deferred_no_pm_client_logged = true;
                    }
                }
                SupervisorPmRestartClientResult::SendFailed => {
                    record.pm_restart_state = SupervisorPmRestartState::PmClientSendFailed;
                }
                SupervisorPmRestartClientResult::MalformedReply
                | SupervisorPmRestartClientResult::ProtocolViolationAccepted => {
                    record.pm_restart_state = SupervisorPmRestartState::ProtocolViolation;
                }
                SupervisorPmRestartClientResult::BuildFailed => {
                    record.pm_restart_state = SupervisorPmRestartState::ProtocolViolation;
                }
            }
            self.managed[idx] = Some(record);
            idx += 1;
        }
        Ok(restarted)
    }

    #[cfg(test)]
    fn next_due_tick(&self) -> Option<TickInstant> {
        self.managed
            .iter()
            .flatten()
            .filter_map(|record| record.pending_restart_due)
            .min_by_key(|tick| tick.0)
    }

    #[cfg(test)]
    fn has_due_restart_ready(&self) -> bool {
        self.managed.iter().flatten().any(|record| {
            record
                .pending_restart_due
                .is_some_and(|due| due.0 <= self.current_tick.0)
        })
    }

    fn process_manager_tid(&self) -> Option<u64> {
        self.managed.iter().flatten().find_map(|record| {
            matches!(
                record.kind,
                ManagedServiceKind::Core(CoreServiceKind::ProcessManager)
            )
            .then_some(record.tid)
        })
    }

    fn is_trusted_control_sender(&self, sender_tid: u64) -> bool {
        sender_tid == self.init_tid || self.process_manager_tid() == Some(sender_tid)
    }

    fn validate_control_sender(&self, request: &Message) -> Result<(), KernelError> {
        if self.is_trusted_control_sender(request.sender_tid.0) {
            Ok(())
        } else {
            #[cfg(not(test))]
            yarm_user_rt::user_log!(
                "SUPERVISOR_CONTROL_REJECT_UNTRUSTED_SENDER sender_tid={} opcode={}",
                request.sender_tid.0,
                request.opcode
            );
            Err(KernelError::MissingRight)
        }
    }

    fn validate_fault_sender(
        &self,
        sender_tid: u64,
        claimed_tid: u64,
        fault_endpoint: bool,
    ) -> Result<(), KernelError> {
        let trusted_kernel_fault = fault_endpoint && sender_tid == 0;
        let trusted_pm = self.process_manager_tid() == Some(sender_tid);
        if trusted_kernel_fault || trusted_pm {
            Ok(())
        } else {
            #[cfg(not(test))]
            yarm_user_rt::user_log!(
                "SUPERVISOR_FAULT_SENDER_REJECTED claimed_tid={} sender_tid={}",
                claimed_tid,
                sender_tid
            );
            Err(KernelError::MissingRight)
        }
    }

    fn service_name(kind: ManagedServiceKind) -> &'static str {
        match kind {
            ManagedServiceKind::Core(CoreServiceKind::ProcessManager) => "process_manager",
            ManagedServiceKind::Core(CoreServiceKind::Vfs) => "vfs",
            ManagedServiceKind::Core(CoreServiceKind::Supervisor) => "supervisor",
            ManagedServiceKind::Driver => "driver",
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn handle_supervisor_event(
        &mut self,
        outbound_ops: &mut impl SupervisorTaskExitOps,
        restart_ops: &mut impl SupervisorRestartRedelegationOps,
        event: SupervisorEvent,
    ) -> Result<SupervisorStepOutcome, KernelError> {
        let mut outcome = SupervisorStepOutcome::default();
        match event {
            SupervisorEvent::Control(msg) => {
                self.handle_control_request(outbound_ops, msg)?;
                outcome.handled += 1;
            }
            SupervisorEvent::Fault(msg) => {
                match msg.opcode {
                    SUPERVISOR_OP_FAULT_REPORT_WIRE => {
                        let fault = SupervisorFaultReportWire::decode(msg.as_slice())
                            .ok_or(KernelError::WrongObject)?;
                        if self
                            .validate_fault_sender(msg.sender_tid.0, fault.tid, true)
                            .is_err()
                        {
                            #[cfg(not(test))]
                            yarm_user_rt::user_log!("SUPERVISOR_FAULT_SENDER_REJECTED");
                        } else if let Some(restart_token) =
                            outbound_ops.task_restart_token(fault.tid)
                        {
                            let event = TaskExitedEvent {
                                tid: fault.tid,
                                exit_code: fault.synthetic_exit_code(),
                                restart_token,
                            };
                            let _ = self.handle_task_exit(outbound_ops, event)?;
                        } else {
                            #[cfg(not(test))]
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_RESTART_REQUEST_DEFERRED_NO_PM_OP tid={}",
                                fault.tid
                            );
                        }
                    }
                    SUPERVISOR_OP_TASK_EXITED => {
                        let event = TaskExitedEvent::decode(msg.as_slice())
                            .ok_or(KernelError::WrongObject)?;
                        if self
                            .validate_fault_sender(msg.sender_tid.0, event.tid, true)
                            .is_err()
                        {
                            #[cfg(not(test))]
                            yarm_user_rt::user_log!("SUPERVISOR_FAULT_SENDER_REJECTED");
                        } else {
                            let _ = self.handle_task_exit(outbound_ops, event)?;
                        }
                    }
                    SUPERVISOR_OP_TRANSFER_REVOKED => {
                        let _ = TransferRevokedEvent::decode(msg.as_slice())
                            .ok_or(KernelError::WrongObject)?;
                    }
                    _ => return Err(KernelError::WrongObject),
                }
                outcome.handled += 1;
            }
            SupervisorEvent::Tick => {
                self.advance_ticks(TickDuration(1));
                #[cfg(not(test))]
                yarm_user_rt::user_log!("SUPERVISOR_TICK_ADVANCE tick={}", self.current_tick.0);
                outcome.tick_advanced = true;
                outcome.restarts_executed += self.execute_due_restarts(restart_ops)?;
            }
        }
        Ok(outcome)
    }

    #[cfg(test)]
    pub fn service_step(&mut self, kernel: &mut KernelState) -> Result<usize, KernelError> {
        let mut changed = 0usize;
        if self.has_due_restart_ready() {
            let mut restart_ops = KernelSupervisorOutboundMessageOps::new(kernel);
            changed += self.execute_due_restarts(&mut restart_ops)?;
            if changed > 0 {
                return Ok(changed);
            }
        }
        if self.next_due_tick().is_some() {
            return Ok(changed);
        }
        while let Some(request) =
            self.recv_with_budget(kernel, self.handoff.supervisor_control_recv_cap)?
        {
            let mut outbound_ops = KernelSupervisorOutboundMessageOps::new(kernel);
            self.handle_control_request(&mut outbound_ops, request)?;
            changed += 1;
        }
        while let Some(message) =
            self.recv_with_budget(kernel, self.handoff.supervisor_fault_recv_cap)?
        {
            match message.opcode {
                SUPERVISOR_OP_FAULT_REPORT_WIRE => {
                    let fault = SupervisorFaultReportWire::decode(message.as_slice())
                        .ok_or(KernelError::WrongObject)?;
                    let mut task_exit_ops = KernelSupervisorOutboundMessageOps::new(kernel);
                    if let Some(restart_token) = task_exit_ops.task_restart_token(fault.tid) {
                        let event = TaskExitedEvent {
                            tid: fault.tid,
                            exit_code: fault.synthetic_exit_code(),
                            restart_token,
                        };
                        let _ = self.handle_task_exit(&mut task_exit_ops, event)?;
                    }
                }
                SUPERVISOR_OP_TASK_EXITED => {
                    let event = TaskExitedEvent::decode(message.as_slice())
                        .ok_or(KernelError::WrongObject)?;
                    let mut task_exit_ops = KernelSupervisorOutboundMessageOps::new(kernel);
                    let _ = self.handle_task_exit(&mut task_exit_ops, event)?;
                }
                SUPERVISOR_OP_TRANSFER_REVOKED => {
                    let _ = TransferRevokedEvent::decode(message.as_slice())
                        .ok_or(KernelError::WrongObject)?;
                }
                _ => return Err(KernelError::WrongObject),
            }
            changed += 1;
        }
        let mut restart_ops = KernelSupervisorOutboundMessageOps::new(kernel);
        changed += self.execute_due_restarts(&mut restart_ops)?;
        Ok(changed)
    }

    #[cfg(test)]
    fn test_set_disable_budgeted_receive_for_tracked_tid(&mut self, tid: Option<u64>) {
        self.test_disable_budgeted_receive_for_tracked_tid = tid;
    }

    #[cfg(test)]
    pub fn run_until_idle(&mut self, kernel: &mut KernelState) -> Result<usize, KernelError> {
        let mut progress = 0usize;
        loop {
            let changed = self.service_step(kernel)?;
            progress += changed;
            if changed > 0 {
                continue;
            }
            let Some(next_due) = self.next_due_tick() else {
                break;
            };
            self.current_tick = next_due;
        }
        Ok(progress)
    }

    #[cfg(test)]
    pub fn run_live_for_ticks(
        &mut self,
        kernel: &mut KernelState,
        idle_ticks: u64,
    ) -> Result<usize, KernelError> {
        let mut progress = 0usize;
        let mut idle_elapsed = 0u64;
        while idle_elapsed < idle_ticks {
            let changed = self.service_step(kernel)?;
            progress += changed;
            if changed == 0 {
                self.advance_ticks(TickDuration(1));
                idle_elapsed += 1;
            }
        }
        progress += self.service_step(kernel)?;
        Ok(progress)
    }

    pub fn handle_task_exit(
        &mut self,
        task_exit_ops: &mut impl SupervisorTaskExitOps,
        event: TaskExitedEvent,
    ) -> Result<SupervisorDecision, KernelError> {
        #[cfg(not(test))]
        yarm_user_rt::user_log!("SUPERVISOR_HANDLE_TASK_EXIT_BEGIN tid={}", event.tid);
        let Some(snapshot) = self.find_record(event.tid) else {
            #[cfg(not(test))]
            {
                yarm_user_rt::user_log!(
                    "SUPERVISOR_RECORD_LOOKUP tid={} result=missing",
                    event.tid
                );
                yarm_user_rt::user_log!(
                    "SUPERVISOR_FAULT_LOOKUP_FAIL fault_tid={} reason=unmanaged",
                    event.tid
                );
                yarm_user_rt::user_log!(
                    "SUPERVISOR_RESTART_NOT_SCHEDULED tid={} reason=unmanaged",
                    event.tid
                );
                yarm_user_rt::user_log!(
                    "SUPERVISOR_HANDLE_TASK_EXIT_RESULT tid={} decision=ignored-missing-record",
                    event.tid
                );
            }
            return Ok(SupervisorDecision::Ignored { tid: event.tid });
        };
        let policy_snapshot = self.policy_for(snapshot);
        #[cfg(not(test))]
        {
            yarm_user_rt::user_log!("SUPERVISOR_RECORD_LOOKUP tid={} result=found", event.tid);
            yarm_user_rt::user_log!(
                "SUPERVISOR_FAULT_LOOKUP_OK fault_tid={} record_tid={} attempt={}",
                event.tid,
                snapshot.tid,
                snapshot.restart_attempts
            );
            yarm_user_rt::user_log!(
                "SUPERVISOR_RECORD_STATE tid={} max_restarts={} attempts={} token_present={} pending={:?} degraded={}",
                event.tid,
                policy_snapshot.max_restarts,
                snapshot.restart_attempts,
                usize::from(snapshot.pending_restart_token.is_some()),
                snapshot.pm_restart_state,
                self.degraded
            );
        }
        if matches!(snapshot.kind, ManagedServiceKind::Core(kind) if CoreServicePolicyTable::restart_owner_for(kind) == RestartOwner::Init)
        {
            self.send_init_alert(
                task_exit_ops,
                InitAlert {
                    tid: event.tid,
                    kind: InitAlertKind::SupervisorRestarted,
                },
            )?;
            #[cfg(not(test))]
            yarm_user_rt::user_log!(
                "SUPERVISOR_HANDLE_TASK_EXIT_RESULT tid={} decision=ignored-init-owner",
                event.tid
            );
            return Ok(SupervisorDecision::Ignored { tid: event.tid });
        }
        let policy = self.policy_for(snapshot);
        let restart_window_ticks = self.handoff.restart_window_ticks;
        let current_tick = self.current_tick;
        let within_window_exhausted = {
            let mut exhausted = false;
            let record = self
                .find_record_mut(event.tid)
                .ok_or(KernelError::TaskMissing)?;
            if TickDuration(restart_window_ticks)
                .has_elapsed_since(record.window_start_tick, current_tick)
            {
                record.window_start_tick = current_tick;
                record.restart_attempts = 0;
            }
            record.last_exit_code = event.exit_code;
            record.last_exit_tick = current_tick;
            if record.restart_attempts >= policy.max_restarts {
                exhausted = true;
            }
            exhausted
        };
        if within_window_exhausted {
            #[cfg(not(test))]
            yarm_user_rt::user_log!(
                "SUPERVISOR_RESTART_LIMIT_EXCEEDED attempts={}",
                policy.max_restarts
            );
            #[cfg(not(test))]
            yarm_user_rt::user_log!("SUPERVISOR_SERVICE_DEGRADED_FINAL");
            self.degraded = true;
            let mark_result = task_exit_ops.mark_task_dead(event.tid);
            let alert_result = self.send_init_alert(
                task_exit_ops,
                InitAlert {
                    tid: event.tid,
                    kind: InitAlertKind::ServiceDegraded,
                },
            );
            mark_result?;
            alert_result?;
            #[cfg(not(test))]
            yarm_user_rt::user_log!(
                "SUPERVISOR_HANDLE_TASK_EXIT_RESULT tid={} decision=degraded-final",
                event.tid
            );
            return Ok(SupervisorDecision::MarkedDead {
                tid: event.tid,
                kind: snapshot.kind,
            });
        }

        let restart_reason = if (event.exit_code & SUPERVISOR_FAULT_EXIT_CODE_TAG)
            == SUPERVISOR_FAULT_EXIT_CODE_TAG
        {
            SupervisorRestartReason::Fault
        } else {
            SupervisorRestartReason::NormalExit
        };
        let due_tick =
            match self.schedule_restart_with_reason(event.tid, event.restart_token, restart_reason)
            {
                Ok(due_tick) => due_tick,
                Err(err) => {
                    #[cfg(not(test))]
                    {
                        yarm_user_rt::user_log!(
                            "SUPERVISOR_RESTART_SCHEDULE_FAIL tid={} reason={:?}",
                            event.tid,
                            err
                        );
                        yarm_user_rt::user_log!(
                            "SUPERVISOR_HANDLE_TASK_EXIT_ERR tid={} err={:?}",
                            event.tid,
                            err
                        );
                    }
                    return Err(err);
                }
            };
        #[cfg(not(test))]
        yarm_user_rt::user_log!(
            "SUPERVISOR_HANDLE_TASK_EXIT_RESULT tid={} decision=scheduled-restart",
            event.tid
        );
        for dependent_tid in self.dependent_tids(snapshot).into_iter().flatten() {
            let Some(token) = task_exit_ops.task_restart_token(dependent_tid) else {
                #[cfg(not(test))]
                yarm_user_rt::user_log!(
                    "SUPERVISOR_DEPENDENT_RESTART_BLOCKED_NO_TOKEN dependent_tid={} failed_tid={}",
                    dependent_tid,
                    event.tid
                );
                continue;
            };
            let _ = self.schedule_restart_with_reason(
                dependent_tid,
                token,
                SupervisorRestartReason::Dependency {
                    cause_tid: event.tid,
                },
            );
        }
        Ok(SupervisorDecision::ScheduledRestart {
            tid: event.tid,
            kind: snapshot.kind,
            due_tick,
            redelegated: !matches!(snapshot.kind, ManagedServiceKind::Driver),
        })
    }

    pub fn complete_redelegation(&mut self, tid: u64) -> bool {
        if let Some(record) = self.find_record_mut(tid) {
            if record.pending_redelegation {
                record.pending_redelegation = false;
                return true;
            }
        }
        false
    }

    pub fn pending_redelegation(&self, tid: u64) -> bool {
        self.find_record(tid)
            .map(|record| record.pending_redelegation)
            .unwrap_or(false)
    }

    pub fn status_for(&self, tid: u64) -> Option<SupervisorStatusReply> {
        self.find_record(tid)
            .map(|record| self.status_reply(record))
    }

    fn status_reply(&self, record: ManagedServiceRecord) -> SupervisorStatusReply {
        let policy = self.policy_for(record);
        let restart_owner = match record.kind {
            ManagedServiceKind::Core(kind) => match CoreServicePolicyTable::restart_owner_for(kind)
            {
                RestartOwner::Init => 1,
                RestartOwner::Supervisor => 2,
            },
            ManagedServiceKind::Driver => 2,
        };
        SupervisorStatusReply {
            tid: record.tid,
            degraded: self.degraded,
            pending_redelegation: record.pending_redelegation,
            restart_attempts: record.restart_attempts,
            restart_group: record.restart_group,
            max_restarts: policy.max_restarts,
            restart_owner,
            last_exit_code: record.last_exit_code,
            last_exit_tick: record.last_exit_tick.0,
            pending_restart_due: record.pending_restart_due.map(|tick| tick.0).unwrap_or(0),
            last_restart_tick: record.last_restart_tick.0,
        }
    }
}

pub fn query_status_via_call_reply(
    supervisor_query_ops: &mut impl SupervisorStatusQueryOps,
    supervisor_control_send_cap: CapId,
    caller_reply_recv_cap: CapId,
    requester_tid: u64,
    queried_tid: u64,
    recv_timeout_ticks: u64,
) -> Result<SupervisorStatusReply, KernelError> {
    supervisor_query_ops.query_status_via_call_reply(
        supervisor_control_send_cap,
        caller_reply_recv_cap,
        requester_tid,
        queried_tid,
        recv_timeout_ticks,
    )
}

pub trait SupervisorStatusQueryOps {
    fn query_status_via_call_reply(
        &mut self,
        supervisor_control_send_cap: CapId,
        caller_reply_recv_cap: CapId,
        requester_tid: u64,
        queried_tid: u64,
        recv_timeout_ticks: u64,
    ) -> Result<SupervisorStatusReply, KernelError>;
}

#[cfg(test)]
pub struct KernelSupervisorStatusQueryOps<'a> {
    kernel: &'a mut KernelState,
}

#[cfg(test)]
impl<'a> KernelSupervisorStatusQueryOps<'a> {
    pub fn new(kernel: &'a mut KernelState) -> Self {
        Self { kernel }
    }
}

#[cfg(test)]
impl SupervisorStatusQueryOps for KernelSupervisorStatusQueryOps<'_> {
    fn query_status_via_call_reply(
        &mut self,
        supervisor_control_send_cap: CapId,
        caller_reply_recv_cap: CapId,
        requester_tid: u64,
        queried_tid: u64,
        recv_timeout_ticks: u64,
    ) -> Result<SupervisorStatusReply, KernelError> {
        let request =
            query_status_message(requester_tid, SupervisorStatusRequest { tid: queried_tid })
                .map_err(|_| KernelError::WrongObject)?;
        let caller_tid = ThreadId(self.kernel.current_tid().ok_or(KernelError::TaskMissing)?);
        let reply_cap =
            self.kernel
                .create_reply_cap_for_caller(caller_tid, caller_reply_recv_cap, None)?;
        let request_with_reply_cap = Message::with_header(
            request.sender_tid.0,
            request.opcode,
            request.flags | Message::FLAG_CAP_TRANSFER,
            Some(reply_cap.0),
            request.as_slice(),
        )
        .map_err(|_| KernelError::WrongObject)?;

        self.kernel
            .ipc_send(supervisor_control_send_cap, request_with_reply_cap)?;
        let reply = self
            .kernel
            .ipc_recv_with_deadline(caller_reply_recv_cap, recv_timeout_ticks)?
            .ok_or(KernelError::WrongObject)?;
        SupervisorStatusReply::decode(reply.as_slice()).ok_or(KernelError::WrongObject)
    }
}

pub fn query_status_via_call_reply_with_default_timeout(
    supervisor_query_ops: &mut impl SupervisorStatusQueryOps,
    supervisor_control_send_cap: CapId,
    caller_reply_recv_cap: CapId,
    requester_tid: u64,
    queried_tid: u64,
) -> Result<SupervisorStatusReply, KernelError> {
    query_status_via_call_reply(
        supervisor_query_ops,
        supervisor_control_send_cap,
        caller_reply_recv_cap,
        requester_tid,
        queried_tid,
        SUPERVISOR_QUERY_STATUS_CALL_RECV_TIMEOUT_TICKS,
    )
}

#[cfg(test)]
pub fn run() {
    let mut kernel = yarm::kernel::boot::Bootstrap::init().expect("init");
    let mut init = crate::control_plane::init::InitService::new();
    let graph = crate::control_plane::init::CoreServiceGraph {
        init_tid: 1,
        process_manager_tid: 2,
        vfs_tid: 3,
        supervisor_tid: 4,
    };
    init.register_core_graph(&mut kernel, graph).expect("graph");
    init.launch_core_services(
        &mut kernel,
        crate::control_plane::init::CoreServiceImagePlan {
            process_manager_entry: 0x8000,
            vfs_entry: 0x9000,
            supervisor_entry: 0xA000,
        },
    )
    .expect("launch");
    let handoff = init
        .install_fault_handoff(&mut kernel, 100)
        .expect("handoff");
    init.seed_supervisor_registrations(&mut kernel)
        .expect("seed");
    let mut supervisor = SupervisorService::new(1, handoff, init.restart_policies());
    let handled = supervisor
        .run_live_for_ticks(&mut kernel, 64)
        .expect("loop");
    yarm_user_rt::user_log!(
        "supervisor.srv online: handled={}, degraded={}, tick={}",
        handled,
        supervisor.degraded(),
        supervisor.current_tick().0
    );
}

#[cfg(not(test))]
pub fn run() {
    yarm_user_rt::user_log!("SUP_RUN_ENTER");
    let startup = startup_context();
    let process_manager_caps = startup.process_manager_caps();
    let runtime_handoff = SupervisorRuntimeHandoff::from_startup_context(startup);
    let mut transport = SyscallIpcTransport;

    let service = SupervisorService::new_from_runtime_handoff(runtime_handoff);
    match service {
        Ok(supervisor) => {
            let mut supervisor = supervisor;
            yarm_user_rt::user_log!(
                "supervisor.srv runtime handoff ready: init_tid={}, supervisor_tid={}, control_recv_ep={}, control_send_ep={}, fault_recv_ep={}, init_alert_send_ep={}, init_alert_recv_ep={}, degraded={}; runtime receive loop enabled",
                supervisor.init_tid,
                supervisor.handoff.supervisor_tid,
                supervisor.handoff.supervisor_control_recv_cap.0,
                supervisor.handoff.supervisor_control_send_cap.0,
                supervisor.handoff.supervisor_fault_recv_cap.0,
                supervisor.handoff.init_alert_send_cap.0,
                supervisor.handoff.init_alert_recv_cap.0,
                supervisor.degraded(),
            );
            // Query PM lifecycle table for the supervisor's own TID to establish
            // truthful supervision metadata before entering the event loop. In
            // the gated crash-restart runtime this self-probe is obsolete: the
            // managed-record path is seeded by explicit supervisor registration
            // messages and the PM restart contract, while early PM lifecycle
            // request/reply caps may not yet be usable for this optional probe.
            // Skip only that optional probe so real restart/token/PM errors
            // remain fail-closed.
            let supervisor_tid = startup.task_id;
            yarm_user_rt::user_log!(
                "SUPERVISOR_LIFECYCLE_QUERY_BEGIN tid={} cap_kind=pm_request_reply source=startup",
                supervisor_tid
            );
            if supervisor_restart_test_build_gate_enabled() {
                yarm_user_rt::user_log!(
                    "SUPERVISOR_LIFECYCLE_QUERY_SKIP tid={} reason=restart_test_managed_record_probe_obsolete",
                    supervisor_tid
                );
            } else {
                yarm_user_rt::user_log!("SUPERVISOR_LIFECYCLE_QUERY tid={}", supervisor_tid);
                match query_lifecycle_via_process_manager(process_manager_caps, supervisor_tid) {
                    Ok(Some(reply)) if reply.is_found() => {
                        yarm_user_rt::user_log!(
                            "SUPERVISOR_LIFECYCLE_FOUND tid={} image_id={} restart_supported=0",
                            reply.tid,
                            reply.image_id
                        );
                        yarm_user_rt::user_log!(
                            "restart unsupported: PM lifecycle record found but no restart token source wired"
                        );
                    }
                    Ok(Some(_)) | Ok(None) => {
                        yarm_user_rt::user_log!(
                            "SUPERVISOR_LIFECYCLE_MISSING tid={}",
                            supervisor_tid
                        );
                    }
                    Err(err) => {
                        yarm_user_rt::user_log!(
                            "SUPERVISOR_LIFECYCLE_QUERY_ERR tid={} err={:?}",
                            supervisor_tid,
                            err
                        );
                    }
                }
            }
            loop {
                yarm_user_rt::user_log!(
                    "SUPERVISOR_EVENT_LOOP_TICK tick={}",
                    supervisor.current_tick.0
                );
                let mut made_progress = false;
                supervisor.drop_stale_pending_faults();
                if supervisor.has_managed_records() {
                    // SUP-L7H: once fault-bearing records exist, do not perform a
                    // bounded control receive before fault wait. The previous
                    // SUP-L7G control poll could block on the control endpoint
                    // while faults queued on the separate fault endpoint with
                    // woke=0.
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_CONTROL_POLL_SKIPPED reason=managed_records_ready"
                    );
                } else {
                    yarm_user_rt::user_log!("SUPERVISOR_CONTROL_POLL_BEGIN");
                    if supervisor_restart_test_build_gate_enabled() {
                        yarm_user_rt::user_log!(
                            "SUPERVISOR_CONTROL_OPTIONAL_PROBE_SKIP reason=registration_receive_required"
                        );
                    }
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_CONTROL_REQUIRED_RECV_BEGIN mode=registration cap={}",
                        supervisor.handoff.supervisor_control_recv_cap.0
                    );
                    match supervisor_recv_short_deadline(
                        &mut transport,
                        supervisor.handoff.supervisor_control_recv_cap.0 as u32,
                    ) {
                        Ok(Some(msg)) => {
                            made_progress = true;
                            let payload = msg.as_slice();
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_CONTROL_REQUIRED_RECV_OK sender={} opcode={} len={}",
                                msg.sender_tid.0,
                                msg.opcode,
                                payload.len()
                            );
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_CONTROL_RECV sender={} opcode={} len={}",
                                msg.sender_tid.0,
                                msg.opcode,
                                payload.len()
                            );
                            let first = [
                                payload.first().copied().unwrap_or(0),
                                payload.get(1).copied().unwrap_or(0),
                                payload.get(2).copied().unwrap_or(0),
                                payload.get(3).copied().unwrap_or(0),
                                payload.get(4).copied().unwrap_or(0),
                                payload.get(5).copied().unwrap_or(0),
                                payload.get(6).copied().unwrap_or(0),
                                payload.get(7).copied().unwrap_or(0),
                            ];
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_CONTROL_PAYLOAD first8=[{:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}] len={}",
                                first[0],
                                first[1],
                                first[2],
                                first[3],
                                first[4],
                                first[5],
                                first[6],
                                first[7],
                                payload.len()
                            );
                            yarm_user_rt::user_log!(
                                "supervisor.srv control msg: opcode={}",
                                msg.opcode
                            );
                            let msg = if msg.opcode == 0 && payload.len() >= 2 {
                                let framed_opcode = u16::from_le_bytes([payload[0], payload[1]]);
                                yarm_user_rt::user_log!(
                                    "SUPERVISOR_CONTROL_DISPATCH opcode={}",
                                    framed_opcode
                                );
                                match Message::with_header(
                                    msg.sender_tid.0,
                                    framed_opcode,
                                    msg.flags,
                                    msg.transferred_cap().map(|cap| cap.0),
                                    &payload[2..],
                                ) {
                                    Ok(normalized) => normalized,
                                    Err(_) => {
                                        yarm_user_rt::user_log!(
                                            "SUPERVISOR_CONTROL_WRONG_OBJECT site=inline-normalize opcode={} reason=message",
                                            framed_opcode
                                        );
                                        msg
                                    }
                                }
                            } else {
                                msg
                            };
                            let mut ops = RuntimeSupervisorTaskExitOps {
                                token_tid: 0,
                                token: 0,
                                supervisor_tid: startup.task_id,
                                process_manager_caps,
                            };
                            let registered_tid = match msg.opcode {
                                SUPERVISOR_OP_REGISTER_CORE_SERVICE => {
                                    RegisterCoreServiceRequest::decode(msg.as_slice())
                                        .map(|request| request.tid)
                                }
                                SUPERVISOR_OP_REGISTER_DRIVER => {
                                    RegisterDriverRequest::decode(msg.as_slice())
                                        .map(|request| request.tid)
                                }
                                _ => None,
                            };
                            if let Err(err) = supervisor.handle_control_request(&mut ops, msg) {
                                yarm_user_rt::user_log!(
                                    "supervisor.srv control handler error: {:?}",
                                    err
                                );
                            } else if let Some(registered_tid) = registered_tid {
                                let replayed = supervisor_replay_ready_pending_faults(
                                    &mut supervisor,
                                    &mut transport,
                                    process_manager_caps,
                                    startup.task_id,
                                    registered_tid,
                                );
                                if replayed > 0 {
                                    made_progress = true;
                                }
                            }
                        }
                        Ok(None) => {
                            yarm_user_rt::user_log!("SUPERVISOR_CONTROL_POLL_EMPTY");
                        }
                        Err(err) => {
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_CONTROL_REQUIRED_RECV_ERR err={:?}",
                                err
                            );
                            let _ = yarm_user_rt::syscall::yield_now();
                        }
                    }
                }

                if !made_progress && supervisor.has_managed_records() {
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_IDLE_WAIT_SELECT mode=fault reason=managed_records_ready"
                    );
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_CONTROL_WAIT_SKIPPED reason=managed_records_ready"
                    );
                    let _ = supervisor_fault_idle_wait(
                        &mut supervisor,
                        &mut transport,
                        process_manager_caps,
                        startup.task_id,
                    );
                } else if !made_progress {
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_IDLE_WAIT_SELECT mode=control reason=no_managed_records"
                    );
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_CONTROL_WAIT_BEGIN timeout={}",
                        SUPERVISOR_RUNTIME_IDLE_RECV_TIMEOUT_TICKS
                    );
                    match supervisor_idle_wait(
                        &mut transport,
                        supervisor.handoff.supervisor_control_recv_cap.0 as u32,
                    ) {
                        Ok(true) => {
                            yarm_user_rt::user_log!("SUPERVISOR_CONTROL_WAIT_DONE result=message");
                        }
                        Ok(false) => {
                            yarm_user_rt::user_log!("SUPERVISOR_CONTROL_WAIT_DONE result=empty");
                        }
                        Err(err) => {
                            yarm_user_rt::user_log!("SUPERVISOR_CONTROL_WAIT_DONE result=err");
                            yarm_user_rt::user_log!("supervisor.srv idle wait error: {:?}", err)
                        }
                    }
                }

                supervisor.advance_ticks(TickDuration(1));
                yarm_user_rt::user_log!(
                    "SUPERVISOR_TICK_ADVANCE tick={}",
                    supervisor.current_tick.0
                );
                let mut ops = RuntimeSupervisorTaskExitOps {
                    token_tid: 0,
                    token: 0,
                    supervisor_tid: startup.task_id,
                    process_manager_caps,
                };
                match supervisor.execute_due_restarts(&mut ops) {
                    Ok(count) if count > 0 => {
                        yarm_user_rt::user_log!("SUPERVISOR_RESTART_DUE_EXECUTE count={}", count)
                    }
                    Ok(_) => {}
                    Err(err) => yarm_user_rt::user_log!(
                        "SUPERVISOR_RESTART_REQUEST_DEFERRED_NO_PM_OP err={:?}",
                        err
                    ),
                }
            }
        }
        Err(err) => {
            yarm_user_rt::user_log!(
                "supervisor.srv runtime handoff incomplete: startup_task_id={}, err={:?}; TODO: provide endpoint caps via startup BootInfo/runtime args",
                startup.task_id,
                err
            );
            loop {
                let _ = yarm_user_rt::syscall::yield_now();
            }
        }
    }
}

#[cfg(not(test))]
fn supervisor_recv_short_deadline(
    transport: &mut impl IpcTransport,
    cap: u32,
) -> Result<Option<Message>, KernelError> {
    // SUP-L7G: legacy recv_with_deadline(..., 0) is not a try-receive in this
    // runtime; use a small positive timeout for bounded control/reply waits.
    transport
        .recv_with_deadline(cap, SUPERVISOR_SHORT_RECV_TIMEOUT_TICKS)
        .map_err(|_| KernelError::WrongObject)
}

#[cfg(not(test))]
#[allow(dead_code)]
fn supervisor_try_recv_fault(cap: u32) -> Result<Option<Message>, KernelError> {
    // SUP-L7G: NR 30 RecvSharedV3 is the live userspace nonblocking receive API.
    // It preserves sender/transfer metadata but not the message opcode in its
    // output record, so use it only on the dedicated supervisor fault endpoint,
    // whose wire payload is always SUPERVISOR_OP_FAULT_REPORT_WIRE.
    let mut payload = [0u8; yarm_user_rt::ipc::Message::MAX_PAYLOAD];
    let mut output = RecvSharedV3Output::new_zeroed();
    let delivery = unsafe {
        ipc_recv_shared_v3_nonblocking(
            cap as u64,
            payload.as_mut_ptr() as u64,
            payload.len() as u64,
            &mut output,
        )
    }
    .map_err(|_| KernelError::WrongObject)?;
    let Some(delivery) = delivery else {
        return Ok(None);
    };
    let len = delivery.message_len as usize;
    if len > payload.len() {
        return Err(KernelError::WrongObject);
    }
    Message::with_header(
        delivery.sender_tid,
        SUPERVISOR_OP_FAULT_REPORT_WIRE,
        delivery.message_flags as u16,
        delivery.transferred_cap,
        &payload[..len],
    )
    .map(Some)
    .map_err(|_| KernelError::WrongObject)
}

#[cfg(not(test))]
fn supervisor_fault_idle_wait(
    supervisor: &mut SupervisorService,
    transport: &mut impl IpcTransport,
    process_manager_caps: Option<(u32, u32)>,
    supervisor_tid: u64,
) -> usize {
    let fault_cap = supervisor.handoff.supervisor_fault_recv_cap.0 as u32;
    yarm_user_rt::user_log!(
        "SUPERVISOR_FAULT_WAIT_BEGIN timeout={}",
        SUPERVISOR_SHORT_RECV_TIMEOUT_TICKS
    );
    match transport.recv_with_deadline(fault_cap, SUPERVISOR_SHORT_RECV_TIMEOUT_TICKS) {
        Ok(Some(msg)) => {
            let queued_tid = if msg.opcode == SUPERVISOR_OP_FAULT_REPORT_WIRE {
                SupervisorFaultReportWire::decode(msg.as_slice()).map(|fault| fault.tid)
            } else {
                None
            };
            if let Some(tid) = queued_tid {
                yarm_user_rt::user_log!("SUPERVISOR_FAULT_WAIT_RECV tid={}", tid);
            } else {
                yarm_user_rt::user_log!("SUPERVISOR_FAULT_WAIT_RECV tid=0");
            }
            let handled = supervisor_handle_fault_endpoint_message(
                supervisor,
                transport,
                process_manager_caps,
                supervisor_tid,
                msg,
            );
            yarm_user_rt::user_log!("SUPERVISOR_FAULT_WAIT_DONE result=message");
            usize::from(handled)
        }
        Ok(None) => {
            yarm_user_rt::user_log!("SUPERVISOR_FAULT_WAIT_EMPTY");
            yarm_user_rt::user_log!("SUPERVISOR_FAULT_WAIT_DONE result=empty");
            0
        }
        Err(err) => {
            yarm_user_rt::user_log!("SUPERVISOR_FAULT_WAIT_DONE result=err");
            yarm_user_rt::user_log!("supervisor.srv fault wait error: {:?}", err);
            0
        }
    }
}

/// Cooperative idle path for the production supervisor loop.
///
/// Uses a bounded recv-timeout budget instead of aggressive yield polling.
/// Control-channel messages are advisory in this staged path, so an arrived
/// message is consumed to wake the loop and then normal polling resumes.
#[cfg(not(test))]
#[inline]
fn supervisor_idle_wait(
    transport: &mut impl IpcTransport,
    control_recv_cap: u32,
) -> Result<bool, KernelError> {
    match transport.recv_with_deadline(control_recv_cap, SUPERVISOR_RUNTIME_IDLE_RECV_TIMEOUT_TICKS)
    {
        Ok(Some(_msg)) => Ok(true),
        Ok(None) => Ok(false),
        Err(_err) => Ok(false),
    }
}

#[cfg(not(test))]
fn query_restart_token_via_process_manager(
    _transport: &mut impl IpcTransport,
    process_manager_caps: Option<(u32, u32)>,
    tid: u64,
) -> Result<Option<u64>, KernelError> {
    let Some((req_cap, rep_cap)) = process_manager_caps else {
        return Ok(None);
    };
    yarm_user_rt::user_log!("SUPERVISOR_RESTART_TOKEN_QUERY_BEGIN tid={}", tid);
    let req = TaskRestartTokenRequest::new(tid);
    let msg = Message::with_header(0, PROC_OP_TASK_RESTART_TOKEN, 0, None, &req.encode())
        .map_err(|_| KernelError::WrongObject)?;
    // Use the same PM request/reply-cap call convention as lifecycle and restart
    // requests; a plain send cannot carry the reply cap PM needs to answer.
    unsafe { yarm_user_rt::syscall::ipc_call(req_cap, rep_cap, &msg) }.map_err(|_| {
        yarm_user_rt::user_log!(
            "SUPERVISOR_RESTART_TOKEN_QUERY_FAIL tid={} reason=ipc-call",
            tid
        );
        KernelError::WrongObject
    })?;
    yarm_user_rt::user_log!("SUPERVISOR_RESTART_TOKEN_QUERY_CALL_SENT tid={}", tid);
    let Some(reply_msg) = unsafe {
        yarm_user_rt::syscall::ipc_recv_with_deadline(rep_cap, SUPERVISOR_SHORT_RECV_TIMEOUT_TICKS)
    }
    .map_err(|_| {
        yarm_user_rt::user_log!(
            "SUPERVISOR_RESTART_TOKEN_QUERY_FAIL tid={} reason=recv",
            tid
        );
        KernelError::WrongObject
    })?
    else {
        yarm_user_rt::user_log!("SUPERVISOR_RESTART_TOKEN_QUERY_TIMEOUT tid={}", tid);
        yarm_user_rt::user_log!(
            "SUPERVISOR_RESTART_TOKEN_QUERY_FAIL tid={} reason=timeout",
            tid
        );
        return Ok(None);
    };
    let reply = match TaskRestartTokenReply::decode(reply_msg.as_slice()) {
        Ok(reply) => reply,
        Err(_) => {
            yarm_user_rt::user_log!(
                "SUPERVISOR_RESTART_TOKEN_QUERY_DECODE_FAIL tid={} reason=payload",
                tid
            );
            return Err(KernelError::WrongObject);
        }
    };
    yarm_user_rt::user_log!("SUPERVISOR_RESTART_TOKEN_QUERY_DECODE_OK tid={}", tid);
    yarm_user_rt::user_log!(
        "SUPERVISOR_RESTART_TOKEN_QUERY_REPLY tid={} status={} len={} fingerprint={}",
        tid,
        reply.found,
        reply_msg.as_slice().len(),
        (reply.token & 0xffff) as u16
    );
    Ok(reply.found_token())
}

#[cfg(not(test))]
#[allow(dead_code)]
const SUPERVISOR_FAULT_DRAIN_MAX_PER_TICK: usize = 8;

#[cfg(not(test))]
fn supervisor_handle_fault_endpoint_message(
    supervisor: &mut SupervisorService,
    transport: &mut impl IpcTransport,
    process_manager_caps: Option<(u32, u32)>,
    supervisor_tid: u64,
    msg: Message,
) -> bool {
    match msg.opcode {
        SUPERVISOR_OP_FAULT_REPORT_WIRE => {
            match SupervisorFaultReportWire::decode(msg.as_slice()) {
                Some(fault) => {
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_FAULT_REPORT_RECV claimed_tid={} sender_tid={}",
                        fault.tid,
                        msg.sender_tid.0
                    );
                    if let Err(err) =
                        supervisor.validate_fault_sender(msg.sender_tid.0, fault.tid, true)
                    {
                        yarm_user_rt::user_log!(
                            "supervisor.srv fault sender rejected: tid={}, sender={}, err={:?}",
                            fault.tid,
                            msg.sender_tid.0,
                            err
                        );
                        yarm_user_rt::user_log!("SUPERVISOR_FAULT_SENDER_REJECTED");
                        yarm_user_rt::user_log!(
                            "SUPERVISOR_FAULT_REPORT_REJECTED tid={} sender={} reason={:?}",
                            fault.tid,
                            msg.sender_tid.0,
                            err
                        );
                        return true;
                    }
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_FAULT_SENDER_OK tid={} sender={}",
                        fault.tid,
                        msg.sender_tid.0
                    );
                    yarm_user_rt::user_log!("SUPERVISOR_FAULT_REPORT_ACCEPTED tid={}", fault.tid);
                    yarm_user_rt::user_log!("SUPERVISOR_POST_FAULT_ACCEPT_BEGIN tid={}", fault.tid);
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_FAULT_LOOKUP_BEGIN fault_tid={}",
                        fault.tid
                    );
                    match supervisor.find_record(fault.tid) {
                        Some(record) => {
                            let policy = supervisor.policy_for(record);
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_RECORD_LOOKUP tid={} result=found",
                                fault.tid
                            );
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_FAULT_LOOKUP_OK fault_tid={} record_tid={} attempt={}",
                                fault.tid,
                                record.tid,
                                record.restart_attempts
                            );
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_RECORD_STATE tid={} max_restarts={} attempts={} token_present={} pending={:?} degraded={}",
                                fault.tid,
                                policy.max_restarts,
                                record.restart_attempts,
                                usize::from(record.pending_restart_token.is_some()),
                                record.pm_restart_state,
                                supervisor.degraded
                            );
                        }
                        None => {
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_RECORD_LOOKUP tid={} result=missing",
                                fault.tid
                            );
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_FAULT_LOOKUP_FAIL fault_tid={} reason=unmanaged phase=startup",
                                fault.tid
                            );
                            if supervisor.stash_pending_fault(fault) {
                                return true;
                            }
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_RESTART_NOT_SCHEDULED tid={} reason=unmanaged",
                                fault.tid
                            );
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_POST_FAULT_ACCEPT_FAIL tid={} reason=missing-record",
                                fault.tid
                            );
                            return false;
                        }
                    }
                    yarm_user_rt::user_log!(
                        "SUPERVISOR_RESTART_TOKEN_RECORD_CHECK tid={}",
                        fault.tid
                    );
                    let token_result = match supervisor
                        .find_record(fault.tid)
                        .and_then(|record| record.pending_restart_token)
                    {
                        Some(token) => {
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_RESTART_TOKEN_RECORD_HIT tid={} fingerprint={}",
                                fault.tid,
                                (token & 0xffff) as u16
                            );
                            Ok(Some((token, "record")))
                        }
                        None => query_restart_token_via_process_manager(
                            transport,
                            process_manager_caps,
                            fault.tid,
                        )
                        .map(|token| token.map(|token| (token, "pm-query"))),
                    };
                    match token_result {
                        Ok(Some((restart_token, token_source))) => {
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_RESTART_TOKEN_STATE tid={} present=1 source={}",
                                fault.tid,
                                token_source
                            );
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_CRASH_TEST_RESTART_TOKEN_RECEIVED tid={} fingerprint={}",
                                fault.tid,
                                (restart_token & 0xffff) as u16
                            );
                            if token_source == "pm-query" {
                                if let Some(record) = supervisor.find_record_mut(fault.tid) {
                                    record.pending_restart_token = Some(restart_token);
                                }
                            }
                            let event = TaskExitedEvent {
                                tid: fault.tid,
                                exit_code: fault.synthetic_exit_code(),
                                restart_token,
                            };
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_POST_FAULT_ACCEPT_CALL_HANDLE_EXIT tid={}",
                                fault.tid
                            );
                            let mut ops = RuntimeSupervisorTaskExitOps {
                                token_tid: fault.tid,
                                token: restart_token,
                                supervisor_tid,
                                process_manager_caps,
                            };
                            match supervisor.handle_task_exit(&mut ops, event) {
                                Ok(SupervisorDecision::ScheduledRestart {
                                    tid, due_tick, ..
                                }) => {
                                    yarm_user_rt::user_log!(
                                        "supervisor.srv restart scheduled through due path only: tid={}, due_tick={}",
                                        tid,
                                        due_tick.0
                                    );
                                    let attempt = supervisor
                                        .find_record(tid)
                                        .map(|record| record.restart_attempts)
                                        .unwrap_or(0);
                                    yarm_user_rt::user_log!(
                                        "SUPERVISOR_RESTART_DUE tid={} attempt={}",
                                        tid,
                                        attempt
                                    );
                                }
                                Ok(_) => {}
                                Err(err) => {
                                    yarm_user_rt::user_log!(
                                        "supervisor.srv failed to apply restart policy decision: tid={}, err={:?}",
                                        fault.tid,
                                        err
                                    );
                                    yarm_user_rt::user_log!(
                                        "SUPERVISOR_POST_FAULT_ACCEPT_FAIL tid={} reason=handle-exit-err",
                                        fault.tid
                                    );
                                }
                            }
                        }
                        Ok(None) => {
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_RESTART_TOKEN_STATE tid={} present=0 source=missing",
                                fault.tid
                            );
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_POST_FAULT_ACCEPT_FAIL tid={} reason=missing-token",
                                fault.tid
                            );
                        }
                        Err(err) => {
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_RESTART_TOKEN_STATE tid={} present=0 source=missing",
                                fault.tid
                            );
                            yarm_user_rt::user_log!(
                                "SUPERVISOR_POST_FAULT_ACCEPT_FAIL tid={} reason=token-query-err",
                                fault.tid
                            );
                            yarm_user_rt::user_log!(
                                "supervisor.srv restart-token lookup failed: tid={}, err={:?}",
                                fault.tid,
                                err
                            );
                        }
                    }
                    true
                }
                None => {
                    yarm_user_rt::user_log!(
                        "supervisor.srv fault report decode failed: len={}",
                        msg.as_slice().len()
                    );
                    true
                }
            }
        }
        SUPERVISOR_OP_TASK_EXITED => {
            if let Some(event) = TaskExitedEvent::decode(msg.as_slice()) {
                if let Err(err) =
                    supervisor.validate_fault_sender(msg.sender_tid.0, event.tid, true)
                {
                    yarm_user_rt::user_log!(
                        "supervisor.srv task-exited sender rejected: tid={}, sender={}, err={:?}",
                        event.tid,
                        msg.sender_tid.0,
                        err
                    );
                    yarm_user_rt::user_log!("SUPERVISOR_FAULT_SENDER_REJECTED");
                } else {
                    let mut ops = RuntimeSupervisorTaskExitOps {
                        token_tid: event.tid,
                        token: event.restart_token,
                        supervisor_tid,
                        process_manager_caps,
                    };
                    match supervisor.handle_task_exit(&mut ops, event) {
                        Ok(decision) => yarm_user_rt::user_log!(
                            "supervisor.srv task-exited handled: decision={:?}",
                            decision
                        ),
                        Err(err) => yarm_user_rt::user_log!(
                            "supervisor.srv task-exited handler error: tid={}, err={:?}",
                            event.tid,
                            err
                        ),
                    }
                }
            }
            true
        }
        _ => {
            yarm_user_rt::user_log!(
                "supervisor.srv fault/control unknown opcode: opcode={}",
                msg.opcode
            );
            true
        }
    }
}

#[cfg(not(test))]
fn supervisor_replay_ready_pending_faults(
    supervisor: &mut SupervisorService,
    transport: &mut impl IpcTransport,
    process_manager_caps: Option<(u32, u32)>,
    supervisor_tid: u64,
    registered_tid: u64,
) -> usize {
    let Some(fault) = supervisor.take_pending_fault_for_registered_tid(registered_tid) else {
        return 0;
    };
    yarm_user_rt::user_log!("SUPERVISOR_FAULT_PENDING_REPLAY_BEGIN tid={}", fault.tid);
    let payload = fault.encode();
    let Ok(msg) = Message::with_header(0, SUPERVISOR_OP_FAULT_REPORT_WIRE, 0, None, &payload)
    else {
        yarm_user_rt::user_log!(
            "SUPERVISOR_FAULT_PENDING_REPLAY_FAIL tid={} reason=message",
            fault.tid
        );
        return 0;
    };
    if supervisor_handle_fault_endpoint_message(
        supervisor,
        transport,
        process_manager_caps,
        supervisor_tid,
        msg,
    ) {
        yarm_user_rt::user_log!("SUPERVISOR_FAULT_PENDING_REPLAY_OK tid={}", fault.tid);
        1
    } else {
        yarm_user_rt::user_log!(
            "SUPERVISOR_FAULT_PENDING_REPLAY_FAIL tid={} reason=handler",
            fault.tid
        );
        0
    }
}

#[cfg(not(test))]
#[allow(dead_code)]
fn supervisor_drain_fault_endpoint(
    supervisor: &mut SupervisorService,
    transport: &mut impl IpcTransport,
    process_manager_caps: Option<(u32, u32)>,
    supervisor_tid: u64,
) -> usize {
    let fault_cap = supervisor.handoff.supervisor_fault_recv_cap.0 as u32;
    yarm_user_rt::user_log!("SUPERVISOR_FAULT_DRAIN_BEGIN");
    let mut count = 0usize;
    while count < SUPERVISOR_FAULT_DRAIN_MAX_PER_TICK {
        match supervisor_try_recv_fault(fault_cap) {
            Ok(Some(msg)) => {
                let queued_tid = if msg.opcode == SUPERVISOR_OP_FAULT_REPORT_WIRE {
                    SupervisorFaultReportWire::decode(msg.as_slice()).map(|fault| fault.tid)
                } else {
                    None
                };
                if let Some(tid) = queued_tid {
                    yarm_user_rt::user_log!("SUPERVISOR_FAULT_QUEUE_PENDING_DRAIN tid={}", tid);
                    yarm_user_rt::user_log!("SUPERVISOR_FAULT_DRAIN_RECV tid={}", tid);
                } else {
                    yarm_user_rt::user_log!("SUPERVISOR_FAULT_DRAIN_RECV tid=0");
                }
                if supervisor_handle_fault_endpoint_message(
                    supervisor,
                    transport,
                    process_manager_caps,
                    supervisor_tid,
                    msg,
                ) {
                    count += 1;
                }
                if let Some(tid) = queued_tid {
                    yarm_user_rt::user_log!("SUPERVISOR_FAULT_QUEUE_DRAINED tid={}", tid);
                }
            }
            Ok(None) => {
                yarm_user_rt::user_log!("SUPERVISOR_FAULT_DRAIN_EMPTY");
                break;
            }
            Err(err) => {
                yarm_user_rt::user_log!("supervisor.srv fault recv error: {:?}", err);
                break;
            }
        }
    }
    yarm_user_rt::user_log!("SUPERVISOR_FAULT_DRAIN_DONE count={}", count);
    count
}
/// Query PM's lifecycle table for `tid`.
///
/// Uses the same `ipc_call` + `ipc_recv_with_deadline` pattern as
/// init → PM SpawnV5 calls, so the kernel delivers a reply-cap to PM
/// alongside the message and PM can reply to it.
///
/// Returns `Ok(Some(reply))` on success, `Ok(None)` when PM caps are
/// unavailable or no reply arrived, `Err` on IPC encoding failure.
#[cfg(not(test))]
fn query_lifecycle_via_process_manager(
    process_manager_caps: Option<(u32, u32)>,
    tid: u64,
) -> Result<Option<LifecycleQueryReply>, KernelError> {
    let Some((req_cap, rep_cap)) = process_manager_caps else {
        return Ok(None);
    };
    let req = LifecycleQueryRequest::new(tid);
    let msg = Message::with_header(0, PROC_OP_LIFECYCLE_QUERY, 0, None, &req.encode())
        .map_err(|_| KernelError::WrongObject)?;
    // SAFETY: Uses kernel-provided startup caps for synchronous PM IPC call,
    // identical to the init → PM SpawnV5 pattern in init/service.rs.
    let _ = unsafe { yarm_user_rt::syscall::ipc_call(req_cap, rep_cap, &msg) };
    let reply_result = unsafe {
        yarm_user_rt::syscall::ipc_recv_with_deadline(rep_cap, SUPERVISOR_SHORT_RECV_TIMEOUT_TICKS)
    };
    let Some(reply_msg) = reply_result.map_err(|_| KernelError::WrongObject)? else {
        yarm_user_rt::user_log!("SUPERVISOR_LIFECYCLE_QUERY_TIMEOUT tid={}", tid);
        return Ok(None);
    };
    let reply =
        LifecycleQueryReply::decode(reply_msg.as_slice()).map_err(|_| KernelError::WrongObject)?;
    Ok(Some(reply))
}

#[cfg(not(test))]
fn send_pm_restart_v1_via_process_manager(
    process_manager_caps: Option<(u32, u32)>,
    client_request: SupervisorPmRestartClientRequest,
) -> SupervisorPmRestartClientResult {
    let Some((req_cap, rep_cap)) = process_manager_caps else {
        yarm_user_rt::user_log!("SUPERVISOR_PM_RESTART_SEND_FAIL reason=no-pm-client");
        return SupervisorPmRestartClientResult::NoPmClient;
    };
    if client_request.token_owner_tid != client_request.target_tid {
        yarm_user_rt::user_log!(
            "SUPERVISOR_PM_RESTART_REQUEST_BUILD_FAIL tid={} reason=token-owner-mismatch",
            client_request.target_tid
        );
        return SupervisorPmRestartClientResult::BuildFailed;
    };
    yarm_user_rt::user_log!(
        "SUPERVISOR_PM_RESTART_REQUEST_BUILD_BEGIN tid={} request_id={} supervisor_tid={} service_kind={} reason={:?} dependency_cause_tid={}",
        client_request.target_tid,
        client_request.request_id,
        client_request.supervisor_tid,
        client_request.service_kind,
        client_request.reason.as_pm_reason(),
        client_request.reason.dependency_cause_tid()
    );
    let mut request = match PmRestartRequestV1::new(
        client_request.request_id,
        client_request.supervisor_tid,
        client_request.target_tid,
        client_request.service_kind,
        client_request.service_name,
        client_request.reason.as_pm_reason(),
        PmRestartTokenDescriptor::scoped(
            client_request.token_owner_tid,
            client_request.token_fingerprint,
        ),
    ) {
        Ok(request) => request,
        Err(_err) => {
            yarm_user_rt::user_log!(
                "SUPERVISOR_PM_RESTART_REQUEST_BUILD_FAIL tid={} reason=codec",
                client_request.target_tid
            );
            return SupervisorPmRestartClientResult::BuildFailed;
        }
    };
    request.attempt_count = client_request.attempt_count;
    request.due_tick = client_request.due_tick;
    request.dependency_cause_tid = client_request.reason.dependency_cause_tid();
    request.degraded_hint = client_request.degraded_hint;
    request.policy_flags = client_request.policy_flags;
    yarm_user_rt::user_log!(
        "SUPERVISOR_PM_RESTART_REQUEST_BUILD_OK request_id={} target_tid={} service_name_len={}",
        request.request_id,
        request.target_tid,
        request.service_name_len
    );
    let encoded = match encode_pm_restart_request_v1(&request) {
        Ok(encoded) => encoded,
        Err(_err) => return SupervisorPmRestartClientResult::BuildFailed,
    };
    let msg = match Message::with_header(
        client_request.supervisor_tid,
        PROC_OP_PM_RESTART_V1,
        0,
        None,
        &encoded,
    ) {
        Ok(msg) => msg,
        Err(_err) => return SupervisorPmRestartClientResult::BuildFailed,
    };
    yarm_user_rt::user_log!(
        "SUPERVISOR_PM_RESTART_SEND_BEGIN tid={} request_id={}",
        client_request.target_tid,
        client_request.request_id
    );
    // SAFETY: Uses kernel-provided PM request/reply caps when present. No cap is
    // encoded in the payload as restart authority.
    if unsafe { yarm_user_rt::syscall::ipc_call(req_cap, rep_cap, &msg) }.is_err() {
        yarm_user_rt::user_log!("SUPERVISOR_PM_RESTART_SEND_FAIL reason=ipc_call");
        return SupervisorPmRestartClientResult::SendFailed;
    }
    yarm_user_rt::user_log!(
        "SUPERVISOR_PM_RESTART_SEND_OK tid={} request_id={}",
        client_request.target_tid,
        client_request.request_id
    );
    yarm_user_rt::user_log!(
        "SUPERVISOR_PM_RESTART_REPLY_WAIT_BEGIN tid={} request_id={}",
        client_request.target_tid,
        client_request.request_id
    );
    let reply_msg = match unsafe { yarm_user_rt::syscall::ipc_recv_v2(rep_cap) } {
        Ok(Some(received)) => received.message,
        Ok(None) => return SupervisorPmRestartClientResult::SendFailed,
        Err(_err) => return SupervisorPmRestartClientResult::SendFailed,
    };
    yarm_user_rt::user_log!(
        "SUPERVISOR_PM_RESTART_REPLY_RECV tid={} request_id={}",
        client_request.target_tid,
        client_request.request_id
    );
    let reply_len = reply_msg.as_slice().len();
    if reply_msg.opcode != SUPERVISOR_PM_RESTART_REPLY_CAP_OPCODE
        || reply_len != PM_RESTART_REPLY_V1_LEN
    {
        yarm_user_rt::user_log!(
            "SUPERVISOR_PM_RESTART_REPLY_SHAPE_FAIL opcode={} len={}",
            reply_msg.opcode,
            reply_len
        );
        yarm_user_rt::user_log!(
            "SUPERVISOR_PM_RESTART_REPLY_DECODE_FAIL reason=shape opcode={} len={}",
            reply_msg.opcode,
            reply_len
        );
        return SupervisorPmRestartClientResult::MalformedReply;
    }
    yarm_user_rt::user_log!(
        "SUPERVISOR_PM_RESTART_REPLY_SHAPE_OK opcode={} abi_opcode={} len={}",
        reply_msg.opcode,
        PROC_OP_PM_RESTART_REPLY_V1,
        reply_len
    );
    let reply = match decode_pm_restart_reply_v1(reply_msg.as_slice()) {
        Ok(reply) => {
            yarm_user_rt::user_log!(
                "SUPERVISOR_PM_RESTART_REPLY_DECODE_OK request_id={} target_tid={}",
                client_request.request_id,
                client_request.target_tid
            );
            reply
        }
        Err(_err) => {
            yarm_user_rt::user_log!("SUPERVISOR_PM_RESTART_REPLY_DECODE_FAIL reason=codec");
            return SupervisorPmRestartClientResult::MalformedReply;
        }
    };
    if reply.request_id != client_request.request_id
        || reply.target_tid != client_request.target_tid
    {
        yarm_user_rt::user_log!(
            "SUPERVISOR_PM_RESTART_REPLY_PROTOCOL_VIOLATION_MISMATCH request_id={} reply_request_id={} target_tid={} reply_target_tid={}",
            client_request.request_id,
            reply.request_id,
            client_request.target_tid,
            reply.target_tid
        );
        return SupervisorPmRestartClientResult::MalformedReply;
    }
    if !matches!(reply.status, PmRestartReplyStatus::Accepted)
        && (reply.replacement_handle_kind != 0 || reply.replacement_handle_value != 0)
    {
        yarm_user_rt::user_log!(
            "SUPERVISOR_PM_RESTART_REPLY_PROTOCOL_VIOLATION_REPLACEMENT_HANDLE tid={} request_id={}",
            client_request.target_tid,
            client_request.request_id
        );
        return SupervisorPmRestartClientResult::MalformedReply;
    }
    match reply.status {
        PmRestartReplyStatus::Deferred => {
            yarm_user_rt::user_log!(
                "SUPERVISOR_PM_RESTART_REPLY_DEFERRED tid={} request_id={} failure={:?} retry_tick={}",
                client_request.target_tid,
                client_request.request_id,
                reply.failure,
                reply.next_retry_tick
            );
            SupervisorPmRestartClientResult::Deferred {
                failure: reply.failure,
                retry_tick: reply.next_retry_tick,
            }
        }
        PmRestartReplyStatus::RolledBack
            if reply.failure == PmRestartFailure::ResourceUnavailable =>
        {
            yarm_user_rt::user_log!(
                "SUPERVISOR_PM_RESTART_REPLY_DEFERRED tid={} request_id={} failure={:?} retry_tick={} source=rolled_back",
                client_request.target_tid,
                client_request.request_id,
                reply.failure,
                reply.next_retry_tick
            );
            SupervisorPmRestartClientResult::Deferred {
                failure: reply.failure,
                retry_tick: reply.next_retry_tick,
            }
        }
        PmRestartReplyStatus::Rejected
        | PmRestartReplyStatus::NoSuchTarget
        | PmRestartReplyStatus::UnsupportedVersion
        | PmRestartReplyStatus::AlreadyRestarting
        | PmRestartReplyStatus::RolledBack => {
            yarm_user_rt::user_log!(
                "SUPERVISOR_PM_RESTART_REPLY_REJECTED tid={} request_id={} status={:?} failure={:?}",
                client_request.target_tid,
                client_request.request_id,
                reply.status,
                reply.failure
            );
            SupervisorPmRestartClientResult::Rejected {
                failure: reply.failure,
            }
        }
        PmRestartReplyStatus::Accepted => {
            if client_request.accepted_reply_enabled
                && reply.replacement_handle_kind
                    == SUPERVISOR_PM_RESTART_REPLACEMENT_HANDLE_KIND_TASK_TID
                && reply.replacement_handle_value != 0
            {
                yarm_user_rt::user_log!(
                    "SUPERVISOR_PM_RESTART_REPLY_ACCEPTED tid={} request_id={} replacement_tid={} replacement_handle_kind={} replacement_handle_value={}",
                    client_request.target_tid,
                    client_request.request_id,
                    reply.replacement_handle_value,
                    reply.replacement_handle_kind,
                    reply.replacement_handle_value
                );
                return SupervisorPmRestartClientResult::Accepted {
                    replacement_handle_kind: reply.replacement_handle_kind,
                    replacement_handle_value: reply.replacement_handle_value,
                };
            }
            yarm_user_rt::user_log!(
                "SUPERVISOR_PM_RESTART_REPLY_PROTOCOL_VIOLATION_ACCEPTED tid={} request_id={}",
                client_request.target_tid,
                client_request.request_id
            );
            SupervisorPmRestartClientResult::ProtocolViolationAccepted
        }
    }
}

#[cfg(not(test))]
#[allow(dead_code)]
fn execute_restart_via_process_manager(
    transport: &mut impl IpcTransport,
    process_manager_caps: Option<(u32, u32)>,
    tid: u64,
    restart_token: u64,
) -> Result<u8, KernelError> {
    let Some((req_cap, rep_cap)) = process_manager_caps else {
        return Ok(ExecuteRestartReply::STATUS_INTERNAL_UNSUPPORTED);
    };
    let req = ExecuteRestartRequest::new(tid, restart_token);
    let msg = Message::with_header(0, PROC_OP_EXECUTE_RESTART, 0, None, &req.encode())
        .map_err(|_| KernelError::WrongObject)?;
    transport
        .send(req_cap, &msg)
        .map_err(|_| KernelError::WrongObject)?;
    let Some(reply_msg) = transport
        .recv(rep_cap)
        .map_err(|_| KernelError::WrongObject)?
    else {
        return Ok(ExecuteRestartReply::STATUS_INTERNAL_UNSUPPORTED);
    };
    let reply =
        ExecuteRestartReply::decode(reply_msg.as_slice()).map_err(|_| KernelError::WrongObject)?;
    Ok(reply.status)
}

#[cfg(not(test))]
struct RuntimeSupervisorTaskExitOps {
    token_tid: u64,
    token: u64,
    supervisor_tid: u64,
    process_manager_caps: Option<(u32, u32)>,
}

#[cfg(not(test))]
impl SupervisorOutboundMessageOps for RuntimeSupervisorTaskExitOps {
    fn ipc_send(&mut self, _cap: CapId, _msg: Message) -> Result<(), KernelError> {
        yarm_user_rt::user_log!("SUPERVISOR_INIT_ALERT_UNAVAILABLE");
        Err(KernelError::InvalidCapability)
    }
    fn ipc_reply(&mut self, _cap: CapId, _msg: Message) -> Result<(), KernelError> {
        yarm_user_rt::user_log!("SUPERVISOR_INIT_ALERT_UNAVAILABLE");
        Err(KernelError::InvalidCapability)
    }
}

#[cfg(not(test))]
impl SupervisorTaskExitOps for RuntimeSupervisorTaskExitOps {
    fn mark_task_dead(&mut self, _tid: u64) -> Result<(), KernelError> {
        yarm_user_rt::user_log!("SUPERVISOR_TASK_EXIT_OP_UNAVAILABLE");
        Err(KernelError::InvalidCapability)
    }
    fn task_restart_token(&self, tid: u64) -> Option<u64> {
        (tid == self.token_tid).then_some(self.token)
    }
}

#[cfg(not(test))]
impl SupervisorRestartRedelegationOps for RuntimeSupervisorTaskExitOps {
    fn restart_task(
        &mut self,
        request: SupervisorPmRestartClientRequest,
    ) -> SupervisorPmRestartClientResult {
        let request = SupervisorPmRestartClientRequest {
            supervisor_tid: self.supervisor_tid,
            ..request
        };
        send_pm_restart_v1_via_process_manager(self.process_manager_caps, request)
    }

    fn delegate_driver_bundle(
        &mut self,
        _server_tid: u64,
        _plan: DriverRecoveryPlan,
    ) -> Result<(), KernelError> {
        yarm_user_rt::user_log!("SUPERVISOR_RESOURCE_CLEANUP_DEFERRED_NO_PM_KERNEL_API");
        Err(KernelError::InvalidCapability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::init::{CoreServiceGraph, CoreServiceImagePlan, InitService};
    use yarm::kernel::boot::Bootstrap;
    use yarm::std::thread;
    use yarm::std::vec::Vec;
    use yarm_ipc_abi::supervisor_abi::{
        CoreServiceRegistrationKind, InitAlertKind, RegisterDriverRequest,
        SUPERVISOR_OP_INIT_ALERT, SUPERVISOR_OP_QUERY_STATUS, SupervisorStatusRequest,
        TransferRevokedEvent,
    };
    use yarm_user_rt::vm::PAGE_SIZE;

    #[derive(Default)]
    struct MockOutboundOps {
        sends: Vec<(CapId, Message)>,
        replies: Vec<(CapId, Message)>,
    }

    impl SupervisorOutboundMessageOps for MockOutboundOps {
        fn ipc_send(&mut self, cap: CapId, msg: Message) -> Result<(), KernelError> {
            self.sends.push((cap, msg));
            Ok(())
        }

        fn ipc_reply(&mut self, cap: CapId, msg: Message) -> Result<(), KernelError> {
            self.replies.push((cap, msg));
            Ok(())
        }
    }

    impl SupervisorTaskExitOps for MockOutboundOps {
        fn mark_task_dead(&mut self, _tid: u64) -> Result<(), KernelError> {
            Ok(())
        }

        fn task_restart_token(&self, tid: u64) -> Option<u64> {
            Some(0xAA00 + tid)
        }
    }

    impl SupervisorRestartRedelegationOps for MockOutboundOps {
        fn restart_task(
            &mut self,
            _request: SupervisorPmRestartClientRequest,
        ) -> SupervisorPmRestartClientResult {
            SupervisorPmRestartClientResult::Deferred {
                failure: PmRestartFailure::ResourceUnavailable,
                retry_tick: 0,
            }
        }

        fn delegate_driver_bundle(
            &mut self,
            _server_tid: u64,
            _plan: DriverRecoveryPlan,
        ) -> Result<(), KernelError> {
            Ok(())
        }
    }

    struct FailingRestartOps {
        attempts: usize,
    }

    impl SupervisorOutboundMessageOps for FailingRestartOps {
        fn ipc_send(&mut self, _cap: CapId, _msg: Message) -> Result<(), KernelError> {
            Ok(())
        }
        fn ipc_reply(&mut self, _cap: CapId, _msg: Message) -> Result<(), KernelError> {
            Ok(())
        }
    }

    impl SupervisorRestartRedelegationOps for FailingRestartOps {
        fn restart_task(
            &mut self,
            _request: SupervisorPmRestartClientRequest,
        ) -> SupervisorPmRestartClientResult {
            self.attempts += 1;
            SupervisorPmRestartClientResult::NoPmClient
        }
        fn delegate_driver_bundle(
            &mut self,
            _server_tid: u64,
            _plan: DriverRecoveryPlan,
        ) -> Result<(), KernelError> {
            Ok(())
        }
    }

    struct MissingDependentTokenOps;

    impl SupervisorOutboundMessageOps for MissingDependentTokenOps {
        fn ipc_send(&mut self, _cap: CapId, _msg: Message) -> Result<(), KernelError> {
            Ok(())
        }
        fn ipc_reply(&mut self, _cap: CapId, _msg: Message) -> Result<(), KernelError> {
            Ok(())
        }
    }

    impl SupervisorTaskExitOps for MissingDependentTokenOps {
        fn mark_task_dead(&mut self, _tid: u64) -> Result<(), KernelError> {
            Ok(())
        }
        fn task_restart_token(&self, tid: u64) -> Option<u64> {
            (tid == 2).then_some(0x2222)
        }
    }

    struct FailingOutboundOps;

    impl SupervisorOutboundMessageOps for FailingOutboundOps {
        fn ipc_send(&mut self, _cap: CapId, _msg: Message) -> Result<(), KernelError> {
            Err(KernelError::InvalidCapability)
        }
        fn ipc_reply(&mut self, _cap: CapId, _msg: Message) -> Result<(), KernelError> {
            Err(KernelError::InvalidCapability)
        }
    }

    impl SupervisorTaskExitOps for FailingOutboundOps {
        fn mark_task_dead(&mut self, _tid: u64) -> Result<(), KernelError> {
            Err(KernelError::InvalidCapability)
        }
        fn task_restart_token(&self, _tid: u64) -> Option<u64> {
            None
        }
    }

    fn setup_supervisor() -> (
        yarm::std::boxed::Box<KernelState>,
        yarm::std::boxed::Box<InitService>,
        InitFaultHandoff,
        yarm::std::boxed::Box<SupervisorService>,
    ) {
        let mut kernel = yarm::std::boxed::Box::new(Bootstrap::init().expect("init"));
        let mut init = yarm::std::boxed::Box::new(InitService::new());
        let graph = CoreServiceGraph {
            init_tid: 1,
            process_manager_tid: 2,
            vfs_tid: 3,
            supervisor_tid: 4,
        };
        init.register_core_graph(&mut kernel, graph).expect("graph");
        init.launch_core_services(
            &mut kernel,
            CoreServiceImagePlan {
                process_manager_entry: 0x8000,
                vfs_entry: 0x9000,
                supervisor_entry: 0xA000,
            },
        )
        .expect("launch");
        let handoff = init
            .install_fault_handoff(&mut kernel, 20)
            .expect("handoff");
        init.seed_supervisor_registrations(&mut kernel)
            .expect("seed");
        let supervisor =
            yarm::std::boxed::Box::new(SupervisorService::new(1, handoff, init.restart_policies()));
        (kernel, init, handoff, supervisor)
    }

    fn run_with_large_stack<F>(f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let handle = thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(f)
            .expect("spawn large-stack test thread");
        handle.join().expect("join large-stack test thread");
    }

    fn enter_delegation_owner_context(kernel: &mut KernelState, owner_tid: u64) -> (u64, u64) {
        if kernel.task_status(owner_tid).is_none() {
            kernel
                .register_task_with_class(owner_tid, to_kernel_task_class(TaskClass::SystemServer))
                .expect("register delegation owner");
        }
        switch_to_current_task(kernel, owner_tid);
        let (_id, mem) = kernel.alloc_anonymous_memory_object().expect("mem");
        let iova = kernel.create_iova_space_cap().expect("iova");
        (mem.0, iova.0)
    }

    fn switch_to_current_task(kernel: &mut KernelState, tid: u64) {
        if kernel.current_tid() == Some(tid) {
            return;
        }
        match kernel.enqueue_current_cpu(tid) {
            Ok(()) | Err(KernelError::WouldBlock) => {}
            Err(err) => panic!("enqueue task: {err:?}"),
        }
        let _ = kernel.dispatch_next_current_cpu();
        assert_eq!(kernel.current_tid(), Some(tid));
    }

    fn restore_delegation_owner_context(kernel: &mut KernelState, owner_tid: u64) {
        if kernel.current_tid() != Some(owner_tid) {
            switch_to_current_task(kernel, owner_tid);
        }
        assert_eq!(kernel.current_tid(), Some(owner_tid));
    }

    fn run_until_idle_with_progress_guard(
        supervisor: &mut SupervisorService,
        kernel: &mut KernelState,
        tracked_tid: u64,
        owner_tid: u64,
        mem_cap: u64,
        iova_cap: u64,
        disable_budgeted_receive_for_tracked: bool,
        max_steps: usize,
    ) -> usize {
        supervisor.test_set_disable_budgeted_receive_for_tracked_tid(
            disable_budgeted_receive_for_tracked.then_some(tracked_tid),
        );
        let mut total_changed = 0usize;
        let mut step = 0usize;
        while step < max_steps {
            let changed = match supervisor.service_step(kernel) {
                Ok(changed) => changed,
                Err(err) => {
                    let debug = kernel.debug_driver_redelegation_context(
                        owner_tid,
                        tracked_tid,
                        CapId(mem_cap),
                        CapId(iova_cap),
                    );
                    panic!("service step: {err:?}; redelegation_debug={debug:?}");
                }
            };
            total_changed += changed;
            if supervisor.status_for(tracked_tid).is_some_and(|status| {
                status.pending_restart_due == 0 && !status.pending_redelegation
            }) && kernel.task_status(tracked_tid).map(map_task_status)
                == Some(TaskStatus::Runnable)
            {
                supervisor.test_set_disable_budgeted_receive_for_tracked_tid(None);
                return total_changed;
            }
            if changed == 0 {
                if let Some(next_due) = supervisor.status_for(tracked_tid).and_then(|status| {
                    (status.pending_restart_due > 0)
                        .then_some(TickInstant(status.pending_restart_due))
                }) {
                    supervisor.current_tick = next_due;
                    step += 1;
                    continue;
                }
                return total_changed;
            }
            step += 1;
        }
        supervisor.test_set_disable_budgeted_receive_for_tracked_tid(None);
        panic!(
            "supervisor run stalled: tracked_tid={}, current_tid={:?}, status={:?}",
            tracked_tid,
            kernel.current_tid(),
            supervisor.status_for(tracked_tid)
        );
    }

    #[test]
    fn supervisor_source_guardrail_prefers_try_or_budgeted_receive_paths() {
        let src = include_str!("service.rs");
        let legacy_call = ["kernel", ".ipc_recv", "("].concat();
        assert!(src.contains("try_ipc_recv("));
        assert!(src.contains("ipc_recv_with_deadline("));
        assert!(!src.contains(legacy_call.as_str()));
    }

    #[test]
    fn control_request_status_query_roundtrip_works_with_mock_outbound_ops() {
        let handoff =
            InitFaultHandoff::new(1, CapId(10), CapId(11), CapId(12), CapId(13), CapId(14), 20);
        let mut supervisor = SupervisorService::new(1, handoff, CoreServicePolicyTable::baseline());
        let mut outbound = MockOutboundOps::default();

        let register = Message::with_header(
            1,
            SUPERVISOR_OP_REGISTER_CORE_SERVICE,
            0,
            None,
            &RegisterCoreServiceRequest {
                tid: 2,
                kind: CoreServiceRegistrationKind::ProcessManager,
                max_restarts: 3,
                restart_group: 1,
                dependency_mask: 0,
                backoff_ticks: 10,
            }
            .encode(),
        )
        .expect("registration");
        supervisor
            .handle_control_request(&mut outbound, register)
            .expect("register service");

        let query = query_status_message(1, SupervisorStatusRequest { tid: 2 }).expect("query");
        supervisor
            .handle_control_request(&mut outbound, query)
            .expect("query status");

        assert_eq!(outbound.replies.len(), 0);
        assert_eq!(outbound.sends.len(), 1);
        let (sent_cap, sent_msg) = &outbound.sends[0];
        assert_eq!(*sent_cap, handoff.init_alert_send_cap);
        assert_eq!(sent_msg.opcode, SUPERVISOR_OP_QUERY_STATUS);
        let status = SupervisorStatusReply::decode(sent_msg.as_slice()).expect("status");
        assert_eq!(status.tid, 2);
        assert_eq!(status.max_restarts, 3);
    }

    #[test]
    fn production_style_control_message_reaches_shared_handler() {
        let handoff =
            InitFaultHandoff::new(1, CapId(10), CapId(11), CapId(12), CapId(13), CapId(14), 20);
        let mut supervisor = SupervisorService::new(1, handoff, CoreServicePolicyTable::baseline());
        let mut ops = MockOutboundOps::default();
        let msg = Message::with_header(
            1,
            SUPERVISOR_OP_REGISTER_CORE_SERVICE,
            0,
            None,
            &RegisterCoreServiceRequest {
                tid: 2,
                kind: CoreServiceRegistrationKind::ProcessManager,
                max_restarts: 3,
                restart_group: 1,
                dependency_mask: 0,
                backoff_ticks: 4,
            }
            .encode(),
        )
        .expect("msg");
        let outcome = supervisor
            .handle_supervisor_event(
                &mut ops,
                &mut MockOutboundOps::default(),
                SupervisorEvent::Control(msg),
            )
            .expect("event");
        assert_eq!(outcome.handled, 1);
        assert!(supervisor.status_for(2).is_some());
    }

    #[test]
    fn untrusted_control_sender_is_rejected() {
        let handoff =
            InitFaultHandoff::new(1, CapId(10), CapId(11), CapId(12), CapId(13), CapId(14), 20);
        let mut supervisor = SupervisorService::new(1, handoff, CoreServicePolicyTable::baseline());
        let mut ops = MockOutboundOps::default();
        let msg = Message::with_header(
            99,
            SUPERVISOR_OP_QUERY_STATUS,
            0,
            None,
            &SupervisorStatusRequest { tid: 2 }.encode(),
        )
        .expect("msg");
        assert_eq!(
            supervisor.handle_control_request(&mut ops, msg),
            Err(KernelError::MissingRight)
        );
    }

    #[test]
    fn production_style_task_exited_event_reaches_handle_task_exit() {
        let handoff =
            InitFaultHandoff::new(1, CapId(10), CapId(11), CapId(12), CapId(13), CapId(14), 20);
        let mut supervisor = SupervisorService::new(1, handoff, CoreServicePolicyTable::baseline());
        supervisor
            .register_core_service(CoreServiceKind::ProcessManager, 2, 1, 0)
            .expect("register");
        let msg = Message::with_header(
            2,
            SUPERVISOR_OP_TASK_EXITED,
            0,
            None,
            &TaskExitedEvent {
                tid: 2,
                exit_code: 9,
                restart_token: 0x2222,
            }
            .encode(),
        )
        .expect("msg");
        let mut ops = MockOutboundOps::default();
        let outcome = supervisor
            .handle_supervisor_event(
                &mut ops,
                &mut MockOutboundOps::default(),
                SupervisorEvent::Fault(msg),
            )
            .expect("event");
        assert_eq!(outcome.handled, 1);
        assert_eq!(
            supervisor
                .status_for(2)
                .expect("status")
                .pending_restart_due,
            10
        );
    }

    #[test]
    fn accepted_kernel_fault_report_schedules_restart_attempt_one() {
        let handoff =
            InitFaultHandoff::new(1, CapId(10), CapId(11), CapId(12), CapId(13), CapId(14), 20);
        let mut supervisor = SupervisorService::new(1, handoff, CoreServicePolicyTable::baseline());
        supervisor
            .register_driver(
                10008,
                ServiceRestartPolicy {
                    max_restarts: 3,
                    backoff_ticks: 0,
                },
                13,
                0,
                DriverRecoveryPlan {
                    irq_line: 0,
                    mem_cap: CapId(0),
                    dma_len: 0,
                    iova_cap: CapId(0),
                    iova_base: 0,
                    iova_len: 0,
                },
            )
            .expect("register crash-test service");
        let fault = SupervisorFaultReportWire {
            tid: 10008,
            fault_addr: 0,
            access: 2,
            flags: 0,
        };
        let msg =
            Message::with_header(0, SUPERVISOR_OP_FAULT_REPORT_WIRE, 0, None, &fault.encode())
                .expect("fault message");
        let mut ops = MockOutboundOps::default();
        let outcome = supervisor
            .handle_supervisor_event(
                &mut ops,
                &mut MockOutboundOps::default(),
                SupervisorEvent::Fault(msg),
            )
            .expect("accepted kernel fault");

        assert_eq!(outcome.handled, 1);
        let status = supervisor.status_for(10008).expect("record");
        assert_eq!(status.restart_attempts, 1);
        assert_eq!(status.max_restarts, 3);
        assert_eq!(status.pending_restart_due, 0);
        assert_eq!(status.pending_restart_token, 0xAA00 + 10008);
        assert_eq!(
            status.pm_restart_state,
            SupervisorPmRestartState::PendingDue as u8
        );
    }

    #[test]
    fn dependent_restart_without_dependent_token_is_blocked() {
        let handoff =
            InitFaultHandoff::new(1, CapId(10), CapId(11), CapId(12), CapId(13), CapId(14), 20);
        let mut supervisor = SupervisorService::new(1, handoff, CoreServicePolicyTable::baseline());
        supervisor
            .register_core_service(CoreServiceKind::Vfs, 3, 1, 0)
            .expect("vfs");
        supervisor
            .register_driver(
                20,
                ServiceRestartPolicy {
                    max_restarts: 2,
                    backoff_ticks: 3,
                },
                1,
                DEP_VFS,
                DriverRecoveryPlan {
                    irq_line: 1,
                    mem_cap: CapId(1),
                    dma_len: 4096,
                    iova_cap: CapId(2),
                    iova_base: 0,
                    iova_len: 4096,
                },
            )
            .expect("driver");
        let mut ops = MissingDependentTokenOps;
        supervisor
            .handle_task_exit(
                &mut ops,
                TaskExitedEvent {
                    tid: 3,
                    exit_code: 7,
                    restart_token: 0x3333,
                },
            )
            .expect("exit");
        assert_eq!(
            supervisor
                .status_for(20)
                .expect("dependent")
                .pending_restart_due,
            0
        );
    }

    #[test]
    fn failed_outbound_alert_does_not_pretend_degraded_alert_delivered() {
        let handoff =
            InitFaultHandoff::new(1, CapId(10), CapId(11), CapId(12), CapId(13), CapId(14), 20);
        let mut supervisor = SupervisorService::new(1, handoff, CoreServicePolicyTable::baseline());
        supervisor
            .register_core_service(CoreServiceKind::ProcessManager, 2, 1, 0)
            .expect("register");
        supervisor.policies.process_manager.max_restarts = 0;
        let mut ops = FailingOutboundOps;
        assert_eq!(
            supervisor.handle_task_exit(
                &mut ops,
                TaskExitedEvent {
                    tid: 2,
                    exit_code: 1,
                    restart_token: 2
                }
            ),
            Err(KernelError::InvalidCapability)
        );
        assert!(
            !supervisor.degraded(),
            "state must not commit degraded after failed outbound ops"
        );
    }

    #[test]
    fn fault_sender_mismatch_is_rejected() {
        let handoff =
            InitFaultHandoff::new(1, CapId(10), CapId(11), CapId(12), CapId(13), CapId(14), 20);
        let mut supervisor = SupervisorService::new(1, handoff, CoreServicePolicyTable::baseline());
        supervisor
            .register_core_service(CoreServiceKind::ProcessManager, 2, 1, 0)
            .expect("register");
        let msg = Message::with_header(
            99,
            SUPERVISOR_OP_TASK_EXITED,
            0,
            None,
            &TaskExitedEvent {
                tid: 2,
                exit_code: 1,
                restart_token: 2,
            }
            .encode(),
        )
        .expect("msg");
        let mut ops = MockOutboundOps::default();
        assert_eq!(
            supervisor.handle_supervisor_event(
                &mut ops,
                &mut MockOutboundOps::default(),
                SupervisorEvent::Fault(msg)
            ),
            Err(KernelError::MissingRight)
        );
    }

    #[test]
    fn fault_access_encode_decode_constants_are_stable() {
        for access in [FaultAccess::Read, FaultAccess::Write, FaultAccess::Execute] {
            let mut payload = [0u8; SUPERVISOR_FAULT_REPORT_WIRE_LEN];
            payload[SUPERVISOR_FAULT_REPORT_TID_START..SUPERVISOR_FAULT_REPORT_TID_END]
                .copy_from_slice(&2u64.to_le_bytes());
            payload[SUPERVISOR_FAULT_REPORT_ADDR_START..SUPERVISOR_FAULT_REPORT_ADDR_END]
                .copy_from_slice(&0x55u64.to_le_bytes());
            payload[SUPERVISOR_FAULT_REPORT_ACCESS_INDEX] = access.wire();
            let decoded = SupervisorFaultReportWire::decode(&payload).expect("decode");
            assert_eq!(decoded.access, access);
            assert_eq!(
                (decoded.synthetic_exit_code() >> SUPERVISOR_FAULT_EXIT_CODE_ACCESS_SHIFT) & 0xF,
                access.wire() as u64
            );
        }
        let mut bad = [0u8; SUPERVISOR_FAULT_REPORT_WIRE_LEN];
        bad[SUPERVISOR_FAULT_REPORT_ACCESS_INDEX] = 3;
        assert!(SupervisorFaultReportWire::decode(&bad).is_none());
    }

    #[test]
    fn logical_tick_executes_due_restart_through_shared_step() {
        let handoff =
            InitFaultHandoff::new(1, CapId(10), CapId(11), CapId(12), CapId(13), CapId(14), 20);
        let mut supervisor = SupervisorService::new(1, handoff, CoreServicePolicyTable::baseline());
        supervisor
            .register_core_service(CoreServiceKind::ProcessManager, 2, 1, 0)
            .expect("register");
        let mut ops = MockOutboundOps::default();
        supervisor
            .handle_task_exit(
                &mut ops,
                TaskExitedEvent {
                    tid: 2,
                    exit_code: 1,
                    restart_token: 0x2222,
                },
            )
            .expect("exit");
        for _ in 0..10 {
            let mut outbound = MockOutboundOps::default();
            let mut restart = MockOutboundOps::default();
            let _ = supervisor
                .handle_supervisor_event(&mut outbound, &mut restart, SupervisorEvent::Tick)
                .expect("tick");
        }
        assert_eq!(supervisor.current_tick(), TickInstant(10));
        assert_eq!(
            supervisor
                .status_for(2)
                .expect("status")
                .pending_restart_due,
            0
        );
    }

    #[test]
    fn long_running_loop_registers_services_from_init_requests() {
        run_with_large_stack(|| {
            let mut kernel = yarm::std::boxed::Box::new(Bootstrap::init().expect("init"));
            let (_, _supervisor_fault_send_cap, supervisor_fault_recv_cap) =
                kernel.create_endpoint(8).expect("fault endpoint");
            let (_, supervisor_control_send_cap, supervisor_control_recv_cap) =
                kernel.create_endpoint(8).expect("control endpoint");
            let (_, init_alert_send_cap, init_alert_recv_cap) =
                kernel.create_endpoint(8).expect("init alert endpoint");
            let handoff = InitFaultHandoff::new(
                1,
                supervisor_fault_recv_cap,
                supervisor_control_send_cap,
                supervisor_control_recv_cap,
                init_alert_send_cap,
                init_alert_recv_cap,
                20,
            );
            let mut supervisor = yarm::std::boxed::Box::new(SupervisorService::new(
                1,
                handoff,
                CoreServicePolicyTable::baseline(),
            ));

            let register_proc = Message::with_header(
                1,
                SUPERVISOR_OP_REGISTER_CORE_SERVICE,
                0,
                None,
                &RegisterCoreServiceRequest {
                    tid: 2,
                    kind: CoreServiceRegistrationKind::ProcessManager,
                    max_restarts: 3,
                    restart_group: 1,
                    dependency_mask: 0,
                    backoff_ticks: 10,
                }
                .encode(),
            )
            .expect("proc registration");
            let register_vfs = Message::with_header(
                1,
                SUPERVISOR_OP_REGISTER_CORE_SERVICE,
                0,
                None,
                &RegisterCoreServiceRequest {
                    tid: 3,
                    kind: CoreServiceRegistrationKind::Vfs,
                    max_restarts: 3,
                    restart_group: 1,
                    dependency_mask: 0,
                    backoff_ticks: 10,
                }
                .encode(),
            )
            .expect("vfs registration");
            let register_supervisor = Message::with_header(
                1,
                SUPERVISOR_OP_REGISTER_CORE_SERVICE,
                0,
                None,
                &RegisterCoreServiceRequest {
                    tid: 4,
                    kind: CoreServiceRegistrationKind::Supervisor,
                    max_restarts: 8,
                    restart_group: 2,
                    dependency_mask: 0,
                    backoff_ticks: 5,
                }
                .encode(),
            )
            .expect("supervisor registration");
            kernel
                .ipc_send(supervisor_control_send_cap, register_proc)
                .expect("send proc registration");
            kernel
                .ipc_send(supervisor_control_send_cap, register_vfs)
                .expect("send vfs registration");
            kernel
                .ipc_send(supervisor_control_send_cap, register_supervisor)
                .expect("send supervisor registration");

            assert_eq!(supervisor.run_until_idle(&mut kernel).expect("loop"), 3);
            assert!(supervisor.status_for(2).is_some());
            assert!(supervisor.status_for(3).is_some());
            assert!(supervisor.status_for(4).is_some());
        });
    }

    #[test]
    fn fault_wire_opcode_zero_is_decoded_and_routed_to_restart_path() {
        run_with_large_stack(|| {
            let mut kernel = yarm::std::boxed::Box::new(Bootstrap::init().expect("init"));
            let (_fault_eid, fault_send_cap, fault_recv_cap) =
                kernel.create_endpoint(8).expect("fault endpoint");
            let (_control_eid, control_send_cap, control_recv_cap) =
                kernel.create_endpoint(8).expect("control endpoint");
            let (_alert_eid, init_alert_send_cap, init_alert_recv_cap) =
                kernel.create_endpoint(8).expect("init alert endpoint");
            let handoff = InitFaultHandoff::new(
                1,
                fault_recv_cap,
                control_send_cap,
                control_recv_cap,
                init_alert_send_cap,
                init_alert_recv_cap,
                20,
            );
            let mut supervisor = yarm::std::boxed::Box::new(SupervisorService::new(
                1,
                handoff,
                CoreServicePolicyTable::baseline(),
            ));

            let register_proc = Message::with_header(
                1,
                SUPERVISOR_OP_REGISTER_CORE_SERVICE,
                0,
                None,
                &RegisterCoreServiceRequest {
                    tid: 2,
                    kind: CoreServiceRegistrationKind::ProcessManager,
                    max_restarts: 3,
                    restart_group: 1,
                    dependency_mask: 0,
                    backoff_ticks: 10,
                }
                .encode(),
            )
            .expect("registration");
            kernel
                .ipc_send(control_send_cap, register_proc)
                .expect("send registration");
            supervisor.run_until_idle(&mut kernel).expect("seed loop");

            let token = kernel.exit_task(2, 7).expect("exit for restart token");
            let _ = kernel
                .try_ipc_recv(fault_recv_cap)
                .expect("drain exit event")
                .expect("exit event");

            let mut payload = [0u8; SUPERVISOR_FAULT_REPORT_WIRE_LEN];
            payload[SUPERVISOR_FAULT_REPORT_TID_START..SUPERVISOR_FAULT_REPORT_TID_END]
                .copy_from_slice(&2u64.to_le_bytes());
            payload[SUPERVISOR_FAULT_REPORT_ADDR_START..SUPERVISOR_FAULT_REPORT_ADDR_END]
                .copy_from_slice(&0xDEAD_BEEFu64.to_le_bytes());
            payload[SUPERVISOR_FAULT_REPORT_ACCESS_INDEX] = 1;
            kernel
                .ipc_send(
                    fault_send_cap,
                    Message::with_header(0, SUPERVISOR_OP_FAULT_REPORT_WIRE, 0, None, &payload)
                        .expect("fault wire msg"),
                )
                .expect("send fault wire");

            supervisor
                .run_until_idle(&mut kernel)
                .expect("process fault wire");
            let status = supervisor.status_for(2).expect("status");
            assert_eq!(status.restart_attempts, 1);
            assert_eq!(kernel.task_restart_token(2), Some(token));
        });
    }

    #[test]
    #[ignore = "stack-heavy supervisor integration path overflows in hosted-dev unit-test harness"]
    fn exited_service_produces_and_processes_supervisor_event() {
        let (mut kernel, _init, handoff, mut supervisor) = setup_supervisor();
        supervisor.run_until_idle(&mut kernel).expect("loop");
        let token = kernel.exit_task(2, 7).expect("exit");
        let raw = kernel
            .try_ipc_recv(handoff.supervisor_fault_recv_cap)
            .expect("recv")
            .expect("msg");
        let event = TaskExitedEvent::decode(raw.as_slice()).expect("event");
        assert_eq!(event.restart_token, token);
        kernel
            .ipc_send(handoff.supervisor_fault_recv_cap, raw)
            .expect_err("recv cap cannot send");
    }

    #[test]
    #[ignore = "stack-heavy supervisor integration path overflows in hosted-dev unit-test harness"]
    fn restart_window_and_backoff_are_enforced() {
        let (mut kernel, _init, _handoff, mut supervisor) = setup_supervisor();
        supervisor.run_until_idle(&mut kernel).expect("loop");
        let token = kernel.exit_task(2, 9).expect("exit");
        let mut task_exit_ops = KernelSupervisorOutboundMessageOps::new(&mut kernel);
        let decision = supervisor
            .handle_task_exit(
                &mut task_exit_ops,
                TaskExitedEvent {
                    tid: 2,
                    exit_code: 9,
                    restart_token: token,
                },
            )
            .expect("decision");
        match decision {
            SupervisorDecision::ScheduledRestart { due_tick, .. } => {
                assert_eq!(due_tick, TickInstant(10));
            }
            _ => panic!("expected scheduled restart"),
        }
        assert_eq!(
            kernel.task_status(2).map(map_task_status),
            Some(TaskStatus::Exited(9))
        );
        supervisor.run_until_idle(&mut kernel).expect("idle");
        assert_eq!(supervisor.current_tick(), TickInstant(10));
        assert_eq!(
            kernel.task_status(2).map(map_task_status),
            Some(TaskStatus::Runnable)
        );
    }

    #[test]
    #[ignore = "stack-heavy supervisor integration path overflows in hosted-dev unit-test harness"]
    fn dependency_aware_restart_groups_restart_dependents() {
        let (mut kernel, _init, _handoff, mut supervisor) = setup_supervisor();
        supervisor.run_until_idle(&mut kernel).expect("loop");
        let token_vfs = kernel.exit_task(3, 1).expect("vfs exit");
        let token_proc = kernel.exit_task(2, 2).expect("proc exit");
        let mut task_exit_ops = KernelSupervisorOutboundMessageOps::new(&mut kernel);
        let _ = supervisor
            .handle_task_exit(
                &mut task_exit_ops,
                TaskExitedEvent {
                    tid: 3,
                    exit_code: 1,
                    restart_token: token_vfs,
                },
            )
            .expect("schedule");
        let proc_status = supervisor.status_for(2).expect("proc status");
        assert_eq!(proc_status.restart_group, 1);
        assert_eq!(kernel.task_restart_token(2), Some(token_proc));
    }

    #[test]
    #[ignore = "stack-heavy supervisor integration path overflows in hosted-dev unit-test harness"]
    fn actual_init_supervisor_alert_delivery_and_status_query_work() {
        let (mut kernel, _init, handoff, mut supervisor) = setup_supervisor();
        supervisor.run_until_idle(&mut kernel).expect("loop");
        let query = query_status_message(1, SupervisorStatusRequest { tid: 2 }).expect("query");
        kernel
            .ipc_send(handoff.supervisor_control_send_cap, query)
            .expect("send");
        supervisor
            .run_until_idle(&mut kernel)
            .expect("process query");
        let reply = kernel
            .try_ipc_recv(handoff.init_alert_recv_cap)
            .expect("recv")
            .expect("reply");
        assert_eq!(reply.opcode, SUPERVISOR_OP_QUERY_STATUS);
        let status = SupervisorStatusReply::decode(reply.as_slice()).expect("status");
        assert_eq!(status.tid, 2);
        assert_eq!(status.max_restarts, 3);
        assert_eq!(status.restart_owner, 2);
    }

    #[test]
    #[ignore = "stack-heavy supervisor integration path overflows in hosted-dev unit-test harness"]
    fn transfer_revocation_events_are_observable_without_breaking_supervisor_loop() {
        let (mut kernel, _init, handoff, mut supervisor) = setup_supervisor();
        supervisor.run_until_idle(&mut kernel).expect("loop");
        let msg = transfer_revoked_message(
            0,
            TransferRevokedEvent {
                owner_pid: 2,
                cap: 9,
                base: 0xA000,
                len: PAGE_SIZE as u64,
            },
        )
        .expect("event");
        kernel
            .report_transfer_revoke_to_supervisor(2, 9, 0xA000, PAGE_SIZE as u64)
            .expect("send");
        let queued = kernel
            .try_ipc_recv(handoff.supervisor_fault_recv_cap)
            .expect("recv")
            .expect("queued");
        assert_eq!(queued, msg);
        kernel
            .report_transfer_revoke_to_supervisor(2, 9, 0xA000, PAGE_SIZE as u64)
            .expect("requeue");
        let handled = supervisor.service_step(&mut kernel).expect("step");
        assert_eq!(handled, 1);
    }

    #[test]
    fn automatic_driver_redelegation_runs_after_restart() {
        run_with_large_stack(|| {
            let mut kernel = yarm::std::boxed::Box::new(Bootstrap::init().expect("init"));
            let owner_tid = 31;
            let (mem_cap, iova_cap) = enter_delegation_owner_context(&mut kernel, owner_tid);
            let (_, _supervisor_fault_send_cap, supervisor_fault_recv_cap) =
                kernel.create_endpoint(8).expect("fault endpoint");
            let (_, supervisor_control_send_cap, supervisor_control_recv_cap) =
                kernel.create_endpoint(8).expect("control endpoint");
            let (_, init_alert_send_cap, init_alert_recv_cap) =
                kernel.create_endpoint(8).expect("init alert endpoint");
            let handoff = InitFaultHandoff::new(
                1,
                supervisor_fault_recv_cap,
                supervisor_control_send_cap,
                supervisor_control_recv_cap,
                init_alert_send_cap,
                init_alert_recv_cap,
                20,
            );
            let mut supervisor = yarm::std::boxed::Box::new(SupervisorService::new(
                1,
                handoff,
                CoreServicePolicyTable::baseline(),
            ));
            let register_vfs = Message::with_header(
                1,
                SUPERVISOR_OP_REGISTER_CORE_SERVICE,
                0,
                None,
                &RegisterCoreServiceRequest {
                    tid: 3,
                    kind: CoreServiceRegistrationKind::Vfs,
                    max_restarts: 3,
                    restart_group: 1,
                    dependency_mask: 0,
                    backoff_ticks: 10,
                }
                .encode(),
            )
            .expect("vfs registration");
            let register_driver = Message::with_header(
                1,
                SUPERVISOR_OP_REGISTER_DRIVER,
                0,
                None,
                &RegisterDriverRequest {
                    tid: 20,
                    max_restarts: 2,
                    restart_group: 2,
                    dependency_mask: DEP_VFS,
                    backoff_ticks: 3,
                    irq_line: 5,
                    mem_cap,
                    iova_cap,
                    iova_base: 0x4000,
                    dma_len: PAGE_SIZE as u64,
                    iova_len: PAGE_SIZE as u64,
                }
                .encode(),
            )
            .expect("driver registration");
            let mut outbound_ops = KernelSupervisorOutboundMessageOps::new(&mut kernel);
            supervisor
                .handle_control_request(&mut outbound_ops, register_vfs)
                .expect("register vfs");
            kernel
                .register_task_with_class(3, to_kernel_task_class(TaskClass::SystemServer))
                .expect("task 3");
            let mut outbound_ops = KernelSupervisorOutboundMessageOps::new(&mut kernel);
            supervisor
                .handle_control_request(&mut outbound_ops, register_driver)
                .expect("register driver");
            kernel
                .register_task_with_class(20, to_kernel_task_class(TaskClass::Driver))
                .expect("task 20");
            kernel.register_driver(20).expect("driver");
            restore_delegation_owner_context(&mut kernel, owner_tid);

            let token = kernel.exit_task(20, 11).expect("exit");
            let mut task_exit_ops = KernelSupervisorOutboundMessageOps::new(&mut kernel);
            let _ = supervisor
                .handle_task_exit(
                    &mut task_exit_ops,
                    TaskExitedEvent {
                        tid: 20,
                        exit_code: 11,
                        restart_token: token,
                    },
                )
                .expect("schedule");
            assert_eq!(
                kernel.task_status(20).map(map_task_status),
                Some(TaskStatus::Exited(11))
            );
            assert_eq!(
                kernel.task_class(20).map(map_task_class),
                Some(TaskClass::Driver)
            );
            restore_delegation_owner_context(&mut kernel, owner_tid);
            let handled = run_until_idle_with_progress_guard(
                &mut supervisor,
                &mut kernel,
                20,
                owner_tid,
                mem_cap,
                iova_cap,
                true,
                64,
            );
            assert!(handled >= 1);
            assert!(!supervisor.pending_redelegation(20));
            assert_eq!(
                kernel.task_status(20).map(map_task_status),
                Some(TaskStatus::Runnable)
            );
        });
    }

    #[test]
    #[ignore = "stack-heavy supervisor integration path overflows in hosted-dev unit-test harness"]
    fn degraded_service_alert_is_delivered_to_init() {
        let (mut kernel, _init, handoff, mut supervisor) = setup_supervisor();
        supervisor.run_until_idle(&mut kernel).expect("loop");
        supervisor.policies.process_manager.max_restarts = 0;
        let token = kernel.exit_task(2, 9).expect("exit");
        let mut task_exit_ops = KernelSupervisorOutboundMessageOps::new(&mut kernel);
        let decision = supervisor
            .handle_task_exit(
                &mut task_exit_ops,
                TaskExitedEvent {
                    tid: 2,
                    exit_code: 9,
                    restart_token: token,
                },
            )
            .expect("dead");
        assert_eq!(
            decision,
            SupervisorDecision::MarkedDead {
                tid: 2,
                kind: ManagedServiceKind::Core(CoreServiceKind::ProcessManager),
            }
        );
        let alert = kernel
            .try_ipc_recv(handoff.init_alert_recv_cap)
            .expect("recv")
            .expect("alert");
        assert_eq!(alert.opcode, SUPERVISOR_OP_INIT_ALERT);
        assert_eq!(
            InitAlert::decode(alert.as_slice()).expect("alert").kind,
            InitAlertKind::ServiceDegraded
        );
    }

    #[test]
    #[ignore = "stack-heavy supervisor integration path overflows in hosted-dev unit-test harness"]
    fn restarted_supervisor_rebuilds_state_from_init_replay() {
        let (mut kernel, mut init, handoff, mut supervisor) = setup_supervisor();
        supervisor.run_until_idle(&mut kernel).expect("loop");
        let token = kernel.exit_task(4, 99).expect("exit");
        init.recover_supervisor_failure(&mut kernel, token)
            .expect("recover");

        let mut restarted = SupervisorService::new(1, handoff, init.restart_policies());
        assert_eq!(restarted.run_until_idle(&mut kernel).expect("replay"), 3);
        assert!(restarted.status_for(2).is_some());
        assert!(restarted.status_for(3).is_some());
        assert!(restarted.status_for(4).is_some());
    }

    #[test]
    fn status_tracks_last_exit_and_restart_schedule() {
        run_with_large_stack(|| {
            let mut kernel = yarm::std::boxed::Box::new(Bootstrap::init().expect("init"));
            let (_, _supervisor_fault_send_cap, supervisor_fault_recv_cap) =
                kernel.create_endpoint(8).expect("fault endpoint");
            let (_, supervisor_control_send_cap, supervisor_control_recv_cap) =
                kernel.create_endpoint(8).expect("control endpoint");
            let (_, init_alert_send_cap, init_alert_recv_cap) =
                kernel.create_endpoint(8).expect("init alert endpoint");
            let handoff = InitFaultHandoff::new(
                1,
                supervisor_fault_recv_cap,
                supervisor_control_send_cap,
                supervisor_control_recv_cap,
                init_alert_send_cap,
                init_alert_recv_cap,
                20,
            );
            let mut supervisor = yarm::std::boxed::Box::new(SupervisorService::new(
                1,
                handoff,
                CoreServicePolicyTable::baseline(),
            ));
            let register_proc = Message::with_header(
                1,
                SUPERVISOR_OP_REGISTER_CORE_SERVICE,
                0,
                None,
                &RegisterCoreServiceRequest {
                    tid: 2,
                    kind: CoreServiceRegistrationKind::ProcessManager,
                    max_restarts: 3,
                    restart_group: 1,
                    dependency_mask: 0,
                    backoff_ticks: 10,
                }
                .encode(),
            )
            .expect("proc registration");
            kernel
                .ipc_send(supervisor_control_send_cap, register_proc)
                .expect("send proc registration");
            kernel
                .register_task_with_class(2, to_kernel_task_class(TaskClass::SystemServer))
                .expect("task 2");
            supervisor.run_until_idle(&mut kernel).expect("loop");

            let token = kernel.exit_task(2, 44).expect("exit");
            let mut task_exit_ops = KernelSupervisorOutboundMessageOps::new(&mut kernel);
            let _ = supervisor
                .handle_task_exit(
                    &mut task_exit_ops,
                    TaskExitedEvent {
                        tid: 2,
                        exit_code: 44,
                        restart_token: token,
                    },
                )
                .expect("schedule");
            let status = supervisor.status_for(2).expect("status");
            assert_eq!(status.last_exit_code, 44);
            assert_eq!(status.last_exit_tick, 0);
            assert_eq!(status.pending_restart_due, 10);
        });
    }

    #[test]
    #[ignore = "stack-heavy supervisor integration path overflows in hosted-dev unit-test harness"]
    fn live_loop_advances_time_and_executes_restarts() {
        let (mut kernel, _init, _handoff, mut supervisor) = setup_supervisor();
        supervisor.run_until_idle(&mut kernel).expect("loop");
        let token = kernel.exit_task(2, 1).expect("exit");
        let mut task_exit_ops = KernelSupervisorOutboundMessageOps::new(&mut kernel);
        let _ = supervisor
            .handle_task_exit(
                &mut task_exit_ops,
                TaskExitedEvent {
                    tid: 2,
                    exit_code: 1,
                    restart_token: token,
                },
            )
            .expect("schedule");
        let handled = supervisor
            .run_live_for_ticks(&mut kernel, 10)
            .expect("live");
        assert!(handled >= 1);
        assert_eq!(
            kernel.task_status(2).map(map_task_status),
            Some(TaskStatus::Runnable)
        );
    }

    #[test]
    fn driver_dependency_on_vfs_triggers_restart_group_recovery() {
        run_with_large_stack(|| {
            let mut kernel = yarm::std::boxed::Box::new(Bootstrap::init().expect("init"));
            let owner_tid = 31;
            let (mem_cap, iova_cap) = enter_delegation_owner_context(&mut kernel, owner_tid);
            let (_, _supervisor_fault_send_cap, supervisor_fault_recv_cap) =
                kernel.create_endpoint(8).expect("fault endpoint");
            let (_, supervisor_control_send_cap, supervisor_control_recv_cap) =
                kernel.create_endpoint(8).expect("control endpoint");
            let (_, init_alert_send_cap, init_alert_recv_cap) =
                kernel.create_endpoint(8).expect("init alert endpoint");
            let handoff = InitFaultHandoff::new(
                1,
                supervisor_fault_recv_cap,
                supervisor_control_send_cap,
                supervisor_control_recv_cap,
                init_alert_send_cap,
                init_alert_recv_cap,
                20,
            );
            let mut supervisor = yarm::std::boxed::Box::new(SupervisorService::new(
                1,
                handoff,
                CoreServicePolicyTable::baseline(),
            ));
            let register_vfs = Message::with_header(
                1,
                SUPERVISOR_OP_REGISTER_CORE_SERVICE,
                0,
                None,
                &RegisterCoreServiceRequest {
                    tid: 3,
                    kind: CoreServiceRegistrationKind::Vfs,
                    max_restarts: 3,
                    restart_group: 1,
                    dependency_mask: 0,
                    backoff_ticks: 10,
                }
                .encode(),
            )
            .expect("vfs registration");
            let register_driver = Message::with_header(
                1,
                SUPERVISOR_OP_REGISTER_DRIVER,
                0,
                None,
                &RegisterDriverRequest {
                    tid: 20,
                    max_restarts: 2,
                    restart_group: 1,
                    dependency_mask: DEP_VFS,
                    backoff_ticks: 3,
                    irq_line: 5,
                    mem_cap,
                    iova_cap,
                    iova_base: 0x4000,
                    dma_len: PAGE_SIZE as u64,
                    iova_len: PAGE_SIZE as u64,
                }
                .encode(),
            )
            .expect("driver registration");
            let mut outbound_ops = KernelSupervisorOutboundMessageOps::new(&mut kernel);
            supervisor
                .handle_control_request(&mut outbound_ops, register_vfs)
                .expect("register vfs");
            kernel.register_task(3).expect("task 3");
            let mut outbound_ops = KernelSupervisorOutboundMessageOps::new(&mut kernel);
            supervisor
                .handle_control_request(&mut outbound_ops, register_driver)
                .expect("register driver");
            kernel
                .register_task_with_class(20, to_kernel_task_class(TaskClass::Driver))
                .expect("task 20");
            kernel.register_driver(20).expect("driver");
            restore_delegation_owner_context(&mut kernel, owner_tid);

            let vfs_token = kernel.exit_task(3, 7).expect("vfs exit");
            let mut task_exit_ops = KernelSupervisorOutboundMessageOps::new(&mut kernel);
            let _ = supervisor
                .handle_task_exit(
                    &mut task_exit_ops,
                    TaskExitedEvent {
                        tid: 3,
                        exit_code: 7,
                        restart_token: vfs_token,
                    },
                )
                .expect("schedule");
            let status = supervisor.status_for(20).expect("status");
            assert_eq!(status.pending_restart_due, 3);
        });
    }

    #[test]
    fn fault_sender_validation_rejects_self_report_and_accepts_trusted_sources() {
        run_with_large_stack(|| {
            let (_kernel, _init, _handoff, supervisor) = setup_supervisor();
            assert!(supervisor.validate_fault_sender(3, 3, true).is_err());
            assert!(supervisor.validate_fault_sender(2, 3, true).is_ok());
            assert!(supervisor.validate_fault_sender(0, 3, true).is_ok());
            assert!(supervisor.validate_fault_sender(0, 3, false).is_err());
            assert!(supervisor.validate_fault_sender(99, 3, true).is_err());
            assert!(supervisor.validate_fault_sender(99, 2, true).is_err());
        });
    }

    #[test]
    fn invalid_fault_sender_does_not_prevent_later_due_restart_processing() {
        run_with_large_stack(|| {
            let (_kernel, _init, _handoff, mut supervisor) = setup_supervisor();
            let mut outbound = MockOutboundOps::default();
            supervisor
                .handle_task_exit(
                    &mut outbound,
                    TaskExitedEvent {
                        tid: 3,
                        exit_code: 7,
                        restart_token: 0x3333,
                    },
                )
                .expect("schedule restart");
            supervisor.current_tick = TickInstant(10);

            let invalid = Message::with_header(
                99,
                SUPERVISOR_OP_TASK_EXITED,
                0,
                None,
                &TaskExitedEvent {
                    tid: 3,
                    exit_code: 8,
                    restart_token: 0x4444,
                }
                .encode(),
            )
            .expect("invalid task-exited");
            let mut restart = MockOutboundOps::default();
            let outcome = supervisor
                .handle_supervisor_event(
                    &mut outbound,
                    &mut restart,
                    SupervisorEvent::Fault(invalid),
                )
                .expect("invalid sender is rejected without aborting the step");
            assert_eq!(outcome.handled, 1);

            let outcome = supervisor
                .handle_supervisor_event(&mut outbound, &mut restart, SupervisorEvent::Tick)
                .expect("tick still processes due restarts");
            assert_eq!(outcome.restarts_executed, 1);
            assert_eq!(supervisor.status_for(3).unwrap().pending_restart_due, 0);
        });
    }

    #[test]
    fn unavailable_pm_restart_blocks_without_repeated_execution_or_state_clear() {
        run_with_large_stack(|| {
            let (_kernel, _init, _handoff, mut supervisor) = setup_supervisor();
            let mut outbound = MockOutboundOps::default();
            supervisor
                .handle_task_exit(
                    &mut outbound,
                    TaskExitedEvent {
                        tid: 3,
                        exit_code: 7,
                        restart_token: 0x3333,
                    },
                )
                .expect("schedule restart");
            supervisor.current_tick = TickInstant(10);
            let mut restart = FailingRestartOps { attempts: 0 };
            assert_eq!(supervisor.execute_due_restarts(&mut restart).unwrap(), 0);
            assert_eq!(restart.attempts, 1);
            assert!(supervisor.status_for(3).unwrap().pending_restart_due > 0);
            assert_eq!(supervisor.execute_due_restarts(&mut restart).unwrap(), 0);
            assert_eq!(restart.attempts, 1);
            assert!(supervisor.status_for(3).unwrap().pending_restart_due > 0);
        });
    }

    #[test]
    fn init_alert_failure_preserves_degraded_state_after_confirmed_fault() {
        run_with_large_stack(|| {
            let (_kernel, _init, _handoff, mut supervisor) = setup_supervisor();
            supervisor.policies.vfs.max_restarts = 0;
            let mut ops = FailingOutboundOps;
            assert!(
                supervisor
                    .handle_task_exit(
                        &mut ops,
                        TaskExitedEvent {
                            tid: 3,
                            exit_code: 9,
                            restart_token: 0x3333,
                        },
                    )
                    .is_err()
            );
            assert!(supervisor.degraded);
            assert_eq!(supervisor.status_for(3).unwrap().pending_restart_due, 0);
        });
    }

    #[test]
    fn supervisor_source_guardrails_prevent_direct_pm_restart_bypass_and_log_spam() {
        let src = include_str!("service.rs");
        assert!(!src.contains("execute_restart_via_process_manager(\n                                                            &mut transport"));
        assert!(!src.contains("SUPERVISOR_PM_RESTART_IPC_DEFERRED_NO_PM_CLIENT"));
        assert!(!src.contains("SUPERVISOR_PM_RESTART_VALIDATION_DEFERRED"));
        assert!(src.contains("SUPERVISOR_RESTART_EXEC_DEFERRED_NO_PM_CLIENT tid={}"));
        assert!(src.contains("RestartBlockedNoPmClient"));
    }

    #[test]
    fn supervisor_source_guardrail_includes_query_status_reply_cap_compatibility_path() {
        let src = include_str!("service.rs");
        assert!(
            src.contains("request.transferred_cap()"),
            "supervisor query-status handling should inspect transferred reply-cap"
        );
        assert!(
            src.contains("kernel.ipc_reply("),
            "supervisor query-status handling should support reply-cap reply path"
        );
    }
}
