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

    /// Activate `asid` on `cpu`.
    ///
    /// Stage 199D takes `cpu` explicitly: an active address space is **CPU-local** hardware
    /// state (TTBR0_EL1 / CR3 are per-core registers), so the record of what is active must be
    /// keyed by CPU. Passing it in is what stops one core's activation from being recorded as,
    /// or overwritten by, another's.
    fn switch_address_space(&mut self, cpu: CpuId, asid: Asid);
    fn acknowledge_interrupt(&mut self, cpu: CpuId, irq_line: u16);
    fn complete_external_interrupt(&mut self, irq_line: u16);
    fn program_timer_deadline(&mut self, cpu: CpuId, ticks_from_now: u64);
    fn decode_trap_event(&self, context: &Self::TrapContext) -> TrapEvent;
}

/// Stage 199D — the AUTHORITATIVE, **per-CPU** record of which address space is activated.
///
/// This used to be a plain `Option<Asid>` field inside `SelectedIsaHal`, which lives inside
/// `KernelState` and is therefore reachable only under the broad lock. AArch64 blocker 3 needs
/// the incoming task's ASID/TTBR0 activated from the post-lock dispatch drain, where the broad
/// lock is deliberately not held — and writing the record there would be a `KernelState`
/// mutation.
///
/// Moving the record to a lock-free cell keeps ONE authority rather than creating a second:
/// [`SelectedIsaHal::active_asid_on`] READS this table, so an activation performed off-lock and
/// one performed under the broad lock are observed identically by every existing consumer.
///
/// It is **keyed by CPU**. `TTBR0_EL1` and `CR3` are per-core registers, so "the active address
/// space" is not a global fact: a single cell would let one core's activation silently
/// overwrite another's, and a consumer on CPU 0 could read CPU 1's ASID and compute the wrong
/// page-table root from it. The single-dispatcher slice makes that unobservable today, but the
/// record must not encode that assumption — the wake-only APs already run their own trap paths.
///
/// `u32::MAX` is the "nothing activated yet on this CPU" sentinel (`Asid` is a `u16`).
static ACTIVE_ASID: [core::sync::atomic::AtomicU32; crate::kernel::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU32::new(u32::MAX) }; crate::kernel::scheduler::MAX_CPUS];

/// Record an address-space activation on `cpu`. Called by every activation path, on and off
/// lock. Out-of-range CPU indices are ignored rather than aliasing another CPU's slot.
pub(crate) fn note_address_space_activated(cpu: CpuId, asid: Asid) {
    let idx = cpu.0 as usize;
    if idx < crate::kernel::scheduler::MAX_CPUS {
        ACTIVE_ASID[idx].store(asid.0 as u32, core::sync::atomic::Ordering::Release);
    }
}

/// The address space currently activated on `cpu`, or `None` if none has been.
pub(crate) fn active_address_space(cpu: CpuId) -> Option<Asid> {
    let idx = cpu.0 as usize;
    if idx >= crate::kernel::scheduler::MAX_CPUS {
        return None;
    }
    match ACTIVE_ASID[idx].load(core::sync::atomic::Ordering::Acquire) {
        u32::MAX => None,
        raw => Some(Asid(raw as u16)),
    }
}

/// Clear one CPU's activation record. Test-support and boot-reset only.
// Reachable from the tests below and from a boot reset; a plain hosted `lib` build compiles no
// route to it, exactly like the sibling `*_from_raw` domain projectors.
#[allow(dead_code)]
pub(crate) fn reset_active_address_space(cpu: CpuId) {
    let idx = cpu.0 as usize;
    if idx < crate::kernel::scheduler::MAX_CPUS {
        ACTIVE_ASID[idx].store(u32::MAX, core::sync::atomic::Ordering::Release);
    }
}

