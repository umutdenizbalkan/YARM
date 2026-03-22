use crate::kernel::bootstrap::{Bootstrap, KernelError};
use crate::kernel::init::{
    CoreServiceGraph, CoreServiceImagePlan, InitBootPhase, InitFaultHandoff, InitServerLite,
};
use crate::kernel::ipc::Message;
use crate::kernel::process_abi::{PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2, SpawnV2Args, WaitPidV2Args};
use crate::kernel::process::{ProcessService, SpawnV2Result, WaitPidV2Result};
use crate::kernel::vfs::{
    OpenAtRequest, ReadWriteRequest, VfsService, openat_message, read_message,
};
use crate::services::fs::initramfs::{INITRAMFS_BUSYBOX_PATH_PTR, InitramfsBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitBootSummary {
    pub init_phase: InitBootPhase,
    pub proc_wait_exit: u64,
    pub vfs_open_opcode: u16,
    pub vfs_read_opcode: u16,
    pub irq_notification_opcode: Option<u16>,
}

pub fn run_init_core_bootstrap_scenario() -> Result<InitBootSummary, KernelError> {
    let mut kernel = Bootstrap::init()?;
    let mut init = InitServerLite::new();
    let graph = CoreServiceGraph {
        init_tid: 1,
        process_manager_tid: 2,
        vfs_tid: 3,
        supervisor_tid: 4,
    };
    init.register_core_graph(&mut kernel, graph)?;
    let _ = init.launch_core_services(
        &mut kernel,
        CoreServiceImagePlan {
            process_manager_entry: 0x8000,
            vfs_entry: 0x9000,
            supervisor_entry: 0xA000,
        },
    )?;
    init.install_fault_handoff(InitFaultHandoff {
        supervisor_tid: graph.supervisor_tid,
        restart_window_ticks: 100,
    })?;
    init.begin_running()?;

    let (_notif, notif_send_cap, notif_recv_cap) = kernel.create_notification(8)?;
    kernel.bind_irq_notification(9, notif_send_cap)?;

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
    let child = SpawnV2Result::decode(spawn_rep.as_slice()).map_err(|_| KernelError::WrongObject)?;
    proc.mark_exit(child.pid, 5)
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
    let waited = WaitPidV2Result::decode(wait_rep.as_slice()).map_err(|_| KernelError::WrongObject)?;

    let mut vfs = VfsService::with_backend(InitramfsBackend::new(4096));
    let open = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: INITRAMFS_BUSYBOX_PATH_PTR,
        flags: 0,
        mode: 0,
    })
    .map_err(|_| KernelError::WrongObject)?;
    let open_rep = vfs
        .handle_request(open)
        .map_err(|_| KernelError::WrongObject)?;
    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(open_rep.as_slice());
    let fd = u64::from_le_bytes(fd_bytes);
    let read = read_message(ReadWriteRequest {
        fd,
        buf_ptr: 0,
        len: 64,
    })
    .map_err(|_| KernelError::WrongObject)?;
    let read_rep = vfs
        .handle_request(read)
        .map_err(|_| KernelError::WrongObject)?;

    kernel.route_external_irq(9)?;
    let irq_notification_opcode = kernel
        .ipc_recv(notif_recv_cap)?
        .ok_or(KernelError::WrongObject)
        .map(|msg| msg.opcode)
        .map(Some)?;

    Ok(InitBootSummary {
        init_phase: init.phase(),
        proc_wait_exit: waited.exit_code,
        vfs_open_opcode: open_rep.opcode,
        vfs_read_opcode: read_rep.opcode,
        irq_notification_opcode,
    })
}
