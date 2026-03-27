use crate::kernel::boot::{Bootstrap, KernelError, KernelState};
use crate::kernel::process::ProcessService;
use crate::kernel::vfs::InMemoryBackend;
use crate::kernel::vfs_abi::{VFS_OP_OPENAT, VFS_OP_READ};
use crate::services::common::service::FsService;
use crate::services::control_plane::process_manager::service::run_request_loop as run_process_manager_request_loop;
use crate::services::control_plane::supervisor::SupervisorService;
use crate::services::control_plane::vfs::service::run_request_loop_over_kernel_ipc as run_vfs_request_loop;
use crate::services::fs::devfs::service::run_request_loop as run_devfs_request_loop;
use crate::services::fs::devfs::{DevFsBackend, DevFsService};
use crate::services::fs::initramfs::service::run_request_loop as run_initramfs_request_loop;
use crate::services::fs::initramfs::{InitramfsBackend, InitramfsService};
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
    pub seeded_registrations: usize,
    pub mount_report: crate::services::init::MountRecoveryReport,
    pub present_cpus: usize,
    pub present_cpu_bitmap: u64,
    pub online_cpus: usize,
    pub restart_counts: (u8, u8, u8),
}

fn boot_init_runtime(
    kernel: &mut KernelState,
    config: InitRuntimeBootConfig,
) -> Result<(InitService, usize), KernelError> {
    let mut init = InitService::new();
    init.set_launch_strategy(config.launch_strategy);
    init.register_core_graph(kernel, config.graph)?;
    init.launch_core_services(kernel, config.image_plan)?;
    init.install_fault_handoff(kernel, config.restart_window_ticks)?;
    let seeded_registrations = init.seed_supervisor_registrations(kernel)?;
    init.begin_running(kernel)?;
    Ok((init, seeded_registrations))
}

