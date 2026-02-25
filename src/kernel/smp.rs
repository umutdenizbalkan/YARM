use super::lock::SpinLock;
use super::scheduler::CpuId;

pub const MAX_CROSS_CPU_WORK: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkItem {
    Reschedule { target_cpu: CpuId },
    TlbShootdown { target_cpu: CpuId, asid: u16 },
    WakeTask { target_cpu: CpuId, tid: u64 },
}

#[derive(Debug)]
struct WorkQueue {
    ring: [Option<WorkItem>; MAX_CROSS_CPU_WORK],
    head: usize,
    len: usize,
}

impl WorkQueue {
    const fn new() -> Self {
        Self {
            ring: [None; MAX_CROSS_CPU_WORK],
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, item: WorkItem) -> Result<(), WorkItem> {
        if self.len >= MAX_CROSS_CPU_WORK {
            return Err(item);
        }
        let tail = (self.head + self.len) % MAX_CROSS_CPU_WORK;
        self.ring[tail] = Some(item);
        self.len += 1;
        Ok(())
    }

    fn pop(&mut self) -> Option<WorkItem> {
        if self.len == 0 {
            return None;
        }
        let idx = self.head;
        self.head = (self.head + 1) % MAX_CROSS_CPU_WORK;
        self.len -= 1;
        self.ring[idx].take()
    }

    fn len(&self) -> usize {
        self.len
    }
}

#[derive(Debug)]
pub struct CrossCpuWorkQueue {
    inner: SpinLock<WorkQueue>,
}

impl Default for CrossCpuWorkQueue {
    fn default() -> Self {
        Self {
            inner: SpinLock::new(WorkQueue::new()),
        }
    }
}

impl CrossCpuWorkQueue {
    pub fn submit(&self, item: WorkItem) -> Result<(), WorkItem> {
        self.inner.lock().push(item)
    }

    pub fn take(&self) -> Option<WorkItem> {
        self.inner.lock().pop()
    }

    pub fn pending(&self) -> usize {
        self.inner.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_cpu_queue_is_fifo() {
        let queue = CrossCpuWorkQueue::default();
        queue
            .submit(WorkItem::Reschedule {
                target_cpu: CpuId(1),
            })
            .expect("queue 1");
        queue
            .submit(WorkItem::WakeTask {
                target_cpu: CpuId(2),
                tid: 44,
            })
            .expect("queue 2");

        assert_eq!(queue.pending(), 2);
        assert_eq!(
            queue.take(),
            Some(WorkItem::Reschedule {
                target_cpu: CpuId(1)
            })
        );
        assert_eq!(
            queue.take(),
            Some(WorkItem::WakeTask {
                target_cpu: CpuId(2),
                tid: 44
            })
        );
        assert_eq!(queue.take(), None);
    }
}
