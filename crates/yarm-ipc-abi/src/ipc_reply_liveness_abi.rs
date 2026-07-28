// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 200D-2B — the SINGLE, ARCHITECTURE-LOCAL decoder for the direct-reply LIVENESS
//! oracle's startup slot-5 selector.
//!
//! Supersedes the Stage 200C2C2C-R2C `ipc_reply_timeout_abi`. The name changed because the
//! subject did: the question is no longer only "did the deadline or the reply win", it is
//! "how did the blocked caller regain liveness" — by deadline, by reply, or because the
//! authorized replier ceased to exist.
//!
//! # Why interpretation must be architecture-local
//!
//! Each architecture owns a distinct free RUN of slot-5 selector values,
//! `(base, base + 1, base + 2)` = `(TimeoutWins, ReplyWins, ServerDies)`:
//!
//! | arch    | TimeoutWins | ReplyWins | ServerDies |
//! |---------|-------------|-----------|------------|
//! | AArch64 | 8           | 9         | 10         |
//! | RISC-V  | 9           | 10        | 11         |
//! | x86_64  | 10          | 11        | 12         |
//!
//! **The runs overlap.** `10` is AArch64's `ServerDies`, RISC-V's `ReplyWins` AND x86_64's
//! `TimeoutWins`; `11` is RISC-V's `ServerDies` and x86_64's `ReplyWins`. A shared numeric
//! match therefore has no single meaning — Stage 200C2C2C-R2B measured exactly that failure,
//! where `matches!(selector, 8 | 9 | 10)` silently decoded two architectures' `ReplyWins` as
//! `TimeoutWins` and made reply-wins unreachable on both non-x86 ports.
//!
//! The fix is structural rather than a wider match: only the CURRENT build's base is
//! compiled in, so a selector belonging to another architecture's run — and not also to
//! this one's — cannot activate any scenario here.
//!
//! # Why the module is shared
//!
//! The kernel writes the selector into init's startup slot 5; userspace reads it back and
//! picks the scenario. Both directions live here, so the encoder and decoder cannot drift
//! apart the way two separate copies once did.

/// The three direct-reply liveness scenarios. There are no others: caller-exit
/// cancellation, endpoint-destruction liveness, multi-pair concurrency and queued
/// `IpcCall` are all explicitly out of scope for this oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcReplyLivenessScenario {
    /// The deadline claims the terminal; the caller resumes with the canonical `TimedOut`,
    /// and the server's later reply is rejected.
    TimeoutWins,
    /// The server's reply claims the terminal first; the caller resumes with the payload,
    /// a duplicate reply is rejected, and the stale deadline is later scanned harmlessly.
    ReplyWins,
    /// The authorized replier exits without replying. Teardown publishes a deferred
    /// completion, the post-lock drain claims `PeerDeath`, and the caller resumes with the
    /// canonical `ServerDied` (code 10); the stale deadline then loses.
    ServerDies,
}

/// AArch64's slot-5 run base (`8`/`9`/`10`).
pub const AARCH64_SELECTOR_BASE: usize = 8;
/// RISC-V's slot-5 run base (`9`/`10`/`11`).
pub const RISCV64_SELECTOR_BASE: usize = 9;
/// x86_64's slot-5 run base (`10`/`11`/`12`).
pub const X86_64_SELECTOR_BASE: usize = 10;

/// The base compiled into THIS build. Selected by `cfg`, never by a runtime value, so a
/// foreign architecture's base is not even present in the binary.
#[cfg(target_arch = "aarch64")]
pub const CURRENT_ARCH_SELECTOR_BASE: usize = AARCH64_SELECTOR_BASE;
#[cfg(target_arch = "riscv64")]
pub const CURRENT_ARCH_SELECTOR_BASE: usize = RISCV64_SELECTOR_BASE;
#[cfg(target_arch = "x86_64")]
pub const CURRENT_ARCH_SELECTOR_BASE: usize = X86_64_SELECTOR_BASE;
/// Ports without an oracle cell get a base no selector can match, so the oracle is inert
/// rather than accidentally aliased onto another architecture's run.
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
pub const fn ipc_reply_liveness_scenario_for_base(
    base: usize,
    selector: usize,
) -> Option<IpcReplyLivenessScenario> {
    if base == usize::MAX {
        return None;
    }
    if selector == base {
        Some(IpcReplyLivenessScenario::TimeoutWins)
    } else if selector == base + 1 {
        Some(IpcReplyLivenessScenario::ReplyWins)
    } else if selector == base + 2 {
        Some(IpcReplyLivenessScenario::ServerDies)
    } else {
        None
    }
}

/// Decode `selector` for the architecture this build targets.
///
/// A selector outside this build's own run yields `None` — including one that is a valid
/// selector for a DIFFERENT architecture. A selector that is also a value of this build's
/// run is interpreted with THIS architecture's meaning, which is the only meaning it can
/// have here.
#[must_use]
pub const fn ipc_reply_liveness_scenario_for_current_arch(
    selector: usize,
) -> Option<IpcReplyLivenessScenario> {
    ipc_reply_liveness_scenario_for_base(CURRENT_ARCH_SELECTOR_BASE, selector)
}

/// The slot-5 selector this build's kernel must publish for `scenario` — the exact inverse
/// of `ipc_reply_liveness_scenario_for_current_arch`, so encode and decode cannot drift.
#[must_use]
pub const fn ipc_reply_liveness_selector_for_current_arch(
    scenario: IpcReplyLivenessScenario,
) -> usize {
    match scenario {
        IpcReplyLivenessScenario::TimeoutWins => CURRENT_ARCH_SELECTOR_BASE,
        IpcReplyLivenessScenario::ReplyWins => CURRENT_ARCH_SELECTOR_BASE + 1,
        IpcReplyLivenessScenario::ServerDies => CURRENT_ARCH_SELECTOR_BASE + 2,
    }
}
