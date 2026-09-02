// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! A64-DEPTH — the SINGLE owner of the AArch64 terminal-fault (U9-FT4) oracle's startup
//! slot-5 selector.
//!
//! # Why this exists
//!
//! Slot 5 is a shared, **mutually exclusive** oracle selector: init reads one value and runs
//! exactly one scenario. 199E-A64CALL gave the terminal-fault oracle the value `21` as a bare
//! kernel constant matched by a bare userspace literal (`supervisor_control_recv_ep == Some(21)`).
//! Stage 200D-0C1 then assigned `21` to the AArch64 `ExitCurrentTask` oracle through
//! [`crate::exit_current_task_abi`], which documents 20/21/22 as the reserved exit block.
//!
//! Nothing tied the two assignments together, so they collided silently. The kernel's
//! `init_args[5] == 0` guard prevented a double *write* — only one oracle ever wrote slot 5 —
//! but it could not help init tell the two apart, because both read the same value. Init's
//! terminal-fault arm is checked first and diverges (`-> !`), so it always won: the AArch64
//! `ExitCurrentTask` oracle was unreachable from the day it landed, and its profile's failure
//! looked like a hang inside `spawn_thread` rather than a selector collision.
//!
//! This module is the one place the terminal-fault mapping exists, exactly as
//! `exit_current_task_abi` is for the exit oracles. The kernel asks it what to write; init asks
//! it what a slot-5 value means. Neither side writes a literal, so the two cannot drift, and a
//! future third oracle that wants a slot-5 value has two authoritative tables to check instead
//! of a scattering of literals.
//!
//! # Selector assignment
//!
//! | value   | owner                                        |
//! |---------|----------------------------------------------|
//! | `1..=9` | Yield / FutexWait / FutexWake / shared-region / IpcCall-direct / reply-timeout |
//! | `20`    | x86_64 `ExitCurrentTask` (Stage 200D-0B2)    |
//! | `21`    | AArch64 `ExitCurrentTask` (Stage 200D-0C1)   |
//! | `22`    | RISC-V `ExitCurrentTask` (reserved)          |
//! | `23`    | AArch64 terminal-fault / U9-FT4 (this module) |
//!
//! The terminal-fault oracle moved off `21` rather than the exit oracle moving off it: the
//! exit block is a sealed, documented, three-architecture range whose x86_64 member was
//! already shipped, whereas this selector had no owner at all. The oracle itself is unchanged
//! — same deliberate unhandled read at address 0, same witness, same default-off knob.

/// The only scenario this oracle encodes: init takes ONE deliberate unhandled user read at
/// address 0, giving the U9-FT4 terminal-PageFault witness a trigger of its own.
///
/// An enum rather than a bare `bool` so a second scenario extends the mapping here instead of
/// growing a second literal at each end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalFaultScenario {
    /// One deliberate unhandled read at address 0, taken by init before the SpawnV5 chain.
    DeliberateUnhandledRead,
}

/// AArch64's slot-5 terminal-fault selector. Distinct from every value in the reserved
/// `ExitCurrentTask` block (20/21/22) and from the `1..=9` range the earlier oracles use.
pub const AARCH64_TERMINAL_FAULT_SELECTOR: usize = 23;

/// The selector to write into init's startup slot 5 for `scenario`.
#[must_use]
pub const fn terminal_fault_selector(scenario: TerminalFaultScenario) -> usize {
    match scenario {
        TerminalFaultScenario::DeliberateUnhandledRead => AARCH64_TERMINAL_FAULT_SELECTOR,
    }
}

/// The inverse: what a slot-5 value means to the terminal-fault oracle, or `None` when the
/// value belongs to some other oracle.
#[must_use]
pub const fn terminal_fault_scenario_for(slot5: usize) -> Option<TerminalFaultScenario> {
    if slot5 == AARCH64_TERMINAL_FAULT_SELECTOR {
        Some(TerminalFaultScenario::DeliberateUnhandledRead)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encoder and decoder are exact inverses — the property whose absence let 199E-A64CALL and
    /// Stage 200D-0C1 both claim 21.
    #[test]
    fn encode_decode_round_trips() {
        let s = TerminalFaultScenario::DeliberateUnhandledRead;
        assert_eq!(
            terminal_fault_scenario_for(terminal_fault_selector(s)),
            Some(s)
        );
    }

    /// The terminal-fault selector must not collide with ANY reserved `ExitCurrentTask`
    /// selector. This is the guard the two tables lacked.
    #[test]
    fn it_does_not_collide_with_the_exit_block() {
        use crate::exit_current_task_abi as exit;
        for taken in [
            exit::X86_64_EXIT_SELECTOR,
            exit::AARCH64_EXIT_SELECTOR,
            exit::RISCV64_EXIT_SELECTOR,
        ] {
            assert_ne!(
                AARCH64_TERMINAL_FAULT_SELECTOR, taken,
                "slot 5 is mutually exclusive; the terminal-fault oracle may not share a value \
                 with a reserved ExitCurrentTask selector"
            );
            assert!(
                terminal_fault_scenario_for(taken).is_none(),
                "an ExitCurrentTask selector must not decode as a terminal-fault scenario"
            );
        }
    }

    /// And it must stay clear of the earlier `1..=9` oracles.
    #[test]
    fn it_does_not_collide_with_the_early_oracle_range() {
        for taken in 1usize..=9 {
            assert!(terminal_fault_scenario_for(taken).is_none());
        }
        assert!(terminal_fault_scenario_for(0).is_none());
    }
}
