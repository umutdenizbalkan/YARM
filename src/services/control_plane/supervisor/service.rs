use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::supervisor_abi::{
    InitAlert, InitAlertKind, TaskExitedEvent, init_alert_message,
};
use crate::services::init::{
    CoreServiceKind, CoreServicePolicyTable, InitFaultHandoff, ServiceRestartPolicy,
};

const MAX_MANAGED_SERVICES: usize = 8;
const MAX_PENDING_ALERTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedServiceKind {
    Core(CoreServiceKind),
    Driver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorDecision {
    Restarted {
        tid: u64,
        kind: ManagedServiceKind,
        backoff_ticks: u64,
        redelegation_required: bool,
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
struct ManagedServiceRecord {
    tid: u64,
    kind: ManagedServiceKind,
    restart_attempts: u8,
    pending_redelegation: bool,
    total_backoff_ticks: u64,
    driver_policy: Option<ServiceRestartPolicy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorService {
    init_tid: u64,
    handoff: InitFaultHandoff,
    policies: CoreServicePolicyTable,
    managed: [Option<ManagedServiceRecord>; MAX_MANAGED_SERVICES],
    degraded: bool,
    pending_alerts: [Option<InitAlert>; MAX_PENDING_ALERTS],
    pending_alert_count: usize,
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
            pending_alerts: [None; MAX_PENDING_ALERTS],
            pending_alert_count: 0,
        }
    }

    pub const fn degraded(&self) -> bool {
        self.degraded
    }

    pub fn register_core_service(
        &mut self,
        kind: CoreServiceKind,
        tid: u64,
    ) -> Result<(), KernelError> {
        self.register_record(ManagedServiceRecord {
            tid,
            kind: ManagedServiceKind::Core(kind),
            restart_attempts: 0,
            pending_redelegation: false,
            total_backoff_ticks: 0,
            driver_policy: None,
        })
    }

    pub fn register_driver(
        &mut self,
        tid: u64,
        policy: ServiceRestartPolicy,
    ) -> Result<(), KernelError> {
        self.register_record(ManagedServiceRecord {
            tid,
            kind: ManagedServiceKind::Driver,
            restart_attempts: 0,
            pending_redelegation: false,
            total_backoff_ticks: 0,
            driver_policy: Some(policy),
        })
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

    fn find_record_mut(&mut self, tid: u64) -> Option<&mut ManagedServiceRecord> {
        self.managed
            .iter_mut()
            .flatten()
            .find(|record| record.tid == tid)
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

    fn push_alert(&mut self, alert: InitAlert) {
        if self.pending_alert_count < self.pending_alerts.len() {
            self.pending_alerts[self.pending_alert_count] = Some(alert);
            self.pending_alert_count += 1;
        }
    }

    pub fn take_pending_alert(&mut self) -> Option<InitAlert> {
        if self.pending_alert_count == 0 {
            return None;
        }
        let alert = self.pending_alerts[0].take();
        let mut idx = 1;
        while idx < self.pending_alert_count {
            self.pending_alerts[idx - 1] = self.pending_alerts[idx].take();
            idx += 1;
        }
        self.pending_alert_count -= 1;
        self.pending_alerts[self.pending_alert_count] = None;
        alert
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
        self.managed
            .iter()
            .flatten()
            .find(|record| record.tid == tid)
            .map(|record| record.pending_redelegation)
            .unwrap_or(false)
    }

    pub fn handle_kernel_event(
        &mut self,
        kernel: &mut KernelState,
    ) -> Result<Option<SupervisorDecision>, KernelError> {
        let Some(message) = kernel.ipc_recv(self.handoff.supervisor_fault_recv_cap)? else {
            return Ok(None);
        };
        let event = TaskExitedEvent::decode(message.as_slice()).ok_or(KernelError::WrongObject)?;
        self.handle_task_exit(kernel, event).map(Some)
    }

    pub fn handle_task_exit(
        &mut self,
        kernel: &mut KernelState,
        event: TaskExitedEvent,
    ) -> Result<SupervisorDecision, KernelError> {
        let Some(snapshot) = self
            .managed
            .iter()
            .flatten()
            .find(|record| record.tid == event.tid)
            .copied()
        else {
            return Ok(SupervisorDecision::Ignored { tid: event.tid });
        };
        let policy = self.policy_for(snapshot);
        let record = self
            .find_record_mut(event.tid)
            .ok_or(KernelError::TaskMissing)?;
        let kind = record.kind;
        if record.restart_attempts < policy.max_restarts {
            record.restart_attempts = record.restart_attempts.saturating_add(1);
            record.total_backoff_ticks = record
                .total_backoff_ticks
                .saturating_add(policy.backoff_ticks);
            let redelegation_required = matches!(kind, ManagedServiceKind::Driver);
            if redelegation_required {
                record.pending_redelegation = true;
            }
            kernel.restart_task(event.tid, event.restart_token)?;
            if redelegation_required {
                self.push_alert(InitAlert {
                    tid: event.tid,
                    kind: InitAlertKind::RedelegationRequired,
                });
            }
            return Ok(SupervisorDecision::Restarted {
                tid: event.tid,
                kind,
                backoff_ticks: policy.backoff_ticks,
                redelegation_required,
            });
        }

        kernel.mark_task_dead(event.tid)?;
        self.degraded = true;
        self.push_alert(InitAlert {
            tid: event.tid,
            kind: InitAlertKind::ServiceDegraded,
        });
        Ok(SupervisorDecision::MarkedDead {
            tid: event.tid,
            kind,
        })
    }

    pub fn emit_init_alert_message(&mut self) -> Option<crate::kernel::ipc::Message> {
        let alert = self.take_pending_alert()?;
        init_alert_message(self.init_tid, None, alert).ok()
    }
}

pub fn run() {
    let handoff = InitFaultHandoff::new(4, crate::kernel::capabilities::CapId(0), 100);
    let mut supervisor = SupervisorService::new(1, handoff, CoreServicePolicyTable::baseline());
    let _ = supervisor.register_core_service(CoreServiceKind::ProcessManager, 2);
    let _ = supervisor.register_core_service(CoreServiceKind::Vfs, 3);
    crate::yarm_log!(
        "supervisor.srv scaffold online: init_tid={}, degraded={}",
        1,
        supervisor.degraded()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::task::{TaskClass, TaskStatus};
    use crate::kernel::vm::PAGE_SIZE;

    fn setup_supervisor() -> (KernelState, InitFaultHandoff, SupervisorService) {
        let mut kernel = Bootstrap::init().expect("init");
        for tid in [1u64, 2, 3, 4, 20] {
            kernel.register_task(tid).expect("task");
        }
        let (_, _, recv_cap) = kernel.create_endpoint(8).expect("endpoint");
        kernel.set_supervisor_endpoint(recv_cap).expect("bind");
        let handoff = InitFaultHandoff::new(4, recv_cap, 100);
        let mut supervisor = SupervisorService::new(1, handoff, CoreServicePolicyTable::baseline());
        supervisor
            .register_core_service(CoreServiceKind::ProcessManager, 2)
            .expect("register proc");
        supervisor
            .register_core_service(CoreServiceKind::Vfs, 3)
            .expect("register vfs");
        supervisor
            .register_core_service(CoreServiceKind::Supervisor, 4)
            .expect("register sup");
        (kernel, handoff, supervisor)
    }

    #[test]
    fn exited_service_produces_supervisor_event() {
        let (mut kernel, handoff, _supervisor) = setup_supervisor();
        let token = kernel.exit_task(2, 7).expect("exit");
        let msg = kernel
            .ipc_recv(handoff.supervisor_fault_recv_cap)
            .expect("recv")
            .expect("msg");
        let event = TaskExitedEvent::decode(msg.as_slice()).expect("event");
        assert_eq!(event.tid, 2);
        assert_eq!(event.exit_code, 7);
        assert_eq!(event.restart_token, token);
    }

    #[test]
    fn restart_within_budget_succeeds() {
        let (mut kernel, _handoff, mut supervisor) = setup_supervisor();
        let _token = kernel.exit_task(2, 9).expect("exit");
        let decision = supervisor
            .handle_kernel_event(&mut kernel)
            .expect("handle")
            .expect("decision");
        assert_eq!(
            decision,
            SupervisorDecision::Restarted {
                tid: 2,
                kind: ManagedServiceKind::Core(CoreServiceKind::ProcessManager),
                backoff_ticks: 10,
                redelegation_required: false,
            }
        );
        assert_eq!(kernel.task_status(2), Some(TaskStatus::Runnable));
    }

    #[test]
    fn exhausted_budget_marks_service_dead() {
        let (mut kernel, _handoff, mut supervisor) = setup_supervisor();
        supervisor.policies.process_manager.max_restarts = 1;
        let token = kernel.exit_task(2, 9).expect("exit");
        let _ = supervisor
            .handle_task_exit(
                &mut kernel,
                TaskExitedEvent {
                    tid: 2,
                    exit_code: 9,
                    restart_token: token,
                },
            )
            .expect("restart");
        let token = kernel.exit_task(2, 10).expect("exit2");
        let decision = supervisor
            .handle_task_exit(
                &mut kernel,
                TaskExitedEvent {
                    tid: 2,
                    exit_code: 10,
                    restart_token: token,
                },
            )
            .expect("mark dead");
        assert_eq!(
            decision,
            SupervisorDecision::MarkedDead {
                tid: 2,
                kind: ManagedServiceKind::Core(CoreServiceKind::ProcessManager),
            }
        );
        assert!(supervisor.degraded());
        assert_eq!(kernel.task_status(2), Some(TaskStatus::Dead));
    }

    #[test]
    fn driver_restart_requires_redelegation() {
        let (mut kernel, _handoff, mut supervisor) = setup_supervisor();
        kernel
            .register_task_with_class(20, TaskClass::Driver)
            .expect("task 20");
        kernel.register_driver(20).expect("driver");
        let (_id, mem) = kernel.alloc_anonymous_memory_object().expect("mem");
        let iova = kernel.create_iova_space_cap().expect("iova");
        let bundle = kernel
            .delegate_driver_bundle(crate::kernel::boot::DriverBundlePlan {
                server_tid: crate::kernel::ipc::ThreadId(20),
                irq_line: 5,
                mem_cap: mem,
                iova_cap: iova,
                iova_base: 0x4000,
                iova_len: PAGE_SIZE,
            })
            .expect("bundle");
        supervisor
            .register_driver(
                20,
                ServiceRestartPolicy {
                    max_restarts: 2,
                    backoff_ticks: 3,
                },
            )
            .expect("reg driver");

        let token = kernel.exit_task(20, 11).expect("exit");
        let decision = supervisor
            .handle_task_exit(
                &mut kernel,
                TaskExitedEvent {
                    tid: 20,
                    exit_code: 11,
                    restart_token: token,
                },
            )
            .expect("handle");
        assert_eq!(
            decision,
            SupervisorDecision::Restarted {
                tid: 20,
                kind: ManagedServiceKind::Driver,
                backoff_ticks: 3,
                redelegation_required: true,
            }
        );
        assert!(supervisor.pending_redelegation(20));
        assert_eq!(
            kernel.validate_driver_bundle_live(20, bundle),
            Err(KernelError::StaleCapability)
        );
        let alert = supervisor.take_pending_alert().expect("alert");
        assert_eq!(alert.kind, InitAlertKind::RedelegationRequired);
        assert!(supervisor.complete_redelegation(20));
        assert!(!supervisor.pending_redelegation(20));
    }

    #[test]
    fn init_alert_messages_encode_pending_notifications() {
        let (mut kernel, _handoff, mut supervisor) = setup_supervisor();
        supervisor.policies.process_manager.max_restarts = 0;
        let token = kernel.exit_task(2, 9).expect("exit");
        let _ = supervisor
            .handle_task_exit(
                &mut kernel,
                TaskExitedEvent {
                    tid: 2,
                    exit_code: 9,
                    restart_token: token,
                },
            )
            .expect("dead");
        let msg = supervisor.emit_init_alert_message().expect("msg");
        let alert = InitAlert::decode(msg.as_slice()).expect("alert");
        assert_eq!(alert.kind, InitAlertKind::ServiceDegraded);
    }
}
