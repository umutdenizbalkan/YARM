// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! AP-SCHED2 §2 — the RISC-V hart-admission POLICY: which hart becomes which logical CPU, which
//! logical CPUs may dispatch, and which harts a real limit excludes. Pure and arch-neutral, so the
//! hosted suite executes the rules the RISC-V port applies (`arch/riscv64/boot.rs` claims,
//! registers and releases through these functions, and nothing else decides).
//!
//! # The limits, and where each comes from
//!
//! * [`HANDOFF_SLOTS`] — the secondary handoff, boot-stack and park-stack tables are indexed by
//!   hart id (slot = hart id), and the boot hart starts secondaries through SBI HSM for ids below
//!   it. A hart with a larger id is never started; it is reported excluded
//!   ([`Exclusion::BeyondHandoffTable`]), never silently ignored.
//! * [`PRODUCTION_CPU_CEILING`] — the total number of logical CPUs production admits, the boot
//!   CPU included: 8. It is a qualification bound, not a storage one — it is the largest
//!   topology the scheduling witness covers and qualification ran — and it is applied to the
//!   COUNT, independently of which hart booted. A secondary whose claim would reach logical id 8
//!   is excluded ([`Exclusion::ProductionCeiling`]) and never started, whether the boot hart lies
//!   inside the handoff table or not.
//! * [`DISPATCH_TRAP_STACKS`] — a CPU that dispatches runs the whole syscall, fault and timer
//!   settlement on its trap stack, so it needs the boot hart's trap-stack size (2 MiB; the
//!   `riscv-trap-stack-debt` note on it says why that is a correctness size, not a measured
//!   bound). One such stack per secondary the ceiling admits: `PRODUCTION_CPU_CEILING - 1` = 7,
//!   14 MiB of `.bss`, reserved statically whatever the machine's hart count. (AP-SCHED2 reserved
//!   8, one per handoff slot, because the boot hart may lie outside the table; under the ceiling
//!   the 8th could never be used.) A logical CPU without one would be
//!   [`Exclusion::NoDispatchStack`]: it exists and parks, but is never released to dispatch.
//! * `max_cpus` — the scheduler's per-CPU tables and 64-bit masks (logical identity capacity,
//!   distinct from the ceiling).
//!
//! # Identity
//!
//! The hart id is never the logical CPU. Logical CPU 0 is whichever hart booted — its real hart id
//! is kept, whatever it is — and each other admissible hart takes the lowest free logical id ≥ 1,
//! in ascending hart-id order (the order the boot hart starts them), so which secondaries the
//! ceiling admits is deterministic: the lowest-numbered eligible ones. So a nonzero boot hart and non-contiguous hart ids still produce a dense logical
//! numbering, and [`logical_present_bitmap`] is that numbering — not the hart-id bitmap.

/// Secondary handoff table size (slot = hart id).
pub const HANDOFF_SLOTS: usize = 8;

/// The total number of logical CPUs production admits, the boot CPU included.
pub const PRODUCTION_CPU_CEILING: usize = 8;

/// Dispatch-sized trap stacks: one per secondary the ceiling admits. The boot CPU keeps its own.
pub const DISPATCH_TRAP_STACKS: usize = PRODUCTION_CPU_CEILING - 1;

/// Why a hart, or a logical CPU, takes no part in dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exclusion {
    /// Hart id ≥ [`HANDOFF_SLOTS`]: no handoff slot, never started.
    BeyondHandoffTable,
    /// No free logical CPU id below the scheduler's limit.
    NoLogicalCpu,
    /// The production ceiling already holds [`PRODUCTION_CPU_CEILING`] logical CPUs.
    ProductionCeiling,
    /// A logical CPU with no dispatch-sized trap stack: it parks, wake-only, and is never released.
    NoDispatchStack,
}

impl Exclusion {
    /// The name every marker uses.
    pub const fn marker(self) -> &'static str {
        match self {
            Exclusion::BeyondHandoffTable => "beyond_hsm_handoff_table",
            Exclusion::NoLogicalCpu => "no_free_logical_cpu",
            Exclusion::ProductionCeiling => "beyond_production_cpu_ceiling",
            Exclusion::NoDispatchStack => "no_dispatch_trap_stack",
        }
    }
}

