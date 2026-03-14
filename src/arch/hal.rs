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
    fn program_timer_deadline(&mut self, cpu: CpuId, ticks_from_now: u64);
    fn decode_trap_event(&self, context: &Self::TrapContext) -> TrapEvent;
}
