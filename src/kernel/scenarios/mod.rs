mod boot;
mod mount;
mod scripted;

pub use boot::{InitBootSummary, run_init_core_bootstrap_scenario};
pub use mount::{
    MountFallbackTelemetry, MountMatrixRow, MountOrchestrationSummary,
    run_mount_failure_matrix_scenarios, run_mount_fallback_telemetry_scenario,
    run_mount_orchestration_scenario,
};
pub use scripted::{
    Scenario, SimStep, SimSummary, run_deterministic_script, run_scenario, scenario_catalog,
};

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::kernel::init::InitBootPhase;
    use crate::kernel::vfs_abi::{VFS_OP_OPENAT, VFS_OP_READ};

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
                    assert_eq!(summary.send_count, scenario.expected_send_count, "{}", scenario.name);
                }
            })
            .expect("spawn thread");
        handle.join().expect("join");
    }
}
