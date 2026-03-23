mod launch;
mod mount;
mod policy;

use crate::kernel::boot::{KernelError, KernelState, UserImageSpec};
use crate::kernel::task::TaskClass;
use crate::kernel::vm::Asid;

pub use launch::{CoreLaunchReport, CoreServiceGraph, CoreServiceHandles, CoreServiceImagePlan};
pub use mount::{MountPlan, MountRecoveryReport, MountServiceKind};
pub use policy::{
    CoreLaunchStrategy, CoreServiceKind, CoreServicePolicyTable, InitBootPhase,
    InitFaultHandoff, ServiceRestartPolicy, StartupCap, StartupCapSet,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitServerLite {
    phase: InitBootPhase,
    handles: CoreServiceHandles,
    startup_caps: StartupCapSet,
    fault_handoff: Option<InitFaultHandoff>,
    restart_policies: CoreServicePolicyTable,
    launch_strategy: CoreLaunchStrategy,
    launch_order: [Option<CoreServiceKind>; 3],
    launch_count: usize,
    mount_plan: MountPlan,
    mount_status: Option<MountRecoveryReport>,
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
            fault_handoff: None,
            restart_policies: CoreServicePolicyTable::baseline(),
            launch_strategy: CoreLaunchStrategy::ProcessManagerFirst,
            launch_order: [None; 3],
            launch_count: 0,
            mount_plan: MountPlan::baseline(),
            mount_status: None,
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

    pub const fn fault_handoff(&self) -> Option<InitFaultHandoff> {
        self.fault_handoff
    }

    pub const fn restart_policies(&self) -> CoreServicePolicyTable {
        self.restart_policies
    }

    pub fn set_restart_policies(
        &mut self,
        policies: CoreServicePolicyTable,
    ) -> Result<(), KernelError> {
        if !policies.is_sane() {
            return Err(KernelError::InvalidCapability);
        }
        self.restart_policies = policies;
        Ok(())
    }

    pub const fn launch_order(&self) -> [Option<CoreServiceKind>; 3] {
        self.launch_order
    }

    pub const fn launch_strategy(&self) -> CoreLaunchStrategy {
        self.launch_strategy
    }

    pub const fn mount_plan(&self) -> MountPlan {
        self.mount_plan
    }

    pub const fn mount_status(&self) -> Option<MountRecoveryReport> {
        self.mount_status
    }

    pub fn set_mount_plan(&mut self, plan: MountPlan) -> Result<(), KernelError> {
        if plan.count == 0 || plan.count > plan.order.len() {
            return Err(KernelError::WrongObject);
        }
        self.mount_plan = plan;
        self.mount_status = None;
        Ok(())
    }

    pub fn set_launch_strategy(&mut self, strategy: CoreLaunchStrategy) {
        self.launch_strategy = strategy;
    }

    fn record_launch(
        &mut self,
        kernel: &mut KernelState,
        kind: CoreServiceKind,
        tid: u64,
        entry: usize,
        asid: u16,
    ) -> Result<(), KernelError> {
        self.launch_order[self.launch_count] = Some(kind);
        self.launch_count += 1;
        kernel.spawn_user_task_from_image(UserImageSpec {
            tid,
            entry,
            asid: Some(Asid(asid)),
            class: TaskClass::SystemServer,
        })?;
        Ok(())
    }

    pub fn execute_mount_plan_with_fail_at(
        &self,
        fail_at: Option<usize>,
    ) -> Result<MountRecoveryReport, KernelError> {
        let mut mounted = 0usize;
        let mut recovered = false;
        for idx in 0..self.mount_plan.count {
            if fail_at == Some(idx) {
                if self.mount_plan.allow_fallback_to_fat {
                    recovered = true;
                    mounted = mounted.saturating_add(1);
                    break;
                }
                return Err(KernelError::WrongObject);
            }
            if self.mount_plan.order[idx].is_some() {
                mounted = mounted.saturating_add(1);
            }
        }
        Ok(MountRecoveryReport {
            mounted_count: mounted,
            recovered_with_fat: recovered,
        })
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

        self.handles.process_manager_tid = Some(graph.process_manager_tid);
        self.handles.vfs_tid = Some(graph.vfs_tid);
        self.handles.supervisor_tid = Some(graph.supervisor_tid);
        self.phase = InitBootPhase::CoreServicesRegistered;
        Ok(())
    }

    pub fn launch_core_services(
        &mut self,
        kernel: &mut KernelState,
        plan: CoreServiceImagePlan,
    ) -> Result<CoreLaunchReport, KernelError> {
        self.launch_core_services_with_mount_fail_at(kernel, plan, None)
    }

    pub fn launch_core_services_with_mount_fail_at(
        &mut self,
        kernel: &mut KernelState,
        plan: CoreServiceImagePlan,
        fail_at: Option<usize>,
    ) -> Result<CoreLaunchReport, KernelError> {
        if self.phase != InitBootPhase::CoreServicesRegistered {
            return Err(KernelError::WrongObject);
        }
        self.phase = InitBootPhase::LaunchingCore;
        if !self.restart_policies.is_sane() {
            return Err(KernelError::InvalidCapability);
        }
        self.launch_order = [None; 3];
        self.launch_count = 0;

        let proc_tid = self
            .handles
            .process_manager_tid
            .ok_or(KernelError::WrongObject)?;
        let vfs_tid = self.handles.vfs_tid.ok_or(KernelError::WrongObject)?;
        let supervisor_tid = self
            .handles
            .supervisor_tid
            .ok_or(KernelError::WrongObject)?;

        match self.launch_strategy {
            CoreLaunchStrategy::ProcessManagerFirst => {
                self.record_launch(
                    kernel,
                    CoreServiceKind::ProcessManager,
                    proc_tid,
                    plan.process_manager_entry,
                    11,
                )?;
                self.record_launch(kernel, CoreServiceKind::Vfs, vfs_tid, plan.vfs_entry, 12)?;
                self.record_launch(
                    kernel,
                    CoreServiceKind::Supervisor,
                    supervisor_tid,
                    plan.supervisor_entry,
                    13,
                )?;
            }
            CoreLaunchStrategy::SupervisorFirst => {
                self.record_launch(
                    kernel,
                    CoreServiceKind::Supervisor,
                    supervisor_tid,
                    plan.supervisor_entry,
                    13,
                )?;
                self.record_launch(
                    kernel,
                    CoreServiceKind::ProcessManager,
                    proc_tid,
                    plan.process_manager_entry,
                    11,
                )?;
                self.record_launch(kernel, CoreServiceKind::Vfs, vfs_tid, plan.vfs_entry, 12)?;
            }
        }

        let mount_status = self.execute_mount_plan_with_fail_at(fail_at)?;
        self.mount_status = Some(mount_status);

        Ok(CoreLaunchReport {
            process_manager_spawned: true,
            vfs_spawned: true,
            supervisor_spawned: true,
        })
    }

    pub fn install_fault_handoff(&mut self, handoff: InitFaultHandoff) -> Result<(), KernelError> {
        if self.phase != InitBootPhase::LaunchingCore {
            return Err(KernelError::WrongObject);
        }
        if self.handles.supervisor_tid != Some(handoff.supervisor_tid) {
            return Err(KernelError::WrongObject);
        }
        self.fault_handoff = Some(handoff);
        Ok(())
    }

    pub fn mark_failed(&mut self) {
        self.phase = InitBootPhase::Failed;
    }

    pub fn begin_running(&mut self) -> Result<(), KernelError> {
        if self.phase != InitBootPhase::LaunchingCore
            || self.fault_handoff.is_none()
            || self.mount_status.is_none()
        {
            return Err(KernelError::WrongObject);
        }
        self.phase = InitBootPhase::Running;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;

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
    fn init_server_launch_flow_registers_launches_and_enters_running() {
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

        let report = init
            .launch_core_services(
                &mut state,
                CoreServiceImagePlan {
                    process_manager_entry: 0x8000,
                    vfs_entry: 0x9000,
                    supervisor_entry: 0xA000,
                },
            )
            .expect("launch");
        assert!(report.process_manager_spawned);
        assert!(report.vfs_spawned);
        assert!(report.supervisor_spawned);

        init.install_fault_handoff(InitFaultHandoff {
            supervisor_tid: 4,
            restart_window_ticks: 50,
        })
        .expect("handoff");
        init.begin_running().expect("running");
        assert_eq!(init.phase(), InitBootPhase::Running);
    }

    #[test]
    fn launch_order_is_deterministic() {
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
        let _ = init
            .launch_core_services(
                &mut state,
                CoreServiceImagePlan {
                    process_manager_entry: 0x8000,
                    vfs_entry: 0x9000,
                    supervisor_entry: 0xA000,
                },
            )
            .expect("launch");
        assert_eq!(
            init.launch_order(),
            [
                Some(CoreServiceKind::ProcessManager),
                Some(CoreServiceKind::Vfs),
                Some(CoreServiceKind::Supervisor),
            ]
        );
    }

    #[test]
    fn launch_order_can_prioritize_supervisor() {
        let mut state = Bootstrap::init().expect("init");
        let mut init = InitServerLite::new();
        init.set_launch_strategy(CoreLaunchStrategy::SupervisorFirst);
        let graph = CoreServiceGraph {
            init_tid: 1,
            process_manager_tid: 2,
            vfs_tid: 3,
            supervisor_tid: 4,
        };
        init.register_core_graph(&mut state, graph)
            .expect("register");
        let _ = init
            .launch_core_services(
                &mut state,
                CoreServiceImagePlan {
                    process_manager_entry: 0x8000,
                    vfs_entry: 0x9000,
                    supervisor_entry: 0xA000,
                },
            )
            .expect("launch");
        assert_eq!(
            init.launch_order(),
            [
                Some(CoreServiceKind::Supervisor),
                Some(CoreServiceKind::ProcessManager),
                Some(CoreServiceKind::Vfs),
            ]
        );
    }

    #[test]
    fn launch_sets_mount_status() {
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
        init.launch_core_services(
            &mut state,
            CoreServiceImagePlan {
                process_manager_entry: 0x8000,
                vfs_entry: 0x9000,
                supervisor_entry: 0xA000,
            },
        )
        .expect("launch");
        assert!(init.mount_status().is_some());
    }

    #[test]
    fn begin_running_requires_fault_handoff() {
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
        init.launch_core_services(
            &mut state,
            CoreServiceImagePlan {
                process_manager_entry: 0x8000,
                vfs_entry: 0x9000,
                supervisor_entry: 0xA000,
            },
        )
        .expect("launch");
        assert_eq!(init.begin_running(), Err(KernelError::WrongObject));
    }

    #[test]
    fn rejects_invalid_restart_policies() {
        let mut init = InitServerLite::new();
        let bad = CoreServicePolicyTable {
            process_manager: ServiceRestartPolicy {
                max_restarts: 0,
                backoff_ticks: 1,
            },
            vfs: ServiceRestartPolicy {
                max_restarts: 1,
                backoff_ticks: 1,
            },
            supervisor: ServiceRestartPolicy {
                max_restarts: 1,
                backoff_ticks: 1,
            },
        };
        assert_eq!(
            init.set_restart_policies(bad),
            Err(KernelError::InvalidCapability)
        );
    }

    #[test]
    fn init_server_supports_failure_transition() {
        let mut init = InitServerLite::new();
        init.mark_failed();
        assert_eq!(init.phase(), InitBootPhase::Failed);
    }

    #[test]
    fn mount_plan_supports_ext4_to_fat_recovery() {
        let init = InitServerLite::new();
        let report = init
            .execute_mount_plan_with_fail_at(Some(3))
            .expect("mount recovery");
        assert!(report.recovered_with_fat);
        assert!(report.mounted_count >= 4);
    }

    #[test]
    fn mount_plan_without_fallback_fails() {
        let mut init = InitServerLite::new();
        let mut plan = MountPlan::baseline();
        plan.allow_fallback_to_fat = false;
        init.set_mount_plan(plan).expect("set plan");
        assert_eq!(
            init.execute_mount_plan_with_fail_at(Some(3)),
            Err(KernelError::WrongObject)
        );
    }
}
