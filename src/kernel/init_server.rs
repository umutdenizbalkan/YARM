use super::bootstrap::{KernelError, KernelState, ServiceRole};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitBootPhase {
    Uninitialized,
    CoreServicesRegistered,
    Running,
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
pub struct CoreServiceGraph {
    pub init_tid: u64,
    pub process_manager_tid: u64,
    pub vfs_tid: u64,
    pub supervisor_tid: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CoreServiceHandles {
    pub process_manager_tid: Option<u64>,
    pub vfs_tid: Option<u64>,
    pub supervisor_tid: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitServerLite {
    phase: InitBootPhase,
    handles: CoreServiceHandles,
    startup_caps: StartupCapSet,
}

impl Default for InitServerLite {
    fn default() -> Self {
        Self::new()
    }
}

impl InitServerLite {
    pub const fn new() -> Self {
        Self {
            phase: InitBootPhase::Uninitialized,
            handles: CoreServiceHandles {
                process_manager_tid: None,
                vfs_tid: None,
                supervisor_tid: None,
            },
            startup_caps: StartupCapSet::core_required_minimum(),
        }
    }

    pub const fn phase(&self) -> InitBootPhase {
        self.phase
    }

    pub const fn handles(&self) -> CoreServiceHandles {
        self.handles
    }

    pub const fn startup_caps(&self) -> StartupCapSet {
        self.startup_caps
    }

    pub fn set_startup_caps(&mut self, caps: StartupCapSet) {
        self.startup_caps = caps;
    }

    pub fn validate_startup_caps(&self) -> Result<(), KernelError> {
        let min = StartupCapSet::core_required_minimum();
        if min.endpoint_factory && !self.startup_caps.endpoint_factory {
            return Err(KernelError::MissingRight);
        }
        if min.memory_object_factory && !self.startup_caps.memory_object_factory {
            return Err(KernelError::MissingRight);
        }
        Ok(())
    }

    pub fn register_core_graph(
        &mut self,
        kernel: &mut KernelState,
        graph: CoreServiceGraph,
    ) -> Result<(), KernelError> {
        self.validate_startup_caps()?;
        kernel.register_task(graph.init_tid)?;
        kernel.register_task(graph.process_manager_tid)?;
        kernel.register_task(graph.vfs_tid)?;
        kernel.register_task(graph.supervisor_tid)?;

        kernel.register_service_role(graph.init_tid, ServiceRole::Init)?;
        kernel.register_service_role(graph.process_manager_tid, ServiceRole::ProcessManager)?;
        kernel.register_service_role(graph.vfs_tid, ServiceRole::Vfs)?;
        kernel.register_service_role(graph.supervisor_tid, ServiceRole::Supervisor)?;

        self.handles.process_manager_tid = Some(graph.process_manager_tid);
        self.handles.vfs_tid = Some(graph.vfs_tid);
        self.handles.supervisor_tid = Some(graph.supervisor_tid);
        self.phase = InitBootPhase::CoreServicesRegistered;
        Ok(())
    }

    pub fn begin_running(&mut self) -> Result<(), KernelError> {
        if self.phase != InitBootPhase::CoreServicesRegistered {
            return Err(KernelError::WrongObject);
        }
        self.phase = InitBootPhase::Running;
        Ok(())
    }

    pub fn validate_core_delegation_paths(
        &self,
        kernel: &KernelState,
        init_tid: u64,
    ) -> Result<(), KernelError> {
        let proc_tid = self
            .handles
            .process_manager_tid
            .ok_or(KernelError::WrongObject)?;
        let vfs_tid = self.handles.vfs_tid.ok_or(KernelError::WrongObject)?;
        let sup_tid = self
            .handles
            .supervisor_tid
            .ok_or(KernelError::WrongObject)?;

        kernel.validate_service_delegation(init_tid, proc_tid)?;
        kernel.validate_service_delegation(init_tid, vfs_tid)?;
        kernel.validate_service_delegation(init_tid, sup_tid)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::bootstrap::Bootstrap;

    #[test]
    fn init_server_requires_minimum_startup_caps() {
        let mut state = Bootstrap::init().expect("init");
        let mut init = InitServerLite::new();
        init.set_startup_caps(StartupCapSet {
            endpoint_factory: false,
            memory_object_factory: true,
            irq_control: false,
            clock: false,
        });
        let graph = CoreServiceGraph {
            init_tid: 1,
            process_manager_tid: 2,
            vfs_tid: 3,
            supervisor_tid: 4,
        };
        assert_eq!(
            init.register_core_graph(&mut state, graph),
            Err(KernelError::MissingRight)
        );
    }

    #[test]
    fn init_server_registers_core_graph_and_enters_running() {
        let mut state = Bootstrap::init().expect("init");
        let mut init = InitServerLite::new();
        let graph = CoreServiceGraph {
            init_tid: 1,
            process_manager_tid: 2,
            vfs_tid: 3,
            supervisor_tid: 4,
        };

        init.register_core_graph(&mut state, graph)
            .expect("register");
        assert_eq!(init.phase(), InitBootPhase::CoreServicesRegistered);
        init.validate_core_delegation_paths(&state, 1)
            .expect("validate delegation");

        init.begin_running().expect("running");
        assert_eq!(init.phase(), InitBootPhase::Running);
    }

    #[test]
    fn init_server_rejects_running_without_registration() {
        let mut init = InitServerLite::new();
        assert_eq!(init.begin_running(), Err(KernelError::WrongObject));
    }
}
