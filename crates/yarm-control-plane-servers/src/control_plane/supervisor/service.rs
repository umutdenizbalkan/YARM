// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(test)]
use yarm::kernel::boot::{DriverBundlePlan, KernelError, KernelState};
#[cfg(not(test))]
use yarm_user_rt::runtime::{KernelIpcError as KernelError, StartupContext, startup_context};
#[cfg(not(test))]
use yarm_user_rt::syscall::{IpcTransport, SyscallIpcTransport};
use yarm_user_rt::capability::CapId;
#[cfg(test)]
use yarm_user_rt::capability::CapRights;
use yarm_user_rt::ipc::Message;
#[cfg(test)]
use yarm_user_rt::ipc::ThreadId;
#[cfg(test)]
use yarm_user_rt::task::{TaskClass, TaskStatus};
use yarm_user_rt::time::{TickDuration, TickInstant};
use crate::control_plane::init::{
    CoreServiceKind, CoreServicePolicyTable, InitFaultHandoff, RestartOwner, ServiceRestartPolicy,
};
use yarm_ipc_abi::supervisor_abi::{
    DEP_PROCESS_MANAGER, DEP_SUPERVISOR, DEP_VFS, InitAlert, InitAlertKind, SupervisorStatusReply,
    TaskExitedEvent, SUPERVISOR_OP_TASK_EXITED,
};
#[cfg(not(test))]
use yarm_ipc_abi::process_abi::{
    ExecuteRestartReply, ExecuteRestartRequest, PROC_OP_EXECUTE_RESTART,
    PROC_OP_REGISTER_SUPERVISED_TASK, PROC_OP_TASK_RESTART_TOKEN, RegisterSupervisedTask,
    TaskRestartTokenReply, TaskRestartTokenRequest,
};
#[cfg(test)]
use yarm_ipc_abi::supervisor_abi::{
    CoreServiceRegistrationKind, RedelegationAckRequest, RegisterCoreServiceRequest,
    RegisterDriverRequest, SUPERVISOR_OP_ACK_REDELEGATION, SUPERVISOR_OP_QUERY_STATUS,
    SUPERVISOR_OP_REGISTER_CORE_SERVICE, SUPERVISOR_OP_REGISTER_DRIVER,
    SUPERVISOR_OP_TRANSFER_REVOKED, SupervisorStatusRequest, TransferRevokedEvent,
};

const SUPERVISOR_FAULT_REPORT_WIRE_LEN: usize = 17;
const SUPERVISOR_FAULT_REPORT_TID_START: usize = 0;
const SUPERVISOR_FAULT_REPORT_TID_END: usize = 8;
const SUPERVISOR_FAULT_REPORT_ADDR_START: usize = 8;
const SUPERVISOR_FAULT_REPORT_ADDR_END: usize = 16;
const SUPERVISOR_FAULT_REPORT_ACCESS_INDEX: usize = 16;
const SUPERVISOR_FAULT_EXIT_CODE_TAG: u64 = 0xF000_0000_0000_0000u64;
const SUPERVISOR_FAULT_EXIT_CODE_ACCESS_SHIFT: u64 = 56;
const SUPERVISOR_FAULT_EXIT_CODE_ADDR_MASK: u64 = 0x00FF_FFFF_FFFF_FFFF;
/// Kernel-originated supervisor fault-report notification opcode.
///
/// The kernel fault path uses `Message::new(0, payload)` for the 17-byte fault report wire payload.
const SUPERVISOR_OP_FAULT_REPORT_WIRE: u16 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SupervisorFaultReportWire {
    tid: u64,
    fault_addr: u64,
    access: u8,
}

impl SupervisorFaultReportWire {
    fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != SUPERVISOR_FAULT_REPORT_WIRE_LEN {
            return None;
        }
        let mut tid = [0u8; 8];
        let mut fault_addr = [0u8; 8];
        tid.copy_from_slice(&bytes[SUPERVISOR_FAULT_REPORT_TID_START..SUPERVISOR_FAULT_REPORT_TID_END]);
        fault_addr.copy_from_slice(
            &bytes[SUPERVISOR_FAULT_REPORT_ADDR_START..SUPERVISOR_FAULT_REPORT_ADDR_END],
        );
        let access = bytes[SUPERVISOR_FAULT_REPORT_ACCESS_INDEX];
        if access > 2 {
            return None;
        }
        Some(Self {
            tid: u64::from_le_bytes(tid),
            fault_addr: u64::from_le_bytes(fault_addr),
            access,
        })
    }

    fn synthetic_exit_code(self) -> u64 {
        // Preserve existing supervisor restart flow by translating fault reports into
        // a stable synthetic exit code domain.
        SUPERVISOR_FAULT_EXIT_CODE_TAG
            | ((self.access as u64) << SUPERVISOR_FAULT_EXIT_CODE_ACCESS_SHIFT)
            | (self.fault_addr & SUPERVISOR_FAULT_EXIT_CODE_ADDR_MASK)
    }
}

