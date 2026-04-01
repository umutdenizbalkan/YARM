// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use core::ops::{Add, Sub};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TickInstant(pub u64);

pub type Tick = TickInstant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TickDuration(pub u64);

impl Add<TickDuration> for TickInstant {
    type Output = TickInstant;

    fn add(self, rhs: TickDuration) -> Self::Output {
        TickInstant(self.0.wrapping_add(rhs.0))
    }
}

impl Sub for TickInstant {
    type Output = TickDuration;

    fn sub(self, rhs: Self) -> Self::Output {
        TickDuration(self.0.wrapping_sub(rhs.0))
    }
}

impl Add for TickDuration {
    type Output = TickDuration;

    fn add(self, rhs: Self) -> Self::Output {
        TickDuration(self.0.wrapping_add(rhs.0))
    }
}

impl TickDuration {
    /// Returns true once `now` is at least this wrapped duration past `start`.
    pub const fn has_elapsed_since(self, start: TickInstant, now: TickInstant) -> bool {
        now.0.wrapping_sub(start.0) >= self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_instant_arithmetic_wraps_consistently() {
        assert_eq!(TickInstant(u64::MAX) + TickDuration(2), TickInstant(1));
        assert_eq!(TickInstant(1) - TickInstant(u64::MAX), TickDuration(2));
    }

    #[test]
    fn tick_duration_supports_wrapping_add_and_elapsed_checks() {
        assert_eq!(TickDuration(u64::MAX) + TickDuration(2), TickDuration(1));
        assert!(TickDuration(5).has_elapsed_since(TickInstant(10), TickInstant(15)));
    }
}
