// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 200D-0C1 — the SINGLE, ARCHITECTURE-LOCAL encoder/decoder for the
//! `ExitCurrentTask` (NR 16) live-oracle startup slot-5 selector.
//!
//! # Why this exists
//!
//! The x86_64 cell (Stage 200D-0B1/0B2) hard-wired its selector twice: the kernel
//! declared `X86_EXIT_CURRENT_TASK_ORACLE_SELECTOR = 20` and the init server matched a
//! bare literal `Some(20)`. Nothing tied the two together — the identical shape that let
//! the reply-timeout selectors drift apart in Stage 200C2C2C-R2B, where userspace decoded
//! one architecture's selector with another's meaning and made a whole scenario
//! unreachable.
//!
//! This module is the one place the mapping exists. The kernel asks it what to write into
//! init's startup slot 5; init asks it what a slot-5 value means. Neither side computes a
//! base plus an offset, so the two directions cannot disagree.
//!
//! # Selector assignment
//!
//! Slot 5 is a shared, mutually-exclusive oracle selector. Values `1..=11` are taken by
//! the Yield / FutexWait / FutexWake / shared-region / IpcCall-direct / reply-timeout
//! oracles. Each architecture owns exactly ONE distinct value here — unlike the
//! reply-timeout pairs, these do NOT overlap, because there is a single scenario per
//! architecture:
//!
//! | arch    | `ExitCurrentTask` selector | stage      |
//! |---------|----------------------------|------------|
//! | x86_64  | 20                         | 200D-0B2   |
//! | AArch64 | 21                         | 200D-0C1   |
//! | RISC-V  | 22                         | (reserved) |
//!
//! x86_64's `20` is stated here to match the value Stage 200D-0B2 already sealed; that
//! cell keeps its own constant, so this module does not change it.

/// The only scenario this oracle encodes: one disposable task calls NR 16 on itself.
///
/// It is an enum rather than a bare `bool` so a future second scenario (for example a
/// declined `WouldBlock` preflight) extends the mapping in this one module instead of
/// growing a second literal at each end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCurrentTaskScenario {
    /// A disposable, non-essential task invokes `ExitCurrentTask` on itself and must
    /// never return to EL0/ring 3.
    SelfExit,
}

/// x86_64's slot-5 `ExitCurrentTask` selector (Stage 200D-0B2, already sealed).
pub const X86_64_EXIT_SELECTOR: usize = 20;
/// AArch64's slot-5 `ExitCurrentTask` selector (Stage 200D-0C1).
pub const AARCH64_EXIT_SELECTOR: usize = 21;
/// RISC-V's slot-5 `ExitCurrentTask` selector. Reserved so the value cannot be handed to
/// an unrelated oracle before the RISC-V cell exists; nothing writes it today.
pub const RISCV64_EXIT_SELECTOR: usize = 22;

/// The selector compiled into THIS build. Chosen by `cfg`, never by a runtime value, so a
/// foreign architecture's selector is not present in the binary at all.
#[cfg(target_arch = "x86_64")]
pub const CURRENT_ARCH_EXIT_SELECTOR: usize = X86_64_EXIT_SELECTOR;
#[cfg(target_arch = "aarch64")]
pub const CURRENT_ARCH_EXIT_SELECTOR: usize = AARCH64_EXIT_SELECTOR;
#[cfg(target_arch = "riscv64")]
pub const CURRENT_ARCH_EXIT_SELECTOR: usize = RISCV64_EXIT_SELECTOR;
/// Ports with no cell get a selector no slot-5 value can equal, so the oracle is inert
/// rather than accidentally aliased onto another architecture's selector.
#[cfg(not(any(
    target_arch = "aarch64",
    target_arch = "riscv64",
    target_arch = "x86_64"
)))]
pub const CURRENT_ARCH_EXIT_SELECTOR: usize = usize::MAX;

/// Decode `selector` against an EXPLICIT architecture selector.
///
/// Split out from the current-arch entry point only so every architecture's mapping —
/// including the ones this build is not — stays provable in one hosted test run. Nothing
/// on the live path calls it with a selector other than its own.
#[must_use]
pub const fn exit_current_task_scenario_for_selector(
    arch_selector: usize,
    selector: usize,
) -> Option<ExitCurrentTaskScenario> {
    if arch_selector == usize::MAX {
        return None;
    }
    if selector == arch_selector {
        Some(ExitCurrentTaskScenario::SelfExit)
    } else {
        None
    }
}

/// Decode `selector` for the architecture this build targets.
///
/// A selector belonging to a DIFFERENT architecture's cell yields `None`, so an
/// AArch64 image can never be driven by x86_64's `20` and vice versa.
#[must_use]
pub const fn exit_current_task_scenario_for_current_arch(
    selector: usize,
) -> Option<ExitCurrentTaskScenario> {
    exit_current_task_scenario_for_selector(CURRENT_ARCH_EXIT_SELECTOR, selector)
}

/// The slot-5 selector this build's kernel must publish for `scenario` — the exact
/// inverse of `exit_current_task_scenario_for_current_arch`.
#[must_use]
pub const fn exit_current_task_selector_for_current_arch(
    scenario: ExitCurrentTaskScenario,
) -> usize {
    match scenario {
        ExitCurrentTaskScenario::SelfExit => CURRENT_ARCH_EXIT_SELECTOR,
    }
}
