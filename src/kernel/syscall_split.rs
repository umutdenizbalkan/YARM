// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 28: trap/syscall split-dispatch bridge (whitelist-only scaffold).
//! Stage 29: live-wired for `ControlPlaneSetCnodeSlots` (NR 8) via
//! [`try_split_dispatch_into_frame`].
//! Stage 32B: live-wired for `IpcRecv` (NR 2), kernel-task queued-plain case only.
//! Stage 114: live-wired for `VmBrk` (NR 14), page-crossing-shrink case only, via
//! [`try_split_vm_brk_shrink_into_frame`].
//!
//! This module hosts the minimal, **whitelist-only** mechanism that classifies
//! a decoded `Syscall` as eligible for *split-dispatch* — i.e. servicing it via
//! per-domain split-mut/split-read helpers on [`SharedKernel`] WITHOUT taking the
//! global `SpinLock<KernelState>` and WITHOUT calling `with` / `with_cpu`.
//!
//! ## Default-deny contract
//!
//! [`try_split_dispatch`] returns `Some(result)` ONLY for syscalls on the
//! explicit whitelist. Every other syscall — including all IPC, Spawn/fork/exec,
//! VM, and futex paths — falls through the `_ => None` arm and MUST be handled by
//! the unchanged global-lock dispatch path (`SharedKernel::with_cpu` →
//! `KernelState::handle_trap` → `syscall::dispatch`). This guarantees that adding
//! the bridge can never silently change the behavior of any non-whitelisted
//! syscall: the fallback is the existing, fully-tested global-lock path.
//!
//! ## Stage 29 — live-wired result-writeback contract
//!
//! The whitelisted candidate (`ControlPlaneSetCnodeSlots`) returns a *non-trivial
//! trapframe payload*: the production handler writes
//! `frame.set_ok(slot_capacity, target_pid, 0)` — two meaningful return registers,
//! not a single status code. [`try_split_dispatch`] (Stage 28) returns only the
//! logical `Result<(), KernelError>`.
//!
//! Stage 29 adds [`try_split_dispatch_into_frame`], the minimal pre-global-lock
//! *result-writeback contract*. `TrapFrame::set_ok` / `set_err` are pure register
//! writes (no global-lock dependency, architecture-neutral — see
//! `kernel/trapframe.rs`), so the seam calls them directly:
//!   * It decodes `(target_pid, slots)` from the frame exactly as the global-lock
//!     handler does (`arg(SYSCALL_ARG_CAP)`, `arg(SYSCALL_ARG_PTR)`).
//!   * It reads the requester TID via `SharedKernel::current_tid_split_read(cpu)`
//!     (scheduler lock only) — value-equivalent to the global-lock
//!     `with_cpu(cpu, |k| k.current_tid())` the old `current_tid()` used.
//!   * On success it writes `set_ok(slots, pid, 0)` — byte-for-byte the encoding
//!     the global-lock handler produced — and returns `Some(Ok(()))`.
//!   * On a domain error it returns `Some(Err(TrapHandleError::Syscall(..)))` so
//!     the arch stub propagates it on exactly the path the old `Err(SyscallError)`
//!     return took (the control-plane syscall's errors are fatal/propagated, not
//!     user-recoverable — the old handler never wrote `set_err` for them either).
//!   * It returns `None` for every non-whitelisted syscall (and when the requester
//!     TID is unavailable), so the caller falls back to the UNCHANGED global-lock
//!     path.
//!
//! The split path never blocks/yields/schedules and never switches tasks, so
//! `entering_tid == exiting_tid` (i.e. `task_switched == false`) stays observable
//! to the arch `write_trap_returns_to_saved_regs` branch exactly as before. The
//! `entering_tid` / `exiting_tid` snapshots and the trap boundary are left
//! untouched. See `doc/KERNEL_LOCKING.md` §47.

use crate::kernel::boot::{KernelError, TrapHandleError};
use crate::kernel::scheduler::CpuId;
use crate::kernel::syscall::{Syscall, SyscallError};
use crate::kernel::trapframe::TrapFrame;
use crate::runtime::SharedKernel;

/// Syscalls eligible for split-dispatch (no global lock).
///
/// **WHITELIST ONLY.** A variant exists here only after the corresponding
/// `SharedKernel` split helper is proven safe (single ascending lock-domain
/// order, no blocking/yield/schedule, no user-memory copy in the bridge itself,
/// result encodable as the existing syscall return type).
// Stage 29: live-wired for `ControlPlaneCnodeSlots` via
// `try_split_dispatch_into_frame`. The default-deny `_ => None` fallback keeps
// every other syscall on the unchanged global-lock dispatch path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SplitEligibleSyscall {
    /// `Syscall::ControlPlaneSetCnodeSlots` (NR 8). Serviced by
    /// `SharedKernel::control_plane_set_process_cnode_slots_split_mut`
    /// (task read rank 2 → boot-config read → capability mutate rank 4).
    ControlPlaneCnodeSlots {
        requester_tid: u64,
        target_pid: u64,
        slots: usize,
    },
    /// `Syscall::IpcRecv` (NR 2), kernel-task receiver of a queued plain message.
    ///
    /// Stage 32B: split eligibility for IpcRecv cannot be fully decided from the
    /// syscall number + raw args alone — whether the receiver is a kernel task,
    /// whether the endpoint has a queued plain message, and whether a sender-wake
    /// or recv-v2 path applies are all resolved INSIDE
    /// `try_split_ipc_recv_queued_plain_into_frame`. This variant therefore marks
    /// IpcRecv as "attempt the split"; the helper itself returns `None` for every
    /// case it cannot service (user-ASID receiver, empty queue, sender-wake,
    /// cap-transfer, recv-v2), and that `None` propagates straight back to the
    /// global-lock fallback. The variant carries no decoded args for that reason.
    IpcRecvKernelTask,
    // Add others ONLY when the per-domain helper is proven safe.
    //
    // Stage 114: `Syscall::VmBrk` (NR 14) is intentionally NOT added here.
    // Like `IpcRecv`, its split eligibility cannot be decided from the
    // syscall number + raw args alone (group-leader status, brk bounds,
    // page-crossing, and online-CPU count all require domain reads), but
    // unlike `IpcRecv` there is no need for an enum variant: it is
    // special-cased directly in `try_split_dispatch_into_frame` (mirroring
    // the `Syscall::IpcRecv` special case below) and routed straight to
    // `try_split_vm_brk_shrink_into_frame`, never through
    // `classify_split_eligible` / `try_split_dispatch`.
}

/// Classify a decoded syscall + raw args into a split-eligible descriptor.
///
/// Returns `None` for every non-whitelisted syscall (default-deny). For the
/// whitelisted control-plane syscall it also validates the same argument
/// preconditions the global-lock handler enforces (`target_pid != 0`,
/// `slots != 0`); on a precondition miss it returns `None` so the caller falls
/// back to the global-lock path, which will produce the canonical
/// `InvalidArgs` error and the correct trapframe encoding.
pub(crate) fn classify_split_eligible(
    syscall: Syscall,
    requester_tid: u64,
    args: [u64; 6],
) -> Option<SplitEligibleSyscall> {
    match syscall {
        Syscall::ControlPlaneSetCnodeSlots => {
            // args[0] = target_pid (SYSCALL_ARG_CAP), args[1] = slots (SYSCALL_ARG_PTR).
            let target_pid = args[0];
            let slots = args[1] as usize;
            if target_pid == 0 || slots == 0 {
                // Defer the InvalidArgs encoding to the global-lock path.
                return None;
            }
            Some(SplitEligibleSyscall::ControlPlaneCnodeSlots {
                requester_tid,
                target_pid,
                slots,
            })
        }
        // Stage 32B: IpcRecv (NR 2) is split-eligible at classification time, but it
        // is serviced through the frame-level seam
        // (`try_split_dispatch_into_frame` → `try_split_ipc_recv_queued_plain_into_frame`),
        // not through `try_split_dispatch` (which has no `cpu`/`frame`). The variant
        // documents eligibility; `try_split_dispatch` returns `None` for it so the
        // arg-only caller defers to the frame-level recv path / global-lock fallback.
        Syscall::IpcRecv => Some(SplitEligibleSyscall::IpcRecvKernelTask),
        // Default-deny: every other syscall falls back to the global-lock path.
        _ => None,
    }
}

/// Try to dispatch a syscall through the split (no-global-lock) path.
///
/// Returns `Some(result)` if the syscall is on the whitelist and was serviced via
/// per-domain split helpers; returns `None` to signal the caller to fall back to
/// the unchanged global-lock dispatch path. This function itself never blocks,
/// yields, schedules, or copies user memory.
pub(crate) fn try_split_dispatch(
    shared: &SharedKernel,
    syscall: Syscall,
    requester_tid: u64,
    args: [u64; 6],
) -> Option<Result<(), KernelError>> {
    let eligible = classify_split_eligible(syscall, requester_tid, args)?;
    match eligible {
        SplitEligibleSyscall::ControlPlaneCnodeSlots {
            requester_tid,
            target_pid,
            slots,
        } => Some(shared.control_plane_set_process_cnode_slots_split_mut(
            requester_tid,
            target_pid,
            slots,
        )),
        // IpcRecv is serviced by the frame-level seam, not this arg-only path.
        // Returning `None` defers to `try_split_dispatch_into_frame`'s dedicated
        // recv routing (and ultimately the global-lock fallback).
        SplitEligibleSyscall::IpcRecvKernelTask => None,
    }
}

/// # Validation status
/// - LIVE_TRAP_SMOKE_X86_64 — entry point for the NR 8 live split-dispatch path;
///   called from `handle_trap_entry_shared` before the global lock; x86_64 smoke
///   validated (Stage 29 / 29A, marker `YARM_LOCK_SPLIT_DISPATCH nr=8 result=ok`).
///
/// Stage 29 live-wire seam: try to service a syscall through the split
/// (no-global-lock) path AND write its result into the trap frame.
///
/// This is the pre-global-lock *result-writeback contract*. It is called from
/// `handle_trap_entry_shared` BEFORE the global `with_cpu` lock is taken.
///
/// U9-QA §2 — what the pre-lock split dispatcher DID, as three exact meanings.
///
/// Before U9-QA the answer was `Option<Result<(), TrapHandleError>>`, which could express only
/// two: "not mine, fall back" and "mine, finished". Both are non-switching, and the trap entry
/// acted on that — a `Some` early-returned through the live frame, because no split class had
/// ever published a terminal transition.
///
/// FutexWait is the first that does. Its caller ends the trap `Blocked(Futex)` and current on no
/// CPU, so BOTH old answers are wrong for it: falling back would re-execute the syscall on an
/// already-blocked task, and early-returning would `iret`/`eret` through the parked caller's own
/// frame. The third meaning names that state explicitly, so neither mistake is representable.
// `QueueAdvanceCommitted` is constructed only by the pre-lock FutexWait route, which is
// `cfg(not(hosted-dev))`; the hosted build compiles the type but never mints that variant.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
/// U9-FT4 — the pre-lock AArch64 terminal PageFault route.
///
/// Admits ONLY the class an existing live witness proves: an AArch64 user PageFault below
/// `KERNEL_SPACE_BASE`, classified `TerminallyUnhandled`, under a terminating policy, whose
/// report is publishable on the BUFFERED path with no waiter.
///
/// ORDERING IS MODELLED ON FutexWait: admit -> reserve the deferral -> publish -> transition ->
/// drain. The queue advance is NOT performed here; the EXISTING post-lock drain consumes the
/// deferral once and applies the exact incoming context. `QueueAdvanceCommitted` is returned
/// ONLY with a reserved deferral, which is what makes the incoming apply structurally guaranteed
/// — returning it without one was the FT3 defect that resumed the faulting PC.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_terminal_page_fault_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    fault: Option<crate::kernel::trap::FaultInfo>,
    frame: Option<&crate::kernel::trapframe::TrapFrame>,
) -> SplitDispatchDisposition {
    use crate::kernel::boot::{
        BufferedFaultAdmission as A, BufferedFaultCommit as C, PageFaultRoute,
        TerminalFaultTransition as T, page_fault_route_for,
    };
    use SplitDispatchDisposition as D;

    if !cfg!(target_arch = "aarch64") {
        return D::NotHandled;
    }
    let (Some(fault), Some(frame)) = (fault, frame) else {
        return D::NotHandled;
    };
    let cpu_idx = cpu.0 as usize;
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return D::NotHandled;
    }
    // (1) Classify off-lock. A stale identity refuses before anything is decided.
    let Ok((class, Some(facts))) = shared.classify_page_fault_shared(cpu, fault) else {
        return D::NotHandled;
    };
    if !matches!(
        page_fault_route_for("aarch64", class),
        PageFaultRoute::SplitTerminal
    ) {
        return D::NotHandled;
    }
    // (2) Terminal policy, against the EXACT coordinate we classified with. `NotifyAndContinue`
    // reports and RESUMES — a different shape entirely — so it keeps the unchanged broad path.
    let Ok(snapshot) = shared.read_terminal_fault_policy_shared(cpu, facts.tid, facts.asid) else {
        return D::NotHandled;
    };
    if !snapshot.terminates_task() {
        return D::NotHandled;
    }
    // (3) Buffered eligibility. A waiter, a full buffer, a stale endpoint or no route all decline
    // here, before anything is published.
    let admission = shared.admit_buffered_fault_report_shared(&snapshot);
    let A::BufferedEligible {
        endpoint_idx,
        generation,
        queued_before,
        via_fault_handler,
    } = admission
    else {
        crate::yarm_log!(
            "TERMINAL_FAULT_SPLIT_REFUSED cpu={} tid={} phase=admit reason={}",
            cpu.0,
            facts.tid,
            match admission {
                A::WaiterPresent { .. } => "waiter_present",
                A::BufferFull { .. } => "buffer_full",
                A::EndpointStale { .. } => "endpoint_stale",
                A::NoRoute => "no_route",
                A::BufferedEligible { .. } => "unreachable",
            }
        );
        return D::NotHandled;
    };
    // (4) U9-QA admission — the capability check, before any mutation.
    if let Err(refusal) = shared.queue_advance_admit_split(
        cpu,
        crate::kernel::boot::QueueAdvanceApply::ExactTokenResume,
    ) {
        crate::yarm_log!(
            "TERMINAL_FAULT_SPLIT_REFUSED cpu={} tid={} phase=queue_admit reason={:?}",
            cpu.0,
            facts.tid,
            refusal
        );
        return D::NotHandled;
    }
    // (5) RESERVE THE DEFERRAL BEFORE ANY PUBLICATION. A reservation failure is pre-mutation and
    // may fall back; holding it is what guarantees the drain will apply an incoming context.
    if !crate::kernel::boot::futex_wait_dispatch_try_defer(cpu_idx, facts.tid) {
        crate::yarm_log!(
            "TERMINAL_FAULT_SPLIT_REFUSED cpu={} tid={} phase=defer reason=defer_unavailable",
            cpu.0,
            facts.tid
        );
        return D::NotHandled;
    }
    // (6) Capture the outgoing context while the reservation is held and nothing is published.
    let captured = shared.capture_outgoing_user_context_split(facts.tid, frame);
    // The exact facts the broad arm prints, in the broad arm's order. `PAGE_FAULT_ENTRY` is
    // emitted here because this route intercepts BEFORE the broad arm that would have printed
    // it, and the marker stream must stay faithful to what an observer sees today.
    crate::yarm_log!(
        "PAGE_FAULT_ENTRY tid={} addr=0x{:x} access={:?} rip=0x{:x}",
        facts.tid,
        fault.addr.0,
        fault.access,
        frame.saved_pc
    );
    crate::yarm_log!(
        "PAGE_FAULT_UNHANDLED tid={} addr=0x{:x} access={:?} rip=0x{:x}",
        facts.tid,
        fault.addr.0,
        fault.access,
        frame.saved_pc
    );
    crate::yarm_log!(
        "TASK_FAULT_CURRENT tid={} fault_addr=0x{:x} access={:?}",
        facts.tid,
        fault.addr.0,
        Some(fault.access)
    );
    crate::yarm_log!("TASK_FAULT_REPORT_BEGIN tid={}", facts.tid);
    crate::yarm_log!(
        "TASK_FAULT_REPORT_TARGET tid={} endpoint={} generation={}",
        facts.tid,
        endpoint_idx,
        generation
    );
    crate::yarm_log!(
        "TASK_FAULT_REPORT_QUEUE_STATE_BEFORE endpoint={} waiters=0 queued={}",
        endpoint_idx,
        queued_before
    );
    // (7) PUBLISH. Past this line broad fallback is forbidden.
    match shared.commit_buffered_fault_report_shared(
        facts.tid,
        fault,
        endpoint_idx,
        generation,
        via_fault_handler,
    ) {
        C::Buffered { .. } => {}
        // Pre-publication refusal: release the reservation and let the broad path run.
        _ => {
            crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
            return D::NotHandled;
        }
    }
    // (8) The terminal task transition. Fail-closed from here.
    match shared.commit_terminal_fault_transition_shared(cpu, facts.tid, facts.asid, frame) {
        T::Committed { .. } => {}
        _ => {
            // The report is published, so the broad emitter must NOT run again. The deferral is
            // released because the outgoing task is NOT `Faulted` — the drain's reverify would
            // decline it anyway, and leaving it armed would strand the CPU.
            crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
            crate::yarm_log!(
                "TERMINAL_FAULT_SPLIT_FAILED_CLOSED cpu={} tid={} captured={}",
                cpu.0,
                facts.tid,
                u8::from(captured)
            );
            return D::Complete(Ok(()));
        }
    }
    crate::yarm_log!(
        "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=terminal_fault_switch_required tid={} cpu={}",
        facts.tid,
        cpu_idx
    );
    D::QueueAdvanceCommitted
}

#[cfg(feature = "hosted-dev")]
fn try_split_terminal_page_fault_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _fault: Option<crate::kernel::trap::FaultInfo>,
    _frame: Option<&crate::kernel::trapframe::TrapFrame>,
) -> SplitDispatchDisposition {
    SplitDispatchDisposition::NotHandled
}

/// U9-COW1 §2 — service the witnessed x86_64 private-copy COW PageFault off the broad lock.
///
/// Admits ONE class and nothing else: an x86_64 user WRITE fault on a present, non-writable,
/// COW-marked page belonging to the exact task the classifier saw. Everything else — a read
/// fault, another architecture, a demand candidate, a terminal fault, an already-writable page
/// (the broad `path=already_writable` arm, which has zero live witnesses) — declines here having
/// touched nothing and reaches the unchanged broad dispatcher.
///
/// ## What the disposition means for this class
///
/// The broad arm's COW success is `return Ok(())`: no frame writeback, no scheduler change, no
/// queue advance. The trap returns through the architecture epilogue and the faulting instruction
/// re-executes against the now-writable private copy. That is exactly `Complete(Ok(()))`, and it
/// is why this route publishes no deferral and needs no drain — there is nothing to drain.
///
/// ## Why a post-mutation failure is `Complete(Err(..))`, not `NotHandled`
///
/// After the transaction has allocated a frame, falling back would let the broad arm allocate a
/// SECOND one for a fault it never saw declined. The recovery rolls its own allocation back
/// exactly, and the trap then carries the same error the broad arm would have produced.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_cow_page_fault_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    fault: Option<crate::kernel::trap::FaultInfo>,
) -> SplitDispatchDisposition {
    use crate::kernel::boot::{CowRecovery as R, PageFaultRoute, page_fault_route_for};
    use crate::kernel::trap::FaultAccess;
    use SplitDispatchDisposition as D;

    // U9-A64-COW2 §4: x86_64 and AArch64. RISC-V is deliberately absent — it has no independent
    // COW witness of its own, and §3 admits a class only on one.
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return D::NotHandled;
    };
    let Some(fault) = fault else {
        return D::NotHandled;
    };
    // The broad arm attempts COW only for writes, before anything else. Same screen, same place.
    if !matches!(fault.access, FaultAccess::Write) {
        return D::NotHandled;
    }
    // (1) Classify off-lock, through the ONE evaluator the broad arm uses. A stale identity
    // refuses here, before anything is decided.
    let Ok((class, Some(facts))) = shared.classify_page_fault_shared(cpu, fault) else {
        return D::NotHandled;
    };
    if !matches!(page_fault_route_for(arch, class), PageFaultRoute::SplitCow) {
        return D::NotHandled;
    }
    // (2) This route owns the PRIVATE-COPY arm only. An already-writable COW page is the broad
    // arm's other branch — a bare mark clear with no allocation, no copy and no witness — and it
    // stays there rather than being reimplemented for a case nothing exercises.
    if facts.mapping_writable || !facts.mapping_present {
        return D::NotHandled;
    }
    // The marker the broad arm prints on entry, in the broad arm's position: this route
    // intercepts before it, and the stream an observer sees must not change because the owner did.
    crate::yarm_log!(
        "PAGE_FAULT_ENTRY tid={} addr=0x{:x} access={:?} rip=0x{:x}",
        facts.tid,
        fault.addr.0,
        fault.access,
        0
    );
    if crate::kernel::boot::vm_cow_enabled() {
        crate::yarm_log!(
            "VM_COW_FAULT_BEGIN asid={} va=0x{:x}",
            facts.asid.0,
            facts.page.0
        );
    }
    // (3) The transaction. Everything fallible is revalidated inside it before the first
    // mutation, so a `Refused*` outcome is still safe to hand to the broad arm.
    let outcome = shared.cow_recover_private_copy_split(facts);
    match outcome {
        R::Committed {
            old_phys,
            new_phys,
            shootdown_acked,
        } => {
            if crate::kernel::boot::vm_cow_enabled() {
                crate::yarm_log!(
                    "VM_COW_PHASE_METADATA asid={} va=0x{:x} writable=0",
                    facts.asid.0,
                    facts.page.0
                );
                crate::yarm_log!(
                    "VM_COW_PHASE_FRAME_ALLOC asid={} va=0x{:x} new_pa=0x{:x}",
                    facts.asid.0,
                    facts.page.0,
                    new_phys.0
                );
                crate::yarm_log!(
                    "VM_COW_PHASE_PT_UPDATE asid={} va=0x{:x}",
                    facts.asid.0,
                    facts.page.0
                );
                crate::yarm_log!(
                    "VM_TLB_LOCAL_FLUSH asid={} va=0x{:x}",
                    facts.asid.0,
                    facts.page.0
                );
                crate::yarm_log!(
                    "VM_COW_PHASE_TLB_FLUSH asid={} va=0x{:x}",
                    facts.asid.0,
                    facts.page.0
                );
                crate::yarm_log!(
                    "VM_COW_DONE asid={} va=0x{:x} path=private_copy",
                    facts.asid.0,
                    facts.page.0
                );
            }
            crate::yarm_log!(
                "VM_COW_SPLIT_COMMITTED cpu={} tid={} asid={} va=0x{:x} old_pa=0x{:x} new_pa=0x{:x} acked={}",
                cpu.0,
                facts.tid,
                facts.asid.0,
                facts.page.0,
                old_phys.0,
                new_phys.0,
                u8::from(shootdown_acked)
            );
            crate::yarm_log!("PAGE_FAULT_HANDLED_COW");
            if crate::kernel::boot::fault_delivery_enabled() {
                crate::yarm_log!("FAULT_DELIVERY_CLASSIFY_HANDLED kind=cow");
            }
            D::Complete(Ok(()))
        }
        other if other.may_fall_back_to_broad() => {
            crate::yarm_log!(
                "VM_COW_SPLIT_REFUSED cpu={} tid={} va=0x{:x} reason={}",
                cpu.0,
                facts.tid,
                facts.page.0,
                other.reason()
            );
            D::NotHandled
        }
        other => {
            // Post-allocation failure. The allocation is rolled back; the broad arm must not
            // run, so this carries the error the broad arm's own failure would have carried.
            crate::yarm_log!(
                "VM_COW_SPLIT_FAILED_CLOSED cpu={} tid={} va=0x{:x} reason={}",
                cpu.0,
                facts.tid,
                facts.page.0,
                other.reason()
            );
            if crate::kernel::boot::vm_cow_enabled() {
                crate::yarm_log!(
                    "VM_COW_FAIL reason={} asid={} va=0x{:x}",
                    other.reason(),
                    facts.asid.0,
                    facts.page.0
                );
            }
            // The EXACT error the broad arm produces at the same step, through the SAME
            // `KernelError -> SyscallError` conversion its `map_err` chain uses.
            D::Complete(Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::from(
                    other
                        .kernel_error()
                        .unwrap_or(crate::kernel::boot::KernelError::UserMemoryFault),
                ),
            )))
        }
    }
}

#[cfg(feature = "hosted-dev")]
fn try_split_cow_page_fault_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _fault: Option<crate::kernel::trap::FaultInfo>,
) -> SplitDispatchDisposition {
    SplitDispatchDisposition::NotHandled
}

/// U9-COW1 — the bridge entry for the x86_64 private-copy COW PageFault route.
///
/// Tried BEFORE the terminal route, because that is the order the broad arm uses: COW first and
/// only for writes, then the demand screen, then the terminal fall-through. The two are mutually
/// exclusive by class anyway — `page_fault_route_for` maps `CowCandidate` to `SplitCow` and
/// `TerminallyUnhandled` to `SplitTerminal`, never both — but preserving the order is what makes
/// "split and broad cannot disagree about a fault" true for the sequencing as well as the verdict.
pub(crate) fn try_split_cow_page_fault_dispatch(
    shared: &SharedKernel,
    cpu: CpuId,
    fault: Option<crate::kernel::trap::FaultInfo>,
) -> SplitDispatchDisposition {
    try_split_cow_page_fault_into_frame(shared, cpu, fault)
}

/// U9-FT4 — the bridge entry for the terminal PageFault route.
pub(crate) fn try_split_terminal_page_fault_dispatch(
    shared: &SharedKernel,
    cpu: CpuId,
    fault: Option<crate::kernel::trap::FaultInfo>,
    frame: Option<&crate::kernel::trapframe::TrapFrame>,
) -> SplitDispatchDisposition {
    try_split_terminal_page_fault_into_frame(shared, cpu, fault, frame)
}

#[derive(Debug)]
pub(crate) enum SplitDispatchDisposition {
    /// The split route declined BEFORE it mutated anything. This is the ONLY route to a
    /// fallback, and it is what makes the fallback safe: the trap enters the unchanged broad
    /// dispatcher exactly as if the split route had never been consulted.
    NotHandled,
    /// A NON-SWITCHING split class serviced the syscall completely and wrote its result into the
    /// frame. `Ok` is a success, `Err(Syscall(_))` an ordinary user-visible error the frame
    /// carries back, any other `Err` a genuine kernel-side failure. The caller returns through
    /// the architecture epilogue and runs NO queue-advance drain — there is nothing to drain.
    Complete(Result<(), TrapHandleError>),
    /// A terminal transition has been PUBLISHED: the caller is blocked/preempted/faulted, the
    /// current slot is clear, and the exact outgoing identity is carried on the existing
    /// per-CPU deferral. From here fallback is structurally impossible. The caller must NOT
    /// enter the broad dispatcher and must NOT return through the outgoing frame; it falls
    /// through to the existing post-lock drains, which consume that one deferral and settle the
    /// trap as Switch, ResumeSame or TerminalIdle.
    QueueAdvanceCommitted,
    /// U9-TM §3 — the route finished its own work, mutated no scheduler state, and still owes the
    /// architecture tail's POST-WORK.
    ///
    /// `Complete` is wrong for this: it returns through the epilogue immediately, and the trap
    /// entry's `run_due_ipc_timeout_work` sits far below that early return — so a `Complete`
    /// timer tick would silently skip the production timeout pipeline that owns all three
    /// timeout classes.
    ///
    /// `QueueAdvanceCommitted` is wrong too, and more dangerously: it means a terminal
    /// transition was published, which for a NON-preempting tick is simply false. Using it would
    /// send a tick that changed no scheduler state into the queue-advance drains.
    ///
    /// So this is its own outcome: the broad dispatcher is skipped, NO queue selection runs, the
    /// existing post-work drains run exactly once, and the trap then settles through its normal
    /// frame/idle path. It is decided by the ROUTE, never inferred from a non-empty stash.
    ///
    /// 199G-C4 §2 — `finalize_syscall` says whether the CALLER's syscall is finished. A timer
    /// tick has no syscall to finish and passes `false`. An `IpcSend` that delivered or enqueued
    /// finished its caller's syscall and passes `true`, so the architecture syscall-return ABI
    /// runs before the drain. An `IpcSend` that is about to PARK its caller passes `false`: the
    /// sender's result arrives from the completion its waker publishes, and advancing its PC or
    /// exporting a result here would hand a blocked task an answer to a send that has not
    /// happened. Like the disposition itself this is decided by the route, never inferred from
    /// what is in the stash.
    PostWorkCommitted { finalize_syscall: bool },
}

