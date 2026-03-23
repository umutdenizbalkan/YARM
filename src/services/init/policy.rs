use crate::kernel::capabilities::CapId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitBootPhase {
    Uninitialized,
    CoreServicesRegistered,
    LaunchingCore,
    Running,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupCap {
    EndpointFactory,
    MemoryObjectFactory,
    IrqControl,
    Clock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupCapSet {
    pub endpoint_factory: bool,
    pub memory_object_factory: bool,
    pub irq_control: bool,
    pub clock: bool,
}

impl StartupCapSet {
    pub const fn core_required_minimum() -> Self {
        Self {
            endpoint_factory: true,
            memory_object_factory: true,
            irq_control: false,
            clock: false,
        }
    }

    pub const fn contains(self, cap: StartupCap) -> bool {
        match cap {
            StartupCap::EndpointFactory => self.endpoint_factory,
            StartupCap::MemoryObjectFactory => self.memory_object_factory,
            StartupCap::IrqControl => self.irq_control,
            StartupCap::Clock => self.clock,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreServiceKind {
    ProcessManager,
    Vfs,
    Supervisor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreLaunchStrategy {
    ProcessManagerFirst,
    SupervisorFirst,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceRestartPolicy {
    pub max_restarts: u8,
    pub backoff_ticks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreServicePolicyTable {
    pub process_manager: ServiceRestartPolicy,
    pub vfs: ServiceRestartPolicy,
    pub supervisor: ServiceRestartPolicy,
}

impl CoreServicePolicyTable {
    pub const fn baseline() -> Self {
        Self {
            process_manager: ServiceRestartPolicy {
                max_restarts: 3,
                backoff_ticks: 10,
            },
            vfs: ServiceRestartPolicy {
                max_restarts: 3,
                backoff_ticks: 10,
            },
            supervisor: ServiceRestartPolicy {
                max_restarts: 8,
                backoff_ticks: 5,
            },
        }
    }

    pub const fn policy_for(self, service: CoreServiceKind) -> ServiceRestartPolicy {
        match service {
            CoreServiceKind::ProcessManager => self.process_manager,
            CoreServiceKind::Vfs => self.vfs,
            CoreServiceKind::Supervisor => self.supervisor,
        }
    }

    pub const fn is_sane(self) -> bool {
        self.process_manager.max_restarts > 0
            && self.vfs.max_restarts > 0
            && self.supervisor.max_restarts > 0
            && self.process_manager.backoff_ticks > 0
            && self.vfs.backoff_ticks > 0
            && self.supervisor.backoff_ticks > 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitFaultHandoff {
    pub supervisor_tid: u64,
    pub supervisor_fault_recv_cap: CapId,
    pub restart_window_ticks: u64,
}

impl InitFaultHandoff {
    pub const fn new(
        supervisor_tid: u64,
        supervisor_fault_recv_cap: CapId,
        restart_window_ticks: u64,
    ) -> Self {
        Self {
            supervisor_tid,
            supervisor_fault_recv_cap,
            restart_window_ticks,
        }
    }
}
