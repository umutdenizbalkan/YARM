// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::{KernelError, KernelState, TrapHandleError};
use crate::kernel::capabilities::CapId;
use crate::kernel::ipc::Message;
use crate::kernel::lock::{SpinLock, SpinLockIrq};
#[cfg(test)]
use crate::kernel::lock::SpinLockGuard;
use crate::kernel::boot::SchedulerState;
use crate::kernel::trap::Trap;
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::scheduler::CpuId;

#[derive(Debug)]
pub struct SharedKernel {
    state: SpinLock<KernelState>,
    scheduler_state: *const SpinLockIrq<SchedulerState>,
}

impl SharedKernel {
    pub fn new(state: KernelState) -> Self {
        let scheduler_state = state.scheduler_state_lock_ptr();
        Self {
            state: SpinLock::new(state),
            scheduler_state,
        }
    }

    #[cfg(test)]
    pub fn lock(&self) -> SpinLockGuard<'_, KernelState> {
        self.state.lock()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut KernelState) -> R) -> R {
        let mut guard = self.state.lock();
        f(&mut guard)
    }

    pub fn with_cpu<R>(
        &self,
        cpu: CpuId,
        f: impl FnOnce(&mut KernelState) -> R,
    ) -> Result<R, KernelError> {
        let mut guard = self.state.lock();
        guard.set_current_cpu(cpu)?;
        Ok(f(&mut guard))
    }


    pub fn scheduler_tick_now_split_read(&self) -> u64 {
        // Stage 2B split: read scheduler tick directly under scheduler lock.
        crate::yarm_log!("YARM_LOCK_SPLIT_STAGE2B path=scheduler_tick_now_split_read");
        // SAFETY: `scheduler_state` points at the scheduler lock embedded in the
        // same `KernelState` owned by `self.state`; the storage is stable for
        // the `SharedKernel` lifetime.
        let scheduler_state = unsafe { &*self.scheduler_state };
        let sched = scheduler_state.lock();
        sched.timer.current_ticks().0
    }


    pub fn ipc_recv_with_deadline_split_bridge(
        &self,
        recv_cap: CapId,
        timeout_ticks: u64,
    ) -> Result<Option<Message>, KernelError> {
        crate::yarm_log!("YARM_LOCK_SPLIT_STAGE2D path=ipc_recv_timeout_deadline_bridge");
        if timeout_ticks == 0 {
            return self.with(|state| state.try_ipc_recv(recv_cap));
        }
        let now = self.scheduler_tick_now_split_read();
        let deadline = now.wrapping_add(timeout_ticks);
        self.with(|state| state.ipc_recv_until_deadline(recv_cap, deadline))
    }


    pub fn handle_trap_with_cpu(
        &self,
        cpu: CpuId,
        trap: Trap,
        frame: Option<&mut TrapFrame>,
    ) -> Result<(), TrapHandleError> {
        let result = self
            .with_cpu(cpu, |kernel| kernel.handle_trap(trap, frame))
            .map_err(|err| TrapHandleError::Syscall(err.into()))?;
        result
    }

    pub fn control_plane_set_process_cnode_slots_via_syscall(
        &self,
        target_pid: u64,
        slot_capacity: usize,
    ) -> Result<(), TrapHandleError> {
        self.with(|state| {
            state.control_plane_set_process_cnode_slots_via_syscall(target_pid, slot_capacity)
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::scheduler::CpuId;
    use crate::kernel::smp::WorkItem;
    use crate::kernel::task::TaskClass;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn shared_kernel_serializes_access() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));

        kernel.with(|state| {
            state
                .submit_cross_cpu_work(CpuId(0), WorkItem::Reschedule)
                .expect("submit");
        });

        let processed = kernel.with(|state| {
            state
                .process_cross_cpu_work_for_cpu(CpuId(0))
                .expect("process")
        });

        assert_eq!(processed, 1);
    }

    #[test]
    fn with_cpu_applies_targeted_cross_cpu_work_before_closure() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state.bring_up_cpu(CpuId(1)).expect("cpu1");
            state.register_task(2).expect("task2");
            state
                .submit_cross_cpu_work(CpuId(1), WorkItem::WakeTask { tid: ThreadId(2) })
                .expect("submit");
        });

        let processed = kernel
            .with_cpu(CpuId(1), |state| {
                assert_eq!(state.current_cpu(), CpuId(1));
                state
                    .process_cross_cpu_work_for_cpu(CpuId(1))
                    .expect("drain")
            })
            .expect("with_cpu");
        assert_eq!(processed, 1);
    }

    #[test]
    fn with_cpu_propagates_invalid_cpu_errors() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let result = kernel.with_cpu(CpuId(1), |_| 0);
        assert!(result.is_err());
    }

    #[test]
    fn shared_kernel_allows_concurrent_serialized_access() {
        let kernel: Arc<SharedKernel> =
            Arc::new(SharedKernel::new(Bootstrap::init().expect("init")));
        let k1 = Arc::clone(&kernel);
        let k2 = Arc::clone(&kernel);

        let t1 = thread::spawn(move || {
            for _ in 0..32 {
                k1.with(|state| {
                    state
                        .submit_cross_cpu_work(CpuId(0), WorkItem::Reschedule)
                        .expect("submit t1");
                });
            }
        });

        let t2 = thread::spawn(move || {
            for _ in 0..32 {
                k2.with(|state| {
                    state
                        .submit_cross_cpu_work(CpuId(0), WorkItem::Reschedule)
                        .expect("submit t2");
                });
            }
        });

        t1.join().expect("join t1");
        t2.join().expect("join t2");

        let drained =
            kernel.with(|state| state.process_cross_cpu_work_for_cpu(CpuId(1)).unwrap_or(0));
        assert_eq!(drained, 0);

        let drained_cpu0 = kernel.with(|state| {
            state
                .process_cross_cpu_work_for_cpu(CpuId(0))
                .expect("drain cpu0")
        });
        assert_eq!(drained_cpu0, 64);
    }

    #[test]
    fn shared_kernel_control_plane_syscall_wrapper_resizes_target_cnode() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state
                .register_task_with_class(900, TaskClass::SystemServer)
                .expect("system server");
            state
                .register_task_with_class(901, TaskClass::App)
                .expect("target app");
            state.enqueue_current_cpu(900).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            if state.current_tid() != Some(900) {
                state.yield_current().expect("switch");
            }
        });

        let (target_cnode, before) = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(901).expect("target cnode");
            let before = state.cnode_slot_capacity(cnode).expect("before");
            (cnode, before)
        });
        let requested = before.saturating_add(4);
        kernel
            .control_plane_set_process_cnode_slots_via_syscall(901, requested)
            .expect("resize");
        let after = kernel.with(|state| state.cnode_slot_capacity(target_cnode));
        assert_eq!(after, Some(requested));
    }

    #[test]
    fn shared_kernel_control_plane_syscall_wrapper_denies_unprivileged_cross_process_resize() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state
                .register_task_with_class(910, TaskClass::App)
                .expect("requester");
            state
                .register_task_with_class(911, TaskClass::App)
                .expect("target");
            state.enqueue_current_cpu(910).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            if state.current_tid() != Some(910) {
                state.yield_current().expect("switch");
            }
        });

        let err = kernel
            .control_plane_set_process_cnode_slots_via_syscall(911, 8)
            .expect_err("must deny");
        assert_eq!(
            err,
            TrapHandleError::Syscall(crate::kernel::syscall::SyscallError::MissingRight)
        );
    }
}
