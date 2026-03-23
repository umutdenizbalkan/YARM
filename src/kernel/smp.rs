use super::ipc::ThreadId;
use super::lock::SpinLockIrq;
use super::scheduler::{CpuId, MAX_CPUS};
use super::vm::{Asid, VirtAddr};

pub const MAX_CROSS_CPU_WORK: usize = 64;
const _: () = assert!(MAX_CROSS_CPU_WORK.is_power_of_two());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmpError {
    InvalidCpu,
    QueueFull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkItem {
    Reschedule,
    TlbShootdown {
        asid: Asid,
        va_range: Option<(VirtAddr, VirtAddr)>,
    },
    WakeTask {
        tid: ThreadId,
    },
}

#[derive(Debug)]
struct WorkQueue {
    // Prototype note: head/count already encode occupancy, so this could become
    // `MaybeUninit<WorkItem>` later if queue footprint becomes material.
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

    fn push(&mut self, item: WorkItem) -> Result<(), SmpError> {
        if self.count >= MAX_CROSS_CPU_WORK {
            return Err(SmpError::QueueFull);
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
    pub fn submit(&self, item: WorkItem) -> Result<(), SmpError> {
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

#[derive(Debug)]
pub struct SmpMailbox {
    inboxes: [CrossCpuWorkQueue; MAX_CPUS],
}

impl Default for SmpMailbox {
    fn default() -> Self {
        Self {
            inboxes: core::array::from_fn(|_| CrossCpuWorkQueue::default()),
        }
    }
}

impl SmpMailbox {
    fn inbox(&self, cpu: CpuId) -> Result<&CrossCpuWorkQueue, SmpError> {
        self.inboxes.get(cpu.0 as usize).ok_or(SmpError::InvalidCpu)
    }

    pub fn send_to(&self, cpu: CpuId, item: WorkItem) -> Result<(), SmpError> {
        self.inbox(cpu)?.submit(item)
    }

    pub fn take_for_cpu(&self, cpu: CpuId) -> Result<Option<WorkItem>, SmpError> {
        Ok(self.inbox(cpu)?.take())
    }

    pub fn drain_current<F: FnMut(WorkItem)>(&self, cpu: CpuId, mut f: F) -> Result<(), SmpError> {
        while let Some(item) = self.take_for_cpu(cpu)? {
            f(item);
        }
        Ok(())
    }

    #[cfg(debug_assertions)]
    pub fn pending_for_cpu(&self, cpu: CpuId) -> Result<usize, SmpError> {
        Ok(self.inbox(cpu)?.pending())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_cpu_queue_is_fifo() {
        let queue = CrossCpuWorkQueue::default();
        queue.submit(WorkItem::Reschedule).expect("queue 1");
        queue
            .submit(WorkItem::WakeTask { tid: ThreadId(44) })
            .expect("queue 2");

        #[cfg(debug_assertions)]
        assert_eq!(queue.pending(), 2);
        assert_eq!(queue.take(), Some(WorkItem::Reschedule));
        assert_eq!(queue.take(), Some(WorkItem::WakeTask { tid: ThreadId(44) }));
        assert_eq!(queue.take(), None);
    }

    #[test]
    fn submit_returns_typed_err_when_full() {
        let queue = CrossCpuWorkQueue::default();
        for i in 0..MAX_CROSS_CPU_WORK {
            queue
                .submit(WorkItem::WakeTask {
                    tid: ThreadId(i as u64),
                })
                .expect("fill queue");
        }

        assert_eq!(queue.submit(WorkItem::Reschedule), Err(SmpError::QueueFull));
    }

    #[test]
    fn queue_wraparound_preserves_order() {
        let queue = CrossCpuWorkQueue::default();

        for i in 0..MAX_CROSS_CPU_WORK {
            queue
                .submit(WorkItem::WakeTask {
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
                    tid: ThreadId(i as u64),
                })
                .expect("wrap submit");
        }

        for i in (MAX_CROSS_CPU_WORK / 2)..(MAX_CROSS_CPU_WORK + MAX_CROSS_CPU_WORK / 2) {
            assert_eq!(
                queue.take(),
                Some(WorkItem::WakeTask {
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
            asid: Asid(7),
            va_range: Some((VirtAddr(0x1000), VirtAddr(0x2000))),
        };
        queue.submit(item).expect("submit");
        assert_eq!(queue.take(), Some(item));
    }

    #[test]
    fn mailbox_routes_work_to_target_cpu_inbox() {
        let mailbox = SmpMailbox::default();
        mailbox
            .send_to(CpuId(1), WorkItem::Reschedule)
            .expect("cpu1");
        mailbox
            .send_to(CpuId(2), WorkItem::WakeTask { tid: ThreadId(55) })
            .expect("cpu2");

        assert_eq!(mailbox.take_for_cpu(CpuId(0)), Ok(None));
        assert_eq!(
            mailbox.take_for_cpu(CpuId(1)),
            Ok(Some(WorkItem::Reschedule))
        );
        assert_eq!(
            mailbox.take_for_cpu(CpuId(2)),
            Ok(Some(WorkItem::WakeTask { tid: ThreadId(55) }))
        );
    }

    #[test]
    fn mailbox_rejects_invalid_cpu() {
        let mailbox = SmpMailbox::default();
        assert_eq!(
            mailbox.send_to(CpuId(MAX_CPUS as u8), WorkItem::Reschedule),
            Err(SmpError::InvalidCpu)
        );
    }
}
