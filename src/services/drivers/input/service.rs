extern crate std;

use std::println;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputStats {
    pub events: u64,
    pub dropped_events: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputService {
    stats: InputStats,
}

impl InputService {
    pub const fn new() -> Self {
        Self {
            stats: InputStats {
                events: 0,
                dropped_events: 0,
            },
        }
    }

    pub fn push_event(&mut self, accepted: bool) {
        if accepted {
            self.stats.events = self.stats.events.saturating_add(1);
        } else {
            self.stats.dropped_events = self.stats.dropped_events.saturating_add(1);
        }
    }

    pub const fn stats(&self) -> InputStats {
        self.stats
    }
}

pub fn run() {
    let mut s = InputService::new();
    s.push_event(true);
    let stats = s.stats();
    println!(
        "input.srv online: events={}, dropped_events={}",
        stats.events, stats.dropped_events
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_tracks_accepted_and_dropped_events() {
        let mut s = InputService::new();
        s.push_event(true);
        s.push_event(false);
        assert_eq!(
            s.stats(),
            InputStats {
                events: 1,
                dropped_events: 1,
            }
        );
    }
}