/// May the boot hart start (and claim a logical CPU for) `hart`? The boot hart itself is never a
/// secondary.
pub fn admissible_secondary_hart(hart: usize, boot_hart: usize) -> Result<(), Exclusion> {
    if hart >= HANDOFF_SLOTS || hart >= 64 {
        return Err(Exclusion::BeyondHandoffTable);
    }
    if hart == boot_hart {
        return Err(Exclusion::NoLogicalCpu);
    }
    Ok(())
}

/// The logical CPU the next admissible secondary claims, given the ids already `claimed` (bit 0,
/// the boot hart's, is always claimed): the lowest free id ≥ 1 below `max_cpus`, provided it is
/// inside the production ceiling. Each refusal names the limit that actually bound: the
/// scheduler's tables ([`Exclusion::NoLogicalCpu`]) or the ceiling
/// ([`Exclusion::ProductionCeiling`]).
pub fn next_logical_cpu(claimed: u64, max_cpus: usize) -> Result<usize, Exclusion> {
    let id = (1..max_cpus.min(64))
        .find(|&id| claimed & (1u64 << id) == 0)
        .ok_or(Exclusion::NoLogicalCpu)?;
    if id >= PRODUCTION_CPU_CEILING {
        return Err(Exclusion::ProductionCeiling);
    }
    Ok(id)
}

/// May logical CPU `cpu` dispatch, given `stacks` dispatch-sized trap stacks for secondaries?
/// CPU 0 (the boot hart) always may.
pub const fn dispatch_capable(cpu: usize, stacks: usize) -> bool {
    cpu == 0 || cpu <= stacks
}

/// The dispatch-sized trap stack logical CPU `cpu` uses, or `None` (the boot hart has its own).
pub const fn dispatch_stack_index(cpu: usize, stacks: usize) -> Option<usize> {
    if cpu >= 1 && cpu <= stacks {
        Some(cpu - 1)
    } else {
        None
    }
}

/// The logical CPU each discovered hart becomes, applying exactly the boot hart's claim order.
/// `logical[h]` for every hart id `h < 64`: `Ok(cpu)` or why it has none.
pub fn plan(
    discovered_harts: u64,
    boot_hart: usize,
    max_cpus: usize,
) -> [Result<usize, Option<Exclusion>>; 64] {
    let mut out = [Err(None); 64];
    let mut claimed = 1u64;
    if boot_hart < 64 {
        out[boot_hart] = Ok(0);
    }
    for (hart, slot) in out.iter_mut().enumerate() {
        if hart == boot_hart || discovered_harts & (1u64 << hart) == 0 {
            continue;
        }
        *slot = match admissible_secondary_hart(hart, boot_hart)
            .and_then(|()| next_logical_cpu(claimed, max_cpus))
        {
            Ok(cpu) => {
                claimed |= 1u64 << cpu;
                Ok(cpu)
            }
            Err(e) => Err(Some(e)),
        };
    }
    out
}

/// The logical CPUs the discovered harts produce (the scheduler's present set) — never the
/// hart-id bitmap itself.
pub fn logical_present_bitmap(discovered_harts: u64, boot_hart: usize, max_cpus: usize) -> u64 {
    plan(discovered_harts, boot_hart, max_cpus)
        .iter()
        .filter_map(|r| r.ok())
        .fold(0u64, |acc, cpu| acc | (1u64 << cpu))
}

/// The discovered harts a real limit excludes, as a hart-id bitmap.
pub fn excluded_harts(discovered_harts: u64, boot_hart: usize, max_cpus: usize) -> u64 {
    plan(discovered_harts, boot_hart, max_cpus)
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r, Err(Some(_))))
        .fold(0u64, |acc, (hart, _)| acc | (1u64 << hart))
}

