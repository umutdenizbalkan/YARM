// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;
use core::sync::atomic::{AtomicBool, Ordering};

static WITH_TCBS_PROBE_ACTIVE: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_with_tcbs_probe(active: bool) {
    WITH_TCBS_PROBE_ACTIVE.store(active, Ordering::Release);
}

impl KernelState {
    #[inline]
    fn debug_lock_order_note(_domain: &'static str) {
        #[cfg(debug_assertions)]
        {
            // Stage-1 scaffolding only: keep this as a low-risk instrumentation hook
            // for future lock-order assertions without changing runtime behavior.
            let _ = _domain;
        }
    }

    /// Stage-1 alias for scheduler lock access.
    ///
    /// This intentionally forwards to existing behavior while giving callers a
    /// stable helper name for future lock-discipline migration.
    #[allow(dead_code)]
    pub(crate) fn with_scheduler<R>(&self, f: impl FnOnce(&SchedulerState) -> R) -> R {
        // Lock-order domain: scheduler
        Self::debug_lock_order_note("scheduler");
        self.with_scheduler_state(f)
    }

    pub(crate) fn scheduler_state(
        &self,
    ) -> crate::kernel::lock::SpinLockIrqGuard<'_, SchedulerState> {
        // Lock-order domain: scheduler
        Self::debug_lock_order_note("scheduler");
        self.scheduler_state.lock()
    }

    pub(crate) fn with_scheduler_state<R>(&self, f: impl FnOnce(&SchedulerState) -> R) -> R {
        // Lock-order domain: scheduler
        Self::debug_lock_order_note("scheduler");
        let sched = self.scheduler_state.lock();
        f(&sched)
    }

    pub(crate) fn with_scheduler_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut SchedulerState) -> R,
    ) -> R {
        // Lock-order domain: scheduler
        Self::debug_lock_order_note("scheduler");
        let mut sched = self.scheduler_state.lock();
        f(&mut sched)
    }

    #[cfg(test)]
    pub(crate) fn set_timer_for_test(&mut self, timer: Timer) {
        let mut sched = self.scheduler_state.lock();
        sched.timer = timer;
    }

    #[cfg(test)]
    pub(crate) fn runnable_count_on_for_test(&self, cpu: CpuId) -> usize {
        let sched = self.scheduler_state.lock();
        kernel_ref(&sched.scheduler).runnable_count_on(cpu)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn timer_ticks_for_test(&self) -> u64 {
        let sched = self.scheduler_state.lock();
        sched.timer.current_ticks().0
    }

    pub(crate) fn scheduler_tick_now(&self) -> u64 {
        let sched = self.scheduler_state.lock();
        sched.timer.current_ticks().0
    }

    #[cfg(feature = "posix-compat")]
    pub(crate) fn scheduler_tick_advance(&mut self) -> u64 {
        let mut sched = self.scheduler_state.lock();
        sched.timer.tick_and_check().0.0
    }

    pub(crate) fn with_ipc_state<R>(&self, f: impl FnOnce(&IpcSubsystem) -> R) -> R {
        // Lock-order domain: ipc
        Self::debug_lock_order_note("ipc");
        let _ipc_guard = self.ipc_state_lock.lock();
        f(kernel_ref(&self.ipc))
    }

    pub(crate) fn with_ipc_state_mut<R>(&mut self, f: impl FnOnce(&mut IpcSubsystem) -> R) -> R {
        // Lock-order domain: ipc
        Self::debug_lock_order_note("ipc");
        let _ipc_guard = self.ipc_state_lock.lock();
        f(kernel_mut(&mut self.ipc))
    }

    /// Stage-1 alias for task-state lock access.
    ///
    /// This intentionally forwards to existing behavior while giving callers a
    /// stable helper name for future lock-discipline migration.
    #[allow(dead_code)]
    pub(crate) fn with_task_state<R>(
        &self,
        f: impl FnOnce(&[Option<ThreadControlBlock>; MAX_TASKS]) -> R,
    ) -> R {
        // Lock-order domain: task
        Self::debug_lock_order_note("task");
        self.with_tcbs(f)
    }

    pub(crate) fn with_driver_state<R>(&self, f: impl FnOnce(&DriverSubsystem) -> R) -> R {
        // Lock-order domain: driver
        Self::debug_lock_order_note("driver");
        let _driver_guard = self.driver_state_lock.lock();
        f(kernel_ref(&self.drivers))
    }

    pub(crate) fn with_driver_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut DriverSubsystem) -> R,
    ) -> R {
        // Lock-order domain: driver
        Self::debug_lock_order_note("driver");
        let _driver_guard = self.driver_state_lock.lock();
        f(kernel_mut(&mut self.drivers))
    }

    pub(crate) fn with_fault_state<R>(&self, f: impl FnOnce(&FaultSubsystem) -> R) -> R {
        // Lock-order domain: fault
        Self::debug_lock_order_note("fault");
        let _fault_guard = self.fault_state_lock.lock();
        f(kernel_ref(&self.faults))
    }

    pub(crate) fn with_fault_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut FaultSubsystem) -> R,
    ) -> R {
        // Lock-order domain: fault
        Self::debug_lock_order_note("fault");
        let _fault_guard = self.fault_state_lock.lock();
        f(kernel_mut(&mut self.faults))
    }

    #[allow(dead_code)]
    pub(crate) fn with_restart_state<R>(&self, f: impl FnOnce(&RestartSubsystem) -> R) -> R {
        // Lock-order domain: restart
        Self::debug_lock_order_note("restart");
        let _restart_guard = self.restart_state_lock.lock();
        f(kernel_ref(&self.restart))
    }

    pub(crate) fn with_restart_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut RestartSubsystem) -> R,
    ) -> R {
        // Lock-order domain: restart
        Self::debug_lock_order_note("restart");
        let _restart_guard = self.restart_state_lock.lock();
        f(kernel_mut(&mut self.restart))
    }

    pub(crate) fn with_capability_state<R>(&self, f: impl FnOnce(&CapabilitySubsystem) -> R) -> R {
        // Lock-order domain: capability
        Self::debug_lock_order_note("capability");
        let _capability_guard = self.capability_state_lock.lock();
        f(&self.capability)
    }

    pub(crate) fn with_capability_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut CapabilitySubsystem) -> R,
    ) -> R {
        // Lock-order domain: capability
        Self::debug_lock_order_note("capability");
        let _capability_guard = self.capability_state_lock.lock();
        f(&mut self.capability)
    }

    pub(crate) fn with_telemetry_state<R>(&self, f: impl FnOnce(&TelemetrySubsystem) -> R) -> R {
        // Lock-order domain: telemetry
        Self::debug_lock_order_note("telemetry");
        let _telemetry_guard = self.telemetry_state_lock.lock();
        f(kernel_ref(&self.telemetry))
    }

    pub(crate) fn with_telemetry_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut TelemetrySubsystem) -> R,
    ) -> R {
        // Lock-order domain: telemetry
        Self::debug_lock_order_note("telemetry");
        let _telemetry_guard = self.telemetry_state_lock.lock();
        f(kernel_mut(&mut self.telemetry))
    }

    pub(crate) fn with_boot_config<R>(&self, f: impl FnOnce(&BootConfigSubsystem) -> R) -> R {
        // Lock-order domain: boot_config
        Self::debug_lock_order_note("boot_config");
        let _boot_config_guard = self.boot_config_state_lock.lock();
        f(kernel_ref(&self.boot_config))
    }

    #[allow(dead_code)]
    pub(crate) fn with_boot_config_mut<R>(
        &mut self,
        f: impl FnOnce(&mut BootConfigSubsystem) -> R,
    ) -> R {
        // Lock-order domain: boot_config
        Self::debug_lock_order_note("boot_config");
        let _boot_config_guard = self.boot_config_state_lock.lock();
        f(kernel_mut(&mut self.boot_config))
    }

    pub(crate) fn with_task_then_capability<R>(
        &self,
        f: impl FnOnce(&[Option<ThreadControlBlock>; MAX_TASKS], &CapabilitySubsystem) -> R,
    ) -> R {
        // Multi-lock helper order (must match doc/KERNEL_LOCKING.md):
        // task -> capability
        Self::debug_lock_order_note("task");
        let _task_guard = self.task_state_lock.lock();
        Self::debug_lock_order_note("capability");
        let _capability_guard = self.capability_state_lock.lock();
        f(kernel_ref(&self.tcbs), &self.capability)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_scheduler_then_ipc<R>(
        &self,
        f: impl FnOnce(&SchedulerState, &IpcSubsystem) -> R,
    ) -> R {
        // Multi-lock helper order (must match doc/KERNEL_LOCKING.md):
        // scheduler -> ipc
        Self::debug_lock_order_note("scheduler");
        let sched = self.scheduler_state.lock();
        Self::debug_lock_order_note("ipc");
        let _ipc_guard = self.ipc_state_lock.lock();
        f(&sched, kernel_ref(&self.ipc))
    }

    #[cfg(test)]
    pub(crate) fn lock_order_snapshot_for_test(&self) -> (u8, usize, u64) {
        self.with_scheduler_then_ipc(|sched, ipc| {
            (
                sched.current_cpu.0,
                kernel_ref(&sched.scheduler).online_cpu_count(),
                ipc.telemetry.scheduler_dispatch_calls,
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn lock_order_task_capability_snapshot_for_test(&self) -> (usize, usize) {
        self.with_task_then_capability(|tcbs, capability| {
            (
                tcbs.iter().flatten().count(),
                capability.process_cnodes.iter().flatten().count(),
            )
        })
    }

    pub(crate) fn with_user_spaces<R>(&self, f: impl FnOnce(&AddressSpaceManager) -> R) -> R {
        // Lock-order domain: vm
        Self::debug_lock_order_note("vm");
        let _vm_guard = self.vm_state_lock.lock();
        f(kernel_ref(&self.user_spaces))
    }

    pub(crate) fn with_user_spaces_mut<R>(
        &mut self,
        f: impl FnOnce(&mut AddressSpaceManager) -> R,
    ) -> R {
        // Lock-order domain: vm
        Self::debug_lock_order_note("vm");
        let _vm_guard = self.vm_state_lock.lock();
        f(kernel_mut(&mut self.user_spaces))
    }

    pub(crate) fn with_tcbs<R>(
        &self,
        f: impl FnOnce(&[Option<ThreadControlBlock>; MAX_TASKS]) -> R,
    ) -> R {
        // Lock-order domain: task
        Self::debug_lock_order_note("task");
        let _task_guard = self.task_state_lock.lock();
        #[cfg(not(feature = "hosted-dev"))]
        let probe_active = WITH_TCBS_PROBE_ACTIVE.load(Ordering::Acquire)
            && self.current_cpu().0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID;
        #[cfg(feature = "hosted-dev")]
        let probe_active = false;
        if probe_active {
            crate::yarm_log!(
                "WX2 after acquiring with_tcbs lock self_ptr=0x{:x} task_lock_ptr=0x{:x}",
                self as *const _ as usize,
                &self.task_state_lock as *const _ as usize
            );
        }
        let tcbs = kernel_ref(&self.tcbs);
        if probe_active {
            crate::yarm_log!(
                "WX3 after obtaining tcbs container pointer tcbs_ptr=0x{:x} tcbs_storage_ptr=0x{:x}",
                tcbs as *const _ as usize,
                &self.tcbs as *const _ as usize
            );
        }
        f(tcbs)
    }

    pub(crate) fn with_tcbs_mut<R>(
        &mut self,
        f: impl FnOnce(&mut [Option<ThreadControlBlock>; MAX_TASKS]) -> R,
    ) -> R {
        // Lock-order domain: task
        Self::debug_lock_order_note("task");
        let _task_guard = self.task_state_lock.lock();
        f(kernel_mut(&mut self.tcbs))
    }

    pub(crate) fn with_memory_state<R>(&self, f: impl FnOnce(&MemorySubsystem) -> R) -> R {
        // Lock-order domain: memory
        Self::debug_lock_order_note("memory");
        let _mem_guard = self.memory_state_lock.lock();
        f(kernel_ref(&self.memory))
    }

    pub(crate) fn with_memory_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut MemorySubsystem) -> R,
    ) -> R {
        // Lock-order domain: memory
        Self::debug_lock_order_note("memory");
        let _mem_guard = self.memory_state_lock.lock();
        f(kernel_mut(&mut self.memory))
    }
}
