// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::{
    BootConfigSubsystem, FaultSubsystem, KernelCapacityProfile, KernelError, KernelState,
    KernelStorage, RuntimeCapacityConfig, SchedulerState, TelemetrySubsystem, TrapHandleError,
    kernel_mut, kernel_ref,
};
use crate::kernel::capabilities::CapId;
use crate::kernel::ipc::Message;
use crate::kernel::task::FaultPolicy;
#[cfg(test)]
use crate::kernel::lock::SpinLockGuard;
use crate::kernel::lock::{SpinLock, SpinLockIrq};
use crate::kernel::scheduler::CpuId;
use crate::kernel::trap::{FaultInfo, Trap};
use crate::kernel::trapframe::TrapFrame;

/// Pre-read snapshot of diagnostic data for the fatal-trap log path.
///
/// Populated by `SharedKernel::fatal_trap_read_snapshot` using only sub-global
/// split-read locks (scheduler rank 1, task rank 2). Used by the x86_64
/// shared-kernel trap path to log fatal trap diagnostics without acquiring the
/// global `SharedKernel` lock.
#[derive(Debug, Clone, Copy)]
pub struct FatalTrapReadSnapshot {
    pub current_tid: u64,
    pub current_asid: u64,
}

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

    fn with_telemetry_split_mut<R>(&self, f: impl FnOnce(&mut TelemetrySubsystem) -> R) -> R {
        // Stage 3C-B helper-only split mutation: use only telemetry_state_lock
        // and mutate only simple diagnostic telemetry counters. Do not acquire
        // the outer SharedKernel lock and do not touch current_cpu, scheduler,
        // IPC, VM, task, capability, driver, fault, or boot-config state.
        // SAFETY: `state.data_ptr()` is the stable KernelState storage owned by
        // this SharedKernel. `telemetry_split_mut_ptrs_from_raw` derives raw
        // field pointers without creating a whole-KernelState reference; the
        // telemetry lock serializes access to telemetry storage.
        let (telemetry_state_lock, telemetry) =
            unsafe { KernelState::telemetry_split_mut_ptrs_from_raw(self.state.data_ptr()) };
        let telemetry_state_lock = unsafe { &*telemetry_state_lock };
        let _guard = telemetry_state_lock.lock();
        let telemetry = unsafe { &mut *telemetry };
        f(kernel_mut(telemetry))
    }

    pub fn increment_tlb_shootdown_count_split_mut(&self) {
        self.with_telemetry_split_mut(|telemetry| {
            telemetry.tlb_shootdown_count = telemetry.tlb_shootdown_count.wrapping_add(1);
        });
    }

    pub fn add_tlb_shootdown_timeout_count_split_mut(&self, delta: u64) {
        self.with_telemetry_split_mut(|telemetry| {
            telemetry.tlb_shootdown_timeout_count =
                telemetry.tlb_shootdown_timeout_count.wrapping_add(delta);
        });
    }

    fn with_fault_split_read<R>(&self, f: impl FnOnce(&FaultSubsystem) -> R) -> R {
        // Stage 4T+5 split-read: acquires fault_state_lock (rank 8) only.
        // Does not acquire the outer SharedKernel lock. Does not mutate any state.
        // Callers must not hold any lock of rank ≤ 8 (scheduler/task/ipc/cap/vm/
        // memory/driver) when invoking this helper.
        // SAFETY: `fault_split_mut_ptrs_from_raw` derives raw field pointers from
        // the stable KernelState storage owned by this SharedKernel without creating
        // a whole-KernelState reference. The fault lock serializes access; the *mut
        // pointer is downgraded to *const for this read-only use.
        let (fault_state_lock, faults) =
            unsafe { KernelState::fault_split_mut_ptrs_from_raw(self.state.data_ptr()) };
        let fault_state_lock = unsafe { &*fault_state_lock };
        let _guard = fault_state_lock.lock();
        let faults: &KernelStorage<FaultSubsystem> = unsafe { &*(faults as *const _) };
        f(kernel_ref(faults))
    }

    pub fn last_fault_split_read(&self) -> Option<crate::kernel::trap::FaultInfo> {
        // Stage 4T+5 split-read: reads last_fault under fault_state_lock (rank 8).
        // Does not acquire the outer SharedKernel lock.
        self.with_fault_split_read(|faults| faults.last_fault)
    }

    pub fn last_fault_frame_split_read(&self) -> Option<crate::kernel::trapframe::TrapFrame> {
        // Stage 4T+5 split-read: reads last_fault_frame under fault_state_lock (rank 8).
        // Does not acquire the outer SharedKernel lock.
        self.with_fault_split_read(|faults| faults.last_fault_frame.clone())
    }

    pub fn fault_policy_split_read(&self) -> FaultPolicy {
        // Stage 4T+5 split-read: reads fault_policy under fault_state_lock (rank 8).
        // Does not acquire the outer SharedKernel lock.
        self.with_fault_split_read(|faults| faults.fault_policy)
    }

    fn with_telemetry_split_read<R>(&self, f: impl FnOnce(&TelemetrySubsystem) -> R) -> R {
        // Stage 4T+5 split-read: acquires telemetry_state_lock (rank 10) only.
        // Does not acquire the outer SharedKernel lock. Does not mutate any state.
        // Callers must not hold any lock of rank ≤ 10 when invoking this helper.
        // SAFETY: `telemetry_split_mut_ptrs_from_raw` derives raw field pointers
        // from the stable KernelState storage owned by this SharedKernel without
        // creating a whole-KernelState reference. The telemetry lock serializes
        // access; the *mut pointer is downgraded to *const for read-only use.
        let (telemetry_state_lock, telemetry) =
            unsafe { KernelState::telemetry_split_mut_ptrs_from_raw(self.state.data_ptr()) };
        let telemetry_state_lock = unsafe { &*telemetry_state_lock };
        let _guard = telemetry_state_lock.lock();
        let telemetry: &KernelStorage<TelemetrySubsystem> = unsafe { &*(telemetry as *const _) };
        f(kernel_ref(telemetry))
    }

    pub fn tlb_shootdown_count_split_read(&self) -> u64 {
        // Stage 4T+5 split-read: reads tlb_shootdown_count under telemetry_state_lock (rank 10).
        // Does not acquire the outer SharedKernel lock.
        self.with_telemetry_split_read(|telemetry| telemetry.tlb_shootdown_count)
    }

    pub fn tlb_shootdown_timeout_count_split_read(&self) -> u64 {
        // Stage 4T+5 split-read: reads tlb_shootdown_timeout_count under telemetry_state_lock (rank 10).
        // Does not acquire the outer SharedKernel lock.
        self.with_telemetry_split_read(|telemetry| telemetry.tlb_shootdown_timeout_count)
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

    pub fn task_asid_for_tid_split_read(&self, tid: u64) -> u64 {
        // Stage 4T+7 split-read: acquires task_state_lock (rank 2) only.
        // Does not acquire the outer SharedKernel lock. Does not mutate any state.
        // SAFETY: `state.data_ptr()` is the stable KernelState storage owned by
        // this SharedKernel. `task_asid_for_tid_from_raw` derives raw field pointers
        // without creating a whole-KernelState reference; the task lock serializes
        // access to the TCB array.
        unsafe {
            KernelState::task_asid_for_tid_from_raw(self.state.data_ptr() as *const _, tid)
        }
    }

    pub fn fatal_trap_read_snapshot(&self, cpu: CpuId) -> FatalTrapReadSnapshot {
        // Stage 4T+7 split-read: pre-read diagnostic data for the fatal-trap log.
        // Acquires scheduler lock (rank 1) for current_tid, then task lock (rank 2)
        // for ASID — each held transiently and released before the next is acquired.
        // Does not acquire the outer SharedKernel lock.
        let current_tid = self.current_tid_split_read(cpu).unwrap_or(0);
        let current_asid = if current_tid != 0 {
            self.task_asid_for_tid_split_read(current_tid)
        } else {
            0
        };
        FatalTrapReadSnapshot { current_tid, current_asid }
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
    fn telemetry_split_mut_helpers_match_kernel_state_accessors() {
        std::thread::Builder::new()
            .name("telemetry_split_mut_helpers".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
                let (initial_shootdowns, initial_timeouts) = kernel.with(|state| {
                    (
                        state.tlb_shootdown_count(),
                        state.tlb_shootdown_timeout_count(),
                    )
                });

                kernel.increment_tlb_shootdown_count_split_mut();
                assert_eq!(
                    kernel.with(|state| state.tlb_shootdown_count()),
                    initial_shootdowns.wrapping_add(1)
                );

                kernel.add_tlb_shootdown_timeout_count_split_mut(7);
                assert_eq!(
                    kernel.with(|state| state.tlb_shootdown_timeout_count()),
                    initial_timeouts.wrapping_add(7)
                );
            })
            .expect("spawn test thread")
            .join()
            .expect("join test thread");
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

    // ── Stage 4T+5 split-read helpers ─────────────────────────────────────────

    #[test]
    fn fault_split_read_helpers_match_kernel_state_accessors() {
        use crate::kernel::trap::{FaultAccess, FaultInfo};
        use crate::kernel::vm::VirtAddr;

        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));

        // Initially no fault recorded.
        assert_eq!(kernel.last_fault_split_read(), None);
        assert_eq!(kernel.last_fault_frame_split_read(), None);

        let fault = FaultInfo { addr: VirtAddr(0xDEAD_0000), access: FaultAccess::Write };
        kernel.record_fault_split_mut(fault);

        // Split-read must match the global-lock read.
        assert_eq!(
            kernel.last_fault_split_read(),
            kernel.with(|state| state.last_fault()),
            "last_fault_split_read must match kernel.with last_fault after record"
        );
        assert_eq!(kernel.last_fault_split_read(), Some(fault));

        let mut frame = TrapFrame::new(11, [1, 2, 3, 4, 5, 6]);
        frame.set_saved_pc(0x6000);
        frame.set_saved_sp(0xA000);
        kernel.record_fault_frame_snapshot_split_mut(&frame);

        assert_eq!(
            kernel.last_fault_frame_split_read(),
            kernel.with(|state| state.last_fault_frame()),
            "last_fault_frame_split_read must match kernel.with last_fault_frame after snapshot"
        );
        assert!(kernel.last_fault_frame_split_read().is_some());

        // After clear: both split-read and global-lock read return None.
        kernel.clear_last_fault_split_mut();
        assert_eq!(kernel.last_fault_split_read(), None);
        assert_eq!(kernel.with(|state| state.last_fault()), None);
    }

    #[test]
    fn fault_policy_split_read_matches_kernel_state_accessor() {
        use crate::kernel::task::FaultPolicy;

        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let expected = kernel.with(|state| state.fault_policy());
        let split = kernel.fault_policy_split_read();
        assert_eq!(
            split, expected,
            "fault_policy_split_read must match kernel.with fault_policy"
        );
        // Default policy must be KillTask.
        assert_eq!(split, FaultPolicy::KillTask);
    }

    #[test]
    fn telemetry_split_read_helpers_match_kernel_state_accessors() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));

        let (count0, timeout0) = kernel.with(|state| {
            (state.tlb_shootdown_count(), state.tlb_shootdown_timeout_count())
        });

        // Initial values match.
        assert_eq!(kernel.tlb_shootdown_count_split_read(), count0);
        assert_eq!(kernel.tlb_shootdown_timeout_count_split_read(), timeout0);

        // After mutations via split_mut, split_read sees the updated values.
        kernel.increment_tlb_shootdown_count_split_mut();
        kernel.add_tlb_shootdown_timeout_count_split_mut(5);

        assert_eq!(kernel.tlb_shootdown_count_split_read(), count0.wrapping_add(1));
        assert_eq!(
            kernel.tlb_shootdown_timeout_count_split_read(),
            timeout0.wrapping_add(5)
        );

        // Split-read matches global-lock read.
        assert_eq!(
            kernel.tlb_shootdown_count_split_read(),
            kernel.with(|state| state.tlb_shootdown_count()),
            "tlb_shootdown_count split_read must match global read"
        );
        assert_eq!(
            kernel.tlb_shootdown_timeout_count_split_read(),
            kernel.with(|state| state.tlb_shootdown_timeout_count()),
            "tlb_shootdown_timeout_count split_read must match global read"
        );
    }

    // ── Stage 4T+6 x86_64 trap TID split-read equivalence tests ─────────────

    #[test]
    fn current_tid_split_read_matches_with_cpu_current_tid_entering_snapshot() {
        // Proves that current_tid_split_read(cpu) is equivalent to
        // with_cpu(cpu, |k| k.current_tid()).unwrap_or(None) at the
        // point in time that corresponds to the entering_tid snapshot
        // in the x86_64 shared trap path (Stage 4T+6 conversion).
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);

        kernel.with(|state| {
            state.register_task(77).expect("task77");
            state.enqueue_current_cpu(77).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
        });

        let split = kernel.current_tid_split_read(cpu);
        let conservative = kernel.with_cpu(cpu, |k| k.current_tid()).unwrap_or(None);
        assert_eq!(
            split, conservative,
            "entering_tid: current_tid_split_read must equal with_cpu current_tid"
        );
        assert_eq!(split, Some(77));
    }

    #[test]
    fn current_tid_split_read_reflects_task_switch_for_exiting_snapshot() {
        // Proves that current_tid_split_read(cpu) correctly reflects a task
        // switch — the exiting_tid snapshot in the x86_64 shared trap path
        // must see the newly-dispatched task, not the entering task.
        //
        // Setup: enqueue both 81 and 82 before dispatch so the runqueue has
        // [81, 82]. Dispatch picks 81; queue is [82]. Yield from 81 → queue
        // becomes [82, 81] → dispatch picks 82. This guarantees a switch.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);

        kernel.with(|state| {
            state.register_task(81).expect("task81");
            state.register_task(82).expect("task82");
            // Enqueue both before dispatch so 82 is waiting when 81 yields.
            state.enqueue_current_cpu(81).expect("enqueue 81");
            state.enqueue_current_cpu(82).expect("enqueue 82");
            state.dispatch_next_task().expect("dispatch to 81");
        });

        // Entering snapshot: current is task 81 (first FIFO pick).
        let entering_tid = kernel.current_tid_split_read(cpu);
        assert_eq!(entering_tid, Some(81), "entering_tid must be task 81");

        // Simulate task switch: yield task 81; queue now has [82, 81], dispatch picks 82.
        kernel.with(|state| {
            state.yield_current().expect("yield 81");
        });

        // Exiting snapshot: task 82 (or 81 re-dispatched on single-task edge case —
        // we assert only that the scheduler call is visible, not the exact TID).
        let exiting_tid = kernel.current_tid_split_read(cpu);
        assert_ne!(
            exiting_tid, entering_tid,
            "exiting_tid must differ from entering_tid after yield"
        );
        // task_switched detection — same logic as the x86_64 trap handler.
        let task_switched = entering_tid != exiting_tid;
        assert!(task_switched, "task_switched must be true when TIDs differ");
    }

    #[test]
    fn current_tid_split_read_no_switch_detection_for_same_task_return() {
        // Proves that when no task switch occurs, entering_tid == exiting_tid
        // via current_tid_split_read — triggering the "write trap returns only"
        // branch in the x86_64 trap handler (Stage 4T+6).
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);

        kernel.with(|state| {
            state.register_task(91).expect("task91");
            state.enqueue_current_cpu(91).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
        });

        let entering_tid = kernel.current_tid_split_read(cpu);

        // No dispatch between entering and exiting — same task continues.
        let exiting_tid = kernel.current_tid_split_read(cpu);

        assert_eq!(
            entering_tid, exiting_tid,
            "exiting_tid must equal entering_tid when no task switch"
        );
        let task_switched = entering_tid != exiting_tid;
        assert!(!task_switched, "task_switched must be false for same-task return");
    }

    #[test]
    fn current_tid_split_read_offline_cpu_returns_none() {
        // Proves that current_tid_split_read for an offline CPU returns None —
        // same as the former with_cpu path (validate_online_cpu fail → unwrap_or(None)).
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let offline_cpu = CpuId(7);

        let split = kernel.current_tid_split_read(offline_cpu);
        let conservative = kernel.with_cpu(offline_cpu, |k| k.current_tid()).unwrap_or(None);
        assert_eq!(
            split, None,
            "offline CPU must return None from current_tid_split_read"
        );
        assert_eq!(
            split, conservative,
            "split_read must match with_cpu for offline CPU"
        );
    }

    // ── Stage 4T+7 fatal-trap snapshot split-read tests ──────────────────────

    #[test]
    fn fatal_trap_read_snapshot_tid_matches_split_read() {
        // Proves that fatal_trap_read_snapshot.current_tid equals
        // current_tid_split_read(cpu).unwrap_or(0) for the same cpu at the
        // same scheduler state — validating the TID leg of Stage 4T+7.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);

        kernel.with(|state| {
            state.register_task(73).expect("task73");
            state.enqueue_current_cpu(73).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
        });

        let snapshot = kernel.fatal_trap_read_snapshot(cpu);
        let expected_tid = kernel.current_tid_split_read(cpu).unwrap_or(0);
        assert_eq!(
            snapshot.current_tid, expected_tid,
            "fatal_trap_read_snapshot.current_tid must equal current_tid_split_read"
        );
        assert_eq!(snapshot.current_tid, 73);
    }

    #[test]
    fn fatal_trap_read_snapshot_asid_matches_kernel_state_task_asid() {
        // Proves that fatal_trap_read_snapshot.current_asid equals
        // task_asid_for_tid_split_read(current_tid) — both return 0 for a task
        // without an ASID binding, validating the ASID leg of Stage 4T+7.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);

        kernel.with(|state| {
            state.register_task(74).expect("task74");
            state.enqueue_current_cpu(74).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
        });

        let snapshot = kernel.fatal_trap_read_snapshot(cpu);
        let asid_via_split = kernel.task_asid_for_tid_split_read(74);
        let asid_via_global = kernel.with(|state| state.task_asid(74).map(|a| a.0 as u64).unwrap_or(0));

        assert_eq!(
            snapshot.current_asid, asid_via_split,
            "snapshot.current_asid must match task_asid_for_tid_split_read"
        );
        assert_eq!(
            snapshot.current_asid, asid_via_global,
            "snapshot.current_asid must match global-lock task_asid"
        );
        // No ASID was bound, so both should be 0.
        assert_eq!(snapshot.current_asid, 0);
    }

    #[test]
    fn fatal_trap_read_snapshot_offline_cpu_returns_zeros() {
        // Proves that fatal_trap_read_snapshot for an offline CPU returns
        // current_tid=0 and current_asid=0 — the safe zero-fill sentinel used
        // by log_decoded_fatal_trap_from_snapshot when no task is running.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let offline_cpu = CpuId(255);

        let snapshot = kernel.fatal_trap_read_snapshot(offline_cpu);
        assert_eq!(
            snapshot.current_tid, 0,
            "offline CPU must produce current_tid=0 in fatal_trap_read_snapshot"
        );
        assert_eq!(
            snapshot.current_asid, 0,
            "offline CPU must produce current_asid=0 in fatal_trap_read_snapshot"
        );
    }
}
