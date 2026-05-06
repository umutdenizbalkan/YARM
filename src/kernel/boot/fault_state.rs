// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState, TrapHandleError};
use crate::arch::hal::Hal;
use crate::kernel::ipc::Message;
use crate::kernel::syscall::{Syscall, SyscallError, dispatch as dispatch_syscall};
use crate::kernel::task::FaultPolicy;
use crate::kernel::task::TaskStatus;
use crate::kernel::trap::{FaultAccess, Trap, TrapEvent};
use crate::kernel::trapframe::TrapFrame;
use yarm_ipc_abi::supervisor_abi::{
    SUPERVISOR_OP_FAULT_REPORT_WIRE, encode_supervisor_envelope_into,
};

const STRICT_UNKNOWN_TRAPS: bool = !cfg!(feature = "hosted-dev");
const DEMAND_STACK_GROWTH_WINDOW: u64 = 8 * 1024 * 1024;
#[allow(dead_code)]
const DEBUG_TIMER_LOG: bool = false;
/// Supervisor fault notification wire ABI payload length.
///
/// Layout (little-endian):
/// - bytes [0..8): faulting tid (u64)
/// - bytes [8..16): fault address (u64)
/// - byte [16]: access kind (0=Read, 1=Write, 2=Execute)
pub(crate) const SUPERVISOR_FAULT_REPORT_WIRE_LEN: usize = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SupervisorFaultReportWire {
    pub(crate) faulting_tid: u64,
    pub(crate) fault_addr: u64,
    pub(crate) access: FaultAccess,
}

impl SupervisorFaultReportWire {
    pub(crate) fn encode(self) -> [u8; SUPERVISOR_FAULT_REPORT_WIRE_LEN] {
        let mut payload = [0u8; SUPERVISOR_FAULT_REPORT_WIRE_LEN];
        payload[..8].copy_from_slice(&self.faulting_tid.to_le_bytes());
        payload[8..16].copy_from_slice(&self.fault_addr.to_le_bytes());
        payload[16] = match self.access {
            FaultAccess::Read => 0,
            FaultAccess::Write => 1,
            FaultAccess::Execute => 2,
        };
        payload
    }

    #[cfg(test)]
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != SUPERVISOR_FAULT_REPORT_WIRE_LEN {
            return None;
        }
        let mut tid = [0u8; 8];
        let mut addr = [0u8; 8];
        tid.copy_from_slice(&bytes[..8]);
        addr.copy_from_slice(&bytes[8..16]);
        let access = match bytes[16] {
            0 => FaultAccess::Read,
            1 => FaultAccess::Write,
            2 => FaultAccess::Execute,
            _ => return None,
        };
        Some(Self {
            faulting_tid: u64::from_le_bytes(tid),
            fault_addr: u64::from_le_bytes(addr),
            access,
        })
    }
}

