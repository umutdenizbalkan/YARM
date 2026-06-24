// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{FaultBookkeepingMode, KernelError, KernelState, TrapHandleError};
use crate::arch::hal::Hal;
use crate::kernel::ipc::Message;
use crate::kernel::syscall::{Syscall, SyscallError, dispatch as dispatch_syscall};
use crate::kernel::task::FaultPolicy;
use crate::kernel::task::TaskStatus;
use crate::kernel::trap::{FaultAccess, FaultInfo, Trap, TrapEvent};
use crate::kernel::trapframe::TrapFrame;

const STRICT_UNKNOWN_TRAPS: bool = !cfg!(feature = "hosted-dev");
const DEMAND_STACK_GROWTH_WINDOW: u64 = 8 * 1024 * 1024;
#[allow(dead_code)]
const DEBUG_TIMER_LOG: bool = false;

// Stage 137: arch-specific PTE flag check for demand-page verification.
// Returns true iff the PTE grants user-mode read access (and write if need_write).
#[cfg(target_arch = "x86_64")]
fn demand_pte_flags_ok(
    pte: crate::arch::selected_isa::page_table::PageTableEntry,
    need_write: bool,
) -> bool {
    use crate::arch::selected_isa::page_table::PageTableEntry;
    let user = (pte.0 & PageTableEntry::USER) != 0;
    let writable = (pte.0 & PageTableEntry::WRITABLE) != 0;
    user && (!need_write || writable)
}

#[cfg(target_arch = "aarch64")]
fn demand_pte_flags_ok(
    pte: crate::arch::selected_isa::page_table::PageTableEntry,
    need_write: bool,
) -> bool {
    use crate::arch::selected_isa::page_table::PageTableEntry;
    let user = (pte.0 & PageTableEntry::USER) != 0;
    let read_only = (pte.0 & PageTableEntry::READ_ONLY) != 0;
    user && (!need_write || !read_only)
}

