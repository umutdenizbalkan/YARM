// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState, kernel_mut, kernel_ref, map_scheduler_error};
use crate::arch::hal::Hal;
use crate::kernel::ipc::Message;
use crate::kernel::ipc::ThreadId;
use crate::kernel::scheduler::{CpuId, TaskPriority};
use crate::kernel::smp::{SmpError, WorkItem};
use crate::kernel::task::{TaskClass, TaskStatus};
use crate::kernel::time::Tick;

fn map_smp_error(err: SmpError) -> KernelError {
    match err {
        SmpError::InvalidCpu => KernelError::VmFull,
        SmpError::QueueFull => KernelError::TaskTableFull,
    }
}

const BOOTSTRAP_FIRST_USER_TID: u64 = 1;
const DEBUG_DISPATCH_CONTEXT_LOG: bool = false;

impl KernelState {
    pub fn bring_up_cpu(&mut self, cpu: CpuId) -> Result<(), KernelError> {
        self.with_scheduler_state_mut(|sched| {
            kernel_mut(&mut sched.scheduler)
                .bring_up_cpu(cpu)
                .map_err(map_scheduler_error)
        })?;
        crate::arch::cpu_mapping::register_cpu_mapping(cpu);
        Ok(())
    }

    pub fn set_current_cpu(&mut self, cpu: CpuId) -> Result<(), KernelError> {
        self.with_scheduler_state_mut(|sched| {
            kernel_ref(&sched.scheduler)
                .validate_online_cpu(cpu)
                .map_err(map_scheduler_error)?;
            sched.current_cpu = cpu;
            Ok(())
        })?;
        Ok(())
    }

    pub fn current_cpu(&self) -> CpuId {
        #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
        {
            let mpidr = crate::arch::aarch64::read_mpidr_el1();
            return CpuId((mpidr & 0xff) as u8);
        }
        #[cfg(any(feature = "hosted-dev", not(target_arch = "aarch64")))]
        {
            self.with_scheduler_state(|sched| sched.current_cpu)
        }
    }

    pub fn current_tid(&self) -> Option<u64> {
        let cpu = self.current_cpu();
        self.with_scheduler_state(|sched| {
            kernel_ref(&sched.scheduler)
                .current_tid_on(cpu)
                .map(|tid| tid.0)
        })
    }

    pub fn current_tid_on_cpu(&self, cpu: CpuId) -> Option<u64> {
        self.with_scheduler_state(|sched| {
            kernel_ref(&sched.scheduler)
                .current_tid_on(cpu)
                .map(|tid| tid.0)
        })
    }

