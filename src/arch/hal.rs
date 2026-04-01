// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::scheduler::CpuId;
use crate::kernel::trap::TrapEvent;
use crate::kernel::vm::Asid;

/// Thin machine-adaptation boundary for YARM kernel core.
///
/// The kernel core should only rely on these primitive operations:
/// - address-space switch
/// - interrupt acknowledge/delivery handoff
/// - timer programming
pub trait Hal {
    type TrapContext;

    fn switch_address_space(&mut self, asid: Asid);
    fn acknowledge_interrupt(&mut self, cpu: CpuId, irq_line: u16);
    fn complete_external_interrupt(&mut self, irq_line: u16);
    fn program_timer_deadline(&mut self, cpu: CpuId, ticks_from_now: u64);
    fn decode_trap_event(&self, context: &Self::TrapContext) -> TrapEvent;
}

#[derive(Debug, Default)]
pub struct SelectedIsaHal {
    active_asid: Option<Asid>,
}

impl SelectedIsaHal {
    pub fn active_asid(&self) -> Option<Asid> {
        self.active_asid
    }
}

impl Hal for SelectedIsaHal {
    type TrapContext = crate::arch::hal_adapters::AdapterTrapContext;

    fn switch_address_space(&mut self, asid: Asid) {
        crate::arch::hal_adapters::switch_address_space(asid);
        self.active_asid = Some(asid);
    }

    fn acknowledge_interrupt(&mut self, _cpu: CpuId, irq_line: u16) {
        crate::arch::hal_adapters::acknowledge_interrupt(_cpu, irq_line);
    }

    fn complete_external_interrupt(&mut self, irq_line: u16) {
        crate::arch::hal_adapters::complete_external_interrupt(irq_line);
    }

    fn program_timer_deadline(&mut self, cpu: CpuId, ticks_from_now: u64) {
        crate::arch::hal_adapters::program_timer_deadline(cpu, ticks_from_now);
    }

