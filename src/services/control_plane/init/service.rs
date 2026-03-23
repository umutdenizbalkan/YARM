use crate::kernel::boot::{Bootstrap, KernelError, KernelState};
use crate::services::init::{
    CoreLaunchStrategy, CoreServiceGraph, CoreServiceImagePlan, InitBootPhase, InitService,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitRuntimeBootConfig {
    pub launch_strategy: CoreLaunchStrategy,
    pub graph: CoreServiceGraph,
    pub image_plan: CoreServiceImagePlan,
    pub restart_window_ticks: u64,
}

impl InitRuntimeBootConfig {
    pub const fn baseline() -> Self {
        Self {
            launch_strategy: CoreLaunchStrategy::SupervisorFirst,
            graph: CoreServiceGraph {
                init_tid: 1,
                process_manager_tid: 2,
                vfs_tid: 3,
                supervisor_tid: 4,
            },
            image_plan: CoreServiceImagePlan {
                process_manager_entry: 0x8000,
                vfs_entry: 0x9000,
                supervisor_entry: 0xA000,
            },
            restart_window_ticks: 100,
        }
    }
}

impl Default for InitRuntimeBootConfig {
    fn default() -> Self {
        Self::baseline()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitRuntimeSummary {
    pub phase: InitBootPhase,
    pub handles: crate::services::init::CoreServiceHandles,
    pub replayed_registrations: usize,
    pub present_cpus: usize,
    pub present_cpu_bitmap: u64,
    pub online_cpus: usize,
}

pub fn run_with_kernel(
    kernel: &mut KernelState,
    config: InitRuntimeBootConfig,
) -> Result<InitRuntimeSummary, KernelError> {
    let mut init = InitService::new();
    init.set_launch_strategy(config.launch_strategy);
    init.register_core_graph(kernel, config.graph)?;
    init.launch_core_services(kernel, config.image_plan)?;
    init.install_fault_handoff(kernel, config.restart_window_ticks)?;
    let replayed_registrations = init.seed_supervisor_registrations(kernel)?;
    init.begin_running(kernel)?;

    Ok(InitRuntimeSummary {
        phase: init.phase(),
        handles: init.handles(),
        replayed_registrations,
        present_cpus: kernel.present_cpu_count(),
        present_cpu_bitmap: kernel.present_cpu_bitmap(),
        online_cpus: kernel.online_cpu_count(),
    })
}

pub fn run() {
    let mut kernel = Bootstrap::init().expect("init");
    let summary =
        run_with_kernel(&mut kernel, InitRuntimeBootConfig::baseline()).expect("runtime init");

    crate::yarm_log!(
        "init.srv runtime online: phase={:?}, handles={:?}, replayed_registrations={}, present_cpus={}, present_bitmap=0x{:x}, online_cpus={}",
        summary.phase,
        summary.handles,
        summary.replayed_registrations,
        summary.present_cpus,
        summary.present_cpu_bitmap,
        summary.online_cpus
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_boot_config_can_drive_external_kernel_state() {
        let mut kernel = Bootstrap::init().expect("init");
        let summary = run_with_kernel(&mut kernel, InitRuntimeBootConfig::baseline())
            .expect("runtime boot");

        assert_eq!(summary.phase, InitBootPhase::Running);
        assert_eq!(summary.handles.init_tid, Some(1));
        assert_eq!(summary.handles.process_manager_tid, Some(2));
        assert_eq!(summary.handles.vfs_tid, Some(3));
        assert_eq!(summary.handles.supervisor_tid, Some(4));
        assert_eq!(summary.replayed_registrations, 3);
    }

    #[test]
    fn runtime_boot_config_supports_custom_service_identity_layout() {
        let mut kernel = Bootstrap::init().expect("init");
        let summary = run_with_kernel(
            &mut kernel,
            InitRuntimeBootConfig {
                graph: CoreServiceGraph {
                    init_tid: 41,
                    process_manager_tid: 42,
                    vfs_tid: 43,
                    supervisor_tid: 44,
                },
                ..InitRuntimeBootConfig::baseline()
            },
        )
        .expect("runtime boot");

        assert_eq!(summary.phase, InitBootPhase::Running);
        assert_eq!(summary.handles.init_tid, Some(41));
        assert_eq!(summary.handles.process_manager_tid, Some(42));
        assert_eq!(summary.handles.vfs_tid, Some(43));
        assert_eq!(summary.handles.supervisor_tid, Some(44));
    }
}
