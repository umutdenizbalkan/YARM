// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! AP-SCHED4 §A2 — which logical CPU is executing, on x86_64, from that CPU's own hardware
//! identity. Pure and arch-neutral, so the hosted suite executes the lookup the x86_64 port
//! performs on every trap.
//!
//! # Two identities, kept distinct
//!
//! * **Hardware identity** — the local APIC ID. Each CPU reads its own (`CPUID.01H:EBX[31:24]`,
//!   the initial APIC ID, which software cannot rewrite). APIC IDs are firmware's numbering: they
//!   need not be contiguous and the boot CPU's need not be 0.
//! * **Logical identity** — the scheduler's `CpuId`. Logical CPU 0 is the boot CPU.
//!
//! The only bridge between them is an explicit [`ApicCpuMap`] binding, made once per CPU before
//! that CPU can execute kernel code that asks who it is: the boot CPU binds itself first thing in
//! boot, and the boot CPU binds each secondary before it sends the start-up IPI. A lookup that
//! finds no binding answers `None`. There is no fallback — not "the APIC ID is the logical id",
//! not "the boot CPU", and not the scheduler's ambient `current_cpu` field, which names whichever
//! CPU last bound it and is not an identity at all.

use core::sync::atomic::{AtomicU8, AtomicU16, Ordering};

/// xAPIC IDs are eight bits; this is the size of the hardware-identity space the map covers.
pub const APIC_ID_SPACE: usize = 256;

/// The logical-CPU table capacity (the scheduler's `MAX_CPUS`).
pub const MAX_LOGICAL_CPUS: usize = crate::arch::platform_constants::MAX_CPUS;

const NO_LOGICAL: u8 = u8::MAX;
const NO_APIC: u16 = u16::MAX;

const _: () = assert!(MAX_LOGICAL_CPUS < NO_LOGICAL as usize);

/// Why a binding was refused. Every refusal leaves the map unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindError {
    /// The APIC ID does not fit the xAPIC identity space.
    ApicOutOfRange,
    /// The logical id is beyond the scheduler's tables.
    LogicalOutOfRange,
    /// This APIC ID already names a different logical CPU.
    ApicAlreadyBound { logical: u8 },
    /// This logical CPU already belongs to a different APIC ID.
    LogicalAlreadyBound { apic: u32 },
}

impl BindError {
    pub const fn marker(self) -> &'static str {
        match self {
            Self::ApicOutOfRange => "apic_out_of_range",
            Self::LogicalOutOfRange => "logical_out_of_range",
            Self::ApicAlreadyBound { .. } => "apic_already_bound",
            Self::LogicalAlreadyBound { .. } => "logical_already_bound",
        }
    }
}

/// A successful binding: new, or the identical pair already present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    New,
    AlreadyExact,
}

/// The explicit, bidirectional APIC-ID ↔ logical-CPU binding.
///
/// Both directions are written under one rule — a pair is accepted only if neither side is bound
/// to anything else — so the map is always a partial bijection. Bindings are never removed: a CPU
/// that is not admitted is never bound in the first place.
pub struct ApicCpuMap {
    apic_to_logical: [AtomicU8; APIC_ID_SPACE],
    logical_to_apic: [AtomicU16; MAX_LOGICAL_CPUS],
    /// Serializes writers (bindings happen on the boot CPU, before the bound CPU runs); readers
    /// are lock-free.
    writer: crate::kernel::lock::SpinLock<()>,
}

impl ApicCpuMap {
    pub const fn new() -> Self {
        Self {
            apic_to_logical: [const { AtomicU8::new(NO_LOGICAL) }; APIC_ID_SPACE],
            logical_to_apic: [const { AtomicU16::new(NO_APIC) }; MAX_LOGICAL_CPUS],
            writer: crate::kernel::lock::SpinLock::new(()),
        }
    }

