use super::{KernelError, KernelState, TrapHandleError};
use crate::kernel::ipc::Message;
use crate::kernel::syscall::{SyscallError, dispatch as dispatch_syscall};
use crate::kernel::task::FaultPolicy;
use crate::kernel::task::TaskStatus;
use crate::kernel::trap::{FaultAccess, Trap, TrapEvent};
use crate::kernel::trapframe::TrapFrame;

const STRICT_UNKNOWN_TRAPS: bool = !cfg!(feature = "hosted-dev");

impl KernelState {
    fn emit_fault_report(&mut self, faulted_tid: u64) {
        let Some(endpoint_idx) = self.faults.fault_handler_endpoint else {
            return;
        };
        let Some(fault) = self.faults.last_fault else {
            return;
        };

        let mut payload = [0u8; 17];
        payload[..8].copy_from_slice(&faulted_tid.to_le_bytes());
        let addr_bytes = fault.addr.0.to_le_bytes();
        payload[8..16].copy_from_slice(&addr_bytes);
        payload[16] = match fault.access {
            FaultAccess::Read => 0,
            FaultAccess::Write => 1,
            FaultAccess::Execute => 2,
        };

        let msg = match Message::new(0, &payload) {
            Ok(msg) => msg,
            Err(_) => return,
        };

        let sent = if let Some(endpoint) = self
            .ipc
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
        {
            endpoint.send(msg).is_ok()
        } else {
            false
        };

        if sent {
            let _ = self.wake_waiter_for_endpoint(endpoint_idx);
        }
    }

    fn fault_current_task(&mut self) -> Result<(), KernelError> {
        let running_tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        self.emit_fault_report(running_tid);

        if self.effective_fault_policy_for(running_tid) == FaultPolicy::NotifyAndContinue {
            return Ok(());
        }

        let faulted_tid = self.block_current_cpu().ok_or(KernelError::TaskMissing)?;
        let tcb = self.tcb_mut(faulted_tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Faulted;
        let _ = self.dispatch_next_task()?;
        Ok(())
    }

    pub fn handle_trap(
        &mut self,
        trap: Trap,
        frame: Option<&mut TrapFrame>,
    ) -> Result<(), TrapHandleError> {
        match trap {
            Trap::Syscall => {
                self.clear_last_fault();
                let trapframe = frame.ok_or(TrapHandleError::MissingTrapFrame)?;
                let _ = self.sync_current_thread_from_frame(trapframe);
                dispatch_syscall(self, trapframe).map_err(TrapHandleError::Syscall)?;
                if trapframe.error_code() == Some(SyscallError::PageFault.code()) {
                    self.fault_current_task()
                        .map_err(SyscallError::from)
                        .map_err(TrapHandleError::Syscall)?;
                }
                Ok(())
            }
            Trap::TimerInterrupt => {
                let (_, should_preempt) = self.timer.tick_and_check();
                if should_preempt {
                    self.yield_current()
                        .map_err(SyscallError::from)
                        .map_err(TrapHandleError::Syscall)?;
                }
                Ok(())
            }
            Trap::PageFault | Trap::ExternalInterrupt | Trap::Unknown => Ok(()),
        }
    }

    pub fn handle_selected_arch_trap_entry(
        &mut self,
        cpu: crate::kernel::scheduler::CpuId,
        context: crate::arch::trap_entry::ArchTrapContext,
        frame: Option<&mut TrapFrame>,
    ) -> Result<(), TrapHandleError> {
        crate::arch::trap_entry::handle_trap_entry(self, cpu, context, frame)
    }

    pub fn handle_trap_event(
        &mut self,
        event: TrapEvent,
        frame: Option<&mut TrapFrame>,
    ) -> Result<(), TrapHandleError> {
        if let Some(fault) = event.fault() {
            self.record_fault(fault);
        }

        match event {
            TrapEvent::PageFault(_) => self
                .fault_current_task()
                .map_err(SyscallError::from)
                .map_err(TrapHandleError::Syscall),
            TrapEvent::ExternalInterrupt(irq) => {
                let irq_state = crate::arch::irq_guard::irq_save();
                let route_result = self
                    .route_external_irq(irq)
                    .map_err(SyscallError::from)
                    .map_err(TrapHandleError::Syscall);
                crate::arch::irq_guard::external_irq_eoi(irq);
                crate::arch::irq_guard::irq_restore(irq_state);
                route_result?;
                self.handle_trap(Trap::ExternalInterrupt, frame)
            }
            TrapEvent::Syscall => self.handle_trap(Trap::Syscall, frame),
            TrapEvent::TimerInterrupt => self.handle_trap(Trap::TimerInterrupt, frame),
            TrapEvent::Unknown { arch_code } => {
                crate::yarm_log!(
                    "unknown trap event cpu={} arch_code=0x{:x}",
                    self.current_cpu().0,
                    arch_code
                );
                if STRICT_UNKNOWN_TRAPS {
                    panic!(
                        "strict unknown trap policy: cpu={} arch_code=0x{:x}",
                        self.current_cpu().0,
                        arch_code
                    );
                }
                self.handle_trap(Trap::Unknown, frame)
            }
        }
    }
}
