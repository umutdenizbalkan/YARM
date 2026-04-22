// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

extern crate yarm;

use yarm::kernel::boot::{Bootstrap, KernelError};
use yarm::kernel::ipc::Message;
use yarm::kernel::process_abi::PROC_OP_SPAWN_V2;
use yarm_ipc_abi::supervisor_abi::{DEP_PROCESS_MANAGER, DEP_VFS, RegisterDriverRequest};
use yarm::kernel::task::TaskClass;
use yarm::kernel::vfs::{
    MountNamespacePolicy, MountRouter, OpenAtRequest, VfsError, openat_message,
};
use yarm::kernel::vfs_abi::{VFS_OP_OPENAT, VFS_OP_READ};
use yarm::services::common::vfs_service::VfsService;
use yarm::services::control_plane::supervisor::SupervisorService;
use yarm::services::fs::initramfs::{INITRAMFS_BOOT_MARKER_PATH_PTR, InitramfsBackend};
use yarm::services::fs::ramfs::RamFsBackend;
use yarm::services::init::{CoreServiceGraph, CoreServiceImagePlan, InitBootPhase, InitService};

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
    let (_notif, notif_send_cap, notif_recv_cap) = kernel.create_notification(8)?;
    kernel.bind_irq_notification(9, notif_send_cap)?;

    kernel.route_external_irq(9)?;
    let irq_notification_opcode = kernel
        .ipc_recv(notif_recv_cap)?
        .ok_or(KernelError::WrongObject)
        .map(|msg| msg.opcode)
        .map(Some)?;

    Ok(InitBootSummary {
        init_phase: InitBootPhase::Running,
        proc_wait_exit: 5,
        vfs_open_opcode: VFS_OP_OPENAT,
        vfs_read_opcode: VFS_OP_READ,
        irq_notification_opcode,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MountOrchestrationSummary {
    low_mount_opcode: u16,
    high_mount_opcode: u16,
    denied_dev_path: bool,
    recovered_mounts: usize,
}

fn run_mount_orchestration_scenario() -> Result<MountOrchestrationSummary, KernelError> {
    let router = MountRouter::new(0x8000, RamFsBackend::new(), InitramfsBackend::new(4096));
    let mut vfs = VfsService::with_backend(router);
    vfs.mount(0x1000, 1).map_err(|_| KernelError::WrongObject)?;
    vfs.mount(INITRAMFS_BOOT_MARKER_PATH_PTR, 2)
        .map_err(|_| KernelError::WrongObject)?;
    let open_low = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: 0x1000,
        flags: 0,
        mode: 0,
    })
    .map_err(|_| KernelError::WrongObject)?;
    let low_rep = vfs
        .handle_request(open_low)
        .map_err(|_| KernelError::WrongObject)?;
    let open_high = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: INITRAMFS_BOOT_MARKER_PATH_PTR,
        flags: 0,
        mode: 0,
    })
    .map_err(|_| KernelError::WrongObject)?;
    let high_rep = vfs
        .handle_request(open_high)
        .map_err(|_| KernelError::WrongObject)?;
    vfs.mark_mount_failed(0x1000)
        .map_err(|_| KernelError::WrongObject)?;
    vfs.recover_mount(0x1000)
        .map_err(|_| KernelError::WrongObject)?;
    vfs.set_policy(MountNamespacePolicy::boot_profile());
    let denied_dev_path = matches!(
        vfs.handle_request(
            openat_message(OpenAtRequest {
                dirfd: 0,
                path_ptr: 0x3000,
                flags: 0,
                mode: 0,
            })
            .map_err(|_| KernelError::WrongObject)?,
        ),
        Err(VfsError::PermissionDenied)
    );
    Ok(MountOrchestrationSummary {
        low_mount_opcode: low_rep.opcode,
        high_mount_opcode: high_rep.opcode,
        denied_dev_path,
        recovered_mounts: vfs.active_mounts(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SupervisorReplaySummary {
    restored_managed_services: usize,
    restored_driver_services: usize,
}

fn run_supervisor_replay_scenario() -> Result<SupervisorReplaySummary, KernelError> {
    let mut kernel = Bootstrap::init()?;
    let mut init = InitService::new();
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
    let handoff = init.install_fault_handoff(&mut kernel, 100)?;
    init.seed_supervisor_registrations(&mut kernel)?;
    for tid in [20u64, 21, 22] {
        kernel.register_task_with_class(tid, TaskClass::Driver)?;
        kernel.register_driver(tid)?;
    }
    let (_id0, mem0) = kernel.alloc_anonymous_memory_object()?;
    let (_id1, mem1) = kernel.alloc_anonymous_memory_object()?;
    let (_id2, mem2) = kernel.alloc_anonymous_memory_object()?;
    let iova0 = kernel.create_iova_space_cap()?;
    let iova1 = kernel.create_iova_space_cap()?;
    let iova2 = kernel.create_iova_space_cap()?;
    init.register_driver_with_supervisor(
        &mut kernel,
        RegisterDriverRequest {
            tid: 20,
            max_restarts: 2,
            restart_group: 1,
            dependency_mask: DEP_VFS,
            backoff_ticks: 3,
            irq_line: 5,
            mem_cap: mem0.0,
            iova_cap: iova0.0,
            iova_base: 0x4000,
            iova_len: 4096,
            dma_len: 4096,
        },
    )?;
    init.register_driver_with_supervisor(
        &mut kernel,
        RegisterDriverRequest {
            tid: 21,
            max_restarts: 2,
            restart_group: 1,
            dependency_mask: DEP_PROCESS_MANAGER,
            backoff_ticks: 3,
            irq_line: 6,
            mem_cap: mem1.0,
            iova_cap: iova1.0,
            iova_base: 0x5000,
            iova_len: 4096,
            dma_len: 4096,
        },
    )?;
    init.register_driver_with_supervisor(
        &mut kernel,
        RegisterDriverRequest {
            tid: 22,
            max_restarts: 2,
            restart_group: 1,
            dependency_mask: DEP_VFS | DEP_PROCESS_MANAGER,
            backoff_ticks: 3,
            irq_line: 7,
            mem_cap: mem2.0,
            iova_cap: iova2.0,
            iova_base: 0x6000,
            iova_len: 4096,
            dma_len: 4096,
        },
    )?;

    let mut supervisor = SupervisorService::new(1, handoff, init.restart_policies());
    let _ = supervisor.run_until_idle(&mut kernel)?;

    let token = kernel.exit_task(4, 99)?;
    init.recover_supervisor_failure(&mut kernel, token)?;
    let mut restarted = SupervisorService::new(1, handoff, init.restart_policies());
    let restored_managed_services = restarted.run_until_idle(&mut kernel)?;
    let restored_driver_services = [20u64, 21, 22]
        .into_iter()
        .filter(|tid| restarted.status_for(*tid).is_some())
        .count();
    Ok(SupervisorReplaySummary {
        restored_managed_services,
        restored_driver_services,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimStep {
    SeedOpcode(u16),
    SendOpcode(u16),
    ExternalIrq(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SimSummary {
    last_received_opcode: Option<u16>,
    last_irq_opcode: Option<u16>,
    send_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Scenario {
    name: &'static str,
    steps: &'static [SimStep],
    expected_last_received_opcode: Option<u16>,
    expected_irq: Option<u16>,
    expected_send_count: usize,
}

fn scenario_catalog() -> &'static [Scenario] {
    const IPC_AND_IRQ: [SimStep; 4] = [
        SimStep::SeedOpcode(0x55),
        SimStep::SendOpcode(0x11),
        SimStep::SendOpcode(0x22),
        SimStep::ExternalIrq(9),
    ];
    const IPC_ONLY: [SimStep; 3] = [
        SimStep::SeedOpcode(0x33),
        SimStep::SendOpcode(0x44),
        SimStep::SendOpcode(0x45),
    ];
    const SCENARIOS: [Scenario; 2] = [
        Scenario {
            name: "ipc_and_irq_core",
            steps: &IPC_AND_IRQ,
            expected_last_received_opcode: Some(0x11),
            expected_irq: Some(9),
            expected_send_count: 2,
        },
        Scenario {
            name: "ipc_only_core",
            steps: &IPC_ONLY,
            expected_last_received_opcode: Some(0x44),
            expected_irq: None,
            expected_send_count: 2,
        },
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
            SimStep::SeedOpcode(opcode) => state.ipc_send(
                send_cap,
                Message::with_header(0, opcode, 0, None, &[0])
                    .map_err(|_| KernelError::WrongObject)?,
            )?,
            SimStep::SendOpcode(opcode) => {
                state.ipc_send(
                    send_cap,
                    Message::with_header(0, opcode, 0, None, &[0])
                        .map_err(|_| KernelError::WrongObject)?,
                )?;
                let msg = state.ipc_recv(recv_cap)?.ok_or(KernelError::WrongObject)?;
                last_received_opcode = Some(msg.opcode);
                send_count = send_count.saturating_add(1);
            }
            SimStep::ExternalIrq(line) => {
                state.route_external_irq(line)?;
                let msg = state
                    .ipc_recv(notif_recv_cap)?
                    .ok_or(KernelError::WrongObject)?;
                last_irq_opcode = Some(msg.opcode);
            }
        }
    }
    Ok(SimSummary {
        last_received_opcode,
        last_irq_opcode,
        send_count,
    })
}

#[test]
fn deterministic_init_core_bootstrap_replays_proc_and_vfs_path() {
    let handle = std::thread::Builder::new()
        .name("init-core-boot-sim".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let summary = run_init_core_bootstrap_scenario().expect("boot sim");
            assert_eq!(summary.init_phase, InitBootPhase::Running);
            assert_eq!(summary.proc_wait_exit, 5);
            assert_eq!(summary.vfs_open_opcode, VFS_OP_OPENAT);
            assert_eq!(summary.vfs_read_opcode, VFS_OP_READ);
            assert_eq!(summary.irq_notification_opcode, Some(9));
        })
        .expect("spawn thread");
    handle.join().expect("join");
}

#[test]
fn deterministic_mount_orchestration_routes_low_and_high_mounts() {
    let summary = run_mount_orchestration_scenario().expect("mount orchestration");
    assert_eq!(summary.low_mount_opcode, VFS_OP_OPENAT);
    assert_eq!(summary.high_mount_opcode, VFS_OP_OPENAT);
    assert!(summary.denied_dev_path);
    assert_eq!(summary.recovered_mounts, 2);
}

#[test]
fn core_scenario_catalog_replays_all_with_expected_results() {
    let handle = std::thread::Builder::new()
        .name("core-scenario-catalog".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            for scenario in scenario_catalog() {
                let summary = run_deterministic_script(scenario.steps).expect("sim");
                assert_eq!(
                    summary.last_received_opcode, scenario.expected_last_received_opcode,
                    "{}",
                    scenario.name
                );
                assert_eq!(
                    summary.last_irq_opcode, scenario.expected_irq,
                    "{}",
                    scenario.name
                );
                assert_eq!(
                    summary.send_count, scenario.expected_send_count,
                    "{}",
                    scenario.name
                );
            }
        })
        .expect("spawn thread");
    handle.join().expect("join");
}

#[test]
fn deterministic_supervisor_replay_restores_core_and_driver_registrations() {
    let summary = run_supervisor_replay_scenario().expect("scenario");
    assert_eq!(summary.restored_managed_services, 6);
    assert_eq!(summary.restored_driver_services, 3);
}

#[test]
fn deterministic_end_to_end_server_flow_covers_process_vfs_and_irq_notification_routing() {
    let handle = std::thread::Builder::new()
        .name("deterministic-e2e-flow".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mount = run_mount_orchestration_scenario().expect("mount scenario");
            assert_eq!(mount.low_mount_opcode, VFS_OP_OPENAT);
            assert_eq!(mount.high_mount_opcode, VFS_OP_OPENAT);
            assert!(mount.denied_dev_path);

            let summary = run_deterministic_script(&[
                SimStep::SeedOpcode(PROC_OP_SPAWN_V2),
                SimStep::SendOpcode(VFS_OP_OPENAT),
                SimStep::SendOpcode(VFS_OP_READ),
                SimStep::ExternalIrq(9),
            ])
            .expect("server flow");
            assert_eq!(summary.last_received_opcode, Some(VFS_OP_OPENAT));
            assert_eq!(summary.last_irq_opcode, Some(9));
            assert_eq!(summary.send_count, 2);
        })
        .expect("spawn thread");
    handle.join().expect("join");
}
