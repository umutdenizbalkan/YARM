use super::bootstrap::{Bootstrap, KernelError};
use super::ipc::Message;
use super::init_server::{
    CoreServiceGraph, CoreServiceImagePlan, InitBootPhase, InitFaultHandoff, InitServerLite,
};
use super::proc_proto::{SpawnV2Args, WaitPidV2Args, PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2};
use super::process_manager::{ProcessService, SpawnV2Result, WaitPidV2Result};
use super::vfs::{
    openat_message, read_message, MountRouter, OpenAtRequest, ReadWriteRequest, VfsLiteService,
};
use crate::services::fs::initramfs::{INITRAMFS_BUSYBOX_PATH_PTR, InitramfsBackend};
use crate::services::fs::ramfs::RamFsBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitBootSummary {
    pub init_phase: InitBootPhase,
    pub proc_wait_exit: u64,
    pub vfs_open_opcode: u16,
    pub vfs_read_opcode: u16,
    pub irq_notification_opcode: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountOrchestrationSummary {
    pub low_mount_opcode: u16,
    pub high_mount_opcode: u16,
}

pub fn run_mount_orchestration_scenario() -> Result<MountOrchestrationSummary, KernelError> {
    let router = MountRouter::new(0x8000, RamFsBackend::new(), InitramfsBackend::new(4096));
    let mut vfs = VfsLiteService::with_backend(router);

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
        path_ptr: INITRAMFS_BUSYBOX_PATH_PTR,
        flags: 0,
        mode: 0,
    })
    .map_err(|_| KernelError::WrongObject)?;
    let high_rep = vfs
        .handle_request(open_high)
        .map_err(|_| KernelError::WrongObject)?;

    Ok(MountOrchestrationSummary {
        low_mount_opcode: low_rep.opcode,
        high_mount_opcode: high_rep.opcode,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountFallbackTelemetry {
    pub recovered_with_fat: bool,
    pub mounted_count: usize,
}

pub fn run_mount_fallback_telemetry_scenario() -> Result<MountFallbackTelemetry, KernelError> {
    let init = InitServerLite::new();
    let report = init.execute_mount_plan_with_fail_at(Some(3))?;
    Ok(MountFallbackTelemetry {
        recovered_with_fat: report.recovered_with_fat,
        mounted_count: report.mounted_count,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountMatrixRow {
    pub fail_at: Option<usize>,
    pub allow_fallback: bool,
    pub result: Result<MountFallbackTelemetry, KernelError>,
}

pub fn run_mount_failure_matrix_scenarios() -> [MountMatrixRow; 10] {
    core::array::from_fn(|i| {
        let allow_fallback = i >= 5;
        let fail_at = match i % 5 {
            0 => None,
            1 => Some(0),
            2 => Some(1),
            3 => Some(2),
            _ => Some(3),
        };
        let mut init = InitServerLite::new();
        let mut plan = init.mount_plan();
        plan.allow_fallback_to_fat = allow_fallback;
        let _ = init.set_mount_plan(plan);
        MountMatrixRow {
            fail_at,
            allow_fallback,
            result: init.execute_mount_plan_with_fail_at(fail_at).map(|report| {
                MountFallbackTelemetry {
                    recovered_with_fat: report.recovered_with_fat,
                    mounted_count: report.mounted_count,
                }
            }),
        }
    })
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
    init.validate_core_delegation_paths(&kernel, graph.init_tid)?;
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
    let child =
        SpawnV2Result::decode(spawn_rep.as_slice()).map_err(|_| KernelError::WrongObject)?;
    proc.mark_exit(child.pid, 5)
        .map_err(|_| KernelError::WrongObject)?;
    let wait = Message::with_header(
        0,
        PROC_OP_WAITPID_V2,
        0,
        None,
        &WaitPidV2Args::new(1, child.pid).encode(),
    )
    .map_err(|_| KernelError::WrongObject)?;
    let wait_rep = proc.handle(wait).map_err(|_| KernelError::WrongObject)?;
    let waited =
        WaitPidV2Result::decode(wait_rep.as_slice()).map_err(|_| KernelError::WrongObject)?;

    let mut vfs = VfsLiteService::with_backend(InitramfsBackend::new(4096));
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimStep {
    SeedOpcode(u16),
    SendOpcode(u16),
    ExternalIrq(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimSummary {
    pub last_received_opcode: Option<u16>,
    pub last_irq_opcode: Option<u16>,
    pub send_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scenario {
    pub name: &'static str,
    pub steps: &'static [SimStep],
    pub expected_last_received_opcode: Option<u16>,
    pub expected_irq: Option<u16>,
    pub expected_send_count: usize,
}

pub fn scenario_catalog() -> &'static [Scenario] {
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

pub fn run_scenario(scenario: &Scenario) -> Result<SimSummary, KernelError> {
    run_deterministic_script(scenario.steps)
}

pub fn run_deterministic_script(steps: &[SimStep]) -> Result<SimSummary, KernelError> {
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

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use super::super::vfs_proto::{VFS_OP_OPENAT, VFS_OP_READ};

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
    fn deterministic_mount_fallback_telemetry_is_reported() {
        let telem = run_mount_fallback_telemetry_scenario().expect("mount fallback telemetry");
        assert!(telem.recovered_with_fat);
        assert!(telem.mounted_count >= 4);
    }

    #[test]
    fn mount_failure_matrix_without_fallback_fails_all_fail_points() {
        let matrix = run_mount_failure_matrix_scenarios();
        for row in matrix.iter().filter(|row| !row.allow_fallback) {
            if row.fail_at.is_some() {
                assert!(row.result.is_err(), "row={:?}", row.fail_at);
            } else {
                assert!(row.result.is_ok());
            }
        }
    }

    #[test]
    fn mount_failure_matrix_with_fallback_recovers() {
        let matrix = run_mount_failure_matrix_scenarios();
        for row in matrix.iter().filter(|row| row.allow_fallback) {
            let report = row.result.expect("fallback row should recover");
            if row.fail_at.is_some() {
                assert!(report.recovered_with_fat);
            } else {
                assert!(!report.recovered_with_fat);
            }
        }
    }

    #[test]
    fn deterministic_mount_orchestration_routes_low_and_high_mounts() {
        let summary = run_mount_orchestration_scenario().expect("mount orchestration");
        assert_eq!(summary.low_mount_opcode, VFS_OP_OPENAT);
        assert_eq!(summary.high_mount_opcode, VFS_OP_OPENAT);
    }

    #[test]
    fn deterministic_core_simulation_replays_ipc_and_irq() {
        let handle = std::thread::Builder::new()
            .name("core-sim-ipc-irq".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let scenario = scenario_catalog()
                    .iter()
                    .find(|scenario| scenario.name == "ipc_and_irq_core")
                    .expect("scenario");
                let summary = run_scenario(scenario).expect("sim");

                assert_eq!(
                    summary.last_received_opcode,
                    scenario.expected_last_received_opcode
                );
                assert_eq!(summary.last_irq_opcode, scenario.expected_irq);
                assert_eq!(summary.send_count, scenario.expected_send_count);
            })
            .expect("spawn thread");
        handle.join().expect("join");
    }

    #[test]
    fn core_scenario_catalog_replays_all_with_expected_results() {
        let handle = std::thread::Builder::new()
            .name("core-scenario-catalog".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                for scenario in scenario_catalog() {
                    let summary = run_scenario(scenario).expect("sim");
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
}
