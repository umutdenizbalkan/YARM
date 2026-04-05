// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuId(pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum TaskPriority {
    High = 0,
    Normal = 1,
    Low = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerError {
    InvalidCpu,
    CpuOffline,
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
}
