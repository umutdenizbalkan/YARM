// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CNodeId(pub u64);

impl CapId {
    pub const INDEX_BITS: u64 = 16;
    pub const INDEX_MASK: u64 = (1 << Self::INDEX_BITS) - 1;

    pub const fn new(index: usize, generation: u64) -> Self {
        Self((generation << Self::INDEX_BITS) | (index as u64))
    }

    pub const fn index(self) -> usize {
        (self.0 & Self::INDEX_MASK) as usize
    }

    pub const fn generation(self) -> u64 {
        self.0 >> Self::INDEX_BITS
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapRights(u8);

impl CapRights {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const MAP: Self = Self(1 << 2);
    pub const SEND: Self = Self(1 << 3);
    pub const RECEIVE: Self = Self(1 << 4);
    pub const SCHEDULE: Self = Self(1 << 5);
    pub const SIGNAL: Self = Self(1 << 6);
    pub const WAIT: Self = Self(1 << 7);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub const fn is_subset_of(self, other: Self) -> bool {
        (self.0 & !other.0) == 0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl core::ops::BitOr for CapRights {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl core::ops::BitOrAssign for CapRights {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityDeriveError {
    ParentMissing,
    RightsEscalation,
    SpaceFull,
    AllocFailed,
    SlotOccupied,
    InvalidSlot,
    NotFound,
}

impl fmt::Display for CapabilityDeriveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ParentMissing => "parent capability does not exist",
            Self::RightsEscalation => "derived capability would escalate rights",
            Self::SpaceFull => "capability space is full",
            Self::AllocFailed => "capability space allocation failed",
            Self::SlotOccupied => "destination slot is occupied",
            Self::InvalidSlot => "invalid destination slot",
            Self::NotFound => "capability does not exist",
        };
        f.write_str(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_id_roundtrip_index_generation() {
        let cap = CapId::new(17, 9);
        assert_eq!(cap.index(), 17);
        assert_eq!(cap.generation(), 9);
    }

    #[test]
    fn rights_union_and_subset() {
        let rw = CapRights::READ | CapRights::WRITE;
        assert!(rw.contains(CapRights::READ));
        assert!(CapRights::READ.is_subset_of(rw));
        assert!(!CapRights::MAP.is_subset_of(rw));
    }
}
