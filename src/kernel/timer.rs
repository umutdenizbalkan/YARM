#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Tick(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timer {
    current: Tick,
    quantum_ticks: u64,
    last_preempt_tick: Option<Tick>,
}

impl Timer {
    /// Timer combines a monotonic tick counter with a simple quantum-based
    /// preemption decision policy for the current running task.
    pub fn new(quantum_ticks: u64) -> Self {
        let bounded_quantum = quantum_ticks.max(1);
        Self {
            current: Tick(0),
            quantum_ticks: bounded_quantum,
            last_preempt_tick: None,
        }
    }

    pub fn tick(&mut self) -> Tick {
        self.current.0 = self.current.0.wrapping_add(1);
        self.current
    }

    /// Returns `true` at most once per quantum boundary tick.
    pub fn should_preempt(&mut self) -> bool {
        let at_boundary = self.current.0 != 0 && self.current.0 % self.quantum_ticks == 0;
        if !at_boundary {
            return false;
        }

        if self.last_preempt_tick == Some(self.current) {
            return false;
        }

        self.last_preempt_tick = Some(self.current);
        true
    }

    pub fn tick_and_check(&mut self) -> (Tick, bool) {
        let tick = self.tick();
        let should_preempt = self.should_preempt();
        (tick, should_preempt)
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

        assert!(!timer.should_preempt());
        timer.tick();
        assert!(!timer.should_preempt());
        timer.tick();
        assert!(timer.should_preempt());
        assert!(!timer.should_preempt());
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
        let t = timer.tick();
        assert_eq!(timer.current_ticks(), t);
    }

    #[test]
    fn multiple_boundaries_preempt_once_per_tick() {
        let mut timer = Timer::new(2);
        timer.tick();
        assert!(!timer.should_preempt());
        timer.tick();
        assert!(timer.should_preempt());
        assert!(!timer.should_preempt());
        timer.tick();
        assert!(!timer.should_preempt());
        timer.tick();
        assert!(timer.should_preempt());
    }
}
