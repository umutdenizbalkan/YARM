// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! AP-SCHED3 §B — the AArch64 CPU-admission POLICY: which hardware CPU (MPIDR affinity) becomes
//! which logical CPU, which logical CPUs may dispatch, and which CPUs a real limit excludes.
//! Pure and arch-neutral, so the hosted suite executes the rules the AArch64 port applies
//! (`arch/aarch64/boot.rs` plans, starts and validates through these functions, and nothing else
//! decides).
//!
//! # Three identities, kept distinct
//!
//! * **Hardware identity** — the CPU's MPIDR_EL1 affinity (Aff3:Aff2:Aff1:Aff0), as the device
//!   tree's `cpu` node `reg` names it and as PSCI `CPU_ON` targets it. It is compared WHOLE: a
//!   CPU is never identified by `Aff0` alone, which would alias `0x000` and `0x100`.
//! * **Logical identity** — the scheduler's `CpuId`. Logical CPU 0 is whichever CPU booted; every
//!   other admissible CPU takes the next logical id in ascending affinity order, so the numbering
//!   is dense and deterministic whatever the affinity layout. Each CPU holds its own logical id in
//!   `TPIDR_EL1` (written once, at its entry, from this plan) and the trap path reads it there.
//! * **GIC CPU-interface identity** — the bit GICv2 routes an SGI to. Never assumed from either of
//!   the others: each CPU reads its own banked `GICD_ITARGETSR0` and publishes it
//!   (`arch::gicv2_sgi::TargetTable`).
//!
//! # The limits, and where each comes from
//!
//! * [`GICV2_MAX_INTERFACES`] — GICv2 has at most eight CPU interfaces (`GICD_TYPER.CPUNumber`
//!   reports how many this distributor implements), and an SGI can only target an interface. A
//!   CPU beyond the implemented interfaces could never be woken: [`Exclusion::BeyondGicInterfaces`].
//!   The scheduler's `MAX_CPUS` (64) is table capacity, not proof that this controller can wake
//!   64 CPUs.
//! * [`PRODUCTION_CPU_CEILING`] — the total number of logical CPUs production admits, the boot CPU
//!   included: 8, the qualified topology ([`Exclusion::ProductionCeiling`]).
//! * [`DISPATCH_STACKS`] × [`DISPATCH_STACK_BYTES`] — a dispatching CPU takes every exception on
//!   its own `SP_EL1` stack and runs the whole syscall, fault and timer settlement there, so it
//!   needs a correctness size: 2 MiB, the size RISC-V measured its deepest dispatch against (between
//!   256 KiB and 1 MiB; `riscv-trap-stack-debt`). It is NOT a measured AArch64 bound. One per
//!   secondary the ceiling admits: 7, 14 MiB of `.bss`, reserved statically whatever the machine's
//!   CPU count. A build whose secondary stacks are smaller (the RPi5 high-half image) admits no
//!   secondary: [`Exclusion::NoDispatchStack`].
//! * `max_cpus` — the scheduler's per-CPU tables and 64-bit masks ([`Exclusion::NoLogicalCpu`]).

/// GICv2's architectural CPU-interface limit.
pub const GICV2_MAX_INTERFACES: usize = 8;

/// The total number of logical CPUs production admits, the boot CPU included.
pub const PRODUCTION_CPU_CEILING: usize = 8;

/// Dispatch-sized secondary stacks: one per secondary the ceiling admits.
pub const DISPATCH_STACKS: usize = PRODUCTION_CPU_CEILING - 1;

/// The correctness size of one dispatching secondary's stack.
pub const DISPATCH_STACK_BYTES: usize = 2 * 1024 * 1024;

/// The MPIDR_EL1 affinity fields: Aff3 [39:32], Aff2 [23:16], Aff1 [15:8], Aff0 [7:0].
pub const MPIDR_AFFINITY_MASK: u64 = 0xff_00ff_ffff;

/// The CPU-node table the plan is computed over (device-tree order is not assumed).
pub const MAX_PLANNED_CPUS: usize = 64;

