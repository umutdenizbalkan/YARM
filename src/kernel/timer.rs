#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timer {
    current: Tick,
    quantum_ticks: u64,
}

impl Timer {
    pub fn new(quantum_ticks: u64) -> Self {
        Self {
            current: Tick(0),
            quantum_ticks,
        }
    }

    pub fn tick(&mut self) -> Tick {
        self.current.0 += 1;
        self.current
    }

    pub fn should_preempt(&self) -> bool {
        self.current.0 != 0 && self.current.0 % self.quantum_ticks == 0
    }

    pub fn current_ticks(&self) -> u64 {
        self.current.0
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
    }
}