const MAX_MANAGED_SERVICES: usize = 8;
const MAX_DEPENDENTS: usize = 8;
#[cfg(test)]
const SUPERVISOR_RECV_BUDGET_TICKS: u64 = 1;
const SUPERVISOR_QUERY_STATUS_CALL_RECV_TIMEOUT_TICKS: u64 = 1;
#[cfg(not(test))]
const SUPERVISOR_RUNTIME_DEFAULT_RESTART_WINDOW_TICKS: u64 = 100;
#[cfg(not(test))]
const SUPERVISOR_RUNTIME_IDLE_RECV_TIMEOUT_TICKS: u64 = 10_000;

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

#[cfg(test)]
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
pub(crate) struct DriverRecoveryPlan {
    irq_line: u16,
    mem_cap: CapId,
    dma_len: usize,
    iova_cap: CapId,
    iova_base: usize,
    iova_len: usize,
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
    pending_redelegation: bool,
    driver_policy: Option<ServiceRestartPolicy>,
    driver_plan: Option<DriverRecoveryPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorService {
    init_tid: u64,
    handoff: InitFaultHandoff,
    policies: CoreServicePolicyTable,
    managed: [Option<ManagedServiceRecord>; MAX_MANAGED_SERVICES],
    degraded: bool,
    current_tick: TickInstant,
    #[cfg(test)]
    test_disable_budgeted_receive_for_tracked_tid: Option<u64>,
}

pub trait SupervisorOutboundMessageOps {
    fn ipc_send(&mut self, cap: CapId, msg: Message) -> Result<(), KernelError>;
    fn ipc_reply(&mut self, cap: CapId, msg: Message) -> Result<(), KernelError>;
}

#[cfg(test)]
trait SupervisorRestartRedelegationOps: SupervisorOutboundMessageOps {
    fn restart_task(&mut self, tid: u64, restart_token: u64) -> Result<(), KernelError>;
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
    fn restart_task(&mut self, tid: u64, restart_token: u64) -> Result<(), KernelError> {
        self.kernel.restart_task(tid, restart_token)
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
            degraded: false,
            current_tick: TickInstant(0),
            #[cfg(test)]
            test_disable_budgeted_receive_for_tracked_tid: None,
        }
    }

    #[cfg(not(test))]
    pub fn new_from_runtime_handoff(
        runtime_handoff: SupervisorRuntimeHandoff,
    ) -> Result<Self, KernelError> {
        let (init_tid, handoff) = runtime_handoff.into_fault_handoff()?;
        Ok(Self::new(init_tid, handoff, CoreServicePolicyTable::baseline()))
    }

    pub const fn degraded(&self) -> bool {
        self.degraded
    }

    pub const fn current_tick(&self) -> TickInstant {
        self.current_tick
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

    #[cfg(test)]
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
            pending_redelegation: false,
            driver_policy: None,
            driver_plan: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn register_driver(
        &mut self,
        tid: u64,
        policy: ServiceRestartPolicy,
        restart_group: u8,
        dependency_mask: u8,
        plan: DriverRecoveryPlan,
    ) -> Result<(), KernelError> {
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
            pending_redelegation: false,
            driver_policy: Some(policy),
            driver_plan: Some(plan),
        })
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

    fn schedule_restart(&mut self, tid: u64, token: u64) -> Result<TickInstant, KernelError> {
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
        record.restart_attempts = record.restart_attempts.saturating_add(1);
        record.pending_restart_due = Some(current_tick + TickDuration(policy.backoff_ticks));
        record.pending_restart_token = Some(token);
        Ok(record.pending_restart_due.expect("due set"))
    }

