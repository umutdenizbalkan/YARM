// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

impl KernelState {
    pub(crate) fn scheduler_state(
        &self,
    ) -> crate::kernel::lock::SpinLockIrqGuard<'_, SchedulerState> {
        self.scheduler_state.lock()
    }

    pub(crate) fn with_scheduler_state<R>(&self, f: impl FnOnce(&SchedulerState) -> R) -> R {
        let sched = self.scheduler_state.lock();
        f(&sched)
    }

    pub(crate) fn with_scheduler_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut SchedulerState) -> R,
    ) -> R {
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
        let _ipc_guard = self.ipc_state_lock.lock();
        f(kernel_ref(&self.ipc))
    }

    pub(crate) fn with_ipc_state_mut<R>(&mut self, f: impl FnOnce(&mut IpcSubsystem) -> R) -> R {
        let _ipc_guard = self.ipc_state_lock.lock();
        f(kernel_mut(&mut self.ipc))
    }

    pub(crate) fn with_driver_state<R>(&self, f: impl FnOnce(&DriverSubsystem) -> R) -> R {
        let _driver_guard = self.driver_state_lock.lock();
        f(kernel_ref(&self.drivers))
    }

    pub(crate) fn with_driver_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut DriverSubsystem) -> R,
    ) -> R {
        let _driver_guard = self.driver_state_lock.lock();
        f(kernel_mut(&mut self.drivers))
    }

    pub(crate) fn with_fault_state<R>(&self, f: impl FnOnce(&FaultSubsystem) -> R) -> R {
        let _fault_guard = self.fault_state_lock.lock();
        f(kernel_ref(&self.faults))
    }

    pub(crate) fn with_fault_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut FaultSubsystem) -> R,
    ) -> R {
        let _fault_guard = self.fault_state_lock.lock();
        f(kernel_mut(&mut self.faults))
    }

    #[allow(dead_code)]
    pub(crate) fn with_restart_state<R>(&self, f: impl FnOnce(&RestartSubsystem) -> R) -> R {
        let _restart_guard = self.restart_state_lock.lock();
        f(kernel_ref(&self.restart))
    }

    pub(crate) fn with_restart_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut RestartSubsystem) -> R,
    ) -> R {
        let _restart_guard = self.restart_state_lock.lock();
        f(kernel_mut(&mut self.restart))
    }

    pub(crate) fn with_capability_state<R>(&self, f: impl FnOnce(&CapabilitySubsystem) -> R) -> R {
        let _capability_guard = self.capability_state_lock.lock();
        f(&self.capability)
    }

    pub(crate) fn with_capability_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut CapabilitySubsystem) -> R,
    ) -> R {
        let _capability_guard = self.capability_state_lock.lock();
        f(&mut self.capability)
    }

    pub(crate) fn with_telemetry_state<R>(&self, f: impl FnOnce(&TelemetrySubsystem) -> R) -> R {
        let _telemetry_guard = self.telemetry_state_lock.lock();
        f(kernel_ref(&self.telemetry))
    }

    pub(crate) fn with_telemetry_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut TelemetrySubsystem) -> R,
    ) -> R {
        let _telemetry_guard = self.telemetry_state_lock.lock();
        f(kernel_mut(&mut self.telemetry))
    }

    pub(crate) fn with_boot_config<R>(&self, f: impl FnOnce(&BootConfigSubsystem) -> R) -> R {
        let _boot_config_guard = self.boot_config_state_lock.lock();
        f(kernel_ref(&self.boot_config))
    }

    #[allow(dead_code)]
    pub(crate) fn with_boot_config_mut<R>(
        &mut self,
        f: impl FnOnce(&mut BootConfigSubsystem) -> R,
    ) -> R {
        let _boot_config_guard = self.boot_config_state_lock.lock();
        f(kernel_mut(&mut self.boot_config))
    }

    pub(crate) fn with_task_then_capability<R>(
        &self,
        f: impl FnOnce(&[Option<ThreadControlBlock>; MAX_TASKS], &CapabilitySubsystem) -> R,
    ) -> R {
        let _task_guard = self.task_state_lock.lock();
        let _capability_guard = self.capability_state_lock.lock();
        f(kernel_ref(&self.tcbs), &self.capability)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_scheduler_then_ipc<R>(
        &self,
        f: impl FnOnce(&SchedulerState, &IpcSubsystem) -> R,
    ) -> R {
        let sched = self.scheduler_state.lock();
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
        let _vm_guard = self.vm_state_lock.lock();
        f(kernel_ref(&self.user_spaces))
    }

    pub(crate) fn with_user_spaces_mut<R>(
        &mut self,
        f: impl FnOnce(&mut AddressSpaceManager) -> R,
    ) -> R {
        let _vm_guard = self.vm_state_lock.lock();
        f(kernel_mut(&mut self.user_spaces))
    }

    pub(crate) fn with_tcbs<R>(
        &self,
        f: impl FnOnce(&[Option<ThreadControlBlock>; MAX_TASKS]) -> R,
    ) -> R {
        let _task_guard = self.task_state_lock.lock();
        f(kernel_ref(&self.tcbs))
    }

    pub(crate) fn with_tcbs_mut<R>(
        &mut self,
        f: impl FnOnce(&mut [Option<ThreadControlBlock>; MAX_TASKS]) -> R,
    ) -> R {
        let _task_guard = self.task_state_lock.lock();
        f(kernel_mut(&mut self.tcbs))
    }

    pub(crate) fn with_memory_state<R>(&self, f: impl FnOnce(&MemorySubsystem) -> R) -> R {
        let _mem_guard = self.memory_state_lock.lock();
        f(kernel_ref(&self.memory))
    }

    pub(crate) fn with_memory_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut MemorySubsystem) -> R,
    ) -> R {
        let _mem_guard = self.memory_state_lock.lock();
        f(kernel_mut(&mut self.memory))
    }
}