    fn decode_trap_event(&self, context: &Self::TrapContext) -> TrapEvent {
        crate::arch::hal_adapters::decode_trap_event(*context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::trap::{FaultAccess, FaultInfo};
    use crate::kernel::vm::VirtAddr;

    #[derive(Debug, Clone, Copy)]
    struct RiscvTrapContext {
        scause: usize,
        stval: usize,
    }

    #[derive(Debug, Default)]
    struct MockRiscvHal {
        last_asid: Option<Asid>,
        last_irq: Option<(CpuId, u16)>,
        last_completed_irq: Option<u16>,
        last_timer: Option<(CpuId, u64)>,
    }

    impl Hal for MockRiscvHal {
        type TrapContext = RiscvTrapContext;

        fn switch_address_space(&mut self, asid: Asid) {
            self.last_asid = Some(asid);
        }

        fn acknowledge_interrupt(&mut self, cpu: CpuId, irq_line: u16) {
            self.last_irq = Some((cpu, irq_line));
        }

        fn complete_external_interrupt(&mut self, irq_line: u16) {
            self.last_completed_irq = Some(irq_line);
        }

        fn program_timer_deadline(&mut self, cpu: CpuId, ticks_from_now: u64) {
            self.last_timer = Some((cpu, ticks_from_now));
        }

        fn decode_trap_event(&self, context: &Self::TrapContext) -> TrapEvent {
            if context.scause == 8 {
                TrapEvent::Syscall
            } else {
                TrapEvent::PageFault(FaultInfo {
                    addr: VirtAddr(context.stval as u64),
                    access: FaultAccess::Write,
                })
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct X86TrapContext {
        vector: u8,
        fault_addr: usize,
    }

    #[derive(Debug, Default)]
    struct MockX86Hal {
        last_asid: Option<Asid>,
        last_irq: Option<(CpuId, u16)>,
        last_completed_irq: Option<u16>,
        last_timer: Option<(CpuId, u64)>,
    }

    impl Hal for MockX86Hal {
        type TrapContext = X86TrapContext;

        fn switch_address_space(&mut self, asid: Asid) {
            self.last_asid = Some(asid);
        }

        fn acknowledge_interrupt(&mut self, cpu: CpuId, irq_line: u16) {
            self.last_irq = Some((cpu, irq_line));
        }

        fn complete_external_interrupt(&mut self, irq_line: u16) {
            self.last_completed_irq = Some(irq_line);
        }

        fn program_timer_deadline(&mut self, cpu: CpuId, ticks_from_now: u64) {
            self.last_timer = Some((cpu, ticks_from_now));
        }

        fn decode_trap_event(&self, context: &Self::TrapContext) -> TrapEvent {
            if context.vector == 0x80 {
                TrapEvent::Syscall
            } else {
                TrapEvent::PageFault(FaultInfo {
                    addr: VirtAddr(context.fault_addr as u64),
                    access: FaultAccess::Read,
                })
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct Aarch64TrapContext {
        esr: u32,
        far: u64,
    }

    #[derive(Debug, Default)]
    struct MockAarch64Hal {
        last_asid: Option<Asid>,
        last_irq: Option<(CpuId, u16)>,
        last_completed_irq: Option<u16>,
        last_timer: Option<(CpuId, u64)>,
    }

    impl Hal for MockAarch64Hal {
        type TrapContext = Aarch64TrapContext;

        fn switch_address_space(&mut self, asid: Asid) {
            self.last_asid = Some(asid);
        }

        fn acknowledge_interrupt(&mut self, cpu: CpuId, irq_line: u16) {
            self.last_irq = Some((cpu, irq_line));
        }

        fn complete_external_interrupt(&mut self, irq_line: u16) {
            self.last_completed_irq = Some(irq_line);
        }

        fn program_timer_deadline(&mut self, cpu: CpuId, ticks_from_now: u64) {
            self.last_timer = Some((cpu, ticks_from_now));
        }

        fn decode_trap_event(&self, context: &Self::TrapContext) -> TrapEvent {
            if context.esr == 0x15 {
                TrapEvent::Syscall
            } else {
                TrapEvent::PageFault(FaultInfo {
                    addr: VirtAddr(context.far),
                    access: FaultAccess::Read,
                })
            }
        }
    }

    #[test]
    fn hal_contract_is_isa_agnostic_for_riscv_like_impl() {
        let mut hal = MockRiscvHal::default();
        hal.switch_address_space(Asid(3));
        hal.acknowledge_interrupt(CpuId(0), 9);
        hal.complete_external_interrupt(9);
        hal.program_timer_deadline(CpuId(0), 100);

        let trap = hal.decode_trap_event(&RiscvTrapContext {
            scause: 8,
            stval: 0,
        });
        assert_eq!(trap, TrapEvent::Syscall);
        assert_eq!(hal.last_asid, Some(Asid(3)));
        assert_eq!(hal.last_irq, Some((CpuId(0), 9)));
        assert_eq!(hal.last_completed_irq, Some(9));
        assert_eq!(hal.last_timer, Some((CpuId(0), 100)));
    }

    #[test]
    fn hal_contract_is_isa_agnostic_for_x86_like_impl() {
        let mut hal = MockX86Hal::default();
        hal.switch_address_space(Asid(7));
        hal.acknowledge_interrupt(CpuId(1), 33);
        hal.complete_external_interrupt(33);
        hal.program_timer_deadline(CpuId(1), 250);

        let trap = hal.decode_trap_event(&X86TrapContext {
            vector: 14,
            fault_addr: 0xDEAD_0000,
        });
        assert_eq!(
            trap,
            TrapEvent::PageFault(FaultInfo {
                addr: VirtAddr(0xDEAD_0000),
                access: FaultAccess::Read,
            })
        );
        assert_eq!(hal.last_asid, Some(Asid(7)));
        assert_eq!(hal.last_irq, Some((CpuId(1), 33)));
        assert_eq!(hal.last_completed_irq, Some(33));
        assert_eq!(hal.last_timer, Some((CpuId(1), 250)));
    }

    #[test]
    fn hal_contract_is_isa_agnostic_for_aarch64_like_impl() {
        let mut hal = MockAarch64Hal::default();
        hal.switch_address_space(Asid(9));
        hal.acknowledge_interrupt(CpuId(2), 41);
        hal.complete_external_interrupt(41);
        hal.program_timer_deadline(CpuId(2), 500);

        let trap = hal.decode_trap_event(&Aarch64TrapContext {
            esr: 0x24,
            far: 0xABCD_1000,
        });
        assert_eq!(
            trap,
            TrapEvent::PageFault(FaultInfo {
                addr: VirtAddr(0xABCD_1000),
                access: FaultAccess::Read,
            })
        );
        assert_eq!(hal.last_asid, Some(Asid(9)));
        assert_eq!(hal.last_irq, Some((CpuId(2), 41)));
        assert_eq!(hal.last_completed_irq, Some(41));
        assert_eq!(hal.last_timer, Some((CpuId(2), 500)));
    }

    #[test]
    fn selected_isa_hal_tracks_last_switched_asid() {
        let mut hal = SelectedIsaHal::default();
        hal.switch_address_space(Asid(42));
        assert_eq!(hal.active_asid(), Some(Asid(42)));
    }
}