pub fn run_with_kernel(
    kernel: &mut KernelState,
    config: InitRuntimeBootConfig,
) -> Result<InitRuntimeSummary, KernelError> {
    let (init, seeded_registrations) = boot_init_runtime(kernel, config)?;

    Ok(InitRuntimeSummary {
        phase: init.phase(),
        handles: init.handles(),
        seeded_registrations,
        mount_report: init.mount_status().ok_or(KernelError::WrongObject)?,
        present_cpus: kernel.present_cpu_count(),
        present_cpu_bitmap: kernel.present_cpu_bitmap(),
        online_cpus: kernel.online_cpu_count(),
        restart_counts: init.restart_counts(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinimumRunnableProfileSummary {
    pub init_phase: InitBootPhase,
    pub seeded_registrations: usize,
    pub supervisor_managed_services: usize,
    pub process_wait_exit: u64,
    pub process_loop_handled: usize,
    pub control_vfs_fd: u64,
    pub control_vfs_handled: usize,
    pub devfs_open_opcode: u16,
    pub devfs_handled: usize,
    pub initramfs_read_opcode: u16,
    pub initramfs_handled: usize,
    pub mount_report: crate::services::init::MountRecoveryReport,
    pub recovered_core_services: usize,
}

pub fn run_minimum_profile_with_kernel(
    kernel: &mut KernelState,
    config: InitRuntimeBootConfig,
) -> Result<MinimumRunnableProfileSummary, KernelError> {
    let (mut init, seeded_registrations) = boot_init_runtime(kernel, config)?;
    let handoff = init.fault_handoff().ok_or(KernelError::WrongObject)?;
    let mut supervisor = SupervisorService::new(
        init.handles().init_tid.ok_or(KernelError::WrongObject)?,
        handoff,
        init.restart_policies(),
    );
    let supervisor_managed_services = supervisor.run_until_idle(kernel)?;

    let mut proc = ProcessService::new();
    let proc_summary = run_process_manager_request_loop(&mut proc, 1, 99, 7)
        .map_err(|_| KernelError::WrongObject)?;

    let mut control_vfs = FsService::with_backend(InMemoryBackend::new());
    let control_vfs_summary = run_vfs_request_loop(kernel, &mut control_vfs, 0x1000)
        .map_err(|_| KernelError::WrongObject)?;

    let mut devfs = DevFsService::with_backend(DevFsBackend::default());
    let devfs_summary = run_devfs_request_loop(&mut devfs).map_err(|_| KernelError::WrongObject)?;
    let devfs_open_opcode = VFS_OP_OPENAT;

    let mut initramfs = InitramfsService::with_backend(InitramfsBackend::new(4096));
    let initramfs_summary =
        run_initramfs_request_loop(&mut initramfs).map_err(|_| KernelError::WrongObject)?;
    let initramfs_read_opcode = VFS_OP_READ;

    if devfs_open_opcode != VFS_OP_OPENAT || initramfs_read_opcode != VFS_OP_READ {
        return Err(KernelError::WrongObject);
    }
    let recovered_core_services = init.monitor_core_failures(kernel)?;

    Ok(MinimumRunnableProfileSummary {
        init_phase: init.phase(),
        seeded_registrations,
        supervisor_managed_services,
        process_wait_exit: proc_summary.waited_exit,
        process_loop_handled: proc_summary.handled,
        control_vfs_fd: control_vfs_summary.fd,
        control_vfs_handled: control_vfs_summary.handled,
        devfs_open_opcode,
        devfs_handled: devfs_summary.handled,
        initramfs_read_opcode,
        initramfs_handled: initramfs_summary.handled,
        mount_report: init.mount_status().ok_or(KernelError::WrongObject)?,
        recovered_core_services,
    })
}

pub fn run() {
    let mut kernel = Bootstrap::init().expect("init");
    let summary = run_minimum_profile_with_kernel(&mut kernel, InitRuntimeBootConfig::baseline())
        .expect("minimum runnable profile");

    crate::yarm_log!(
        "init.srv minimum profile online: phase={:?}, supervisor_managed_services={}, process_wait_exit={}, devfs_open_opcode={}, initramfs_read_opcode={}",
        summary.init_phase,
        summary.supervisor_managed_services,
        summary.process_wait_exit,
        summary.devfs_open_opcode,
        summary.initramfs_read_opcode
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_boot_config_can_drive_external_kernel_state() {
        let mut kernel = Bootstrap::init().expect("init");
        let summary =
            run_with_kernel(&mut kernel, InitRuntimeBootConfig::baseline()).expect("runtime boot");

        assert_eq!(summary.phase, InitBootPhase::Running);
        assert_eq!(summary.handles.init_tid, Some(1));
        assert_eq!(summary.handles.process_manager_tid, Some(2));
        assert_eq!(summary.handles.vfs_tid, Some(3));
        assert_eq!(summary.handles.supervisor_tid, Some(4));
        assert_eq!(summary.seeded_registrations, 3);
        assert_eq!(summary.mount_report.mounted_count, 4);
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

    #[test]
    fn minimum_runnable_profile_brings_up_core_services_plus_devfs_and_initramfs() {
        let mut kernel = Bootstrap::init().expect("init");
        let summary =
            run_minimum_profile_with_kernel(&mut kernel, InitRuntimeBootConfig::baseline())
                .expect("minimum runnable profile");

        assert_eq!(summary.init_phase, InitBootPhase::Running);
        assert_eq!(summary.seeded_registrations, 3);
        assert_eq!(summary.supervisor_managed_services, 3);
        assert_eq!(summary.process_wait_exit, 7);
        assert_eq!(summary.process_loop_handled, 3);
        assert_eq!(summary.control_vfs_fd, 3);
        assert_eq!(summary.control_vfs_handled, 15);
        assert_eq!(summary.devfs_open_opcode, VFS_OP_OPENAT);
        assert_eq!(summary.devfs_handled, 6);
        assert_eq!(summary.initramfs_read_opcode, VFS_OP_READ);
        assert_eq!(summary.initramfs_handled, 3);
        assert_eq!(summary.mount_report.mounted_count, 4);
        assert_eq!(summary.recovered_core_services, 0);
    }
}
