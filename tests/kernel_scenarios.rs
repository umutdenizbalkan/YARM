extern crate yarm;

use yarm::kernel::boot::{Bootstrap, KernelError};
use yarm::kernel::ipc::Message;
use yarm::kernel::process::{ProcessService, SpawnV2Result, WaitPidV2Result};
use yarm::kernel::process_abi::{PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2, SpawnV2Args, WaitPidV2Args};
use yarm::kernel::vfs::{MountRouter, OpenAtRequest, ReadWriteRequest, VfsService, openat_message, read_message};
use yarm::kernel::vfs_abi::{VFS_OP_OPENAT, VFS_OP_READ};
use yarm::services::fs::initramfs::{INITRAMFS_BUSYBOX_PATH_PTR, InitramfsBackend};
use yarm::services::fs::ramfs::RamFsBackend;
use yarm::services::init::{CoreServiceGraph, CoreServiceImagePlan, InitBootPhase, InitFaultHandoff, InitServerLite};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InitBootSummary {
    init_phase: InitBootPhase,
    proc_wait_exit: u64,
    vfs_open_opcode: u16,
    vfs_read_opcode: u16,
    irq_notification_opcode: Option<u16>,
}

fn run_init_core_bootstrap_scenario() -> Result<InitBootSummary, KernelError> {
    let mut kernel = Bootstrap::init()?;
    let mut init = InitServerLite::new();
    let graph = CoreServiceGraph { init_tid: 1, process_manager_tid: 2, vfs_tid: 3, supervisor_tid: 4 };
    init.register_core_graph(&mut kernel, graph)?;
    let _ = init.launch_core_services(&mut kernel, CoreServiceImagePlan {
        process_manager_entry: 0x8000,
        vfs_entry: 0x9000,
        supervisor_entry: 0xA000,
    })?;
    init.install_fault_handoff(InitFaultHandoff { supervisor_tid: graph.supervisor_tid, restart_window_ticks: 100 })?;
    init.begin_running()?;

    let (_notif, notif_send_cap, notif_recv_cap) = kernel.create_notification(8)?;
    kernel.bind_irq_notification(9, notif_send_cap)?;

    let mut proc = ProcessService::new();
    let spawn = Message::with_header(0, PROC_OP_SPAWN_V2, 0, None, &SpawnV2Args::new(1, 99).encode())
        .map_err(|_| KernelError::WrongObject)?;
    let spawn_rep = proc.handle(spawn).map_err(|_| KernelError::WrongObject)?;
    let child = SpawnV2Result::decode(spawn_rep.as_slice()).map_err(|_| KernelError::WrongObject)?;
    proc.mark_exit(child.pid, 5).map_err(|_| KernelError::WrongObject)?;
    let wait = Message::with_header(0, PROC_OP_WAITPID_V2, 0, None, &WaitPidV2Args::new(1, child.pid.0).encode())
        .map_err(|_| KernelError::WrongObject)?;
    let wait_rep = proc.handle(wait).map_err(|_| KernelError::WrongObject)?;
    let waited = WaitPidV2Result::decode(wait_rep.as_slice()).map_err(|_| KernelError::WrongObject)?;

    let mut vfs = VfsService::with_backend(InitramfsBackend::new(4096));
    let open = openat_message(OpenAtRequest { dirfd: 0, path_ptr: INITRAMFS_BUSYBOX_PATH_PTR, flags: 0, mode: 0 })
        .map_err(|_| KernelError::WrongObject)?;
    let open_rep = vfs.handle_request(open).map_err(|_| KernelError::WrongObject)?;
    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(open_rep.as_slice());
    let fd = u64::from_le_bytes(fd_bytes);
    let read = read_message(ReadWriteRequest { fd, buf_ptr: 0, len: 64 }).map_err(|_| KernelError::WrongObject)?;
    let read_rep = vfs.handle_request(read).map_err(|_| KernelError::WrongObject)?;

    kernel.route_external_irq(9)?;
    let irq_notification_opcode = kernel.ipc_recv(notif_recv_cap)?
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MountOrchestrationSummary {
    low_mount_opcode: u16,
    high_mount_opcode: u16,
}

fn run_mount_orchestration_scenario() -> Result<MountOrchestrationSummary, KernelError> {
    let router = MountRouter::new(0x8000, RamFsBackend::new(), InitramfsBackend::new(4096));
    let mut vfs = VfsService::with_backend(router);
    let open_low = openat_message(OpenAtRequest { dirfd: 0, path_ptr: 0x1000, flags: 0, mode: 0 })
        .map_err(|_| KernelError::WrongObject)?;
    let low_rep = vfs.handle_request(open_low).map_err(|_| KernelError::WrongObject)?;
    let open_high = openat_message(OpenAtRequest { dirfd: 0, path_ptr: INITRAMFS_BUSYBOX_PATH_PTR, flags: 0, mode: 0 })
        .map_err(|_| KernelError::WrongObject)?;
    let high_rep = vfs.handle_request(open_high).map_err(|_| KernelError::WrongObject)?;
    Ok(MountOrchestrationSummary { low_mount_opcode: low_rep.opcode, high_mount_opcode: high_rep.opcode })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimStep { SeedOpcode(u16), SendOpcode(u16), ExternalIrq(u16) }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SimSummary { last_received_opcode: Option<u16>, last_irq_opcode: Option<u16>, send_count: usize }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Scenario {
    name: &'static str,
    steps: &'static [SimStep],
    expected_last_received_opcode: Option<u16>,
    expected_irq: Option<u16>,
    expected_send_count: usize,
}

fn scenario_catalog() -> &'static [Scenario] {
    const IPC_AND_IRQ: [SimStep; 4] = [SimStep::SeedOpcode(0x55), SimStep::SendOpcode(0x11), SimStep::SendOpcode(0x22), SimStep::ExternalIrq(9)];
    const IPC_ONLY: [SimStep; 3] = [SimStep::SeedOpcode(0x33), SimStep::SendOpcode(0x44), SimStep::SendOpcode(0x45)];
    const SCENARIOS: [Scenario; 2] = [
        Scenario { name: "ipc_and_irq_core", steps: &IPC_AND_IRQ, expected_last_received_opcode: Some(0x11), expected_irq: Some(9), expected_send_count: 2 },
        Scenario { name: "ipc_only_core", steps: &IPC_ONLY, expected_last_received_opcode: Some(0x44), expected_irq: None, expected_send_count: 2 },
    ];
    &SCENARIOS
}

fn run_deterministic_script(steps: &[SimStep]) -> Result<SimSummary, KernelError> {
    let mut state = Bootstrap::init()?;
    let (_ep, send_cap, recv_cap) = state.create_endpoint(16)?;
    let (_notif, notif_send_cap, notif_recv_cap) = state.create_notification(8)?;
    state.bind_irq_notification(9, notif_send_cap)?;
    let mut last_received_opcode = None;
    let mut last_irq_opcode = None;
    let mut send_count = 0usize;
    for step in steps {
        match *step {
            SimStep::SeedOpcode(opcode) => state.ipc_send(send_cap, Message::with_header(0, opcode, 0, None, &[0]).map_err(|_| KernelError::WrongObject)?)?,
            SimStep::SendOpcode(opcode) => {
                state.ipc_send(send_cap, Message::with_header(0, opcode, 0, None, &[0]).map_err(|_| KernelError::WrongObject)?)?;
                let msg = state.ipc_recv(recv_cap)?.ok_or(KernelError::WrongObject)?;
                last_received_opcode = Some(msg.opcode);
                send_count = send_count.saturating_add(1);
            }
            SimStep::ExternalIrq(line) => {
                state.route_external_irq(line)?;
                let msg = state.ipc_recv(notif_recv_cap)?.ok_or(KernelError::WrongObject)?;
                last_irq_opcode = Some(msg.opcode);
            }
        }
    }
    Ok(SimSummary { last_received_opcode, last_irq_opcode, send_count })
}

#[test]
fn deterministic_init_core_bootstrap_replays_proc_and_vfs_path() {
    let handle = std::thread::Builder::new().name("init-core-boot-sim".into()).stack_size(8 * 1024 * 1024).spawn(|| {
        let summary = run_init_core_bootstrap_scenario().expect("boot sim");
        assert_eq!(summary.init_phase, InitBootPhase::Running);
        assert_eq!(summary.proc_wait_exit, 5);
        assert_eq!(summary.vfs_open_opcode, VFS_OP_OPENAT);
        assert_eq!(summary.vfs_read_opcode, VFS_OP_READ);
        assert_eq!(summary.irq_notification_opcode, Some(9));
    }).expect("spawn thread");
    handle.join().expect("join");
}

#[test]
fn deterministic_mount_orchestration_routes_low_and_high_mounts() {
    let summary = run_mount_orchestration_scenario().expect("mount orchestration");
    assert_eq!(summary.low_mount_opcode, VFS_OP_OPENAT);
    assert_eq!(summary.high_mount_opcode, VFS_OP_OPENAT);
}

#[test]
fn core_scenario_catalog_replays_all_with_expected_results() {
    let handle = std::thread::Builder::new().name("core-scenario-catalog".into()).stack_size(8 * 1024 * 1024).spawn(|| {
        for scenario in scenario_catalog() {
            let summary = run_deterministic_script(scenario.steps).expect("sim");
            assert_eq!(summary.last_received_opcode, scenario.expected_last_received_opcode, "{}", scenario.name);
            assert_eq!(summary.last_irq_opcode, scenario.expected_irq, "{}", scenario.name);
            assert_eq!(summary.send_count, scenario.expected_send_count, "{}", scenario.name);
        }
    }).expect("spawn thread");
    handle.join().expect("join");
}