impl SplitDispatchDisposition {
    /// The two-valued answer this seam gave before U9-QA.
    ///
    /// The five existing non-switching split classes must behave EXACTLY as they did, and their
    /// coverage is written against that older shape. Rather than restate every one of those
    /// assertions in new terms — which would quietly relicense what they prove — this maps the
    /// two dispositions those classes can produce back onto it, so the cases keep asserting the
    /// same facts about the same code.
    ///
    /// `QueueAdvanceCommitted` deliberately has NO legacy form. It is precisely the state the
    /// old type could not express, and flattening it to either `None` or `Some` would reintroduce
    /// one of the two mistakes the third variant exists to prevent, so it panics instead.
    #[cfg(test)]
    pub(crate) fn legacy(self) -> Option<Result<(), TrapHandleError>> {
        match self {
            Self::NotHandled => None,
            Self::Complete(result) => Some(result),
            Self::QueueAdvanceCommitted => {
                panic!("a committed queue advance has no pre-U9-QA equivalent")
            }
            Self::PostWorkCommitted { .. } => {
                panic!("a committed post-work outcome has no pre-U9-QA equivalent")
            }
        }
    }
}

/// U9-QA §2 — the pre-lock split dispatcher.
///
/// FutexWait is tried first and separately because it is the only SWITCHING class: it must never
/// reach the NR-only whitelist, whose whole contract is that every class on it is non-switching
/// and may be early-returned. Every other class goes to the unchanged non-switching dispatcher
/// and keeps its exact previous behavior — `None` becomes `NotHandled`, `Some(r)` becomes
/// `Complete(r)`, and nothing about how those five are serviced changes.
/// U9-TM §2 — the pre-lock TIMER entry point.
///
/// Separate from [`try_split_dispatch_into_frame`] because a timer interrupt is not a syscall:
/// it carries no NR, no ABI and no frame arguments, and the syscall dispatcher's whole default-
/// deny structure is written against those. Keeping them apart is what stops a timer trap being
/// classified by a syscall whitelist it has no business reaching.
pub(crate) fn try_split_timer_dispatch(
    shared: &SharedKernel,
    cpu: CpuId,
    is_timer: bool,
) -> SplitDispatchDisposition {
    try_split_timer_into_frame(shared, cpu, is_timer)
}

pub(crate) fn try_split_dispatch_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> SplitDispatchDisposition {
    match try_split_futex_wait_into_frame(shared, cpu, frame) {
        SplitDispatchDisposition::NotHandled => {}
        handled => return handled,
    }
    // U9-RX3 §3 — the SECOND switching class. It is tried before the non-switching dispatcher for
    // the same reason FutexWait is: the NR-only whitelist's contract is that every class on it may
    // be early-returned through the caller's own frame, and a blocking receive may not.
    //
    // It runs AFTER the non-blocking queued-plain recv would have, in the sense that matters:
    // this route admits ONLY the state in which that one declines (an empty buffered endpoint with
    // no waiters), so the two never contend for the same trap.
    match try_split_blocking_ipc_recv_into_frame(shared, cpu, frame) {
        SplitDispatchDisposition::NotHandled => {}
        handled => return handled,
    }
    // 199G-C4 §2 — the THIRD class that may not be early-returned through the caller's own
    // frame. `IpcSend` produces all three committed shapes: a completed syscall, a delivery the
    // post-work drain owes, and a parked sender whose queue advance the drain performs. Like
    // the two above it, it is tried before the NR-only whitelist, whose whole contract is that
    // everything on it is non-switching.
    match try_split_ipc_send_into_frame(shared, cpu, frame) {
        SplitDispatchDisposition::NotHandled => {}
        handled => return handled,
    }
    // U9-EXIT1 §5 — the FOURTH switching class, and the only one that never returns at all. It is
    // tried here for the same reason the three above it are: the NR-only whitelist's contract is
    // that every class on it may be early-returned through the caller's own frame, and an exiting
    // task has no frame to return through. It answers `QueueAdvanceCommitted` so the EXISTING
    // post-lock drain — the one FutexWait and the terminal fault already share — selects and
    // applies the next context.
    match try_split_exit_current_task(shared, cpu) {
        SplitDispatchDisposition::NotHandled => {}
        handled => return handled,
    }
    match try_split_dispatch_nonswitching_into_frame(shared, cpu, frame) {
        None => SplitDispatchDisposition::NotHandled,
        Some(result) => SplitDispatchDisposition::Complete(result),
    }
}

/// 199G-C4 §1 — service `IpcSend` (NR 1) off the broad lock, on all three architectures.
///
/// This is the LAST syscall family that could still reach a terminal broad dispatcher. It adds
/// no policy: every decision below belongs to an owner §1–§3 extracted, and this function is the
/// order in which they are consulted.
///
/// ## Ordering, and why each step is where it is
///
/// `decode/admit → copy/snapshot → acquire pin if owed → rank-3 commit → disposition`
///
/// Everything that can refuse comes before anything that can be consumed, so a decline is
/// always safe to hand back to the broad path. Once the transfer envelope is stashed — which is
/// also where a shared-region grant's pin is acquired — falling back would re-run the whole
/// send and stash a SECOND envelope for one syscall, so from that point every exit settles
/// through the split owners instead.
///
/// ## The two impossible classes
///
/// A `Kernel` capability and a `Synchronous` endpoint are both production-unreachable (199G-C2
/// §1, 199D-KR §1). They are refused here BEFORE anything is consumed, with a typed invariant
/// error rather than a fallback: handing an impossible class to the broad dispatcher would be
/// the one edge this stage exists to remove, and it would be an edge no production trap can
/// ever take.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_ipc_send_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> SplitDispatchDisposition {
    use crate::kernel::capabilities::{CapId, CapObject, CapRights};
    use crate::kernel::ipc::{EndpointMode, SharedMemoryRegion};
    use crate::kernel::syscall::{
        IpcSendPayloadShape, REPLY_CAP_QUEUEING_SUPPORTED, SYSCALL_ARG_CAP,
        SYSCALL_ARG_INLINE_PAYLOAD0, SYSCALL_ARG_INLINE_PAYLOAD1, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR,
        SyscallError, classify_ipc_send_payload_shape, frame_ipc_send_message,
        transfer_cap_arg_present,
    };
    use SplitDispatchDisposition as D;

    // ── (1) NR and CPU ──────────────────────────────────────────────────────────────────────
    if !matches!(Syscall::decode(frame.syscall_num()), Ok(Syscall::IpcSend)) {
        return D::NotHandled;
    }
    let cpu_idx = cpu.0 as usize;
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return D::NotHandled;
    }
    let Some(tid) = shared.current_tid_authoritative(cpu) else {
        crate::yarm_log!(
            "IPC_SEND_SPLIT_REFUSED cpu={} reason=no_current_task",
            cpu.0
        );
        return D::NotHandled;
    };

    // Helper: a completed syscall's frame result, exactly as the broad handler writes it.
    let complete_ok = |frame: &mut TrapFrame| {
        frame.set_ok(0, 0, 0);
        frame.set_ret2(
            usize::try_from(crate::kernel::syscall::SYSCALL_NO_TRANSFER_CAP).unwrap_or(0),
        );
    };

    // ── (2) ADMIT: the send capability ──────────────────────────────────────────────────────
    // The same four questions `validate_endpoint_right` asks, in the same order and with the
    // same errors: resolvable, live, an endpoint, and carrying SEND.
    let cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let Ok(capability) = shared.resolve_capability_for_task_split(tid, cap) else {
        return D::Complete(Err(TrapHandleError::Syscall(
            SyscallError::InvalidCapability,
        )));
    };
    if !shared.sr_object_live_split(capability.object) {
        return D::Complete(Err(TrapHandleError::Syscall(
            SyscallError::InvalidCapability,
        )));
    }
    let endpoint = capability.object;
    if !matches!(endpoint, CapObject::Endpoint { .. }) {
        // 199G-C4 §4 — this is where a `Kernel` capability would arrive, and it fails closed
        // here having touched nothing. `handle_ipc_send` refuses it at exactly this question
        // too, which is why `ipc_send_routed`'s restart-control branch was never reachable
        // through NR 1 in the first place.
        if endpoint == CapObject::Kernel {
            crate::yarm_log!(
                "IPC_SEND_SPLIT_INVARIANT cpu={} tid={} reason=kernel_cap_send result=failed_closed",
                cpu.0,
                tid
            );
        }
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::WrongObject)));
    }
    if !capability.has_right(CapRights::SEND) {
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::MissingRight)));
    }
    let Ok(endpoint_idx) = shared.resolve_endpoint_index_split(endpoint) else {
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::WrongObject)));
    };
    let CapObject::Endpoint {
        generation: endpoint_generation,
        ..
    } = endpoint
    else {
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::WrongObject)));
    };

    // ── (3) ADMIT: the endpoint mode ────────────────────────────────────────────────────────
    // Buffered is the only production mode (199G-C2 §1: `Synchronous` has a private field, no
    // setter, no deserializer, and every constructor that names it is test-only).
    match shared.endpoint_mode_split_read(endpoint_idx, endpoint_generation) {
        Some(EndpointMode::Buffered) => {}
        Some(EndpointMode::Synchronous) => {
            crate::yarm_log!(
                "IPC_SEND_SPLIT_INVARIANT cpu={} tid={} endpoint={} reason=synchronous_endpoint result=failed_closed",
                cpu.0,
                tid,
                endpoint_idx
            );
            return D::Complete(Err(TrapHandleError::Syscall(SyscallError::WrongObject)));
        }
        None => return D::Complete(Err(TrapHandleError::Syscall(SyscallError::WrongObject))),
    }

    // ── (4) ADMIT: the transfer capability ──────────────────────────────────────────────────
    let transfer_cap = if transfer_cap_arg_present(frame) {
        Some(CapId(
            frame.arg(crate::kernel::syscall::SYSCALL_ARG_TRANSFER_CAP) as u64,
        ))
    } else {
        None
    };
    let transfer_object = match transfer_cap {
        None => None,
        Some(tc) => match shared.resolve_capability_for_task_split(tid, tc) {
            Ok(c) => Some(c),
            Err(_) => {
                return D::Complete(Err(TrapHandleError::Syscall(
                    SyscallError::InvalidCapability,
                )));
            }
        },
    };

    // ── (5) ADMIT: reply capabilities are direct-delivery only ──────────────────────────────
    // Stage 198D-S: a Reply is never stored in an endpoint queue, so with no compatible
    // receiver ready the send is refused BEFORE any envelope exists.
    if !REPLY_CAP_QUEUEING_SUPPORTED
        && matches!(
            transfer_object.map(|c| c.object),
            Some(CapObject::Reply { .. })
        )
    {
        let ready = shared
            .endpoint_waiter_tid_split_read(endpoint_idx)
            .is_some_and(|rt| shared.is_task_recv_v2_blocked_split_read(rt.0));
        if !ready {
            crate::yarm_log!(
                "IPC_SEND_REPLY_CAP_DIRECT_ONLY tid={} reason=no_blocked_receiver",
                tid
            );
            return D::Complete(Err(TrapHandleError::Syscall(SyscallError::WouldBlock)));
        }
    }

    // ── (6) ADMIT: sender class, payload shape and timeout ──────────────────────────────────
    let sender_asid = shared.task_asid_opt_split_read(tid);
    let sender_has_user_asid = sender_asid.is_some();
    let len = frame.arg(SYSCALL_ARG_LEN);
    let user_ptr_or_offset = frame.arg(SYSCALL_ARG_PTR);
    let send_timeout_ticks = if sender_has_user_asid || len == 0 {
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1) as u64
    } else {
        0
    };
    let Ok(shape) = classify_ipc_send_payload_shape(sender_has_user_asid, len) else {
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::InvalidArgs)));
    };

    // ── (7) COPY/SNAPSHOT: the payload, still consuming nothing ─────────────────────────────
    let mut payload_buf = [0u8; crate::kernel::ipc::Message::MAX_PAYLOAD];
    let shared_region = match shape {
        IpcSendPayloadShape::SharedRegion => {
            let Some(grant) = transfer_object else {
                return D::Complete(Err(TrapHandleError::Syscall(SyscallError::InvalidArgs)));
            };
            match grant.object {
                CapObject::MemoryObject { .. } | CapObject::DmaRegion { .. } => {}
                _ => return D::Complete(Err(TrapHandleError::Syscall(SyscallError::WrongObject))),
            }
            if !grant.has_right(CapRights::READ) || !grant.has_right(CapRights::MAP) {
                return D::Complete(Err(TrapHandleError::Syscall(SyscallError::MissingRight)));
            }
            if sender_has_user_asid
                && crate::kernel::syscall::validate_user_region(
                    user_ptr_or_offset as u64,
                    len as u64,
                )
                .is_err()
            {
                return D::Complete(Err(TrapHandleError::Syscall(SyscallError::InvalidArgs)));
            }
            let region = SharedMemoryRegion {
                offset: user_ptr_or_offset as u64,
                len: len as u64,
            };
            let encoded = region.encode();
            payload_buf[..encoded.len()].copy_from_slice(&encoded);
            Some((
                encoded.len(),
                crate::kernel::boot::TransferSharedRegion {
                    offset: region.offset,
                    len: region.len,
                },
            ))
        }
        IpcSendPayloadShape::Inline => {
            if let Some(asid) = sender_asid {
                // A user sender's payload lives in ITS address space; a fault here is the
                // ordinary user-memory fault the broad handler reports, and nothing is consumed.
                let Some(bytes) =
                    shared.copy_from_user_asid_split_read(asid.0 as u64, user_ptr_or_offset, len)
                else {
                    crate::yarm_log!(
                        "IPC_SEND_SPLIT_REFUSED cpu={} tid={} reason=payload_fault",
                        cpu.0,
                        tid
                    );
                    return D::NotHandled;
                };
                payload_buf[..len].copy_from_slice(&bytes[..len]);
            } else {
                // A kernel task's payload rides in the argument registers.
                let words = [
                    frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
                    frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
                ];
                let Some(regs) = crate::kernel::ipc::unpack_register_payload(words, len) else {
                    return D::Complete(Err(TrapHandleError::Syscall(SyscallError::InvalidArgs)));
                };
                payload_buf[..len].copy_from_slice(&regs[..len]);
            }
            None
        }
    };
    let payload_len = shared_region.map_or(len, |(l, _)| l);

    // ── (8) ACQUIRE: stash the envelope, taking the pin iff the descriptor owes one ─────────
    // THE first consuming step. From here a decline is no longer safe: re-running the send
    // would stash a second envelope for one syscall.
    let (transfer_handle, stashed_pin_owed, stash_bound_receiver) = match transfer_cap {
        None => (None, false, None),
        Some(source_cap) => {
            let bound = shared.endpoint_waiter_tid_split_read(endpoint_idx);
            match shared.stash_transfer_envelope_split(
                crate::kernel::ipc::ThreadId(tid),
                source_cap,
                endpoint,
                bound,
                shared_region.map(|(_, r)| r),
            ) {
                Ok(stashed) => (Some(stashed.handle), stashed.pin.is_some(), bound),
                Err(refusal) => {
                    crate::yarm_log!(
                        "IPC_SEND_SPLIT_REFUSED cpu={} tid={} reason=envelope_stash slug={:?}",
                        cpu.0,
                        tid,
                        refusal
                    );
                    return D::Complete(Err(TrapHandleError::Syscall(
                        SyscallError::InvalidCapability,
                    )));
                }
            }
        }
    };
    let _ = stashed_pin_owed;
    let Ok(msg) = frame_ipc_send_message(
        tid,
        shape,
        &payload_buf[..payload_len],
        transfer_cap,
        transfer_handle,
    ) else {
        settle_ipc_send_envelope(
            shared,
            transfer_handle,
            endpoint_idx,
            stash_bound_receiver,
            tid,
        );
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::InvalidArgs)));
    };

    // ── (9) COMMIT: the authoritative send sequence ─────────────────────────────────────────
    // Exactly the order `ipc_send_routed` follows for a buffered endpoint: a recv-v2 blocked
    // waiter takes a direct delivery, everything else enqueues, and a full endpoint parks the
    // sender.
    let waiter = shared.endpoint_waiter_tid_split_read(endpoint_idx);
    if let Some(waiter_tid) = waiter
        && shared.is_task_recv_v2_blocked_split_read(waiter_tid.0)
    {
        crate::yarm_log!(
            "IPC_RECV_DELIVER_TO_WAITER tid={} endpoint={} len={} reply_cap={}",
            waiter_tid.0,
            endpoint_idx,
            msg.len,
            msg.transferred_cap().map(|c| c.0).unwrap_or(u64::MAX)
        );
        // The four producers, in the order `try_ipc_send_boundary_split_any_pub` uses, plus the
        // shared-region class the broad router tries last. Each declines having consumed
        // nothing, so trying them in order costs nothing.
        //
        // Each success TAGS THE STASH ORIGIN, exactly as the broad boundary wrappers do. The
        // tag is what makes the drain emit this class's `IPC_SEND_BOUNDARY_*` markers instead
        // of the generic delivery ones — the same existing marker family, from the same drain,
        // reached by a different route. Without it a delivery that really happened would report
        // itself as some other class's, and every live IpcSend witness reads those markers.
        crate::yarm_log!(
            "IPC_SEND_BOUNDARY_SPLIT_BEGIN waiter_tid={} endpoint={}",
            waiter_tid.0,
            endpoint_idx
        );
        let produced = shared
            .produce_blocked_waiter_plain_delivery_split(waiter_tid.0, endpoint_idx, &msg)
            .map(|done| {
                if done {
                    crate::kernel::boot::ipc_send_boundary_origin_set(cpu_idx);
                    crate::yarm_log!(
                        "IPC_SEND_BOUNDARY_PLAIN_SNAPSHOT_OK waiter_tid={}",
                        waiter_tid.0
                    );
                }
                done
            })
            .and_then(|done| {
                if done {
                    return Ok(true);
                }
                crate::yarm_log!(
                    "IPC_SEND_REPLY_CAP_BOUNDARY_SPLIT_BEGIN waiter_tid={} endpoint={}",
                    waiter_tid.0,
                    endpoint_idx
                );
                shared
                    .produce_blocked_waiter_reply_cap_delivery_split(
                        waiter_tid.0,
                        endpoint_idx,
                        &msg,
                    )
                    .map(|done| {
                        if done {
                            crate::kernel::boot::ipc_send_reply_cap_boundary_origin_set(cpu_idx);
                            crate::yarm_log!(
                                "IPC_SEND_REPLY_CAP_BOUNDARY_SNAPSHOT_OK waiter_tid={}",
                                waiter_tid.0
                            );
                        }
                        done
                    })
            })
            .and_then(|done| {
                if done {
                    return Ok(true);
                }
                crate::yarm_log!(
                    "IPC_SEND_CAP_BOUNDARY_SPLIT_BEGIN waiter_tid={} endpoint={}",
                    waiter_tid.0,
                    endpoint_idx
                );
                shared
                    .produce_blocked_waiter_ordinary_cap_delivery_split(
                        waiter_tid.0,
                        endpoint_idx,
                        &msg,
                        // IpcSend origin: no reply record, no terminal, nothing owed.
                        None,
                    )
                    .map(|done| {
                        if done {
                            crate::kernel::boot::ipc_send_cap_boundary_origin_set(cpu_idx);
                            crate::yarm_log!(
                                "IPC_SEND_CAP_BOUNDARY_SNAPSHOT_OK waiter_tid={}",
                                waiter_tid.0
                            );
                        }
                        done
                    })
            })
            .and_then(|done| {
                if done {
                    Ok(true)
                } else {
                    // The shared-region producer tags its own origin through
                    // `stash_shared_region_delivery(.., SharedRegionLiveOrigin::Direct)`.
                    shared.produce_blocked_waiter_shared_region_delivery_split(
                        waiter_tid.0,
                        endpoint_idx,
                        &msg,
                    )
                }
            });
        match produced {
            Ok(true) => {
                // The drain completes the copy/materialize, clears the waiter slot and wakes it
                // exactly once. The SENDER's syscall is finished, so its result goes in now.
                complete_ok(frame);
                crate::yarm_log!(
                    "IPC_SEND_SPLIT_DONE cpu={} tid={} endpoint={} result=direct_delivery",
                    cpu.0,
                    tid,
                    endpoint_idx
                );
                return D::PostWorkCommitted {
                    finalize_syscall: true,
                };
            }
            Ok(false) => {
                // No producer claimed a recv-v2 blocked waiter. In production this is
                // unreachable — the four classes are exhaustive over the messages NR 1 can
                // build, and the trap-entry drainer is active by construction here — so it
                // fails closed rather than re-running the send under the broad lock.
                settle_ipc_send_envelope(
                    shared,
                    transfer_handle,
                    endpoint_idx,
                    stash_bound_receiver,
                    tid,
                );
                crate::yarm_log!(
                    "IPC_SEND_SPLIT_INVARIANT cpu={} tid={} endpoint={} reason=no_delivery_owner result=failed_closed",
                    cpu.0,
                    tid,
                    endpoint_idx
                );
                return D::Complete(Err(TrapHandleError::Syscall(SyscallError::Internal)));
            }
            Err(err) => {
                // A real Phase-A error. The envelope disposition is the producer's; anything it
                // left stashed is settled here, exactly as the broad error path settles it.
                settle_ipc_send_envelope(
                    shared,
                    transfer_handle,
                    endpoint_idx,
                    stash_bound_receiver,
                    tid,
                );
                crate::yarm_log!(
                    "IPC_SEND_SPLIT_DONE cpu={} tid={} endpoint={} result=delivery_error code={}",
                    cpu.0,
                    tid,
                    endpoint_idx,
                    err.code()
                );
                return D::Complete(Err(TrapHandleError::Syscall(err)));
            }
        }
    }

    // No recv-v2 waiter: the authoritative unconditional enqueue.
    //
    // The Stage-193E enqueue boundary's markers are emitted around it, for the same reason the
    // delivery classes' are: this route now OWNS the boundary, and every live IpcSend witness
    // reads this family. The wrapper is not called — the directive names the authoritative
    // unconditional enqueue as the final enqueue policy, and the wrapper wraps the conservative
    // Stage-4E screen — so the markers come from the route, unchanged in name and meaning.
    crate::yarm_log!(
        "IPC_SEND_ENQUEUE_BOUNDARY_SPLIT_BEGIN endpoint={} len={}",
        endpoint_idx,
        msg.as_slice().len()
    );
    // Phase A: the payload/meta are snapshotted by value — no user copy, no materialization.
    crate::yarm_log!(
        "IPC_SEND_ENQUEUE_BOUNDARY_SNAPSHOT_OK endpoint={}",
        endpoint_idx
    );
    match shared.ipc_endpoint_enqueue_authoritative_split(endpoint_idx, msg) {
        Err(_) => {
            settle_ipc_send_envelope(
                shared,
                transfer_handle,
                endpoint_idx,
                stash_bound_receiver,
                tid,
            );
            D::Complete(Err(TrapHandleError::Syscall(SyscallError::WrongObject)))
        }
        Ok(true) => {
            // Enqueued exactly once into the endpoint queue.
            crate::yarm_log!(
                "IPC_SEND_ENQUEUE_BOUNDARY_ENQUEUE_OK endpoint={}",
                endpoint_idx
            );
            // Sender state matches legacy: a send that enqueues does NOT block the sender and
            // is NOT published as a sender-waiter — it returns Ok and continues.
            crate::yarm_log!(
                "IPC_SEND_ENQUEUE_BOUNDARY_SENDER_STATE_OK endpoint={} sender_blocked=0",
                endpoint_idx
            );
            crate::yarm_log!(
                "IPC_SEND_ENQUEUE_BOUNDARY_SPLIT_DONE result=ok endpoint={}",
                endpoint_idx
            );
            crate::kernel::boot::maybe_log_ipc_send_plain_enqueue_retired();
            // Wake any legacy waiter through the one shared owner, then finish.
            let _ = shared.wake_waiter_for_endpoint_split(cpu, endpoint_idx);
            complete_ok(frame);
            crate::yarm_log!(
                "IPC_SEND_SPLIT_DONE cpu={} tid={} endpoint={} result=enqueued",
                cpu.0,
                tid,
                endpoint_idx
            );
            D::Complete(Ok(()))
        }
        Ok(false) => {
            // The endpoint is full: park the sender through the EXISTING U6 publication owner.
            // The route stashes the proposal; the post-work drain runs the rank-ordered
            // transaction, arms the established D2-send deferral on success, and on refusal
            // settles this same envelope and encodes the canonical error into this frame. No
            // result is written here: a parked sender's answer comes from its waker.
            let Some(sender_asid) = sender_asid else {
                // A kernel task cannot park on a send: it has no incarnation ASID for the
                // transaction's identity check. Settle and refuse, as the broad path does.
                settle_ipc_send_envelope(
                    shared,
                    transfer_handle,
                    endpoint_idx,
                    stash_bound_receiver,
                    tid,
                );
                return D::Complete(Err(TrapHandleError::Syscall(SyscallError::WouldBlock)));
            };
            let deadline = if send_timeout_ticks == 0 {
                None
            } else {
                Some(
                    shared
                        .scheduler_tick_now_split_read()
                        .wrapping_add(send_timeout_ticks),
                )
            };
            let snapshot = crate::kernel::dispatch_post_work::BlockingSendCommitSnapshot {
                cpu,
                sender_tid: tid,
                sender_asid,
                endpoint_idx,
                endpoint_generation,
                send_cap: cap,
                msg,
                deadline,
                transfer_envelope: transfer_handle.map(|handle| {
                    crate::kernel::dispatch_post_work::BlockingSendEnvelopeCleanup {
                        handle,
                        endpoint_idx,
                        cleanup_tid: stash_bound_receiver
                            .unwrap_or(crate::kernel::ipc::ThreadId(tid)),
                    }
                }),
            };
            // SAFETY: local-CPU trap path, interrupts disabled, no concurrent access —
            // identical discipline to every other producer's store.
            unsafe {
                crate::kernel::boot::DISPATCH_POST_WORK_STASH[cpu_idx].store(
                    crate::kernel::dispatch_post_work::DispatchPostWork::BlockingSendCommit(
                        snapshot,
                    ),
                );
            }
            crate::yarm_log!(
                "IPC_SEND_SPLIT_DONE cpu={} tid={} endpoint={} result=blocking_publication_pending",
                cpu.0,
                tid,
                endpoint_idx
            );
            D::PostWorkCommitted {
                finalize_syscall: false,
            }
        }
    }
}

/// 199G-C4 §1 — settle a stashed transfer envelope on an NR 1 exit that is not a delivery.
///
/// One helper rather than five copies of the same three arguments, and it goes through the
/// EXISTING settle owner, which for a shared-region envelope also releases the transient pin
/// exactly once through the sequential rank-3 → rank-6 no-reclaim transaction.
#[cfg(not(feature = "hosted-dev"))]
fn settle_ipc_send_envelope(
    shared: &SharedKernel,
    handle: Option<u64>,
    endpoint_idx: usize,
    bound_receiver: Option<crate::kernel::ipc::ThreadId>,
    sender_tid: u64,
) {
    let Some(handle) = handle else {
        return;
    };
    let cleanup_tid = bound_receiver.unwrap_or(crate::kernel::ipc::ThreadId(sender_tid));
    shared.settle_blocked_send_envelope_split(handle, endpoint_idx, cleanup_tid);
}

#[cfg(feature = "hosted-dev")]
fn try_split_ipc_send_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> SplitDispatchDisposition {
    SplitDispatchDisposition::NotHandled
}