/// The secondaries the boot hart releases at the end of its boot borrow:
/// * production — every trap-ready logical CPU that may dispatch;
/// * diagnostic (`yarm.ap_user_dispatch=1`) — every trap-ready secondary, as before; the witnesses
///   own what they admit.
///
/// CPU 0 is never in the set.
pub fn release_set(trap_ready: u64, dispatch_capable_cpus: u64, diagnostic: bool) -> u64 {
    let secondaries = trap_ready & !1;
    if diagnostic {
        secondaries
    } else {
        secondaries & dispatch_capable_cpus
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_boot_hart_with_contiguous_harts_numbers_them_densely() {
        let p = plan(0b1111, 0, 64);
        assert_eq!(&p[..4], &[Ok(0), Ok(1), Ok(2), Ok(3)]);
        assert_eq!(logical_present_bitmap(0b1111, 0, 64), 0b1111);
        assert_eq!(excluded_harts(0b1111, 0, 64), 0);
    }

    #[test]
    fn a_nonzero_boot_hart_is_logical_cpu_zero() {
        let p = plan(0b1111, 2, 64);
        assert_eq!(p[2], Ok(0));
        assert_eq!((p[0], p[1], p[3]), (Ok(1), Ok(2), Ok(3)));
        assert_eq!(logical_present_bitmap(0b1111, 2, 64), 0b1111);
    }

    #[test]
    fn non_contiguous_hart_ids_still_give_dense_logical_ids() {
        // Harts 0, 2, 5 and 7: four logical CPUs, 0..=3, never the hart bitmap 0b1010_0101.
        let harts = 0b1010_0101;
        let p = plan(harts, 5, 64);
        assert_eq!(p[5], Ok(0));
        assert_eq!((p[0], p[2], p[7]), (Ok(1), Ok(2), Ok(3)));
        assert_eq!(logical_present_bitmap(harts, 5, 64), 0b1111);
        assert_ne!(logical_present_bitmap(harts, 5, 64), harts);
    }

    #[test]
    fn harts_beyond_the_handoff_table_are_excluded_by_name() {
        // A 10-hart board: ids 8 and 9 have no slot.
        let harts = (1u64 << 10) - 1;
        let p = plan(harts, 0, 64);
        assert_eq!(p[7], Ok(7));
        assert_eq!(p[8], Err(Some(Exclusion::BeyondHandoffTable)));
        assert_eq!(p[9], Err(Some(Exclusion::BeyondHandoffTable)));
        assert_eq!(excluded_harts(harts, 0, 64), 0b11_0000_0000);
        assert_eq!(logical_present_bitmap(harts, 0, 64), 0xff);
        assert_eq!(
            Exclusion::BeyondHandoffTable.marker(),
            "beyond_hsm_handoff_table"
        );
        assert_eq!(
            admissible_secondary_hart(8, 0),
            Err(Exclusion::BeyondHandoffTable)
        );
    }

    #[test]
    fn the_scheduler_limit_bounds_logical_ids() {
        // A scheduler built for 3 CPUs: the fourth hart has no logical id.
        let p = plan(0b1111, 0, 3);
        assert_eq!((p[1], p[2]), (Ok(1), Ok(2)));
        assert_eq!(p[3], Err(Some(Exclusion::NoLogicalCpu)));
        assert_eq!(next_logical_cpu(0b111, 3), Err(Exclusion::NoLogicalCpu));
        assert_eq!(next_logical_cpu(0b1, 3), Ok(1));
        assert_eq!(next_logical_cpu(0b101, 64), Ok(1));
    }

    #[test]
    fn every_admissible_secondary_has_a_dispatch_stack_and_no_more() {
        assert_eq!(DISPATCH_TRAP_STACKS, PRODUCTION_CPU_CEILING - 1);
        assert!(dispatch_capable(0, DISPATCH_TRAP_STACKS));
        for cpu in 1..=DISPATCH_TRAP_STACKS {
            assert!(dispatch_capable(cpu, DISPATCH_TRAP_STACKS));
            assert_eq!(
                dispatch_stack_index(cpu, DISPATCH_TRAP_STACKS),
                Some(cpu - 1)
            );
        }
        assert!(!dispatch_capable(
            DISPATCH_TRAP_STACKS + 1,
            DISPATCH_TRAP_STACKS
        ));
        assert_eq!(dispatch_stack_index(0, DISPATCH_TRAP_STACKS), None);
        // A smaller pool would leave the excess logical CPUs parked, by name.
        assert!(!dispatch_capable(3, 2));
        assert_eq!(dispatch_stack_index(3, 2), None);
    }

    #[test]
    fn a_boot_hart_beyond_the_table_still_gives_eight_cpus() {
        // -smp 10 with boot hart 9: harts 0..=6 take logical CPUs 1..=7; hart 7 is in the table
        // but the ceiling is full; hart 8 has no slot. Eight CPUs, as with any other boot hart.
        let p = plan(0x3ff, 9, 64);
        assert_eq!(p[9], Ok(0));
        for hart in 0..7 {
            assert_eq!(p[hart], Ok(hart + 1));
            assert!(dispatch_capable(hart + 1, DISPATCH_TRAP_STACKS));
        }
        assert_eq!(p[7], Err(Some(Exclusion::ProductionCeiling)));
        assert_eq!(p[8], Err(Some(Exclusion::BeyondHandoffTable)));
        assert_eq!(logical_present_bitmap(0x3ff, 9, 64), 0xff);
        assert_eq!(excluded_harts(0x3ff, 9, 64), (1 << 7) | (1 << 8));
        // -smp 9 with boot hart 8: the whole table is secondaries; the ceiling stops at hart 6.
        let p = plan(0x1ff, 8, 64);
        assert_eq!(p[8], Ok(0));
        assert_eq!(p[7], Err(Some(Exclusion::ProductionCeiling)));
        assert_eq!(logical_present_bitmap(0x1ff, 8, 64), 0xff);
    }

    #[test]
    fn the_ceiling_is_a_count_whatever_hart_boots() {
        // Every boot hart of a 10-hart board admits exactly 8 CPUs, the boot hart keeping its
        // real id as logical 0, and the secondaries chosen in ascending hart order.
        for boot in 0..10 {
            let p = plan(0x3ff, boot, 64);
            assert_eq!(p[boot], Ok(0), "boot hart {boot}");
            assert_eq!(
                logical_present_bitmap(0x3ff, boot, 64),
                0xff,
                "boot hart {boot}"
            );
            let admitted: alloc::vec::Vec<usize> =
                (0..10).filter(|&h| h != boot && p[h].is_ok()).collect();
            let mut eligible: alloc::vec::Vec<usize> =
                (0..HANDOFF_SLOTS).filter(|&h| h != boot).collect();
            eligible.truncate(PRODUCTION_CPU_CEILING - 1);
            assert_eq!(admitted, eligible, "boot hart {boot}");
            for h in 0..10 {
                if let Err(Some(why)) = p[h] {
                    let named = if h >= HANDOFF_SLOTS {
                        Exclusion::BeyondHandoffTable
                    } else {
                        Exclusion::ProductionCeiling
                    };
                    assert_eq!(why, named, "boot hart {boot}, hart {h}");
                }
            }
        }
        // Sparse ids: harts 1, 3, 4, 5, 6, 7 and 12, 40 present, boot hart 40 (outside the
        // table). Six table harts are secondaries: logical 1..=6, seven CPUs, nobody excluded by
        // the ceiling; 12 is beyond the table.
        let harts = (1u64 << 1) | (0b1111_1000) | (1 << 12) | (1 << 40);
        let p = plan(harts, 40, 64);
        assert_eq!(p[40], Ok(0));
        assert_eq!(
            (p[1], p[3], p[4], p[5], p[6], p[7]),
            (Ok(1), Ok(2), Ok(3), Ok(4), Ok(5), Ok(6))
        );
        assert_eq!(p[12], Err(Some(Exclusion::BeyondHandoffTable)));
        assert_eq!(logical_present_bitmap(harts, 40, 64), 0x7f);
        // Absent harts claim nothing: only the present ones are numbered.
        assert_eq!(plan(0b1000_0001, 0, 64)[7], Ok(1));
        assert_eq!(plan(0b1000_0001, 0, 64)[3], Err(None));
        // A scheduler smaller than the ceiling binds first, and says so.
        assert_eq!(plan(0xff, 0, 4)[4], Err(Some(Exclusion::NoLogicalCpu)));
        assert_eq!(
            Exclusion::ProductionCeiling.marker(),
            "beyond_production_cpu_ceiling"
        );
    }

    #[test]
    fn production_releases_every_trap_ready_dispatch_capable_secondary() {
        let capable = 0b1111_1111;
        assert_eq!(release_set(0b1111, capable, false), 0b1110);
        assert_eq!(release_set(0b1, capable, false), 0);
        // A trap-ready CPU without a dispatch stack is not released...
        assert_eq!(release_set(0b1111, 0b0111, false), 0b0110);
        // ...and a dispatch-capable CPU that is not trap-ready is not either.
        assert_eq!(release_set(0b0101, capable, false), 0b0100);
        // The knob keeps the diagnostic set.
        assert_eq!(release_set(0b1111, 0b0011, true), 0b1110);
    }
}
