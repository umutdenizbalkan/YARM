// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuId(pub u8);

/// A scheduling priority: one of [`TaskPriority::LEVELS`] fixed levels, **0 highest**.
///
/// RT-SCHED1A (`doc/SCHEDULING_CONTRACT.md` §23): the run queues hold 32 levels. The
/// general-purpose profile uses the three lowest-numbered of them — [`TaskPriority::High`] (0),
/// [`TaskPriority::Normal`] (1) and [`TaskPriority::Low`] (2) — exactly the three levels, the
/// same numeric order and the same indices it has always had, so a GP configuration selects
/// decision for decision as before. A real-time configuration binds any level `0..=31` to a
/// context ([`TaskPriority::level`]).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct TaskPriority(u8);

#[allow(non_upper_case_globals)]
impl TaskPriority {
    /// The number of priority levels (SC-PRI-10).
    pub const LEVELS: usize = 32;
    /// GP `SystemServer` level (0).
    pub const High: TaskPriority = TaskPriority(0);
    /// GP `Driver` / `App` level (1).
    pub const Normal: TaskPriority = TaskPriority(1);
    /// GP lowest level (2): the scheduler-owned idle placeholder.
    pub const Low: TaskPriority = TaskPriority(2);
    /// The lowest RT level (31).
    pub const LOWEST: TaskPriority = TaskPriority(Self::LEVELS as u8 - 1);

    /// The priority at `level`, or `None` past [`Self::LEVELS`].
    pub const fn from_level(level: usize) -> Option<Self> {
        if level < Self::LEVELS {
            Some(Self(level as u8))
        } else {
            None
        }
    }

    /// This priority's level, `0` highest: the run-queue index.
    pub const fn level(self) -> usize {
        self.0 as usize
    }

    /// Strictly higher priority than `other` (a numerically lower level).
    pub const fn is_higher_than(self, other: Self) -> bool {
        self.0 < other.0
    }
}

impl core::fmt::Debug for TaskPriority {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The three GP levels keep the names every existing marker and assertion prints.
        match self.0 {
            0 => f.write_str("High"),
            1 => f.write_str("Normal"),
            2 => f.write_str("Low"),
            n => write!(f, "L{n}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerError {
    InvalidCpu,
    CpuOffline,
    /// The CPU is PRESENT and ONLINE but wake-only: it runs no dispatcher, so explicit
    /// placement is refused. Materially different from `CpuOffline` — the CPU is up, and a
    /// caller that must distinguish "the target is down" from "the target is up but refuses
    /// work" (a direct-IPC wake, for one) cannot do so if the two collapse.
    WakeOnly,
    QueueFull,
    AlreadyQueued,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_priority_order_is_stable() {
        assert!(TaskPriority::High < TaskPriority::Normal);
        assert!(TaskPriority::Normal < TaskPriority::Low);
    }

    #[test]
    fn thirty_two_levels_with_zero_highest() {
        assert_eq!(TaskPriority::LEVELS, 32);
        assert_eq!(TaskPriority::from_level(0), Some(TaskPriority::High));
        assert_eq!(TaskPriority::from_level(31), Some(TaskPriority::LOWEST));
        assert_eq!(TaskPriority::from_level(32), None);
        assert!(TaskPriority::High.is_higher_than(TaskPriority::LOWEST));
        assert!(!TaskPriority::LOWEST.is_higher_than(TaskPriority::LOWEST));
        assert_eq!(TaskPriority::High.level(), 0);
        assert_eq!(TaskPriority::Normal.level(), 1);
        assert_eq!(TaskPriority::Low.level(), 2);
    }
}
