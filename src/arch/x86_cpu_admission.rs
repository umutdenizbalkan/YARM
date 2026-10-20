// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! AP-SCHED4 §B — the x86_64 CPU-admission POLICY: which firmware-listed processor becomes which
//! logical CPU, and which processors a real limit excludes. Pure and arch-neutral, so the hosted
//! suite executes the rules the x86_64 port applies (`arch/x86_64/smp.rs` plans, binds and starts
//! through [`plan`], and nothing else decides).
//!
//! # Identities
//!
//! * **Hardware** — the local APIC ID, as the MADT lists it and as `CPUID.01H:EBX[31:24]` reports
//!   it on the CPU itself. Firmware numbering: not necessarily contiguous, and the boot CPU's is
//!   whatever it is.
//! * **Logical** — the scheduler's `CpuId`. The boot CPU is logical 0; every other admissible
//!   processor takes the next logical id in ascending APIC-ID order, so the numbering is dense and
//!   deterministic whatever the APIC layout.
//!
//! The bridge between them is [`crate::arch::x86_cpu_identity::ApicCpuMap`], bound from this plan.
//!
//! # Limits, and where each comes from
//!
//! * [`PRODUCTION_CPU_CEILING`] — 8 logical CPUs including the boot CPU, the qualified topology
//!   and the ceiling of the other production ports ([`Exclusion::ProductionCeiling`]).
//! * [`DISPATCH_STACKS`] — one dispatch-sized kernel stack (and one set of IST stacks) per
//!   secondary the ceiling admits, reserved statically. A secondary takes every trap from user mode
//!   on its own stack and runs the whole syscall, fault and timer settlement there.
//! * [`XAPIC_ID_LIMIT`] — the kernel drives the local APIC in xAPIC mode: an inter-processor
//!   interrupt names its target with an 8-bit physical destination, and 0xFF is the broadcast
//!   address. A processor whose APIC ID is 0xFF or above could never be sent a reschedule or
//!   shootdown IPI ([`Exclusion::X2apicOnlyId`]).
//! * `max_cpus` — the scheduler's per-CPU tables and 64-bit masks ([`Exclusion::NoLogicalCpu`]).
//! * Firmware state — a processor the MADT marks not enabled is not started
//!   ([`Exclusion::NotEnabled`]); hot-add of an online-capable one is not supported.

/// The total number of logical CPUs production admits, the boot CPU included.
pub const PRODUCTION_CPU_CEILING: usize = 8;

/// Dispatch-sized secondary stacks: one per secondary the ceiling admits.
pub const DISPATCH_STACKS: usize = PRODUCTION_CPU_CEILING - 1;

/// xAPIC physical destinations are 8 bits and 0xFF is broadcast: usable APIC IDs are below this.
pub const XAPIC_ID_LIMIT: u32 = 0xFF;

/// Processor entries the plan covers (the MADT order is not assumed).
pub const MAX_PLANNED_CPUS: usize = 64;

/// Why a listed processor takes no part in dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exclusion {
    /// The firmware lists it but does not mark it enabled (absent, or hot-add capable only).
    NotEnabled,
    /// Its APIC ID cannot be named by an xAPIC IPI.
    X2apicOnlyId,
    /// A second entry for an APIC ID already planned.
    DuplicateApicId,
    /// The production ceiling already holds [`PRODUCTION_CPU_CEILING`] logical CPUs.
    ProductionCeiling,
    /// No free logical CPU id below the scheduler's limit.
    NoLogicalCpu,
    /// More processor entries than the plan covers.
    BeyondPlanTable,
}

impl Exclusion {
    /// The name every marker uses.
    pub const fn marker(self) -> &'static str {
        match self {
            Exclusion::NotEnabled => "not_enabled_by_firmware",
            Exclusion::X2apicOnlyId => "apic_id_beyond_xapic",
            Exclusion::DuplicateApicId => "duplicate_apic_id",
            Exclusion::ProductionCeiling => "beyond_production_cpu_ceiling",
            Exclusion::NoLogicalCpu => "no_free_logical_cpu",
            Exclusion::BeyondPlanTable => "beyond_plan_table",
        }
    }
}

