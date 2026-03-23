const INPUT_QUEUE_LIMIT: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputStats {
    pub events: u64,
    pub dropped_events: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputService {
    stats: InputStats,
    in_queue: usize,
}

impl InputService {
    pub const fn new() -> Self {
        Self {
            stats: InputStats {
                events: 0,
                dropped_events: 0,
            },
            in_queue: 0,
        }
    }

    pub fn push_event(&mut self, accepted: bool) {
        if !accepted {
            self.stats.dropped_events = self.stats.dropped_events.saturating_add(1);
            return;
        }
        if self.in_queue >= INPUT_QUEUE_LIMIT {
            self.stats.dropped_events = self.stats.dropped_events.saturating_add(1);
            return;
        }
        self.in_queue = self.in_queue.saturating_add(1);
        self.stats.events = self.stats.events.saturating_add(1);
    }

    pub fn drain(&mut self, events: usize) {
        self.in_queue = self.in_queue.saturating_sub(events);
    }

    pub const fn stats(&self) -> InputStats {
        self.stats
    }
}

pub fn run() {
    let mut s = InputService::new();
    s.push_event(true);
    let stats = s.stats();
    crate::yarm_log!(
        "input.srv online: events={}, dropped_events={}",
        stats.events,
        stats.dropped_events
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_queue_overflow_and_drain_is_deterministic() {
        let mut s = InputService::new();
        for _ in 0..130 {
            s.push_event(true);
        }
        s.push_event(false);
        s.drain(64);
        for _ in 0..64 {
            s.push_event(true);
        }

        assert_eq!(
            s.stats(),
            InputStats {
                events: 192,
                dropped_events: 3,
            }
        );
    }
}
