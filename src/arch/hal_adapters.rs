// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::scheduler::CpuId;
use crate::kernel::vm::Asid;

pub type AdapterTrapContext = crate::arch::trap_entry::ArchTrapContext;
pub type AdapterTrapEvent = crate::arch::trap::TrapEvent;
const DEBUG_ASID_SWITCH: bool = false;

#[inline]
pub fn switch_address_space(asid: Asid) {
    // RISC-V: defer enabling Sv39 paging. Writing the per-task `satp` here would
    // translate every subsequent kernel instruction fetch through a page table
    // that maps only user pages — not the kernel text or the S-mode trap vector
    // — so the very next fetch would fault with no mapped handler to report it
    // (a silent dead-lock). Paging activation is intentionally gated until the
    // U-mode entry + kernel-in-every-address-space (trampoline) bring-up lands;
    // the kernel runs identity-mapped (satp=0) until then. See
    // RISCV_USERSPACE_DEFERRED. This keeps the boot deterministic and honest:
    // no address-space switch is performed because no user task is entered yet.
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    {
        static RISCV_PAGING_DEFERRED_LOGGED: core::sync::atomic::AtomicBool =
            core::sync::atomic::AtomicBool::new(false);
        if !RISCV_PAGING_DEFERRED_LOGGED.swap(true, core::sync::atomic::Ordering::AcqRel) {
            crate::yarm_log!(
                "RISCV_PAGING_DEFERRED asid={} reason=no_umode_paging_bringup",
                asid.0
            );
        }
    }
    #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "riscv64")))]
    {
        #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
        let mut cr3_before: u64 = 0;
        #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
        unsafe {
            core::arch::asm!(
                "mov {}, cr3",
                out(reg) cr3_before,
                options(nostack, preserves_flags)
            );
        }
        match crate::arch::selected_isa::page_table::activate_asid(asid) {
            Ok(target_root) => {
                #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
                {
                    let mut cr3_after: u64 = 0;
                    unsafe {
                        core::arch::asm!(
                            "mov {}, cr3",
                            out(reg) cr3_after,
                            options(nostack, preserves_flags)
                        );
                    }
                    if DEBUG_ASID_SWITCH {
                        crate::yarm_log!(
                            "ADDRESS_SPACE_SWITCH_OK asid={} target_root=0x{:x} before=0x{:x} after=0x{:x}",
                            asid.0,
                            target_root,
                            cr3_before,
                            cr3_after
                        );
                    }
                }
                #[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
                if DEBUG_ASID_SWITCH {
                    crate::yarm_log!(
                        "ADDRESS_SPACE_SWITCH_OK asid={} target_root=0x{:x}",
                        asid.0,
                        target_root
                    );
                }
            }
            Err(err) => {
                crate::yarm_log!("ADDRESS_SPACE_SWITCH_FAIL asid={} err={:?}", asid.0, err);
            }
        }
    }
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
