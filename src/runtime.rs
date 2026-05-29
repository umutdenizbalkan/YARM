// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::{
    kernel_mut, kernel_ref, BootConfigSubsystem, FaultSubsystem, KernelCapacityProfile,
    KernelError, KernelState, KernelStorage, RuntimeCapacityConfig, SchedulerState,
    TrapHandleError,
};
use crate::kernel::capabilities::CapId;
use crate::kernel::ipc::Message;
#[cfg(test)]
use crate::kernel::lock::SpinLockGuard;
use crate::kernel::lock::{SpinLock, SpinLockIrq};
use crate::kernel::scheduler::CpuId;
use crate::kernel::trap::{FaultInfo, Trap};
use crate::kernel::trapframe::TrapFrame;

#[derive(Debug)]
pub struct SharedKernel {
    state: SpinLock<KernelState>,
    scheduler_state: *const SpinLockIrq<SchedulerState>,
    boot_config_state_lock: *const SpinLockIrq<()>,
    boot_config: *const KernelStorage<BootConfigSubsystem>,
}

impl SharedKernel {
    pub fn new(state: KernelState) -> Self {
        let scheduler_state = state.scheduler_state_lock_ptr();
        let (boot_config_state_lock, boot_config) = state.boot_config_split_read_ptrs();
        Self {
            state: SpinLock::new(state),
            scheduler_state,
            boot_config_state_lock,
            boot_config,
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

    pub fn current_tid_split_read(&self, cpu: CpuId) -> Option<u64> {
        // Phase L5A split: read the scheduler's per-CPU current TID directly
        // under the scheduler lock.  This intentionally avoids the global
        // SharedKernel lock and does not mutate current_cpu or task state.
        // SAFETY: `scheduler_state` points at the scheduler lock embedded in the
        // same `KernelState` owned by `self.state`; the storage is stable for
        // the `SharedKernel` lifetime.
        let scheduler_state = unsafe { &*self.scheduler_state };
        let sched = scheduler_state.lock();
        kernel_ref(&sched.scheduler)
            .current_tid_on(cpu)
            .map(|tid| tid.0)
    }

    pub fn online_cpu_count_split_read(&self) -> usize {
        // Phase L7A split: read scheduler topology through scheduler_state only.
        // This is a read-only staged helper; it does not acquire the global
        // SharedKernel lock, mutate runqueues, or update current_cpu.
        // SAFETY: `scheduler_state` points at the scheduler lock embedded in the
        // same `KernelState` owned by `self.state`; the storage is stable for
        // the `SharedKernel` lifetime.
        let scheduler_state = unsafe { &*self.scheduler_state };
        let sched = scheduler_state.lock();
        kernel_ref(&sched.scheduler).online_cpu_count()
    }

    pub fn present_cpu_count_split_read(&self) -> usize {
        // Phase L7A split: read scheduler topology through scheduler_state only.
        // This is a read-only staged helper; it does not acquire the global
        // SharedKernel lock, mutate runqueues, or update current_cpu.
        // SAFETY: `scheduler_state` points at the scheduler lock embedded in the
        // same `KernelState` owned by `self.state`; the storage is stable for
        // the `SharedKernel` lifetime.
        let scheduler_state = unsafe { &*self.scheduler_state };
        let sched = scheduler_state.lock();
        kernel_ref(&sched.scheduler).present_cpu_count()
    }

    pub fn capacity_profile_split_read(&self) -> KernelCapacityProfile {
        // Phase L8B split: read immutable boot configuration under only the
        // boot_config lock domain. This intentionally avoids the global
        // SharedKernel lock and does not mutate boot config or runtime state.
        // SAFETY: these pointers refer to the boot_config lock and storage
        // embedded in the same `KernelState` owned by `self.state`; that storage
        // is stable for the `SharedKernel` lifetime.
        let boot_config_state_lock = unsafe { &*self.boot_config_state_lock };
        let _guard = boot_config_state_lock.lock();
        let boot_config = unsafe { &*self.boot_config };
        kernel_ref(boot_config).capacity_profile
    }

    pub fn runtime_capacity_config_split_read(&self) -> RuntimeCapacityConfig {
        let profile = self.capacity_profile_split_read();
        KernelState::runtime_capacity_config_for_profile(profile)
    }

    fn with_fault_split_mut<R>(&self, f: impl FnOnce(&mut FaultSubsystem) -> R) -> R {
        // Stage 3B-A helper-only split mutation: use only fault_state_lock and
        // mutate only diagnostic fault bookkeeping. Do not acquire the outer
        // SharedKernel lock and do not touch current_cpu or other subsystems.
        // SAFETY: `state.data_ptr()` is the stable KernelState storage owned by
        // this SharedKernel. `fault_split_mut_ptrs_from_raw` derives raw field
        // pointers without creating a whole-KernelState reference; the fault
        // lock serializes access to the fault subsystem storage.
        let (fault_state_lock, faults) =
            unsafe { KernelState::fault_split_mut_ptrs_from_raw(self.state.data_ptr()) };
        let fault_state_lock = unsafe { &*fault_state_lock };
        let _guard = fault_state_lock.lock();
        let faults = unsafe { &mut *faults };
        f(kernel_mut(faults))
    }

    pub fn record_fault_split_mut(&self, fault: FaultInfo) {
        self.with_fault_split_mut(|faults| faults.last_fault = Some(fault));
    }

    pub fn record_fault_frame_snapshot_split_mut(&self, frame: &TrapFrame) {
        self.with_fault_split_mut(|faults| faults.last_fault_frame = Some(frame.clone()));
    }

    pub fn clear_last_fault_split_mut(&self) {
        self.with_fault_split_mut(|faults| {
            faults.last_fault = None;
            faults.last_fault_frame = None;
        });
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

    /// Borrow `&mut KernelState` directly, bypassing the `SpinLock`.
    ///
    /// This exists solely for AArch64 boot code that must pass `&mut KernelState`
    /// to a callback that eventually calls `yarm_aarch64_enter_user_mode_eret -> !`.
    /// Holding the `SpinLock` across an ERET that never returns would leave
    /// `held = true` permanently, deadlocking all subsequent trap handlers.
    ///
    /// # Safety
    /// * Must only be called during single-CPU boot before any trap handler can
    ///   concurrently call `SharedKernel::with` or `with_cpu`.
    /// * The returned reference must not be used after the ERET to user space;
    ///   from that point all KernelState access must go through `with` / `with_cpu`.
    /// * `TRAP_KERNEL_STATE_PTR` must remain null while this reference is live so
    ///   that the trap fallback path cannot also yield `&mut KernelState`.
    #[cfg(not(feature = "hosted-dev"))]
    pub(crate) unsafe fn borrow_kernel_for_boot(&self) -> &mut KernelState {
        // SAFETY: delegated to caller (see doc comment above).
        unsafe { &mut *self.state.data_ptr() }
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
    fn current_tid_split_read_matches_scheduler_current_on_cpu() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state.register_task(42).expect("task42");
            state.enqueue_current_cpu(42).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            assert_eq!(state.current_tid_on_cpu(CpuId(0)), Some(42));
        });

        assert_eq!(kernel.current_tid_split_read(CpuId(0)), Some(42));
        assert_eq!(kernel.current_tid_split_read(CpuId(7)), None);
    }

    #[test]
    fn topology_count_split_reads_match_scheduler_state() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let (online, present) =
            kernel.with(|state| (state.online_cpu_count(), state.present_cpu_count()));

        assert_eq!(kernel.online_cpu_count_split_read(), online);
        assert_eq!(kernel.present_cpu_count_split_read(), present);
        assert!(kernel.online_cpu_count_split_read() <= kernel.present_cpu_count_split_read());
    }

