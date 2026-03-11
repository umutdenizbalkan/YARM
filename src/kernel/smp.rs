use super::ipc::ThreadId;
use super::lock::SpinLock;
use super::scheduler::CpuId;
use super::vm::Asid;

pub const MAX_CROSS_CPU_WORK: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkItem {
    Reschedule { target_cpu: CpuId },
    TlbShootdown { target_cpu: CpuId, asid: Asid },
    WakeTask { target_cpu: CpuId, tid: ThreadId },
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
            ring: [const { None }; MAX_CROSS_CPU_WORK],
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, item: WorkItem) -> Result<(), WorkItem> {
        if self.len >= MAX_CROSS_CPU_WORK {
            return Err(item);
        }
        let tail = (self.head + self.len) & (MAX_CROSS_CPU_WORK - 1);
        self.ring[tail] = Some(item);
        self.len += 1;
        Ok(())
    }

    fn pop(&mut self) -> Option<WorkItem> {
        if self.len == 0 {
            return None;
        }
        let idx = self.head;
        self.head = (self.head + 1) & (MAX_CROSS_CPU_WORK - 1);
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
    /// Submits cross-CPU work into the shared ring.
    ///
    /// On overflow the original item is returned to the caller so it can retry,
    /// log, or escalate rather than silently dropping work.
    pub fn submit(&self, item: WorkItem) -> Result<(), WorkItem> {
        self.inner.lock().push(item)
    }

    pub fn take(&self) -> Option<WorkItem> {
        self.inner.lock().pop()
    }

    /// Returns the instantaneous queued-item count.
    ///
    /// This value is for diagnostics/telemetry only: producers/consumers may
    /// change the queue immediately after it is observed.
    pub fn pending(&self) -> usize {
        self.inner.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::vm::Asid;

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
                tid: ThreadId(44),
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
                tid: ThreadId(44)
            })
        );
        assert_eq!(queue.take(), None);
    }

    #[test]
    fn submit_returns_err_when_full() {
        let queue = CrossCpuWorkQueue::default();
        for i in 0..MAX_CROSS_CPU_WORK {
            queue
                .submit(WorkItem::WakeTask {
                    target_cpu: CpuId(0),
                    tid: ThreadId(i as u64),
                })
                .expect("fill queue");
        }

        let overflow = WorkItem::Reschedule {
            target_cpu: CpuId(1),
        };
        assert_eq!(queue.submit(overflow), Err(overflow));
    }

    #[test]
    fn pending_and_empty_take_behave() {
        let queue = CrossCpuWorkQueue::default();
        assert_eq!(queue.pending(), 0);
        assert_eq!(queue.take(), None);

        queue
            .submit(WorkItem::TlbShootdown {
                target_cpu: CpuId(0),
                asid: Asid(7),
            })
            .expect("submit");
        assert_eq!(queue.pending(), 1);
        assert!(matches!(
            queue.take(),
            Some(WorkItem::TlbShootdown {
                target_cpu: CpuId(0),
                asid: Asid(7)
            })
        ));
        assert_eq!(queue.pending(), 0);
    }

    #[test]
    fn queue_wraparound_preserves_order() {
        let queue = CrossCpuWorkQueue::default();

        for i in 0..MAX_CROSS_CPU_WORK {
            queue
                .submit(WorkItem::WakeTask {
                    target_cpu: CpuId(0),
                    tid: ThreadId(i as u64),
                })
                .expect("prime");
        }
        for _ in 0..(MAX_CROSS_CPU_WORK / 2) {
            let _ = queue.take();
        }
        for i in MAX_CROSS_CPU_WORK..(MAX_CROSS_CPU_WORK + MAX_CROSS_CPU_WORK / 2) {
            queue
                .submit(WorkItem::WakeTask {
                    target_cpu: CpuId(0),
                    tid: ThreadId(i as u64),
                })
                .expect("wrap submit");
        }

        for i in (MAX_CROSS_CPU_WORK / 2)..(MAX_CROSS_CPU_WORK + MAX_CROSS_CPU_WORK / 2) {
            assert_eq!(
                queue.take(),
                Some(WorkItem::WakeTask {
                    target_cpu: CpuId(0),
                    tid: ThreadId(i as u64)
                })
            );
        }
        assert_eq!(queue.take(), None);
    }
}
