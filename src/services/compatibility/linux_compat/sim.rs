use crate::kernel::bootstrap::{Bootstrap, KernelError};
use crate::kernel::ipc::Message;
use crate::kernel::proc_abi::PROC_OP_GETPID;
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vfs_abi::VFS_OP_OPENAT;

use super::{LINUX_NR_GETPID, LINUX_NR_OPENAT, LinuxServiceBindings, dispatch};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimStep {
    SeedGetPid(u64),
    SeedOpenAt(u64),
    SysGetPid,
    SysOpenAt(usize),
    ExternalIrq(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimSummary {
    pub last_getpid: usize,
    pub last_openat: usize,
    pub last_irq_opcode: Option<u16>,
    pub proc_requests: usize,
    pub vfs_requests: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scenario {
    pub name: &'static str,
    pub steps: &'static [SimStep],
    pub expected_getpid: usize,
    pub expected_openat: usize,
    pub expected_irq: Option<u16>,
    pub expected_proc_requests: usize,
    pub expected_vfs_requests: usize,
}

pub fn scenario_catalog() -> &'static [Scenario] {
    const MIXED: [SimStep; 5] = [
        SimStep::SeedGetPid(500),
        SimStep::SeedOpenAt(3),
        SimStep::SysGetPid,
        SimStep::ExternalIrq(9),
        SimStep::SysOpenAt(0x1000),
    ];
    const PROC_HEAVY: [SimStep; 4] = [
        SimStep::SeedGetPid(100),
        SimStep::SeedGetPid(101),
        SimStep::SysGetPid,
        SimStep::SysGetPid,
    ];
    const VFS_HEAVY: [SimStep; 4] = [
        SimStep::SeedOpenAt(11),
        SimStep::SeedOpenAt(12),
        SimStep::SysOpenAt(0x2000),
        SimStep::SysOpenAt(0x3000),
    ];
    const SCENARIOS: [Scenario; 3] = [
        Scenario {
            name: "mixed_irq_proc_vfs",
            steps: &MIXED,
            expected_getpid: 500,
            expected_openat: 3,
            expected_irq: Some(9),
            expected_proc_requests: 1,
            expected_vfs_requests: 1,
        },
        Scenario {
            name: "proc_heavy",
            steps: &PROC_HEAVY,
            expected_getpid: 101,
            expected_openat: 0,
            expected_irq: None,
            expected_proc_requests: 2,
            expected_vfs_requests: 0,
        },
        Scenario {
            name: "vfs_heavy",
            steps: &VFS_HEAVY,
            expected_getpid: 0,
            expected_openat: 12,
            expected_irq: None,
            expected_proc_requests: 0,
            expected_vfs_requests: 2,
        },
    ];
    &SCENARIOS
}

pub fn run_scenario(scenario: &Scenario) -> Result<SimSummary, KernelError> {
    run_deterministic_script(scenario.steps)
}

pub fn run_deterministic_script(steps: &[SimStep]) -> Result<SimSummary, KernelError> {
    let mut state = Bootstrap::init()?;
    let mut bindings = LinuxServiceBindings::default();

    let (_proc_req_ep, proc_req_send, _proc_req_recv) = state.create_endpoint(16)?;
    let (_proc_rep_ep, proc_rep_send, proc_rep_recv) = state.create_endpoint(16)?;
    bindings.register_process_manager(&state, proc_req_send, proc_rep_recv)?;

    let (_vfs_req_ep, vfs_req_send, _vfs_req_recv) = state.create_endpoint(16)?;
    let (_vfs_rep_ep, vfs_rep_send, vfs_rep_recv) = state.create_endpoint(16)?;
    bindings.register_vfs_manager(&state, vfs_req_send, vfs_rep_recv)?;

    let (_notif, notif_cap, notif_recv) = state.create_notification(8)?;
    state.bind_irq_notification(9, notif_cap)?;

    let mut last_getpid = 0usize;
    let mut last_openat = 0usize;
    let mut last_irq_opcode = None;
    let mut proc_requests = 0usize;
    let mut vfs_requests = 0usize;

    for step in steps {
        match *step {
            SimStep::SeedGetPid(pid) => state.ipc_send(
                proc_rep_send,
                Message::with_header(0, PROC_OP_GETPID, 0, None, &pid.to_le_bytes())
                    .map_err(|_| KernelError::WrongObject)?,
            )?,
            SimStep::SeedOpenAt(fd) => state.ipc_send(
                vfs_rep_send,
                Message::with_header(0, VFS_OP_OPENAT, 0, None, &fd.to_le_bytes())
                    .map_err(|_| KernelError::WrongObject)?,
            )?,
            SimStep::SysGetPid => {
                let mut frame = TrapFrame::new(LINUX_NR_GETPID, [0, 0, 0, 0, 0, 0]);
                dispatch(&mut state, &bindings, &mut frame);
                last_getpid = frame.ret0();
                proc_requests = proc_requests.saturating_add(1);
            }
            SimStep::SysOpenAt(path) => {
                let mut frame = TrapFrame::new(LINUX_NR_OPENAT, [0, path, 0, 0, 0, 0]);
                dispatch(&mut state, &bindings, &mut frame);
                last_openat = frame.ret0();
                vfs_requests = vfs_requests.saturating_add(1);
            }
            SimStep::ExternalIrq(line) => {
                state.route_external_irq(line)?;
                let msg = state
                    .ipc_recv(notif_recv)?
                    .ok_or(KernelError::WrongObject)?;
                last_irq_opcode = Some(msg.opcode);
            }
        }
    }

    Ok(SimSummary {
        last_getpid,
        last_openat,
        last_irq_opcode,
        proc_requests,
        vfs_requests,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_linux_simulation_replays_mixed_subsystems() {
        let scenario = scenario_catalog()
            .iter()
            .find(|scenario| scenario.name == "mixed_irq_proc_vfs")
            .expect("scenario");
        let summary = run_scenario(scenario).expect("sim");

        assert_eq!(summary.last_getpid, scenario.expected_getpid);
        assert_eq!(summary.last_openat, scenario.expected_openat);
        assert_eq!(summary.last_irq_opcode, scenario.expected_irq);
        assert_eq!(summary.proc_requests, scenario.expected_proc_requests);
        assert_eq!(summary.vfs_requests, scenario.expected_vfs_requests);
    }

    #[test]
    fn linux_scenario_catalog_replays_all_with_expected_results() {
        for scenario in scenario_catalog() {
            let summary = run_scenario(scenario).expect("sim");
            assert_eq!(
                summary.last_getpid, scenario.expected_getpid,
                "{}",
                scenario.name
            );
            assert_eq!(
                summary.last_openat, scenario.expected_openat,
                "{}",
                scenario.name
            );
            assert_eq!(
                summary.last_irq_opcode, scenario.expected_irq,
                "{}",
                scenario.name
            );
            assert_eq!(
                summary.proc_requests, scenario.expected_proc_requests,
                "{}",
                scenario.name
            );
            assert_eq!(
                summary.vfs_requests, scenario.expected_vfs_requests,
                "{}",
                scenario.name
            );
        }
    }
}