/// U9-RX3 §3 — service a BLOCKING `IpcRecv` (NR 2) off the broad lock.
///
/// This is the migration of the existing block-and-publish sequence onto the four SharedKernel
/// phase twins, reusing the deferral/drain topology the broad entry already publishes into. It
/// creates no drain, no syscall, no ABI lane and no marker family beyond its own attributed
/// refusals.
///
/// ## The ordering is forced by source, not chosen
///
/// `block_current_on_receive_with_deadline` runs scheduler(1) → task(2) → ipc(3), and the comment
/// above it says why the publish may not be hoisted ahead of the block: a sender that observes a
/// published waiter must also observe a `Blocked` TCB, or it will attempt direct delivery to a
/// task that is still `Running`. So the recheck-loses race (`QueueNonEmpty`) cannot be made
/// mutation-free, and this route must be able to UNDO the rank-1 block. That inverse is
/// `SharedKernel::recv_block_unwind_race_split`, and its existence is what makes this route
/// possible at all.
///
/// ## Steps, ordered so the last decline precedes the first mutation
///
/// 1. **NR + architecture** — `IpcRecv` only, and only where a live witness exists.
/// 2. **Publication gates** — the broad blocked-recv arm calls three `maybe_publish_*_ack` hooks
///    that take `&mut KernelState`. They are strict no-ops in the default configuration; when any
///    is armed this route refuses so the unchanged broad arm runs them exactly as before.
/// 3. **ABI** — decoded through the SAME canonical `RecvRequest` builder the broad entry uses.
///    Only a `recv-v2` request is admitted: it is the shape whose `BlockedRecvState` the four
///    `DispatchPostWork::BlockedWaiter*Delivery` classes complete by writeback. A legacy request
///    saves no state and is completed differently; it keeps the broad path.
/// 4. **Capability** — resolved off-lock through the existing task(2) → capability(4) split read.
/// 5. **Would-block** — the conservative rank-3 structural read. The broad entry answers this by
///    attempting the take; a pre-lock route cannot, because the attempt is the mutation.
/// 6. **Admission** — `queue_advance_admit_split`, plus this class's own precondition that no
///    same-class deferral is outstanding. Every refusal lands here, while fallback is still safe.
/// 7. **Reserve the deferral** — before any publication, so a reservation failure stays
///    pre-mutation. Holding it is what guarantees the drain applies an incoming context.
/// 8. **Phase A / B / C** — the three twins, in rank order. `QueueNonEmpty` runs the exact
///    inverse and falls back; the broad path then services the message that arrived.
/// 9. **`QueueAdvanceCommitted`** — the existing D2-recv drain consumes the one deferral,
///    re-verifies `Blocked(EndpointReceive)`, selects and resumes.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_blocking_ipc_recv_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> SplitDispatchDisposition {
    use crate::kernel::capabilities::{CapId, CapObject};
    use crate::kernel::recv_core::{RecvBlockingPolicy, RecvMetaTarget, RecvRequest};
    use crate::kernel::syscall::{
        SYSCALL_ARG_CAP, SYSCALL_ARG_INLINE_PAYLOAD0, SYSCALL_ARG_INLINE_PAYLOAD1, SYSCALL_ARG_LEN,
        SYSCALL_ARG_PTR, SYSCALL_ARG_TRANSFER_CAP,
    };
    use crate::kernel::task::BlockedRecvState;
    use SplitDispatchDisposition as D;

    // (1) NR, then architecture. The body is architecture-neutral; the gate names the two that
    // have the drain this route publishes into, and since U9-RX4 repaired the queued-plain
    // writeback both of them actually REACH it — AArch64's NR 2 ABI import is no longer held
    // back (see `pre_split_import_syscall_abi`). RISC-V is excluded for want of a live witness,
    // not for a structural reason: its D2-recv drain is the same shape.
    //
    // Stage 199G-B §2 — NR 5 (`ipc_recv_timeout`) joins NR 2 on THIS route rather than getting a
    // second one. The two differ in exactly three places, all of them named below: which ABI
    // builder decodes the request, whether a deadline is armed, and which `RecvAbiVariant` the
    // saved state carries. Everything between — the would-block read, the admission, the
    // deferral reservation, Phases A/B/C, the race unwind and the committed disposition — is the
    // same code servicing both, which is the only way "one delivery lifecycle" stays true.
    //
    // NR 5 is admitted on ALL THREE architectures. Its completion is the variant-driven writeback
    // plus the existing D2-recv drain, both architecture-neutral and all three already reached by
    // the receive-timeout scan that arms it, so RISC-V's want of an NR-2 witness (the reason the
    // gate below still excludes it) says nothing about NR 5.
    let recv_timeout = match Syscall::decode(frame.syscall_num()) {
        Ok(Syscall::IpcRecvTimeout) => true,
        Ok(Syscall::IpcRecv) => false,
        _ => return D::NotHandled,
    };
    if !recv_timeout && !cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        return D::NotHandled;
    }
    let cpu_idx = cpu.0 as usize;
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return D::NotHandled;
    }
    // (2) The publication work that still requires a broad `&KernelState`.
    //
    // Stage 199D-WA3C2 NARROWS this. It used to yield on `ipccall_direct_publication_enabled()`
    // outright, because the NR6/NR7 acknowledgements could only be published from the broad
    // blocked-recv arm — and with the x86_64 production default now ON, that yield would fire on
    // EVERY boot and silently retire the delivered split blocking-IpcRecv route. The publication
    // bodies are now shared (`publish_ipccall_direct_blocked_server_ack_with` /
    // `publish_ipcreply_direct_blocked_caller_ack_with`), so this route publishes its own
    // acknowledgements at step (10) below and no longer has to yield the whole receive.
    //
    // The shared-region oracle's acknowledgement family is unrelated to those two and still
    // broad-only, so it keeps its yield unconditionally.
    //
    // Stage 199G-B §2: all three yields below are scoped to NR 2. Every hook they protect lives in
    // `handle_ipc_recv` — `maybe_publish_shared_region_blocked_recv_ack`,
    // `maybe_publish_ipccall_direct_blocked_server_ack`,
    // `maybe_publish_ipcreply_direct_blocked_caller_ack` — and each one additionally refuses any
    // `RecvAbiVariant` but `RecvV2`. `handle_ipc_recv_timeout` calls none of them, so there is no
    // broad-arm publication for an NR 5 receive to yield BACK to, and yielding anyway would be
    // the one thing §4 forbids: an NR 5 edge into the terminal broad dispatcher.
    if !recv_timeout && cfg!(feature = "shared-region-direct-oracle") {
        crate::yarm_log!(
            "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} reason=shared_region_ack_publication_armed",
            cpu.0
        );
        return D::NotHandled;
    }
    // DIRECT3-CAP-FINAL §5 — ONE owner for this yield, on every architecture.
    //
    // The yield is scoped to an ARMED SELECTOR, never to the production default: a selector's
    // profile depends on blocked-recv work this route does not reproduce, while the ordinary
    // production configuration needs the route to keep the receive and publish its own
    // acknowledgements at step (10). It is not an admission question, so the split dispatcher's
    // admission logic stays free of proof-gate terms.
    //
    // This used to be TWO owners. Non-x86_64 yielded on `ipccall_direct_publication_enabled()`,
    // written when only the broad arm could publish the NR6/NR7 acknowledgements. Step (10) has
    // published them unconditionally since WA3C2 made the publication bodies shared — both
    // publishers are strict no-ops when publication is off — so that branch had become a stale
    // duplicate of this policy. Live on AArch64 it was the §5 gap: with direct production on,
    // `publication_enabled()` turned true, the branch fired on EVERY blocking recv-v2, and the
    // whole receive was handed to the broad arm. That reopened the window this route exists to
    // close — the caller's block was published late, so a reply could arrive with no claimable
    // acknowledgement and an armed terminal, be declined as mode-indeterminate, fall to legacy,
    // and be LOST (`IPC_REPLY_FAIL err=WrongObject`, caller never resumed, `resume=0`).
    #[cfg(not(feature = "hosted-dev"))]
    if !recv_timeout && crate::kernel::boot::blocked_recv_split_route_yields_to_broad_arm() {
        crate::yarm_log!(
            "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} reason=direct_oracle_selector_armed",
            cpu.0
        );
        return D::NotHandled;
    }
    let Some(tid) = shared.current_tid_authoritative(cpu) else {
        crate::yarm_log!(
            "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} reason=no_current_task",
            cpu.0
        );
        return D::NotHandled;
    };
    // (3) ABI, through the canonical builder. `is_kernel_task` is the same question
    // `current_task_has_user_asid` asks, read through the rank-2 seam.
    let cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let is_kernel_task = shared.task_asid_opt_split_read(tid).is_none();
    let payload_user_ptr = frame.arg(SYSCALL_ARG_PTR);
    let payload_user_len = frame.arg(SYSCALL_ARG_LEN);
    // Stage 199G-B §2 — the ONE place the two receives decode differently, and the point at which
    // the completion contract is fixed. Both go through their own canonical `RecvRequest`
    // builder, the same one their broad handler uses, so neither route can invent an ABI the
    // other would not recognise.
    //
    // NR 5 arms a deadline; NR 2 does not, which is exactly what keeps NR 2 out of the
    // receive-timeout scan and its reply-deadline/oracle arming inert. The deadline formula is
    // `ipc_recv_with_deadline`'s, read from the SAME rank-1 tick owner the trap entry's staging
    // block uses, so a receive that parks here expires on the tick it would have expired on had
    // it parked under the broad lock.
    let (state, deadline, timed_blocking) = if recv_timeout {
        let timeout_ticks = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0) as u64;
        let absolute = shared
            .scheduler_tick_now_split_read()
            .wrapping_add(timeout_ticks);
        let request = RecvRequest::from_ipc_recv_timeout(
            tid,
            cap,
            payload_user_ptr,
            payload_user_len,
            timeout_ticks,
            Some(absolute),
            is_kernel_task,
        );
        // `timeout_ticks == 0` is NR 5's non-blocking probe: it never parks, so it is not this
        // route's business and the broad `NoWait` arm keeps servicing it unchanged.
        let RecvBlockingPolicy::Deadline(_) = request.blocking else {
            crate::yarm_log!(
                "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} reason=not_timed_recv",
                cpu.0,
                tid
            );
            return D::NotHandled;
        };
        // Which SHAPE the receive owes is the caller's to decide, not the syscall number's.
        // `RecvRequest::from_ipc_recv_timeout` hard-codes `RecvMetaTarget::None`, but that is a
        // PLANNING artifact that never reaches a writeback: NR 5's own result owner,
        // `handle_ipc_recv_result_with_empty_error`, computes `recv_v2_meta_written` from args
        // 4/5 and writes the 40-byte struct — with `ret0 = 0` — whenever a buffer is supplied,
        // and the live `yarm-user-rt::ipc_recv_with_deadline` wrapper always supplies one. So
        // this route reads the SAME predicate that owner reads, and a receive that parks here is
        // owed exactly what the same arguments would have been owed had a message been waiting.
        let meta_user_ptr = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1);
        let meta_user_len = frame.arg(SYSCALL_ARG_TRANSFER_CAP);
        let state = if meta_user_ptr != 0
            && meta_user_len >= crate::kernel::syscall::IPC_RECV_META_V2_ENCODED_LEN
        {
            BlockedRecvState {
                recv_cap: cap,
                payload_user_ptr,
                payload_user_len,
                meta_user_ptr,
                meta_user_len,
                recv_abi: crate::kernel::task::RecvAbiVariant::RecvV2,
            }
        } else {
            BlockedRecvState::legacy_timeout(cap, payload_user_ptr, payload_user_len)
        };
        (
            state,
            Some(absolute),
            // Carried, not re-derived: the adapter marker below prints the policy the canonical
            // builder produced, at the point in the broad entry's order where it prints it.
            Some(request.blocking),
        )
    } else {
        let request = RecvRequest::from_legacy_ipc_recv(
            tid,
            cap,
            payload_user_ptr,
            payload_user_len,
            frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
            frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
            is_kernel_task,
        );
        let RecvMetaTarget::V2 {
            ptr: meta_user_ptr,
            len: meta_user_len,
        } = request.meta_target
        else {
            crate::yarm_log!(
                "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} reason=not_recv_v2",
                cpu.0,
                tid
            );
            return D::NotHandled;
        };
        (
            BlockedRecvState {
                recv_cap: cap,
                payload_user_ptr,
                payload_user_len,
                meta_user_ptr,
                meta_user_len,
                recv_abi: crate::kernel::task::RecvAbiVariant::RecvV2,
            },
            None,
            None,
        )
    };
    // (4) Capability: task(2) pid read → capability(4) resolve, both off the broad lock. Every
    // refusal here has a canonical error the broad handler produces, so fall back and let it.
    let Ok(snapshot) = shared.resolve_endpoint_recv_cap_split_read(tid, cap) else {
        crate::yarm_log!(
            "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} reason=cap_resolve",
            cpu.0,
            tid
        );
        return D::NotHandled;
    };
    let CapObject::Endpoint {
        index: endpoint_idx,
        generation,
    } = snapshot.endpoint
    else {
        return D::NotHandled;
    };
    // (5) Would-block, under one rank-3 acquisition.
    if !shared.recv_would_block_split_read(endpoint_idx, generation) {
        crate::yarm_log!(
            "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} endpoint={} reason=would_not_block",
            cpu.0,
            tid,
            endpoint_idx
        );
        return D::NotHandled;
    }
    // (6) ADMISSION — the last point at which falling back is safe.
    if crate::kernel::boot::d2_recv_dispatch_is_deferred(cpu_idx) {
        crate::yarm_log!(
            "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} reason=already_deferred",
            cpu.0,
            tid
        );
        return D::NotHandled;
    }
    if let Err(refusal) = shared.queue_advance_admit_split(
        cpu,
        crate::kernel::boot::QueueAdvanceApply::ExactTokenResume,
    ) {
        crate::yarm_log!(
            "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} reason={:?}",
            cpu.0,
            tid,
            refusal
        );
        return D::NotHandled;
    }
    // The markers the broad entry emits before it blocks, in the broad entry's order — this route
    // intercepts before the arm that would have printed them, and the stream an observer sees must
    // not change because the owner did.
    //
    // Stage 199G-B §2: the two entries print DIFFERENT prologues, so this route prints whichever
    // one it intercepted. `handle_ipc_recv_timeout` emits no `IPC_RECV_ENTER` at all, and it
    // emits the cap-ok line and the supervisor line BEFORE its adapter line — the reverse of NR
    // 2's order. Reproducing each order exactly is the point: an observer must not be able to
    // tell from the marker stream that the owner changed.
    if let Some(blocking) = timed_blocking {
        crate::yarm_log!(
            "IPC_RECV_AFTER_CAP_OK tid={} cap={} endpoint={:?}",
            tid,
            cap.0,
            snapshot.endpoint
        );
        if tid == 2 && shared.fault_or_supervisor_endpoint_split_read(endpoint_idx) {
            crate::yarm_log!(
                "SUPERVISOR_FAULT_RECV_CAP cap={} endpoint={} generation={}",
                cap.0,
                endpoint_idx,
                generation
            );
        }
        crate::yarm_log!(
            "YARM_RECV_CORE_ADAPTER kind=legacy_timeout is_kernel_task={} blocking={:?}",
            is_kernel_task,
            blocking
        );
    } else {
        crate::yarm_log!("IPC_RECV_ENTER tid={} cap={}", tid, cap.0);
        crate::yarm_log!(
            "YARM_RECV_CORE_ADAPTER kind=legacy_full_path is_kernel_task={}",
            is_kernel_task
        );
        crate::yarm_log!(
            "IPC_RECV_AFTER_CAP_OK tid={} cap={} endpoint={:?}",
            tid,
            cap.0,
            snapshot.endpoint
        );
        if tid == 2 && shared.fault_or_supervisor_endpoint_split_read(endpoint_idx) {
            crate::yarm_log!(
                "SUPERVISOR_FAULT_RECV_CAP cap={} endpoint={} generation={}",
                cap.0,
                endpoint_idx,
                generation
            );
        }
    }
    // (7) Reserve the deferral BEFORE any publication.
    if !crate::kernel::boot::d2_recv_dispatch_try_defer(cpu_idx, tid) {
        crate::yarm_log!(
            "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} reason=defer_unavailable",
            cpu.0,
            tid
        );
        return D::NotHandled;
    }
    // (8) Phase A — scheduler rank 1. A victim mismatch unwinds its own single step inside the
    // twin, so this is still pre-mutation from the route's point of view.
    let Some(receiver_asid) = shared.recv_block_phase_a_split(cpu, tid) else {
        crate::kernel::boot::d2_recv_dispatch_clear(cpu_idx);
        crate::yarm_log!(
            "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} reason=phase_a",
            cpu.0,
            tid
        );
        return D::NotHandled;
    };
    // Phase B — task rank 2, with the state and deadline the ABI step decoded. For NR 2 the
    // deadline is `None` (it carries no timeout, which is what keeps the reply-deadline and oracle
    // arming the broad arm performs strict no-ops); for NR 5 it is the absolute tick, which is
    // what puts the parked receiver in front of the existing receive-timeout scan.
    let Some(wait_generation) = shared.recv_block_phase_b_split(tid, cap, deadline, state) else {
        // The task half refused before it wrote anything; undo Phase A's block and fall back.
        let _ = shared.recv_block_unwind_race_split(cpu, tid);
        crate::kernel::boot::d2_recv_dispatch_clear(cpu_idx);
        crate::yarm_log!(
            "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} reason=phase_b",
            cpu.0,
            tid
        );
        return D::NotHandled;
    };
    // `IPC_RECV_BLOCKED_STATE_SAVE` is NOT emitted here: `recv_block_phase_b_split` is the owner
    // of that write and already prints it, so printing it again would double the marker.
    // Phase C — ipc rank 3. The atomic recheck-and-publish, through the ONE policy owner both
    // routes share.
    let (outcome, reply_wait_arm) = shared.recv_block_phase_c_split(
        endpoint_idx,
        crate::kernel::boot::EndpointWaiterRecord::new(
            crate::kernel::boot::ReceiverWaiterIdentity::new(
                crate::kernel::ipc::ThreadId(tid),
                receiver_asid,
            ),
            wait_generation,
        ),
        cap,
        // 199E-ARM: a finite, non-zero deadline is what makes this wait deadline-bearing. The
        // terminal cell is armed either way; this only decides the identity's
        // `deadline_token_generation`, so an unregistered deadline is never implied.
        deadline.is_some_and(|tick| tick != 0),
    );
    match outcome {
        crate::kernel::recv_waiter_split::PublishWaiterOutcome::Published => {}
        // THE RACE. A sender enqueued between step (5) and this publish. Reverse ranks 2 and 1
        // exactly, release the reservation, and let the broad path service the message. This is
        // the branch the serialized broad entry documents as unreachable and this route makes
        // reachable — it is why the unwind twin had to exist first.
        // Stage 199D-WA3C2 — the same reversal as the race, for the same reason: NOTHING was
        // published, so ranks 2 and 1 unwind to the exact pre-block state and the route
        // declines. The decline is deliberate rather than terminal — the broad
        // `block_current_on_receive_with_deadline` is the ONE owner of the ownership-busy
        // policy (it answers `WouldBlock`), and this route must not fork a second copy of it.
        crate::kernel::recv_waiter_split::PublishWaiterOutcome::WaiterOwnershipBusy
        | crate::kernel::recv_waiter_split::PublishWaiterOutcome::QueueNonEmpty => {
            let busy = matches!(
                outcome,
                crate::kernel::recv_waiter_split::PublishWaiterOutcome::WaiterOwnershipBusy
            );
            let unwound = shared.recv_block_unwind_race_split(cpu, tid);
            crate::kernel::boot::d2_recv_dispatch_clear(cpu_idx);
            if !unwound {
                // The inverse could not complete. Falling back would re-execute the syscall on a
                // task the scheduler no longer holds as current, so fail closed instead.
                crate::yarm_log!(
                    "IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED cpu={} tid={} phase=unwind",
                    cpu.0,
                    tid
                );
                return D::Complete(Err(TrapHandleError::Syscall(
                    crate::kernel::syscall::SyscallError::Internal,
                )));
            }
            crate::yarm_log!(
                "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} endpoint={} reason={}",
                cpu.0,
                tid,
                endpoint_idx,
                if busy {
                    "waiter_ownership_busy"
                } else {
                    "queue_non_empty"
                }
            );
            return D::NotHandled;
        }
        // The live publish policy preserves last-receiver-wins and never returns
        // `ReceiverAlreadyWaiting`, and step (5) validated the index and generation under the
        // same rank-3 lock, so `InvalidEndpoint` is defensively unreachable.
        _ => {
            let unwound = shared.recv_block_unwind_race_split(cpu, tid);
            crate::kernel::boot::d2_recv_dispatch_clear(cpu_idx);
            crate::yarm_log!(
                "IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED cpu={} tid={} phase=publish unwound={}",
                cpu.0,
                tid,
                u8::from(unwound)
            );
            return D::Complete(Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::WrongObject,
            )));
        }
    }
    // (9b) 199E-DL — the COMPENSATED rank-2 half of the finite-deadline registration.
    //
    // Phase C reserved the token under rank 3, in the same scope that armed the terminal and
    // published the waiter. A reservation is not claimable: the timeout collector scans TCBs and
    // reaches a token only through `tcb.reply_timeout_token`, and every other touch of the store
    // is keyed by a handle already read from a TCB. So this write is what activates it, and no
    // interval exists in which timeout can win using a token the caller does not yet own.
    //
    // Ranks are never held together — rank 3 was released when Phase C returned. On refusal the
    // exact reservation is cancelled, so nothing is left armed for a caller that does not own it.
    match reply_wait_arm {
        // Not a reply wait, or a reply wait with no finite deadline: nothing was reserved.
        crate::kernel::boot::ReplyWaitArm::NotAReplyWait
        | crate::kernel::boot::ReplyWaitArm::Armed { token: None, .. } => {}
        // A FINITE wait whose terminal armed but whose deadline could not be reserved. Parking it
        // would leave a blocked caller with a deadline it cannot identify, so unwind the whole
        // block exactly as the publish races do and let the broad arm own the outcome.
        crate::kernel::boot::ReplyWaitArm::DeadlineRefused { .. } => {
            let unwound = shared.recv_block_unwind_race_split(cpu, tid);
            crate::kernel::boot::d2_recv_dispatch_clear(cpu_idx);
            crate::yarm_log!(
                "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} endpoint={} reason=deadline_reservation unwound={}",
                cpu.0,
                tid,
                endpoint_idx,
                u8::from(unwound)
            );
            if !unwound {
                return D::Complete(Err(TrapHandleError::Syscall(
                    crate::kernel::syscall::SyscallError::Internal,
                )));
            }
            return D::NotHandled;
        }
        crate::kernel::boot::ReplyWaitArm::Armed {
            token: Some(handle),
            ..
        } => {
            let published = shared.publish_reply_timeout_token_split(
                tid,
                receiver_asid,
                wait_generation,
                handle,
                crate::kernel::deadline_token::ReplyDeadlineClock::ProductionTick,
            );
            if published {
                crate::yarm_log!(
                    "IPC_REPLY_TIMEOUT_ARMED arch={} caller_tid={} caller_asid={} record_index={} record_generation={} terminal_epoch={} token_slot={} token_generation={} deadline={} result=ok",
                    crate::kernel::boot::REPLY_TIMEOUT_ARCH,
                    tid,
                    receiver_asid.0,
                    handle.identity().terminal_identity.reply_record_index,
                    handle.identity().terminal_identity.reply_record_generation,
                    handle.identity().terminal_epoch,
                    handle.identity().token_index,
                    handle.identity().token_generation,
                    deadline.unwrap_or(0)
                );
            } else {
                // The caller incarnation moved under us between rank 3 and rank 2. Cancel the
                // EXACT reservation — a stale cancel mutates nothing — and leave the block
                // otherwise intact: the wait is still armed for reply/death/caller-exit, and its
                // deadline stays on the ordinary receive-timeout class exactly as an
                // unregistered wait's does. Nothing is left claimable that the caller cannot own.
                let cancelled = shared.cancel_deadline_exact_split(&handle);
                crate::yarm_log!(
                    "IPC_REPLY_TIMEOUT_ARM_COMPENSATED caller_tid={} caller_asid={} token_slot={} token_generation={} cancelled={} result=ok",
                    tid,
                    receiver_asid.0,
                    handle.identity().token_index,
                    handle.identity().token_generation,
                    u8::from(cancelled)
                );
            }
        }
    }
    // (10) Stage 199D-WA3C2 — publish the NR6/NR7 blocked-waiter acknowledgements from the
    // SAME fully-committed recv-v2 point the broad arm uses: Phase B stored `BlockedRecvState`,
    // Phase C linked the waiter, and the task is `Blocked(EndpointReceive)`. This is what makes
    // direct IpcCall/IpcReply production reachable for receivers that took the split route —
    // without it the x86_64 default would admit NR6/NR7 with nothing ever to claim.
    //
    // The two reads the shared bodies need are supplied off-lock: the receiver ASID from the
    // rank-2 seam, and the live waiter identity from the rank-3 seam. Both publishers are
    // strict no-ops when publication is not enabled or the endpoint is not admitted.
    {
        let endpoint = crate::kernel::capabilities::CapObject::Endpoint {
            index: endpoint_idx,
            generation,
        };
        let asid = Some(receiver_asid);
        let waiter_identity = crate::kernel::boot::ReceiverWaiterIdentity::new(
            crate::kernel::ipc::ThreadId(tid),
            receiver_asid,
        );
        let live_waiter = |index: usize| {
            shared
                .endpoint_waiter_is_split_read(index, generation, waiter_identity)
                .then_some(waiter_identity)
        };
        let _ = crate::kernel::boot::publish_ipccall_direct_blocked_server_ack_with(
            tid,
            asid,
            endpoint,
            &state,
            live_waiter,
        );
        let _ = crate::kernel::boot::publish_ipcreply_direct_blocked_caller_ack_with(
            tid,
            asid,
            endpoint,
            &state,
            live_waiter,
        );
    }
    crate::yarm_log!(
        "IPC_RECV_BLOCK_REGISTER endpoint={} tid={}",
        endpoint_idx,
        tid
    );
    crate::yarm_log!(
        "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=blocking_recv_switch_required tid={} cpu={}",
        tid,
        cpu_idx
    );
    // (9) The syscall's own result, into the outgoing frame before the switch — the exact
    // `WouldBlock` the broad arm returns for a receive that parked. A delivering sender overwrites
    // it through `complete_blocked_recv_for_waiter`; it is what the caller observes only if it is
    // resumed without a delivery.
    frame.set_err(crate::kernel::syscall::SyscallError::WouldBlock.code());
    crate::yarm_log!(
        "IPC_RECV_BLOCK_SPLIT_DONE cpu={} tid={} endpoint={} wait_gen={} result=blocked",
        cpu.0,
        tid,
        endpoint_idx,
        wait_generation
    );
    D::QueueAdvanceCommitted
}

#[cfg(feature = "hosted-dev")]
fn try_split_blocking_ipc_recv_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> SplitDispatchDisposition {
    SplitDispatchDisposition::NotHandled
}

/// U9-TM §2 — service a NON-PREEMPTING `TimerInterrupt` off the broad lock.
///
/// This is a DEFAULT-CONFIGURATION route, not TimerInterrupt retirement. Two fallbacks remain,
/// both taken before anything is claimed, ticked or mutated:
///
/// 1. **any timer-only proof knob armed** — the five `maybe_run_*` hooks are called only from the
///    broad arm; U9-TM does not relocate them, so an armed profile takes the unchanged broad
///    route and every hook runs exactly as before.
/// 2. **this tick would preempt** — no existing profile witnesses a timer-driven preemption
///    (zero `preempt=1` across 77 recorded profiles), so the preempting branch cannot be
///    live-proven and is not shipped. `scheduler_tick_if_no_switch_split_mut` refuses atomically,
///    having incremented nothing.
///
/// What the route does own, when neither fallback applies:
///
/// * claim/ack through the SAME lock-free adapter the `Hal` method delegates to;
/// * exactly ONE tick, through the single `SchedulerTimer` policy;
/// * re-arm through the same adapter — on RISC-V SBI `set_timer` is itself the completion, and
///   PLIC source 0 is never touched;
/// * and then `PostWorkCommitted`, so the architecture tail still runs the production timeout
///   pipeline that owns all three timeout classes.
///
/// It performs NO queue selection and publishes no transition: a non-preempting tick changes no
/// scheduler state beyond the tick itself.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_timer_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    is_timer: bool,
) -> SplitDispatchDisposition {
    use SplitDispatchDisposition as D;

    if !is_timer {
        return D::NotHandled;
    }
    // (1) FAIL BEFORE MUTATION — the proof-mode gate. Evaluated before claim, tick, timeout work,
    // preemption or re-arm, so a refused trap reaches the broad arm having changed nothing.
    if crate::kernel::boot::timer_proof_hooks_armed() {
        crate::yarm_log!("TIMER_SPLIT_REFUSED cpu={} reason=proof_hooks_armed", cpu.0);
        return D::NotHandled;
    }
    // (2) The would-preempt refusal, also before any mutation. One rank-1 acquisition decides
    // and, when it declines, has incremented nothing.
    let Some(outcome) = shared.scheduler_tick_if_no_switch_split_mut(cpu) else {
        crate::yarm_log!("TIMER_SPLIT_REFUSED cpu={} reason=would_preempt", cpu.0);
        return D::NotHandled;
    };
    // Past this point the tick HAS happened; there is no route back to the broad arm, which
    // would tick a second time.
    let tick = match outcome {
        crate::runtime::SchedulerTickOutcome::NoSwitch { tick, .. } => tick,
        // Unreachable: the seam returns `None` rather than a preempting outcome.
        crate::runtime::SchedulerTickOutcome::Preempt { tick, .. } => tick,
    };
    // (3) Claim/ack — the SAME free function `Hal::acknowledge_interrupt` delegates to.
    crate::arch::hal_adapters::acknowledge_interrupt(cpu, 0);
    // (4) Re-arm — the SAME free function `Hal::program_timer_deadline` delegates to, with the
    // same deadline constant the broad arm passes. On RISC-V this single SBI `set_timer` both
    // clears the pending condition and programs the next deadline: it IS the completion, and
    // there is no separate end-of-interrupt.
    crate::arch::hal_adapters::program_timer_deadline(
        cpu,
        crate::arch::platform_constants::BOOTSTRAP_TIMER_DEADLINE_TICKS,
    );
    crate::yarm_log!(
        "TIMER_SPLIT_TICK_OK cpu={} tick={} preempt=0 rearm=1",
        cpu.0,
        tick
    );
    // (5) The architecture tail still owes the production timeout pipeline.
    D::PostWorkCommitted {
        finalize_syscall: false,
    }
}