#[cfg(target_arch = "riscv64")]
fn demand_pte_flags_ok(
    pte: crate::arch::selected_isa::page_table::PageTableEntry,
    need_write: bool,
) -> bool {
    use crate::arch::selected_isa::page_table::PageTableEntry;
    let user = (pte.0 & PageTableEntry::USER) != 0;
    let readable = (pte.0 & PageTableEntry::READ) != 0;
    let writable = (pte.0 & PageTableEntry::WRITE) != 0;
    user && readable && (!need_write || writable)
}
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

    pub(crate) fn try_handle_demand_page_fault(
        &mut self,
        fault: crate::arch::trap::FaultInfo,
    ) -> Result<bool, KernelError> {
        if matches!(fault.access, FaultAccess::Execute) {
            return Ok(false);
        }
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let Some(asid) = self.task_asid(tid) else {
            return Ok(false); // No user address space → not a demand-paged fault
        };
        let page = fault.addr.page_align_down();
        if page.0 >= crate::kernel::vm::KERNEL_SPACE_BASE {
            return Ok(false);
        }
        if !self.fault_addr_in_demand_backed_region(tid, page.0) {
            return Ok(false);
        }
        let existing = self
            .user_spaces
            .get(asid)
            .ok_or(KernelError::Vm(crate::kernel::vm::VmError::InvalidAsid))?
            .resolve(page);
        if let Some(mapping) = existing {
            // Stage 163G fix: the page is already in the address space. Only treat
            // this as a demand fault (stale-TLB re-walk) if the EXISTING mapping
            // actually satisfies the faulting access. A WRITE fault on a present
            // read-only page is a protection/COW fault, NOT a demand fault — masking
            // it here (INVLPG + claim handled) would loop forever on an unchanged
            // RO PTE. Decline so the caller routes it to COW / task-fault instead.
            let write_satisfied =
                !matches!(fault.access, FaultAccess::Write) || mapping.flags.write;
            if !write_satisfied {
                return Ok(false);
            }
            // Stage 137: the page is already in VmSpace but the TLB may hold a
            // stale not-present entry from the original fault.  INVLPG flushes
            // that entry so the CPU re-walks the hardware page table and finds
            // the valid PTE instead of re-faulting indefinitely.
            crate::arch::selected_isa::page_table::invalidate_page(page);
            return Ok(true);
        }

        let (_id, mem_cap) = self.alloc_anonymous_memory_object()?;
        let flags = crate::kernel::vm::PageFlags::USER_RW;
        // Stage 8: asid resolved plan-first above (line 98); identical to
        // map_user_page_in_current_asid_with_caps under the global lock since
        // current_tid cannot change between the plan-first resolution and here.
        self.map_user_page_in_asid_with_caps(asid, mem_cap, page, flags)?;

        #[cfg(feature = "hosted-dev")]
        self.with_memory_state_mut(|memory| {
            for byte in 0..crate::kernel::vm::PAGE_SIZE {
                memory.user_memory.insert((asid.0, page.0 + byte as u64), 0);
            }
        });

        Ok(true)
    }

    fn emit_fault_report_for_fault(&mut self, faulted_tid: u64, fault: FaultInfo) {
        let endpoint_idx = self.with_fault_state(|faults| faults.fault_handler_endpoint);
        let Some(endpoint_idx) = endpoint_idx else {
            return;
        };

        let payload = SupervisorFaultReportWire {
            faulting_tid: faulted_tid,
            fault_addr: fault.addr.0,
            access: fault.access,
        }
        .encode();

        let msg = match Message::new(0, &payload) {
            Ok(msg) => msg,
            Err(_) => return,
        };

        // send_message_to_endpoint_and_wake enqueues under ipc_state_lock
        // (rank 3) and wakes outside the lock (task lock rank 2 < ipc rank 3).
        let _ = self.send_message_to_endpoint_and_wake(endpoint_idx, msg);
    }

    fn emit_fault_report(&mut self, faulted_tid: u64) {
        let fault = self.with_fault_state(|faults| faults.last_fault);
        let Some(fault) = fault else {
            return;
        };
        self.emit_fault_report_for_fault(faulted_tid, fault);
    }

    fn fault_current_task_for_fault(&mut self, fault: FaultInfo) -> Result<(), KernelError> {
        self.fault_current_task_with_fault(Some(fault))
    }

    fn fault_current_task(&mut self) -> Result<(), KernelError> {
        let fault = self.with_fault_state(|faults| faults.last_fault);
        self.fault_current_task_with_fault(fault)
    }

    fn fault_current_task_with_fault(
        &mut self,
        fault_opt: Option<FaultInfo>,
    ) -> Result<(), KernelError> {
        let cpu = self.current_cpu();
        // Diagnostic: log the fault before acting. TrapEvent::PageFault callers
        // pass the current FaultInfo explicitly so report/log behavior does not
        // depend on re-reading global last_fault; legacy syscall/raw callers can
        // still pass the diagnostic last_fault snapshot.
        {
            let cur_tid = self.current_tid().unwrap_or(u64::MAX);
            crate::yarm_log!(
                "TASK_FAULT_CURRENT tid={} fault_addr=0x{:x} access={:?}",
                cur_tid,
                fault_opt.map(|f| f.addr.0).unwrap_or(0),
                fault_opt.map(|f| f.access)
            );
        }
        let running_tid = self.current_tid().ok_or_else(|| {
            if cfg!(not(feature = "hosted-dev")) {
                crate::yarm_log!(
                    "TASK_MISSING site=fault_current_task/current_tid cpu={}",
                    cpu.0
                );
            }
            KernelError::TaskMissing
        })?;
        if let Some(fault) = fault_opt {
            self.emit_fault_report_for_fault(running_tid, fault);
        } else {
            self.emit_fault_report(running_tid);
        }

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

    #[cfg(test)]
    pub(crate) fn emit_fault_report_for_fault_for_test(
        &mut self,
        faulted_tid: u64,
        fault: crate::kernel::trap::FaultInfo,
    ) {
        self.emit_fault_report_for_fault(faulted_tid, fault);
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
                // Encode normal user syscall errors into the frame instead of
                // propagating as TrapHandleError. All three arch entry points
                // (AArch64 yarm_aarch64_vector_entry, x86_64 halt_forever,
                // RISC-V) treat Err(TrapHandleError) as a fatal kernel halt.
                // Normal SyscallError values (InvalidArgs, MissingRight, …)
                // must be returned to userspace as x0/error_code, not halt the kernel.
                if let Err(e) = dispatch_syscall(self, trapframe) {
                    trapframe.set_err(e.code());
                }
                if trapframe.error_code() == Some(SyscallError::PageFault.code()) {
                    self.fault_current_task()
                        .map_err(SyscallError::from)
                        .map_err(TrapHandleError::Syscall)?;
                }
                Ok(())
            }
            Trap::TimerInterrupt => {
                self.hal.acknowledge_interrupt(self.current_cpu(), 0);
                // x86_64: During bootstrap, borrow_kernel_for_boot() holds a raw
                // &mut KernelState without the SpinLock. The timer ISR acquires the
                // SpinLock via with_cpu(), creating aliased mutable references — UB.
                // Guard: skip tick/yield until signal_bootstrap_scheduler_ready() is
                // called (after all user tasks are spawned and enqueued). EOI + re-arm
                // keeps the timer alive without corrupting mid-bootstrap kernel state.
                #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
                if !crate::arch::x86_64::descriptor_tables::bootstrap_scheduler_is_ready() {
                    crate::yarm_log!(
                        "X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY cpu={}",
                        self.current_cpu().0
                    );
                    self.hal.program_timer_deadline(
                        self.current_cpu(),
                        crate::arch::platform_constants::BOOTSTRAP_TIMER_DEADLINE_TICKS,
                    );
                    return Ok(());
                }
                let (_tick, should_preempt) = self.tick_scheduler_timer();
                let _ = self
                    .process_ipc_timeout_deadlines(_tick.0)
                    .map_err(SyscallError::from)
                    .map_err(TrapHandleError::Syscall)?;
                // Emit timer health markers unconditionally but only for the
                // first few ticks so that the smoke test can verify the timer
                // fires and the scheduler advances without flooding the UART.
                // (BOOTSTRAP_TIMER_DEADLINE_TICKS / 16 ≈ 3 ms/tick on QEMU;
                //  at 90 s we would get ~30 000 ticks — far too many to log.)
                #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
                {
                    use core::sync::atomic::{AtomicU64, Ordering};
                    static TIMER_LOG_EMITTED: AtomicU64 = AtomicU64::new(0);
                    let log_seq = TIMER_LOG_EMITTED.fetch_add(1, Ordering::Relaxed);
                    if log_seq < 4 {
                        crate::yarm_log!("YARM_TIMER_EOI_DONE cpu={}", self.current_cpu().0);
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
                }
                #[cfg(all(not(feature = "hosted-dev"), not(target_arch = "x86_64")))]
                if DEBUG_TIMER_LOG {
                    crate::yarm_log!("YARM_TIMER_EOI_DONE cpu={}", self.current_cpu().0);
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
        // After the Stage 81A parity fix, handle_trap encodes syscall errors
        // into the frame instead of propagating them as TrapHandleError.
        // Translate the frame error code back so callers retain the expected
        // Result<(), TrapHandleError> contract (policy denials stay visible).
        self.handle_trap(Trap::Syscall, Some(&mut frame))?;
        if let Some(code) = frame.error_code() {
            return Err(TrapHandleError::Syscall(SyscallError::from_code(code)));
        }
        Ok(())
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
        self.handle_trap_event_with_fault_bookkeeping_mode(
            event,
            frame,
            FaultBookkeepingMode::RecordInHandleTrapEvent,
        )
    }

    pub(crate) fn handle_trap_event_with_fault_bookkeeping_mode(
        &mut self,
        event: TrapEvent,
        frame: Option<&mut TrapFrame>,
        fault_bookkeeping_mode: FaultBookkeepingMode,
    ) -> Result<(), TrapHandleError> {
        if matches!(
            fault_bookkeeping_mode,
            FaultBookkeepingMode::RecordInHandleTrapEvent
        ) {
            if let Some(fault) = event.fault() {
                self.record_fault(fault);
                if let Some(frame) = frame.as_ref() {
                    self.record_fault_frame_snapshot(frame);
                }
            }
        }

        match event {
            TrapEvent::PageFault(fault) => {
                crate::yarm_log!(
                    "PAGE_FAULT_ENTRY tid={} addr=0x{:x} access={:?} rip=0x{:x}",
                    self.current_tid().unwrap_or(u64::MAX),
                    fault.addr.0,
                    fault.access,
                    frame.as_ref().map(|f| f.saved_pc).unwrap_or(0)
                );
                // Stage 163G: proof-gated page-fault classification diagnostics
                // (active only under the sender-wake sub-knob, so normal boots are
                // not polluted). Reveals why a present write fault routes to demand:
                // whether the page is found, writable, COW-marked, and demand-backed.
                if crate::kernel::boot::ipc_recv_proof_sender_wake_active()
                    && let Some(tid) = self.current_tid()
                    && let Some(asid) = self.task_asid(tid)
                {
                    let page = fault.addr.page_align_down();
                    let mapping =
                        self.with_user_spaces(|s| s.get(asid).and_then(|a| a.resolve(page)));
                    let cow = self.is_cow_page(asid, page);
                    let demand = self.fault_addr_in_demand_backed_region(tid, page.0);
                    crate::yarm_log!(
                        "PF_PROOF_CLASSIFY tid={} asid={} va=0x{:x} access={:?}",
                        tid,
                        asid.0,
                        fault.addr.0,
                        fault.access
                    );
                    crate::yarm_log!(
                        "PF_PROOF_LOOKUP_MAPPING tid={} asid={} va=0x{:x} found={} writable={} cow={} demand={} phys=0x{:x}",
                        tid,
                        asid.0,
                        page.0,
                        mapping.is_some() as u8,
                        mapping.map(|m| m.flags.write as u8).unwrap_or(0),
                        cow as u8,
                        demand as u8,
                        mapping.map(|m| m.phys.0).unwrap_or(0)
                    );
                }
                if matches!(fault.access, FaultAccess::Write) {
                    if let Some(tid) = self.current_tid()
                        && let Some(asid) = self.task_asid(tid)
                        && self
                            .try_handle_cow_fault(asid, fault.addr)
                            .map_err(SyscallError::from)
                            .map_err(TrapHandleError::Syscall)?
                    {
                        crate::yarm_log!("PAGE_FAULT_HANDLED_COW");
                        return Ok(());
                    }
                }
                if self
                    .try_handle_demand_page_fault(fault)
                    .map_err(SyscallError::from)
                    .map_err(TrapHandleError::Syscall)?
                {
                    // Stage 137: verify the hardware PTE is accessible before
                    // declaring the fault handled.  Also fix ASID/CR3 if the
                    // task's address space differs from what the HAL recorded.
                    let page = fault.addr.page_align_down();
                    let need_write = matches!(fault.access, FaultAccess::Write);
                    let tid = self.current_tid().unwrap_or(u64::MAX);
                    let task_asid = self.task_asid(tid).unwrap_or(crate::kernel::vm::Asid(0));
                    let active_asid_num = self.d6_diag_active_asid_num();
                    let active_asid = crate::kernel::vm::Asid(active_asid_num as u16);
                    let task_pte =
                        crate::arch::selected_isa::page_table::resolve_page(task_asid, page);
                    let active_pte = if active_asid.0 != task_asid.0 {
                        crate::arch::selected_isa::page_table::resolve_page(active_asid, page)
                    } else {
                        task_pte
                    };
                    let task_present = task_pte.is_some();
                    let active_present = active_pte.is_some();
                    let task_flags = task_pte.map(|p| p.0).unwrap_or(0);
                    let active_flags = active_pte.map(|p| p.0).unwrap_or(0);
                    crate::yarm_log!(
                        "PAGE_FAULT_DEMAND_VERIFY tid={} page=0x{:x} task_asid={} active_asid={} task_present={} active_present={} task_flags=0x{:x} active_flags=0x{:x}",
                        tid,
                        page.0,
                        task_asid.0,
                        active_asid.0,
                        task_present,
                        active_present,
                        task_flags,
                        active_flags,
                    );
                    let pte_ok = task_pte
                        .map(|p| demand_pte_flags_ok(p, need_write))
                        .unwrap_or(false);
                    if pte_ok && !active_present && active_asid.0 != task_asid.0 {
                        self.hal.switch_address_space(task_asid);
                    }
                    // Stage 138: hardware CR3 PTE walk to confirm the CPU will
                    // actually see the page as accessible after demand mapping.
                    // Software VM resolve says present, but the CPU may be
                    // walking a different (stale) page table.
                    // Only performed on real x86_64 hardware; hosted-dev (test)
                    // mode has no real page tables so hw_demand_ok is trivially true.
                    #[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
                    let hw_demand_ok = {
                        let hw_cr3 = crate::arch::x86_64::page_table::read_hw_cr3();
                        let hw_root = hw_cr3 & !0xfffu64;
                        let (pml4e, pdpte, pde, hw_pte) =
                            crate::arch::x86_64::page_table::hw_pte_walk_verbose(hw_root, page.0);
                        let hw_present = (hw_pte & 1) != 0;
                        let hw_user = (hw_pte & 4) != 0;
                        let hw_writable = (hw_pte & 2) != 0;
                        crate::yarm_log!(
                            "PAGE_FAULT_POST_DEMAND_HW_PTE_WALK cr3=0x{:016x} va=0x{:016x} pml4e=0x{:016x} pdpte=0x{:016x} pde=0x{:016x} pte=0x{:016x} present={} user={} writable={}",
                            hw_cr3,
                            page.0,
                            pml4e,
                            pdpte,
                            pde,
                            hw_pte,
                            hw_present as u8,
                            hw_user as u8,
                            hw_writable as u8,
                        );
                        hw_present && hw_user && (!need_write || hw_writable)
                    };
                    #[cfg(any(not(target_arch = "x86_64"), feature = "hosted-dev"))]
                    let hw_demand_ok = true;
                    if pte_ok && hw_demand_ok {
                        crate::yarm_log!("PAGE_FAULT_HANDLED_DEMAND");
                        return Ok(());
                    }
                }
                crate::yarm_log!(
                    "PAGE_FAULT_UNHANDLED tid={} addr=0x{:x} access={:?} rip=0x{:x}",
                    self.current_tid().unwrap_or(u64::MAX),
                    fault.addr.0,
                    fault.access,
                    frame.as_ref().map(|f| f.saved_pc).unwrap_or(0)
                );
                self.fault_current_task_for_fault(fault)
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
