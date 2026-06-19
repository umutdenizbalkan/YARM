// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::time::{Tick, TickInstant};

/// Combines a monotonic tick counter with a quantum-based preemption
/// decision for the current running task.
///
/// Note: This conflates tick counting (hardware concern) with preemption
/// policy (scheduler concern) for prototype simplicity. When per-priority
/// or per-task quanta are needed, split into a `TickCounter` and a
/// `QuantumTracker` held per CPU in the scheduler.
///
/// One `Timer` instance must exist per CPU. Sharing across CPUs is incorrect —
/// each CPU advances its own tick counter from its own timer interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timer {
    current: Tick,
    quantum_ticks: u64,
    ticks_remaining: u64,
}

impl Timer {
    pub fn new(quantum_ticks: u64) -> Self {
        debug_assert!(
            quantum_ticks > 0,
            "quantum_ticks must be non-zero; got {}. Clamping to 1.",
            quantum_ticks
        );
        let bounded_quantum = quantum_ticks.max(1);
        Self {
            current: TickInstant(0),
            quantum_ticks: bounded_quantum,
            ticks_remaining: bounded_quantum,
        }
    }

    pub fn tick_and_check(&mut self) -> (Tick, bool) {
        self.current.0 = self.current.0.wrapping_add(1);
        debug_assert!(
            self.ticks_remaining > 0,
            "ticks_remaining should never be 0 at tick entry"
        );
        self.ticks_remaining -= 1;
        let should_preempt = self.ticks_remaining == 0;
        if should_preempt {
            self.ticks_remaining = self.quantum_ticks;
        }
        (self.current, should_preempt)
    }

    pub fn current_ticks(&self) -> Tick {
        self.current
    }

    pub const fn quantum(&self) -> u64 {
        self.quantum_ticks
    }

    /// Reset the remaining quantum to the full quantum length.
    ///
    /// Call this on voluntary task switches (block, yield-to-other) so the
    /// next scheduled task always starts with a full quantum rather than
    /// inheriting the stale remainder from the task that just left the CPU.
    pub fn reset_quantum(&mut self) {
        self.ticks_remaining = self.quantum_ticks;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_preempts_at_quantum_boundary() {
        let mut timer = Timer::new(2);

        assert_eq!(timer.tick_and_check(), (TickInstant(1), false));
        assert_eq!(timer.tick_and_check(), (TickInstant(2), true));
        assert_eq!(timer.tick_and_check(), (TickInstant(3), false));
    }

    #[test]
    fn quantum_one_preempts_each_tick_once() {
        let mut timer = Timer::new(1);
        let (_, p1) = timer.tick_and_check();
        let (_, p2) = timer.tick_and_check();
        assert!(p1);
        assert!(p2);
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn zero_quantum_is_clamped_to_one() {
        let mut timer = Timer::new(0);
        assert_eq!(timer.quantum(), 1);
        let (_, preempt) = timer.tick_and_check();
        assert!(preempt);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "quantum_ticks must be non-zero")]
    fn zero_quantum_panics_in_debug_builds() {
        let _ = Timer::new(0);
    }

    #[test]
    fn current_ticks_matches_tick_return() {
        let mut timer = Timer::new(3);
        let t = timer.tick_and_check().0;
        assert_eq!(timer.current_ticks(), t);
    }

    #[test]
    fn multiple_boundaries_preempt_once_per_tick() {
        let mut timer = Timer::new(2);
        assert_eq!(timer.tick_and_check(), (TickInstant(1), false));
        assert_eq!(timer.tick_and_check(), (TickInstant(2), true));
        assert_eq!(timer.tick_and_check(), (TickInstant(3), false));
        assert_eq!(timer.tick_and_check(), (TickInstant(4), true));
    }

    #[test]
    fn reset_quantum_restores_full_quantum_mid_flight() {
        let mut timer = Timer::new(5);
        // Burn 2 ticks into the quantum (ticks_remaining drops to 3)
        assert_eq!(timer.tick_and_check().1, false);
        assert_eq!(timer.tick_and_check().1, false);
        // Reset: ticks_remaining must be restored to quantum (5)
        timer.reset_quantum();
        // Must not preempt for the next 4 ticks
        assert_eq!(timer.tick_and_check().1, false);
        assert_eq!(timer.tick_and_check().1, false);
        assert_eq!(timer.tick_and_check().1, false);
        assert_eq!(timer.tick_and_check().1, false);
        // 5th tick after reset: preemption fires
        assert_eq!(timer.tick_and_check().1, true);
    }
}
