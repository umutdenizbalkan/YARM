// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::{platform_constants, topology};
use crate::kernel::ipc::ThreadId;
use crate::kernel::topology::CpuTopology;
pub use yarm_kernel::scheduler::{CpuId, SchedulerError, TaskPriority};

pub const MAX_RUN_QUEUE: usize = 64;
pub const MAX_CPUS: usize = platform_constants::MAX_CPUS;
const _: () = assert!(MAX_RUN_QUEUE.is_power_of_two());
const MEMBERSHIP_SLOTS: usize = 64;
const MEMBERSHIP_EMPTY: u8 = 0;
const MEMBERSHIP_TOMBSTONE: u8 = 1;
const MEMBERSHIP_FULL: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScheduledTask {
    tid: ThreadId,
    priority: TaskPriority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RingQueue {
    tids: [ThreadId; MAX_RUN_QUEUE],
    head: usize,
    len: usize,
}

impl RingQueue {
    const fn new() -> Self {
        Self {
            tids: [ThreadId(0); MAX_RUN_QUEUE],
            head: 0,
            len: 0,
        }
    }

    fn index(offset: usize) -> usize {
        offset & (MAX_RUN_QUEUE - 1)
    }

    fn contains(&self, tid: ThreadId) -> bool {
        for offset in 0..self.len {
            let idx = Self::index(self.head + offset);
            if self.tids[idx] == tid {
                return true;
            }
        }
        false
    }

    fn push(&mut self, tid: ThreadId) -> Result<(), SchedulerError> {
        if self.len >= MAX_RUN_QUEUE {
            return Err(SchedulerError::QueueFull);
        }
        let tail = Self::index(self.head + self.len);
        self.tids[tail] = tid;
        self.len += 1;
        Ok(())
    }

    fn pop(&mut self) -> Option<ThreadId> {
        if self.len == 0 {
            return None;
        }
        let tid = self.tids[self.head];
        self.head = Self::index(self.head + 1);
        self.len -= 1;
        Some(tid)
    }
}

#[derive(Debug)]
pub struct PriorityScheduler {
    queues: [RingQueue; 3],
    current: Option<ScheduledTask>,
    membership_keys: [ThreadId; MEMBERSHIP_SLOTS],
    membership_state: [u8; MEMBERSHIP_SLOTS],
    membership_tracking_exhausted: bool,
}

impl Default for PriorityScheduler {
    fn default() -> Self {
        Self {
            queues: [RingQueue::new(), RingQueue::new(), RingQueue::new()],
            current: None,
            membership_keys: [ThreadId(0); MEMBERSHIP_SLOTS],
            membership_state: [MEMBERSHIP_EMPTY; MEMBERSHIP_SLOTS],
            membership_tracking_exhausted: false,
        }
    }
}

impl PriorityScheduler {
    fn membership_hash(tid: ThreadId) -> usize {
        tid.0 as usize & (MEMBERSHIP_SLOTS - 1)
    }

    fn membership_contains(&self, tid: ThreadId) -> bool {
        let mut idx = Self::membership_hash(tid);
        for _ in 0..MEMBERSHIP_SLOTS {
            match self.membership_state[idx] {
                MEMBERSHIP_EMPTY => return false,
                MEMBERSHIP_FULL if self.membership_keys[idx] == tid => return true,
                _ => idx = (idx + 1) & (MEMBERSHIP_SLOTS - 1),
            }
        }
        false
    }

    fn membership_insert(&mut self, tid: ThreadId) -> Result<(), ()> {
        let mut idx = Self::membership_hash(tid);
        let mut first_tombstone: Option<usize> = None;
        for _ in 0..MEMBERSHIP_SLOTS {
            match self.membership_state[idx] {
                MEMBERSHIP_FULL if self.membership_keys[idx] == tid => return Ok(()),
                MEMBERSHIP_TOMBSTONE => {
                    if first_tombstone.is_none() {
                        first_tombstone = Some(idx);
                    }
                }
                MEMBERSHIP_EMPTY => {
                    let insert_idx = first_tombstone.unwrap_or(idx);
                    self.membership_keys[insert_idx] = tid;
                    self.membership_state[insert_idx] = MEMBERSHIP_FULL;
                    return Ok(());
                }
                _ => {}
            }
            idx = (idx + 1) & (MEMBERSHIP_SLOTS - 1);
        }
        Err(())
    }

    fn membership_remove(&mut self, tid: ThreadId) {
        let mut idx = Self::membership_hash(tid);
        for _ in 0..MEMBERSHIP_SLOTS {
            match self.membership_state[idx] {
                MEMBERSHIP_EMPTY => return,
                MEMBERSHIP_FULL if self.membership_keys[idx] == tid => {
                    self.membership_state[idx] = MEMBERSHIP_TOMBSTONE;
                    return;
                }
                _ => idx = (idx + 1) & (MEMBERSHIP_SLOTS - 1),
            }
        }
    }

    fn linear_contains_tid(&self, tid: ThreadId) -> bool {
        if self.current.is_some_and(|task| task.tid == tid) {
            return true;
        }
        self.queues.iter().any(|queue| queue.contains(tid))
    }

    fn rebuild_membership_table(&mut self) -> bool {
        self.membership_keys = [ThreadId(0); MEMBERSHIP_SLOTS];
        self.membership_state = [MEMBERSHIP_EMPTY; MEMBERSHIP_SLOTS];

        let mut exhausted = false;
        if let Some(current) = self.current
            && self.membership_insert(current.tid).is_err()
        {
            exhausted = true;
        }

        for queue_idx in 0..self.queues.len() {
            let queue_len = self.queues[queue_idx].len;
            for offset in 0..queue_len {
                let idx = RingQueue::index(self.queues[queue_idx].head + offset);
                let tid = self.queues[queue_idx].tids[idx];
                if self.membership_insert(tid).is_err() {
                    exhausted = true;
                }
            }
        }

        exhausted
    }

    fn priority_index(priority: TaskPriority) -> usize {
        priority as usize
    }

    fn contains_tid(&self, tid: ThreadId) -> bool {
        if self.membership_tracking_exhausted {
            self.linear_contains_tid(tid)
        } else {
            self.membership_contains(tid)
        }
    }

    pub fn enqueue_with_priority(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        if self.contains_tid(tid) {
            return Err(SchedulerError::AlreadyQueued);
        }
        self.queues[Self::priority_index(priority)].push(tid)?;
        if !self.membership_tracking_exhausted && self.membership_insert(tid).is_err() {
            self.membership_tracking_exhausted = self.rebuild_membership_table();
        }
        Ok(())
    }

    fn dequeue_highest(&mut self) -> Option<ScheduledTask> {
        for priority in [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low] {
            if let Some(tid) = self.queues[Self::priority_index(priority)].pop() {
                return Some(ScheduledTask { tid, priority });
            }
        }
        None
    }

    pub fn dispatch_next(&mut self) -> Option<ThreadId> {
        if let Some(current) = self.current {
            if current.tid.0 == 0 && self.runnable_count() > 0 {
                let next = self.dequeue_highest()?;
                self.current = Some(next);
                return Some(next.tid);
            }
            return Some(current.tid);
        }
        let next = self.dequeue_highest()?;
        self.current = Some(next);
        Some(next.tid)
    }

    pub fn on_preempt(&mut self) -> Option<ThreadId> {
        if let Some(running) = self.current.take() {
            if !self.membership_tracking_exhausted {
                self.membership_remove(running.tid);
            }
            if let Err(err) = self.enqueue_with_priority(running.tid, running.priority) {
                if err != SchedulerError::AlreadyQueued && self.runnable_count() != 0 {
                    crate::yarm_log!(
                        "scheduler inconsistency: failed to re-enqueue preempted task {:?}; preserving current task",
                        err
                    );
                }
                if !self.membership_tracking_exhausted {
                    let _ = self.membership_insert(running.tid);
                }
                self.current = Some(running);
                return Some(running.tid);
            }
        }
        self.dispatch_next()
    }

    pub fn block_current(&mut self) -> Option<ThreadId> {
        let current = self.current.take()?;
        if !self.membership_tracking_exhausted {
            self.membership_remove(current.tid);
        }
        Some(current.tid)
    }

    pub fn current_tid(&self) -> Option<ThreadId> {
        self.current.map(|task| task.tid)
    }

    pub fn current_priority(&self) -> Option<TaskPriority> {
        self.current.map(|task| task.priority)
    }

    pub fn runnable_count(&self) -> usize {
        self.queues.iter().map(|queue| queue.len).sum()
    }
}

#[derive(Debug)]
pub struct SmpScheduler {
    schedulers: [PriorityScheduler; MAX_CPUS],
    topology: CpuTopology,
    next_balance_cpu: usize,
}

impl Default for SmpScheduler {
    fn default() -> Self {
        Self {
            schedulers: core::array::from_fn(|_| PriorityScheduler::default()),
            topology: CpuTopology::from_present_bitmap(topology::default_present_cpu_bitmap()),
            next_balance_cpu: 0,
        }
    }
}

impl SmpScheduler {
    fn check_cpu(cpu: CpuId) -> Result<usize, SchedulerError> {
        let idx = cpu.0 as usize;
        if idx >= MAX_CPUS {
            return Err(SchedulerError::InvalidCpu);
        }
        Ok(idx)
    }

    fn check_online_cpu(&self, cpu: CpuId) -> Result<usize, SchedulerError> {
        let idx = Self::check_cpu(cpu)?;
        if !self.topology.cpu_online(idx as u8) {
            return Err(SchedulerError::CpuOffline);
        }
        Ok(idx)
    }

    pub fn validate_online_cpu(&self, cpu: CpuId) -> Result<(), SchedulerError> {
        self.check_online_cpu(cpu).map(|_| ())
    }

    fn least_loaded_online_cpu(&self, start: usize) -> Result<CpuId, SchedulerError> {
        let mut best: Option<(usize, CpuId)> = None;
        for offset in 0..MAX_CPUS {
            let idx = (start + offset) % MAX_CPUS;
            if self.topology.cpu_online(idx as u8) {
                let load = self.schedulers[idx].runnable_count()
                    + usize::from(self.schedulers[idx].current_tid().is_some());
                let cpu = CpuId(idx as u8);
                if best.map_or(true, |(best_load, _)| load < best_load) {
                    best = Some((load, cpu));
                }
            }
        }
        best.map(|(_, cpu)| cpu).ok_or(SchedulerError::CpuOffline)
    }

    /// Simulates the full secondary-CPU bring-up handshake in a single call.
    ///
    /// This is suitable for tests and single-threaded simulation. On real SMP
    /// hardware, the bootstrap CPU should call `start_secondary_cpu()`, the
    /// secondary CPU's entry point should call `acknowledge_secondary_cpu()` on
    /// itself, and only then should the bootstrap CPU call
    /// `mark_cpu_online()`.
    pub fn bring_up_cpu(&mut self, cpu: CpuId) -> Result<(), SchedulerError> {
        Self::check_cpu(cpu)?;
        self.topology
            .start_secondary_cpu(cpu.0)
            .map_err(|_| SchedulerError::CpuOffline)?;
        self.topology
            .acknowledge_secondary_cpu(cpu.0)
            .map_err(|_| SchedulerError::CpuOffline)?;
        self.topology
            .mark_cpu_online(cpu.0)
            .map_err(|_| SchedulerError::CpuOffline)
    }

    pub fn cpu_is_online(&self, cpu: CpuId) -> bool {
        Self::check_cpu(cpu)
            .ok()
            .map(|idx| self.topology.cpu_online(idx as u8))
            .unwrap_or(false)
    }

    pub fn online_cpu_bitmap(&self) -> u64 {
        self.topology.online_cpu_bitmap()
    }

    pub fn online_cpu_count(&self) -> usize {
        self.topology.online_cpu_count()
    }

    pub fn present_cpu_count(&self) -> usize {
        self.topology.present_cpu_count()
    }

    pub fn present_cpu_bitmap(&self) -> u64 {
        self.topology.present_cpu_bitmap()
    }

    /// WARNING: Resets all secondary CPU online state.
    ///
    /// Any tasks already queued on secondary CPUs remain in their per-CPU run
    /// queues, but those CPUs will appear offline until `bring_up_cpu()` is run
    /// again for each secondary.
    pub fn set_present_cpu_bitmap(&mut self, present: u64) {
        self.topology = CpuTopology::from_present_bitmap(present);
        self.next_balance_cpu = 0;
    }

    pub fn enqueue_on_with_priority(
        &mut self,
        cpu: CpuId,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        let idx = self.check_online_cpu(cpu)?;
        if cfg!(not(feature = "hosted-dev")) {
            crate::yarm_log!(
                "ENQUEUE_QUEUE_INDEX tid={} requested_cpu={} queue_index={}",
                tid.0,
                cpu.0,
                idx
            );
        }
        self.schedulers[idx]
            .enqueue_with_priority(tid, priority)
            .map(|_| {
                if cfg!(not(feature = "hosted-dev")) {
                    crate::yarm_log!("ENQUEUE_COMMIT tid={} queue_cpu={}", tid.0, cpu.0);
                }
            })
    }

    pub fn enqueue_balanced(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<CpuId, SchedulerError> {
        let start = self.next_balance_cpu;
        let cpu = self.least_loaded_online_cpu(start)?;
        self.enqueue_on_with_priority(cpu, tid, priority)?;
        self.next_balance_cpu = (cpu.0 as usize + 1) % MAX_CPUS;
        Ok(cpu)
    }

    pub fn enqueue_on(&mut self, cpu: CpuId, tid: ThreadId) -> Result<(), SchedulerError> {
        self.enqueue_on_with_priority(cpu, tid, TaskPriority::Normal)
    }

    pub fn dispatch_next_on(&mut self, cpu: CpuId) -> Option<ThreadId> {
        let idx = self.check_online_cpu(cpu).ok()?;
        let idle_tid = self.schedulers[idx].current_tid().unwrap_or(ThreadId(0));
        let runq_len = self.schedulers[idx].runnable_count();
        if cfg!(not(feature = "hosted-dev")) {
            crate::yarm_log!(
                "SCHED cpu={} idle_tid={} runq_len={}",
                cpu.0,
                idle_tid.0,
                runq_len
            );
        }
        let final_tid = self.schedulers[idx].dispatch_next();
        if cfg!(not(feature = "hosted-dev")) {
            crate::yarm_log!("SCHED cpu={} final_tid={:?}", cpu.0, final_tid);
        }
        final_tid
    }

    pub fn on_preempt_on(&mut self, cpu: CpuId) -> Option<ThreadId> {
        let idx = self.check_online_cpu(cpu).ok()?;
        self.schedulers[idx].on_preempt()
    }

    pub fn block_current_on(&mut self, cpu: CpuId) -> Option<ThreadId> {
        let idx = self.check_online_cpu(cpu).ok()?;
        self.schedulers[idx].block_current()
    }

    pub fn current_tid_on(&self, cpu: CpuId) -> Option<ThreadId> {
        let idx = self.check_online_cpu(cpu).ok()?;
        self.schedulers[idx].current_tid()
    }

    pub fn current_priority_on(&self, cpu: CpuId) -> Option<TaskPriority> {
        let idx = self.check_online_cpu(cpu).ok()?;
        self.schedulers[idx].current_priority()
    }

    pub fn runnable_count_on(&self, cpu: CpuId) -> usize {
        let Ok(idx) = self.check_online_cpu(cpu) else {
            return 0;
        };
        self.schedulers[idx].runnable_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_rotates_on_preempt() {
        let mut sched = PriorityScheduler::default();
        assert!(
            sched
                .enqueue_with_priority(ThreadId(1), TaskPriority::Normal)
                .is_ok()
        );
        assert!(
            sched
                .enqueue_with_priority(ThreadId(2), TaskPriority::Normal)
                .is_ok()
        );

        assert_eq!(sched.dispatch_next().expect("task 1"), ThreadId(1));
        assert_eq!(sched.on_preempt().expect("task 2"), ThreadId(2));
        assert_eq!(sched.on_preempt().expect("task 1"), ThreadId(1));
    }

    #[test]
    fn scheduler_duplicate_enqueue_is_rejected() {
        let mut sched = PriorityScheduler::default();
        assert!(
            sched
                .enqueue_with_priority(ThreadId(7), TaskPriority::Normal)
                .is_ok()
        );
        assert_eq!(
            sched.enqueue_with_priority(ThreadId(7), TaskPriority::High),
            Err(SchedulerError::AlreadyQueued)
        );
        assert_eq!(sched.runnable_count(), 1);
    }

    #[test]
    fn scheduler_prefers_higher_priority_work() {
        let mut sched = PriorityScheduler::default();
        assert!(
            sched
                .enqueue_with_priority(ThreadId(10), TaskPriority::Low)
                .is_ok()
        );
        assert!(
            sched
                .enqueue_with_priority(ThreadId(20), TaskPriority::High)
                .is_ok()
        );
        assert!(
            sched
                .enqueue_with_priority(ThreadId(30), TaskPriority::Normal)
                .is_ok()
        );
        assert_eq!(sched.dispatch_next(), Some(ThreadId(20)));
        assert_eq!(sched.current_priority(), Some(TaskPriority::High));
    }

    #[test]
    fn balanced_enqueue_round_robins_equal_load_cpus() {
        let mut sched = SmpScheduler::default();
        sched.bring_up_cpu(CpuId(1)).expect("cpu1");

        let cpu_a = sched
            .enqueue_balanced(ThreadId(10), TaskPriority::Normal)
            .expect("enqueue a");
        let cpu_b = sched
            .enqueue_balanced(ThreadId(11), TaskPriority::Normal)
            .expect("enqueue b");

        assert_eq!(cpu_a, CpuId(0));
        assert_eq!(cpu_b, CpuId(1));
    }

    #[test]
    fn smp_scheduler_tracks_per_cpu_queues() {
        let mut sched = SmpScheduler::default();
        assert_eq!(sched.online_cpu_count(), 1);
        assert!(sched.bring_up_cpu(CpuId(1)).is_ok());
        assert_eq!(sched.online_cpu_count(), 2);
        assert!(
            sched
                .enqueue_on_with_priority(CpuId(0), ThreadId(10), TaskPriority::Normal)
                .is_ok()
        );
        assert!(
            sched
                .enqueue_on_with_priority(CpuId(1), ThreadId(20), TaskPriority::High)
                .is_ok()
        );
        assert_eq!(sched.dispatch_next_on(CpuId(0)), Some(ThreadId(10)));
        assert_eq!(sched.dispatch_next_on(CpuId(1)), Some(ThreadId(20)));
        assert_eq!(sched.current_tid_on(CpuId(0)), Some(ThreadId(10)));
        assert_eq!(sched.current_tid_on(CpuId(1)), Some(ThreadId(20)));
    }

    #[test]
    fn membership_tracking_rebuilds_before_falling_back_to_linear_scan() {
        let mut sched = PriorityScheduler::default();
        for tid in 1..=(MEMBERSHIP_SLOTS as u64) {
            sched
                .enqueue_with_priority(ThreadId(tid), TaskPriority::Normal)
                .expect("seed queue");
        }

        for _ in 0..MEMBERSHIP_SLOTS {
            assert!(sched.dispatch_next().is_some());
            assert!(sched.block_current().is_some());
        }
        assert_eq!(sched.runnable_count(), 0);

        for tid in 1000..(1000 + MEMBERSHIP_SLOTS as u64) {
            sched
                .enqueue_with_priority(ThreadId(tid), TaskPriority::Normal)
                .expect("reused membership slot");
        }

        assert!(!sched.membership_tracking_exhausted);
        assert_eq!(
            sched.enqueue_with_priority(ThreadId(1000), TaskPriority::High),
            Err(SchedulerError::AlreadyQueued)
        );
    }

    #[test]
    fn pass_b_scheduler_types_are_reexported_from_yarm_kernel() {
        use core::mem;

        assert_eq!(
            mem::size_of::<CpuId>(),
            mem::size_of::<yarm_kernel::scheduler::CpuId>()
        );
        assert_eq!(
            TaskPriority::High as u8,
            yarm_kernel::scheduler::TaskPriority::High as u8
        );
        let _err: yarm_kernel::scheduler::SchedulerError = SchedulerError::QueueFull;
    }
}
