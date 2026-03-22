use super::time::Tick;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timer {
    current: Tick,
    quantum_ticks: u64,
    ticks_remaining: u64,
}

impl Timer {
    pub fn new(quantum_ticks: u64) -> Self {
        let bounded_quantum = quantum_ticks.max(1);
        Self {
            current: Tick(0),
            quantum_ticks: bounded_quantum,
            ticks_remaining: bounded_quantum,
        }
    }

    pub fn tick_and_check(&mut self) -> (Tick, bool) {
        self.current.0 = self.current.0.wrapping_add(1);
        self.ticks_remaining = self.ticks_remaining.saturating_sub(1);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_preempts_at_quantum_boundary() {
        let mut timer = Timer::new(2);

        assert_eq!(timer.tick_and_check(), (Tick(1), false));
        assert_eq!(timer.tick_and_check(), (Tick(2), true));
        assert_eq!(timer.tick_and_check(), (Tick(3), false));
    }

    #[test]
    fn quantum_one_preempts_each_tick_once() {
        let mut timer = Timer::new(1);
        let (_, p1) = timer.tick_and_check();
        let (_, p2) = timer.tick_and_check();
        assert!(p1);
        assert!(p2);
    }

    #[test]
    fn zero_quantum_is_clamped_to_one() {
        let mut timer = Timer::new(0);
        assert_eq!(timer.quantum(), 1);
        let (_, preempt) = timer.tick_and_check();
        assert!(preempt);
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
        assert_eq!(timer.tick_and_check(), (Tick(1), false));
        assert_eq!(timer.tick_and_check(), (Tick(2), true));
        assert_eq!(timer.tick_and_check(), (Tick(3), false));
        assert_eq!(timer.tick_and_check(), (Tick(4), true));
    }
}