impl KernelState {
    fn fault_addr_in_demand_backed_region(&self, tid: u64, fault_addr: u64) -> bool {
        if let Some((base, end)) = self.task_brk_bounds(tid)
            && fault_addr >= base as u64
            && fault_addr < end as u64
        {
            return true;
        }

        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .and_then(|tcb| tcb.user_stack_top)
                .map(|top| {
                    let low = top.0.saturating_sub(DEMAND_STACK_GROWTH_WINDOW);
                    fault_addr >= low && fault_addr < top.0
                })
                .unwrap_or(false)
        })
    }

    fn try_handle_demand_page_fault(
        &mut self,
        fault: crate::arch::trap::FaultInfo,
    ) -> Result<bool, KernelError> {
        if matches!(fault.access, FaultAccess::Execute) {
            return Ok(false);
        }
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        let page = fault.addr.page_align_down();
        if page.0 >= crate::kernel::vm::KERNEL_SPACE_BASE {
            return Ok(false);
        }
        if !self.fault_addr_in_demand_backed_region(tid, page.0) {
            return Ok(false);
        }
        let already_mapped = self
            .user_spaces
            .get(asid)
            .ok_or(KernelError::Vm(crate::kernel::vm::VmError::InvalidAsid))?
            .resolve(page)
            .is_some();
        if already_mapped {
            return Ok(true);
        }

        let (_id, mem_cap) = self.alloc_anonymous_memory_object()?;
        let flags = crate::kernel::vm::PageFlags::USER_RW;
        self.map_user_page_in_current_asid_with_caps(mem_cap, page, flags)?;

        #[cfg(feature = "hosted-dev")]
        self.with_memory_state_mut(|memory| {
            for byte in 0..crate::kernel::vm::PAGE_SIZE {
                memory.user_memory.insert((asid.0, page.0 + byte as u64), 0);
            }
        });

        Ok(true)
    }

    fn emit_fault_report(&mut self, faulted_tid: u64) {
        let (endpoint_idx, fault) =
            self.with_fault_state(|faults| (faults.fault_handler_endpoint, faults.last_fault));
        let Some(endpoint_idx) = endpoint_idx else {
            return;
        };
        let Some(fault) = fault else {
            return;
        };

        let payload = SupervisorFaultReportWire {
            faulting_tid: faulted_tid,
            fault_addr: fault.addr.0,
            access: fault.access,
        }
        .encode();

        let mut wire = [0u8; SUPERVISOR_FAULT_REPORT_WIRE_LEN + 2];
        let Some(enveloped) =
            encode_supervisor_envelope_into(SUPERVISOR_OP_FAULT_REPORT_WIRE, &payload, &mut wire)
        else {
            return;
        };

        let msg = match Message::new(0, enveloped) {
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
        let cpu = self.current_cpu();
        let running_tid = self.current_tid().ok_or_else(|| {
            if cfg!(not(feature = "hosted-dev")) {
                crate::yarm_log!(
                    "TASK_MISSING site=fault_current_task/current_tid cpu={}",
                    cpu.0
                );
            }
            KernelError::TaskMissing
        })?;
        self.emit_fault_report(running_tid);

        if self.effective_fault_policy_for(running_tid) == FaultPolicy::NotifyAndContinue {
            return Ok(());
        }

        let faulted_tid = self.block_current_cpu().ok_or_else(|| {
            if cfg!(not(feature = "hosted-dev")) {
                crate::yarm_log!(
                    "TASK_MISSING site=fault_current_task/block_current cpu={}",
                    cpu.0
                );
            }
            KernelError::TaskMissing
        })?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == faulted_tid)
                .ok_or_else(|| {
                    if cfg!(not(feature = "hosted-dev")) {
                        crate::yarm_log!(
                            "TASK_MISSING site=fault_current_task/faulted_tcb_lookup cpu={} tid={}",
                            cpu.0,
                            faulted_tid
                        );
                    }
                    KernelError::TaskMissing
                })?;
            tcb.status = TaskStatus::Faulted;
            Ok::<_, KernelError>(())
        })?;
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
                self.hal.acknowledge_interrupt(self.current_cpu(), 0);
                #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
                if DEBUG_TIMER_LOG {
                    crate::yarm_log!("YARM_TIMER_EOI_DONE cpu={}", self.current_cpu().0);
                }
                let (_tick, should_preempt) = self.tick_scheduler_timer();
                let _ = self
                    .process_ipc_timeout_deadlines(_tick.0)
                    .map_err(SyscallError::from)
                    .map_err(TrapHandleError::Syscall)?;
                #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
                if DEBUG_TIMER_LOG {
                    crate::yarm_log!(
                        "YARM_SCHED_TICK cpu={} tick={} preempt={}",
                        self.current_cpu().0,
                        _tick.0,
                        should_preempt as u8
                    );
                    crate::yarm_log!(
                        "YARM_TIMER_IRQ_DELIVERED cpu={} tick={}",
                        self.current_cpu().0,
                        _tick.0
                    );
                }
                if should_preempt {
                    self.yield_current()
                        .map_err(SyscallError::from)
                        .map_err(TrapHandleError::Syscall)?;
                }
                self.hal.program_timer_deadline(
                    self.current_cpu(),
                    crate::arch::platform_constants::BOOTSTRAP_TIMER_DEADLINE_TICKS,
                );
                Ok(())
            }
            Trap::PageFault | Trap::ExternalInterrupt | Trap::Unknown => Ok(()),
        }
    }

    pub fn control_plane_set_process_cnode_slots_via_syscall(
        &mut self,
        target_pid: u64,
        slot_capacity: usize,
    ) -> Result<(), TrapHandleError> {
        let Ok(target_pid_arg) = usize::try_from(target_pid) else {
            return Err(TrapHandleError::Syscall(SyscallError::InvalidArgs));
        };
        let mut frame = TrapFrame::new(
            Syscall::ControlPlaneSetCnodeSlots as usize,
            [target_pid_arg, slot_capacity, 0, 0, 0, 0],
        );
        self.handle_trap(Trap::Syscall, Some(&mut frame))
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
            if let Some(frame) = frame.as_ref() {
                self.record_fault_frame_snapshot(frame);
            }
        }

        match event {
            TrapEvent::PageFault(fault) => {
                if matches!(fault.access, FaultAccess::Write) {
                    if let Some(tid) = self.current_tid()
                        && let Some(asid) = self.task_asid(tid)
                        && self
                            .try_handle_cow_fault(asid, fault.addr)
                            .map_err(SyscallError::from)
                            .map_err(TrapHandleError::Syscall)?
                    {
                        return Ok(());
                    }
                }
                if self
                    .try_handle_demand_page_fault(fault)
                    .map_err(SyscallError::from)
                    .map_err(TrapHandleError::Syscall)?
                {
                    return Ok(());
                }
                self.fault_current_task()
                    .map_err(SyscallError::from)
                    .map_err(TrapHandleError::Syscall)
            }
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