/// One firmware processor entry, as the plan reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Listed {
    pub apic_id: u32,
    pub enabled: bool,
}

/// One planned processor entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Planned {
    pub apic_id: u32,
    /// Its logical CPU, or why it has none.
    pub outcome: Result<u8, Exclusion>,
}

/// The plan for every listed processor: `out[i]` describes `listed[i]`. The boot CPU is logical 0
/// at its own APIC ID whether or not the firmware listed it (`boot_listed` says which); every other
/// enabled entry is planned in ascending APIC-ID order. Entries beyond [`MAX_PLANNED_CPUS`] are
/// `BeyondPlanTable`.
pub fn plan(
    listed: &[Listed],
    boot_apic_id: u32,
    max_cpus: usize,
) -> ([Option<Planned>; MAX_PLANNED_CPUS], bool) {
    let mut out = [None; MAX_PLANNED_CPUS];
    let n = listed.len().min(MAX_PLANNED_CPUS);
    let limit = PRODUCTION_CPU_CEILING.min(max_cpus).min(64);
    let limit_why = if max_cpus < PRODUCTION_CPU_CEILING {
        Exclusion::NoLogicalCpu
    } else {
        Exclusion::ProductionCeiling
    };
    // The boot CPU first, at its first listing.
    let mut boot_listed = false;
    for i in 0..n {
        if listed[i].apic_id == boot_apic_id && !boot_listed {
            boot_listed = true;
            out[i] = Some(Planned {
                apic_id: boot_apic_id,
                outcome: Ok(0),
            });
        }
    }
    // Then every other entry, ascending by APIC ID; ties are duplicates.
    let mut next = 1usize;
    let mut last: Option<u32> = None;
    loop {
        let mut pick: Option<usize> = None;
        for i in 0..n {
            if out[i].is_some() {
                continue;
            }
            let id = listed[i].apic_id;
            if last.is_some_and(|l| id < l) {
                continue;
            }
            if pick.is_none_or(|p| id < listed[p].apic_id) {
                pick = Some(i);
            }
        }
        let Some(i) = pick else { break };
        let entry = listed[i];
        let outcome = if entry.apic_id == boot_apic_id || last == Some(entry.apic_id) {
            Err(Exclusion::DuplicateApicId)
        } else if !entry.enabled {
            Err(Exclusion::NotEnabled)
        } else if entry.apic_id >= XAPIC_ID_LIMIT {
            Err(Exclusion::X2apicOnlyId)
        } else if next >= limit {
            Err(limit_why)
        } else {
            next += 1;
            Ok((next - 1) as u8)
        };
        out[i] = Some(Planned {
            apic_id: entry.apic_id,
            outcome,
        });
        last = Some(entry.apic_id);
    }
    (out, boot_listed)
}

/// The logical CPUs the plan produces (the scheduler's present set), boot CPU included.
pub fn logical_present_bitmap(planned: &[Option<Planned>]) -> u64 {
    let mut bits = 1u64;
    for p in planned.iter().flatten() {
        if let Ok(cpu) = p.outcome {
            bits |= 1u64 << cpu;
        }
    }
    bits
}

