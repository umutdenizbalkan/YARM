use crate::arch::{platform_layout, topology};
use crate::kernel::topology::CpuTopology;

pub const MAX_RUN_QUEUE: usize = 64;
pub const MAX_CPUS: usize = platform_layout::MAX_CPUS;

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
struct RunQueueSlot {
    occupied: bool,
    tid: u64,
    priority: TaskPriority,
}

impl RunQueueSlot {
    const EMPTY: Self = Self {
        occupied: false,
        tid: 0,
        priority: TaskPriority::Normal,
    };

    const fn from_task(task: ScheduledTask) -> Self {
        Self {
            occupied: true,
            tid: task.tid,
            priority: task.priority,
        }
    }

    const fn task(self) -> Option<ScheduledTask> {
        if self.occupied {
            Some(ScheduledTask {
                tid: self.tid,
                priority: self.priority,
            })
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub struct PriorityScheduler {
    run_queue: [RunQueueSlot; MAX_RUN_QUEUE],
    head: usize,
    len: usize,
    current: Option<ScheduledTask>,
}

impl Default for PriorityScheduler {
    fn default() -> Self {
        Self {
            run_queue: [RunQueueSlot::EMPTY; MAX_RUN_QUEUE],
            head: 0,
            len: 0,
            current: None,
        }
    }
}

impl PriorityScheduler {
    fn queue_index(offset: usize) -> usize {
        (offset) & (MAX_RUN_QUEUE - 1)
    }

    fn contains_tid(&self, tid: u64) -> bool {
        if self.current.is_some_and(|task| task.tid == tid) {
            return true;
        }
        let mut i = 0;
        while i < self.len {
            let idx = Self::queue_index(self.head + i);
            if self.run_queue[idx]
                .task()
                .is_some_and(|task| task.tid == tid)
            {
                return true;
            }
            i += 1;
        }
        false
    }

    pub fn enqueue_with_priority(
        &mut self,
        tid: u64,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        if self.contains_tid(tid) {
            return Err(SchedulerError::AlreadyQueued);
        }
        if self.len >= MAX_RUN_QUEUE {
            return Err(SchedulerError::QueueFull);
        }
        let tail = Self::queue_index(self.head + self.len);
        self.run_queue[tail] = RunQueueSlot::from_task(ScheduledTask { tid, priority });
        self.len += 1;
        Ok(())
    }

    fn dequeue_highest(&mut self) -> Option<ScheduledTask> {
        if self.len == 0 {
            return None;
        }

        let mut best_offset = 0usize;
        let mut best = self.run_queue[self.head].task()?;
        let mut offset = 1usize;
        while offset < self.len {
            let idx = Self::queue_index(self.head + offset);
            if let Some(candidate) = self.run_queue[idx].task() {
                if candidate.priority < best.priority {
                    best = candidate;
                    best_offset = offset;
                }
            }
            offset += 1;
        }

        while best_offset + 1 < self.len {
            let from = Self::queue_index(self.head + best_offset + 1);
            let to = Self::queue_index(self.head + best_offset);
            self.run_queue[to] = self.run_queue[from];
            best_offset += 1;
        }
        let tail = Self::queue_index(self.head + self.len - 1);
        self.run_queue[tail] = RunQueueSlot::EMPTY;
        self.len -= 1;
        Some(best)
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
            if self
                .enqueue_with_priority(running.tid, running.priority)
                .is_err_and(|err| err != SchedulerError::AlreadyQueued)
            {
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
        self.len
    }
}

#[derive(Debug)]
pub struct SmpScheduler {
    schedulers: [PriorityScheduler; MAX_CPUS],
    topology: CpuTopology,
    current_cpu: CpuId,
}

impl Default for SmpScheduler {
    fn default() -> Self {
        Self {
            schedulers: core::array::from_fn(|_| PriorityScheduler::default()),
            topology: CpuTopology::from_present_bitmap(topology::default_present_cpu_bitmap()),
            current_cpu: CpuId(0),
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

    fn least_loaded_online_cpu(&self) -> Result<CpuId, SchedulerError> {
        let mut best: Option<(usize, CpuId)> = None;
        let mut idx = 0usize;
        while idx < MAX_CPUS {
            if self.topology.cpu_online(idx as u8) {
                let load = self.schedulers[idx].runnable_count()
                    + usize::from(self.schedulers[idx].current_tid().is_some());
                let cpu = CpuId(idx as u8);
                if best.is_none_or(|(best_load, best_cpu)| {
                    load < best_load || (load == best_load && cpu.0 < best_cpu.0)
                }) {
                    best = Some((load, cpu));
                }
            }
            idx += 1;
        }
        best.map(|(_, cpu)| cpu).ok_or(SchedulerError::CpuOffline)
    }

    pub fn current_cpu(&self) -> CpuId {
        self.current_cpu
    }

    pub fn set_current_cpu(&mut self, cpu: CpuId) -> Result<(), SchedulerError> {
        self.check_online_cpu(cpu)?;
        self.current_cpu = cpu;
        Ok(())
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
        self.schedulers[idx]
            .enqueue_with_priority(tid, priority)
            .or_else(|err| match err {
                SchedulerError::AlreadyQueued => Ok(()),
                other => Err(other),
            })
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

    pub fn enqueue(&mut self, tid: u64) -> Result<(), SchedulerError> {
        self.enqueue_on(self.current_cpu, tid)
    }

    pub fn dispatch_next(&mut self) -> Option<u64> {
        self.dispatch_next_on(self.current_cpu)
    }

    pub fn on_preempt(&mut self) -> Option<u64> {
        self.on_preempt_on(self.current_cpu)
    }

    pub fn block_current(&mut self) -> Option<u64> {
        self.block_current_on(self.current_cpu)
    }

    pub fn current_tid(&self) -> Option<u64> {
        self.current_tid_on(self.current_cpu)
    }

    pub fn runnable_count(&self) -> usize {
        self.runnable_count_on(self.current_cpu)
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
    fn scheduler_duplicate_enqueue_is_ignored() {
        let mut sched = PriorityScheduler::default();
        assert!(sched.enqueue_with_priority(7, TaskPriority::Normal).is_ok());
        assert_eq!(
            sched.enqueue_with_priority(7, TaskPriority::High),
            Err(SchedulerError::AlreadyQueued)
        );
        assert_eq!(sched.runnable_count(), 1);
    }

    #[test]
    fn scheduler_dispatch_next_does_not_overwrite_current() {
        let mut sched = PriorityScheduler::default();
        assert!(sched.enqueue_with_priority(1, TaskPriority::Normal).is_ok());
        assert!(sched.enqueue_with_priority(2, TaskPriority::Normal).is_ok());
        assert_eq!(sched.dispatch_next(), Some(1));
        assert_eq!(sched.dispatch_next(), Some(1));
    }

    #[test]
    fn scheduler_prefers_higher_priority_work() {
        let mut sched = PriorityScheduler::default();
        assert!(sched.enqueue_with_priority(10, TaskPriority::Low).is_ok());
        assert!(sched.enqueue_with_priority(20, TaskPriority::High).is_ok());
        assert!(sched
            .enqueue_with_priority(30, TaskPriority::Normal)
            .is_ok());

        assert_eq!(sched.dispatch_next(), Some(20));
        assert_eq!(sched.current_priority(), Some(TaskPriority::High));
    }

    #[test]
    fn scheduler_wraparound_and_overflow_path() {
        let mut sched = PriorityScheduler::default();
        for tid in 0..MAX_RUN_QUEUE as u64 {
            assert!(sched
                .enqueue_with_priority(tid, TaskPriority::Normal)
                .is_ok());
        }
        assert_eq!(
            sched.enqueue_with_priority(999, TaskPriority::Normal),
            Err(SchedulerError::QueueFull)
        );

        for _ in 0..(MAX_RUN_QUEUE / 2) {
            let _ = sched.dispatch_next();
            let _ = sched.block_current();
        }
        for tid in 1000..1000 + (MAX_RUN_QUEUE / 2) as u64 {
            assert!(sched
                .enqueue_with_priority(tid, TaskPriority::Normal)
                .is_ok());
        }
    }

    #[test]
    fn smp_scheduler_tracks_per_cpu_queues() {
        let mut sched = SmpScheduler::default();
        assert_eq!(sched.online_cpu_count(), 1);
        assert!(sched.bring_up_cpu(CpuId(1)).is_ok());
        assert_eq!(sched.online_cpu_count(), 2);

        assert!(sched
            .enqueue_on_with_priority(CpuId(0), 10, TaskPriority::Normal)
            .is_ok());
        assert!(sched
            .enqueue_on_with_priority(CpuId(1), 20, TaskPriority::High)
            .is_ok());

        assert_eq!(sched.dispatch_next_on(CpuId(0)), Some(10));
        assert_eq!(sched.dispatch_next_on(CpuId(1)), Some(20));
        assert_eq!(sched.current_tid_on(CpuId(0)), Some(10));
        assert_eq!(sched.current_tid_on(CpuId(1)), Some(20));
    }

    #[test]
    fn smp_enqueue_on_offline_cpu_returns_typed_error() {
        let mut sched = SmpScheduler::default();
        assert_eq!(
            sched.enqueue_on(CpuId(2), 55),
            Err(SchedulerError::CpuOffline)
        );
    }

    #[test]
    fn smp_set_current_cpu_rejects_invalid_cpu() {
        let mut sched = SmpScheduler::default();
        assert_eq!(
            sched.set_current_cpu(CpuId(MAX_CPUS as u8)),
            Err(SchedulerError::InvalidCpu)
        );
    }

    #[test]
    fn balanced_enqueue_prefers_least_loaded_online_cpu() {
        let mut sched = SmpScheduler::default();
        sched.bring_up_cpu(CpuId(1)).expect("cpu1");
        sched.bring_up_cpu(CpuId(2)).expect("cpu2");
        sched
            .enqueue_on_with_priority(CpuId(0), 1, TaskPriority::Normal)
            .expect("cpu0");
        sched
            .enqueue_on_with_priority(CpuId(1), 2, TaskPriority::Normal)
            .expect("cpu1");

        let chosen = sched
            .enqueue_balanced(99, TaskPriority::High)
            .expect("balanced");
        assert_eq!(chosen, CpuId(2));
    }
}