    /// Bind `apic` ↔ `logical`. Idempotent for the identical pair.
    pub fn bind(&self, apic: u32, logical: u8) -> Result<Bound, BindError> {
        if apic as usize >= APIC_ID_SPACE {
            return Err(BindError::ApicOutOfRange);
        }
        if logical as usize >= MAX_LOGICAL_CPUS {
            return Err(BindError::LogicalOutOfRange);
        }
        let _w = self.writer.lock();
        let have_logical = self.apic_to_logical[apic as usize].load(Ordering::Acquire);
        let have_apic = self.logical_to_apic[logical as usize].load(Ordering::Acquire);
        if have_logical == logical && have_apic == apic as u16 {
            return Ok(Bound::AlreadyExact);
        }
        if have_logical != NO_LOGICAL {
            return Err(BindError::ApicAlreadyBound {
                logical: have_logical,
            });
        }
        if have_apic != NO_APIC {
            return Err(BindError::LogicalAlreadyBound {
                apic: u32::from(have_apic),
            });
        }
        // Logical side first: a reader resolving `apic` finds a logical id whose reverse entry is
        // already published.
        self.logical_to_apic[logical as usize].store(apic as u16, Ordering::Release);
        self.apic_to_logical[apic as usize].store(logical, Ordering::Release);
        Ok(Bound::New)
    }

    /// The logical CPU `apic` is bound to, or `None`.
    pub fn logical_for(&self, apic: u32) -> Option<u8> {
        let logical = self
            .apic_to_logical
            .get(apic as usize)?
            .load(Ordering::Acquire);
        (logical != NO_LOGICAL).then_some(logical)
    }

    /// The APIC ID `logical` is bound to, or `None`.
    pub fn apic_for(&self, logical: u8) -> Option<u32> {
        let apic = self
            .logical_to_apic
            .get(logical as usize)?
            .load(Ordering::Acquire);
        (apic != NO_APIC).then_some(u32::from(apic))
    }

    /// How many logical CPUs are bound.
    pub fn bound_count(&self) -> usize {
        self.logical_to_apic
            .iter()
            .filter(|a| a.load(Ordering::Acquire) != NO_APIC)
            .count()
    }
}

impl Default for ApicCpuMap {
    fn default() -> Self {
        Self::new()
    }
}

/// The x86_64 port's one map.
pub static X86_APIC_MAP: ApicCpuMap = ApicCpuMap::new();

/// The executing CPU's logical id, given the APIC ID it read from its own hardware: the map's
/// answer and nothing else.
pub fn executing_logical_cpu(map: &ApicCpuMap, own_apic_id: u32) -> Option<u8> {
    map.logical_for(own_apic_id)
}

/// Hosted model of the hardware read. On the target the APIC ID comes from `CPUID`; the hosted
/// suite has no second CPU, so a test states which APIC ID "this CPU" reads and the SAME lookup
/// resolves it. Unset (every test that does not opt in), hosted builds keep their single-CPU
/// answer — the scheduler's bound CPU — which is what every pre-existing hosted test drives.
#[cfg(all(feature = "hosted-dev", not(target_os = "none")))]
pub mod hosted {
    use super::{ApicCpuMap, executing_logical_cpu};
    use crate::std::cell::Cell;