/// The dispatch-stack slot logical CPU `cpu` runs on (`cpu - 1`); `None` for the boot CPU (on its
/// own boot stack) and for any id beyond the pool.
pub const fn stack_slot(cpu: usize) -> Option<usize> {
    if cpu >= 1 && cpu <= DISPATCH_STACKS {
        Some(cpu - 1)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(apic_id: u32) -> Listed {
        Listed {
            apic_id,
            enabled: true,
        }
    }

    fn outcomes(
        p: &[Option<Planned>; MAX_PLANNED_CPUS],
        n: usize,
    ) -> alloc::vec::Vec<(u32, Result<u8, Exclusion>)> {
        p[..n]
            .iter()
            .map(|e| {
                let e = e.expect("every listed entry is planned");
                (e.apic_id, e.outcome)
            })
            .collect()
    }

    #[test]
    fn contiguous_ids_from_zero_map_one_to_one() {
        let listed = [l(0), l(1), l(2), l(3)];
        let (p, boot) = plan(&listed, 0, 64);
        assert!(boot);
        assert_eq!(
            outcomes(&p, 4),
            [(0, Ok(0)), (1, Ok(1)), (2, Ok(2)), (3, Ok(3))]
        );
        assert_eq!(logical_present_bitmap(&p), 0b1111);
    }

    #[test]
    fn sparse_ids_and_a_nonzero_boot_cpu_get_dense_ascending_logical_ids() {
        // Firmware order is not ascending, the boot CPU is APIC 6, and the ids have holes.
        let listed = [l(0x10), l(6), l(2), l(0x40), l(9)];
        let (p, boot) = plan(&listed, 6, 64);
        assert!(boot);
        assert_eq!(
            outcomes(&p, 5),
            [
                (0x10, Ok(3)),
                (6, Ok(0)),
                (2, Ok(1)),
                (0x40, Ok(4)),
                (9, Ok(2))
            ]
        );
    }

    #[test]
    fn disabled_x2apic_only_and_duplicate_entries_are_named() {
        let listed = [
            l(0),
            Listed {
                apic_id: 1,
                enabled: false,
            },
            l(0x1_0000),
            l(0xFF),
            l(3),
            l(3),
            l(0),
        ];
        let (p, _) = plan(&listed, 0, 64);
        assert_eq!(
            outcomes(&p, 7),
            [
                (0, Ok(0)),
                (1, Err(Exclusion::NotEnabled)),
                (0x1_0000, Err(Exclusion::X2apicOnlyId)),
                (0xFF, Err(Exclusion::X2apicOnlyId)),
                (3, Ok(1)),
                (3, Err(Exclusion::DuplicateApicId)),
                (0, Err(Exclusion::DuplicateApicId)),
            ]
        );
    }

    #[test]
    fn the_ceiling_counts_logical_cpus_whichever_cpu_boots() {
        let listed: alloc::vec::Vec<Listed> = (0..12u32).map(|i| l(i * 2)).collect();
        for boot in [0u32, 10, 22] {
            let (p, _) = plan(&listed, boot, 64);
            let admitted = p.iter().flatten().filter(|e| e.outcome.is_ok()).count();
            assert_eq!(admitted, PRODUCTION_CPU_CEILING, "boot apic {boot}");
            let refused = p
                .iter()
                .flatten()
                .filter(|e| e.outcome == Err(Exclusion::ProductionCeiling))
                .count();
            assert_eq!(refused, 12 - PRODUCTION_CPU_CEILING);
            // The boot CPU is always logical 0 and every admitted id is below the ceiling.
            assert!(
                p.iter()
                    .flatten()
                    .any(|e| e.apic_id == boot && e.outcome == Ok(0))
            );
            assert!(
                p.iter()
                    .flatten()
                    .filter_map(|e| e.outcome.ok())
                    .all(|c| (c as usize) < PRODUCTION_CPU_CEILING)
            );
        }
    }

    #[test]
    fn an_unlisted_boot_cpu_is_still_logical_zero() {
        let (p, boot) = plan(&[l(1), l(2)], 5, 64);
        assert!(!boot, "reported as unlisted");
        assert_eq!(outcomes(&p, 2), [(1, Ok(1)), (2, Ok(2))]);
        assert_eq!(logical_present_bitmap(&p), 0b111);
    }

    #[test]
    fn small_scheduler_tables_bind_before_the_ceiling() {
        let (p, _) = plan(&[l(0), l(1), l(2), l(3)], 0, 2);
        assert_eq!(
            outcomes(&p, 4),
            [
                (0, Ok(0)),
                (1, Ok(1)),
                (2, Err(Exclusion::NoLogicalCpu)),
                (3, Err(Exclusion::NoLogicalCpu))
            ]
        );
    }

    #[test]
    fn stack_slots_cover_exactly_the_admitted_secondaries() {
        assert_eq!(stack_slot(0), None);
        assert_eq!(stack_slot(1), Some(0));
        assert_eq!(stack_slot(DISPATCH_STACKS), Some(DISPATCH_STACKS - 1));
        assert_eq!(stack_slot(DISPATCH_STACKS + 1), None);
        assert_eq!(DISPATCH_STACKS + 1, PRODUCTION_CPU_CEILING);
    }
}