#[cfg(feature = "hosted-dev")]
fn try_split_timer_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _is_timer: bool,
) -> SplitDispatchDisposition {
    SplitDispatchDisposition::NotHandled
}

/// U9-QA §2 — service `FutexWait` (NR 9) off the broad lock.
///
/// This is the migration of the EXISTING semantics onto the already-present split primitives —
/// the same Phase A value check, the same Phase B block publication, and the same one-shot
/// per-CPU deferral the in-lock `futex_wait_current` records. No timeout, no `WAIT_BITSET`, no
/// requeue, no PI, no new flag and no ABI change.
///
/// The steps are ordered so that the LAST thing which can decline comes before the FIRST thing
/// which mutates:
///
/// 1. **NR + ABI** — decoded exactly as `handle_futex_wait` decodes it. A non-`u32` argument is
///    an `InvalidArgs` the broad handler owns, so it falls back.
/// 2. **Phase A value check** — `futex_wait_would_block_split_read`. `None` is a validation miss
///    whose canonical error is the broad handler's to produce; fall back and let it.
/// 3. **Not blocking** — the futex word already moved. Nothing is published and no switch is
///    needed, so this is an ordinary `Complete`, encoded exactly as the broad handler encodes it.
/// 4. **Admission** — `queue_advance_admit_split`, plus this class's own precondition that no
///    same-class deferral is outstanding. Every refusal lands here, while falling back is still
///    safe. `Ok(None)` is ADMITTED: it means the advance will idle this CPU, which the drains
///    already settle.
/// 5. **Deferral reservation** — taken BEFORE the publication, because it is the only step that
///    can fail for a reason unrelated to this caller. Reserving first keeps such a failure a
///    pre-mutation refusal.
/// 6. **Publication** — `futex_wait_publish_block_split_mut`. It returns `false` only when the
///    caller has no TCB, and it does so BEFORE touching the scheduler, so that case is still
///    unmutated: release the reservation and fall back.
/// 7. **The result** — `set_ok(1, 0, 0)` into the OUTGOING frame, identical to the broad
///    handler's `set_ok(usize::from(blocked), 0, 0)`. It is written before the switch so the
///    caller observes it when it is later resumed.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_futex_wait_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> SplitDispatchDisposition {
    use crate::kernel::syscall::{SYSCALL_ARG_CAP, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR};
    use SplitDispatchDisposition as D;

    if frame.syscall_num() != crate::kernel::syscall::SYSCALL_FUTEX_WAIT_NR {
        return D::NotHandled;
    }
    let cpu_idx = cpu.0 as usize;
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return D::NotHandled;
    }
    // (1) ABI, identical to `handle_futex_wait`.
    //
    // Every decline from here on is ATTRIBUTED. These were silent, and that cost a diagnosis:
    // AArch64 imported `nr=9` into the frame and still never took this route, with no marker to
    // say which check declined it. A refusal that cannot be seen in a live log cannot be
    // distinguished from a route that was never reached.
    let addr = frame.arg(SYSCALL_ARG_CAP);
    let (Ok(expected), Ok(observed)) = (
        u32::try_from(frame.arg(SYSCALL_ARG_PTR)),
        u32::try_from(frame.arg(SYSCALL_ARG_LEN)),
    ) else {
        // legacy: InvalidArgs, produced by the broad handler
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_REFUSED tid=0 reason=arg_decode cpu={}",
            cpu.0
        );
        return D::NotHandled;
    };
    let Some(tid) = shared.current_tid_authoritative(cpu) else {
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_REFUSED tid=0 reason=no_current_task cpu={}",
            cpu.0
        );
        return D::NotHandled;
    };
    // (2) Phase A: the off-lock value check.
    let Some(would_block) = shared.futex_wait_would_block_split_read(tid, addr, expected, observed)
    else {
        // legacy: WrongObject / UserMemoryFault
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_REFUSED tid={} reason=value_check addr={} expected={} observed={} cpu={}",
            tid,
            addr,
            expected,
            observed,
            cpu.0
        );
        return D::NotHandled;
    };
    // (3) The non-blocking outcome: no transition, no switch, no drain.
    if !would_block {
        frame.set_ok(0, 0, 0);
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_DONE tid={} addr={} result=not_blocked",
            tid,
            addr
        );
        return D::Complete(Ok(()));
    }
    // (4) ADMISSION — the last point at which falling back is safe.
    if crate::kernel::boot::futex_wait_dispatch_is_deferred(cpu_idx) {
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_REFUSED tid={} reason=already_deferred cpu={}",
            tid,
            cpu.0
        );
        return D::NotHandled;
    }
    // The FutexWait drains apply through the EXACT-TOKEN resume on all three architectures — they
    // stash nothing and switch no kernel context — so admission is asked that convention's
    // preconditions, not the stash's.
    if let Err(refusal) = shared.queue_advance_admit_split(
        cpu,
        crate::kernel::boot::QueueAdvanceApply::ExactTokenResume,
    ) {
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_REFUSED tid={} reason={:?} cpu={}",
            tid,
            refusal,
            cpu.0
        );
        return D::NotHandled;
    }
    // (5) Reserve the deferral before publishing, so a reservation failure is pre-mutation.
    if !crate::kernel::boot::futex_wait_dispatch_try_defer(cpu_idx, tid) {
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_REFUSED tid={} reason=defer_unavailable cpu={}",
            tid,
            cpu.0
        );
        return D::NotHandled;
    }
    crate::yarm_log!("FUTEX_WAIT_SPLIT_BEGIN");
    // (6) PUBLICATION. Past this line the trap MUST settle through the drains.
    if !shared.futex_wait_publish_block_split_mut(cpu, tid, addr) {
        // No TCB for the caller: the publish refused before it touched the scheduler, so
        // nothing is mutated. Release the reservation and let the broad handler produce the
        // canonical `TaskMissing`.
        crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_REFUSED tid={} reason=publish_no_tcb cpu={}",
            tid,
            cpu.0
        );
        return D::NotHandled;
    }
    crate::yarm_log!(
        "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=futex_wait_switch_required tid={} cpu={}",
        tid,
        cpu_idx
    );
    // (7) The syscall's own result, into the outgoing frame, before the switch.
    frame.set_ok(1, 0, 0);
    D::QueueAdvanceCommitted
}

#[cfg(feature = "hosted-dev")]
fn try_split_futex_wait_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> SplitDispatchDisposition {
    SplitDispatchDisposition::NotHandled
}

/// Returns:
/// * `Some(Ok(()))`  — the syscall was a whitelisted split-eligible one, was
///   serviced via the per-domain split helpers, and the success payload was
///   written into `frame` via `set_ok(..)`. The caller must SKIP the global-lock
///   dispatch entirely (the result is already in the frame).
/// * `Some(Err(e))`  — the syscall was whitelisted but the domain mutation failed.
///   `e` is the same `TrapHandleError::Syscall(..)` the global-lock path would have
///   returned for this error; the caller propagates it on the existing error path.
/// * `None`          — the syscall is NOT split-eligible (default-deny) OR the
///   requester TID was unavailable. The caller MUST fall back to the unchanged
///   global-lock dispatch path.
///
/// The split path never blocks, yields, schedules, switches tasks, or copies user
/// memory. Because no task switch occurs, `entering_tid == exiting_tid` and
/// `task_switched == false` remain observable to the arch return-register
/// writeback branch exactly as on the global-lock path.
fn try_split_dispatch_nonswitching_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::syscall::{SYSCALL_ARG_CAP, SYSCALL_ARG_PTR};

    // Stage 160B diagnostics (proof-knob–gated so normal/fast boots stay clean):
    // pin exactly where a syscall enters or skips the pre-global-lock split
    // dispatch. These read the frame's *decoded* syscall ABI (`syscall_num()` /
    // `arg()`), which is the same source the eligibility checks below use — so a
    // mismatch versus the real trapped syscall (e.g. an AArch64 frame whose
    // syscall_num/args have not yet been imported from the user GPRs) shows up
    // directly as `nr=0`.
    let probe = crate::kernel::boot::ipc_recv_oracle_proof_enabled();
    let raw_nr = frame.syscall_num();
    if probe {
        crate::yarm_log!("YARM_SPLIT_DISPATCH_ENTER nr={}", raw_nr);
    }

    // U9-ASPACE1 §2: a RETIRED number is answered here, not by the broad dispatcher.
    //
    // An undecodable number normally falls through to the terminal broad acquisition, which
    // decodes it a second time under the whole kernel and returns the same error this returns.
    // For a number that USED to be a syscall that is worse than pointless: retiring a class
    // would hand every caller still naming it a broad-lock acquisition, so the retirement would
    // add terminal broad work rather than remove it. The answer is knowable with no lock at all
    // — the number is on a fixed table — so it is given here, and it is exactly the error the
    // broad path would have produced.
    if let Some(reason) = crate::kernel::syscall::retired_syscall_number(raw_nr) {
        crate::yarm_log!("SYSCALL_RETIRED_REFUSED nr={} reason={}", raw_nr, reason);
        return Some(Err(TrapHandleError::Syscall(
            crate::kernel::syscall::SyscallError::InvalidNumber,
        )));
    }
    // Default-deny by syscall number first (cheap, no lock).
    let Ok(syscall) = Syscall::decode(raw_nr) else {
        if probe {
            crate::yarm_log!(
                "YARM_SPLIT_DISPATCH_FALLBACK reason=nr_undecodable nr={}",
                raw_nr
            );
        }
        return None;
    };
    // Stage 199D: IpcCall (NR 6) + IpcReply (NR 7) are not in the static NR-only whitelist,
    // but they ARE admitted to the direct request/reply gates below. Since WA1-GATE that
    // admission requires the explicit proof gate on EVERY architecture, x86_64 included — the
    // production term is `false` everywhere — so every normal boot stays byte-identical to the
    // legacy path.
    let direct_ipc_admitted = matches!(syscall, Syscall::IpcCall | Syscall::IpcReply)
        && crate::kernel::boot::ipccall_direct_admission_enabled();
    if classify_split_eligible_nr_only(syscall).is_none() && !direct_ipc_admitted {
        if probe {
            crate::yarm_log!(
                "YARM_SPLIT_DISPATCH_FALLBACK reason=nr_not_eligible nr={}",
                raw_nr
            );
        }
        return None;
    }

    // Stage 32B: IpcRecv (NR 2) is routed to the dedicated queued-plain recv
    // helper, which decides split eligibility INTERNALLY (kernel-task receiver,
    // queued plain message, no sender-wake / recv-v2). Crucially, every case the
    // helper cannot service returns `None`, and that `None` propagates UNCHANGED
    // back to the global-lock fallback below — the split path never converts a
    // would-be-fallback into a `Some(Err(..))` (it only returns `Some(Err)` for a
    // cap-resolution error the old path would have raised identically).
    if matches!(syscall, Syscall::IpcRecv) {
        if probe {
            crate::yarm_log!("YARM_SPLIT_DISPATCH_RECV_CONSIDER nr={}", raw_nr);
            crate::yarm_log!("YARM_SPLIT_DISPATCH_RECV_CALL");
        }
        return try_split_ipc_recv_queued_plain_into_frame(shared, cpu, frame);
    }

    // Stage 114: `VmBrk` (NR 14) is routed to the dedicated brk-shrink helper
    // for the same reason `IpcRecv` is above — eligibility (group leader,
    // page-crossing shrink, single CPU online) can only be decided inside the
    // helper. Every case it cannot service returns `None`, which propagates
    // UNCHANGED back to the global-lock fallback below.
    if matches!(syscall, Syscall::VmBrk) {
        return try_split_vm_brk_shrink_into_frame(shared, cpu, frame);
    }

    // U9-MO2 §4: `CreateInitramfsFileSliceMo` (NR 28) is routed to its own pre-lock owner for
    // the same reason as the two above — eligibility (SystemServer caller, resolvable name,
    // non-empty file, provisioned cspace) can only be decided inside the helper. Every case it
    // declines BEFORE its first mutation returns `None`, which propagates UNCHANGED back to the
    // global-lock fallback below; after the mutation it never declines.
    // U9-SPAWN1 SP-2: `SpawnThread` (NR 11) is routed to its own pre-lock owner. Every case it
    // declines BEFORE its first mutation returns `None` and propagates UNCHANGED to the
    // global-lock fallback below; after the mutation it never declines.
    if matches!(syscall, Syscall::SpawnThread) {
        return try_split_spawn_thread_into_frame(shared, cpu, frame);
    }

    if matches!(syscall, Syscall::CreateInitramfsFileSliceMo) {
        return try_split_create_initramfs_mo_into_frame(shared, cpu, frame);
    }

    // U9-SPAWN-TXN3 §4: the two image-loading spawn classes. Both run the ONE generic spawn
    // transaction through `SharedSpawnOwners`, so nothing about the phase order, the validation
    // or the compensation differs from the broad path — only the acquisitions do. Every case they
    // decline is BEFORE the first mutation and returns `None`, propagating UNCHANGED to the
    // global-lock fallback below; after the first mutation neither ever declines.
    if matches!(syscall, Syscall::SpawnProcess) {
        return try_split_spawn_process_into_frame(shared, cpu, frame);
    }

    if matches!(syscall, Syscall::SpawnFromMemoryObject) {
        return try_split_spawn_from_mo_into_frame(shared, cpu, frame);
    }

    // U9-FORK1 §4: Fork (NR 12), before the terminal acquisition. Every refusal it makes is
    // pre-mutation and returns `None`, which propagates UNCHANGED to the global-lock fallback;
    // once the transaction begins it never declines.
    if matches!(syscall, Syscall::Fork) {
        return try_split_fork_into_frame(shared, cpu, frame);
    }

    // U9-REAP1 §4: ReapFaultedTask (NR 31), before the terminal acquisition. It reads no user
    // memory and takes no capability argument — its only input is a numeric TID in arg0 — so the
    // route exists identically on every profile, and every gate it applies is the broad handler's
    // own gate. It declines with `None` in exactly one case: no resolvable caller, which is
    // pre-mutation and which the broad handler re-derives unchanged. Once a caller resolves the
    // route owns the syscall completely, so `TASK_REAP_FAULTED_BEGIN` is emitted exactly once per
    // invocation on either path.
    if matches!(syscall, Syscall::ReapFaultedTask) {
        return try_split_reap_faulted_task_into_frame(shared, cpu, frame);
    }

    // Stage 191A (GLOBAL-LOCK-RETIRE, first class): DebugLog (NR 15) — a pure read
    // serviced off the global lock. The helper returns `None` for any case it cannot
    // service (hosted-dev, unavailable requester), which propagates UNCHANGED back to
    // the global-lock fallback below.
    if matches!(syscall, Syscall::DebugLog) {
        return try_split_debug_log_into_frame(shared, cpu, frame);
    }

    // Stage 191B (GLOBAL-LOCK-RETIRE, second class): FutexWake (NR 11) — waiter/
    // run-queue mutation only, no caller task-switch. The helper returns `None` for any
    // case it cannot service (invalid addr, hosted-dev, unavailable requester), which
    // propagates UNCHANGED to the global-lock fallback (producing the exact error).
    if matches!(syscall, Syscall::FutexWake) {
        return try_split_futex_wake_into_frame(shared, cpu, frame);
    }

    // Stage 199A2B2F (proof-gated, default-OFF): IpcCall (NR 6) direct request. Only
    // attempted when the internal proof gate is armed; the helper snapshots the request
    // off-lock and drives the accepted off-lock transaction. Off the gate — or for any
    // case it cannot service — it returns `None`, so NR 6 stays on its existing path.
    if matches!(syscall, Syscall::IpcCall)
        && crate::kernel::boot::ipccall_direct_admission_enabled()
    {
        if let Some(result) = try_split_ipccall_direct_into_frame(shared, cpu, frame) {
            return Some(result);
        }
    }

    // Stage 199A2B3 (proof-gated, default-OFF): IpcReply (NR 7) direct reply. Only
    // attempted when the internal proof gate is armed; the helper snapshots the reply
    // payload off-lock (owned) and drives the accepted off-lock reply transaction
    // (reserve → caller-copy → exact-waiter claim → record Consumed → single enqueue).
    // Off the gate — or for any case it cannot service — it returns `None`, so NR 7
    // stays on its existing global-lock path.
    if matches!(syscall, Syscall::IpcReply)
        && crate::kernel::boot::ipccall_direct_admission_enabled()
    {
        if let Some(result) = try_split_ipcreply_direct_into_frame(shared, cpu, frame) {
            return Some(result);
        }
    }

    // The requester TID is what the global-lock handler reads via
    // `current_tid(kernel)` (i.e. `kernel.current_tid()`).
    //
    // Stage 29A: this MUST use the authoritative `current_tid_authoritative(cpu)`
    // read, NOT `current_tid_split_read(cpu)`. At the live x86_64 pre-global-lock
    // trap point the split-read of the scheduler's per-CPU current slot is stale
    // (it can observe a prior task such as tid 0 instead of the running requester),
    // which made the requester-class permission check resolve the wrong task and
    // return `MissingRight`. The authoritative read binds `current_cpu` first and
    // returns the same task the global-lock handler sees. It is a read-only
    // current-task snapshot (no dispatch/yield/switch); the domain mutation below
    // still runs lock-free via the split-mut helper. If unavailable, fall back so
    // the global-lock path produces the canonical `Internal` error.
    let requester_tid = shared.current_tid_authoritative(cpu)?;

    // Decode args identically to `handle_control_plane_set_cnode_slots`.
    let mut args = [0u64; 6];
    for (i, slot) in args.iter_mut().enumerate() {
        *slot = frame.arg(i) as u64;
    }

    let result = try_split_dispatch(shared, syscall, requester_tid, args)?;
    match result {
        Ok(()) => {
            // Mirror the global-lock handler's exact success encoding:
            //   frame.set_ok(slot_capacity, target_pid as usize, 0)
            let target_pid = frame.arg(SYSCALL_ARG_CAP);
            let slots = frame.arg(SYSCALL_ARG_PTR);
            frame.set_ok(slots, target_pid, 0);
            Some(Ok(()))
        }
        Err(err) => Some(Err(TrapHandleError::Syscall(SyscallError::from(err)))),
    }
}

// ── Stage 191A GLOBAL-LOCK-RETIRE markers (first class) ──────────────────────
/// Emitted once, the first time a class is serviced off the global lock this boot.
pub const MARK_RETIRE_CLASS_BEGIN: &str = "GLOBAL_LOCK_RETIRE_CLASS_BEGIN";
/// Emitted once, after the first off-global-lock service of a class succeeds.
pub const MARK_RETIRE_CLASS_DONE: &str = "GLOBAL_LOCK_RETIRE_CLASS_DONE";
/// A class was inspected for retirement but kept global-lock-only; carries a reason.
pub const MARK_RETIRE_CLASS_DEFERRED: &str = "GLOBAL_LOCK_RETIRE_CLASS_DEFERRED";

/// One-shot latch so the DebugLog retirement markers are emitted exactly once.
#[cfg(not(feature = "hosted-dev"))]
static DEBUG_LOG_RETIRE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 191A: service `DebugLog` (NR 15) through the split (no-global-lock) path.
///
/// DebugLog is the FIRST retired global-lock class. It is a pure READ: it resolves
/// the requester task, copies the user message bytes, logs `USER_LOG`, and writes
/// `set_ok(0,0,0)`. It never blocks/yields/schedules, never switches tasks, and never
/// mutates `KernelState` (`task_switched == false` stays observable). The copy runs
/// off the global lock via `SharedKernel::copy_from_user_asid_split_read` (VM
/// user-spaces lock + direct map). Behaviorally identical to the global-lock
/// `handle_debug_log` (same null/empty short-circuit, same copy-fail silent path,
/// same `USER_LOG` line, same `set_ok(0,0,0)`). Returns `None` only when the requester
/// TID is unavailable, so that case falls back to the unchanged global-lock path.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_debug_log_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    // DebugLog ABI: arg0 = user ptr, arg1 = len (no cap slot).
    let user_ptr = frame.arg(0);
    let raw_len = frame.arg(1) as u64;
    // Stage 198B: cap at DEBUG_LOG_MAX_BYTES (192, wider than IPC Message::MAX_PAYLOAD) so the
    // canonical ordinary-cap attestations (~138 bytes) log untruncated on the split path.
    let len = (raw_len as usize).min(crate::kernel::syscall::debug::DEBUG_LOG_MAX_BYTES);

    // Authoritative requester TID (binds current_cpu; same task the global handler
    // sees). Unavailable → fall back to the global-lock path.
    let tid = shared.current_tid_authoritative(cpu)?;

    if user_ptr == 0 || len == 0 {
        // Same short-circuit as the global handler: OK, no log.
        frame.set_ok(0, 0, 0);
        maybe_log_debug_log_retired();
        return Some(Ok(()));
    }

    let asid = shared.task_asid_for_tid_split_read(tid);
    match shared.copy_from_user_asid_split_read(asid, user_ptr, len) {
        Some(payload) => {
            let msg = core::str::from_utf8(&payload[..len]).unwrap_or("<utf8_err>");
            crate::yarm_log!("USER_LOG tid={} msg={}", tid, msg);
            // Stage 199A2D2C2B2: terminal cross-CPU request-OK marker, gated on observing the resumed
            // CPU-1 server's X86_AP_RECV_V2_CONTINUED marker here (the off-lock DebugLog path).
            crate::kernel::boot::maybe_emit_ipccall_direct_smp_request_ok(msg);
            // Stage 199A2D2C2C: terminal cross-CPU reply-OK marker, gated on observing the resumed
            // CPU-0 client's X86_BSP_REPLY_USER_VALIDATED marker here (the off-lock DebugLog path).
            crate::kernel::boot::maybe_emit_ipcreply_direct_smp_reply_ok(msg);
            // Stage 200C2C2C-R2B: same causal reply-wins gate release on the off-lock DebugLog
            // path, so the seam the oracle actually takes is never the one that misses it.
            crate::kernel::boot::maybe_release_reply_timeout_collector_gate(msg);
            // Stage 199D: the ServerDies quiescent link-balance attestation. Read-only and
            // one-shot; the live-link count is read through the task-domain split seam, not
            // the broad lock, because this runs on the off-lock DebugLog path.
            crate::kernel::boot::maybe_emit_server_dies_link_balance(
                msg,
                shared.live_server_reply_link_count_split_read(),
            );
            // Stage 199D: the direct-IPC counter attestation. Read-only, one-shot, and a
            // strict no-op until at least one direct attempt has occurred — it reuses this
            // existing observation point rather than adding an emission site of its own, so
            // it costs nothing on a boot that never takes the direct path.
            crate::kernel::direct_ipc_counters::maybe_emit_attestation();
            // Stage 199D production flip: the FINAL quiescent attestation, emitted once, only
            // after the normal service chain has reported healthy AND settled.
            //
            // `INIT_IDLE_PARK_BEGIN` is init parking after every spawn has completed — the
            // latest point in the boot that is still a service-chain marker, and the closest
            // thing to quiescence the kernel gets to observe. The earlier
            // `INIT_SPAWN_V5_REPLY_RECV_OK` proves the chain *works*, but it fires while most
            // servers have not even started, so an occupancy or high-watermark reading taken
            // there is an early sample masquerading as a settled one — a live boot measured a
            // watermark of 2 at that point and then went on to exhaust all 8 slots. The
            // bounded per-direction census (`maybe_emit_attestation`) still covers a boot that
            // never reaches the park, so moving this later loses no diagnostic on failure.
            //
            // The INDEPENDENT waiter census is computed here, off-lock, only when the trigger
            // matches — it is a two-pass scan of the endpoint table, so it must not run on
            // every DebugLog. It is measured from the waiter table, not from the store's own
            // counters, which is what lets it detect a store that balanced its books while
            // dropping or orphaning a lease.
            if msg.starts_with("INIT_IDLE_PARK_BEGIN") {
                let census = (
                    shared.direct_ack_lease_bijection(
                        crate::kernel::boot::ipccall_direct_ack::store(),
                        crate::kernel::boot::ipccall_direct_request_endpoint_admitted,
                    ),
                    shared.direct_ack_lease_bijection(
                        crate::kernel::boot::ipcreply_direct_ack::store(),
                        crate::kernel::boot::ipccall_direct_reply_endpoint_admitted,
                    ),
                );
                crate::kernel::direct_ipc_counters::maybe_emit_quiescent_attestation(
                    true,
                    Some(census),
                );
            }
        }
        // Copy failed (no mapping / not user-readable) — same as the global handler's
        // `DEBUG_LOG_COPY_FAIL` path: OK, no log.
        None => {}
    }
    frame.set_ok(0, 0, 0);
    maybe_log_debug_log_retired();
    Some(Ok(()))
}

/// Emit the DebugLog retirement markers exactly once (first off-global-lock service).
#[cfg(not(feature = "hosted-dev"))]
fn maybe_log_debug_log_retired() {
    if DEBUG_LOG_RETIRE_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        // Stage 197 (FIRST-COHORT SEAL): every architecture emits the canonical arch-tagged
        // retirement marker `arch=<arch> class=DebugLog` (x86_64 normalized from the historical
        // untagged text).
        #[cfg(target_arch = "aarch64")]
        {
            crate::yarm_log!("{} arch=aarch64 class=DebugLog", MARK_RETIRE_CLASS_BEGIN);
            crate::yarm_log!(
                "{} arch=aarch64 class=DebugLog result=ok",
                MARK_RETIRE_CLASS_DONE
            );
        }
        #[cfg(target_arch = "riscv64")]
        {
            crate::yarm_log!("{} arch=riscv64 class=DebugLog", MARK_RETIRE_CLASS_BEGIN);
            crate::yarm_log!(
                "{} arch=riscv64 class=DebugLog result=ok",
                MARK_RETIRE_CLASS_DONE
            );
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
        {
            crate::yarm_log!("{} arch=x86_64 class=DebugLog", MARK_RETIRE_CLASS_BEGIN);
            crate::yarm_log!(
                "{} arch=x86_64 class=DebugLog result=ok",
                MARK_RETIRE_CLASS_DONE
            );
        }
    }
}

/// Hosted-dev: DebugLog stays on the unchanged global-lock path (the split copy uses
/// the direct map, which only exists on real targets).
#[cfg(feature = "hosted-dev")]
fn try_split_debug_log_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    None
}

