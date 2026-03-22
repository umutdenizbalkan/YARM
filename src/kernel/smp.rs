use super::ipc::ThreadId;
use super::lock::SpinLockIrq;
use super::scheduler::CpuId;
use super::vm::{Asid, VirtAddr};

pub const MAX_CROSS_CPU_WORK: usize = 64;
const _: () = assert!(MAX_CROSS_CPU_WORK.is_power_of_two());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkItem {
    Reschedule {
        target_cpu: CpuId,
    },
    TlbShootdown {
        target_cpu: CpuId,
        asid: Asid,
        va_range: Option<(VirtAddr, VirtAddr)>,
    },
    WakeTask {
        target_cpu: CpuId,
        tid: ThreadId,
    },
}

impl WorkItem {
    pub const fn target_cpu(&self) -> CpuId {
        match self {
            Self::Reschedule { target_cpu }
            | Self::TlbShootdown { target_cpu, .. }
            | Self::WakeTask { target_cpu, .. } => *target_cpu,
        }
    }
}

#[derive(Debug)]
struct WorkQueue {
    ring: [Option<WorkItem>; MAX_CROSS_CPU_WORK],
    head: usize,
    count: usize,
}

impl WorkQueue {
    const fn new() -> Self {
        Self {
            ring: [const { None }; MAX_CROSS_CPU_WORK],
            head: 0,
            count: 0,
        }
    }

    fn push(&mut self, item: WorkItem) -> Result<(), WorkItem> {
        if self.count >= MAX_CROSS_CPU_WORK {
            return Err(item);
        }
        let tail = (self.head + self.count) & (MAX_CROSS_CPU_WORK - 1);
        self.ring[tail] = Some(item);
        self.count += 1;
        Ok(())
    }

    fn pop(&mut self) -> Option<WorkItem> {
        if self.count == 0 {
            return None;
        }
        let idx = self.head;
        self.head = (self.head + 1) & (MAX_CROSS_CPU_WORK - 1);
        self.count -= 1;
        self.ring[idx].take()
    }

    #[cfg(debug_assertions)]
    fn item_count(&self) -> usize {
        self.count
    }
}

#[derive(Debug)]
pub struct CrossCpuWorkQueue {
    inner: SpinLockIrq<WorkQueue>,
}

impl Default for CrossCpuWorkQueue {
    fn default() -> Self {
        Self {
            inner: SpinLockIrq::new(WorkQueue::new()),
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

    #[cfg(debug_assertions)]
    pub fn pending(&self) -> usize {
        self.inner.lock().item_count()
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
                tid: ThreadId(44),
            })
            .expect("queue 2");

        #[cfg(debug_assertions)]
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

    #[test]
    fn tlb_shootdown_carries_optional_range() {
        let queue = CrossCpuWorkQueue::default();
        let item = WorkItem::TlbShootdown {
            target_cpu: CpuId(0),
            asid: Asid(7),
            va_range: Some((VirtAddr(0x1000), VirtAddr(0x2000))),
        };
        queue.submit(item).expect("submit");
        assert_eq!(queue.take(), Some(item));
    }
}
