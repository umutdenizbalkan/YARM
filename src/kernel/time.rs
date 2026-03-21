use core::ops::{Add, Sub};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tick(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TickDuration(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TickInstant(pub u64);

impl Add<TickDuration> for TickInstant {
    type Output = TickInstant;

    fn add(self, rhs: TickDuration) -> Self::Output {
        TickInstant(self.0.saturating_add(rhs.0))
    }
}

impl Sub for TickInstant {
    type Output = TickDuration;

    fn sub(self, rhs: Self) -> Self::Output {
        TickDuration(self.0.saturating_sub(rhs.0))
    }
}

impl From<Tick> for TickInstant {
    fn from(value: Tick) -> Self {
        TickInstant(value.0)
    }
}
