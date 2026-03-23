use crate::kernel::boot::{DriverBundlePlan, KernelError, KernelState};
use crate::kernel::capabilities::CapId;
use crate::kernel::ipc::{Message, ThreadId};
use crate::kernel::supervisor_abi::{
    init_alert_message, status_reply_message, CoreServiceRegistrationKind, InitAlert,
    InitAlertKind, RedelegationAckRequest, RegisterCoreServiceRequest, RegisterDriverRequest,
    SupervisorStatusReply, SupervisorStatusRequest, TaskExitedEvent, DEP_PROCESS_MANAGER,
    DEP_SUPERVISOR, DEP_VFS, SUPERVISOR_OP_ACK_REDELEGATION, SUPERVISOR_OP_QUERY_STATUS,
    SUPERVISOR_OP_REGISTER_CORE_SERVICE, SUPERVISOR_OP_REGISTER_DRIVER,
};
use crate::kernel::time::{TickDuration, TickInstant};
use crate::services::init::{
    CoreServiceKind, CoreServicePolicyTable, InitFaultHandoff, RestartOwner, ServiceRestartPolicy,
};

const MAX_MANAGED_SERVICES: usize = 8;
const MAX_DEPENDENTS: usize = 8;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorService {
    init_tid: u64,
    handoff: InitFaultHandoff,
    policies: CoreServicePolicyTable,
    managed: [Option<ManagedServiceRecord>; MAX_MANAGED_SERVICES],
    degraded: bool,
    current_tick: TickInstant,
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
        }
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
        kernel: &mut KernelState,
        msg: Message,
    ) -> Result<(), KernelError> {
        kernel.ipc_send(self.handoff.init_alert_send_cap, msg)
    }

    fn send_init_alert(
        &mut self,
        kernel: &mut KernelState,
        alert: InitAlert,
    ) -> Result<(), KernelError> {
        let msg = init_alert_message(self.init_tid, alert).map_err(|_| KernelError::WrongObject)?;
        self.send_init_message(kernel, msg)
    }

    fn send_status_reply(
        &mut self,
        kernel: &mut KernelState,
        reply: SupervisorStatusReply,
    ) -> Result<(), KernelError> {
        let msg =
            status_reply_message(self.init_tid, reply).map_err(|_| KernelError::WrongObject)?;
        self.send_init_message(kernel, msg)
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

    fn handle_control_request(
        &mut self,
        kernel: &mut KernelState,
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
                self.send_status_reply(kernel, self.status_reply(record))?;
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

    fn execute_due_restarts(&mut self, kernel: &mut KernelState) -> Result<usize, KernelError> {
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
            let token = record
                .pending_restart_token
                .ok_or(KernelError::WrongObject)?;
            kernel.restart_task(record.tid, token)?;
            record.last_restart_tick = self.current_tick;
            record.pending_restart_due = None;
            record.pending_restart_token = None;
            if matches!(record.kind, ManagedServiceKind::Driver) {
                let plan = record.driver_plan;
                if let Some(plan) = plan {
                    let _ = kernel.delegate_driver_bundle(DriverBundlePlan {
                        server_tid: ThreadId(record.tid),
                        irq_line: plan.irq_line,
                        mem_cap: plan.mem_cap,
                        iova_cap: plan.iova_cap,
                        iova_base: plan.iova_base,
                        iova_len: plan.iova_len,
                    })?;
                    record.pending_redelegation = false;
                } else {
                    record.pending_redelegation = true;
                    self.send_init_alert(
                        kernel,
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

    fn next_due_tick(&self) -> Option<TickInstant> {
        self.managed
            .iter()
            .flatten()
            .filter_map(|record| record.pending_restart_due)
            .min_by_key(|tick| tick.0)
    }

    pub fn service_step(&mut self, kernel: &mut KernelState) -> Result<usize, KernelError> {
        let mut changed = 0usize;
        while let Some(request) = kernel.try_ipc_recv(self.handoff.supervisor_control_recv_cap)? {
            self.handle_control_request(kernel, request)?;
            changed += 1;
        }
        while let Some(message) = kernel.try_ipc_recv(self.handoff.supervisor_fault_recv_cap)? {
            let event =
                TaskExitedEvent::decode(message.as_slice()).ok_or(KernelError::WrongObject)?;
            let _ = self.handle_task_exit(kernel, event)?;
            changed += 1;
        }
        changed += self.execute_due_restarts(kernel)?;
        Ok(changed)
    }

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
        kernel: &mut KernelState,
        event: TaskExitedEvent,
    ) -> Result<SupervisorDecision, KernelError> {
        let Some(snapshot) = self.find_record(event.tid) else {
            return Ok(SupervisorDecision::Ignored { tid: event.tid });
        };
        if matches!(snapshot.kind, ManagedServiceKind::Core(kind) if CoreServicePolicyTable::restart_owner_for(kind) == RestartOwner::Init)
        {
            self.send_init_alert(
                kernel,
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
            kernel.mark_task_dead(event.tid)?;
            self.degraded = true;
            self.send_init_alert(
                kernel,
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
            let token = kernel
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

pub fn run() {
    let mut kernel = crate::kernel::boot::Bootstrap::init().expect("init");
    let mut init = crate::services::init::InitService::new();
    let graph = crate::services::init::CoreServiceGraph {
        init_tid: 1,
        process_manager_tid: 2,
        vfs_tid: 3,
        supervisor_tid: 4,
    };
    init.register_core_graph(&mut kernel, graph).expect("graph");
    init.launch_core_services(
        &mut kernel,
        crate::services::init::CoreServiceImagePlan {
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
    crate::yarm_log!(
        "supervisor.srv online: handled={}, degraded={}, tick={}",
        handled,
        supervisor.degraded(),
        supervisor.current_tick().0
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::supervisor_abi::{
        query_status_message, InitAlertKind, RegisterDriverRequest, SupervisorStatusRequest,
        SUPERVISOR_OP_INIT_ALERT, SUPERVISOR_OP_QUERY_STATUS,
    };
    use crate::kernel::task::{TaskClass, TaskStatus};
    use crate::kernel::vm::PAGE_SIZE;
    use crate::services::init::{CoreServiceGraph, CoreServiceImagePlan, InitService};

    fn setup_supervisor() -> (
        KernelState,
        InitService,
        InitFaultHandoff,
        SupervisorService,
    ) {
        let mut kernel = Bootstrap::init().expect("init");
        let mut init = InitService::new();
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
        let supervisor = SupervisorService::new(1, handoff, init.restart_policies());
        (kernel, init, handoff, supervisor)
    }

    #[test]
    fn long_running_loop_registers_services_from_init_requests() {
        let (mut kernel, _init, _handoff, mut supervisor) = setup_supervisor();
        assert_eq!(supervisor.run_until_idle(&mut kernel).expect("loop"), 3);
        assert!(supervisor.status_for(2).is_some());
        assert!(supervisor.status_for(3).is_some());
        assert!(supervisor.status_for(4).is_some());
    }

    #[test]
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
    fn restart_window_and_backoff_are_enforced() {
        let (mut kernel, _init, _handoff, mut supervisor) = setup_supervisor();
        supervisor.run_until_idle(&mut kernel).expect("loop");
        let token = kernel.exit_task(2, 9).expect("exit");
        let decision = supervisor
            .handle_task_exit(
                &mut kernel,
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
        assert_eq!(kernel.task_status(2), Some(TaskStatus::Exited(9)));
        supervisor.run_until_idle(&mut kernel).expect("idle");
        assert_eq!(supervisor.current_tick(), TickInstant(10));
        assert_eq!(kernel.task_status(2), Some(TaskStatus::Runnable));
    }

    #[test]
    fn dependency_aware_restart_groups_restart_dependents() {
        let (mut kernel, _init, _handoff, mut supervisor) = setup_supervisor();
        supervisor.run_until_idle(&mut kernel).expect("loop");
        let token_vfs = kernel.exit_task(3, 1).expect("vfs exit");
        let token_proc = kernel.exit_task(2, 2).expect("proc exit");
        let _ = supervisor
            .handle_task_exit(
                &mut kernel,
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
    fn automatic_driver_redelegation_runs_after_restart() {
        let (mut kernel, mut init, _handoff, mut supervisor) = setup_supervisor();
        kernel
            .register_task_with_class(20, TaskClass::Driver)
            .expect("task 20");
        kernel.register_driver(20).expect("driver");
        let (_id, mem) = kernel.alloc_anonymous_memory_object().expect("mem");
        let iova = kernel.create_iova_space_cap().expect("iova");
        init.register_driver_with_supervisor(
            &mut kernel,
            RegisterDriverRequest {
                tid: 20,
                max_restarts: 2,
                restart_group: 2,
                dependency_mask: DEP_VFS,
                backoff_ticks: 3,
                irq_line: 5,
                mem_cap: mem.0,
                iova_cap: iova.0,
                iova_base: 0x4000,
                iova_len: PAGE_SIZE as u64,
            },
        )
        .expect("register");
        supervisor.run_until_idle(&mut kernel).expect("loop");

        let token = kernel.exit_task(20, 11).expect("exit");
        let _ = supervisor
            .handle_task_exit(
                &mut kernel,
                TaskExitedEvent {
                    tid: 20,
                    exit_code: 11,
                    restart_token: token,
                },
            )
            .expect("schedule");
        supervisor.run_until_idle(&mut kernel).expect("restart");
        assert!(!supervisor.pending_redelegation(20));
    }

    #[test]
    fn degraded_service_alert_is_delivered_to_init() {
        let (mut kernel, _init, handoff, mut supervisor) = setup_supervisor();
        supervisor.run_until_idle(&mut kernel).expect("loop");
        supervisor.policies.process_manager.max_restarts = 0;
        let token = kernel.exit_task(2, 9).expect("exit");
        let decision = supervisor
            .handle_task_exit(
                &mut kernel,
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
        let (mut kernel, _init, _handoff, mut supervisor) = setup_supervisor();
        supervisor.run_until_idle(&mut kernel).expect("loop");
        let token = kernel.exit_task(2, 44).expect("exit");
        let _ = supervisor
            .handle_task_exit(
                &mut kernel,
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
    }

    #[test]
    fn live_loop_advances_time_and_executes_restarts() {
        let (mut kernel, _init, _handoff, mut supervisor) = setup_supervisor();
        supervisor.run_until_idle(&mut kernel).expect("loop");
        let token = kernel.exit_task(2, 1).expect("exit");
        let _ = supervisor
            .handle_task_exit(
                &mut kernel,
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
        assert_eq!(kernel.task_status(2), Some(TaskStatus::Runnable));
    }

    #[test]
    fn driver_dependency_on_vfs_triggers_restart_group_recovery() {
        let (mut kernel, mut init, _handoff, mut supervisor) = setup_supervisor();
        kernel
            .register_task_with_class(20, TaskClass::Driver)
            .expect("task 20");
        kernel.register_driver(20).expect("driver");
        let (_id, mem) = kernel.alloc_anonymous_memory_object().expect("mem");
        let iova = kernel.create_iova_space_cap().expect("iova");
        init.register_driver_with_supervisor(
            &mut kernel,
            RegisterDriverRequest {
                tid: 20,
                max_restarts: 2,
                restart_group: 1,
                dependency_mask: DEP_VFS,
                backoff_ticks: 3,
                irq_line: 5,
                mem_cap: mem.0,
                iova_cap: iova.0,
                iova_base: 0x4000,
                iova_len: PAGE_SIZE as u64,
            },
        )
        .expect("register");
        supervisor.run_until_idle(&mut kernel).expect("loop");
        let vfs_token = kernel.exit_task(3, 7).expect("vfs exit");
        let _ = supervisor
            .handle_task_exit(
                &mut kernel,
                TaskExitedEvent {
                    tid: 3,
                    exit_code: 7,
                    restart_token: vfs_token,
                },
            )
            .expect("schedule");
        let status = supervisor.status_for(20).expect("status");
        assert_eq!(status.pending_restart_due, 3);
    }
}