    fn fresh_map() -> &'static ApicCpuMap {
        crate::std::boxed::Box::leak(crate::std::boxed::Box::new(ApicCpuMap::new()))
    }

    crate::std::thread_local! {
        /// The simulated "this CPU": its map and the APIC ID it reads.
        static SIMULATED: Cell<Option<(&'static ApicCpuMap, u32)>> = const { Cell::new(None) };
        /// The map hosted SMP start-up binds into when no simulation is active.
        static DEFAULT_MAP: &'static ApicCpuMap = fresh_map();
    }

    /// The map in force on this thread: the simulation's, else the thread's default one.
    pub fn map() -> &'static ApicCpuMap {
        SIMULATED
            .with(Cell::get)
            .map(|(m, _)| m)
            .unwrap_or_else(|| DEFAULT_MAP.with(|m| *m))
    }

    /// Simulate, until the guard drops, a machine whose APIC map holds exactly `bindings`
    /// (`(apic_id, logical)`) and whose executing CPU reads APIC ID `apic`.
    pub fn execute_as(bindings: &[(u32, u8)], apic: u32) -> SimulatedCpu {
        let m = fresh_map();
        for &(a, l) in bindings {
            m.bind(a, l).expect("simulated binding");
        }
        let previous = SIMULATED.with(|c| c.replace(Some((m, apic))));
        SimulatedCpu { previous }
    }

    /// `Some(answer)` when a test simulates a hardware read; `None` when none is simulated.
    /// The answer is itself `None` for an unbound APIC ID.
    pub fn simulated_executing_cpu() -> Option<Option<u8>> {
        let (m, apic) = SIMULATED.with(Cell::get)?;
        Some(executing_logical_cpu(m, apic))
    }

    /// The executing CPU as hosted code sees it: `Some` when a test simulates this CPU's APIC ID
    /// (resolved through the same lookup the target uses), `None` otherwise. An unbound simulated
    /// APIC ID is the target's fatal `X86_EXECUTING_CPU_UNMAPPED`.
    pub fn executing_cpu_id() -> Option<crate::kernel::scheduler::CpuId> {
        match simulated_executing_cpu()? {
            Some(logical) => Some(crate::kernel::scheduler::CpuId(logical)),
            None => panic!("X86_EXECUTING_CPU_UNMAPPED (hosted simulation)"),
        }
    }

    #[must_use]
    pub struct SimulatedCpu {
        previous: Option<(&'static ApicCpuMap, u32)>,
    }

    impl Drop for SimulatedCpu {
        fn drop(&mut self) {
            SIMULATED.with(|c| c.set(self.previous));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_ids_and_a_nonzero_boot_apic_resolve_only_through_the_map() {
        let m = ApicCpuMap::new();
        // Firmware numbering: boot CPU APIC 4, secondaries 0, 7, 0x22 — none equals its logical id.
        assert_eq!(m.bind(4, 0), Ok(Bound::New));
        assert_eq!(m.bind(0, 1), Ok(Bound::New));
        assert_eq!(m.bind(7, 2), Ok(Bound::New));
        assert_eq!(m.bind(0x22, 3), Ok(Bound::New));
        assert_eq!(executing_logical_cpu(&m, 4), Some(0));
        assert_eq!(executing_logical_cpu(&m, 0), Some(1));
        assert_eq!(executing_logical_cpu(&m, 7), Some(2));
        assert_eq!(executing_logical_cpu(&m, 0x22), Some(3));
        assert_eq!(m.apic_for(0), Some(4));
        assert_eq!(m.apic_for(3), Some(0x22));
        // An unbound APIC ID — even one numerically equal to a bound LOGICAL id — is no CPU.
        for unbound in [1u32, 2, 3, 5, 0xFF] {
            assert_eq!(executing_logical_cpu(&m, unbound), None, "apic {unbound}");
        }
        assert_eq!(executing_logical_cpu(&m, 4096), None);
        assert_eq!(m.bound_count(), 4);
    }

    #[test]
    fn a_binding_is_a_partial_bijection_and_refusals_change_nothing() {
        let m = ApicCpuMap::new();
        assert_eq!(m.bind(9, 0), Ok(Bound::New));
        assert_eq!(m.bind(9, 0), Ok(Bound::AlreadyExact));
        assert_eq!(
            m.bind(9, 1),
            Err(BindError::ApicAlreadyBound { logical: 0 })
        );
        assert_eq!(
            m.bind(3, 0),
            Err(BindError::LogicalAlreadyBound { apic: 9 })
        );
        assert_eq!(m.bind(256, 1), Err(BindError::ApicOutOfRange));
        assert_eq!(
            m.bind(1, MAX_LOGICAL_CPUS as u8),
            Err(BindError::LogicalOutOfRange)
        );
        assert_eq!(m.bound_count(), 1);
        assert_eq!(m.logical_for(3), None);
        assert_eq!(m.apic_for(1), None);
    }
}