/// One-shot latch so the FutexWake retirement markers are emitted exactly once.
#[cfg(not(feature = "hosted-dev"))]
static FUTEX_WAKE_RETIRE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 191B: service `FutexWake` (NR 11) through the split (no-global-lock) path.
///
/// FutexWake is the SECOND retired global-lock class. The CALLER never task-switches;
/// the syscall only mutates waiter/run-queue state. This helper validates the futex
/// word EXACTLY like the global `validate_current_user_futex_word` (addr != 0, addr+3
/// below `KERNEL_SPACE_BASE`, 4 bytes user-readable), then wakes off the global lock
/// via `SharedKernel::futex_wake_split_mut` (task split-mut wake scan + scheduler
/// split-mut enqueue). It preserves the legacy return value (number of waiters woken)
/// and encodes it with `set_ok(woke, 0, 0)`. Any case it cannot service (invalid addr,
/// non-`u32` max_wake, unavailable requester) returns `None` → unchanged global-lock
/// fallback, which produces the CANONICAL error (WrongObject / UserMemoryFault /
/// InvalidArgs) exactly as before — no silent success masking.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_futex_wake_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::syscall::{SYSCALL_ARG_CAP, SYSCALL_ARG_PTR};
    // FutexWake ABI: arg(CAP) = futex addr, arg(PTR) = max_wake.
    let addr = frame.arg(SYSCALL_ARG_CAP);
    // Non-`u32` max_wake → the global handler returns InvalidArgs; fall back.
    let max_wake = u32::try_from(frame.arg(SYSCALL_ARG_PTR) as u64).ok()?;

    let tid = shared.current_tid_authoritative(cpu)?;

    // Validate the futex word exactly like `validate_current_user_futex_word`. On ANY
    // validation miss, fall back so the global-lock path produces the canonical error.
    if addr == 0 {
        return None; // legacy: WrongObject
    }
    let end = addr.checked_add(core::mem::size_of::<u32>() - 1)?;
    if end as u64 >= crate::kernel::vm::KERNEL_SPACE_BASE {
        return None; // legacy: UserMemoryFault
    }
    let asid = shared.task_asid_for_tid_split_read(tid);
    if shared
        .copy_from_user_asid_split_read(asid, addr, core::mem::size_of::<u32>())
        .is_none()
    {
        return None; // legacy: UserMemoryFault
    }

    // Validation passed — wake off the global lock.
    // Stage 195C: AArch64 emits arch-tagged split markers (FutexWake is the third live
    // AArch64 split-dispatch class). Stage 196C: RISC-V likewise emits an arch=riscv64 tag
    // (with the woke count, mirroring aarch64). x86_64 keeps the exact untagged Stage 191B text.
    #[cfg(target_arch = "aarch64")]
    crate::yarm_log!("FUTEX_WAKE_SPLIT_BEGIN arch=aarch64");
    #[cfg(target_arch = "riscv64")]
    crate::yarm_log!("FUTEX_WAKE_SPLIT_BEGIN arch=riscv64");
    #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
    crate::yarm_log!("FUTEX_WAKE_SPLIT_BEGIN");
    let woke = shared.futex_wake_split_mut(cpu, addr, max_wake);
    crate::yarm_log!("FUTEX_WAKE_SPLIT_WAKE_OK count={}", woke);
    frame.set_ok(woke as usize, 0, 0);
    #[cfg(target_arch = "aarch64")]
    crate::yarm_log!("FUTEX_WAKE_SPLIT_DONE arch=aarch64 result=ok woke={}", woke);
    #[cfg(target_arch = "riscv64")]
    crate::yarm_log!("FUTEX_WAKE_SPLIT_DONE arch=riscv64 result=ok woke={}", woke);
    #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
    crate::yarm_log!("FUTEX_WAKE_SPLIT_DONE result=ok");
    maybe_log_futex_wake_retired();
    Some(Ok(()))
}

/// Emit the FutexWake retirement markers exactly once (first off-global-lock service).
#[cfg(not(feature = "hosted-dev"))]
fn maybe_log_futex_wake_retired() {
    if FUTEX_WAKE_RETIRE_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        // Stage 197 (FIRST-COHORT SEAL): every architecture emits the canonical arch-tagged
        // retirement marker `arch=<arch> class=FutexWake` (x86_64 normalized from the historical
        // untagged text).
        #[cfg(target_arch = "aarch64")]
        {
            crate::yarm_log!("{} arch=aarch64 class=FutexWake", MARK_RETIRE_CLASS_BEGIN);
            crate::yarm_log!(
                "{} arch=aarch64 class=FutexWake result=ok",
                MARK_RETIRE_CLASS_DONE
            );
        }
        #[cfg(target_arch = "riscv64")]
        {
            crate::yarm_log!("{} arch=riscv64 class=FutexWake", MARK_RETIRE_CLASS_BEGIN);
            crate::yarm_log!(
                "{} arch=riscv64 class=FutexWake result=ok",
                MARK_RETIRE_CLASS_DONE
            );
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
        {
            crate::yarm_log!("{} arch=x86_64 class=FutexWake", MARK_RETIRE_CLASS_BEGIN);
            crate::yarm_log!(
                "{} arch=x86_64 class=FutexWake result=ok",
                MARK_RETIRE_CLASS_DONE
            );
        }
    }
}

/// Hosted-dev: FutexWake stays on the unchanged global-lock path (the futex-word
/// validation uses the direct map, which only exists on real targets). The wake logic
/// itself (`futex_wake_split_mut`) is arch-neutral and unit-tested directly.
#[cfg(feature = "hosted-dev")]
fn try_split_futex_wake_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    None
}

/// Stage 199A2B2F: x86 pre-lock NR6 direct-request snapshot publication + off-lock
/// transaction drain (proof-gated). Runs ENTIRELY off the broad `KernelState` lock and
/// off any ranked lock during the source copy:
///   read args → capture caller `{tid,asid}` → validate `len<=128` → copy the request
///   payload through `copy_from_user_asid_split_read` (NO lock held) → build the owned
///   `IpcCallDirectSnapshot` → CLAIM the exact published blocked-server acknowledgement
///   → build one owned `DirectRequestPostWork` → drain it through the accepted
///   `SharedKernel::ipc_call_direct_request_txn`. No userspace payload pointer survives
///   the snapshot. On invalid length / copy fault / no committed ack, returns `None`
///   (the ack is never claimed, nothing is mutated) so NR6 stays on its existing path.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_ipccall_direct_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::capabilities::CapId;
    use crate::kernel::ipccall_direct::{IPC_DIRECT_PAYLOAD_MAX, IpcCallDirectSnapshot};
    use crate::kernel::syscall::{SYSCALL_ARG_CAP, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR};
    // NR6 ABI: arg(CAP)=send cap, arg(TRANSFER_CAP)=reply-endpoint recv cap,
    // arg(PTR)=payload ptr, arg(LEN)=len.
    use crate::kernel::direct_eligibility::{
        DirectRequestFacts, classify_direct_request_eligibility,
    };
    use crate::kernel::direct_ipc_counters::REQUEST as REQUEST_COUNTERS;
    let send_cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let reply_cap = CapId(frame.arg(crate::kernel::syscall::SYSCALL_ARG_TRANSFER_CAP) as u64);
    let user_ptr = frame.arg(SYSCALL_ARG_PTR);
    let len = frame.arg(SYSCALL_ARG_LEN);
    REQUEST_COUNTERS.note_attempt();

    // ── Stage 199D eligibility preflight ────────────────────────────────────────────────
    //
    // Gather the facts (reads only — a decline here mutates nothing), then decide through the
    // ONE pure exhaustive contract. A `Synchronous` endpoint declines here and falls through
    // to the legacy rendezvous path, which is the only path that reproduces its scheduling
    // semantics. The Stage 199A2B4 oracle confinement is carried into the facts unchanged.
    let tid = shared.current_tid_authoritative(cpu);
    let send_cap_resolution = match tid {
        Some(tid) => shared.resolve_endpoint_send_cap_split_read(tid, send_cap),
        None => Err(crate::kernel::boot::KernelError::InvalidCapability),
    };
    let endpoint_mode = match send_cap_resolution {
        Ok(crate::kernel::capabilities::CapObject::Endpoint { index, generation }) => {
            shared.endpoint_mode_split_read(index, generation)
        }
        _ => None,
    };
    let endpoint_admitted = match send_cap_resolution {
        Ok(crate::kernel::capabilities::CapObject::Endpoint { index, .. }) => {
            crate::kernel::boot::ipccall_direct_request_endpoint_admitted(index)
        }
        _ => false,
    };
    let facts = DirectRequestFacts {
        payload_len: len,
        requester_available: tid.is_some(),
        send_cap: send_cap_resolution,
        endpoint_mode,
        endpoint_admitted,
    };
    let verdict = classify_direct_request_eligibility(&facts);
    let Some((send_eidx, send_egen)) = verdict.endpoint() else {
        REQUEST_COUNTERS.note_declined_preflight(
            verdict.is_ineligible_mode(),
            verdict
                == crate::kernel::direct_eligibility::DirectRequestEligibility::EndpointNotAdmitted,
        );
        return None; // ineligible: no ack claim, no copy, no mutation — legacy path
    };
    REQUEST_COUNTERS.note_eligible();
    let tid = tid.expect("eligibility requires an available requester");
    let _ = IPC_DIRECT_PAYLOAD_MAX;
    // Stage 199A2D2C2B2: on the cross-CPU REQUEST path, if the server has NOT yet published its
    // blocked-server acknowledgement (it is not yet blocked in recv-v2), return a NON-MUTATING
    // WouldBlock so the CPU-0 client retries — never the legacy blocking IpcCall path, and never any
    // record reservation / Reply-cap mint / destination copy / waiter claim / enqueue / IPI. Counts
    // the early retry. Confined to the C2B2 selector so the SMP=1 oracle is unaffected.
    if crate::kernel::boot::x86_ipccall_direct_smp_request_enabled()
        && !crate::kernel::boot::ipccall_direct_ack::is_claimable(send_eidx, send_egen)
    {
        crate::kernel::boot::ipccall_direct_smp_request_note_early_wouldblock();
        frame.set_err(crate::kernel::syscall::SyscallError::WouldBlock.code());
        return Some(Ok(()));
    }
    let asid_raw = shared.task_asid_for_tid_split_read(tid);
    let caller = crate::kernel::boot::ReceiverWaiterIdentity::new(
        crate::kernel::ipc::ThreadId(tid),
        crate::kernel::vm::Asid(asid_raw as u16),
    );
    // Source copy OFF-LOCK (no broad/ranked lock held). A fault mutates nothing.
    //
    // From here to the ack claim, every decline is ELIGIBLE-but-pre-transaction: nothing has
    // been mutated, so the legacy path runs — but it must still land in a terminal bucket, or
    // the counters' balance invariant cannot hold.
    let Some(payload) = shared.copy_from_user_asid_split_read(asid_raw, user_ptr, len) else {
        REQUEST_COUNTERS.note_declined_pre_transaction();
        return None;
    };
    let Some(snapshot) = IpcCallDirectSnapshot::build(caller, send_cap, reply_cap, &payload[..len])
    else {
        REQUEST_COUNTERS.note_declined_pre_transaction();
        return None;
    };
    // Consume the acknowledgement published for EXACTLY this endpoint incarnation, at most
    // once (Stage 199D endpoint-keyed, generation-bearing store). A pair belonging to any
    // other endpoint, any other endpoint generation, or already consumed by a duplicate
    // trap yields `None` — no copy result is used, nothing is mutated, NR6 stays legacy.
    let Some((ack, ack_seq)) = crate::kernel::boot::ipccall_direct_ack::claim(send_eidx, send_egen)
    else {
        REQUEST_COUNTERS.note_declined_pre_transaction();
        return None;
    };
    let work = crate::kernel::ipccall_direct_txn::DirectRequestPostWork {
        snapshot,
        ack,
        ack_seq,
    };
    // Stage 199D HARD-STOP B: the transaction outcome is CLASSIFIED, never discarded. The
    // mapping is pure and exhaustive (`crate::kernel::direct_disposition`) — no wildcard arm,
    // so a new error variant cannot silently inherit "success".
    let outcome = shared.drain_direct_request_post_work(cpu, &work);
    let disposition = crate::kernel::direct_disposition::classify_direct_request_outcome(&outcome);
    crate::kernel::direct_ipc_counters::note_disposition(&REQUEST_COUNTERS, disposition);
    // Stage 199D HARD-STOP C: the frame is encoded by the SHARED encoder, which reproduces
    // the legacy `set_ok(0, 0, 0)` + `encode_transfer_cap_ret(frame, None)` success lanes
    // (`ret2 = SYSCALL_NO_TRANSFER_CAP`), zeroes every lane on failure, and leaves the frame
    // untouched on a decline so the legacy global-lock IpcCall runs against a pristine frame.
    // NR6 is request-send-only: success returns now (the caller blocks via a later recv).
    crate::kernel::direct_disposition::apply_direct_disposition(frame, disposition).map(|()| Ok(()))
}

#[cfg(feature = "hosted-dev")]
fn try_split_ipccall_direct_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    // Hosted: the off-lock user-read seam uses the direct map (real targets only). The
    // drain + transaction are exercised directly by the stage199a2b2f hosted tests.
    None
}

/// Stage 199A2B3 (proof-gated, default-OFF): intercept `IpcReply` (NR 7) BEFORE the
/// broad `KernelState` lock and drive the accepted off-lock direct-reply transaction.
///
/// Part 1 — owned pre-lock reply snapshot. Order:
///   read args → capture replier `{tid,asid}` → validate `len<=128` → copy the reply
///   payload through `copy_from_user_asid_split_read` (NO lock held) → build the owned
///   `IpcReplyDirectSnapshot` → CLAIM the exact published blocked-caller acknowledgement
///   → build one owned `DirectReplyPostWork` → drain it through the accepted
///   `SharedKernel::ipc_reply_direct_txn`. No userspace payload pointer survives the
///   snapshot. On invalid length / copy fault / no committed ack, returns `None` (the
///   ack is never claimed, nothing is mutated) so NR7 stays on its existing path.
///
/// # 199A2D-RR §1 — the one-shot barrier and the enumerated visibility order
///
/// THE BARRIER is the reply record's `Reserved → Consumed` transition, taken under the
/// rank-3 IPC claim. After it, a stale or aliased reply capability that still resolves to
/// the same `(record index, generation)` fails through the `Consumed` record — before its
/// physical CNode slots are reclaimed, and before the caller is woken. The record state,
/// not the capability slot, is what makes the reply one-shot; slot reclamation is only
/// storage recovery behind it.
///
/// Everything fallible and still-retryable is ordered AHEAD of the barrier, and everything
/// past it is irrevocable. The full order, blocked (`DeliverBlocked`) mode:
///
/// ```text
///   pre-barrier — a refusal here mutates nothing and may still decline or fall back
///     1  facts + eligibility verdict (replier probe resolved first)
///     2  SMP pre-ack, then the reply payload copied IN from the replier
///     3  owned snapshot built (no user pointer survives it)
///     4  MODE chosen: claimable acknowledgement → blocked; else unarmed terminal → queued
///     5  EXCLUSIVE rank-3 terminal claim  ── Open → Reserved(Reply)
///     6  acknowledgement claimed, at most once, keyed by reply-endpoint incarnation
///     7  record reserved            ── Available → Reserved  (exact replier)
///     8  reply payload copied OUT to the caller's buffer
///     9  recv-v2 meta copied OUT to the caller
///    10  endpoint waiter claimed (removable, and restorable on a later refusal)
///    11  blocked receiver committed ── the caller becomes Runnable
///   ── THE BARRIER ────────────────────────────────────────────────────────────────
///    12  record consumed             ── Reserved → Consumed
///   post-barrier — the one-shot is spent; no other claimant may win
///    13  reply authority reclaimed   ── both CNode slots revoked through one owner
///    14  caller enqueued             ── the SINGLE wake, LAST and non-fallible
///    15  endpoint-waiter claim consumed
///    16  terminal claim resolved     ── Reserved(Reply) → Completed
///    17  record slot released        ── only on success, only AFTER (16)
/// ```
///
/// Two orderings in that list are load-bearing rather than incidental:
///
/// * (12) before (13) and (14). The barrier precedes both the authority revoke and the
///   wake, so no window exists in which the caller is running while the record would still
///   authorize a second reply.
/// * (17) after (16). The slot is handed back to the allocator only once the terminal cell
///   is `Completed`; releasing it while the cell is still `Reserved(Reply)` would let a
///   reallocation of this slot arm over a live claim.
///
/// The queued (`QueueUnblocked`) mode reaches the same barrier through one rank-3
/// acquisition, and settles in the order its reverse link requires:
///
/// ```text
///     1..4 as above (the caller is NOT blocked, so there is no ack and no terminal)
///     5' revalidate record generation, `Available`, exact replier, endpoint incarnation
///     6' enqueue into the reply endpoint — admission decided FIRST; a refusal here leaves
///        the record `Available` and the reply exactly re-sendable
///   ── THE BARRIER ────────────────────────────────────────────────────────────────
///     7' record consumed          ── Available → Consumed, record left PRESENT
///     8' reverse link closed      ── resolves the responder FROM the still-present record
///     9' record slot released     ── through the same release owner as (17)
///    10' reply authority reclaimed
///    11' a receiver is woken ONLY if the commit actually removed one from the waiter
///        table; a polling receiver with no published waiter gets no artificial wake
/// ```
#[cfg(not(feature = "hosted-dev"))]
fn try_split_ipcreply_direct_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::capabilities::CapId;
    use crate::kernel::ipccall_direct::{IPC_DIRECT_PAYLOAD_MAX, IpcReplyDirectSnapshot};
    use crate::kernel::syscall::{SYSCALL_ARG_CAP, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR};
    // NR7 ABI: arg(CAP)=reply cap, arg(PTR)=payload ptr, arg(LEN)=len.
    use crate::kernel::direct_eligibility::{DirectReplyFacts, classify_direct_reply_eligibility};
    use crate::kernel::direct_ipc_counters::REPLY as REPLY_COUNTERS;
    let reply_cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let user_ptr = frame.arg(SYSCALL_ARG_PTR);
    let len = frame.arg(SYSCALL_ARG_LEN);
    REPLY_COUNTERS.note_attempt();

    // ── Stage 199D eligibility preflight ────────────────────────────────────────────────
    //
    // NR7 eligibility is tied to a live ONE-SHOT `Reply` object and its exact caller /
    // reply-endpoint incarnation. There is deliberately NO `EndpointMode` requirement: NR7
    // does not send to an endpoint, it consumes a reply authority the request path already
    // minted and delivers to a caller already committed-blocked on its reply endpoint, so the
    // endpoint's queueing discipline never applies. The Stage 199A2B4 oracle confinement is
    // carried into the facts unchanged.
    let tid = shared.current_tid_authoritative(cpu);
    let reply_object = match tid {
        Some(tid) => shared.resolve_reply_cap_split_read(tid, reply_cap),
        None => Err(crate::kernel::boot::KernelError::InvalidCapability),
    };
    // Stage 199D: carry the reply endpoint GENERATION too — the acknowledgement store is
    // keyed by the exact endpoint incarnation, not by index alone.
    let reply_endpoint = match reply_object {
        Ok((rec_idx, rec_gen)) => shared.reply_record_endpoint_ref_split_read(rec_idx, rec_gen),
        Err(_) => None,
    };
    let endpoint_admitted = match reply_endpoint {
        Some((eidx, _)) => crate::kernel::boot::ipccall_direct_reply_endpoint_admitted(eidx),
        None => false,
    };
    // 199D-TRC: the replier's exact incarnation is needed by the terminal classification, so it
    // is resolved here rather than after the verdict. Both reads; nothing is mutated.
    let replier_probe = tid.map(|t| {
        crate::kernel::boot::ReceiverWaiterIdentity::new(
            crate::kernel::ipc::ThreadId(t),
            crate::kernel::vm::Asid(shared.task_asid_for_tid_split_read(t) as u16),
        )
    });
    let facts = DirectReplyFacts {
        payload_len: len,
        requester_available: tid.is_some(),
        reply_object,
        reply_endpoint,
        endpoint_admitted,
        // Asked through the SAME canonical predicate the legacy `transfer_cap_arg` decode
        // uses, so the two can never disagree about what "cap-bearing" means. The direct
        // transaction cannot transfer a capability, so a cap-bearing reply must decline
        // before any mutation rather than deliver the payload and drop the capability.
        transfer_cap_present: crate::kernel::syscall::ipc_abi::transfer_cap_arg_present(frame),
        // 199D-TRC: the ADVISORY terminal classification, exact in record incarnation, caller,
        // replier and reply-endpoint incarnation. An armed-and-available cell ADMITS this
        // reply — it is one of the cell's five legitimate claimants — and the exclusive claim
        // is taken at the mutation point below. Only a competitor-owned, already-settled or
        // identity-mismatched cell declines, and each declines before any mutation.
        terminal: match (reply_object, reply_endpoint, replier_probe) {
            (Ok((rec_idx, rec_gen)), Some((eidx, egen)), Some(replier)) => shared
                .classify_direct_reply_terminal_split_read(rec_idx, rec_gen, replier, eidx, egen),
            // Without a resolved record, endpoint incarnation or replier there is no identity to
            // be exact about. Those cases are declined by their own facts above; naming the
            // terminal `IdentityMismatch` here keeps the field from ever reading as permissive.
            _ => crate::kernel::direct_eligibility::DirectReplyTerminal::IdentityMismatch,
        },
    };
    let verdict = classify_direct_reply_eligibility(&facts);
    let Some((reply_eidx, reply_egen)) = verdict.endpoint() else {
        // NR7 has no mode decline by construction, so this is never an ineligible-mode count.
        REPLY_COUNTERS.note_declined_preflight_reply(
            verdict
                == crate::kernel::direct_eligibility::DirectReplyEligibility::EndpointNotAdmitted,
            verdict.is_transfer_cap_decline(),
            verdict.is_terminal_arbitration_decline(),
        );
        // DIRECT3-CAP-FINAL §7 — CLOSE THE DETERMINISTIC-REFUSAL EDGE.
        //
        // One shape of ineligibility is not an "ask the broad path instead": it is a refusal
        // whose answer is already known here. When the capability resolves to a `Reply` object
        // but the record it names is gone, generation-stale or no longer invokable — because a
        // deadline, a peer death, a caller exit or an endpoint destruction settled the terminal
        // — the legacy path's ONLY remaining act is to fail. `resolve_reply_index` refuses with
        // `StaleCapability`, which the syscall wrapper maps to `SyscallError::WrongObject`.
        //
        // Entering the broad dispatcher purely to be told that is a terminal edge that buys
        // nothing. The refusal is given here instead, from the SAME typed error written the
        // same way, so the user-visible result is byte-identical — and given having mutated
        // NOTHING, because this is before the record reservation, the terminal claim, the
        // envelope stash and the acknowledgement claim.
        //
        // The predicate is the legacy one mirrored exactly, not a re-derivation from the
        // terminal classification: a classification can be `IdentityMismatch` for reasons whose
        // legacy answer is NOT this error, so the decision is made on the record itself. An
        // unresolved capability still declines to legacy, because then this route has no record
        // identity to be exact about.
        if let Ok((rec_idx, rec_gen)) = reply_object
            && !shared.reply_record_externally_invokable_split_read(rec_idx, rec_gen)
        {
            crate::yarm_log!(
                "IPCREPLY_DIRECT_REFUSED_PRE_LOCK record_index={} record_generation={} replier_tid={} terminal={:?} reply_copies=0 caller_wakes=0 mutations=0 err=WrongObject result=ok",
                rec_idx,
                rec_gen,
                tid.unwrap_or(0),
                facts.terminal
            );
            frame.set_err(crate::kernel::syscall::SyscallError::WrongObject.code());
            return Some(Ok(()));
        }
        return None; // ineligible: no ack claim, no copy, no mutation — legacy path
    };
    REPLY_COUNTERS.note_eligible();
    let tid = tid.expect("eligibility requires an available requester");
    let _ = IPC_DIRECT_PAYLOAD_MAX;
    // Stage 199A2D2C2C: on the cross-CPU REPLY path, bound the CPU-1 server's pre-ack NR7 retry and
    // refuse a duplicate NR7 — WITHOUT touching the legacy path or the accepted transaction. The
    // blocked-caller ack VALID bit is published exactly once (when the CPU-0 caller blocks on its reply
    // endpoint) and never cleared; the CLAIMED bit distinguishes "not yet delivered" from "already
    // delivered". So:
    //   * no ack published yet (snapshot None) → the caller has not blocked: non-mutating WouldBlock
    //     (the server retries, bounded ≤64 in userspace). No copy / claim / enqueue / IPI / wake.
    //   * ack published but no longer claimable (VALID && CLAIMED) → the one successful reply already
    //     consumed the record: a duplicate NR7. Refuse with canonical `WrongObject`; ZERO additional
    //     copies / claims / enqueues / IPIs / wakes (the Consumed record is the one-shot barrier).
    //   * ack claimable (VALID && !CLAIMED) → fall through to the accepted claim + reply transaction.
    #[cfg(not(feature = "hosted-dev"))]
    if crate::kernel::boot::x86_ipccall_direct_smp_reply_enabled() {
        use crate::kernel::syscall::SyscallError;
        if crate::kernel::boot::ipcreply_direct_ack::snapshot(reply_eidx, reply_egen).is_none() {
            crate::kernel::boot::ipcreply_direct_smp_reply_note_early_wouldblock();
            frame.set_err(SyscallError::WouldBlock.code());
            return Some(Ok(()));
        }
        if !crate::kernel::boot::ipcreply_direct_ack::is_claimable(reply_eidx, reply_egen) {
            crate::kernel::boot::ipcreply_direct_smp_note_duplicate_refused();
            crate::yarm_log!(
                "IPCREPLY_DIRECT_SMP_DUPLICATE_REFUSED arch=x86_64 reason=consumed_barrier reply_copies=1 caller_wakes=1 ipis=1 result=ok"
            );
            frame.set_err(SyscallError::WrongObject.code());
            return Some(Ok(()));
        }
    }
    let asid_raw = shared.task_asid_for_tid_split_read(tid);
    // The same incarnation the terminal classification was keyed on.
    let replier = crate::kernel::boot::ReceiverWaiterIdentity::new(
        crate::kernel::ipc::ThreadId(tid),
        crate::kernel::vm::Asid(asid_raw as u16),
    );
    let (rec_idx, rec_gen) = match reply_object {
        Ok(pair) => pair,
        // Unreachable: eligibility required a resolved record. Fail closed rather than assume.
        Err(_) => {
            REPLY_COUNTERS.note_declined_pre_transaction();
            return None;
        }
    };
    // Source copy OFF-LOCK (no broad/ranked lock held). A fault mutates nothing. As on the
    // NR6 twin, every decline from here to the ack claim is eligible-but-pre-transaction and
    // is counted as such — the oracle server's bounded pre-acknowledgement retries live here.
    let Some(payload) = shared.copy_from_user_asid_split_read(asid_raw, user_ptr, len) else {
        REPLY_COUNTERS.note_declined_pre_transaction();
        return None;
    };
    let Some(snapshot) = IpcReplyDirectSnapshot::build(replier, reply_cap, &payload[..len]) else {
        REPLY_COUNTERS.note_declined_pre_transaction();
        return None;
    };
    // 199D-TRC: probe the acknowledgement WITHOUT consuming it, so the ordinary
    // "caller has not blocked yet" decline still happens BEFORE any terminal claim and can
    // still fall back. Everything fallible-and-fallback-worthy is ordered ahead of the claim.
    // DIRECT3-QUEUECAP §3 — CHOOSE THE MODE, before any mutation.
    //
    // A reply has two production shapes and the direct route only ever implemented one.
    // "No claimable acknowledgement" is not a generic decline: together with an unarmed
    // terminal it is the positive signature of the QUEUED mode — the caller is not blocked on
    // its reply endpoint, so there is nothing to deliver into and the reply is enqueued for a
    // later receive. Treating that signature as "let the broad path have it" is what left a
    // permanent legacy population.
    //
    // `Unarmed` alone is never sufficient: the acknowledgement store is consulted too, and the
    // queued commit re-validates the record, its binding and the endpoint incarnation against
    // live state in the same acquisition that mutates.
    let mode = if crate::kernel::boot::ipcreply_direct_ack::is_claimable(reply_eidx, reply_egen) {
        // DIRECT3-CAP-FINAL: the caller IS blocked, so the cap-bearing lane applies when the
        // reply carries a capability. Both lanes claim the same terminal through the same
        // arbitration; they differ only in who performs the delivery and when the reply
        // settles.
        if facts.transfer_cap_present {
            crate::kernel::direct_eligibility::DirectReplyMode::DeliverBlockedWithCap
        } else {
            crate::kernel::direct_eligibility::DirectReplyMode::DeliverBlocked
        }
    } else if matches!(
        facts.terminal,
        crate::kernel::direct_eligibility::DirectReplyTerminal::Unarmed
    ) && !facts.transfer_cap_present
    {
        // A cap-bearing reply to an UNBLOCKED caller would have to ride its transfer envelope
        // in the queued message and be materialized by the receive-side owner on a later
        // receive. That class is not witnessed in production and this lane does not claim it:
        // it declines pre-mutation and the legacy path owns it, rather than being enqueued
        // with a capability nothing would materialize.
        crate::kernel::direct_eligibility::DirectReplyMode::QueueUnblocked
    } else {
        // An armed terminal with no claimable acknowledgement is neither delivery mode: the
        // record is mid-transaction or settling. Refuse pre-mutation rather than guess.
        REPLY_COUNTERS.note_declined_pre_transaction();
        return None;
    };
    // ── DIRECT3-CAP-FINAL — the CAP-BEARING blocked lane ────────────────────────────────
    //
    // Composed entirely from owners that already exist and are already live on the split
    // IpcSend boundary: the transfer-envelope stash, the blocked-waiter ordinary-cap producer,
    // its executor's materialize/rollback seams, and the reply's own terminal, authority,
    // record and reverse-link owners. Nothing here is a second implementation of any of them.
    //
    // The reply claims its terminal HERE and settles it NOWHERE here. Materializing the
    // capability and copying the caller's payload and metadata are the last steps that can
    // still fail, and both run in the executor; so the claim, the record reservation and the
    // authority identities travel to it as a typed continuation. Committing the terminal or
    // revoking the authority at this point would make a materialization failure unrecoverable.
    if mode == crate::kernel::direct_eligibility::DirectReplyMode::DeliverBlockedWithCap {
        let Some(transfer_cap) = crate::kernel::syscall::ipc_abi::transfer_cap_arg_value(frame)
        else {
            REPLY_COUNTERS.note_declined_pre_transaction();
            return None;
        };
        // The caller this reply settles, read from the record itself — never re-derived.
        let Some(caller) = shared.reply_record_caller_split_read(rec_idx, rec_gen) else {
            REPLY_COUNTERS.note_declined_pre_transaction();
            return None;
        };
        // (1) The one-shot authority identities, snapshotted BEFORE any mutation so a recycled
        // record can never hand out another transaction's slots.
        let Some(authority) = shared.reply_authority_slots_split_read(rec_idx, rec_gen) else {
            REPLY_COUNTERS.note_declined_pre_transaction();
            return None;
        };
        // (2) Reserve the record for this exact replier: `Available → Reserved`. Refused
        // pre-mutation if the record is not this replier's to answer.
        if !shared.reserve_existing_reply_record_split(rec_idx, rec_gen, replier) {
            REPLY_COUNTERS.note_declined_pre_transaction();
            return None;
        }
        // (3) THE EXCLUSIVE CLAIM, through the same single authority every other terminal
        // claimant uses. A loser mutates nothing and does NOT fall back to the broad
        // dispatcher: the record has an owner and that owner will complete it.
        let claim = shared
            .claim_direct_reply_terminal_split(rec_idx, rec_gen, replier, reply_eidx, reply_egen);
        let terminal_owner = match claim {
            crate::kernel::boot::DirectReplyTerminalClaim::Won(owner) => owner,
            crate::kernel::boot::DirectReplyTerminalClaim::NotArmed => {
                // The ack was claimable, so a terminal must be armed. Restore and refuse.
                let _ = shared.release_reply_record_split(rec_idx, rec_gen);
                REPLY_COUNTERS.note_declined_pre_transaction();
                return None;
            }
            crate::kernel::boot::DirectReplyTerminalClaim::Lost(class) => {
                let _ = shared.release_reply_record_split(rec_idx, rec_gen);
                REPLY_COUNTERS.note_declined_pre_transaction();
                crate::yarm_log!(
                    "IPCREPLY_DIRECT_TERMINAL_LOST record_index={} record_generation={} replier_tid={} reason={:?} reply_copies=0 caller_wakes=0 result=ok",
                    rec_idx,
                    rec_gen,
                    tid,
                    class
                );
                frame.set_err(crate::kernel::syscall::SyscallError::WrongObject.code());
                return Some(Ok(()));
            }
        };
        // (4) Stash the transfer envelope through the SAME owner the split IpcSend route uses.
        // It only RESOLVES the replier's source capability — it never takes it — which is why
        // a later failure can hand the reply back genuinely re-sendable.
        let reply_endpoint_object = crate::kernel::capabilities::CapObject::Endpoint {
            index: reply_eidx,
            generation: reply_egen,
        };
        let stashed = shared.stash_transfer_envelope_split(
            crate::kernel::ipc::ThreadId(tid),
            transfer_cap,
            reply_endpoint_object,
            Some(caller.tid),
            None,
        );
        let Ok(stashed) = stashed else {
            let _ = shared.release_direct_reply_terminal_split(rec_idx, &terminal_owner);
            let _ = shared.release_reply_record_split(rec_idx, rec_gen);
            REPLY_COUNTERS.note_declined_pre_transaction();
            return None;
        };
        crate::yarm_log!(
            "IPC_REPLY_DIRECT_CAP_STASH tid={} transfer_cap={} handle={} endpoint={} endpoint_generation={} caller_tid={}",
            tid,
            transfer_cap.0,
            stashed.handle,
            reply_eidx,
            reply_egen,
            caller.tid.0
        );
        // (5) The message the caller receives, framed exactly as the legacy reply frames it:
        // FLAG_CAP_TRANSFER_PLAIN, so the receiver does not strip an opcode prefix a reply
        // never prepends.
        let Ok(msg) = crate::kernel::syscall::ipc_abi::frame_reply_message_with_cap(
            tid,
            &payload[..len],
            stashed.handle,
        ) else {
            let _ = shared.release_direct_reply_terminal_split(rec_idx, &terminal_owner);
            let _ = shared.release_reply_record_split(rec_idx, rec_gen);
            REPLY_COUNTERS.note_declined_pre_transaction();
            return None;
        };
        // (6) The delivery, produced by the existing owner, carrying the reply lifecycle.
        let continuation = crate::kernel::dispatch_post_work::ReplyTerminalContinuation {
            record_index: rec_idx,
            record_generation: rec_gen,
            terminal_owner,
            authority,
            replier,
            caller,
        };
        match shared.produce_blocked_waiter_ordinary_cap_delivery_split(
            caller.tid.0,
            reply_eidx,
            &msg,
            Some(continuation),
        ) {
            Ok(true) => {
                crate::yarm_log!(
                    "IPC_REPLY_DIRECT_CAP_PRODUCED record_index={} record_generation={} replier_tid={} caller_tid={} endpoint={} result=ok",
                    rec_idx,
                    rec_gen,
                    tid,
                    caller.tid.0,
                    reply_eidx
                );
                // The acknowledgement published for this exact endpoint incarnation is spent
                // by this delivery; consume it so no second reply can claim the same caller.
                let _ = crate::kernel::boot::ipcreply_direct_ack::claim(reply_eidx, reply_egen);
                crate::kernel::direct_ipc_counters::note_disposition(
                    &REPLY_COUNTERS,
                    crate::kernel::direct_disposition::DirectDisposition::Completed,
                );
                return crate::kernel::direct_disposition::apply_direct_disposition(
                    frame,
                    crate::kernel::direct_disposition::DirectDisposition::Completed,
                )
                .map(|()| Ok(()));
            }
            // Declined or failed having consumed nothing irreversible: hand the reply back
            // re-sendable. The envelope is dropped with it, and the replier still holds the
            // source capability it only ever resolved.
            Ok(false) | Err(_) => {
                let _ = shared.take_transfer_envelope_facts_split(
                    stashed.handle,
                    reply_eidx,
                    caller.tid,
                );
                let _ = shared.release_direct_reply_terminal_split(rec_idx, &terminal_owner);
                let _ = shared.release_reply_record_split(rec_idx, rec_gen);
                REPLY_COUNTERS.note_declined_pre_transaction();
                return None;
            }
        }
    } else if mode == crate::kernel::direct_eligibility::DirectReplyMode::QueueUnblocked {
        // The queued mode carries no capability today: a cap-bearing reply declined at the
        // transfer-cap fact above, before the mode was chosen, so the envelope-bearing message
        // shape cannot reach here. Plain framing, exactly as the broad path builds it when it
        // has no transfer handle.
        let Ok(msg) = crate::kernel::ipc::Message::new(tid, &payload[..len]) else {
            REPLY_COUNTERS.note_declined_pre_transaction();
            return None;
        };
        let authority = shared.reply_authority_slots_split_read(rec_idx, rec_gen);
        return match shared
            .commit_queued_reply_split(rec_idx, rec_gen, replier, reply_eidx, reply_egen, msg)
        {
            Ok(woken) => {
                // The one-shot is spent, so its authority slots are reclaimed through the same
                // owner the blocked mode uses. The transferred payload carries no capability
                // here, so nothing else is owed.
                if let Some(slots) = authority {
                    let reclaim = shared.reclaim_reply_authority_split(slots, tid);
                    crate::yarm_log!(
                        "IPC_REPLY_QUEUED_AUTHORITY_RECLAIMED record_index={} record_generation={} replier_tid={} replier_ok={} caller_ok={} result=ok",
                        rec_idx,
                        rec_gen,
                        tid,
                        u8::from(reclaim.replier_revoked),
                        u8::from(reclaim.caller_revoked)
                    );
                }
                crate::yarm_log!(
                    "IPC_REPLY_QUEUED_SPLIT_OK record_index={} record_generation={} replier_tid={} endpoint={} endpoint_generation={} len={} woken={} result=ok",
                    rec_idx,
                    rec_gen,
                    tid,
                    reply_eidx,
                    reply_egen,
                    len,
                    woken.map(|w| w.tid.0).unwrap_or(0)
                );
                // A receiver that blocked between classification and commit was taken out of
                // the waiter table by the commit, so waking it is this transaction's to do —
                // the same contract the broad path's `SchedulerWakePlain::Wake` carries.
                if let Some(w) = woken {
                    shared.sr_enqueue_committed_receiver_split(w.tid.0, None);
                }
                crate::kernel::direct_disposition::apply_direct_disposition(
                    frame,
                    crate::kernel::direct_disposition::DirectDisposition::Completed,
                )
                .map(|()| Ok(()))
            }
            Err(_) => {
                // Nothing was mutated: the record is still `Available` and the reply is exactly
                // re-sendable. This is the one refusal the broad path cannot offer, because it
                // consumes the record before it ever reaches the queue.
                REPLY_COUNTERS.note_declined_pre_transaction();
                None
            }
        };
    }
    // 199D-TRC — THE EXCLUSIVE CLAIM. Classify and compare-exchange in one rank-3 acquisition,
    // through the same single-authority `TerminalCell` that timeout, peer death, caller exit and
    // endpoint destruction claim. The preflight classification above was advisory; this is the
    // step that decides. A loser mutates nothing and does NOT fall back to the broad
    // dispatcher — the record has a terminal owner, and that owner will complete it.
    let terminal_claim =
        shared.claim_direct_reply_terminal_split(rec_idx, rec_gen, replier, reply_eidx, reply_egen);
    let terminal_owner = match terminal_claim {
        crate::kernel::boot::DirectReplyTerminalClaim::NotArmed => None,
        crate::kernel::boot::DirectReplyTerminalClaim::Won(owner) => Some(owner),
        crate::kernel::boot::DirectReplyTerminalClaim::Lost(class) => {
            // Eligible, but the exclusive claim was lost — counted where every other
            // eligible-but-pre-transaction refusal is counted. It is deliberately NOT a
            // preflight decline: preflight passed, and the arbitration outcome is reported
            // by the marker below rather than folded into the preflight subset.
            REPLY_COUNTERS.note_declined_pre_transaction();
            crate::yarm_log!(
                "IPCREPLY_DIRECT_TERMINAL_LOST record_index={} record_generation={} replier_tid={} reason={:?} reply_copies=0 caller_wakes=0 result=ok",
                rec_idx,
                rec_gen,
                tid,
                class
            );
            // A typed terminal result, never a fallback: the reply authority this replier held
            // has been settled by another claimant, so the canonical answer is the same one a
            // duplicate reply gets. Zero copies, zero wakes, zero mutation.
            frame.set_err(crate::kernel::syscall::SyscallError::WrongObject.code());
            return Some(Ok(()));
        }
    };
    // Consume the acknowledgement published for EXACTLY this reply-endpoint incarnation,
    // at most once (Stage 199D endpoint-keyed, generation-bearing store).
    let Some((ack, ack_seq)) =
        crate::kernel::boot::ipcreply_direct_ack::claim(reply_eidx, reply_egen)
    else {
        // Enumerated post-claim failure #1: the acknowledgement was claimable a moment ago and
        // is not now. Restore the exact claim so the record is left precisely as it was found,
        // then decline pre-mutation. A stale restore mutates nothing.
        if let Some(owner) = terminal_owner.as_ref()
            && !shared.release_direct_reply_terminal_split(rec_idx, owner)
        {
            // Unreachable while we hold `Reserved`; fail closed rather than fall back with an
            // unresolved claim.
            frame.set_err(crate::kernel::syscall::SyscallError::WrongObject.code());
            return Some(Ok(()));
        }
        REPLY_COUNTERS.note_declined_pre_transaction();
        return None;
    };
    let work = crate::kernel::ipccall_direct_txn::DirectReplyPostWork {
        snapshot,
        ack,
        ack_seq,
    };
    // Stage 199D HARD-STOP B: classified, never discarded — see the NR6 twin.
    let outcome = shared.drain_direct_reply_post_work(cpu, &work);
    // 199D-TRC — enumerated post-claim failure #2..n: resolve the claim against what the
    // transaction actually did, exhaustively and by the transaction's OWN documented
    // post-states. `Release` is used for every outcome that left the reply authority
    // re-sendable (nothing delivered, a retryable copy fault, or an enqueue refusal that the
    // transaction explicitly restores); `Commit` for every outcome past the publication line,
    // where the one-shot is spent and no other claimant may win.
    if let Some(owner) = terminal_owner.as_ref() {
        use crate::kernel::ipccall_direct_txn::IpcReplyDirectError as E;
        let commit = match &outcome {
            Ok(_) => true,
            Err(
                E::WouldBlock
                | E::ReplyCapResolve(_)
                | E::ReservePreconditionFailed
                | E::WaiterLost
                | E::LeaseNotClaimed
                | E::PayloadCopyFault
                | E::MetaCopyFault
                | E::EnqueueRejected(_),
            ) => false,
            Err(
                E::WaiterLostAfterCopy
                | E::CallerGone
                | E::RecordConsumeFailed
                | E::EnqueueRejectedUnreconciled(_)
                | E::ReceiverMembershipViolation,
            ) => true,
        };
        let settled = if commit {
            shared.commit_direct_reply_terminal_split(rec_idx, owner)
        } else {
            shared.release_direct_reply_terminal_split(rec_idx, owner)
        };
        crate::yarm_log!(
            "IPCREPLY_DIRECT_TERMINAL_CLAIM record_index={} record_generation={} replier_tid={} terminal=Reply resolution={} settled={} result=ok",
            rec_idx,
            rec_gen,
            tid,
            if commit { "commit" } else { "release" },
            u8::from(settled)
        );
    }
    // DIRECT3-QUEUE3 — RELEASE THE REPLY-RECORD SLOT, last, and only on success.
    //
    // Legacy `ipc_reply` frees the slot (`ipc.reply_caps[slot] = None`); the direct path did
    // not, so every direct reply permanently consumed one of `MAX_REPLY_CAPS` slots. Ordered
    // after the terminal commit on purpose: releasing while the cell is still `Reserved(Reply)`
    // would let a reallocation of this slot `arm` over a live claim. Exact by record generation
    // and by the `Consumed` state, so a repeat, a stale caller or a recycled slot frees nothing.
    if matches!(outcome, Ok(_)) {
        shared.release_consumed_reply_record_split(rec_idx, rec_gen);
    }
    let disposition = crate::kernel::direct_disposition::classify_direct_reply_outcome(&outcome);
    crate::kernel::direct_ipc_counters::note_disposition(&REPLY_COUNTERS, disposition);
    // Same shared encoder as the NR6 twin: legacy `handle_ipc_reply` ends with the identical
    // `set_ok(0, 0, 0)` + `encode_transfer_cap_ret(frame, None)` pair, so NR7's success lanes
    // are the same three values. NR7 delivers the reply and wakes the caller; the replier
    // itself returns Ok.
    crate::kernel::direct_disposition::apply_direct_disposition(frame, disposition).map(|()| Ok(()))
}

