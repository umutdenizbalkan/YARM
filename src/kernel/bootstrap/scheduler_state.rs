use super::{KernelError, KernelState, MAX_CROSS_CPU_WORK, map_scheduler_error};
use crate::kernel::ipc::ThreadId;
use crate::kernel::scheduler::{CpuId, TaskPriority};
use crate::kernel::smp::WorkItem;
use crate::kernel::task::{TaskClass, TaskStatus};

impl KernelState {
    pub fn bring_up_cpu(&mut self, cpu: CpuId) -> Result<(), KernelError> {
        self.scheduler
            .bring_up_cpu(cpu)
            .map_err(map_scheduler_error)
    }

    pub fn set_current_cpu(&mut self, cpu: CpuId) -> Result<(), KernelError> {
        self.scheduler
            .validate_online_cpu(cpu)
            .map_err(map_scheduler_error)?;
        self.current_cpu = cpu;
        Ok(())
    }

    pub fn current_cpu(&self) -> CpuId {
        self.current_cpu
    }

    pub fn current_tid(&self) -> Option<u64> {
        self.scheduler
            .current_tid_on(self.current_cpu)
            .map(|tid| tid.0)
    }

    pub fn dispatch_next_current_cpu(&mut self) -> Option<u64> {
        self.scheduler
            .dispatch_next_on(self.current_cpu)
            .map(|tid| tid.0)
    }

    pub fn on_preempt_current_cpu(&mut self) -> Option<u64> {
        self.scheduler
            .on_preempt_on(self.current_cpu)
            .map(|tid| tid.0)
    }

    pub fn block_current_cpu(&mut self) -> Option<u64> {
        self.scheduler
            .block_current_on(self.current_cpu)
            .map(|tid| tid.0)
    }

    pub fn enqueue_current_cpu(&mut self, tid: u64) -> Result<(), KernelError> {
        self.enqueue_on_cpu(self.current_cpu, tid)
    }

    pub fn online_cpu_count(&self) -> usize {
        self.scheduler.online_cpu_count()
    }

    pub fn present_cpu_count(&self) -> usize {
        self.scheduler.present_cpu_count()
    }

    pub fn present_cpu_bitmap(&self) -> u64 {
        self.scheduler.present_cpu_bitmap()
    }

    pub fn online_cpu_bitmap(&self) -> u64 {
        self.scheduler.online_cpu_bitmap()
    }

    fn task_priority(&self, tid: u64) -> Result<TaskPriority, KernelError> {
        if tid == 0 {
            return Ok(TaskPriority::Normal);
        }
        let class = self
            .tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| tcb.class)
            .ok_or(KernelError::TaskMissing)?;
        Ok(match class {
            TaskClass::SystemServer => TaskPriority::High,
            TaskClass::Driver | TaskClass::App => TaskPriority::Normal,
        })
    }

    pub(crate) fn enqueue_task(&mut self, tid: u64) -> Result<CpuId, KernelError> {
        let priority = self.task_priority(tid)?;
        self.scheduler
            .enqueue_balanced(ThreadId(tid), priority)
            .map_err(map_scheduler_error)
    }

    pub fn enqueue_on_cpu(&mut self, cpu: CpuId, tid: u64) -> Result<(), KernelError> {
        let priority = self.task_priority(tid)?;
        self.scheduler
            .enqueue_on_with_priority(cpu, ThreadId(tid), priority)
            .map_err(map_scheduler_error)
    }

    pub fn submit_cross_cpu_work(&self, item: WorkItem) -> Result<(), KernelError> {
        self.ipc
            .cross_cpu_work
            .submit(item)
            .map_err(|_| KernelError::TaskTableFull)
    }

    pub fn drain_cross_cpu_work(&self) -> Option<WorkItem> {
        self.ipc.cross_cpu_work.take()
    }

    pub fn tlb_shootdown_count(&self) -> u64 {
        self.tlb_shootdown_count
    }

    fn apply_cross_cpu_work(&mut self, item: WorkItem) -> Result<(), KernelError> {
        match item {
            WorkItem::Reschedule { target_cpu } => {
                if self.current_cpu == target_cpu {
                    self.yield_current()?;
                }
                Ok(())
            }
            WorkItem::TlbShootdown {
                target_cpu, asid, ..
            } => {
                self.tlb_shootdown_count = self.tlb_shootdown_count.wrapping_add(1);
                if self.current_cpu == target_cpu && self.user_spaces.retired_entry(asid).is_some()
                {
                    let cpu_bit = 1u64 << target_cpu.0;
                    self.user_spaces
                        .acknowledge_shootdown(asid, cpu_bit)
                        .map_err(KernelError::Vm)?;
                }
                Ok(())
            }
            WorkItem::WakeTask { target_cpu, tid } => {
                let tcb = self.tcb_mut(tid.0).ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Runnable;
                self.enqueue_on_cpu(target_cpu, tid.0)
            }
        }
    }

    pub fn process_cross_cpu_work_for_cpu(&mut self, cpu: CpuId) -> Result<usize, KernelError> {
        let mut deferred = [None; MAX_CROSS_CPU_WORK];
        let mut deferred_len = 0usize;
        let mut processed = 0usize;

        while let Some(item) = self.ipc.cross_cpu_work.take() {
            if item.target_cpu() == cpu {
                self.apply_cross_cpu_work(item)?;
                processed += 1;
            } else if deferred_len < MAX_CROSS_CPU_WORK {
                deferred[deferred_len] = Some(item);
                deferred_len += 1;
            }
        }

        for item in deferred.into_iter().flatten().take(deferred_len) {
            self.ipc
                .cross_cpu_work
                .submit(item)
                .map_err(|_| KernelError::TaskTableFull)?;
        }

        Ok(processed)
    }
}