/// Clear every CPU's activation record. Test-support and boot-reset only.
#[allow(dead_code)]
pub(crate) fn reset_all_active_address_spaces() {
    for slot in ACTIVE_ASID.iter() {
        slot.store(u32::MAX, core::sync::atomic::Ordering::Release);
    }
}

#[derive(Debug, Default)]
pub struct SelectedIsaHal {
    _private: (),
}

impl SelectedIsaHal {
    /// The address space currently activated on `cpu`.
    ///
    /// Reads the single authoritative per-CPU record, so a switch performed by the off-lock
    /// AArch64 dispatch drain is visible here exactly as an in-lock switch is.
    pub fn active_asid_on(&self, cpu: CpuId) -> Option<Asid> {
        active_address_space(cpu)
    }
}

impl Hal for SelectedIsaHal {
    type TrapContext = crate::arch::hal_adapters::AdapterTrapContext;

    fn switch_address_space(&mut self, cpu: CpuId, asid: Asid) {
        crate::arch::hal_adapters::switch_address_space(asid);
        note_address_space_activated(cpu, asid);
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
        last_asid: Option<(CpuId, Asid)>,
        last_irq: Option<(CpuId, u16)>,
        last_completed_irq: Option<u16>,
        last_timer: Option<(CpuId, u64)>,
    }

    impl Hal for MockRiscvHal {
        type TrapContext = RiscvTrapContext;

        fn switch_address_space(&mut self, cpu: CpuId, asid: Asid) {
            self.last_asid = Some((cpu, asid));
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
        last_asid: Option<(CpuId, Asid)>,
        last_irq: Option<(CpuId, u16)>,
        last_completed_irq: Option<u16>,
        last_timer: Option<(CpuId, u64)>,
    }

    impl Hal for MockX86Hal {
        type TrapContext = X86TrapContext;

        fn switch_address_space(&mut self, cpu: CpuId, asid: Asid) {
            self.last_asid = Some((cpu, asid));
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
        last_asid: Option<(CpuId, Asid)>,
        last_irq: Option<(CpuId, u16)>,
        last_completed_irq: Option<u16>,
        last_timer: Option<(CpuId, u64)>,
    }

    impl Hal for MockAarch64Hal {
        type TrapContext = Aarch64TrapContext;

        fn switch_address_space(&mut self, cpu: CpuId, asid: Asid) {
            self.last_asid = Some((cpu, asid));
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
        hal.switch_address_space(CpuId(0), Asid(3));
        hal.acknowledge_interrupt(CpuId(0), 9);
        hal.complete_external_interrupt(9);
        hal.program_timer_deadline(CpuId(0), 100);

        let trap = hal.decode_trap_event(&RiscvTrapContext {
            scause: 8,
            stval: 0,
        });
        assert_eq!(trap, TrapEvent::Syscall);
        assert_eq!(hal.last_asid, Some((CpuId(0), Asid(3))));
        assert_eq!(hal.last_irq, Some((CpuId(0), 9)));
        assert_eq!(hal.last_completed_irq, Some(9));
        assert_eq!(hal.last_timer, Some((CpuId(0), 100)));
    }

    #[test]
    fn hal_contract_is_isa_agnostic_for_x86_like_impl() {
        let mut hal = MockX86Hal::default();
        hal.switch_address_space(CpuId(1), Asid(7));
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
        assert_eq!(hal.last_asid, Some((CpuId(1), Asid(7))));
        assert_eq!(hal.last_irq, Some((CpuId(1), 33)));
        assert_eq!(hal.last_completed_irq, Some(33));
        assert_eq!(hal.last_timer, Some((CpuId(1), 250)));
    }

    #[test]
    fn hal_contract_is_isa_agnostic_for_aarch64_like_impl() {
        let mut hal = MockAarch64Hal::default();
        hal.switch_address_space(CpuId(2), Asid(9));
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
        assert_eq!(hal.last_asid, Some((CpuId(2), Asid(9))));
        assert_eq!(hal.last_irq, Some((CpuId(2), 41)));
        assert_eq!(hal.last_completed_irq, Some(41));
        assert_eq!(hal.last_timer, Some((CpuId(2), 500)));
    }