#[cfg(feature = "hosted-dev")]
fn try_split_ipcreply_direct_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    // Hosted: the off-lock user-read seam uses the direct map (real targets only). The
    // drain + transaction are exercised directly by the stage199a2b3 hosted tests.
    None
}

// ── Stage 191D FUTEXWAIT BLOCK-PUBLISH SEAM markers + deferral ─────────────────────────
//
// FutexWait (NR 1) is DEFERRED: it is NOT added to `classify_split_eligible_nr_only` and
// stays FULLY global-lock-only. Unlike DebugLog/FutexWake/InitramfsReadChunk, a matched
// FutexWait BLOCKS the caller and must dispatch a DIFFERENT runnable task — the
// queue-ADVANCING "switch_required" case that `dispatch_next_task` performs. The kernel's
// own out-of-lock dispatch relocation (D6-GENUINE, `exec_state.rs::dispatch_next_task`)
// explicitly restricts itself to the queue-NEUTRAL case and falls back to the in-lock
// (global-lock) path with `reason=switch_required` for exactly this scenario, so the
// futex-wait block+dispatch cannot be serviced off the global lock without the disclaimed
// multi-stage dispatch rewrite. Stage 191D therefore LANDS + proves the block-publish seam
// (`SharedKernel::futex_wait_would_block_split_read` = Phase A value-check,
// `SharedKernel::futex_wait_publish_block_split_mut` = Phase B block-publish) as
// HELPER-ONLY, ready for that future stage, but does NOT wire FutexWait live.
//
/// FutexWait split marker vocabulary (emitted only if/when FutexWait is wired live; the
/// block-publish seam emits `FUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK` today from its Phase B).
pub const MARK_FUTEX_WAIT_SPLIT_BEGIN: &str = "FUTEX_WAIT_SPLIT_BEGIN";
pub const MARK_FUTEX_WAIT_SPLIT_VALUE_CHECK_OK: &str = "FUTEX_WAIT_SPLIT_VALUE_CHECK_OK";
pub const MARK_FUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK: &str = "FUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK";
pub const MARK_FUTEX_WAIT_SPLIT_DONE_BLOCKED: &str = "FUTEX_WAIT_SPLIT_DONE result=blocked";
/// The one concrete blocker that keeps FutexWait's LIVE retirement deferred: the matched
/// wait's queue-advancing dispatch is the global-lock `switch_required` case.
pub const MARK_FUTEX_WAIT_DEFERRED_REASON: &str = "GLOBAL_LOCK_RETIRE_CLASS_DEFERRED class=FutexWait reason=block_dispatch_switch_required_needs_global_lock";

/// # Validation status
/// - LIVE_TRAP_SMOKE_X86_64 (Stage 32B) — wired into the live trap seam:
///   `try_split_dispatch_into_frame` routes IpcRecv (NR 2) here BEFORE the global
///   lock. Only the kernel-task queued-plain case is serviced; every other case
///   returns `None` and propagates to the unchanged global-lock fallback. See
///   `doc/KERNEL_LOCKING.md` §50.11.
///
/// Stage 31 split-recv seam: attempt to service an `IpcRecv` for a plain queued
/// message on a buffered endpoint, delivered to a kernel-task receiver, with no
/// recv-v2 metadata. Default-deny for every other case.
///
// Lock order: [no lock] → current_tid_authoritative (takes+releases global) →
//             ipc_state_lock (rank 3) → [release] → [no lock]
// Forbidden under ipc_state_lock: scheduler lock, capability lock, VM lock, user-copy
// task_switched: always false (no dispatch/yield/switch)
///
/// Returns:
/// * `Some(Ok(()))` — a plain message was dequeued; success lanes are written into
///   `frame` byte-for-byte as the kernel-task branch of the old recv path
///   (`set_ok(sender, raw_len, NO_TRANSFER_CAP)` + inline payload words).
/// * `Some(Err(e))` — the recv cap was invalid; `e` is the same error the old
///   global-lock recv path returned.
/// * `None` — NOT split-eligible (default-deny): empty queue, recv-v2, cap-transfer
///   or reply-cap message, user-ASID receiver (would require a forbidden user copy),
///   sender-waiter refill, blocking, timeout, or a non-IpcRecv syscall.
///
/// Stage 32B live-wire scope: the realistic live x86_64 receivers (PM/init/VFS) are
/// user-ASID tasks whose plain-recv writeback needs `copy_to_current_user`, which
/// is still forbidden on the split path — those are rejected here (`None`) and fall
/// back unchanged. Only a kernel-task receiver of a queued plain message is
/// serviced on the split path; the endpoint-cap resolution is performed via the
/// Stage 32 phase-separated split-read (`resolve_endpoint_recv_cap_split_read`).
pub(crate) fn try_split_ipc_recv_queued_plain_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    // Number-only default-deny gate: only IpcRecv is considered here.
    let syscall = Syscall::decode(frame.syscall_num()).ok()?;
    if !matches!(syscall, Syscall::IpcRecv) {
        return None;
    }
    shared.try_split_ipc_recv_queued_plain_into_frame(cpu, frame)
}

/// # Validation status
/// - M2_SEAM_LIVE_D3_BRK_SHRINK (Stage 114) — wired into the live trap seam:
///   `try_split_dispatch_into_frame` routes `VmBrk` (NR 14) here BEFORE the
///   global lock. Only the page-crossing shrink case (at most one CPU online,
///   group-leader caller) is serviced; every other case returns `None` and
///   propagates to the unchanged global-lock fallback (`handle_vm_brk`).
///
/// Thin number-only gate mirroring [`try_split_ipc_recv_queued_plain_into_frame`]:
/// re-decode the syscall number defensively, reject anything but `VmBrk`, then
/// delegate to `SharedKernel::try_split_vm_brk_shrink_into_frame`, which holds
/// the full eligibility logic and the single-CPU-online safety proof.
pub(crate) fn try_split_vm_brk_shrink_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    // Number-only default-deny gate: only VmBrk is considered here.
    let syscall = Syscall::decode(frame.syscall_num()).ok()?;
    if !matches!(syscall, Syscall::VmBrk) {
        return None;
    }
    shared.try_split_vm_brk_shrink_into_frame(cpu, frame)
}

/// U9-SPAWN1 SP-2 — the pre-lock NR 11 (`SpawnThread`) route.
///
/// NR 11 is the smallest member of the spawn family: it creates no address space, loads no ELF,
/// mints no capability, creates no endpoint, maps no page and never switches tasks. Its whole
/// body is rank 2 followed by rank 1, which is why it lands in the NON-SWITCHING lane where
/// `entering_tid == exiting_tid` and `task_switched == false` hold for the existing architecture
/// writeback on all three targets — the same lane NR 15 and NR 28 already use. No new resume
/// consumer, no queue-advance publication, no arch-specific adapter.
///
/// The disposition is exhaustive in the direction that matters. Two refusals may still fall back
/// — an unavailable requester, and a parent whose process CNode does not yet exist, which the
/// broad handler would create. Both are reads. Everything after the first mutation is terminal:
/// the transaction compensates its own incarnation and returns the exact error the broad handler
/// would have returned, because falling back would let the broad handler spawn a SECOND thread
/// for a request it never saw refused.
fn try_split_spawn_thread_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::syscall::{SYSCALL_ARG_CAP, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR, SyscallError};

    // ── Pre-mutation. Both refusals here are reads and may still fall back. ──
    let parent_tid = shared.current_tid_authoritative(cpu)?;
    // A thread joins its parent's EXISTING process CNode; registration only ever ensures it.
    // If it is somehow absent the broad handler must create it, so decline before mutating.
    shared.task_cnode_split(parent_tid)?;

    let tls_base = frame.arg(SYSCALL_ARG_CAP);
    let user_stack_top = frame.arg(SYSCALL_ARG_PTR);
    let user_entry = frame.arg(SYSCALL_ARG_LEN);

    // ── The transaction. From here every outcome is terminal. ──
    match shared.try_spawn_thread_split(cpu, parent_tid, tls_base, user_stack_top, user_entry) {
        Ok(tid) => {
            let Ok(ret) = usize::try_from(tid) else {
                return Some(Err(TrapHandleError::Syscall(SyscallError::Internal)));
            };
            frame.set_ok(ret, 0, 0);
            Some(Ok(()))
        }
        Err(err) => Some(Err(TrapHandleError::Syscall(SyscallError::from(err)))),
    }
}

/// U9-MO2 §4 — the pre-lock NR 28 (`CreateInitramfsFileSliceMo`) route.
///
/// NR 28 was the smallest live production class still reaching a terminal broad acquisition. Its
/// whole body is: an access gate, a bounded user string copy, a pure CPIO lookup on the immutable
/// boot initrd, one MemoryObject install and one capability mint — every one of which already had
/// an off-lock owner once the MemoryObject lifecycle learned that an initramfs slice's backing is
/// BORROWED. Before that, an off-lock mint failure had no exact compensation to call: the only
/// reclaim path would have handed the boot initrd's own frames to the allocator.
///
/// The disposition is exhaustive by construction. Everything fallible that can still fall back
/// runs BEFORE the first mutation, so `None` is only reachable there; from the object install
/// onward every path returns `Some`, carrying either the success lanes or the exact error the
/// broad handler would have produced. `Some(Err(..))` after a mutation is deliberate: falling
/// back would let the broad handler create a SECOND object for a request it never saw refused.
///
/// Byte-for-byte at the ABI boundary with `handle_create_initramfs_file_slice_mo`: the same
/// `SystemServer` gate and `MissingRight`, the same `name_len` bounds and `flags != 0` rejection,
/// the same leading-slash and `initramfs/` prefix stripping, the same `InvalidArgs` for a bad
/// UTF-8 name / missing entry / empty file, and the same `set_ok(0, cap_id, file_len)` success.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_create_initramfs_mo_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::syscall::SyscallError;
    use crate::kernel::task::TaskClass;
    use yarm_srv_common::cpio::CpioArchive;

    let fail = |e: SyscallError| -> Option<Result<(), TrapHandleError>> {
        Some(Err(TrapHandleError::Syscall(e)))
    };

    // ── Pre-mutation. Every refusal here may still fall back; none has touched anything. ──
    let tid = shared.current_tid_authoritative(cpu)?;
    // Access gate: SystemServer only, exactly as the broad handler gates it.
    if shared.task_class_split_read(tid) != Some(TaskClass::SystemServer) {
        crate::yarm_log!(
            "CREATE_INITRAMFS_FILE_SLICE_MO_DENIED tid={} reason=not_system_server",
            tid
        );
        return fail(SyscallError::MissingRight);
    }
    let name_ptr = frame.arg(0);
    let name_len = frame.arg(1);
    let flags = frame.arg(2) as u64;
    if name_len == 0 || name_len > 128 || flags != 0 {
        return fail(SyscallError::InvalidArgs);
    }
    let asid_raw = shared.task_asid_for_tid_split_read(tid);
    let Some(name_buf) = shared.copy_from_user_asid_split_read(asid_raw, name_ptr, name_len) else {
        return fail(SyscallError::InvalidArgs);
    };
    let Ok(raw_name) = core::str::from_utf8(&name_buf[..name_len]) else {
        return fail(SyscallError::InvalidArgs);
    };
    // The same normalisation the broad handler applies, in the same order.
    let name = raw_name.trim_start_matches('/');
    let name = name.strip_prefix("initramfs/").unwrap_or(name);
    let name = name.trim_start_matches('/');

    let Some(initrd) = crate::kernel::boot::Bootstrap::boot_initrd_bytes() else {
        return fail(SyscallError::InvalidArgs);
    };
    let Ok(entry) = CpioArchive::new(initrd).find(name) else {
        return fail(SyscallError::InvalidArgs);
    };
    let Some(cpio_entry) = entry else {
        crate::yarm_log!("CREATE_INITRAMFS_FILE_SLICE_MO_NOT_FOUND name={}", name);
        return fail(SyscallError::InvalidArgs);
    };
    let file_data = cpio_entry.file_data();
    let file_len = file_data.len();
    if file_len == 0 {
        crate::yarm_log!("CREATE_INITRAMFS_FILE_SLICE_MO_EMPTY name={}", name);
        return fail(SyscallError::InvalidArgs);
    }
    let Some(file_data_offset) =
        (file_data.as_ptr() as usize).checked_sub(initrd.as_ptr() as usize)
    else {
        return fail(SyscallError::InvalidArgs);
    };
    // The destination cspace, resolved before anything is created.
    let Some(cnode) = shared.task_cnode_split(tid) else {
        return fail(SyscallError::InvalidCapability);
    };

    // ── The transaction. From here every outcome is `Some`: a post-mutation fallback would
    //    let the broad handler build a second object for a request it never saw. ──
    match shared.create_initramfs_file_slice_mo_split(cnode, initrd, file_data_offset, file_len) {
        Ok((mo_id, cap_id)) => {
            crate::yarm_log!(
                "CREATE_INITRAMFS_FILE_SLICE_MO_OK tid={} name={} file_len={} mo_id={} cap={}",
                tid,
                name,
                file_len,
                mo_id,
                cap_id.0
            );
            crate::yarm_log!(
                "CREATE_INITRAMFS_FILE_SLICE_MO_SPLIT_OK tid={} mo_id={} cap={} offset={} file_len={} backing=borrowed result=ok",
                tid,
                mo_id,
                cap_id.0,
                file_data_offset,
                file_len
            );
            frame.set_ok(0, cap_id.0 as usize, file_len);
            Some(Ok(()))
        }
        Err(err) => {
            // Compensated: the object (if any) is released through the backing-aware owner and
            // the mint rolled back its own refcount, so nothing is left behind. The caller sees
            // exactly the error the broad handler would have returned.
            crate::yarm_log!(
                "CREATE_INITRAMFS_FILE_SLICE_MO_SPLIT_FAIL tid={} name={} err={:?} objects=0 caps=0 result=compensated",
                tid,
                name,
                err
            );
            fail(SyscallError::from(err))
        }
    }
}

