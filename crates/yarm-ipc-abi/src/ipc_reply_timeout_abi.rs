// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 200C2C2C-R2C — the SINGLE, ARCHITECTURE-LOCAL decoder for the direct-reply
//! timeout oracle's startup slot-5 selector.
//!
//! # Why this exists
//!
//! Each architecture owns a distinct free PAIR of slot-5 selector values,
//! `(base, base + 1)` = `(TimeoutWins, ReplyWins)`:
//!
//! | arch    | TimeoutWins | ReplyWins |
//! |---------|-------------|-----------|
//! | AArch64 | 8           | 9         |
//! | RISC-V  | 9           | 10        |
//! | x86_64  | 10          | 11        |
//!
//! **The pairs overlap.** AArch64's `ReplyWins` is numerically RISC-V's `TimeoutWins`,
//! and RISC-V's `ReplyWins` is numerically x86_64's `TimeoutWins`. A shared numeric
//! match such as `matches!(selector, 8 | 9 | 10)` therefore has no single meaning: it
//! silently decoded AArch64's and RISC-V's `ReplyWins` as `TimeoutWins`, which made
//! the reply-wins scenario unreachable on both non-x86 ports (Stage 200C2C2C-R2B).
//!
//! The fix is structural, not a wider match: interpretation is compiled for the
//! CURRENT architecture. `ipc_reply_timeout_scenario_for_current_arch` consults only
//! this build's own base, so a selector that belongs to another architecture's pair —
//! and is not also a value of this one's — cannot activate any scenario here.
//!
//! # Why the module is shared
//!
//! The kernel writes the selector into init's startup slot 5; userspace reads it back
//! and picks the scenario. Before this stage those two sides carried SEPARATE copies of
//! the mapping and drifted apart — exactly the defect above. Both sides now depend on
//! this one module, so the encode and decode directions cannot disagree.

/// The two direct-reply timeout scenarios. There are no others: server-death caller
/// wake, caller-exit cancellation, endpoint destruction, multi-pair concurrency and
/// queued `IpcCall` are all explicitly out of scope for this oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcReplyTimeoutScenario {
    /// The deadline claims the terminal; the caller resumes with the canonical
    /// `TimedOut`, and the server's later reply is rejected.
    TimeoutWins,
    /// The server's reply claims the terminal first; the caller resumes with the
    /// payload, and the stale deadline is later scanned harmlessly.
    ReplyWins,
}

/// AArch64's slot-5 pair base (`8` = TimeoutWins, `9` = ReplyWins).
pub const AARCH64_SELECTOR_BASE: usize = 8;
/// RISC-V's slot-5 pair base (`9` = TimeoutWins, `10` = ReplyWins).
pub const RISCV64_SELECTOR_BASE: usize = 9;
/// x86_64's slot-5 pair base (`10` = TimeoutWins, `11` = ReplyWins).
pub const X86_64_SELECTOR_BASE: usize = 10;

/// The base compiled into THIS build. Selected by `cfg`, never by a runtime value, so a
/// foreign architecture's base is not even present in the binary.
#[cfg(target_arch = "aarch64")]
pub const CURRENT_ARCH_SELECTOR_BASE: usize = AARCH64_SELECTOR_BASE;
#[cfg(target_arch = "riscv64")]
pub const CURRENT_ARCH_SELECTOR_BASE: usize = RISCV64_SELECTOR_BASE;
#[cfg(target_arch = "x86_64")]
pub const CURRENT_ARCH_SELECTOR_BASE: usize = X86_64_SELECTOR_BASE;
/// Ports without an oracle cell get a base that no selector can match, so the oracle is
/// inert rather than accidentally aliased onto another architecture's pair.
#[cfg(not(any(
    target_arch = "aarch64",
    target_arch = "riscv64",
    target_arch = "x86_64"
)))]
pub const CURRENT_ARCH_SELECTOR_BASE: usize = usize::MAX;

/// Decode `selector` against an EXPLICIT architecture base.
///
/// Split out from the current-arch entry point purely so every architecture's mapping —
/// including the ones this build is not — stays provable in one hosted test run. Nothing
/// in the live path calls it with a base other than its own.
#[must_use]
pub const fn ipc_reply_timeout_scenario_for_base(
    base: usize,
    selector: usize,
) -> Option<IpcReplyTimeoutScenario> {
    if base == usize::MAX {
        return None;
    }
    if selector == base {
        Some(IpcReplyTimeoutScenario::TimeoutWins)
    } else if selector == base + 1 {
        Some(IpcReplyTimeoutScenario::ReplyWins)
    } else {
        None
    }
}

/// Decode `selector` for the architecture this build targets.
///
/// A selector outside this build's own `(base, base + 1)` pair yields `None` — including
/// one that is a valid selector for a DIFFERENT architecture. A selector that is also a
/// value of this build's pair is interpreted with THIS architecture's meaning, which is
/// the only meaning it can have here.
#[must_use]
pub const fn ipc_reply_timeout_scenario_for_current_arch(
    selector: usize,
) -> Option<IpcReplyTimeoutScenario> {
    ipc_reply_timeout_scenario_for_base(CURRENT_ARCH_SELECTOR_BASE, selector)
}

/// The slot-5 selector this build's kernel must publish for `scenario` — the exact
/// inverse of `ipc_reply_timeout_scenario_for_current_arch`, so encode and decode cannot
/// drift.
#[must_use]
pub const fn ipc_reply_timeout_selector_for_current_arch(
    scenario: IpcReplyTimeoutScenario,
) -> usize {
    match scenario {
        IpcReplyTimeoutScenario::TimeoutWins => CURRENT_ARCH_SELECTOR_BASE,
        IpcReplyTimeoutScenario::ReplyWins => CURRENT_ARCH_SELECTOR_BASE + 1,
    }
}