    #[test]
    fn selected_isa_hal_tracks_last_switched_asid() {
        reset_all_active_address_spaces();
        let mut hal = SelectedIsaHal::default();
        assert_eq!(hal.active_asid_on(CpuId(0)), None, "nothing activated yet");
        hal.switch_address_space(CpuId(0), Asid(42));
        assert_eq!(hal.active_asid_on(CpuId(0)), Some(Asid(42)));
        reset_all_active_address_spaces();
    }

    /// Stage 199D: the activation record is authoritative, not a per-HAL cache. An activation
    /// recorded by the off-lock AArch64 dispatch drain — which holds no `KernelState` and so
    /// has no HAL instance to write into — is observed by an ordinary in-lock read exactly as
    /// an in-lock switch would be. One record, both paths.
    #[test]
    fn off_lock_activation_is_visible_through_the_hal_accessor() {
        reset_all_active_address_spaces();
        let hal = SelectedIsaHal::default();
        note_address_space_activated(CpuId(0), Asid(17));
        assert_eq!(hal.active_asid_on(CpuId(0)), Some(Asid(17)));
        // A second HAL instance agrees — there is no per-instance cache to go stale.
        assert_eq!(
            SelectedIsaHal::default().active_asid_on(CpuId(0)),
            Some(Asid(17))
        );
        reset_all_active_address_spaces();
        assert_eq!(hal.active_asid_on(CpuId(0)), None);
    }

    /// **The activation record is CPU-local.** `TTBR0_EL1` / `CR3` are per-core registers, so
    /// two CPUs may legitimately have different address spaces activated at the same instant.
    /// A single global cell would let the second activation overwrite the first and make a
    /// consumer on CPU 0 read CPU 1's ASID — the defect this table exists to prevent.
    #[test]
    fn two_cpus_hold_independent_active_address_spaces() {
        reset_all_active_address_spaces();
        let mut hal = SelectedIsaHal::default();
        hal.switch_address_space(CpuId(0), Asid(11));
        hal.switch_address_space(CpuId(1), Asid(22));
        assert_eq!(hal.active_asid_on(CpuId(0)), Some(Asid(11)));
        assert_eq!(hal.active_asid_on(CpuId(1)), Some(Asid(22)));

        // Re-activating on CPU 1 leaves CPU 0 completely untouched.
        hal.switch_address_space(CpuId(1), Asid(33));
        assert_eq!(hal.active_asid_on(CpuId(0)), Some(Asid(11)));
        assert_eq!(hal.active_asid_on(CpuId(1)), Some(Asid(33)));

        // Clearing one CPU does not clear the other.
        reset_active_address_space(CpuId(1));
        assert_eq!(hal.active_asid_on(CpuId(0)), Some(Asid(11)));
        assert_eq!(hal.active_asid_on(CpuId(1)), None);

        // Every other CPU is independently unset.
        for cpu in 2..crate::kernel::scheduler::MAX_CPUS {
            assert_eq!(hal.active_asid_on(CpuId(cpu as u8)), None);
        }
        reset_all_active_address_spaces();
    }

    /// An out-of-range CPU index aliases no slot: it records nothing and reads `None`.
    #[test]
    fn an_out_of_range_cpu_aliases_no_slot() {
        reset_all_active_address_spaces();
        let out = CpuId(crate::kernel::scheduler::MAX_CPUS as u8);
        note_address_space_activated(out, Asid(99));
        assert_eq!(active_address_space(out), None);
        for cpu in 0..crate::kernel::scheduler::MAX_CPUS {
            assert_eq!(
                active_address_space(CpuId(cpu as u8)),
                None,
                "an out-of-range activation must not land in any real slot"
            );
        }
    }
}
