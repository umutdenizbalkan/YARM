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
}
