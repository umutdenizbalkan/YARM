extern crate std;

use std::println;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputStats {
    pub events: u64,
}

pub fn run() {
    let s = InputStats { events: 0 };
    println!("input.srv scaffold online: events={}", s.events);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_stats_baseline() {
        let s = InputStats { events: 0 };
        assert_eq!(s.events, 0);
    }
}