    pub fn dispatch_next_current_cpu(&mut self) -> Option<u64> {
        let cpu = self.current_cpu();
        let mut sched = self.scheduler_state();
        let next = kernel_mut(&mut sched.scheduler)
            .dispatch_next_on(cpu)
            .map(|tid| tid.0);
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!("DISPATCH_NEXT cpu={} result_tid={:?}", cpu.0, next);
        }
        next
    }

    pub fn on_preempt_current_cpu(&mut self) -> Option<u64> {
        let cpu = self.current_cpu();
        let mut sched = self.scheduler_state();
        kernel_mut(&mut sched.scheduler)
            .on_preempt_on(cpu)
            .map(|tid| tid.0)
    }

    /// Preempt the current task on the current CPU, preferring `preferred` as the
    /// next task.  Returns the TID of the new current task (which is `preferred`
    /// when it was runnable, or the FIFO head otherwise).
    pub(crate) fn on_preempt_prefer_current_cpu(&mut self, preferred: u64) -> Option<u64> {
        let cpu = self.current_cpu();
        let mut sched = self.scheduler_state();
        kernel_mut(&mut sched.scheduler)
            .on_preempt_prefer_on(cpu, ThreadId(preferred))
            .map(|tid| tid.0)
    }

    pub fn block_current_cpu(&mut self) -> Option<u64> {
        let cpu = self.current_cpu();
        let mut sched = self.scheduler_state();
        kernel_mut(&mut sched.scheduler)
            .block_current_on(cpu)
            .map(|tid| tid.0)
    }

    pub fn enqueue_current_cpu(&mut self, tid: u64) -> Result<(), KernelError> {
        self.enqueue_on_cpu(self.current_cpu(), tid)
    }

    /// Re-enqueue the idle task (TID 0) on CPU 0 after `dispatch_next_task` displaced it.
    ///
    /// In hosted-dev tests `dispatch_next_task` removes TID 0 from the scheduler's
    /// `current` slot when a real task becomes runnable.  Call this immediately after
    /// every `dispatch_next_task` so subsequent `yield_current` calls have TID 0 in the
    /// membership table and can re-enqueue it without an `AlreadyQueued` error.
    ///
    /// See `doc/KERNEL_TEST_RULES.md §Rule 2 — Idle re-enqueue`.
    #[cfg(test)]
    pub fn idle_re_enqueue_for_test(&mut self) -> Result<(), KernelError> {
        self.enqueue_on_cpu(CpuId(0), 0)
    }

    /// Return the number of tasks waiting in the run-queue of `cpu` (excludes the
    /// currently running task).  Zero when `cpu` is offline or out of range.
    #[allow(dead_code)]
    pub(crate) fn runnable_count_on_cpu(&self, cpu: CpuId) -> usize {
        self.with_scheduler_state(|sched| kernel_ref(&sched.scheduler).runnable_count_on(cpu))
    }

    /// Inspect TCB status and return the wake plan without mutating any state.
    ///
    /// Returns `SchedulerWakePlan::Wake(tid)` when the task is in a state that
    /// requires a scheduler wake (i.e. Blocked or Running-but-needs-requeue).
    /// Returns `SchedulerWakePlan::None` when the task is already Runnable, is the
    /// current task, or is not found.
    ///
    /// Usage: call under a domain lock to compute the plan, then call
    /// `apply_scheduler_wake_plan` after releasing the lock.
    #[allow(dead_code)]
    pub(crate) fn compute_wake_plan_for_tid(&self, tid: crate::kernel::ipc::ThreadId) -> super::SchedulerWakePlan {
        let status = match self.task_status(tid.0) {
            Some(s) => s,
            None => return super::SchedulerWakePlan::None,
        };
        match status {
            TaskStatus::Blocked(_) => super::SchedulerWakePlan::Wake(tid),
            TaskStatus::Running if self.current_tid() != Some(tid.0) => super::SchedulerWakePlan::Wake(tid),
            _ => super::SchedulerWakePlan::None,
        }
    }

    pub fn online_cpu_count(&self) -> usize {
        self.with_scheduler_state(|sched| kernel_ref(&sched.scheduler).online_cpu_count())
    }

    pub fn present_cpu_count(&self) -> usize {
        let sched = self.scheduler_state();
        kernel_ref(&sched.scheduler).present_cpu_count()
    }

    pub fn present_cpu_bitmap(&self) -> u64 {
        let sched = self.scheduler_state();
        kernel_ref(&sched.scheduler).present_cpu_bitmap()
    }

    pub fn online_cpu_bitmap(&self) -> u64 {
        let sched = self.scheduler_state();
        kernel_ref(&sched.scheduler).online_cpu_bitmap()
    }

    pub fn program_timer_deadline_current_cpu(&mut self, ticks_from_now: u64) {
        let cpu = self.current_cpu();
        self.hal.program_timer_deadline(cpu, ticks_from_now);
    }

    pub(crate) fn tick_scheduler_timer(&mut self) -> (Tick, bool) {
        let mut sched = self.scheduler_state();
        sched.timer.tick_and_check()
    }

    fn task_priority(&self, tid: u64) -> Result<TaskPriority, KernelError> {
        if tid == 0 {
            return Ok(TaskPriority::Normal);
        }
        let class = self.task_class(tid).ok_or(KernelError::TaskMissing)?;
        Ok(match class {
            TaskClass::SystemServer => TaskPriority::High,
            TaskClass::Driver | TaskClass::App => TaskPriority::Normal,
        })
    }

    fn task_cpu_affinity(&self, tid: u64) -> Result<Option<CpuId>, KernelError> {
        if tid == 0 {
            return Ok(None);
        }
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.cpu_affinity)
                .ok_or(KernelError::TaskMissing)
        })
    }

    fn ensure_driver_affinity(&mut self, tid: u64) -> Result<(), KernelError> {
        if tid == 0 {
            return Ok(());
        }
        let current_cpu = self.current_cpu();
        let class = self.task_class(tid).ok_or(KernelError::TaskMissing)?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            if class == TaskClass::Driver && tcb.cpu_affinity.is_none() {
                tcb.cpu_affinity = Some(current_cpu);
            }
            Ok(())
        })
    }

    pub(crate) fn enqueue_task(&mut self, tid: u64) -> Result<CpuId, KernelError> {
        self.ensure_driver_affinity(tid)?;
        let priority = self.task_priority(tid)?;
        let mut sched = self.scheduler_state();
        if let Some(cpu) = self.task_cpu_affinity(tid)? {
            kernel_mut(&mut sched.scheduler)
                .enqueue_on_with_priority(cpu, ThreadId(tid), priority)
                .map_err(map_scheduler_error)?;
            if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                crate::yarm_log!("ENQUEUE cpu={} tid={} status=Runnable", cpu.0, tid);
            }
            Ok(cpu)
        } else {
            let cpu = kernel_mut(&mut sched.scheduler)
                .enqueue_balanced(ThreadId(tid), priority)
                .map_err(map_scheduler_error)?;
            if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                crate::yarm_log!("ENQUEUE cpu={} tid={} status=Runnable", cpu.0, tid);
            }
            Ok(cpu)
        }
    }

    pub(crate) fn enqueue_woken_task(
        &mut self,
        tid: u64,
    ) -> Result<(CpuId, &'static str), KernelError> {
        if let Some(cpu) = self.task_cpu_affinity(tid)? {
            self.enqueue_on_cpu(cpu, tid)?;
            return Ok((cpu, "pinned"));
        }
        let cpu = self.current_cpu();
        self.enqueue_on_cpu(cpu, tid)?;
        Ok((cpu, "current_cpu"))
    }

    pub fn enqueue_on_cpu(&mut self, cpu: CpuId, tid: u64) -> Result<(), KernelError> {
        let priority = self.task_priority(tid)?;
        let current_cpu = self.current_cpu();
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!(
                "ENQUEUE_CALL cpu_current={} cpu_target={} tid={}",
                current_cpu.0,
                cpu.0,
                tid
            );
        }
        if tid == BOOTSTRAP_FIRST_USER_TID {
            if cpu.0 != crate::arch::platform_constants::BOOTSTRAP_CPU_ID {
                if cfg!(not(feature = "hosted-dev")) {
                    crate::yarm_log!(
                        "FIRST_USER_PIN_VIOLATION cpu={} tid={} chosen_cpu={}",
                        current_cpu.0,
                        tid,
                        cpu.0
                    );
                    assert_eq!(cpu.0, crate::arch::platform_constants::BOOTSTRAP_CPU_ID);
                    assert_eq!(
                        cpu.0 as usize,
                        crate::arch::platform_constants::BOOTSTRAP_CPU_ID as usize
                    );
                }
            }
        }
        let mut sched = self.scheduler_state();
        kernel_mut(&mut sched.scheduler)
            .enqueue_on_with_priority(cpu, ThreadId(tid), priority)
            .map_err(map_scheduler_error)?;
        if tid == BOOTSTRAP_FIRST_USER_TID && cfg!(not(feature = "hosted-dev")) {
            let queue0 = kernel_ref(&sched.scheduler).runnable_count_on(CpuId(0));
            let queue1 = kernel_ref(&sched.scheduler).runnable_count_on(CpuId(1));
            let queue2 = kernel_ref(&sched.scheduler).runnable_count_on(CpuId(2));
            let queue3 = kernel_ref(&sched.scheduler).runnable_count_on(CpuId(3));
            crate::yarm_log!(
                "BOOTSTRAP_ENQUEUE_VERIFY tid=1 queue0_len={} queue1_len={} queue2_len={} queue3_len={}",
                queue0,
                queue1,
                queue2,
                queue3
            );
        }
        Ok(())
    }

    pub fn submit_cross_cpu_work(&self, cpu: CpuId, item: WorkItem) -> Result<(), KernelError> {
        self.with_ipc_state(|ipc| ipc.cross_cpu_work.send_to(cpu, item))
            .map_err(map_smp_error)
    }

    pub fn drain_cross_cpu_work(&self) -> Result<Option<WorkItem>, KernelError> {
        self.with_ipc_state(|ipc| ipc.cross_cpu_work.take_for_cpu(self.current_cpu()))
            .map_err(map_smp_error)
    }

    pub fn tlb_shootdown_count(&self) -> u64 {
        self.with_telemetry_state(|telemetry| telemetry.tlb_shootdown_count)
    }

    pub fn tlb_shootdown_timeout_count(&self) -> u64 {
        self.with_telemetry_state(|telemetry| telemetry.tlb_shootdown_timeout_count)
    }

    fn escalate_tlb_shootdown_timeout(&mut self, timed_out: usize) -> Result<(), KernelError> {
        let Some(endpoint_idx) = self.with_fault_state(|faults| faults.supervisor_endpoint) else {
            return Ok(());
        };
        let mut payload = [0u8; 16];
        payload[..8].copy_from_slice(&(timed_out as u64).to_le_bytes());
        payload[8..16].copy_from_slice(&(self.current_cpu().0 as u64).to_le_bytes());
        let msg = Message::new(0, &payload).map_err(|_| KernelError::WrongObject)?;
        // send_message_to_endpoint_and_wake enqueues under ipc_state_lock
        // (rank 3) and wakes outside the lock (task lock rank 2 < ipc rank 3).
        self.send_message_to_endpoint_and_wake(endpoint_idx, msg)
    }

    fn apply_cross_cpu_work(&mut self, cpu: CpuId, item: WorkItem) -> Result<(), KernelError> {
        match item {
            WorkItem::Reschedule => {
                if self.current_cpu() == cpu {
                    self.yield_current()?;
                }
                Ok(())
            }
            WorkItem::TlbShootdown {
                asid,
                va_range,
                requester,
                sequence,
            } => {
                self.with_telemetry_state_mut(|telemetry| {
                    telemetry.tlb_shootdown_count = telemetry.tlb_shootdown_count.wrapping_add(1);
                });
                let retired = self.with_user_spaces(|spaces| spaces.retired_entry(asid).is_some());
                if self.current_cpu() == cpu {
                    if let Some((start, end)) = va_range {
                        let mut va = start.0;
                        while va < end.0 {
                            crate::arch::selected_isa::page_table::invalidate_page(
                                crate::kernel::vm::VirtAddr(va),
                            );
                            va = va.saturating_add(crate::kernel::vm::PAGE_SIZE as u64);
                        }
                    } else {
                        crate::arch::selected_isa::page_table::invalidate_asid(asid);
                    }
                    if let Some(requester_cpu) = requester {
                        // Ordering note: ACK is queued only after local
                        // invalidation has been executed on this CPU.
                        self.submit_cross_cpu_work(
                            requester_cpu,
                            WorkItem::TlbShootdownAck {
                                sequence,
                                from_cpu: cpu,
                            },
                        )?;
                    }
                    if retired {
                        let cpu_bit = 1u64 << cpu.0;
                        self.with_user_spaces_mut(|spaces| {
                            spaces
                                .acknowledge_shootdown(asid, cpu_bit)
                                .map_err(KernelError::Vm)
                        })?;
                    }
                }
                Ok(())
            }
            WorkItem::TlbShootdownAck { sequence, from_cpu } => {
                if self.current_cpu() != cpu {
                    return Ok(());
                }
                self.with_ipc_state_mut(|ipc| {
                    let Some(wait) = ipc.live_tlb_shootdown.active.as_mut() else {
                        return;
                    };
                    if wait.requester_cpu != cpu || wait.sequence != sequence {
                        return;
                    }
                    let from_bit = 1u64 << from_cpu.0;
                    wait.pending_cpu_bitmap &= !from_bit;
                });
                Ok(())
            }
            WorkItem::WakeTask { tid } => {
                self.with_tcbs_mut(|tcbs| {
                    let tcb = tcbs
                        .iter_mut()
                        .flatten()
                        .find(|tcb| tcb.tid.0 == tid.0)
                        .ok_or(KernelError::TaskMissing)?;
                    tcb.status = TaskStatus::Runnable;
                    Ok::<_, KernelError>(())
                })?;
                self.enqueue_on_cpu(cpu, tid.0)
            }
        }
    }

    pub fn process_cross_cpu_work_for_cpu(&mut self, cpu: CpuId) -> Result<usize, KernelError> {
        let mut processed = 0usize;

        // Take one work item at a time under ipc_state_lock, then release the
        // lock before calling apply_cross_cpu_work, which may itself acquire
        // ipc_state_lock (e.g. TlbShootdownAck path). Matches the drain_cross_cpu_work
        // pattern that already uses with_ipc_state for this field.
        while let Some(item) = self
            .with_ipc_state(|ipc| ipc.cross_cpu_work.take_for_cpu(cpu))
            .map_err(map_smp_error)?
        {
            self.apply_cross_cpu_work(cpu, item)?;
            processed += 1;
        }

        let timed_out = self.with_user_spaces_mut(|spaces| spaces.tick_retired_shootdowns());
        if timed_out > 0 {
            self.with_telemetry_state_mut(|telemetry| {
                telemetry.tlb_shootdown_timeout_count = telemetry
                    .tlb_shootdown_timeout_count
                    .wrapping_add(timed_out as u64);
            });
            self.escalate_tlb_shootdown_timeout(timed_out)?;
        }

        Ok(processed)
    }
}
