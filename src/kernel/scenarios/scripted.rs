use crate::kernel::bootstrap::{Bootstrap, KernelError};
use crate::kernel::ipc::Message;

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