/// Hosted: the off-lock user-read seam uses the direct map, which only exists on the real
/// targets. The transaction itself is exercised directly by the `u9mo2_nr28_*` hosted tests.
#[cfg(feature = "hosted-dev")]
fn try_split_create_initramfs_mo_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    None
}

/// Hosted: both spawn routes read the caller's startup-args array through the off-lock user-read
/// seam, which uses the direct map and therefore only exists on the real targets. The transaction
/// they run is exercised directly by the `u9spawntxn3_*` hosted tests through both adapters.
#[cfg(feature = "hosted-dev")]
fn try_split_spawn_process_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    None
}

/// Hosted counterpart of [`try_split_spawn_from_mo_into_frame`]; see the note above.
#[cfg(feature = "hosted-dev")]
fn try_split_spawn_from_mo_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    None
}

/// Number-only split eligibility classifier (no arg validation, no lock).
///
/// Used by [`try_split_dispatch_into_frame`] as the fast default-deny gate before
/// reading any scheduler/task state. Argument-precondition validation is still
/// performed by `classify_split_eligible`, so a syscall that passes this gate but
/// fails its preconditions (e.g. `target_pid == 0`) still falls back to the
/// global-lock path for the canonical error encoding.
/// Snapshot the spawning task's identity ONCE, before any phase runs.
///
/// U9-SPAWN-IC1's rule: the caller identity is established up front and passed explicitly, never
/// re-read from an ambient current-task lookup partway through a transaction whose locks are
/// released between phases.
fn spawn_owners_for(
    shared: &SharedKernel,
    cpu: CpuId,
) -> Option<crate::kernel::syscall::spawn_txn::SharedSpawnOwners<'_>> {
    let tid = shared.current_tid_authoritative(cpu)?;
    Some(crate::kernel::syscall::spawn_txn::SharedSpawnOwners {
        shared,
        spawner_tid: Some(tid),
        spawner_cnode: shared.task_cnode_split(tid),
        cpu,
    })
}

/// U9-FORK1 §4 — NR 12 `Fork`, before the terminal acquisition.
///
/// The whole route is: resolve the caller, run THE fork transaction through `SharedSpawnOwners`,
/// place the child TID in the parent's return lane. There is no argument to validate — Fork takes
/// none — and nothing is read from user memory, which is why this route (unlike the two spawn
/// routes) is not `cfg`-gated and is exercised by the hosted suite as well as the three targets.
///
/// The child's return lane is not set here. It was installed by the publication, from
/// `fork_child_context`, which is the single owner of that decision on every path.
/// U9-EXIT1 §5 — NR 16 `ExitCurrentTask`, before the terminal acquisition.
///
/// The only admitted class that cannot return through its own frame. It reserves the existing
/// queue-advance deferral before anything irreversible, claims and removes itself, performs the
/// cleanup, and answers `QueueAdvanceCommitted` so the EXISTING post-lock drain selects and applies
/// the next context. No second selector, no second scheduler policy, no second drain.
///
/// Every refusal is pre-mutation and returns `NotHandled`, so the broad handler produces the exact
/// answer it always did — including its `WouldBlock` decline when a reply is owed and no deferred
/// slot is free, which this route reproduces rather than reinterprets. After the claim there is no
/// fallback: re-entering the broad handler would mint a second restart token, publish a second
/// disposition and re-sweep records this transaction already retired.
fn try_split_exit_current_task(shared: &SharedKernel, cpu: CpuId) -> SplitDispatchDisposition {
    use crate::kernel::boot::exit_claim::ExitRefusal;
    use crate::kernel::syscall::exit_txn::{SharedExitOwners, run_exit_transaction};
    type D = SplitDispatchDisposition;

    let mut owners = SharedExitOwners { shared };
    match run_exit_transaction(
        &mut owners,
        cpu,
        crate::kernel::syscall::EXIT_STATUS_SELF_REQUESTED,
    ) {
        Ok(outcome) => {
            let claim = &outcome.claim;
            // The same three markers the broad handler emits, in the broad handler's causal order,
            // so an observer sees one vocabulary for one syscall regardless of route.
            crate::yarm_log!(
                "EXIT_TASK_SYSCALL_DISPATCHED nr={} tid={} asid={} target=self result=ok",
                crate::kernel::syscall::SYSCALL_EXIT_CURRENT_TASK_NR,
                claim.tid(),
                claim.sweep_asid().0
            );
            crate::yarm_log!(
                "EXIT_TASK_LIFECYCLE_TRANSITION tid={} asid={} syscall_returns=0 result=ok",
                claim.tid(),
                claim.sweep_asid().0
            );
            crate::yarm_log!(
                "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=exit_current_task_switch_required tid={} cpu={}",
                claim.tid(),
                cpu.0
            );
            D::QueueAdvanceCommitted
        }
        Err(refusal) if refusal.may_fall_back() => {
            crate::yarm_log!(
                "EXIT_TASK_SPLIT_DECLINED cpu={} reason={} task_mutation=none",
                cpu.0,
                refusal.marker()
            );
            D::NotHandled
        }
        Err(refusal) => {
            // `current` was already cleared when this was discovered, so the trap cannot re-enter
            // the broad dispatcher. Fail closed: the deferral is released, nothing is resumed, and
            // the existing terminal-idle settlement runs.
            debug_assert!(matches!(refusal, ExitRefusal::VictimChanged));
            crate::yarm_log!(
                "EXIT_TASK_SPLIT_FAILED_CLOSED cpu={} reason={}",
                cpu.0,
                refusal.marker()
            );
            D::Complete(Ok(()))
        }
    }
}

/// U9-REAP1 §4 — NR 31 `ReapFaultedTask`, before the terminal acquisition.
///
/// Gate for gate the broad handler: PM-only, never self-targeting, and only a terminal task. The
/// state gate is deliberately kept here AND enforced again by the claim inside the transaction —
/// this one produces the exact `TASK_REAP_FAULTED_REJECT` marker the oracle counts, while the
/// claim is what actually arbitrates, atomically, against a restart or exit that lands between
/// the two.
///
/// NON-SWITCHING. The reaping PM neither blocks nor yields nor changes address space, so the frame
/// is finalized once here and no queue is advanced. Every refusal is pre-mutation. There is no
/// broad fallback after the claim: re-entering the broad handler would re-sweep records this
/// transaction already retired.
fn try_split_reap_faulted_task_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::boot::reap_claim::ReapRefusal;
    use crate::kernel::syscall::SyscallError;
    use crate::kernel::syscall::reap_txn::{SharedReapOwners, run_reap_transaction};

    let fail = |e: SyscallError| -> Option<Result<(), TrapHandleError>> {
        Some(Err(TrapHandleError::Syscall(e)))
    };

    // PRE-MUTATION, and the ONLY case this route declines: with no resolvable caller there is no
    // authorization to check, so the broad handler re-derives the identical answer.
    let caller = shared.current_tid_authoritative(cpu)?;
    let target = frame.arg(0) as u64;
    // U9-REAP1 §6: the split half of the edge measurement. Emitted per invocation, not latched —
    // §6 asserts that this count equals the successful-reap count, which a one-shot marker could
    // not express.
    crate::yarm_log!(
        "TASK_REAP_SPLIT_ENTER caller_tid={} target_tid={}",
        caller,
        target
    );
    crate::yarm_log!(
        "TASK_REAP_FAULTED_BEGIN caller_tid={} target_tid={}",
        caller,
        target
    );

    if caller != crate::kernel::syscall::PM_BOOTSTRAP_TID {
        crate::yarm_log!(
            "TASK_REAP_FAULTED_REJECT target_tid={} reason=not_pm",
            target
        );
        return fail(SyscallError::MissingRight);
    }
    if target == caller {
        crate::yarm_log!("TASK_REAP_FAULTED_REJECT target_tid={} reason=self", target);
        return fail(SyscallError::InvalidArgs);
    }

    let mut owners = SharedReapOwners { shared };
    match run_reap_transaction(&mut owners, target) {
        Ok(_) => {
            crate::yarm_log!("TASK_REAP_FAULTED_OK target_tid={}", target);
            frame.set_ok(0, 0, 0);
            Some(Ok(()))
        }
        // Already gone, or already reaped by a winner that got here first. The broad handler
        // answers `Ok(0, 0, 0)` for a target that no longer exists, and a duplicate reap is the
        // same fact discovered one step later — so it gets the same answer, having mutated
        // nothing.
        Err(refusal) if refusal.is_already_reaped() => {
            crate::yarm_log!(
                "TASK_REAP_FAULTED_ALREADY_GONE target_tid={} reason={}",
                target,
                refusal.marker()
            );
            frame.set_ok(0, 0, 0);
            Some(Ok(()))
        }
        // A live task, a reservation, or a target still resident on a runqueue: the broad
        // handler's `WrongObject`, with zero mutation behind it.
        Err(refusal) => {
            debug_assert!(matches!(
                refusal,
                ReapRefusal::NonTerminal | ReapRefusal::StillScheduled | ReapRefusal::NoProcess
            ));
            crate::yarm_log!(
                "TASK_REAP_FAULTED_REJECT target_tid={} reason={}",
                target,
                refusal.marker()
            );
            fail(SyscallError::WrongObject)
        }
    }
}

fn try_split_fork_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    // PRE-MUTATION refusal: no resolvable caller means no fork. Declining here returns `None` and
    // the broad handler re-derives the same answer.
    let Some(parent_tid) = shared.current_tid_authoritative(cpu) else {
        return None;
    };
    let mut owners = spawn_owners_for(shared, cpu)?;
    let parent_context = frame.capture_user_context();
    match crate::kernel::syscall::fork_txn::fork_process_cow(
        &mut owners,
        parent_tid,
        Some(parent_context),
    ) {
        Ok(child_tid) => {
            let Ok(ret0) = usize::try_from(child_tid) else {
                // Unreachable for any TID this kernel allocates, and NOT a place to fall back to
                // the broad path: the fork has committed, so a second attempt would fork twice.
                frame.set_err(crate::kernel::syscall::SyscallError::Internal as usize);
                return Some(Ok(()));
            };
            frame.set_ok(ret0, 0, 0);
            Some(Ok(()))
        }
        // A refused fork has already unwound itself completely; reporting the error is the whole
        // remaining work, and re-running it on the broad path would repeat the attempt.
        Err(err) => {
            frame.set_err(crate::kernel::syscall::SyscallError::from(err) as usize);
            Some(Ok(()))
        }
    }
}

/// Read and normalise the caller's startup-args array, off-lock.
///
/// Freestanding only: the off-lock user-read seam reads through the direct map, which exists
/// only on the real targets. The hosted suite drives the transaction directly instead.
#[cfg(not(feature = "hosted-dev"))]
///
/// Applies the SAME admission (`plan_spawn_startup_args`), the SAME little-endian decode and the
/// SAME kernel-owned-slot normalisation the broad handler applies — all three are shared pure
/// functions, so a route cannot admit an array the broad handler would refuse, nor let a
/// caller-supplied value survive into a slot the spawn is about to write.
fn split_normalized_startup_args(
    shared: &SharedKernel,
    tid: u64,
    ptr: usize,
    count: usize,
) -> Result<([u64; 18], [u64; 4]), crate::kernel::syscall::SyscallError> {
    use crate::kernel::syscall::process::{
        decode_spawn_startup_args_into, normalize_startup_args, plan_spawn_startup_args,
    };
    let mut out = [0u64; 18];
    let Some(byte_len) = plan_spawn_startup_args(ptr, count)? else {
        return Ok(normalize_startup_args(out));
    };
    let asid = shared.task_asid_for_tid_split_read(tid);
    let mut slot_idx = 0usize;
    let mut remaining = byte_len;
    let mut at = ptr;
    while remaining > 0 {
        let chunk = remaining.min(crate::kernel::ipc::Message::MAX_PAYLOAD);
        let Some(payload) = shared.copy_from_user_asid_split_read(asid, at, chunk) else {
            return Err(crate::kernel::syscall::SyscallError::InvalidArgs);
        };
        decode_spawn_startup_args_into(&mut out, &mut slot_idx, &payload[..chunk]);
        at = at
            .checked_add(chunk)
            .ok_or(crate::kernel::syscall::SyscallError::InvalidArgs)?;
        remaining -= chunk;
    }
    Ok(normalize_startup_args(out))
}

/// U9-SPAWN-TXN3 §4 — NR 23 `SpawnProcess`, before the terminal acquisition.
///
/// Every gate down to the ELF parse is a pure read of boot-time data or of the caller's own
/// memory, and each declines with `None` so the broad handler produces the exact error it always
/// did. The transaction is the first mutation, and from there every outcome is `Some`.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_spawn_process_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::syscall::spawn_image_txn;
    use crate::kernel::syscall::{SyscallError, process::spawn_image_path_for_image_id};
    use crate::kernel::task::TaskClass;
    use yarm_srv_common::cpio::CpioArchive;

    let fail = |e: SyscallError| -> Option<Result<(), TrapHandleError>> {
        Some(Err(TrapHandleError::Syscall(e)))
    };

    // ── Pre-mutation. Nothing below has touched anything until the transaction. ───────
    let mut owners = spawn_owners_for(shared, cpu)?;
    let tid = owners.spawner_tid?;

    let image_id = frame.arg(0) as u64;
    let parent_pid = frame.arg(1) as u64;
    let startup_args_ptr = frame.arg(2);
    let startup_args_count = frame.arg(3);
    crate::yarm_log!(
        "KSPAWN_ENTER image_id={} parent_pid={} args_count={}",
        image_id,
        parent_pid,
        startup_args_count
    );
    let spawn_lc = crate::kernel::boot::spawn_lifecycle_enabled();
    if spawn_lc {
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_REQUEST_BEGIN image_id={} parent_pid={}",
            image_id,
            parent_pid
        );
    }
    let (startup_args, extra_send_caps) =
        match split_normalized_startup_args(shared, tid, startup_args_ptr, startup_args_count) {
            Ok(v) => v,
            Err(e) => return fail(e),
        };
    const INITRAMFS_IMAGE_ID: u64 = 4;
    let Some(image_path) = spawn_image_path_for_image_id(image_id) else {
        if spawn_lc {
            crate::yarm_log!("SPAWN_LIFECYCLE_BAD_IMAGE_ID image_id={}", image_id);
        }
        return fail(SyscallError::InvalidArgs);
    };
    crate::yarm_log!("KSPAWN_PATH path={}", image_path);
    let Some(initrd) = crate::kernel::boot::Bootstrap::boot_initrd_bytes() else {
        return fail(SyscallError::InvalidArgs);
    };
    let entry = match CpioArchive::new(initrd).find(image_path) {
        Ok(Some(entry)) => entry,
        Ok(None) | Err(_) => {
            if spawn_lc {
                crate::yarm_log!("SPAWN_LIFECYCLE_IMAGE_RESOLVE_FAIL image_id={}", image_id);
            }
            return fail(SyscallError::InvalidArgs);
        }
    };
    let elf_bytes = entry.file_data();
    if spawn_lc {
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_IMAGE_RESOLVE_OK image_id={} bytes={}",
            image_id,
            elf_bytes.len()
        );
        crate::yarm_log!("SPAWN_LIFECYCLE_ELF_PARSE_BEGIN image_id={}", image_id);
    }
    crate::yarm_log!("KSPAWN_ELF_FOUND size={}", elf_bytes.len());
    let Ok(elf) = yarm_srv_common::elf::ElfImageInfo::parse(image_id, elf_bytes) else {
        return fail(SyscallError::InvalidArgs);
    };
    crate::yarm_log!("KSPAWN_ELF_PARSED entry={}", elf.entry);
    if spawn_lc {
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_ELF_PARSE_OK image_id={} entry=0x{:x}",
            image_id,
            elf.entry
        );
    }

    // ── The transaction. From here every outcome is `Some`. ──────────────────────────
    match spawn_image_txn::run_image_spawn_transaction(
        &mut owners,
        spawn_image_txn::SpawnImageRequest {
            image_id,
            image_path,
            source: spawn_image_txn::SpawnImageSource::PtLoadSegments {
                elf: elf_bytes,
                entry: elf.entry as usize,
            },
            class: TaskClass::SystemServer,
            parent_pid,
            startup_args,
            extra_send_caps,
            map_initrd_window: image_id == INITRAMFS_IMAGE_ID,
            lifecycle_markers: spawn_lc,
        },
    ) {
        Ok(committed) => {
            frame.set_ok(0, committed.reply_tid, committed.packed_ret2 as usize);
            Some(Ok(()))
        }
        Err(e) => fail(e),
    }
}

/// U9-SPAWN-TXN3 §4 — NR 29 `SpawnFromMemoryObject`, before the terminal acquisition.
///
/// Identical in every respect to NR 23 except the `ImageSource` it builds: the image comes from a
/// MemoryObject the caller already holds, loaded zero-copy from the initrd blob.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_spawn_from_mo_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::capabilities::{CapId, CapObject};
    use crate::kernel::syscall::spawn_image_txn;
    use crate::kernel::syscall::{SyscallError, process::spawn_image_path_for_image_id};
    use crate::kernel::task::TaskClass;

    let fail = |e: SyscallError| -> Option<Result<(), TrapHandleError>> {
        Some(Err(TrapHandleError::Syscall(e)))
    };

    let mut owners = spawn_owners_for(shared, cpu)?;
    let caller_tid = owners.spawner_tid?;
    // Access gate: PM only, exactly as the broad handler gates it.
    if caller_tid != crate::kernel::syscall::PM_BOOTSTRAP_TID {
        crate::yarm_log!("SPAWN_FROM_MO_DENIED tid={} reason=not_pm", caller_tid);
        return fail(SyscallError::MissingRight);
    }

    let image_id = frame.arg(0) as u64;
    let mo_cap_raw = frame.arg(1) as u64;
    let parent_pid = frame.arg(2) as u64;
    let startup_args_ptr = frame.arg(3);
    let startup_args_count = frame.arg(4);
    crate::yarm_log!(
        "SPAWN_FROM_MO_ENTER image_id={} mo_cap={} parent_pid={}",
        image_id,
        mo_cap_raw,
        parent_pid
    );

    let Ok(capability) = shared.resolve_capability_for_task_split(caller_tid, CapId(mo_cap_raw))
    else {
        return fail(SyscallError::InvalidCapability);
    };
    let CapObject::MemoryObject { id: mo_id } = capability.object else {
        crate::yarm_log!(
            "SPAWN_FROM_MO_WRONG_CAP image_id={} mo_cap={}",
            image_id,
            mo_cap_raw
        );
        return fail(SyscallError::WrongObject);
    };
    let Some((file_data_offset, file_len)) = shared.with_memory_split_mut(|memory| {
        memory
            .memory_objects
            .iter()
            .flatten()
            .find(|mo| mo.id == mo_id)
            .and_then(|mo| match mo.kind {
                crate::kernel::boot::MemoryObjectKind::InitramfsFileSlice {
                    initrd_offset,
                    file_len,
                } => Some((initrd_offset as usize, file_len as usize)),
                _ => None,
            })
    }) else {
        return fail(SyscallError::WrongObject);
    };
    let Some(initrd) = crate::kernel::boot::Bootstrap::boot_initrd_bytes() else {
        return fail(SyscallError::InvalidArgs);
    };
    let Some(end) = file_data_offset.checked_add(file_len) else {
        return fail(SyscallError::InvalidArgs);
    };
    if end > initrd.len() {
        crate::yarm_log!(
            "SPAWN_FROM_MO_BOUNDS_ERR image_id={} off={} len={} initrd_len={}",
            image_id,
            file_data_offset,
            file_len,
            initrd.len()
        );
        return fail(SyscallError::InvalidArgs);
    }
    let elf_bytes = &initrd[file_data_offset..end];
    crate::yarm_log!(
        "SPAWN_FROM_MO_ELF image_id={} elf_len={}",
        image_id,
        elf_bytes.len()
    );
    let Ok(elf) = yarm_srv_common::elf::ElfImageInfo::parse(image_id, elf_bytes) else {
        return fail(SyscallError::InvalidArgs);
    };
    crate::yarm_log!("SPAWN_FROM_MO_ENTRY entry=0x{:x}", elf.entry);
    let Some(image_path) = spawn_image_path_for_image_id(image_id) else {
        return fail(SyscallError::InvalidArgs);
    };
    let (startup_args, extra_send_caps) = match split_normalized_startup_args(
        shared,
        caller_tid,
        startup_args_ptr,
        startup_args_count,
    ) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let initrd_virt_raw = initrd.as_ptr() as u64;
    let initrd_phys_base = {
        let virt_base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE;
        let phys_base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_PHYS_BASE;
        if virt_base > phys_base && initrd_virt_raw >= virt_base {
            initrd_virt_raw - virt_base + phys_base
        } else {
            initrd_virt_raw
        }
    };

    // ── The transaction. From here every outcome is `Some`. ──────────────────────────
    match spawn_image_txn::run_image_spawn_transaction(
        &mut owners,
        spawn_image_txn::SpawnImageRequest {
            image_id,
            image_path,
            source: spawn_image_txn::SpawnImageSource::ZeroCopyInitramfsSlice {
                elf: elf_bytes,
                initrd_phys_base,
                file_initrd_offset: file_data_offset as u64,
            },
            class: TaskClass::SystemServer,
            parent_pid,
            startup_args,
            extra_send_caps,
            map_initrd_window: false,
            lifecycle_markers: false,
        },
    ) {
        Ok(committed) => {
            crate::yarm_log!(
                "SPAWN_FROM_MO_OK image_id={} spawned_tid={}",
                image_id,
                committed.tid
            );
            frame.set_ok(0, committed.reply_tid, committed.packed_ret2 as usize);
            Some(Ok(()))
        }
        Err(e) => fail(e),
    }
}