/// Why a CPU takes no part in dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exclusion {
    /// More CPUs than the distributor has CPU interfaces: no SGI could reach it.
    BeyondGicInterfaces,
    /// The production ceiling already holds [`PRODUCTION_CPU_CEILING`] logical CPUs.
    ProductionCeiling,
    /// No free logical CPU id below the scheduler's limit.
    NoLogicalCpu,
    /// A second CPU node with an affinity already planned.
    DuplicateAffinity,
    /// The build's secondary stacks are smaller than [`DISPATCH_STACK_BYTES`].
    NoDispatchStack,
    /// No GICv2 distributor was found: nothing could send any CPU an SGI.
    NoGicDistributor,
}

impl Exclusion {
    /// The name every marker uses.
    pub const fn marker(self) -> &'static str {
        match self {
            Exclusion::BeyondGicInterfaces => "beyond_gicv2_interfaces",
            Exclusion::ProductionCeiling => "beyond_production_cpu_ceiling",
            Exclusion::NoLogicalCpu => "no_free_logical_cpu",
            Exclusion::DuplicateAffinity => "duplicate_mpidr_affinity",
            Exclusion::NoDispatchStack => "no_dispatch_stack",
            Exclusion::NoGicDistributor => "no_gicv2_distributor",
        }
    }
}

/// The number of CPU interfaces a GICv2 distributor implements: `GICD_TYPER.CPUNumber + 1`,
/// never more than [`GICV2_MAX_INTERFACES`].
pub const fn gic_interfaces_from_typer(typer: u32) -> usize {
    (((typer >> 5) & 0x7) + 1) as usize
}

/// The logical ids a planned set may use: below the ceiling, the implemented interfaces and the
/// scheduler's tables, whichever binds first — and which one bound, by name.
fn bound(gic_interfaces: usize, max_cpus: usize) -> (usize, Exclusion) {
    if gic_interfaces == 0 {
        return (1, Exclusion::NoGicDistributor);
    }
    let gic = gic_interfaces.min(GICV2_MAX_INTERFACES);
    let max = max_cpus.min(64);
    let mut limit = (PRODUCTION_CPU_CEILING, Exclusion::ProductionCeiling);
    if gic < limit.0 {
        limit = (gic, Exclusion::BeyondGicInterfaces);
    }
    if max < limit.0 {
        limit = (max, Exclusion::NoLogicalCpu);
    }
    limit
}

/// One planned CPU node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Planned {
    /// The node's MPIDR affinity.
    pub affinity: u64,
    /// Its logical CPU, or why it has none.
    pub outcome: Result<usize, Exclusion>,
}