    #[test]
    fn boot_config_split_reads_match_kernel_state_capacity_config() {
        let kernel = SharedKernel::new(
            Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained)
                .expect("init constrained"),
        );
        let (profile, config) =
            kernel.with(|state| (state.capacity_profile(), state.runtime_capacity_config()));

        assert_eq!(kernel.capacity_profile_split_read(), profile);
        assert_eq!(kernel.runtime_capacity_config_split_read(), config);
    }

    #[test]
    fn fault_bookkeeping_split_mut_helpers_match_kernel_state_accessors() {
        use crate::kernel::trap::FaultAccess;
        use crate::kernel::vm::VirtAddr;

        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let fault = FaultInfo {
            addr: VirtAddr(0xdead_beef),
            access: FaultAccess::Write,
        };
        let mut frame = TrapFrame::new(11, [1, 2, 3, 4, 5, 6]);
        frame.set_saved_pc(0x4000);
        frame.set_saved_sp(0x8000);

        kernel.record_fault_split_mut(fault);
        assert_eq!(kernel.with(|state| state.last_fault()), Some(fault));

        kernel.record_fault_frame_snapshot_split_mut(&frame);
        assert_eq!(
            kernel.with(|state| state.last_fault_frame()),
            Some(frame.clone())
        );

        kernel.clear_last_fault_split_mut();
        assert_eq!(kernel.with(|state| state.last_fault()), None);
        assert_eq!(kernel.with(|state| state.last_fault_frame()), None);

        kernel.with(|state| {
            state.record_fault(fault);
            state.record_fault_frame_snapshot(&frame);
            assert_eq!(state.last_fault(), Some(fault));
            assert_eq!(state.last_fault_frame(), Some(frame.clone()));
            state.clear_last_fault();
            assert_eq!(state.last_fault(), None);
            assert_eq!(state.last_fault_frame(), None);
        });
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
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        for _ in 0..32 {
            kernel.with(|state| {
                state
                    .submit_cross_cpu_work(CpuId(0), WorkItem::Reschedule)
                    .expect("submit t1");
            });
        }
        for _ in 0..32 {
            kernel.with(|state| {
                state
                    .submit_cross_cpu_work(CpuId(0), WorkItem::Reschedule)
                    .expect("submit t2");
            });
        }

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
