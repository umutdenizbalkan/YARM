use crate::arch::{platform_layout, topology};
use crate::kernel::topology::CpuTopology;

pub const MAX_RUN_QUEUE: usize = 64;
pub const MAX_CPUS: usize = platform_layout::MAX_CPUS;
const _: () = assert!(MAX_RUN_QUEUE.is_power_of_two());

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScheduledTask {
    tid: u64,
    priority: TaskPriority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RingQueue {
    tids: [u64; MAX_RUN_QUEUE],
    head: usize,
    len: usize,
}

impl RingQueue {
    const fn new() -> Self {
        Self {
            tids: [0; MAX_RUN_QUEUE],
            head: 0,
            len: 0,
        }
    }

    fn index(offset: usize) -> usize {
        offset & (MAX_RUN_QUEUE - 1)
    }

    fn contains(&self, tid: u64) -> bool {
        for offset in 0..self.len {
            let idx = Self::index(self.head + offset);
            if self.tids[idx] == tid {
                return true;
            }
        }
        false
    }

    fn push(&mut self, tid: u64) -> Result<(), SchedulerError> {
        if self.len >= MAX_RUN_QUEUE {
            return Err(SchedulerError::QueueFull);
        }
        let tail = Self::index(self.head + self.len);
        self.tids[tail] = tid;
        self.len += 1;
        Ok(())
    }

    fn pop(&mut self) -> Option<u64> {
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
}

impl Default for PriorityScheduler {
    fn default() -> Self {
        Self {
            queues: [RingQueue::new(), RingQueue::new(), RingQueue::new()],
            current: None,
        }
    }
}

impl PriorityScheduler {
    fn priority_index(priority: TaskPriority) -> usize {
        priority as usize
    }

    fn contains_tid(&self, tid: u64) -> bool {
        if self.current.is_some_and(|task| task.tid == tid) {
            return true;
        }
        self.queues.iter().any(|queue| queue.contains(tid))
    }

    pub fn enqueue_with_priority(
        &mut self,
        tid: u64,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        if self.contains_tid(tid) {
            return Err(SchedulerError::AlreadyQueued);
        }
        self.queues[Self::priority_index(priority)].push(tid)
    }

    fn dequeue_highest(&mut self) -> Option<ScheduledTask> {
        for priority in [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low] {
            if let Some(tid) = self.queues[Self::priority_index(priority)].pop() {
                return Some(ScheduledTask { tid, priority });
            }
        }
        None
    }

    pub fn dispatch_next(&mut self) -> Option<u64> {
        if let Some(current) = self.current {
            return Some(current.tid);
        }
        let next = self.dequeue_highest()?;
        self.current = Some(next);
        Some(next.tid)
    }

    pub fn on_preempt(&mut self) -> Option<u64> {
        if let Some(running) = self.current.take() {
            if let Err(err) = self.enqueue_with_priority(running.tid, running.priority) {
                if err != SchedulerError::AlreadyQueued && self.runnable_count() != 0 {
                    panic!(
                        "scheduler inconsistency: failed to re-enqueue preempted task {:?}",
                        err
                    );
                }
                self.current = Some(running);
                return Some(running.tid);
            }
        }
        self.dispatch_next()
    }

    pub fn block_current(&mut self) -> Option<u64> {
        let current = self.current.take()?;
        Some(current.tid)
    }

    pub fn current_tid(&self) -> Option<u64> {
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
}

impl Default for SmpScheduler {
    fn default() -> Self {
        Self {
            schedulers: core::array::from_fn(|_| PriorityScheduler::default()),
            topology: CpuTopology::from_present_bitmap(topology::default_present_cpu_bitmap()),
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

    fn least_loaded_online_cpu(&self) -> Result<CpuId, SchedulerError> {
        let mut best: Option<(usize, CpuId)> = None;
        for idx in 0..MAX_CPUS {
            if self.topology.cpu_online(idx as u8) {
                let load = self.schedulers[idx].runnable_count()
                    + usize::from(self.schedulers[idx].current_tid().is_some());
                let cpu = CpuId(idx as u8);
                if best.map_or(true, |(best_load, best_cpu)| {
                    load < best_load || (load == best_load && cpu.0 < best_cpu.0)
                }) {
                    best = Some((load, cpu));
                }
            }
        }
        best.map(|(_, cpu)| cpu).ok_or(SchedulerError::CpuOffline)
    }

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

    pub fn set_present_cpu_bitmap(&mut self, present: u64) {
        self.topology = CpuTopology::from_present_bitmap(present);
    }

    pub fn enqueue_on_with_priority(
        &mut self,
        cpu: CpuId,
        tid: u64,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        let idx = self.check_online_cpu(cpu)?;
        self.schedulers[idx].enqueue_with_priority(tid, priority)
    }

    pub fn enqueue_balanced(
        &mut self,
        tid: u64,
        priority: TaskPriority,
    ) -> Result<CpuId, SchedulerError> {
        let cpu = self.least_loaded_online_cpu()?;
        self.enqueue_on_with_priority(cpu, tid, priority)?;
        Ok(cpu)
    }

    pub fn enqueue_on(&mut self, cpu: CpuId, tid: u64) -> Result<(), SchedulerError> {
        self.enqueue_on_with_priority(cpu, tid, TaskPriority::Normal)
    }

    pub fn dispatch_next_on(&mut self, cpu: CpuId) -> Option<u64> {
        let idx = self.check_online_cpu(cpu).ok()?;
        self.schedulers[idx].dispatch_next()
    }

    pub fn on_preempt_on(&mut self, cpu: CpuId) -> Option<u64> {
        let idx = self.check_online_cpu(cpu).ok()?;
        self.schedulers[idx].on_preempt()
    }

    pub fn block_current_on(&mut self, cpu: CpuId) -> Option<u64> {
        let idx = self.check_online_cpu(cpu).ok()?;
        self.schedulers[idx].block_current()
    }

    pub fn current_tid_on(&self, cpu: CpuId) -> Option<u64> {
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
        assert!(sched.enqueue_with_priority(1, TaskPriority::Normal).is_ok());
        assert!(sched.enqueue_with_priority(2, TaskPriority::Normal).is_ok());

        assert_eq!(sched.dispatch_next().expect("task 1"), 1);
        assert_eq!(sched.on_preempt().expect("task 2"), 2);
        assert_eq!(sched.on_preempt().expect("task 1"), 1);
    }

    #[test]
    fn scheduler_duplicate_enqueue_is_rejected() {
        let mut sched = PriorityScheduler::default();
        assert!(sched.enqueue_with_priority(7, TaskPriority::Normal).is_ok());
        assert_eq!(
            sched.enqueue_with_priority(7, TaskPriority::High),
            Err(SchedulerError::AlreadyQueued)
        );
        assert_eq!(sched.runnable_count(), 1);
    }

    #[test]
    fn scheduler_prefers_higher_priority_work() {
        let mut sched = PriorityScheduler::default();
        assert!(sched.enqueue_with_priority(10, TaskPriority::Low).is_ok());
        assert!(sched.enqueue_with_priority(20, TaskPriority::High).is_ok());
        assert!(
            sched
                .enqueue_with_priority(30, TaskPriority::Normal)
                .is_ok()
        );
        assert_eq!(sched.dispatch_next(), Some(20));
        assert_eq!(sched.current_priority(), Some(TaskPriority::High));
    }

    #[test]
    fn smp_scheduler_tracks_per_cpu_queues() {
        let mut sched = SmpScheduler::default();
        assert_eq!(sched.online_cpu_count(), 1);
        assert!(sched.bring_up_cpu(CpuId(1)).is_ok());
        assert_eq!(sched.online_cpu_count(), 2);
        assert!(
            sched
                .enqueue_on_with_priority(CpuId(0), 10, TaskPriority::Normal)
                .is_ok()
        );
        assert!(
            sched
                .enqueue_on_with_priority(CpuId(1), 20, TaskPriority::High)
                .is_ok()
        );
        assert_eq!(sched.dispatch_next_on(CpuId(0)), Some(10));
        assert_eq!(sched.dispatch_next_on(CpuId(1)), Some(20));
        assert_eq!(sched.current_tid_on(CpuId(0)), Some(10));
        assert_eq!(sched.current_tid_on(CpuId(1)), Some(20));
    }
}