fn classify_split_eligible_nr_only(syscall: Syscall) -> Option<Syscall> {
    match syscall {
        Syscall::ControlPlaneSetCnodeSlots => Some(syscall),
        // Stage 32B: IpcRecv (NR 2) passes the NR gate so the live seam attempts the
        // kernel-task queued-plain split via `try_split_ipc_recv_queued_plain_into_frame`.
        // Final eligibility (kernel-task receiver, queued plain, no sender-wake/recv-v2)
        // is decided inside that helper; ineligible cases return `None` → fallback.
        Syscall::IpcRecv => Some(syscall),
        // Stage 114: VmBrk (NR 14) passes the NR gate so the live seam attempts the
        // page-crossing-shrink split via `try_split_vm_brk_shrink_into_frame`. Final
        // eligibility (group leader, page-crossing shrink, single CPU online) is
        // decided inside that helper; ineligible cases return `None` → fallback.
        Syscall::VmBrk => Some(syscall),
        // Stage 191A (GLOBAL-LOCK-RETIRE, first class): DebugLog (NR 15) is a pure READ
        // syscall — it resolves the current task, copies user bytes, logs, and never
        // blocks/yields/switches tasks or mutates KernelState. It is serviced off the
        // global lock via `try_split_debug_log_into_frame`. Any case it cannot service
        // returns `None` → unchanged global-lock fallback.
        Syscall::DebugLog => Some(syscall),
        // Stage 191B (GLOBAL-LOCK-RETIRE, second class): FutexWake (NR 10) — the CALLER
        // never task-switches; it only mutates waiter/run-queue state (Blocked→Runnable
        // + enqueue). Serviced off the global lock via `try_split_futex_wake_into_frame`
        // (task split-mut wake scan + scheduler split-mut enqueue). NOT FutexWait (NR 9,
        // which blocks the caller — stays global-lock-only). Ineligible cases (invalid
        // addr) return `None` → unchanged global-lock fallback, which produces the exact
        // error. (NR 11 is SpawnThread, NOT FutexWake — do not confuse the two.)
        Syscall::FutexWake => Some(syscall),
        // U9-MO2 §4: CreateInitramfsFileSliceMo (NR 28) — the smallest live production class
        // still reaching a terminal broad acquisition. Its object is BORROWED initrd backing, so
        // its compensation never touches the frame allocator; every owner it needs already exists
        // off-lock. `try_split_create_initramfs_mo_into_frame` decides the rest.
        // U9-SPAWN1 SP-2: SpawnThread (NR 11) — the smallest spawn-family class. Its whole body
        // is rank 2 then rank 1: no address space, no ELF, no endpoint, no capability, no VM
        // work and no task switch. `try_split_spawn_thread_into_frame` decides the rest.
        Syscall::SpawnThread => Some(syscall),
        Syscall::CreateInitramfsFileSliceMo => Some(syscall),
        // U9-SPAWN-TXN3 §4: SpawnProcess (NR 23) and SpawnFromMemoryObject (NR 29) — the last two
        // live production classes reaching a terminal broad acquisition. Both execute the SAME
        // generic spawn transaction the broad path executes; they differ only in how the image
        // reaches the new address space, which is the `ImageSource` each one builds. Their
        // rollback is the exact provisional-capability closure U9-SPAWN-TXN3 §1 derived, so every
        // owner they need exists off-lock. `try_split_spawn_process_into_frame` and
        // `try_split_spawn_from_mo_into_frame` decide the rest.
        Syscall::SpawnProcess => Some(syscall),
        Syscall::SpawnFromMemoryObject => Some(syscall),
        // U9-FORK1 §4: Fork (NR 12). It runs the SAME generic fork transaction the broad path
        // runs, over the same `SharedSpawnOwners`, and unlike the two spawn classes it reads
        // NOTHING from user memory — no startup-args array, no ELF — so the route needs no
        // off-lock user-read seam and exists identically on every profile.
        Syscall::Fork => Some(syscall),
        // U9-REAP1 §4: ReapFaultedTask (NR 31). It runs the SAME reap transaction the broad
        // handler runs, over `SharedReapOwners`, and like Fork it reads NOTHING from user memory,
        // so it needs no off-lock user-read seam. NR 31 is NON-SWITCHING for the calling PM: the
        // reap never makes the caller block, yield or change address space, so the route finalizes
        // the caller's frame once and advances no queue.
        Syscall::ReapFaultedTask => Some(syscall),
        // Stage 197A removed the former NR 27 InitramfsReadChunk split class along with the
        // syscall. Its sibling note — that NR 28 MINTS a capability and therefore "stays
        // global-lock-only" — was retired by U9-MO2 §4: the mint was never the obstacle, the
        // UNCLASSIFIED BACKING was. With `MemoryObjectKind::backing()` exhaustive and the reclaim
        // owner backing-aware, the mint's rollback is exact off-lock, so NR 28 is admitted above
        // rather than excluded here.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::scheduler::CpuId;
    use crate::kernel::syscall::{
        SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR, SYSCALL_COUNT, SYSCALL_IPC_RECV_NR,
        SYSCALL_IPC_SEND_NR, SYSCALL_SPAWN_PROCESS_NR, SYSCALL_VM_MAP_NR,
    };
    use crate::kernel::task::TaskClass;

    fn decode(nr: usize) -> Syscall {
        Syscall::decode(nr).expect("decode syscall nr")
    }

    /// Boot a SharedKernel with a SystemServer requester (900) and an App target
    /// (901), with the requester dispatched as the current task — the same setup
    /// the Stage 27 control-plane helper test uses.
    fn shared_with_control_plane_requester() -> (SharedKernel, u64, u64) {
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
        let _ = CpuId(0);
        (kernel, 900, 901)
    }

    #[test]
    fn stage28_split_dispatch_whitelist_accepts_cnode_slots_syscall() {
        let (kernel, requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before capacity");
        let requested = before.saturating_add(4);

        let syscall = decode(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR);
        let args = [target, requested as u64, 0, 0, 0, 0];

        // Must be classified eligible.
        assert_eq!(
            classify_split_eligible(syscall, requester, args),
            Some(SplitEligibleSyscall::ControlPlaneCnodeSlots {
                requester_tid: requester,
                target_pid: target,
                slots: requested,
            }),
            "control-plane cnode-slots must be split-eligible"
        );

        // Must dispatch through the split path and mutate the capability domain.
        let result = try_split_dispatch(&kernel, syscall, requester, args);
        assert_eq!(
            result,
            Some(Ok(())),
            "split dispatch must service the syscall"
        );

        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(
            after,
            Some(requested),
            "split path must resize the target cnode"
        );
    }

    #[test]
    fn stage28_split_dispatch_whitelist_rejects_ipc_send() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let syscall = decode(SYSCALL_IPC_SEND_NR);
        let args = [1, 2, 3, 4, 5, 6];
        assert_eq!(classify_split_eligible(syscall, 1, args), None);
        assert_eq!(
            try_split_dispatch(&kernel, syscall, 1, args),
            None,
            "IPC send must fall back to the global-lock path"
        );
    }

    #[test]
    fn stage28_split_dispatch_whitelist_rejects_ipc_recv() {
        // Stage 32B: IpcRecv now classifies as `IpcRecvKernelTask` (it is serviced by
        // the frame-level seam), but the ARG-ONLY `try_split_dispatch` path still
        // returns `None` — IpcRecv is never serviced through this entry point; it
        // defers to `try_split_dispatch_into_frame` / global-lock fallback.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let syscall = decode(SYSCALL_IPC_RECV_NR);
        let args = [1, 2, 3, 4, 5, 6];
        assert_eq!(
            classify_split_eligible(syscall, 1, args),
            Some(SplitEligibleSyscall::IpcRecvKernelTask)
        );
        assert_eq!(
            try_split_dispatch(&kernel, syscall, 1, args),
            None,
            "IPC recv must not be serviced by the arg-only split path"
        );
    }

    #[test]
    fn stage28_split_dispatch_whitelist_rejects_spawnv5() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let syscall = decode(SYSCALL_SPAWN_PROCESS_NR);
        let args = [1, 2, 3, 4, 5, 6];
        assert_eq!(classify_split_eligible(syscall, 1, args), None);
        assert_eq!(
            try_split_dispatch(&kernel, syscall, 1, args),
            None,
            "SpawnV5 must fall back to the global-lock path"
        );
    }

    #[test]
    fn stage28_split_dispatch_whitelist_rejects_vm_map() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let syscall = decode(SYSCALL_VM_MAP_NR);
        let args = [1, 2, 3, 4, 5, 6];
        assert_eq!(classify_split_eligible(syscall, 1, args), None);
        assert_eq!(
            try_split_dispatch(&kernel, syscall, 1, args),
            None,
            "VM map must fall back to the global-lock path"
        );
    }

    #[test]
    fn stage28_split_dispatch_fallback_preserved_for_unwhitelisted() {
        // Every non-whitelisted syscall number must classify as None — the
        // default-deny contract. We exhaustively walk every decodable syscall and
        // assert that only ControlPlaneSetCnodeSlots and IpcRecv (Stage 32B) are
        // ever eligible, and that the ARG-ONLY `try_split_dispatch` services none of
        // them with zero args (IpcRecv is always deferred to the frame-level seam).
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let args = [0u64; 6]; // zero args → even cnode-slots fails preconditions → None
        for nr in 0..SYSCALL_COUNT {
            let Ok(syscall) = Syscall::decode(nr) else {
                continue; // gaps in the NR space are not valid syscalls
            };
            // With zero args, only IpcRecv (NR 2, no arg preconditions) classifies
            // eligible; cnode-slots fails its preconditions and everything else is
            // default-deny.
            if matches!(syscall, Syscall::IpcRecv) {
                assert_eq!(
                    classify_split_eligible(syscall, 1, args),
                    Some(SplitEligibleSyscall::IpcRecvKernelTask),
                    "IpcRecv must classify as split-eligible (frame-level serviced)"
                );
            } else {
                assert_eq!(
                    classify_split_eligible(syscall, 1, args),
                    None,
                    "syscall nr {} must default-deny with zero args",
                    nr
                );
            }
            assert_eq!(
                try_split_dispatch(&kernel, syscall, 1, args),
                None,
                "syscall nr {} must not be serviced by the arg-only split path with zero args",
                nr
            );
        }
        // And the control-plane syscall with valid args IS the sole eligible one.
        let cp = decode(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR);
        assert!(
            classify_split_eligible(cp, 1, [5, 8, 0, 0, 0, 0]).is_some(),
            "control-plane cnode-slots with valid args must be eligible"
        );
    }

    #[test]
    fn stage28_syscall_count_unchanged() {
        // ABI guard: the split-dispatch scaffold is pure additive infrastructure
        // and must not alter the syscall ABI.
        assert_eq!(SYSCALL_COUNT, 32, "Stage 28 must not change SYSCALL_COUNT");
    }

    #[test]
    fn stage28_stage27_split_mut_helper_still_works() {
        // Regression: the Stage 27 split-mut helper the bridge delegates to must
        // still behave identically when invoked directly.
        let (kernel, requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(8);
        kernel
            .control_plane_set_process_cnode_slots_split_mut(requester, target, requested)
            .expect("split-mut helper");
        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(after, Some(requested), "Stage 27 helper must still resize");

        // Absent requester still yields the stable TaskMissing error.
        let err = kernel
            .control_plane_set_process_cnode_slots_split_mut(123_456, target, 8)
            .expect_err("absent requester must fail");
        assert_eq!(err, KernelError::TaskMissing);
    }

    // ----------------------------------------------------------------------
    // Stage 29 — live-wired result-writeback seam (try_split_dispatch_into_frame)
    // ----------------------------------------------------------------------

    use crate::kernel::trapframe::TrapFrame;

    const CPU0: CpuId = CpuId(0);

    /// Build the same NR-8 trap frame the live arch path constructs:
    /// arg(SYSCALL_ARG_CAP)=target_pid, arg(SYSCALL_ARG_PTR)=slots.
    fn cnode_slots_frame(target_pid: u64, slots: usize) -> TrapFrame {
        TrapFrame::new(
            SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR,
            [target_pid as usize, slots, 0, 0, 0, 0],
        )
    }

    /// Boot a SharedKernel where an App requester (901) is the current task on
    /// CPU 0, plus a second App target (902). Used to exercise the MissingRight
    /// guard (a non-system-server App may only resize its own cnode).
    fn shared_with_app_requester() -> (SharedKernel, u64, u64) {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state
                .register_task_with_class(901, TaskClass::App)
                .expect("app requester");
            state
                .register_task_with_class(902, TaskClass::App)
                .expect("app target");
            state.enqueue_current_cpu(901).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            if state.current_tid() != Some(901) {
                state.yield_current().expect("switch");
            }
        });
        (kernel, 901, 902)
    }

    #[test]
    fn stage29_split_cnode_slots_ok_return_lanes() {
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(4);
        let mut frame = cnode_slots_frame(target, requested);

        let result = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame).legacy();
        assert_eq!(result, Some(Ok(())), "split seam must service NR 8");
        // Exact lanes the old global-lock handler produced: set_ok(slots, pid, 0).
        assert_eq!(frame.ret0(), requested, "ret0 == slots");
        assert_eq!(frame.ret1(), target as usize, "ret1 == target pid");
        assert_eq!(frame.ret2(), 0, "ret2 == 0");
        assert_eq!(frame.error_code(), None, "no error on success");

        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(after, Some(requested), "capability domain actually resized");
    }

    #[test]
    fn stage29_split_cnode_slots_missing_task_error() {
        // Requester TID with no registered task → TaskMissing. Exercised via the
        // helper the seam delegates to (the seam itself always reads a present
        // current TID; an absent requester must surface the same error).
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let syscall = decode(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR);
        let args = [target, 16, 0, 0, 0, 0];
        let result = try_split_dispatch(&kernel, syscall, 424_242, args);
        assert_eq!(result, Some(Err(KernelError::TaskMissing)));
    }

    #[test]
    fn stage29_split_cnode_slots_bad_requester_class_error() {
        // App requester (901) targeting a DIFFERENT pid (902) → MissingRight.
        let (kernel, _requester, target) = shared_with_app_requester();
        let mut frame = cnode_slots_frame(target, 16);
        let result = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame).legacy();
        assert_eq!(
            result,
            Some(Err(TrapHandleError::Syscall(SyscallError::from(
                KernelError::MissingRight
            )))),
            "App requester resizing another pid's cnode must be MissingRight"
        );
        // On error the seam must NOT write a success payload.
        assert_eq!(frame.ret0(), 0);
        assert_eq!(frame.ret1(), 0);
    }

    #[test]
    fn stage29_split_cnode_slots_missing_cnode_error() {
        // System-server requester targeting a pid with no registered cnode and no
        // pre-reserved cnode space: the create path must fail rather than fabricate
        // a success. We use a target pid that was never registered.
        let (kernel, _requester, _target) = shared_with_control_plane_requester();
        let unregistered_pid = 7_777u64;
        // Whatever the domain decides (create or reject), the seam must propagate
        // the SAME Result the split-mut helper returns — never silently OK with a
        // bogus frame payload. Compare seam vs direct helper.
        let mut frame = cnode_slots_frame(unregistered_pid, 16);
        let seam = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame).legacy();
        let direct =
            kernel.control_plane_set_process_cnode_slots_split_mut(900, unregistered_pid, 16);
        match (seam, direct) {
            (Some(Ok(())), Ok(())) => {
                // Create path succeeded: the frame must carry the canonical lanes.
                assert_eq!(frame.ret0(), 16);
                assert_eq!(frame.ret1(), unregistered_pid as usize);
            }
            (Some(Err(TrapHandleError::Syscall(s))), Err(k)) => {
                assert_eq!(
                    s,
                    SyscallError::from(k),
                    "seam error must equal helper error"
                );
                assert_eq!(
                    frame.error_code(),
                    None,
                    "seam never writes set_err for hard errors"
                );
            }
            (seam, direct) => panic!("seam/direct divergence: {seam:?} vs {direct:?}"),
        }
    }

    #[test]
    fn stage29_split_cnode_slots_duplicate_update_ok() {
        // Calling the seam twice with the same target must be idempotent-OK.
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(6);
        let mut f1 = cnode_slots_frame(target, requested);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut f1).legacy(),
            Some(Ok(()))
        );
        let mut f2 = cnode_slots_frame(target, requested);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut f2).legacy(),
            Some(Ok(()))
        );
        assert_eq!(f2.ret0(), requested);
        assert_eq!(f2.ret1(), target as usize);
        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(after, Some(requested));
    }

    #[test]
    fn stage29_split_cnode_slots_capacity_resize_ok() {
        // Distinct grow then a second grow: lanes track the latest request.
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let base = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("base");
        let grow1 = base.saturating_add(2);
        let mut f1 = cnode_slots_frame(target, grow1);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut f1).legacy(),
            Some(Ok(()))
        );
        assert_eq!(f1.ret0(), grow1);
        let grow2 = grow1.saturating_add(5);
        let mut f2 = cnode_slots_frame(target, grow2);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut f2).legacy(),
            Some(Ok(()))
        );
        assert_eq!(f2.ret0(), grow2);
        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(after, Some(grow2));
    }

    #[test]
    fn stage29_split_cnode_slots_error_code_preserved() {
        // The error code surfaced by the seam must equal the From<KernelError>
        // SyscallError code of the underlying domain error (MissingRight → 4).
        let (kernel, _requester, target) = shared_with_app_requester();
        let mut frame = cnode_slots_frame(target, 16);
        let Some(Err(TrapHandleError::Syscall(err))) =
            try_split_dispatch_into_frame(&kernel, CPU0, &mut frame).legacy()
        else {
            panic!("expected a Syscall error");
        };
        assert_eq!(err, SyscallError::from(KernelError::MissingRight));
        assert_eq!(err.code(), SyscallError::MissingRight.code());
    }

    #[test]
    fn stage29_split_cnode_slots_no_scheduler_side_effect() {
        // The split path must not switch tasks: current TID is unchanged across it.
        let (kernel, requester, target) = shared_with_control_plane_requester();
        let before_tid = kernel.current_tid_split_read(CPU0);
        assert_eq!(before_tid, Some(requester));
        let mut frame = cnode_slots_frame(target, 12);
        let _ = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame).legacy();
        let after_tid = kernel.current_tid_split_read(CPU0);
        assert_eq!(
            after_tid,
            Some(requester),
            "no task switch (task_switched==false)"
        );
    }

    #[test]
    fn stage29_split_cnode_slots_no_ipc_side_effect() {
        // The split path must not enqueue IPC: the target task stays runnable and
        // its status is not changed to any blocked endpoint state.
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let mut frame = cnode_slots_frame(target, 14);
        let _ = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame).legacy();
        let status = kernel.with(|state| state.task_status(target));
        assert!(
            !matches!(
                status,
                Some(crate::kernel::task::TaskStatus::Blocked(
                    crate::kernel::task::WaitReason::EndpointSend(_)
                        | crate::kernel::task::WaitReason::EndpointReceive(_)
                ))
            ),
            "split path must not block the target on any endpoint"
        );
    }

    // ---- Part 5: fallback safety ----

    #[test]
    fn stage29_only_nr8_is_split_eligible() {
        assert!(
            classify_split_eligible_nr_only(decode(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR))
                .is_some()
        );
    }

    #[test]
    fn stage29_ipc_send_not_eligible() {
        let (kernel, _r, _t) = shared_with_control_plane_requester();
        let mut frame = TrapFrame::new(SYSCALL_IPC_SEND_NR, [1, 2, 3, 4, 5, 6]);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut frame).legacy(),
            None,
            "IPC send must fall back to the global-lock path"
        );
        assert!(classify_split_eligible_nr_only(decode(SYSCALL_IPC_SEND_NR)).is_none());
    }

    #[test]
    /// U9-SPAWN-TXN3 §4 INVERTED this guard. NR 23 was excluded from the NR-only gate because
    /// its rollback went through the kernel's general revocation, whose sixteen-substep cascade
    /// spans five domains and cannot run off-lock. §1 proved eleven of those substeps unreachable
    /// for the capabilities a spawn creates — by object kind, not by timing — and §2 replaced the
    /// rollback with exactly the reachable closure. So NR 23 is admitted now.
    ///
    /// What remains true, and is what this guard checks instead, is the property the exclusion
    /// existed to protect: the route may still DECLINE, and every decline is pre-mutation and
    /// falls back unchanged. The hosted build has no off-lock user-read seam (it needs the direct
    /// map), so the route declines here and the fallback is exactly what it always was.
    fn stage29_spawnv5_is_eligible_but_still_declines_pre_mutation() {
        let (kernel, _r, _t) = shared_with_control_plane_requester();
        let mut frame = TrapFrame::new(SYSCALL_SPAWN_PROCESS_NR, [1, 2, 3, 4, 5, 6]);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut frame).legacy(),
            None,
            "a declined spawn must fall back unchanged"
        );
        assert!(
            classify_split_eligible_nr_only(decode(SYSCALL_SPAWN_PROCESS_NR)).is_some(),
            "NR 23 passes the NR-only gate since U9-SPAWN-TXN3 §4"
        );
        assert!(
            classify_split_eligible_nr_only(decode(
                crate::kernel::syscall::SYSCALL_SPAWN_FROM_MEMORY_OBJECT_NR
            ))
            .is_some(),
            "and so does NR 29, its sibling"
        );
    }

    #[test]
    fn stage29_vm_map_not_eligible() {
        let (kernel, _r, _t) = shared_with_control_plane_requester();
        let mut frame = TrapFrame::new(SYSCALL_VM_MAP_NR, [1, 2, 3, 4, 5, 6]);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut frame).legacy(),
            None
        );
        assert!(classify_split_eligible_nr_only(decode(SYSCALL_VM_MAP_NR)).is_none());
    }

    #[test]
    fn stage29_futex_not_eligible() {
        let (kernel, _r, _t) = shared_with_control_plane_requester();
        // FutexWait (NR 9) is genuinely never split-eligible — it BLOCKS the caller,
        // so it stays global-lock-only. (Stage 191B split-retired FutexWake (NR 10),
        // which does NOT block the caller; that eligibility is pinned separately.)
        let mut frame = TrapFrame::new(
            crate::kernel::syscall::SYSCALL_FUTEX_WAIT_NR,
            [1, 2, 3, 4, 5, 6],
        );
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut frame).legacy(),
            None
        );
        assert!(
            classify_split_eligible_nr_only(decode(crate::kernel::syscall::SYSCALL_FUTEX_WAIT_NR))
                .is_none(),
            "FutexWait must stay global-lock-only (it blocks the caller)"
        );
        // Stage 191B: FutexWake IS now split-eligible.
        assert!(
            classify_split_eligible_nr_only(decode(crate::kernel::syscall::SYSCALL_FUTEX_WAKE_NR))
                .is_some(),
            "FutexWake must be split-eligible (Stage 191B)"
        );
    }

    /// Stage 195C guard: pin the exact FutexWake / FutexWait / SpawnThread NR identities so a
    /// future edit cannot silently reintroduce the "FutexWake is NR 11" confusion (NR 11 is
    /// SpawnThread; FutexWake is NR 10; FutexWait is NR 9). Only FutexWake (NR 10) is
    /// split-eligible; FutexWait (NR 9) and SpawnThread (NR 11) stay global-lock-only.
    #[test]
    fn stage195c_futex_wake_nr10_split_eligible_wait_and_spawn_thread_excluded() {
        use crate::kernel::syscall::{
            SYSCALL_FUTEX_WAIT_NR, SYSCALL_FUTEX_WAKE_NR, SYSCALL_SPAWN_THREAD_NR,
        };
        // The real syscall numbers — the Stage 195C task text's "NR11" for FutexWake is wrong.
        assert_eq!(SYSCALL_FUTEX_WAIT_NR, 9, "FutexWait is NR 9");
        assert_eq!(SYSCALL_FUTEX_WAKE_NR, 10, "FutexWake is NR 10 (NOT 11)");
        assert_eq!(
            SYSCALL_SPAWN_THREAD_NR, 11,
            "NR 11 is SpawnThread, NOT FutexWake"
        );
        assert!(
            matches!(decode(SYSCALL_FUTEX_WAKE_NR), Syscall::FutexWake),
            "NR 10 must decode to FutexWake"
        );
        // U9-SPAWN1 SP-2: this guard's subject is the NR-IDENTITY confusion above — the Stage
        // 195C task text called FutexWake "NR11", and NR 11 is SpawnThread. That pinning is
        // unchanged. What changed is the eligibility line: NR 11 is now admitted in its own
        // right, for a reason that has nothing to do with NR 10's, so the guard asserts the
        // DISTINCTION rather than a shared exclusion.
        //
        // NR 10 (FutexWake) and NR 11 (SpawnThread) are both non-switching and both admitted.
        // NR 9 (FutexWait) BLOCKS the caller and is still excluded from the non-switching gate.
        assert!(classify_split_eligible_nr_only(decode(SYSCALL_FUTEX_WAKE_NR)).is_some());
        assert!(classify_split_eligible_nr_only(decode(SYSCALL_SPAWN_THREAD_NR)).is_some());
        assert!(
            classify_split_eligible_nr_only(decode(SYSCALL_FUTEX_WAIT_NR)).is_none(),
            "FutexWait blocks the caller and must stay off the non-switching gate"
        );
    }

    #[test]
    fn stage29_syscall_count_still_30() {
        assert_eq!(SYSCALL_COUNT, 32, "Stage 29 must not change SYSCALL_COUNT");
    }

    #[test]
    fn stage29_whitelist_exhaustive() {
        // Iterate the full NR space; only NR 8 (cnode-slots), NR 2 (IpcRecv,
        // Stage 32B), NR 14 (VmBrk, Stage 114), NR 15 (DebugLog, Stage 191A),
        // NR 10 (FutexWake, Stage 191B), NR 28 (CreateInitramfsFileSliceMo, U9-MO2 §4)
        // NR 11 (SpawnThread, U9-SPAWN1 SP-2), NR 23 + NR 29 (SpawnProcess and
        // SpawnFromMemoryObject, U9-SPAWN-TXN3 §4), NR 12 (Fork, U9-FORK1 §4) and NR 31
        // (ReapFaultedTask, U9-REAP1 §4) may pass the NR-only split-eligibility gate.
        // (Stage 197A removed NR 27 InitramfsReadChunk from the whitelist and the ABI.)
        // Every other syscall stays global-lock-only. This is an EXHAUSTIVE sweep of the
        // whole NR space, so it is the guard that would catch a sixth admission arriving
        // without its own justification — it is widened by exactly one NR, never relaxed.
        for nr in 0..SYSCALL_COUNT {
            let Ok(syscall) = Syscall::decode(nr) else {
                continue;
            };
            let eligible = classify_split_eligible_nr_only(syscall).is_some();
            if nr == SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR
                || nr == SYSCALL_IPC_RECV_NR
                || nr == crate::kernel::syscall::SYSCALL_VM_BRK_NR
                || nr == crate::kernel::syscall::SYSCALL_DEBUG_LOG_NR
                || nr == crate::kernel::syscall::SYSCALL_FUTEX_WAKE_NR
                || nr == crate::kernel::syscall::SYSCALL_CREATE_INITRAMFS_FILE_SLICE_MO_NR
                || nr == crate::kernel::syscall::SYSCALL_SPAWN_THREAD_NR
                || nr == SYSCALL_SPAWN_PROCESS_NR
                || nr == crate::kernel::syscall::SYSCALL_SPAWN_FROM_MEMORY_OBJECT_NR
                || nr == crate::kernel::syscall::SYSCALL_FORK_NR
                || nr == crate::kernel::syscall::SYSCALL_REAP_FAULTED_TASK_NR
            {
                assert!(eligible, "NR {nr} must be split-eligible");
            } else {
                assert!(!eligible, "NR {nr} must NOT be split-eligible");
            }
        }
    }

    // ---- Stage 188H: pre-189 readiness guard ----

    #[test]
    fn stage188h_reap_faulted_task_excluded_from_split_dispatch() {
        // U9-REAP1 §4 RETIRED this exclusion, and this guard now pins its inverse.
        //
        // Stage 188H pinned NR 31 as global-lock-only so a later stage could not "silently
        // whitelist it while wiring the AP/multi-dispatcher path". That protection did its job:
        // NR 31 is admitted here deliberately, by a mission whose §1 recomputed the reap closure,
        // whose §2 gave the reap a linearizable claim and whose §3 put both routes on ONE
        // transaction. What the guard must now hold is the positive fact — the classifier admits
        // it, and it admits it through the same NR-only path every other admitted class uses.
        let syscall = decode(crate::kernel::syscall::SYSCALL_REAP_FAULTED_TASK_NR);
        assert!(
            matches!(syscall, Syscall::ReapFaultedTask),
            "NR 31 must decode to ReapFaultedTask"
        );
        assert!(
            classify_split_eligible_nr_only(syscall).is_some(),
            "U9-REAP1 §4: ReapFaultedTask must be NR-only split-eligible"
        );
        // The route is reached by NUMBER, never by an argument-shaped guess: NR 31's only input
        // is a target TID, which names nothing the classifier could validate without the rank-2
        // claim, so admitting it arg-aware would be admitting it on an unchecked number.
        let args = [3u64, 0, 0, 0, 0, 0];
        assert_eq!(
            classify_split_eligible(syscall, 3, args),
            None,
            "ReapFaultedTask is admitted by NR, not by an arg-aware classification"
        );
        // And the transaction it reaches is the SAME one the broad handler reaches.
        const SRC: &str = include_str!("syscall_split.rs");
        const RESTART: &str = include_str!("boot/restart_state.rs");
        assert!(
            SRC.contains("run_reap_transaction(&mut owners, target)")
                && RESTART.contains("run_reap_transaction(&mut owners, tid)"),
            "both NR 31 routes must drive the one reap transaction"
        );
    }

    // ---- Part 6: result-writeback equivalence ----

    #[test]
    fn stage29_split_result_ok_encodes_same_as_old_path() {
        // The seam's success lanes must equal what the old global-lock handler
        // produced: set_ok(slot_capacity, target_pid, 0).
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(3);
        let mut seam_frame = cnode_slots_frame(target, requested);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut seam_frame).legacy(),
            Some(Ok(()))
        );

        // Reference encoding the old path used.
        let mut ref_frame = cnode_slots_frame(target, requested);
        ref_frame.set_ok(requested, target as usize, 0);

        assert_eq!(seam_frame.ret0(), ref_frame.ret0());
        assert_eq!(seam_frame.ret1(), ref_frame.ret1());
        assert_eq!(seam_frame.ret2(), ref_frame.ret2());
        assert_eq!(seam_frame.error_code(), ref_frame.error_code());
    }

    #[test]
    fn stage29_split_result_err_encodes_same_as_old_path() {
        // On a domain error the seam returns TrapHandleError::Syscall(e) — exactly
        // what the old handler's `Err(SyscallError)` became at the trap boundary —
        // and leaves the frame return lanes untouched (no set_ok), matching the old
        // path which never wrote set_ok on error.
        let (kernel, _requester, target) = shared_with_app_requester();
        let mut frame = cnode_slots_frame(target, 16);
        let result = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame).legacy();
        assert_eq!(
            result,
            Some(Err(TrapHandleError::Syscall(SyscallError::from(
                KernelError::MissingRight
            ))))
        );
        assert_eq!(frame.ret0(), 0, "no success payload on error");
        assert_eq!(frame.ret1(), 0, "no success payload on error");
    }

    #[test]
    fn stage29_split_result_no_task_switch() {
        // entering_tid == exiting_tid across the seam ⇒ task_switched == false,
        // which the arch path requires to take the write_trap_returns branch.
        let (kernel, requester, target) = shared_with_control_plane_requester();
        let entering = kernel.current_tid_split_read(CPU0);
        let mut frame = cnode_slots_frame(target, 10);
        let _ = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame).legacy();
        let exiting = kernel.current_tid_split_read(CPU0);
        assert_eq!(entering, exiting);
        assert_eq!(exiting, Some(requester));
    }

    #[test]
    fn stage29_split_dispatch_fallback_path_unchanged() {
        // A None return from the seam means the global-lock handler still runs.
        // Prove the global-lock dispatch produces the canonical result for the
        // same NR-8 frame the seam would have serviced — i.e. the fallback path is
        // intact and value-equivalent.
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        // A NON-whitelisted syscall returns None from the seam.
        let mut send_frame = TrapFrame::new(SYSCALL_IPC_SEND_NR, [1, 2, 3, 4, 5, 6]);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut send_frame).legacy(),
            None,
            "non-whitelisted syscall must fall back (None)"
        );
        // And the global-lock handler can still service NR 8 directly.
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(7);
        let mut nr8 = cnode_slots_frame(target, requested);
        kernel
            .with(|state| crate::kernel::syscall::dispatch(state, &mut nr8))
            .expect("global-lock dispatch");
        assert_eq!(nr8.ret0(), requested);
        assert_eq!(nr8.ret1(), target as usize);
    }

    // ---- Stage 32B: IpcRecv classification ----

    #[test]
    fn stage32b_ipc_recv_classify_nr2_eligible() {
        // NR 2 (IpcRecv) now passes the NR-only split-eligibility gate.
        assert!(
            classify_split_eligible_nr_only(decode(SYSCALL_IPC_RECV_NR)).is_some(),
            "IpcRecv (NR 2) must be split-eligible at the NR gate"
        );
        // And the arg-level classifier maps it to the IpcRecvKernelTask variant.
        assert_eq!(
            classify_split_eligible(decode(SYSCALL_IPC_RECV_NR), 1, [0; 6]),
            Some(SplitEligibleSyscall::IpcRecvKernelTask)
        );
    }

    #[test]
    fn stage32b_ipc_recv_timeout_nr_not_in_whitelist() {
        // IpcRecvTimeout (NR 5) must NOT be split-eligible: it stays on the
        // global-lock path (scheduler/deadline interaction).
        assert!(
            classify_split_eligible_nr_only(decode(
                crate::kernel::syscall::SYSCALL_IPC_RECV_TIMEOUT_NR
            ))
            .is_none(),
            "IpcRecvTimeout must NOT be split-eligible"
        );
        assert_eq!(
            classify_split_eligible(
                decode(crate::kernel::syscall::SYSCALL_IPC_RECV_TIMEOUT_NR),
                1,
                [0; 6]
            ),
            None,
            "IpcRecvTimeout must fall back"
        );
    }

    #[test]
    fn stage32b_ipc_send_call_reply_not_split_eligible() {
        // The sender-side IPC syscalls stay default-deny.
        for nr in [
            SYSCALL_IPC_SEND_NR,
            crate::kernel::syscall::SYSCALL_IPC_CALL_NR,
            crate::kernel::syscall::SYSCALL_IPC_REPLY_NR,
        ] {
            assert!(
                classify_split_eligible_nr_only(decode(nr)).is_none(),
                "NR {nr} must NOT be split-eligible"
            );
        }
    }

    #[test]
    fn stage32b_arg_only_dispatch_defers_ipc_recv() {
        // The arg-only try_split_dispatch must NEVER service IpcRecv: it returns
        // None so the frame-level seam (and ultimately the global lock) handles it.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        assert_eq!(
            try_split_dispatch(&kernel, decode(SYSCALL_IPC_RECV_NR), 1, [1, 0, 0, 0, 0, 0]),
            None,
            "arg-only dispatch must defer IpcRecv"
        );
    }

    #[test]
    fn stage32b_syscall_count_30() {
        assert_eq!(
            SYSCALL_COUNT, 32,
            "Stage 42+43 adds RecvSharedV3 (NR 30); stage32b invariant updated"
        );
    }
}