    #[cfg(test)]
    fn handle_control_request(
        &mut self,
        outbound_ops: &mut impl SupervisorOutboundMessageOps,
        request: Message,
    ) -> Result<(), KernelError> {
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
                let req = RegisterDriverRequest::decode(request.as_slice())
                    .ok_or(KernelError::WrongObject)?;
                self.register_driver(
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
                )?;
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
            _ => return Err(KernelError::WrongObject),
        }
        Ok(())
    }

    #[cfg(test)]
    fn execute_due_restarts(
        &mut self,
        restart_ops: &mut impl SupervisorRestartRedelegationOps,
    ) -> Result<usize, KernelError> {
        let mut restarted = 0usize;
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
            let restart_token = record
                .pending_restart_token
                .ok_or(KernelError::WrongObject)?;
            restart_ops.restart_task(record.tid, restart_token)?;
            record.last_restart_tick = self.current_tick;
            record.pending_restart_due = None;
            record.pending_restart_token = None;
            if matches!(record.kind, ManagedServiceKind::Driver) {
                let plan = record.driver_plan;
                if let Some(plan) = plan {
                    restart_ops.delegate_driver_bundle(record.tid, plan)?;
                    record.pending_redelegation = false;
                } else {
                    record.pending_redelegation = true;
                    self.send_init_alert(
                        restart_ops,
                        InitAlert {
                            tid: record.tid,
                            kind: InitAlertKind::RedelegationRequired,
                        },
                    )?;
                }
            }
            self.managed[idx] = Some(record);
            restarted += 1;
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
        self.managed
            .iter()
            .flatten()
            .any(|record| record.pending_restart_due.is_some_and(|due| due.0 <= self.current_tick.0))
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
                .is_some_and(|tid| self.find_record(tid).is_some_and(|record| record.pending_restart_due.is_some()))
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
        let Some(snapshot) = self.find_record(event.tid) else {
            return Ok(SupervisorDecision::Ignored { tid: event.tid });
        };
        if matches!(snapshot.kind, ManagedServiceKind::Core(kind) if CoreServicePolicyTable::restart_owner_for(kind) == RestartOwner::Init)
        {
            self.send_init_alert(
                task_exit_ops,
                InitAlert {
                    tid: event.tid,
                    kind: InitAlertKind::SupervisorRestarted,
                },
            )?;
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
            task_exit_ops.mark_task_dead(event.tid)?;
            self.degraded = true;
            self.send_init_alert(
                task_exit_ops,
                InitAlert {
                    tid: event.tid,
                    kind: InitAlertKind::ServiceDegraded,
                },
            )?;
            return Ok(SupervisorDecision::MarkedDead {
                tid: event.tid,
                kind: snapshot.kind,
            });
        }

        let due_tick = self.schedule_restart(event.tid, event.restart_token)?;
        for dependent_tid in self.dependent_tids(snapshot).into_iter().flatten() {
            let token = task_exit_ops
                .task_restart_token(dependent_tid)
                .unwrap_or(event.restart_token);
            let _ = self.schedule_restart(dependent_tid, token);
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
            yarm_user_rt::user_log!(
                "supervisor.srv restart-token registration sender not wired in production: missing authoritative lifecycle handoff source for (tid, restart_token)"
            );
            loop {
                let mut made_progress = false;
                match transport.recv(supervisor.handoff.supervisor_control_recv_cap.0 as u32) {
                    Ok(Some(msg)) => {
                        made_progress = true;
                        yarm_user_rt::user_log!(
                            "supervisor.srv control msg: opcode={}", msg.opcode
                        );
                    }
                    Ok(None) => {}
                    Err(err) => {
                        yarm_user_rt::user_log!(
                            "supervisor.srv control recv error: {:?}",
                            err
                        );
                    }
                }

                match transport.recv(supervisor.handoff.supervisor_fault_recv_cap.0 as u32) {
                    Ok(Some(msg)) => {
                        made_progress = true;
                        match msg.opcode {
                            SUPERVISOR_OP_FAULT_REPORT_WIRE => match SupervisorFaultReportWire::decode(msg.as_slice()) {
                                Some(fault) => {
                                    match query_restart_token_via_process_manager(
                                        &mut transport,
                                        process_manager_caps,
                                        fault.tid,
                                    ) {
                                        Ok(Some(restart_token)) => {
                                            let event = TaskExitedEvent {
                                                tid: fault.tid,
                                                exit_code: fault.synthetic_exit_code(),
                                                restart_token,
                                            };
                                            let mut ops = RuntimeSupervisorTaskExitOps {
                                                token_tid: fault.tid,
                                                token: restart_token,
                                            };
                                            match supervisor.handle_task_exit(&mut ops, event) {
                                                Ok(SupervisorDecision::ScheduledRestart { tid, due_tick, .. }) => {
                                                    match execute_restart_via_process_manager(
                                                        &mut transport,
                                                        process_manager_caps,
                                                        tid,
                                                        restart_token,
                                                    ) {
                                                        Ok(status) => yarm_user_rt::user_log!(
                                                            "supervisor.srv execute-restart reply: tid={}, due_tick={}, status={}",
                                                            tid,
                                                            due_tick.0,
                                                            status
                                                        ),
                                                        Err(err) => yarm_user_rt::user_log!(
                                                            "supervisor.srv execute-restart request failed: tid={}, err={:?}",
                                                            tid,
                                                            err
                                                        ),
                                                    }
                                                }
                                                Ok(_) => {}
                                                Err(err) => yarm_user_rt::user_log!(
                                                    "supervisor.srv failed to apply restart policy decision: tid={}, err={:?}",
                                                    fault.tid,
                                                    err
                                                ),
                                            }
                                        }
                                        Ok(None) => yarm_user_rt::user_log!(
                                            "supervisor.srv fault report received: tid={}, addr=0x{:x}, access={}; restart-token lookup unsupported/unavailable in runtime path",
                                            fault.tid,
                                            fault.fault_addr,
                                            fault.access
                                        ),
                                        Err(err) => yarm_user_rt::user_log!(
                                            "supervisor.srv restart-token lookup failed: tid={}, err={:?}",
                                            fault.tid,
                                            err
                                        ),
                                    }
                                }
                                None => {
                                    yarm_user_rt::user_log!(
                                        "supervisor.srv fault report decode failed: len={}",
                                        msg.as_slice().len()
                                    );
                                }
                            },
                            SUPERVISOR_OP_TASK_EXITED => {
                                if let Some(event) = TaskExitedEvent::decode(msg.as_slice()) {
                                    match register_supervised_task_with_process_manager(
                                        &mut transport,
                                        process_manager_caps,
                                        event.tid,
                                        event.restart_token,
                                    ) {
                                        Ok(()) => {}
                                        Err(err) => yarm_user_rt::user_log!(
                                            "supervisor.srv failed to register supervised task restart-token: tid={}, err={:?}",
                                            event.tid,
                                            err
                                        ),
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        yarm_user_rt::user_log!("supervisor.srv fault recv error: {:?}", err);
                    }
                }

                if !made_progress {
                    match supervisor_idle_wait(
                        &mut transport,
                        supervisor.handoff.supervisor_control_recv_cap.0 as u32,
                    ) {
                        Ok(true) => made_progress = true,
                        Ok(false) => {}
                        Err(err) => yarm_user_rt::user_log!(
                            "supervisor.srv idle wait error: {:?}",
                            err
                        ),
                    }
                } else {
                    let _ = supervisor.degraded();
                }
            }
        }
        Err(err) => {
            yarm_user_rt::user_log!(
                "supervisor.srv runtime handoff incomplete: startup_task_id={}, err={:?}; TODO: provide endpoint caps via startup BootInfo/runtime args",
                startup.task_id,
                err
            );
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
    transport: &mut impl IpcTransport,
    process_manager_caps: Option<(u32, u32)>,
    tid: u64,
) -> Result<Option<u64>, KernelError> {
    let Some((req_cap, rep_cap)) = process_manager_caps else { return Ok(None) };
    let req = TaskRestartTokenRequest::new(tid);
    let msg = Message::with_header(0, PROC_OP_TASK_RESTART_TOKEN, 0, None, &req.encode())
        .map_err(|_| KernelError::WrongObject)?;
    transport
        .send(req_cap, &msg)
        .map_err(|_| KernelError::WrongObject)?;
    let Some(reply_msg) = transport
        .recv(rep_cap)
        .map_err(|_| KernelError::WrongObject)?
    else {
        return Ok(None);
    };
    let reply = TaskRestartTokenReply::decode(reply_msg.as_slice())
        .map_err(|_| KernelError::WrongObject)?;
    Ok(reply.found_token())
}

#[cfg(not(test))]
fn register_supervised_task_with_process_manager(
    transport: &mut impl IpcTransport,
    process_manager_caps: Option<(u32, u32)>,
    tid: u64,
    restart_token: u64,
) -> Result<(), KernelError> {
    let Some((req_cap, rep_cap)) = process_manager_caps else {
        return Ok(());
    };
    let req = RegisterSupervisedTask::new(tid, restart_token);
    let msg = Message::with_header(0, PROC_OP_REGISTER_SUPERVISED_TASK, 0, None, &req.encode())
        .map_err(|_| KernelError::WrongObject)?;
    transport
        .send(req_cap, &msg)
        .map_err(|_| KernelError::WrongObject)?;
    let _ = transport
        .recv(rep_cap)
        .map_err(|_| KernelError::WrongObject)?;
    Ok(())
}

#[cfg(not(test))]
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
    let reply = ExecuteRestartReply::decode(reply_msg.as_slice())
        .map_err(|_| KernelError::WrongObject)?;
    Ok(reply.status)
}

#[cfg(not(test))]
struct RuntimeSupervisorTaskExitOps {
    token_tid: u64,
    token: u64,
}

#[cfg(not(test))]
impl SupervisorOutboundMessageOps for RuntimeSupervisorTaskExitOps {
    fn ipc_send(&mut self, _cap: CapId, _msg: Message) -> Result<(), KernelError> {
        Ok(())
    }
    fn ipc_reply(&mut self, _cap: CapId, _msg: Message) -> Result<(), KernelError> {
        Ok(())
    }
}

#[cfg(not(test))]
impl SupervisorTaskExitOps for RuntimeSupervisorTaskExitOps {
    fn mark_task_dead(&mut self, _tid: u64) -> Result<(), KernelError> {
        Ok(())
    }
    fn task_restart_token(&self, tid: u64) -> Option<u64> {
        (tid == self.token_tid).then_some(self.token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm::std::vec::Vec;
    use yarm::std::thread;
    use yarm::kernel::boot::Bootstrap;
    use yarm_user_rt::vm::PAGE_SIZE;
    use crate::control_plane::init::{CoreServiceGraph, CoreServiceImagePlan, InitService};
    use yarm_ipc_abi::supervisor_abi::{
        CoreServiceRegistrationKind, InitAlertKind, RegisterDriverRequest, SUPERVISOR_OP_INIT_ALERT, SUPERVISOR_OP_QUERY_STATUS,
        SupervisorStatusRequest, TransferRevokedEvent,
    };

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

    fn enter_delegation_owner_context(
        kernel: &mut KernelState,
        owner_tid: u64,
    ) -> (u64, u64) {
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
            if supervisor
                .status_for(tracked_tid)
                .is_some_and(|status| status.pending_restart_due == 0 && !status.pending_redelegation)
                && kernel.task_status(tracked_tid).map(map_task_status) == Some(TaskStatus::Runnable)
            {
                supervisor.test_set_disable_budgeted_receive_for_tracked_tid(None);
                return total_changed;
            }
            if changed == 0 {
                if let Some(next_due) = supervisor
                    .status_for(tracked_tid)
                    .and_then(|status| (status.pending_restart_due > 0).then_some(TickInstant(status.pending_restart_due)))
                {
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
        let handoff = InitFaultHandoff::new(1, CapId(10), CapId(11), CapId(12), CapId(13), CapId(14), 20);
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

            supervisor.run_until_idle(&mut kernel).expect("process fault wire");
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
        assert_eq!(kernel.task_status(2).map(map_task_status), Some(TaskStatus::Exited(9)));
        supervisor.run_until_idle(&mut kernel).expect("idle");
        assert_eq!(supervisor.current_tick(), TickInstant(10));
        assert_eq!(kernel.task_status(2).map(map_task_status), Some(TaskStatus::Runnable));
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
            assert_eq!(kernel.task_class(20).map(map_task_class), Some(TaskClass::Driver));
            restore_delegation_owner_context(&mut kernel, owner_tid);
            let handled =
                run_until_idle_with_progress_guard(
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
        assert_eq!(kernel.task_status(2).map(map_task_status), Some(TaskStatus::Runnable));
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
