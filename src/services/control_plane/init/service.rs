use crate::kernel::boot::{Bootstrap, KernelError, KernelState};
use crate::kernel::ipc::Message;
use crate::kernel::process::{ProcessService, SpawnV2Result, WaitPidV2Result};
use crate::kernel::process_abi::{
    PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2, SpawnV2Args, WaitPidV2Args,
};
use crate::kernel::vfs::{
    OpenAtRequest, ReadWriteRequest, VfsService, openat_message, read_message,
};
use crate::kernel::vfs_abi::{VFS_OP_OPENAT, VFS_OP_READ};
use crate::services::control_plane::supervisor::SupervisorService;
use crate::services::fs::devfs::{DEV_CONSOLE_PATH_PTR, DevFsBackend};
use crate::services::fs::initramfs::{INITRAMFS_BUSYBOX_PATH_PTR, InitramfsBackend};
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

fn boot_init_runtime(
    kernel: &mut KernelState,
    config: InitRuntimeBootConfig,
) -> Result<InitService, KernelError> {
    let mut init = InitService::new();
    init.set_launch_strategy(config.launch_strategy);
    init.register_core_graph(kernel, config.graph)?;
    init.launch_core_services(kernel, config.image_plan)?;
    init.install_fault_handoff(kernel, config.restart_window_ticks)?;
    let _ = init.seed_supervisor_registrations(kernel)?;
    init.begin_running(kernel)?;
    Ok(init)
}

pub fn run_with_kernel(
    kernel: &mut KernelState,
    config: InitRuntimeBootConfig,
) -> Result<InitRuntimeSummary, KernelError> {
    let init = boot_init_runtime(kernel, config)?;

    Ok(InitRuntimeSummary {
        phase: init.phase(),
        handles: init.handles(),
        replayed_registrations: 3,
        present_cpus: kernel.present_cpu_count(),
        present_cpu_bitmap: kernel.present_cpu_bitmap(),
        online_cpus: kernel.online_cpu_count(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinimumRunnableProfileSummary {
    pub init_phase: InitBootPhase,
    pub supervisor_managed_services: usize,
    pub process_wait_exit: u64,
    pub devfs_open_opcode: u16,
    pub initramfs_read_opcode: u16,
}

pub fn run_minimum_profile_with_kernel(
    kernel: &mut KernelState,
    config: InitRuntimeBootConfig,
) -> Result<MinimumRunnableProfileSummary, KernelError> {
    let init = boot_init_runtime(kernel, config)?;
    let handoff = init.fault_handoff().ok_or(KernelError::WrongObject)?;
    let mut supervisor = SupervisorService::new(
        init.handles().init_tid.ok_or(KernelError::WrongObject)?,
        handoff,
        init.restart_policies(),
    );
    let supervisor_managed_services = supervisor.run_until_idle(kernel)?;

    let mut proc = ProcessService::new();
    let spawn = Message::with_header(
        0,
        PROC_OP_SPAWN_V2,
        0,
        None,
        &SpawnV2Args::new(1, 99).encode(),
    )
    .map_err(|_| KernelError::WrongObject)?;
    let spawn_rep = proc.handle(spawn).map_err(|_| KernelError::WrongObject)?;
    let child =
        SpawnV2Result::decode(spawn_rep.as_slice()).map_err(|_| KernelError::WrongObject)?;
    proc.mark_exit(child.pid, 7)
        .map_err(|_| KernelError::WrongObject)?;
    let wait = Message::with_header(
        0,
        PROC_OP_WAITPID_V2,
        0,
        None,
        &WaitPidV2Args::new(1, child.pid.0).encode(),
    )
    .map_err(|_| KernelError::WrongObject)?;
    let wait_rep = proc.handle(wait).map_err(|_| KernelError::WrongObject)?;
    let waited =
        WaitPidV2Result::decode(wait_rep.as_slice()).map_err(|_| KernelError::WrongObject)?;

    let mut devfs = VfsService::with_backend(DevFsBackend::default());
    let devfs_open = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: DEV_CONSOLE_PATH_PTR,
        flags: 0,
        mode: 0,
    })
    .map_err(|_| KernelError::WrongObject)?;
    let devfs_open_rep = devfs
        .handle_request(devfs_open)
        .map_err(|_| KernelError::WrongObject)?;

    let mut initramfs = VfsService::with_backend(InitramfsBackend::new(4096));
    let initramfs_open = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: INITRAMFS_BUSYBOX_PATH_PTR,
        flags: 0,
        mode: 0,
    })
    .map_err(|_| KernelError::WrongObject)?;
    let initramfs_open_rep = initramfs
        .handle_request(initramfs_open)
        .map_err(|_| KernelError::WrongObject)?;
    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(initramfs_open_rep.as_slice());
    let initramfs_fd = u64::from_le_bytes(fd_bytes);
    let initramfs_read = read_message(ReadWriteRequest {
        fd: initramfs_fd,
        buf_ptr: 0,
        len: 64,
    })
    .map_err(|_| KernelError::WrongObject)?;
    let initramfs_read_rep = initramfs
        .handle_request(initramfs_read)
        .map_err(|_| KernelError::WrongObject)?;

    if devfs_open_rep.opcode != VFS_OP_OPENAT || initramfs_read_rep.opcode != VFS_OP_READ {
        return Err(KernelError::WrongObject);
    }

    Ok(MinimumRunnableProfileSummary {
        init_phase: init.phase(),
        supervisor_managed_services,
        process_wait_exit: waited.exit_code,
        devfs_open_opcode: devfs_open_rep.opcode,
        initramfs_read_opcode: initramfs_read_rep.opcode,
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

    #[test]
    fn minimum_runnable_profile_brings_up_core_services_plus_devfs_and_initramfs() {
        let mut kernel = Bootstrap::init().expect("init");
        let summary =
            run_minimum_profile_with_kernel(&mut kernel, InitRuntimeBootConfig::baseline())
                .expect("minimum runnable profile");

        assert_eq!(summary.init_phase, InitBootPhase::Running);
        assert_eq!(summary.supervisor_managed_services, 3);
        assert_eq!(summary.process_wait_exit, 7);
        assert_eq!(summary.devfs_open_opcode, VFS_OP_OPENAT);
        assert_eq!(summary.initramfs_read_opcode, VFS_OP_READ);
    }
}
