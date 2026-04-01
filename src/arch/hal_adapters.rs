// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::scheduler::CpuId;
use crate::kernel::vm::Asid;

pub type AdapterTrapContext = crate::arch::trap_entry::ArchTrapContext;
pub type AdapterTrapEvent = crate::arch::trap::TrapEvent;

#[inline]
pub fn switch_address_space(asid: Asid) {
    let _ = crate::arch::selected_isa::page_table::activate_asid(asid);
}

#[inline]
pub fn acknowledge_interrupt(_cpu: CpuId, irq_line: u16) {
    crate::arch::selected_isa::irq::acknowledge_interrupt(irq_line);
}

#[inline]
pub fn complete_external_interrupt(irq_line: u16) {
    crate::arch::selected_isa::irq::external_irq_eoi(irq_line);
}

#[inline]
pub fn program_timer_deadline(cpu: CpuId, ticks_from_now: u64) {
    crate::arch::selected_isa::irq::program_timer_deadline(cpu, ticks_from_now);
}

#[inline]
pub fn decode_trap_event(context: AdapterTrapContext) -> AdapterTrapEvent {
    crate::arch::trap_entry::decode_trap_context(context)
}
