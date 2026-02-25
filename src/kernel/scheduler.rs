pub const MAX_RUN_QUEUE: usize = 128;
pub const MAX_CPUS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuId(pub usize);

#[derive(Debug)]
pub struct RoundRobinScheduler {
    run_queue: [Option<u64>; MAX_RUN_QUEUE],
    head: usize,
    len: usize,
    current: Option<u64>,
}

impl Default for RoundRobinScheduler {
    fn default() -> Self {
        Self {
            run_queue: [None; MAX_RUN_QUEUE],
            head: 0,
            len: 0,
            current: None,
        }
    }
}

impl RoundRobinScheduler {
    pub fn enqueue(&mut self, tid: u64) -> Result<(), u64> {
        if self.len >= MAX_RUN_QUEUE {
            return Err(tid);
        }

        let tail = (self.head + self.len) % MAX_RUN_QUEUE;
        self.run_queue[tail] = Some(tid);
        self.len += 1;
        Ok(())
    }

    fn dequeue(&mut self) -> Option<u64> {
        if self.len == 0 {
            return None;
        }

        let idx = self.head;
        self.head = (self.head + 1) % MAX_RUN_QUEUE;
        self.len -= 1;
        self.run_queue[idx].take()
    }

    pub fn dispatch_next(&mut self) -> Option<u64> {
        self.current = self.dequeue();
        self.current
    }

    pub fn on_preempt(&mut self) -> Option<u64> {
        if let Some(running_tid) = self.current.take() {
            let _ = self.enqueue(running_tid);
        }
        self.dispatch_next()
    }

    pub fn block_current(&mut self) -> Option<u64> {
        self.current.take()
    }

    pub fn current_tid(&self) -> Option<u64> {
        self.current
    }

    pub fn runnable_count(&self) -> usize {
        self.len
    }
}

#[derive(Debug)]
pub struct SmpScheduler {
    schedulers: [RoundRobinScheduler; MAX_CPUS],
    online: [bool; MAX_CPUS],
    current_cpu: CpuId,
}

impl Default for SmpScheduler {
    fn default() -> Self {
        let mut online = [false; MAX_CPUS];
        online[0] = true;
        Self {
            schedulers: core::array::from_fn(|_| RoundRobinScheduler::default()),
            online,
            current_cpu: CpuId(0),
        }
    }
}

impl SmpScheduler {
    fn check_cpu(cpu: CpuId) -> Result<usize, ()> {
        if cpu.0 >= MAX_CPUS {
            return Err(());
        }
        Ok(cpu.0)
    }

    pub fn current_cpu(&self) -> CpuId {
        self.current_cpu
    }

    pub fn set_current_cpu(&mut self, cpu: CpuId) -> Result<(), ()> {
        let idx = Self::check_cpu(cpu)?;
        if !self.online[idx] {
            return Err(());
        }
        self.current_cpu = cpu;
        Ok(())
    }

    pub fn bring_up_cpu(&mut self, cpu: CpuId) -> Result<(), ()> {
        let idx = Self::check_cpu(cpu)?;
        self.online[idx] = true;
        Ok(())
    }

    pub fn cpu_is_online(&self, cpu: CpuId) -> bool {
        Self::check_cpu(cpu)
            .ok()
            .map(|idx| self.online[idx])
            .unwrap_or(false)
    }

    pub fn online_cpu_count(&self) -> usize {
        self.online.iter().filter(|online| **online).count()
    }

    pub fn enqueue_on(&mut self, cpu: CpuId, tid: u64) -> Result<(), u64> {
        let idx = Self::check_cpu(cpu).map_err(|_| tid)?;
        if !self.online[idx] {
            return Err(tid);
        }
        self.schedulers[idx].enqueue(tid)
    }

    pub fn dispatch_next_on(&mut self, cpu: CpuId) -> Option<u64> {
        let idx = Self::check_cpu(cpu).ok()?;
        if !self.online[idx] {
            return None;
        }
        self.schedulers[idx].dispatch_next()
    }

    pub fn on_preempt_on(&mut self, cpu: CpuId) -> Option<u64> {
        let idx = Self::check_cpu(cpu).ok()?;
        if !self.online[idx] {
            return None;
        }
        self.schedulers[idx].on_preempt()
    }

    pub fn block_current_on(&mut self, cpu: CpuId) -> Option<u64> {
        let idx = Self::check_cpu(cpu).ok()?;
        if !self.online[idx] {
            return None;
        }
        self.schedulers[idx].block_current()
    }

    pub fn current_tid_on(&self, cpu: CpuId) -> Option<u64> {
        let idx = Self::check_cpu(cpu).ok()?;
        if !self.online[idx] {
            return None;
        }
        self.schedulers[idx].current_tid()
    }

    pub fn runnable_count_on(&self, cpu: CpuId) -> usize {
        let idx = match Self::check_cpu(cpu) {
            Ok(idx) => idx,
            Err(_) => return 0,
        };
        if !self.online[idx] {
            return 0;
        }
        self.schedulers[idx].runnable_count()
    }

    // Compatibility helpers for existing single-core call sites (operate on current_cpu).
    pub fn enqueue(&mut self, tid: u64) -> Result<(), u64> {
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
        let mut sched = RoundRobinScheduler::default();
        assert!(sched.enqueue(1).is_ok());
        assert!(sched.enqueue(2).is_ok());

        assert_eq!(sched.dispatch_next().expect("task 1"), 1);
        assert_eq!(sched.on_preempt().expect("task 2"), 2);
        assert_eq!(sched.on_preempt().expect("task 1"), 1);
    }

    #[test]
    fn smp_scheduler_tracks_per_cpu_queues() {
        let mut sched = SmpScheduler::default();
        assert_eq!(sched.online_cpu_count(), 1);
        assert!(sched.bring_up_cpu(CpuId(1)).is_ok());
        assert_eq!(sched.online_cpu_count(), 2);

        assert!(sched.enqueue_on(CpuId(0), 10).is_ok());
        assert!(sched.enqueue_on(CpuId(1), 20).is_ok());

        assert_eq!(sched.dispatch_next_on(CpuId(0)), Some(10));
        assert_eq!(sched.dispatch_next_on(CpuId(1)), Some(20));
        assert_eq!(sched.current_tid_on(CpuId(0)), Some(10));
        assert_eq!(sched.current_tid_on(CpuId(1)), Some(20));
    }
}