/// The plan for every CPU node: `out[i]` describes `cpus[i]` (same order as given). The boot CPU
/// is logical 0 whether or not its node is listed; every other node is planned in ascending
/// affinity order. Nodes beyond [`MAX_PLANNED_CPUS`] are not planned (the caller names them).
pub fn plan(
    cpus: &[u64],
    boot_affinity: u64,
    gic_interfaces: usize,
    max_cpus: usize,
) -> [Option<Planned>; MAX_PLANNED_CPUS] {
    let mut out = [None; MAX_PLANNED_CPUS];
    let n = cpus.len().min(MAX_PLANNED_CPUS);
    let boot = boot_affinity & MPIDR_AFFINITY_MASK;
    let (limit, why) = bound(gic_interfaces, max_cpus);
    // The boot CPU first, wherever it is listed (and only its first listing).
    let mut boot_seen = false;
    for i in 0..n {
        let aff = cpus[i] & MPIDR_AFFINITY_MASK;
        if aff == boot && !boot_seen {
            boot_seen = true;
            out[i] = Some(Planned {
                affinity: aff,
                outcome: Ok(0),
            });
        }
    }
    // Then every other node, ascending by affinity; ties are duplicates.
    let mut next = 1usize;
    let mut last: Option<u64> = None;
    loop {
        // The smallest affinity strictly above `last` among the unplanned nodes.
        let mut pick: Option<usize> = None;
        for i in 0..n {
            if out[i].is_some() {
                continue;
            }
            let aff = cpus[i] & MPIDR_AFFINITY_MASK;
            if last.is_some_and(|l| aff < l) {
                continue;
            }
            if pick.is_none_or(|p| aff < cpus[p] & MPIDR_AFFINITY_MASK) {
                pick = Some(i);
            }
        }
        let Some(i) = pick else { break };
        let aff = cpus[i] & MPIDR_AFFINITY_MASK;
        let duplicate = aff == boot || last == Some(aff);
        out[i] = Some(Planned {
            affinity: aff,
            outcome: if duplicate {
                Err(Exclusion::DuplicateAffinity)
            } else if next >= limit {
                Err(why)
            } else {
                next += 1;
                Ok(next - 1)
            },
        });
        last = Some(aff);
    }
    out
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

/// May logical CPU `cpu` dispatch, given secondary stacks of `stack_bytes` each? CPU 0 (the boot
/// CPU, on its own boot stack) always may.
pub const fn dispatch_capable(cpu: usize, stack_bytes: usize) -> bool {
    cpu == 0 || (cpu <= DISPATCH_STACKS && stack_bytes >= DISPATCH_STACK_BYTES)
}

/// The secondary stack slot logical CPU `cpu` runs on (`cpu - 1`), or `None` for the boot CPU and
/// for any id beyond the pool.
pub const fn stack_slot(cpu: usize, slots: usize) -> Option<usize> {
    if cpu >= 1 && cpu <= slots {
        Some(cpu - 1)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logical(p: &[Option<Planned>; MAX_PLANNED_CPUS], i: usize) -> Result<usize, Exclusion> {
        p[i].expect("planned").outcome
    }

    #[test]
    fn qemu_virt_numbers_contiguous_cpus_densely() {
        let cpus = [0, 1, 2, 3];
        let p = plan(&cpus, 0, 4, 64);
        assert_eq!(
            (0..4)
                .map(|i| logical(&p, i))
                .collect::<alloc::vec::Vec<_>>(),
            [Ok(0), Ok(1), Ok(2), Ok(3)]
        );
        assert_eq!(logical_present_bitmap(&p), 0b1111);
    }

    #[test]
    fn identity_is_the_whole_affinity_never_aff0() {
        // Two clusters: 0x000, 0x001, 0x100, 0x101. Aff0 alone would alias 0x100 onto CPU 0.
        let cpus = [0x101, 0x100, 0x001, 0x000];
        let p = plan(&cpus, 0x000, 8, 64);
        assert_eq!(logical(&p, 3), Ok(0));
        assert_eq!(logical(&p, 2), Ok(1));
        assert_eq!(logical(&p, 1), Ok(2));
        assert_eq!(logical(&p, 0), Ok(3));
        assert_eq!(logical_present_bitmap(&p), 0b1111);
        // Aff3 is part of the identity too; bits outside the affinity fields are not.
        let p = plan(&[0x01_0000_0000, 0x8000_0000], 0x8000_0000, 8, 64);
        assert_eq!(logical(&p, 1), Ok(0), "MPIDR bit 31 (RES1) is not identity");
        assert_eq!(logical(&p, 0), Ok(1));
    }

    #[test]
    fn a_nonzero_boot_cpu_is_logical_zero() {
        let p = plan(&[0, 1, 2, 3], 2, 8, 64);
        assert_eq!(
            (0..4)
                .map(|i| logical(&p, i))
                .collect::<alloc::vec::Vec<_>>(),
            [Ok(1), Ok(2), Ok(0), Ok(3)]
        );
    }

    #[test]
    fn the_gic_interfaces_bound_admission_by_name() {
        // A distributor with 2 interfaces (`GICD_TYPER = 0x28`, as QEMU's -smp 2 reports).
        assert_eq!(gic_interfaces_from_typer(0x28), 2);
        assert_eq!(gic_interfaces_from_typer(0xe0), 8);
        let p = plan(&[0, 1, 2], 0, gic_interfaces_from_typer(0x28), 64);
        assert_eq!(logical(&p, 1), Ok(1));
        assert_eq!(logical(&p, 2), Err(Exclusion::BeyondGicInterfaces));
        assert_eq!(
            Exclusion::BeyondGicInterfaces.marker(),
            "beyond_gicv2_interfaces"
        );
        // No distributor at all: every secondary is named, the boot CPU stays.
        let p = plan(&[0, 1], 0, 0, 64);
        assert_eq!(logical(&p, 0), Ok(0));
        assert_eq!(logical(&p, 1), Err(Exclusion::NoGicDistributor));
        // A claimed interface count above GICv2's eight is clamped to eight.
        let cpus: alloc::vec::Vec<u64> = (0..10).collect();
        let p = plan(&cpus, 0, 99, 64);
        assert_eq!(logical(&p, 7), Ok(7));
        assert_eq!(logical(&p, 8), Err(Exclusion::ProductionCeiling));
    }

    #[test]
    fn the_ceiling_is_a_count_whatever_cpu_boots() {
        // Sixteen CPUs, eight interfaces... the ceiling and GICv2 agree at eight; with a larger
        // interface count than the ceiling the ceiling names itself.
        let cpus: alloc::vec::Vec<u64> = (0..12).collect();
        for boot in [0u64, 5, 11] {
            let p = plan(&cpus, boot, 8, 64);
            let admitted: alloc::vec::Vec<u64> = (0..12)
                .filter(|&i| logical(&p, i).is_ok())
                .map(|i| cpus[i])
                .collect();
            assert_eq!(admitted.len(), 8, "boot {boot}");
            assert!(admitted.contains(&boot), "the boot CPU is always admitted");
            assert_eq!(logical(&p, boot as usize), Ok(0));
            // The secondaries admitted are the lowest affinities, ascending.
            let mut want: alloc::vec::Vec<u64> = (0..12).filter(|&a| a != boot).collect();
            want.truncate(7);
            let mut got: alloc::vec::Vec<u64> =
                admitted.iter().copied().filter(|&a| a != boot).collect();
            got.sort();
            assert_eq!(got, want, "boot {boot}");
            assert_eq!(logical_present_bitmap(&p), 0xff);
        }
        // Sparse affinities and a scheduler smaller than the ceiling: the scheduler names itself.
        let p = plan(&[0, 4, 9, 33], 0, 8, 3);
        assert_eq!(
            (logical(&p, 1), logical(&p, 2), logical(&p, 3)),
            (Ok(1), Ok(2), Err(Exclusion::NoLogicalCpu))
        );
    }

    #[test]
    fn duplicates_and_an_unlisted_boot_cpu() {
        let p = plan(&[0, 1, 1, 0], 0, 8, 64);
        assert_eq!(logical(&p, 0), Ok(0));
        assert_eq!(logical(&p, 1), Ok(1));
        assert_eq!(logical(&p, 2), Err(Exclusion::DuplicateAffinity));
        assert_eq!(logical(&p, 3), Err(Exclusion::DuplicateAffinity));
        // A boot CPU missing from the device tree is still logical 0; the listed ones follow.
        let p = plan(&[1, 2], 0, 8, 64);
        assert_eq!((logical(&p, 0), logical(&p, 1)), (Ok(1), Ok(2)));
        assert_eq!(logical_present_bitmap(&p), 0b111);
    }

    #[test]
    fn dispatch_needs_a_correctness_sized_stack() {
        assert_eq!(DISPATCH_STACKS, PRODUCTION_CPU_CEILING - 1);
        assert!(dispatch_capable(0, 0));
        for cpu in 1..=DISPATCH_STACKS {
            assert!(dispatch_capable(cpu, DISPATCH_STACK_BYTES));
            assert!(
                !dispatch_capable(cpu, 16 * 1024),
                "a 16 KiB stack admits nothing"
            );
            assert_eq!(stack_slot(cpu, DISPATCH_STACKS), Some(cpu - 1));
        }
        assert!(!dispatch_capable(DISPATCH_STACKS + 1, DISPATCH_STACK_BYTES));
        assert_eq!(stack_slot(0, DISPATCH_STACKS), None);
        assert_eq!(stack_slot(DISPATCH_STACKS + 1, DISPATCH_STACKS), None);
        assert_eq!(Exclusion::NoDispatchStack.marker(), "no_dispatch_stack");
    }
}
