// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-CONTEXT1 §3 — the kernel half of the user execution-state witness
//! (`context1-witness`): identity markers for the transitions the witness grades, and a check that
//! the kernel runs under its own FP control environment.
//!
//! Nothing here saves, restores or chooses any state. The markers record what the trap path
//! already decided — which task entered, which task the trap returns to, from which origin — so the
//! grader can prove that another task actually executed between a capture and its restoration
//! instead of inferring it from a Yield request or a timer arrival.

use core::sync::atomic::{AtomicU32, Ordering};

/// Where a trap was taken from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrapOrigin {
    /// Ring 3 / EL0.
    User,
    /// The kernel's authenticated idle halt.
    Idle,
    /// Any other kernel context.
    Kernel,
}

impl TrapOrigin {
    pub const fn label(self) -> &'static str {
        match self {
            TrapOrigin::User => "user",
            TrapOrigin::Idle => "idle",
            TrapOrigin::Kernel => "kernel",
        }
    }
}

/// Whether one trap deserves a `CTX1_TRAP` line: every timer tick (so a same-task return is
/// visible), and every trap that returns to a different task than the one it interrupted (so every
/// switch, block and idle resume is visible). A same-task syscall is neither and stays silent.
pub const fn should_mark(is_timer: bool, entering: Option<u64>, exiting: Option<u64>) -> bool {
    is_timer || !tid_eq(entering, exiting)
}

const fn tid_eq(a: Option<u64>, b: Option<u64>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Emit the identity marker for one trap, if [`should_mark`] says so. `0` names "no task".
pub fn note_trap(
    cpu: usize,
    vector: u64,
    is_timer: bool,
    origin: TrapOrigin,
    entering: Option<u64>,
    exiting: Option<u64>,
) {
    if !should_mark(is_timer, entering, exiting) {
        return;
    }
    crate::yarm_log!(
        "CTX1_TRAP cpu={} vec=0x{:x} timer={} origin={} in={} out={}",
        cpu,
        vector,
        is_timer as u8,
        origin.label(),
        entering.unwrap_or(0),
        exiting.unwrap_or(0)
    );
}

static KERNEL_ENV_CHECKS: AtomicU32 = AtomicU32::new(0);
static KERNEL_ENV_BAD: AtomicU32 = AtomicU32::new(0);

/// Record whether the kernel, entered from user mode, runs under its own FP control environment
/// (`ok`) rather than the interrupted task's. Every violation is counted; the first eight are
/// logged with the observed control word(s).
pub fn note_kernel_env(ok: bool, observed_a: u64, observed_b: u64) {
    KERNEL_ENV_CHECKS.fetch_add(1, Ordering::Relaxed);
    if ok {
        return;
    }
    let n = KERNEL_ENV_BAD.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 8 {
        crate::yarm_log!(
            "CTX1_KERNEL_ENV_BAD n={} a=0x{:x} b=0x{:x}",
            n,
            observed_a,
            observed_b
        );
    }
}

/// `(checks, violations)` so far.
pub fn kernel_env_counts() -> (u32, u32) {
    (
        KERNEL_ENV_CHECKS.load(Ordering::Relaxed),
        KERNEL_ENV_BAD.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_every_tick_and_every_transition_but_not_a_same_task_syscall() {
        assert!(should_mark(true, Some(5), Some(5)));
        assert!(should_mark(false, Some(5), Some(6)));
        assert!(should_mark(false, Some(5), None));
        assert!(should_mark(false, None, Some(5)));
        assert!(!should_mark(false, Some(5), Some(5)));
        assert!(!should_mark(false, None, None));
    }
}
