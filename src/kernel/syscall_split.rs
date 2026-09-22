// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 28: trap/syscall split-dispatch bridge (whitelist-only scaffold).
//! Stage 29: live-wired for `ControlPlaneSetCnodeSlots` (NR 8) via
//! [`try_split_dispatch_into_frame`].
//! Stage 32B: live-wired for `IpcRecv` (NR 2), kernel-task queued-plain case only.
//! Stage 114: live-wired for `VmBrk` (NR 14), page-crossing-shrink case only.
//! U9-VM-ENTRY1: NR 3, NR 13 and NR 14 are live-wired WHOLE, through the one mapping/brk
//! transaction, via [`try_split_vm_map_into_frame`], [`try_split_vm_anon_map_into_frame`] and
//! [`try_split_vm_brk_into_frame`]. Stage 114's shrink-only adapter is gone.
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
use crate::kernel::syscall::{RecvImmediateOutcome, Syscall, SyscallError};
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
    // Stage 114: `Syscall::VmBrk` (NR 14) is intentionally NOT added here, and U9-VM-ENTRY1
    // keeps it that way. Stage 114 could not decide NR 14's eligibility from the syscall number
    // and raw args alone (group-leader status, brk bounds, page-crossing and the online-CPU
    // count all needed domain reads). U9-VM-ENTRY1 removed that question rather than answering
    // it: the route is TOTAL, so there is nothing left to classify. It is special-cased directly
    // in `try_split_dispatch_into_frame` — as NR 3 and NR 13 are — and routed straight to
    // `try_split_vm_brk_into_frame`, never through `classify_split_eligible`.
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
/// U9-PAGEFAULT1 §3 — the ONE classification entry every split PageFault route uses.
///
/// It exists for a reason the raw seam call could not serve: the classifier can answer "this
/// fault cannot be attributed to a running user incarnation", and that answer has three distinct
/// causes which the old `Ok((_, Some(facts)))` pattern discarded along with the class. A
/// supervisor-mode fault, a CPU with no current task, and a current task with no address space
/// are three different bugs, and a route that declined all of them identically told an observer
/// nothing about which one occurred.
///
/// Every cause still DECLINES — none of them may reach a recovery owner, and none of them can
/// produce a fault report, because there is no victim to name. What changes is that the decline
/// is now recorded with its cause, which is what makes the population countable. It is expected
/// to be empty: a supervisor page fault is a kernel bug, and the other two are dispatch-state
/// bugs. A marker that never prints is the evidence that it is empty.
///
/// `None` also covers the seam's own typed refusal (`SharedPageFaultRefusal`), which is a stale
/// identity discovered between the ranked reads — an ordinary race, not a kernel state, and
/// already named by the seam.
#[cfg(not(feature = "hosted-dev"))]
fn classify_for_split(
    shared: &SharedKernel,
    cpu: CpuId,
    fault: crate::kernel::trap::FaultInfo,
    route: &'static str,
) -> Option<(
    crate::kernel::boot::PageFaultClass,
    crate::kernel::boot::PageFaultFacts,
)> {
    match shared.classify_page_fault_shared(cpu, fault) {
        Ok((class, Some(facts))) => Some((class, facts)),
        Ok((crate::kernel::boot::PageFaultClass::KernelOrAbsentTask(cause), None)) => {
            crate::yarm_log!(
                "PF1_UNATTRIBUTABLE_FAULT cpu={} route={} cause={} addr=0x{:x} access={:?} settled=0",
                cpu.0,
                route,
                cause.marker(),
                fault.addr.0,
                fault.access
            );
            None
        }
        // No other class can arrive without facts: every one of them is derived FROM facts.
        Ok((_, None)) | Err(_) => None,
    }
}

#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
/// U9-FT4 — the pre-lock AArch64 terminal PageFault route.
///
/// Admits an AArch64 user PageFault below `KERNEL_SPACE_BASE` classified `TerminallyUnhandled`,
/// and — since U9-PAGEFAULT1 §1e — settles its outcomes rather than only its eligible one.
///
/// ORDERING IS MODELLED ON FutexWait: admit -> reserve the deferral -> publish -> transition ->
/// drain. The queue advance is NOT performed here; the EXISTING post-lock drain consumes the
/// deferral once and applies the exact incoming context. `QueueAdvanceCommitted` is returned
/// ONLY with a reserved deferral, which is what makes the incoming apply structurally guaranteed
/// — returning it without one was the FT3 defect that resumed the faulting PC.
///
/// ## The outcomes it settles, and the ones it still declines
///
/// The route used to require BOTH a terminating policy AND a buffered-eligible report, and hand
/// every other combination to the broad dispatcher. That conflated two independent decisions
/// which the broad arm takes in the opposite order — it reports first and consults policy second
/// — so most of this class's outcomes were never this route's to begin with. They are now:
///
/// | report admission | terminating policy | settlement |
/// |---|---|---|
/// | eligible | yes | publish, transition, `QueueAdvanceCommitted` |
/// | eligible | no | publish, no transition, `Complete(Ok(()))` |
/// | no route / endpoint stale | yes | the emitter's own marker, transition, `QueueAdvanceCommitted` |
/// | no route / endpoint stale | no | the emitter's own marker, `Complete(Ok(()))` |
/// | waiter present / buffer full | either | `NotHandled` — see below |
///
/// Two report endings are genuinely not this route's, and for the same kind of reason — the broad
/// emitter reaches them through a different MECHANISM, not merely a different outcome:
///
/// * `WaiterPresent` — the report is delivered DIRECTLY into the blocked receiver and the
///   receiver is woken. That is a writeback into another task's buffers plus a rank-1 scheduler
///   transition, with its own failure ladder: receive-family work, not fault-family work.
/// * `BufferFull` — the emitter does not predict a full queue, it discovers one inside the
///   enqueue, having already printed the target, the queue state, the sender line and the
///   enqueue-begin line. Reproducing that prefix from a preflight prediction would report an
///   enqueue attempt this route never made.
///
/// Both decline having touched nothing.
///
/// Four refusals remain, and all four are pre-mutation: the policy read, the queue-advance
/// admission, the deferral reservation and a commit that loses its revalidation. Each leaves the
/// fault exactly as the broad arm would find it.
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

    // U9-PAGEFAULT1 §2 — all three ports, each on a witness of its own.
    //
    // The architecture is a derived string rather than a `cfg!` test, exactly as the COW and
    // demand routes derive theirs, so the routing matrix row is the ONLY thing that admits a
    // port. Opening the gate is not what admitted x86_64 and RISC-V: their rows were added after
    // the terminal-fault oracle produced a fault on each and the baseline was measured going
    // broad.
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "riscv64") {
        "riscv64"
    } else {
        return D::NotHandled;
    };
    let (Some(fault), Some(frame)) = (fault, frame) else {
        return D::NotHandled;
    };
    let cpu_idx = cpu.0 as usize;
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return D::NotHandled;
    }
    // (1) Classify off-lock. A stale identity refuses before anything is decided.
    let Some((class, facts)) = classify_for_split(shared, cpu, fault, "terminal") else {
        return D::NotHandled;
    };
    if !matches!(
        page_fault_route_for(arch, class),
        PageFaultRoute::SplitTerminal
    ) {
        return D::NotHandled;
    }
    // (2) Terminal policy, against the EXACT coordinate we classified with.
    //
    // U9-PAGEFAULT1 §1e — `NotifyAndContinue` is no longer a decline. The broad arm's order is
    // REPORT FIRST, policy second: `fault_current_task_with_fault` emits the report and only then
    // asks `effective_fault_policy_for`, returning `Ok(())` for a non-terminating policy. So a
    // notify-and-continue fault is a published report with no transition and no deferral, and
    // this route can produce exactly that. The policy still decides everything below, it just no
    // longer decides whether this route participates.
    let Ok(snapshot) = shared.read_terminal_fault_policy_shared(cpu, facts.tid, facts.asid) else {
        crate::yarm_log!(
            "TERMINAL_FAULT_SPLIT_REFUSED cpu={} tid={} phase=policy reason=policy_read",
            cpu.0,
            facts.tid
        );
        return D::NotHandled;
    };
    let terminates = snapshot.terminates_task();
    // (3) Report admission. U9-PAGEFAULT1 §1e — four of its five endings settle here now.
    //
    // `NoRoute` and `EndpointStale` publish NOTHING in the broad arm either: each is exactly ONE
    // ungated marker followed by a bare `return` from `emit_fault_report_for_fault`, with the
    // policy branch below then running exactly as it does for a published report. The preflight
    // determines both markers completely, so this route reproduces them rather than handing the
    // whole fault away over a report that was never going to exist.
    //
    // The other two still decline, for reasons that are about MECHANISM, not about outcome:
    //
    // * `WaiterPresent` — the broad emitter hands the report DIRECTLY to a blocked receiver, a
    //   writeback into another task's buffers plus a rank-1 wake, with its own failure ladder.
    //   That is a different publication mechanism, and reproducing it is receive-family work.
    // * `BufferFull` — the broad emitter does not discover a full queue at a preflight; it
    //   discovers it INSIDE the enqueue, having already printed the target, the queue state, the
    //   sender line and `TASK_FAULT_REPORT_ENQUEUE_BEGIN` against data a preflight does not
    //   carry. Reproducing that prefix from a prediction would be reporting an enqueue attempt
    //   this route never made.
    let admission = shared.admit_buffered_fault_report_shared(&snapshot);
    let publishable = match admission {
        A::BufferedEligible {
            endpoint_idx,
            generation,
            queued_before,
            via_fault_handler,
        } => Some((endpoint_idx, generation, queued_before, via_fault_handler)),
        A::WaiterPresent { .. } | A::BufferFull { .. } => {
            crate::yarm_log!(
                "TERMINAL_FAULT_SPLIT_REFUSED cpu={} tid={} phase=admit reason={}",
                cpu.0,
                facts.tid,
                match admission {
                    A::WaiterPresent { .. } => "waiter_present",
                    _ => "buffer_full",
                }
            );
            return D::NotHandled;
        }
        A::EndpointStale { .. } | A::NoRoute => None,
    };
    // (4) For a TERMINATING policy only: the capability check and the deferral reservation, both
    // before any marker is printed and before anything is published. A non-terminating policy
    // performs no transition, so it owes no queue advance and reserves nothing — reserving one
    // would strand the CPU on a deferral no drain would ever consume.
    if terminates {
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
        // RESERVE THE DEFERRAL BEFORE ANY PUBLICATION. A reservation failure is pre-mutation and
        // may fall back; holding it is what guarantees the drain will apply an incoming context.
        if !crate::kernel::boot::futex_wait_dispatch_try_defer(cpu_idx, facts.tid) {
            crate::yarm_log!(
                "TERMINAL_FAULT_SPLIT_REFUSED cpu={} tid={} phase=defer reason=defer_unavailable",
                cpu.0,
                facts.tid
            );
            return D::NotHandled;
        }
    }
    // (5) Capture the outgoing context while the reservation is held and nothing is published.
    // Only a terminating fault has an outgoing context: the other task keeps running in its own.
    let captured = if terminates {
        shared.capture_outgoing_user_context_split(facts.tid, frame)
    } else {
        false
    };
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
    // (6) PUBLISH, or reproduce the broad emitter's own no-publication ending.
    if let Some((endpoint_idx, generation, queued_before, via_fault_handler)) = publishable {
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
        // Past this line broad fallback is forbidden.
        match shared.commit_buffered_fault_report_shared(
            facts.tid,
            fault,
            endpoint_idx,
            generation,
            via_fault_handler,
        ) {
            C::Buffered { .. } => {}
            // Pre-publication refusal: release any reservation and let the broad path run.
            _ => {
                if terminates {
                    crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
                }
                return D::NotHandled;
            }
        }
    } else {
        // U9-PAGEFAULT1 §1e — the broad emitter's two no-publication endings, in its own
        // spellings. Each is a bare `return` from `emit_fault_report_for_fault` after one marker,
        // and the policy branch below then runs exactly as it does for a published report.
        //
        // Neither prints a target or a queue-state line, because the emitter returns before both
        // of those. That position is the whole reason these two can be reproduced from a
        // preflight while the full-queue ending cannot.
        match admission {
            A::NoRoute => {
                crate::yarm_log!(
                    "TASK_FAULT_NO_SUPERVISOR_ROUTE tid={} reason=no-fault-or-supervisor-endpoint",
                    facts.tid
                );
            }
            A::EndpointStale { endpoint_idx } => {
                crate::yarm_log!(
                    "TASK_FAULT_REPORT_ENQUEUE_FAIL tid={} endpoint={} reason=missing-endpoint",
                    facts.tid,
                    endpoint_idx
                );
            }
            // Handled above: eligible publishes, the other two declined before this point.
            A::BufferedEligible { .. } | A::WaiterPresent { .. } | A::BufferFull { .. } => {}
        }
        crate::yarm_log!(
            "TERMINAL_FAULT_SPLIT_UNPUBLISHED cpu={} tid={} reason={} terminates={} broad_lock=0",
            cpu.0,
            facts.tid,
            match admission {
                A::NoRoute => "no_route",
                A::EndpointStale { .. } => "endpoint_stale",
                _ => "unreachable",
            },
            u8::from(terminates)
        );
    }
    // (7) U9-PAGEFAULT1 §1e — the non-terminating policy settles here, with no transition.
    //
    // This is the broad arm's `if effective_fault_policy_for(..) == NotifyAndContinue { return
    // Ok(()) }`, reached after the same report step. The faulting task is still `Running`, still
    // `current`, and resumes at the same instruction — which for a terminal fault means it will
    // fault again. That re-fault is the EXISTING production behaviour of this policy; the owner
    // changed and the policy did not.
    if !terminates {
        crate::yarm_log!(
            "TERMINAL_FAULT_SPLIT_NOTIFY_CONTINUE cpu={} tid={} published={} broad_lock=0",
            cpu.0,
            facts.tid,
            u8::from(publishable.is_some())
        );
        return D::Complete(Ok(()));
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

/// U9-PAGEFAULT1 §1c — settle the two COW outcomes that are not the private copy.
///
/// Split out of the route body rather than inlined so the route keeps ONE statement per arm and
/// the two settlements are readable against `try_handle_cow_fault`'s corresponding returns.
///
/// Neither outcome can fall back after a mutation, because neither performs one before it is
/// decided: `MarkCleared` IS the mutation and it is the last step, and `NoMapping` decides before
/// anything is written. `Raced` mutated nothing at all and continues down the route order — which
/// is what the broad arm's `Ok(false)` does too, so the fault still reaches demand and terminal
/// in their canonical order.
#[cfg(not(feature = "hosted-dev"))]
fn settle_cow_non_private_copy(
    shared: &SharedKernel,
    cpu: CpuId,
    fault: crate::kernel::trap::FaultInfo,
    facts: crate::kernel::boot::PageFaultFacts,
) -> SplitDispatchDisposition {
    use crate::kernel::boot::CowNonPrivateSettlement as S;
    use SplitDispatchDisposition as D;

    let settlement = shared.cow_settle_non_private_copy_split(facts);
    if matches!(settlement, S::Raced) {
        // Nothing was read into the marker stream and nothing was written. The next owner in the
        // route order prints `PAGE_FAULT_ENTRY` when it takes the fault, so it still appears
        // exactly once.
        crate::yarm_log!(
            "VM_COW_SPLIT_NONPRIVATE cpu={} tid={} va=0x{:x} outcome=raced settled=0",
            cpu.0,
            facts.tid,
            facts.page.0
        );
        return D::NotHandled;
    }
    // From here the fault IS settled by this route, so it owes the broad arm's entry marker in
    // the broad arm's position.
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
    match settlement {
        S::MarkCleared { phys } => {
            if crate::kernel::boot::vm_cow_enabled() {
                // `writable=1` is what the broad arm prints for this arm, and it is the fact
                // that selected it.
                crate::yarm_log!(
                    "VM_COW_PHASE_METADATA asid={} va=0x{:x} writable=1",
                    facts.asid.0,
                    facts.page.0
                );
                crate::yarm_log!(
                    "VM_COW_DONE asid={} va=0x{:x} path=already_writable",
                    facts.asid.0,
                    facts.page.0
                );
            }
            crate::yarm_log!(
                "VM_COW_SPLIT_NONPRIVATE cpu={} tid={} va=0x{:x} pa=0x{:x} outcome={} settled=1 broad_lock=0",
                cpu.0,
                facts.tid,
                facts.page.0,
                phys.0,
                settlement.reason()
            );
            crate::yarm_log!("PAGE_FAULT_HANDLED_COW");
            if crate::kernel::boot::fault_delivery_enabled() {
                crate::yarm_log!("FAULT_DELIVERY_CLASSIFY_HANDLED kind=cow");
            }
            D::Complete(Ok(()))
        }
        // The broad arm's `.ok_or(KernelError::UserMemoryFault)?`: the COW handler exits by `?`,
        // so the demand attempt below it never runs and `PAGE_FAULT_UNHANDLED` is never printed.
        // Settling it as that error rather than as a fallback keeps both of those properties.
        S::NoMapping => {
            crate::yarm_log!(
                "VM_COW_SPLIT_NONPRIVATE cpu={} tid={} va=0x{:x} outcome={} settled=1 broad_lock=0",
                cpu.0,
                facts.tid,
                facts.page.0,
                settlement.reason()
            );
            D::Complete(Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::from(
                    crate::kernel::boot::KernelError::UserMemoryFault,
                ),
            )))
        }
        // Handled above, before the entry marker.
        S::Raced => D::NotHandled,
    }
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

    // U9-A64-COW2 §4 admitted x86_64 and AArch64 and left RISC-V out, because RISC-V had no COW
    // witness of its own and §3 admits a class only on one.
    //
    // U9-PAGEFAULT1 §2: the architecture is derived here, as it is in the other two routes, so
    // the routing matrix row remains the ONLY thing that admits a port. Deriving a third name
    // does not admit RISC-V — its row does, and its row rests on a measured baseline.
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "riscv64") {
        "riscv64"
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
    let Some((class, facts)) = classify_for_split(shared, cpu, fault, "cow") else {
        return D::NotHandled;
    };
    if !matches!(page_fault_route_for(arch, class), PageFaultRoute::SplitCow) {
        return D::NotHandled;
    }
    // (2) U9-PAGEFAULT1 §1c — the two NON-private-copy arms, which used to be one decline.
    //
    // They are not the same outcome and neither is `PAGE_FAULT_UNHANDLED`: an already-writable
    // COW page is a bare mark clear that reaches `PAGE_FAULT_HANDLED_COW`, and a COW-marked page
    // with no mapping is the broad arm's `UserMemoryFault`, which leaves the COW handler through
    // `?` without ever trying demand. Both settle here, through the same owners.
    if facts.mapping_writable || !facts.mapping_present {
        return settle_cow_non_private_copy(shared, cpu, fault, facts);
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

/// U9-PAGEFAULT1 §1d — settle the demand outcome that is not a fresh mapping.
///
/// The repair allocates nothing, so there is no fail-closed half and no rollback: it either
/// retires the stale translation against a mapping that still satisfies the access, or it finds
/// the facts changed and declines having written nothing.
#[cfg(not(feature = "hosted-dev"))]
fn settle_demand_stale_translation(
    shared: &SharedKernel,
    cpu: CpuId,
    fault: crate::kernel::trap::FaultInfo,
    facts: crate::kernel::boot::PageFaultFacts,
) -> SplitDispatchDisposition {
    use crate::kernel::boot::DemandStaleTranslation as S;
    use SplitDispatchDisposition as D;

    let settlement = shared.demand_settle_stale_translation_split(facts);
    match settlement {
        S::Repaired { write } => {
            crate::yarm_log!(
                "PAGE_FAULT_ENTRY tid={} addr=0x{:x} access={:?} rip=0x{:x}",
                facts.tid,
                fault.addr.0,
                fault.access,
                0usize
            );
            crate::yarm_log!(
                "PF1_DEMAND_SPLIT_STALE_REPAIRED cpu={} tid={} asid={} va=0x{:x} write={} reason={} broad_lock=0",
                cpu.0,
                facts.tid,
                facts.asid.0,
                facts.page.0,
                u8::from(write),
                settlement.reason()
            );
            // The same completion marker the broad arm prints once its own repair returns
            // `Ok(true)` and the post-demand verification passes.
            crate::yarm_log!("PAGE_FAULT_HANDLED_DEMAND");
            D::Complete(Ok(()))
        }
        // Nothing read into the marker stream, nothing written: the next owner in the route
        // order prints `PAGE_FAULT_ENTRY` when it takes the fault.
        S::Raced => {
            crate::yarm_log!(
                "PF1_DEMAND_SPLIT_STALE_REFUSED cpu={} tid={} va=0x{:x} reason={}",
                cpu.0,
                facts.tid,
                facts.page.0,
                settlement.reason()
            );
            D::NotHandled
        }
    }
}

/// U9-PAGEFAULT1 §2c — the pre-lock DEMAND PageFault route.
///
/// Admits the class §3 witnessed on all three ports: a user fault inside a demand-backed region
/// with no mapping present, which `page_fault_route_for` now routes `SplitDemand`.
///
/// It refuses BEFORE any mutation for every condition it does not admit, so a declined trap
/// reaches the unchanged broad arm having changed nothing — and it is fail-closed after its first
/// allocation, because the broad path would otherwise allocate a SECOND frame for a fault it
/// never saw declined.
///
/// A recovered demand fault changes no scheduler state and publishes no deferral: the faulting
/// task is still `Running` and still `current`, and it resumes at the same instruction. So this
/// route returns `Complete`, exactly as the COW route does, never a queue advance.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_demand_page_fault_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    fault: Option<crate::kernel::trap::FaultInfo>,
) -> SplitDispatchDisposition {
    use crate::kernel::boot::{DemandRecovery as R, PageFaultRoute, page_fault_route_for};
    use crate::kernel::trap::FaultAccess;
    use SplitDispatchDisposition as D;

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "riscv64") {
        "riscv64"
    } else {
        return D::NotHandled;
    };
    let Some(fault) = fault else {
        return D::NotHandled;
    };
    // The broad demand handler refuses an instruction fetch outright — a demand page is mapped
    // `USER_RW`, never executable — and this route keeps that screen in the same place, so the
    // two cannot disagree about which accesses the class covers.
    if matches!(fault.access, FaultAccess::Execute) {
        return D::NotHandled;
    }
    // (1) Classify off-lock, through the ONE evaluator the broad arm uses. A supervisor origin
    // or a stale identity refuses here, before anything is decided. A kernel ADDRESS no longer
    // refuses here — U9-PAGEFAULT1 §3 gives it a class of its own, which this route declines
    // below on the ordinary class test, because it is a user fault and not a demand candidate.
    let Some((class, facts)) = classify_for_split(shared, cpu, fault, "demand") else {
        return D::NotHandled;
    };
    if !matches!(
        page_fault_route_for(arch, class),
        PageFaultRoute::SplitDemand
    ) {
        return D::NotHandled;
    }
    // (2) U9-PAGEFAULT1 §1d — the present-mapping arm, which used to be a decline.
    //
    // It is the broad handler's stale-translation repair, not a second allocation path: the
    // software mapping already satisfies the access (the class evaluator admits a present mapping
    // only when it does), so the repair retires the cached walk and the instruction retries.
    if facts.mapping_present {
        return settle_demand_stale_translation(shared, cpu, fault, facts);
    }
    // The marker the broad arm prints on entry, in the broad arm's position: this route
    // intercepts BEFORE the arm that would have printed it, and the stream must stay faithful.
    crate::yarm_log!(
        "PAGE_FAULT_ENTRY tid={} addr=0x{:x} access={:?} rip=0x{:x}",
        facts.tid,
        fault.addr.0,
        fault.access,
        0usize
    );

    // (3) THE TRANSACTION.
    let outcome = shared.demand_recover_page_split(facts);
    match outcome {
        R::Committed { phys } => {
            crate::yarm_log!(
                "PF1_DEMAND_SPLIT_COMMITTED cpu={} tid={} asid={} va=0x{:x} pa=0x{:x} broad_lock=0",
                cpu.0,
                facts.tid,
                facts.asid.0,
                facts.page.0,
                phys.0
            );
            // The marker the broad arm prints for a serviced demand fault, so an observer sees
            // the same completion whichever owner performed it.
            crate::yarm_log!("PAGE_FAULT_HANDLED_DEMAND");
            D::Complete(Ok(()))
        }
        // Pre-mutation: nothing was written, so the broad arm may re-derive and decide.
        other if other.may_fall_back_to_broad() => {
            crate::yarm_log!(
                "PF1_DEMAND_SPLIT_REFUSED cpu={} tid={} va=0x{:x} reason={}",
                cpu.0,
                facts.tid,
                facts.page.0,
                other.reason()
            );
            D::NotHandled
        }
        // Post-allocation: the allocation is rolled back exactly, and the broad path must NOT
        // run — it would allocate a second frame for a fault it never saw declined.
        other => {
            crate::yarm_log!(
                "PF1_DEMAND_SPLIT_FAILED_CLOSED cpu={} tid={} va=0x{:x} reason={}",
                cpu.0,
                facts.tid,
                facts.page.0,
                other.reason()
            );
            // The EXACT error the broad arm produces at the same step, through the SAME
            // `KernelError -> SyscallError` conversion its `map_err` chain uses.
            D::Complete(Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::from(
                    other
                        .failed_closed_error()
                        .unwrap_or(crate::kernel::boot::KernelError::UserMemoryFault),
                ),
            )))
        }
    }
}

#[cfg(feature = "hosted-dev")]
fn try_split_demand_page_fault_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _fault: Option<crate::kernel::trap::FaultInfo>,
) -> SplitDispatchDisposition {
    SplitDispatchDisposition::NotHandled
}

/// U9-PAGEFAULT1 §2c — the bridge entry for the demand PageFault route.
///
/// Tried AFTER the COW route and BEFORE the terminal one, because that is the order the broad arm
/// uses: COW first and only for writes, then the demand screen, then the terminal fall-through.
/// The three are mutually exclusive by class anyway, but preserving the order is what makes
/// "split and broad cannot disagree about a fault" true for the sequencing as well as the verdict.
pub(crate) fn try_split_demand_page_fault_dispatch(
    shared: &SharedKernel,
    cpu: CpuId,
    fault: Option<crate::kernel::trap::FaultInfo>,
) -> SplitDispatchDisposition {
    try_split_demand_page_fault_into_frame(shared, cpu, fault)
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

/// U9-RECV-BLOCK2 §2 — a receive whose `current` was cleared and could not be put back, handed to
/// the BRIDGE that owns the frame, the dispatch window and the architectural landing.
///
/// # Why this is a value and not a jump
///
/// Its predecessor was `recv_unsettleable_idle_terminal`, a `-> !` helper called from the syscall
/// body that jumped straight into an architecture halt loop. Everything the trap boundary owns was
/// bypassed by that jump: the `TrapPathWindow` (which `Drop` cannot retire on a path that never
/// unwinds), the outgoing-context capture, and each port's own idle landing — two of which settle
/// by RETURNING, not by diverging. x86_64's own comment on `settle_post_lock_terminal_idle` says
/// diverging from a drain "would be strictly worse: it would skip the depth clear and the
/// attestation epilogue the tail performs, and it would add a second place that decides how this
/// architecture idles", and RISC-V's landing is the typed `EnterKernelIdle` its bridge returns.
///
/// So the route reports the FACTS and the bridge performs the settlement. The facts are exactly
/// two, and neither can be re-derived at the bridge: which incarnation this trap entered from —
/// captured before Phase A's clear, so it names the entering task and not a replacement — and
/// what the rank-1 recovery actually achieved, as a verified post-state.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) struct SplitBlockUnsettled {
    /// The exact incarnation the trap entered from.
    pub(crate) entering: crate::kernel::recv_waiter_split::RecvEnteringIncarnation,
    /// Where the recovery left it. Read, never assumed.
    pub(crate) outcome: crate::kernel::recv_waiter_split::RecvUnwindOutcome,
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
    /// U9-RECV-BLOCK2 §2 — a RECOGNIZED receive cleared `current` and could not put the entering
    /// incarnation back, so the trap must not return through the entering frame and there is no
    /// deferral for a drain to consume.
    ///
    /// It is emphatically not `NotHandled`: the broad dispatcher must not service it, because the
    /// caller is no longer this CPU's current and re-running the receive against it is the exact
    /// corruption this disposition exists to prevent. It is not `QueueAdvanceCommitted` either —
    /// nothing was published and no drain owes anything. The BRIDGE settles it; see
    /// [`SplitBlockUnsettled`].
    BlockUnsettled(SplitBlockUnsettled),
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
    /// U9-TIMER2 §2 — a preempting TIMER tick on a CPU with NO current task and queued work.
    ///
    /// # Why none of the others fits
    ///
    /// `PostWorkCommitted` is what its idle sibling answers, and it is false here: it means no
    /// queue selection runs, which for a CPU with runnable work is precisely the thing that must
    /// happen. `QueueAdvanceCommitted` is wrong in the other direction — it means a terminal
    /// transition was PUBLISHED and the exact outgoing identity rides on a per-CPU deferral. There
    /// is no outgoing identity here. The CPU was parked at its kernel-idle boundary; nothing was
    /// preempted, nothing was captured, nothing was re-enqueued, and there is no frame belonging
    /// to a task for this trap to return through.
    ///
    /// Fabricating one — tid 0, or borrowing NR 0's or NR 9's deferral cell to activate their
    /// drains — would make a drain believe a task it can name went off-CPU in this trap, and its
    /// reverify, its rollback and its idle settlement would all then be about a task that never
    /// participated.
    ///
    /// # What it means, exactly
    ///
    /// The tick, the claim/ack and the re-arm have COMMITTED. From here the broad dispatcher must
    /// not run — it would tick a second time — so there is no fallback, by construction. What is
    /// owed is one authoritative queue advance with no outgoing task, and the bridge that owns
    /// this trap's frame and landing performs it.
    ///
    /// The run-queue count that selected this disposition is an OBSERVATION, not a reservation.
    /// The queue may be empty by the time the drain's authoritative selection runs, and that is a
    /// legitimate outcome (the CPU idles), not a disagreement to reconcile.
    TimerIdleQueueAdvance,
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
            Self::BlockUnsettled(_) => {
                panic!("an unsettled receive has no pre-U9-QA equivalent")
            }
            Self::PostWorkCommitted { .. } => {
                panic!("a committed post-work outcome has no pre-U9-QA equivalent")
            }
            Self::TimerIdleQueueAdvance => {
                panic!("an idle-boundary timer queue advance has no pre-U9-QA equivalent")
            }
        }
    }
}

/// U9-SEND-FINAL §1 — the outcome of a **recognized** NR 1, which is a strictly smaller set than
/// `SplitDispatchDisposition`.
///
/// The distinction the directive draws is between "this trap is not an `IpcSend`", which only the
/// entry point can answer and which is not a fall-through at all, and "this IS an `IpcSend` and it
/// needs settling", which used to be able to answer `NotHandled` from three further places. Those
/// three were not declines of an unrecognized trap; they were a recognized NR 1 being handed to
/// the terminal broad dispatcher.
///
/// Giving the recognized body its own return type is what makes that unrepeatable: there is no
/// value here meaning "the broad dispatcher should service this trap", exactly as there is none in
/// the `Result<(), TrapHandleError>` U9-IPC-RESIDUAL2 gave NR 7. NR 1 never publishes a queue
/// advance of its own — its park hands one to the post-work drain instead — so
/// `QueueAdvanceCommitted` is absent too, and the type states that rather than leaving it to a
/// comment.
/// U9-RECV-FINAL §1 / U9-FUTEX-WAIT-FINAL §2 — the outcome of a **recognized BLOCKING syscall**,
/// which is a strictly smaller set than `SplitDispatchDisposition`.
///
/// Shared by NR 2 / NR 5 and by NR 9, because the shape is the family-neutral one: a blocking
/// syscall can finish immediately, it can PARK — which is why `QueueAdvanceCommitted` is present
/// here and absent from the send type — it can owe the post-work drain, and it can clear `current`
/// and fail to put the caller back. What is NOT shared is any of the receive family's own policy:
/// its `BlockedRecvState`, its waiter publication and its empty-answer encoding are the receive
/// transaction's, and NR 9 uses none of them.
///
/// The receive family had two entry points and both could answer `NotHandled` long after they had
/// recognized the syscall — the queued lane through an `Option`, the blocking lane through
/// eighteen separate declines. Those were not "this trap is not a receive"; they were a
/// recognized receive being handed to the terminal broad dispatcher.
///
/// Giving the recognized body its own return type is what makes that unrepeatable, as it did for
/// NR 1 and NR 7. A receive can finish immediately, it can PARK — which is why
/// `QueueAdvanceCommitted` is present here and absent from the send type — and it can owe the
/// post-work drain a delivery. It can never ask the broad dispatcher to service it.
#[derive(Debug)]
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) enum SplitBlockingDisposition {
    /// The syscall is finished and its result is in the frame.
    Complete(Result<(), TrapHandleError>),
    /// The receiver is PARKED: a terminal transition is published, the caller is no longer
    /// current, and the existing D2-recv drain owes the queue advance.
    QueueAdvanceCommitted,
    /// A delivery the post-work drain owes.
    PostWorkCommitted { finalize_syscall: bool },
    /// U9-RECV-BLOCK2 §2 — `current` was cleared and the entering incarnation could not be put
    /// back. The BRIDGE settles it; the frame already carries this receive's canonical answer, so
    /// a settlement that captures it completes the syscall rather than re-entering it.
    Unsettled(SplitBlockUnsettled),
}

impl SplitBlockingDisposition {
    /// Widen to the dispatcher's type. Total by construction: every arm names a committed
    /// outcome, so widening can never introduce the `NotHandled` the narrow type excludes.
    #[cfg_attr(feature = "hosted-dev", allow(dead_code))]
    pub(crate) fn into_dispatch(self) -> SplitDispatchDisposition {
        match self {
            Self::Complete(result) => SplitDispatchDisposition::Complete(result),
            Self::QueueAdvanceCommitted => SplitDispatchDisposition::QueueAdvanceCommitted,
            Self::PostWorkCommitted { finalize_syscall } => {
                SplitDispatchDisposition::PostWorkCommitted { finalize_syscall }
            }
            Self::Unsettled(unsettled) => SplitDispatchDisposition::BlockUnsettled(unsettled),
        }
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) enum SplitSendDisposition {
    /// The syscall is finished and its result is in the frame.
    Complete(Result<(), TrapHandleError>),
    /// A delivery the post-work drain owes, or a parked sender whose queue advance it performs.
    PostWorkCommitted { finalize_syscall: bool },
}

impl SplitSendDisposition {
    /// Widen to the dispatcher's type. Total by construction: every arm names a committed outcome,
    /// so widening can never introduce the `NotHandled` the narrow type exists to exclude.
    #[cfg_attr(feature = "hosted-dev", allow(dead_code))]
    pub(crate) fn into_dispatch(self) -> SplitDispatchDisposition {
        match self {
            Self::Complete(result) => SplitDispatchDisposition::Complete(result),
            Self::PostWorkCommitted { finalize_syscall } => {
                SplitDispatchDisposition::PostWorkCommitted { finalize_syscall }
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
    // U9-RECV-BLOCK1 §4 — the receive family authenticates its admission on the trap's own
    // authority rather than on the ambient dispatch CPU. Minting it here from `cpu` would defeat
    // the point (an authority must be unforgeable), so the live window is READ: `for_cpu` returns
    // the authority this CPU's open trap window already holds, and `none` when no window is open —
    // which is never live and therefore authorizes nothing.
    let authority = crate::runtime::DispatchAuthority::for_open_window(cpu);
    match try_split_futex_wait_into_frame(shared, cpu, frame, authority) {
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
    match try_split_ipc_recv_family_into_frame(shared, cpu, frame, authority) {
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
    match try_split_exit_current_task(shared, cpu, frame) {
        SplitDispatchDisposition::NotHandled => {}
        handled => return handled,
    }
    // U9-RESIDUAL1 §3 — the FIFTH switching class, and the last one whose queue advance was
    // already leaving the broad lock while its DECISION was not. Yield is tried here for the same
    // reason the four above it are: the NR-only whitelist's contract is that every class on it may
    // be early-returned through the caller's own frame, and a yielding task is re-enqueued behind
    // whatever the drain selects. It answers `QueueAdvanceCommitted` so the EXISTING post-lock
    // Yield drain — present and default-on on all three architectures since 192B/195G/196G —
    // selects and applies the next context.
    match try_split_yield_into_frame(shared, cpu, frame) {
        SplitDispatchDisposition::NotHandled => {}
        handled => return handled,
    }
    // U9-IPC-RESIDUAL2 §2 — the SIXTH switching class. `IpcCall` (NR 6) joins the five above it
    // for the reason the list exists: its full-endpoint arm parks the caller, and a parked
    // caller may not be early-returned through its own frame. It produces every shape the list
    // was built for — a completed syscall, a delivery the post-work drain owes, and a parked
    // sender whose queue advance that drain performs — exactly as `IpcSend` does.
    match try_split_ipccall_into_frame(shared, cpu, frame) {
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
///
/// ## U9-SEND-FINAL §1 — the entry point, and the ONLY thing it decides
///
/// It answers exactly one question: is this trap an `IpcSend`? A `NotHandled` from here is not a
/// fall-through — it is a different syscall, for which this route has no opinion and the
/// dispatcher goes on to consult the next class. Everything a RECOGNIZED NR 1 can produce is
/// settled by the body below, whose return type cannot express "hand this to the broad
/// dispatcher".
#[cfg(not(feature = "hosted-dev"))]
fn try_split_ipc_send_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> SplitDispatchDisposition {
    if !matches!(Syscall::decode(frame.syscall_num()), Ok(Syscall::IpcSend)) {
        return SplitDispatchDisposition::NotHandled;
    }
    try_split_ipc_send_recognized(shared, cpu, frame).into_dispatch()
}

/// U9-SEND-FINAL §1 — a RECOGNIZED NR 1, settled pre-lock in every reachable outcome.
///
/// The two admission escapes this used to answer `NotHandled` from are now settled with the
/// error the broad path produces for the same condition, derived from source rather than
/// assumed:
///
/// * **`cpu_idx >= MAX_CPUS`.** The broad phase is `with_cpu(cpu, ..)`, which runs
///   `set_current_cpu(cpu)?` BEFORE its closure — `validate_online_cpu` → `check_cpu` →
///   `SchedulerError::InvalidCpu` → `map_scheduler_error` → `KernelError::WrongObject` →
///   `SyscallError::WrongObject`, with the closure never entered. So the broad dispatcher does
///   not service this trap either; it produces that error one lock later. Handing it over was
///   never a fallback, it was a slower way to the same answer, and settling it here is exact.
///   The same derivation covers an in-range but OFFLINE CPU, which `validate_online_cpu` refuses
///   with the same `SchedulerError` class.
/// * **No current task.** `handle_ipc_send` opens with `validate_endpoint_right(cap, SEND)?`,
///   whose first step is `current_task_cnode()`; with no current task that is `None`, the slot
///   lookup is `None`, and it answers `InvalidCapability` — before `current_tid(kernel)?` is ever
///   reached, so `Internal` is NOT the broad answer for this condition.
///
/// Neither is a fallback with a proof attached; both are settlements, so nothing about them
/// needs one.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_ipc_send_recognized(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> SplitSendDisposition {
    use crate::kernel::capabilities::{CapId, CapObject, CapRights};
    use crate::kernel::ipc::{EndpointMode, SharedMemoryRegion};
    use crate::kernel::syscall::{
        IpcSendPayloadShape, REPLY_CAP_QUEUEING_SUPPORTED, SYSCALL_ARG_CAP,
        SYSCALL_ARG_INLINE_PAYLOAD0, SYSCALL_ARG_INLINE_PAYLOAD1, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR,
        SyscallError, classify_ipc_send_payload_shape, frame_ipc_send_message,
        transfer_cap_arg_present,
    };
    use SplitSendDisposition as D;

    // ── (1) CPU and requester ───────────────────────────────────────────────────────────────
    let cpu_idx = cpu.0 as usize;
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        crate::yarm_log!(
            "IPC_SEND_SPLIT_INVARIANT cpu={} reason=cpu_out_of_range result=failed_closed",
            cpu.0
        );
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::WrongObject)));
    }
    let Some(tid) = shared.current_tid_authoritative(cpu) else {
        crate::yarm_log!(
            "IPC_SEND_SPLIT_REFUSED cpu={} reason=no_current_task err=InvalidCapability",
            cpu.0
        );
        return D::Complete(Err(TrapHandleError::Syscall(
            SyscallError::InvalidCapability,
        )));
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
            // U9-SEND-FINAL §2 — the source is SELECTED, not inferred from a reader's refusal.
            //
            // The delivered route asked `copy_from_user_asid_split_read` and treated every `None`
            // alike, answering `NotHandled`. That reader refuses three unrelated things, and only
            // one of them is a fault:
            //
            //   * `len == 0`   → a LEGAL EMPTY PAYLOAD, which `copy_from_user` answers `Ok` for
            //                    (its per-byte loop does not run) and which the broad path sends
            //                    happily. Folding it into the refusal is how every zero-length
            //                    user send reached the terminal broad dispatcher.
            //   * `len > 192`  → IMPOSSIBLE here, derived rather than assumed:
            //                    `classify_ipc_send_payload_shape` yields `Inline` for a user
            //                    sender only when `len <= Message::MAX_PAYLOAD` (128), and the
            //                    reader's cap is `DEBUG_LOG_MAX_BYTES` (192). The assertion below
            //                    states that bound where it is relied on; it is not a runtime
            //                    branch, because no reachable value could take one.
            //   * anything else → a genuine user-memory fault, including a caller whose
            //                    incarnation ASID is the kernel's (`asid_raw == 0`), which
            //                    `copy_from_user`'s `validate_user_access_for_asid` answers
            //                    `UserMemoryFault` for too.
            //
            // A fault is settled through the owner U9-IPC-RESIDUAL2 §2 established, which runs
            // the same two steps in the same order `record_user_fault` does, and the syscall then
            // returns SUCCESS, exactly as `handle_ipc_send`'s `record_user_fault(..); return
            // Ok(())` does.
            debug_assert!(
                len <= crate::kernel::ipc::Message::MAX_PAYLOAD,
                "the Inline shape bounds an inline length below the reader's cap"
            );
            match nr1_classify_inline_source(sender_asid, len) {
                // Nothing to copy. The buffer is already zeroed and `payload_len` is 0.
                Nr1InlineSource::Empty => {}
                Nr1InlineSource::UserMemory { asid_raw, len } => {
                    let Some(bytes) =
                        shared.copy_from_user_asid_split_read(asid_raw, user_ptr_or_offset, len)
                    else {
                        return nr1_source_read_fault(
                            shared,
                            cpu,
                            frame,
                            tid,
                            user_ptr_or_offset,
                            len,
                        );
                    };
                    payload_buf[..len].copy_from_slice(&bytes[..len]);
                }
                Nr1InlineSource::Registers { len } => {
                    // A kernel task's payload rides in the argument registers.
                    let words = [
                        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
                        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
                    ];
                    let Some(regs) = crate::kernel::ipc::unpack_register_payload(words, len) else {
                        return D::Complete(Err(TrapHandleError::Syscall(
                            SyscallError::InvalidArgs,
                        )));
                    };
                    payload_buf[..len].copy_from_slice(&regs[..len]);
                }
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
                // U9-IPC-RESIDUAL2 §3: NR 1 mints no reply authority — a `Reply` capability is
                // refused admission to a queueing send at step (5) — so it owes none back.
                reply_authority: None,
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

/// U9-SEND-FINAL §2 — **where an inline NR 1 payload comes from**, as one policy.
///
/// `handle_ipc_send`'s inline arm makes this choice in two places that look like one:
///
/// ```text
/// if sender_has_user_asid { copy_from_current_user(ptr, len)? } else { inline_payload_from_frame(frame, len)? }
/// ```
///
/// and `copy_from_user` then answers `len == 0` with `Ok` and an untouched buffer, because its
/// per-byte loop does not run. So there are THREE sources, not two, and the empty one is a
/// success rather than a copy. The delivered split route folded the empty case into its reader's
/// refusal and answered `NotHandled` for it, which sent every zero-length user send to the
/// terminal broad dispatcher.
///
/// Stating the choice once, as a value, is what stops the two sides drifting again: the
/// production route selects its source from here, and the differential cases ask this the same
/// question they ask the real dispatcher.
///
/// Nothing here reads memory, resolves a capability or mutates anything — it is a decision about
/// two inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Nr1InlineSource {
    /// A legal EMPTY payload. No copy is owed and none is attempted; the message carries zero
    /// bytes. Reached by a user sender with `len == 0` — a kernel sender's empty payload goes
    /// through the register source, which handles zero the same way.
    Empty,
    /// A USER sender's payload, in its own address space. An unreadable buffer here is the
    /// canonical `record_user_fault(.., Read)` case, never `InvalidArgs`.
    UserMemory { asid_raw: u64, len: usize },
    /// A KERNEL-ASID sender's payload, riding in the argument registers.
    Registers { len: usize },
}

#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) fn nr1_classify_inline_source(
    sender_asid: Option<crate::kernel::vm::Asid>,
    len: usize,
) -> Nr1InlineSource {
    match sender_asid {
        // The register source owns every kernel-ASID sender, including `len == 0`:
        // `inline_payload_from_frame` is what `handle_ipc_send` calls for one, at every length.
        None => Nr1InlineSource::Registers { len },
        Some(_) if len == 0 => Nr1InlineSource::Empty,
        Some(asid) => Nr1InlineSource::UserMemory {
            asid_raw: asid.0 as u64,
            len,
        },
    }
}

/// U9-SEND-FINAL §2 — settle an unreadable NR 1 source buffer through the canonical fault owner.
///
/// `handle_ipc_send` answers this with `record_user_fault(kernel, frame, user_ptr,
/// FaultAccess::Read); return Ok(())` — a fault RECORD and a `PageFault` frame, and a SUCCESSFUL
/// syscall return, not an error return. `record_split_source_read_fault` is the split twin
/// U9-IPC-RESIDUAL2 §2 extracted for NR 6 and NR 7, and it performs those two steps in that same
/// order.
///
/// ## Why returning through the entering frame is safe here, which recording a fault does not by
/// itself establish
///
/// The record is a rank-8 write and a frame error. It publishes NO terminal transition: the
/// caller is not blocked, not preempted, not exiting; it is still `current` on this CPU and still
/// owns this frame, exactly as it is after the broad handler's own `record_user_fault`. Nothing
/// is stashed on the per-CPU post-work channel and no deferral is armed, so the architecture tail
/// has nothing to drain and returns through this frame — which is the same frame the broad path
/// would have returned through. The `Complete(Ok(()))` disposition is what says so: it is the one
/// outcome that means "serviced, return now, no drain owed".
///
/// This is also strictly BEFORE anything is consumed. The fault is raised inside step (7), and
/// step (8) — `stash_transfer_envelope_split` — is the first consuming step, so on this path
/// there is no envelope, no transient pin, no `Message`, no queue entry, no receiver copy and no
/// wake to undo. That ordering is `handle_ipc_send`'s too: its `record_user_fault` arm returns
/// before `stash_transfer_handle` is called.
///
/// ## The fallible settlement
///
/// The split twin can fail where the broad one cannot, and for exactly one reason: it must BIND
/// the CPU that `with_cpu` had already bound on entry, through the same `validate_online_cpu`
/// predicate. If that refuses, the record did not happen and the frame carries no `PageFault` —
/// so answering `Ok(())` would return a task an unset result for a fault the kernel never
/// recorded. It is refused instead, with the error that predicate's own failure maps to
/// (`InvalidCpu`/`CpuOffline` → `KernelError::WrongObject` → `SyscallError::WrongObject`), which
/// is what the broad phase's `with_cpu` would have produced for the same CPU.
#[cfg(not(feature = "hosted-dev"))]
fn nr1_source_read_fault(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
    tid: u64,
    user_ptr: usize,
    len: usize,
) -> SplitSendDisposition {
    match shared.record_split_source_read_fault(cpu, frame, user_ptr) {
        Ok(()) => {
            crate::yarm_log!(
                "IPC_SEND_SPLIT_SOURCE_FAULT cpu={} tid={} user_ptr={:#x} len={} access=read envelopes=0 enqueues=0 deliveries=0 wakes=0 result=ok",
                cpu.0,
                tid,
                user_ptr,
                len
            );
            SplitSendDisposition::Complete(Ok(()))
        }
        Err(err) => {
            crate::yarm_log!(
                "IPC_SEND_SPLIT_INVARIANT cpu={} tid={} reason=fault_record_refused err={:?} result=failed_closed",
                cpu.0,
                tid,
                err
            );
            SplitSendDisposition::Complete(Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::from(err),
            )))
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

/// U9-RECV-BLOCK2 §1 — what the BLOCKING lane answers, as a type rather than as a disposition
/// that has to be re-interpreted by its caller.
///
/// The lane used to answer `SplitDispatchDisposition`, whose `NotHandled` meant three unrelated
/// things at once — "the immediate lane owns this state", "this CPU cannot park anything right
/// now", and "not a receive at all" — and the family entry could not tell them apart, so it
/// translated all three into the same terminal broad hand-off. Naming them is what makes each one
/// settleable.
#[cfg(not(feature = "hosted-dev"))]
enum BlockingLaneOutcome {
    /// The lane answered the trap.
    Settled(SplitBlockingDisposition),
    /// A precondition for parking on THIS CPU did not hold, for a reason that is not about the
    /// message. Each variant carries its own established-impossibility argument; see
    /// [`settle_cannot_park`].
    CannotPark(CannotParkReason),
}

#[cfg(not(feature = "hosted-dev"))]
impl BlockingLaneOutcome {
    /// The lane answered the trap with a completed syscall. A constructor rather than a wrapped
    /// literal so the lane's own `return` sites read as they did before the type was introduced.
    fn complete(result: Result<(), TrapHandleError>) -> Self {
        Self::Settled(SplitBlockingDisposition::Complete(result))
    }
}

/// U9-RECV-BLOCK2 §1 — the reasons a parking receive cannot be committed on this CPU.
///
/// Every one of these is proven unreachable from an authority-bearing receive inside a live trap
/// window; the proofs live on [`settle_cannot_park`]. They are kept as distinct variants rather
/// than collapsed so that a marker names WHICH invariant broke if one ever does.
#[cfg(not(feature = "hosted-dev"))]
#[derive(Clone, Copy, Debug)]
enum CannotParkReason {
    /// This CPU's receive deferral was already taken when the lane checked it.
    AlreadyDeferred,
    /// …or was taken between that check and the reservation.
    DeferUnavailable,
    /// The queue-advance admission refused, for a refusal that is not `IncomingUnavailable`
    /// (which this family treats as "nothing resumable here", the same as no candidate at all).
    AdmissionRefused(crate::kernel::boot::QueueAdvanceRefusal),
    /// Phase A's compare-and-clear found a different task current than the one this trap decoded.
    PhaseAVictimChanged,
    /// U9-RECV-BLOCK2b §1 — a state the IMMEDIATE lane has already answered reached this lane.
    ///
    /// The immediate lane runs first and only [`RecvImmediateOutcome::EmptyAwaitingPark`]
    /// continues into this one, which already implies a recognized NR whose request asked to wait
    /// and whose capability resolved to an endpoint. The reason word names which of those
    /// implications appeared to fail.
    ImmediateLaneAlreadyAnswered(&'static str),
}

/// U9-RECV-FINAL §1 — **THE receive family entry**, and the only thing it decides.
///
/// It answers one question: is this trap an `IpcRecv` or an `IpcRecvTimeout`? A `NotHandled` from
/// here is not a fall-through — it is a different syscall, for which this route has no opinion.
/// Everything a RECOGNIZED receive can produce is settled by the body below, whose return type
/// cannot express "hand this to the broad dispatcher".
fn try_split_ipc_recv_family_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
    authority: crate::runtime::DispatchAuthority,
) -> SplitDispatchDisposition {
    let Ok(Syscall::IpcRecv | Syscall::IpcRecvTimeout) = Syscall::decode(frame.syscall_num())
    else {
        // U9-RECV-BLOCK2 §1 — THE ONLY `NotHandled` the receive family can produce, and it is not
        // a fall-through: this trap is a different syscall, about which this route has no opinion.
        //
        // The family used to have a second one. `try_split_recv_recognized` returned an `Option`
        // whose `None` arm landed here and was counted as a terminal broad entry — so a
        // RECOGNIZED receive that no lane had settled left the family through the same door as a
        // syscall that was never ours. That door is gone: the recognized body now returns a
        // `SplitBlockingDisposition`, a type with no representation for "hand this to the broad
        // dispatcher", and every outcome it can reach is settled. The narrow type is the final
        // enforcement; the settlements are the work.
        return SplitDispatchDisposition::NotHandled;
    };
    try_split_recv_recognized(shared, cpu, frame, authority).into_dispatch()
}

/// U9-RECV-FINAL §1/§2 — a RECOGNIZED receive, and the order its lanes are consulted in.
///
/// The two admission escapes are settled with the error the broad path produces for the same
/// condition, derived from source exactly as NR 1's were:
///
/// * **`cpu_idx >= MAX_CPUS`.** The broad phase is `with_cpu(cpu, ..)`, which runs
///   `set_current_cpu(cpu)?` BEFORE its closure — `validate_online_cpu` → `check_cpu` →
///   `SchedulerError::InvalidCpu` → `map_scheduler_error` → `KernelError::WrongObject` →
///   `SyscallError::WrongObject`, closure never entered. The broad dispatcher does not service
///   that trap either; it produces the same error one lock later.
/// * **No current task.** `handle_ipc_recv` reads `current_tid().unwrap_or(0)` and then calls
///   `validate_endpoint_right(cap, RECEIVE)?`, whose first step is `current_task_cnode()`; with
///   no current task that is `None`, so the answer is `InvalidCapability` — not the `Internal`
///   that a `current_tid()?` would give, because this handler never asks that question.
///
/// ## Lane order, and why it is this one
///
/// The BLOCKING lane is consulted first, as it was before this package, because it admits only
/// the state in which the immediate lane declines — an endpoint with nothing to take — and
/// refuses with `would_not_block` the moment a message is waiting. The two are disjoint, so the
/// order is a preservation of live behaviour rather than a policy choice.
///
/// A decline from either lane is INTERNAL: it reaches the next pre-lock owner, never the broad
/// dispatcher. That is the whole point of the narrow return type.
fn try_split_recv_recognized(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
    authority: crate::runtime::DispatchAuthority,
) -> SplitBlockingDisposition {
    use crate::kernel::syscall::SyscallError;
    use SplitBlockingDisposition as D;

    let cpu_idx = cpu.0 as usize;
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        crate::yarm_log!(
            "IPC_RECV_SPLIT_INVARIANT cpu={} reason=cpu_out_of_range result=failed_closed",
            cpu.0
        );
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::WrongObject)));
    }
    let Some(tid) = shared.current_tid_authoritative(cpu) else {
        crate::yarm_log!(
            "IPC_RECV_SPLIT_REFUSED cpu={} reason=no_current_task err=InvalidCapability",
            cpu.0
        );
        return D::Complete(Err(TrapHandleError::Syscall(
            SyscallError::InvalidCapability,
        )));
    };

    // (3) The IMMEDIATE lane, FIRST — the canonical order, and the reason the queue-consumption
    // race no longer exists.
    //
    // This entry used to consult the BLOCKING lane first, on the reasoning that the two are
    // disjoint: the blocking lane refuses `would_not_block` the moment a message is waiting, and
    // the immediate lane serves exactly that state. They are disjoint at any INSTANT, and that is
    // not the same as being disjoint across two separate observations. The interleaving:
    //
    //   A issues a finite NR 5. The blocking lane's rank-3 would-block read sees a queued
    //   message and refuses `would_not_block`. B consumes that message. A's immediate lane now
    //   finds the endpoint empty and declines, because an empty endpoint under a finite timeout
    //   belongs to the blocking owner. Neither lane served it, and the receive fell out of the
    //   family — which is precisely what the terminal broad hand-off was catching.
    //
    // The canonical handler has no such window: `ipc_recv_with_optional_deadline` TRIES THE TAKE,
    // and parks only if the take came back empty. The take is the observation. Running the
    // immediate lane first reproduces that order exactly, so "saw a message" and "took the
    // message" are one step rather than two, and a sender that arrives after an empty take is
    // caught where the canonical owner catches it — at the rank-3 publish, whose `QueueNonEmpty`
    // race already continues into this same immediate owner.
    // U9-RECV-BLOCK2b §1 — the take's answer IS the continuation. There is no second observation
    // of the queue anywhere past this point, on either lane.
    match try_split_recv_immediate_lane(shared, cpu, frame) {
        RecvImmediateOutcome::Answered(result) => return D::Complete(result),
        // The planner refused the SHAPE, or the lane could not read a current task. No take ran,
        // so the endpoint state is unknown and parking is not licensed. Established-impossible
        // for this family; `settle_no_immediate_take` carries the argument and the answer.
        RecvImmediateOutcome::NoTakeAttempted(reason) => {
            return settle_no_immediate_take(shared, cpu, tid, frame, reason);
        }
        // The take ran and the endpoint was empty, and the request asked to wait. This is
        // `ipc_recv_with_optional_deadline`'s state immediately before
        // `block_current_on_receive_with_deadline`, and it is the ONLY state that continues into
        // the blocking lane.
        RecvImmediateOutcome::EmptyAwaitingPark => {}
    }

    // (4) The BLOCKING lane, entered on the strength of the EMPTY TAKE above and nothing else.
    //
    // A sender that enqueues between that take and the park is not a defect this lane can read
    // its way out of — the canonical owner has the same gap and closes it in the same place, at
    // the rank-3 waiter publication, whose `QueueNonEmpty` refusal unwinds the park and continues
    // into the immediate owner for the message that raced in. That is a PUBLICATION outcome, not
    // a pre-park predicate, which is why removing the predicate does not reopen anything.
    //
    // This is the one half of the family that cannot exist on the hosted profile: it parks a task
    // and publishes into a queue-advance drain no hosted build runs. The IMMEDIATE lane above is
    // production code the hosted cases drive directly, so it stays compiled.
    #[cfg(not(feature = "hosted-dev"))]
    match try_split_blocking_ipc_recv_into_frame(shared, cpu, cpu_idx, tid, frame, authority) {
        BlockingLaneOutcome::Settled(settled) => settled,
        BlockingLaneOutcome::CannotPark(reason) => {
            settle_cannot_park(shared, cpu, tid, frame, reason)
        }
    }
    // The hosted profile compiles NO parking owner at all, so an empty take that asked to wait
    // has nobody to hand to. This landing is the hosted build's alone — it is not a fallback the
    // production route can reach, and `the_production_family_has_no_empty_answer_substitute`
    // pins that the production arm above contains no equivalent. Hosted cases that mean to
    // exercise the parking POLICY drive the immediate lane and Phase A directly and assert on
    // `RecvImmediateOutcome::EmptyAwaitingPark`, which is the policy's own answer.
    #[cfg(feature = "hosted-dev")]
    {
        let _ = (tid, authority);
        crate::yarm_log!(
            "IPC_RECV_SPLIT_SETTLED cpu={} reason=hosted_profile_has_no_parking_owner",
            cpu.0
        );
        D::Complete(recv_encode_empty_answer(
            frame,
            crate::kernel::syscall::SyscallError::WouldBlock,
        ))
    }
}

/// U9-RECV-BLOCK2 §1 — the immediate lanes, one per syscall, behind one call.
///
/// The two decode DIFFERENT argument slots — NR 5 carries its timeout in arg 3, the slot NR 2 uses
/// for its recv-v2 metadata pointer — so each builds its own `RecvRequest` from its own ABI and
/// both drive the same delivery engine. Neither rewrites the frame's syscall number into the
/// other's.
///
/// U9-RECV-BLOCK2b §1 — the answer is a [`RecvImmediateOutcome`], not an `Option`. The lane's
/// TAKE is the observation the continuation is derived from: `EmptyAwaitingPark` is the canonical
/// parking precondition and `NoTakeAttempted` says the queue was never looked at. No caller may
/// re-derive either by reading the endpoint a second time.
fn try_split_recv_immediate_lane(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> RecvImmediateOutcome {
    match Syscall::decode(frame.syscall_num()) {
        Ok(Syscall::IpcRecv) => try_split_ipc_recv_queued_plain_into_frame(shared, cpu, frame),
        // EVERY NR 5, not only the non-blocking probe: a probe is answered here outright, and a
        // TIMED receive whose message is already queued is served by the same engine, because the
        // canonical handler also tries the immediate engine before it looks at `request.blocking`.
        // An empty endpoint under a finite timeout is reported as the parking precondition, never
        // converted into `WouldBlock`.
        Ok(Syscall::IpcRecvTimeout) => {
            shared.try_split_ipc_recv_timeout_immediate_into_frame(cpu, frame)
        }
        // The family entry decoded this same field and declined anything else before either lane
        // was reached, so no take is attempted here.
        _ => RecvImmediateOutcome::NoTakeAttempted("not_recv_family"),
    }
}

/// U9-RECV-BLOCK2b §1 — settle a recognized receive whose immediate lane attempted NO take.
///
/// # This is not an empty queue, and it must never be answered as one
///
/// The predecessor of this function re-ran the immediate lane and, on a second decline, encoded
/// the canonical empty answer. That was the concealment: a `WouldBlock` handed to a receive that
/// asked for a finite wait and whose deadline had not elapsed, produced from a state where the
/// queue had never been read at all. The lane now reports WHY it declined, so this settlement
/// answers the actual condition instead of guessing the queue's contents.
///
/// # Every producer is established-impossible for NR 2 / NR 5
///
/// * **`shape_not_served`.** `plan_recv_core` answers `RecvPlan::FallbackRequired` in exactly
///   three places: `RecvRequestKind::SharedV3Future`, a `RecvMetaTarget::V3Future` metadata
///   target, and `map_intent != RecvMapIntent::None`. The two builders this family uses —
///   `RecvRequest::from_legacy_ipc_recv` (NR 2) and `RecvRequest::from_ipc_recv_timeout` (NR 5) —
///   construct none of them: both hard-code `map_intent: RecvMapIntent::None`, both set `kind` to
///   `LegacyRecv`/`NonblockingProbe`/`TimedRecv`, and the only metadata target either can produce
///   is `RecvMetaTarget::V2` (NR 5's is set from args 4/5 by the immediate lane, to `V2` or left
///   `None`). `the_recv_family_cannot_construct_an_unserved_shape` re-derives that from source.
/// * **`no_current_task`.** The family entry read `tid` through `current_tid_authoritative(cpu)`
///   in this same trap before either lane ran; installing a current on a CPU requires running on
///   that CPU, and this trap is what is running on it.
/// * **`not_nr2` / `not_recv_family`.** Defensive re-decodes of the syscall number the family
///   entry has already matched against the same field.
/// * **`no_phase_a_result`.** NR 2's engine builds `result` from a match whose every remaining
///   arm produces `Some(..)`; the declining arm returns before that point.
///
/// # Why fail-closed rather than divergent
///
/// Identical to [`settle_cannot_park`]: nothing has been mutated, the caller is still this CPU's
/// current task and the entering frame is still its own. Answering `Internal` through that frame
/// reports the broken invariant without taking the machine down, and without the broad hand-off
/// this slice exists to remove.
fn settle_no_immediate_take(
    _shared: &SharedKernel,
    cpu: CpuId,
    tid: u64,
    _frame: &mut TrapFrame,
    reason: &'static str,
) -> SplitBlockingDisposition {
    crate::yarm_log!(
        "IPC_RECV_SPLIT_INVARIANT cpu={} tid={} reason={} result=failed_closed",
        cpu.0,
        tid,
        reason
    );
    SplitBlockingDisposition::Complete(Err(TrapHandleError::Syscall(
        crate::kernel::syscall::SyscallError::Internal,
    )))
}

/// U9-RECV-BLOCK2 §1 — settle a receive that must park but whose CPU could not commit the park.
///
/// # Every reason here is PRE-MUTATION, and every one is established-impossible
///
/// All four are decided before Phase A clears `current`, or by a Phase A compare-and-clear that
/// refused without touching the slot. The caller is therefore still this CPU's current task and
/// the entering frame is still its own, so answering through that frame loses nothing and parks
/// nothing — this is a fail-closed answer, not a settlement that has to account for a cleared
/// current. §2 governs the post-clear paths; none of them arrive here.
///
/// The impossibility argument, per reason, from the executed bodies:
///
/// * **`AlreadyDeferred` / `DeferUnavailable`.** The D2-recv deferral is per CPU. This route is
///   the only producer, it reserves at step (7), and every decline after that point clears it
///   before returning — `every_decline_after_the_reservation_clears_it` pins that. Traps do not
///   nest on any of the three architectures, so no second reservation can exist on this CPU while
///   this trap is running.
/// * **`AdmissionRefused`.** `IncomingUnavailable` never reaches here — the lane treats it as
///   "nothing resumable on this CPU", which is what it means for a parking receiver.
///   `ArchUnsupported` and `StashOccupied` are scoped to `StashedKernelSwitch` and this caller
///   passes `ExactTokenResume`. `MultiCpu` and `CpuNotAuthoritative` are the AMBIENT contract,
///   scoped by U9-RECV-BLOCK1 §4 to callers holding no authority. `NoTrapDrainer` and
///   `OutgoingIdentityStale` are both refuted by `TrapPathWindow::establish`: it sets
///   `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu]` and opens the dispatch window that minted this
///   authority, in that order, and only its `Drop` clears either. While a live authority exists,
///   the drainer flag is set and the epoch matches.
/// * **`PhaseAVictimChanged`.** `tid` was read by `current_tid_authoritative(cpu)` in this same
///   trap. Installing a current on a CPU requires running on that CPU, and this trap is what is
///   running on it, so the slot cannot have changed underneath.
/// * **`ImmediateLaneAlreadyAnswered`.** U9-RECV-BLOCK2b §1. Only
///   `RecvImmediateOutcome::EmptyAwaitingPark` continues into this lane, and that outcome is
///   produced by exactly one arm of each immediate engine: the one reached after the engine
///   resolved the receive capability to an endpoint, ran the authoritative take, and classified
///   the request as one that asked to WAIT. Each of the three reason words names one of those
///   established facts appearing to be false — a non-receive syscall number, a non-blocking
///   probe, or a resolved capability that is not an endpoint.
///
/// # Why fail-closed rather than divergent
///
/// Divergence is licensed for an established impossibility, but it is not required, and it is the
/// worse choice when the alternative loses nothing. The task is current, runnable and resumable;
/// answering `Internal` through its own frame reports the broken invariant to the caller and to
/// the log without taking the machine down over a condition no production path can construct.
/// What is NOT available is handing the trap to the broad dispatcher: that would be the escape
/// this slice exists to remove, wearing a different name.
#[cfg(not(feature = "hosted-dev"))]
fn settle_cannot_park(
    shared: &SharedKernel,
    cpu: CpuId,
    tid: u64,
    frame: &mut TrapFrame,
    reason: CannotParkReason,
) -> SplitBlockingDisposition {
    let _ = (shared, frame);
    let slug = match reason {
        CannotParkReason::AlreadyDeferred => "already_deferred",
        CannotParkReason::DeferUnavailable => "defer_unavailable",
        CannotParkReason::AdmissionRefused(_) => "admission_refused",
        CannotParkReason::PhaseAVictimChanged => "phase_a_victim_changed",
        CannotParkReason::ImmediateLaneAlreadyAnswered(detail) => detail,
    };
    crate::yarm_log!(
        "IPC_RECV_SPLIT_INVARIANT cpu={} tid={} reason={} detail={:?} result=failed_closed",
        cpu.0,
        tid,
        slug,
        reason
    );
    SplitBlockingDisposition::Complete(Err(TrapHandleError::Syscall(
        crate::kernel::syscall::SyscallError::Internal,
    )))
}

/// U9-RECV-BLOCK1 §3 — THE empty-result encoding for a receive that settles without a message.
///
/// This is not an error return, and getting that backwards is how a receiver ends up reading a
/// stale transfer-cap lane. `handle_ipc_recv_result_with_empty_error`'s `None` arm sets the
/// frame's ERROR lane to the empty error AND the transfer-cap return lane to the no-transfer
/// sentinel, then answers the SYSCALL `Ok(())`. Every settlement that ends a receive empty — the
/// non-blocking probe, the ownership-busy retry, the deadline-reservation refusal — owes exactly
/// those two writes, so there is one place that performs them.
fn recv_encode_empty_answer(
    frame: &mut TrapFrame,
    empty_error: crate::kernel::syscall::SyscallError,
) -> Result<(), TrapHandleError> {
    frame.set_err(empty_error.code());
    if crate::kernel::syscall::recv_boundary_encode_transfer_cap_ret(frame, None).is_err() {
        return Err(TrapHandleError::Syscall(
            crate::kernel::syscall::SyscallError::Internal,
        ));
    }
    Ok(())
}

/// U9-RECV-BLOCK2 §2 — settle a receive whose block has been UNWOUND, from the recovery's verified
/// outcome.
///
/// # One answer, two destinations
///
/// Every caller of this reaches it the same way: a post-clear failure whose canonical answer it
/// has already computed, and a rank-1 recovery that either put the entering incarnation back or
/// did not. The answer does not depend on which — `WrongObject` for a publish that named an
/// endpoint the recheck refused, `WouldBlock` for a deadline reservation that could not be taken —
/// so it is ENCODED INTO THE ENTERING FRAME first, unconditionally, and only then does the
/// outcome decide where that frame goes.
///
/// * `Restored` — the exact incarnation is this CPU's `current` and `Running`. The frame is its
///   own, so the trap RETURNS through it and the syscall is complete.
/// * anything else — the frame is not this CPU's to return through, but it is still this
///   receive's completed answer. The bridge either captures it into the exact incarnation (which
///   completes the syscall for a task that will be dispatched later) or settles divergently; that
///   decision needs the trap boundary, so it is the bridge's.
///
/// # Why the answer is encoded rather than returned as `Err`
///
/// `SplitDispatchDisposition::Complete(Err(e))` leaves the encoding to the architecture epilogue,
/// which only runs on the returning path. A settlement that CAPTURES the frame instead needs the
/// answer already in it — otherwise the captured context carries this receive's arguments and no
/// result, and the task resumes past its syscall reading whatever the previous one left behind.
/// `recv_encode_empty_answer` is the receive family's own encoder: the error lane plus the
/// no-transfer sentinel, exactly the two writes `handle_ipc_recv_result_with_empty_error` performs.
#[cfg(not(feature = "hosted-dev"))]
fn recv_settle_after_unwind(
    cpu: CpuId,
    frame: &mut TrapFrame,
    entering: crate::kernel::recv_waiter_split::RecvEnteringIncarnation,
    outcome: crate::kernel::recv_waiter_split::RecvUnwindOutcome,
    answer: crate::kernel::syscall::SyscallError,
    reason: &'static str,
) -> SplitBlockingDisposition {
    let encoded = recv_encode_empty_answer(frame, answer);
    crate::yarm_log!(
        "IPC_RECV_BLOCK_SPLIT_SETTLED cpu={} tid={} asid={} reason={} answer={} outcome={} resumable={}",
        cpu.0,
        entering.tid,
        entering.asid.0,
        reason,
        answer.code(),
        outcome.slug(),
        u8::from(outcome.may_resume_entering_frame())
    );
    if outcome.may_resume_entering_frame() {
        return SplitBlockingDisposition::Complete(encoded);
    }
    // The encode failed only if the transfer-cap lane could not be written, which is a frame
    // defect rather than a settlement one; carry it as the answer so the bridge captures a frame
    // that says so rather than one that says nothing.
    let _ = encoded;
    SplitBlockingDisposition::Unsettled(SplitBlockUnsettled { entering, outcome })
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
/// `SharedKernel::recv_block_unwind_exact_split`, and its existence is what makes this route
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
    cpu_idx: usize,
    tid: u64,
    frame: &mut TrapFrame,
    authority: crate::runtime::DispatchAuthority,
) -> BlockingLaneOutcome {
    use crate::kernel::capabilities::{CapId, CapObject};
    use crate::kernel::recv_core::{RecvBlockingPolicy, RecvMetaTarget, RecvRequest};
    use crate::kernel::syscall::{
        SYSCALL_ARG_CAP, SYSCALL_ARG_INLINE_PAYLOAD0, SYSCALL_ARG_INLINE_PAYLOAD1, SYSCALL_ARG_LEN,
        SYSCALL_ARG_PTR, SYSCALL_ARG_TRANSFER_CAP,
    };
    use crate::kernel::task::BlockedRecvState;
    // U9-RECV-BLOCK2 §1 — the lane composes RECEIVE dispositions now; the family entry is the
    // only place a `SplitDispatchDisposition` is produced, and only for "not a receive".
    use SplitBlockingDisposition as D;

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
        // Unreachable: `try_split_ipc_recv_family_into_frame` decodes the same field and returns
        // `NotHandled` for anything else before this lane is entered, and the immediate lane
        // decoded it a third time on the way here.
        _ => {
            return BlockingLaneOutcome::CannotPark(
                CannotParkReason::ImmediateLaneAlreadyAnswered("not_recv_family"),
            );
        }
    };
    // U9-RECV-FINAL §1 retires the architecture exclusion this gate carried.
    //
    // The comment above it said exactly why it existed — "RISC-V is excluded for want of a live
    // witness, not for a structural reason: its D2-recv drain is the same shape" — and NR 5,
    // which shares this route step for step, has been admitted on all three ports throughout.
    // §4 supplies the witness, so the term is gone rather than narrowed.
    let _ = recv_timeout;
    // (2) THE PUBLICATION YIELDS ARE GONE. All of them. This step is now empty, and the history
    // is kept because it is the whole argument for why nothing belongs here.
    //
    // Every yield that ever stood at this position existed for the same reason: a `maybe_publish_*`
    // hook in the broad blocked-recv arm took `&mut KernelState`, this route holds no broad
    // reference, and so the entire receive was handed away to preserve work it could not perform.
    // Each one was retired the same way — by making the hook's body take the facts it needs
    // instead of the kernel it read them from:
    //
    // * **NR6/NR7 acknowledgements** (Stage 199D-WA3C2). Yielded on
    //   `ipccall_direct_publication_enabled()`. Retired by
    //   `publish_ipccall_direct_blocked_server_ack_with` /
    //   `publish_ipcreply_direct_blocked_caller_ack_with`, which step (10) drives. Live on
    //   AArch64 this yield was a real defect, not merely a cost: with direct production on it
    //   fired on EVERY blocking recv-v2, the caller's block was published late, and a reply could
    //   arrive with no claimable acknowledgement, be declined as mode-indeterminate, fall to
    //   legacy, and be LOST (`IPC_REPLY_FAIL err=WrongObject`, caller never resumed).
    // * **Shared-region acknowledgement** (U9-RECV-BLOCK1 §2). Yielded on
    //   `cfg!(feature = "shared-region-direct-oracle")` — a COMPILE-TIME term, so it fired in any
    //   build carrying the feature whether or not the oracle was ever enabled at runtime.
    //   Measured on the armed x86_64 profile: 114 ordinary receives per boot reaching the terminal
    //   acquisition to preserve an acknowledgement none of them could produce. Retired by
    //   `publish_shared_region_blocked_recv_ack_with`.
    // * **The SMP blocked-server marker** (U9-RECV-BLOCK1 §2/§5). The last one, keyed on any armed
    //   direct-oracle selector. What it protected was five authoritative reads inside one marker
    //   emitter; they are now carried as `SmpServerBlockedFacts` and step (10) drives the SAME
    //   body the broad arm drives. Keeping it would have been the §5 escape in its last form:
    //   `NotHandled` may remain only for "not NR 2 / NR 5", and "an oracle is armed" is not that.
    //
    // NR 5 never had a publication to yield back to at all — `handle_ipc_recv_timeout` calls none
    // of these hooks — which is why every one of them was scoped to NR 2 while it existed.
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
        // U9-RECV-BLOCK2 §1 — the ORIGINAL absolute deadline, consumed from the trap entry's
        // staging slot exactly as `handle_ipc_recv_timeout` consumes it.
        //
        // This route used to recompute `scheduler_tick_now_split_read() + timeout_ticks` here.
        // That is a SECOND tick read, taken later in the trap than the canonical one, so a
        // receive parking on this route expired on a later tick than the same receive parking
        // under the broad lock — and, worse, it made the deadline a function of WHEN in the trap
        // the blocking lane happened to run. `recv_block_phase_c_split` can send this receive
        // back through the immediate owner on the `QueueNonEmpty` race, so "when the blocking
        // lane ran" is not a fixed point, and a re-evaluated receive would have silently
        // extended its own timeout.
        //
        // `arch/trap_entry.rs` stages `now + timeout_ticks` for every NR 5 with a non-zero
        // timeout BEFORE any dispatch runs, and the broad handler `swap(0)`s it out. Taking the
        // same value here makes the deadline the trap's, not the lane's, and therefore stable
        // across every re-evaluation inside this trap. The fallback keeps the old formula for
        // the paths that stage nothing (a raw trap entry, or a CPU index out of range).
        let absolute = shared
            .take_preread_recv_timeout_deadline_split(cpu)
            .unwrap_or_else(|| {
                shared
                    .scheduler_tick_now_split_read()
                    .wrapping_add(timeout_ticks)
            });
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
            // U9-RECV-BLOCK2b §1 — established impossible. `timeout_ticks == 0` is NR 5's probe,
            // and a probe never reaches this lane: the immediate lane classifies it from the same
            // `RecvRequest::from_ipc_recv_timeout` builder and ANSWERS it — delivery if the take
            // found a message, the canonical empty encoding if it did not — so it can only leave
            // that lane as `Answered`, never as `EmptyAwaitingPark`.
            crate::yarm_log!(
                "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} reason=not_timed_recv",
                cpu.0,
                tid
            );
            return BlockingLaneOutcome::CannotPark(
                CannotParkReason::ImmediateLaneAlreadyAnswered("nonblocking_probe"),
            );
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
        let state = Some(
            if meta_user_ptr != 0
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
                // NR 5's canonical arm DOES store this one — see `handle_ipc_recv_timeout`'s
                // blocking branch, which derives the shape from args 4/5 exactly as this does.
                BlockedRecvState::legacy_timeout(cap, payload_user_ptr, payload_user_len)
            },
        );
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
        // U9-RECV-BLOCK1 §1(a) — NR 2's LEGACY shape is ADMITTED, and it leaves NO record.
        //
        // This arm used to refuse it (`reason=not_recv_v2`), which was a recognized receive
        // reaching the terminal acquisition for a shape the route can serve. It is served now.
        //
        // What it must NOT do is invent the record. `handle_ipc_recv`'s blocking arm stores
        // `BlockedRecvState` only inside `if recv_v2_request { … }`: a legacy NR 2 parks with
        // `blocked_recv_state` left `None`, publishes none of the three acknowledgements, and
        // answers `WouldBlock`. A sender that later finds this waiter therefore FAILS the
        // delivery — `complete_blocked_recv_for_waiter` opens with
        // `blocked_recv_state.take().ok_or(SyscallError::InvalidArgs)?`.
        //
        // An earlier form of this slice built `BlockedRecvState::legacy_timeout` here, reasoning
        // that NR 5's arm constructs it twelve lines above. NR 5's arm does, and correctly: NR 5's
        // own result owner derives the shape from the caller's arguments, so a parked NR 5 is owed
        // what those same arguments would have been owed had a message been waiting. NR 2's owner
        // does not, and storing one here would have made this route SUCCEED where the canonical
        // route fails — a repair dressed as a reproduction, and a silent divergence between the
        // two routes for a shape neither of them announces. `None` is what the canonical arm
        // leaves, so `None` is what this arm leaves.
        let state = match request.meta_target {
            RecvMetaTarget::V2 {
                ptr: meta_user_ptr,
                len: meta_user_len,
            } => Some(BlockedRecvState {
                recv_cap: cap,
                payload_user_ptr,
                payload_user_len,
                meta_user_ptr,
                meta_user_len,
                recv_abi: crate::kernel::task::RecvAbiVariant::RecvV2,
            }),
            _ => None,
        };
        (state, None, None)
    };
    // (4) Capability: task(2) pid read → capability(4) resolve, both off the broad lock. Every
    // refusal here has a canonical error the broad handler produces, so fall back and let it.
    let snapshot = match shared.resolve_endpoint_recv_cap_split_read(tid, cap) {
        Ok(snapshot) => snapshot,
        // U9-RECV-BLOCK2b §1 — the immediate lane resolved THIS capability through THIS reader
        // and its take ran, so the capability was revoked inside this syscall, after the take and
        // before the park. The canonical receive answers the resolver's own error for a receive
        // capability it cannot use; that is this answer, raised through the entering frame, which
        // is still current and still unmutated at this step. Bouncing back to the immediate lane
        // instead would re-resolve the same revoked capability and learn nothing new.
        Err(e) => {
            crate::yarm_log!(
                "IPC_RECV_BLOCK_SPLIT_SETTLED cpu={} tid={} reason=cap_revoked_after_take",
                cpu.0,
                tid
            );
            return BlockingLaneOutcome::complete(Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::from(e),
            )));
        }
    };
    let CapObject::Endpoint {
        index: endpoint_idx,
        generation,
    } = snapshot.endpoint
    else {
        // The resolver only ever answers `Ok` for an endpoint carrying RECEIVE. Established
        // impossible, and settled as such rather than handed back to a lane that has already run.
        return BlockingLaneOutcome::CannotPark(CannotParkReason::ImmediateLaneAlreadyAnswered(
            "resolved_non_endpoint",
        ));
    };
    // (5) U9-RECV-BLOCK2b §1 — THE WOULD-BLOCK READ IS GONE, and nothing replaces it.
    //
    // It asked, structurally, the question the immediate lane had already answered by ATTEMPTING
    // THE TAKE moments earlier — and asking it a second time is what created the window. The
    // interleaving it permitted, in full: A's take finds nothing and this lane is entered; B
    // enqueues; this read sees B's message and refuses `would_not_block`; C consumes it; the
    // re-run immediate take finds nothing again; and the settlement answered `WouldBlock` to a
    // receive that had asked for a finite wait whose deadline had not elapsed.
    //
    // The canonical owner has no such read. `ipc_recv_with_optional_deadline` calls
    // `ipc_recv_endpoint_take` and, on `Ok(None)`, goes straight to
    // `block_current_on_receive_with_deadline`. A sender arriving in that gap is caught at the
    // rank-3 waiter publication — `QueueNonEmpty` — which unwinds the park and continues into the
    // immediate owner for the message that raced in. That is step (8) below, it is a PUBLICATION
    // outcome rather than a pre-park guess, and it is the only place this route may learn that the
    // queue stopped being empty.
    // (6) ADMISSION — the last point at which falling back is safe.
    if crate::kernel::boot::d2_recv_dispatch_is_deferred(cpu_idx) {
        crate::yarm_log!(
            "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} reason=already_deferred",
            cpu.0,
            tid
        );
        return BlockingLaneOutcome::CannotPark(CannotParkReason::AlreadyDeferred);
    }
    // U9-RECV-BLOCK1 §4 — authenticated on the trap's own authority. The ambient
    // `CpuNotAuthoritative` comparison and the blanket `MultiCpu` refusal do not apply to a
    // caller that provably IS this CPU's trap; every other precondition is the shared body's and
    // is unchanged. This is what lets a blocking receive park on a boot with more than one
    // dispatching CPU, which the ambient contract refused outright.
    //
    // U9-RECV-BLOCK2 §1 — `IncomingUnavailable` is NOT a refusal for this family.
    //
    // Admission answers two different questions at once: "may this caller publish a queue
    // advance?" and "is there an incoming task this convention can resume?". `Ok(None)` — no
    // candidate at all — already proceeds to park, and the existing D2-recv drain settles that
    // CPU idle (`D2_RECV_GENUINE_IDLE_PROVENANCE_PUBLISHED`). Treating `IncomingUnavailable` as a
    // refusal is what made a receive that must block have nowhere to go, and the only remaining
    // destination was the terminal broad acquisition.
    //
    // U9-RECV-BLOCK2b §3 — WHY ignoring it is sound, corrected. The earlier argument here was
    // that a candidate the convention cannot resume "has the identical consequence" as no
    // candidate at all, so the drain owes the same idle. That did not follow, and the gap was
    // fatal rather than merely untidy:
    //
    //   * Admission classifies the RAW PEEKED HEAD (`peek_next_runnable_on`).
    //   * The drain selects through `queue_advance_select_step_split`, a FILTERED dequeue that
    //     skips unacceptable tasks — so it can land on a task admission never looked at.
    //   * That filter used to ask only whether the dispatch transition would be accepted and
    //     whether the ASID resolves. The APPLY asks
    //     `classify_incoming_resume_convention(.., ExactTokenResume)`, and a `None` there is
    //     `X86ResumeRefusal::Context` — raised after the dequeue and after the mark, with the
    //     scheduler already believing the task is running, leaving the shared D2 drain no move
    //     but `d2_resume_refused_fatal`.
    //
    // So "the drain will find no candidate" was never established by this refusal; what the
    // refusal permitted was the drain selecting a DIFFERENT candidate and taking the machine down
    // on it. The repair is in the selection owner, not here: that filter now asks the same
    // classifier the apply asks, so a task the apply would refuse is left in the queue instead of
    // being dequeued and fataled, and a drain that finds no acceptable candidate settles through
    // its existing typed idle. With selection and apply agreeing, admission's peek-based
    // `IncomingUnavailable` is a statement about a candidate the drain will not select, and
    // ignoring it costs this family nothing.
    if let Err(refusal) = shared.queue_advance_admit_with_authority_split(
        authority,
        crate::kernel::boot::QueueAdvanceApply::ExactTokenResume,
    ) && !matches!(
        refusal,
        crate::kernel::boot::QueueAdvanceRefusal::IncomingUnavailable
    ) {
        crate::yarm_log!(
            "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} reason={:?}",
            cpu.0,
            tid,
            refusal
        );
        return BlockingLaneOutcome::CannotPark(CannotParkReason::AdmissionRefused(refusal));
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
        return BlockingLaneOutcome::CannotPark(CannotParkReason::DeferUnavailable);
    }
    // (8) Phase A — scheduler rank 1. A victim mismatch unwinds its own single step inside the
    // twin, so this is still pre-mutation from the route's point of view.
    // U9-RECV-BLOCK1 §3 — a `None` here is PRE-MUTATION and stays so: the compare-and-clear
    // inside Phase A refuses without touching the slot, so the caller is still current and this
    // decline is as safe as the ones above it.
    #[allow(unused_variables)]
    let Some((receiver_asid, victim_priority)) = shared.recv_block_phase_a_split(cpu, tid) else {
        crate::kernel::boot::d2_recv_dispatch_clear(cpu_idx);
        crate::yarm_log!(
            "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} reason=phase_a",
            cpu.0,
            tid
        );
        return BlockingLaneOutcome::CannotPark(CannotParkReason::PhaseAVictimChanged);
    };
    // Phase B — task rank 2, with the state and deadline the ABI step decoded. For NR 2 the
    // deadline is `None` (it carries no timeout, which is what keeps the reply-deadline and oracle
    // arming the broad arm performs strict no-ops); for NR 5 it is the absolute tick, which is
    // what puts the parked receiver in front of the existing receive-timeout scan.
    let Some(wait_generation) =
        shared.recv_block_phase_b_split(tid, receiver_asid, cap, deadline, state)
    else {
        // U9-RECV-BLOCK1 §3 — the task half wrote NOTHING, so only Phase A's clear has to be
        // reversed. There is no wait generation to name yet, which is exactly why this one
        // failure reverses the scheduler half directly instead of going through the exact unwind:
        // the identity the unwind authenticates against does not exist.
        //
        // The restore is still exact — same CPU, same priority, and it refuses rather than
        // displace anything the scheduler chose in between.
        // U9-RECV-BLOCK2 §2 — through the ONE rank-1 recovery, which tries the exact restore,
        // falls back to the preempt-and-prefer primitive for the reachable `contains_tid`
        // refusal, and PROVES status and placement agree before it reports `Restored`. The
        // predecessor called `restore_current_exact_split` alone, so the reachable refusal —
        // a remote deadline scan having enqueued this task since Phase A — went straight to the
        // non-resumable arm below even though the task was trivially recoverable.
        let outcome = shared.restore_entering_incarnation_exact_split(
            cpu,
            crate::kernel::recv_waiter_split::RecvEnteringIncarnation {
                tid,
                asid: receiver_asid,
                priority: victim_priority,
            },
        );
        crate::kernel::boot::d2_recv_dispatch_clear(cpu_idx);
        crate::yarm_log!(
            "IPC_RECV_BLOCK_SPLIT_REFUSED cpu={} tid={} reason=phase_b restored={} outcome={}",
            cpu.0,
            tid,
            u8::from(outcome.may_resume_entering_frame()),
            outcome.slug()
        );
        // U9-RECV-BLOCK2 §2 — ONE answer, encoded into the entering frame, and the outcome
        // decides whether that frame is returned or handed to the bridge. Phase B failing at all
        // means the TCB vanished between Phase A's compare-and-clear and this call, or a u64 wait
        // generation overflowed; neither is constructible, so the answer is `Internal`.
        return BlockingLaneOutcome::Settled(recv_settle_after_unwind(
            cpu,
            frame,
            crate::kernel::recv_waiter_split::RecvEnteringIncarnation {
                tid,
                asid: receiver_asid,
                priority: victim_priority,
            },
            outcome,
            crate::kernel::syscall::SyscallError::Internal,
            "phase_b",
        ));
    };
    // U9-RECV-BLOCK1 §3 — THE identity every compensation below authenticates against. Built
    // once, from the four facts this transaction minted and nothing else: the authoritative
    // requester TID, the ASID it held when Phase A removed it, the placement Phase A's
    // compare-and-clear returned, and the wait generation Phase B advanced. A compensation that
    // matched on the numeric TID alone could overwrite a replacement incarnation, another
    // winner's status, or another transaction's pending work; with this it can only undo its own.
    let block_identity =
        |wait_generation: u64| crate::kernel::recv_waiter_split::RecvBlockIdentity {
            tid,
            asid: receiver_asid,
            priority: victim_priority,
            wait_generation,
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
            let unwound =
                shared.recv_block_unwind_exact_split(cpu, block_identity(wait_generation));
            crate::kernel::boot::d2_recv_dispatch_clear(cpu_idx);
            if !unwound.may_resume_entering_frame() {
                // U9-RECV-BLOCK2 §2 — the exact inverse could not put the entering incarnation
                // back, so the entering frame is not this task's to return through. The ANSWER is
                // still the one this arm would have given — `WouldBlock`, the retry lane both
                // races settle on when they cannot dequeue — and it is encoded into the frame so
                // that a bridge which captures that frame completes the syscall rather than
                // leaving a task to resume past its receive with no result.
                return BlockingLaneOutcome::Settled(recv_settle_after_unwind(
                    cpu,
                    frame,
                    block_identity(wait_generation).entering(),
                    unwound,
                    crate::kernel::syscall::SyscallError::WouldBlock,
                    if busy {
                        "waiter_ownership_busy_unwound"
                    } else {
                        "queue_non_empty_unwound"
                    },
                ));
            }
            // U9-RECV-BLOCK1 §3 — the exact incarnation is current again, so this transaction
            // may answer. The two races answer DIFFERENTLY, and both answers are the broad
            // owner's rather than this route's:
            //
            // * `QueueNonEmpty` — a sender enqueued after step (5). `recv_block_unwind_race`
            //   wakes and redispatches, and `ipc_recv_with_optional_deadline` then runs its
            //   PHASE 2 post-wake `ipc_recv_endpoint_take`, delivering that message. So the
            //   canonical continuation is a RECEIVE, not a decline — and this route continues
            //   into the immediate receive owner, which is the same engine that phase-2 dequeue
            //   is. Handing it to the broad dispatcher instead would have been a second trip
            //   through a syscall that is already mid-flight.
            // * `WaiterOwnershipBusy` — the queue is empty and an in-flight direct transaction
            //   holds the previous incarnation. `block_current_on_receive_with_deadline` is THE
            //   owner of that policy and answers `WouldBlock`, deliberately not the
            //   `QueueNonEmpty` answer, so the receiver retries instead of dequeuing nothing.
            crate::yarm_log!(
                "IPC_RECV_BLOCK_SPLIT_SETTLED cpu={} tid={} endpoint={} reason={} outcome={}",
                cpu.0,
                tid,
                endpoint_idx,
                if busy {
                    "waiter_ownership_busy"
                } else {
                    "queue_non_empty"
                },
                unwound.slug()
            );
            if busy {
                return BlockingLaneOutcome::complete(recv_encode_empty_answer(
                    frame,
                    crate::kernel::syscall::SyscallError::WouldBlock,
                ));
            }
            // Continue into the immediate receive owner for the message that raced in, through
            // the same one-call lane the family entry drives — not a second copy of its decode.
            return BlockingLaneOutcome::complete(
                match try_split_recv_immediate_lane(shared, cpu, frame) {
                    RecvImmediateOutcome::Answered(result) => result,
                    // U9-RECV-BLOCK2b §1 — THE POST-WAKE EMPTY TAKE, which is a different fact
                    // from the pre-parking one and settles differently.
                    //
                    // The racing message went to somebody else between the unwind and here. This
                    // receive has already been through a park and a wake, so the canonical
                    // continuation is `ipc_recv_with_optional_deadline`'s PHASE 2: it calls
                    // `ipc_recv_endpoint_take` once after the wake and returns whatever that take
                    // gives it — `Ok(None)` included, ending the syscall with the empty answer.
                    // It does NOT re-park, and it does not wait for the deadline to elapse first.
                    // So the empty answer here is the canonical post-wake completion, not the
                    // pre-parking substitute §1 removed from the entry path.
                    RecvImmediateOutcome::EmptyAwaitingPark => recv_encode_empty_answer(
                        frame,
                        crate::kernel::syscall::SyscallError::WouldBlock,
                    ),
                    // Established impossible for the same reasons `settle_no_immediate_take`
                    // records: the shape and the syscall number are the ones this same lane
                    // decoded, and this task is current again because the unwind put it back.
                    RecvImmediateOutcome::NoTakeAttempted(reason) => {
                        crate::yarm_log!(
                            "IPC_RECV_SPLIT_INVARIANT cpu={} tid={} reason={} phase=post_wake result=failed_closed",
                            cpu.0,
                            tid,
                            reason
                        );
                        Err(TrapHandleError::Syscall(
                            crate::kernel::syscall::SyscallError::Internal,
                        ))
                    }
                },
            );
        }
        // `ReceiverAlreadyWaiting` and `InvalidEndpoint`. The live publish policy is canonical
        // last-receiver-wins and never returns the first; step (5) validated index and generation
        // under the same rank-3 lock, so neither is reachable. The canonical owner answers
        // `WrongObject` for both — and notably does NOT unwind, leaving the task Blocked. This
        // route unwinds first, because returning `WrongObject` through the entering frame is only
        // licensed once the exact incarnation is current again.
        _ => {
            let unwound =
                shared.recv_block_unwind_exact_split(cpu, block_identity(wait_generation));
            crate::kernel::boot::d2_recv_dispatch_clear(cpu_idx);
            crate::yarm_log!(
                "IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED cpu={} tid={} phase=publish outcome={}",
                cpu.0,
                tid,
                unwound.slug()
            );
            // U9-RECV-BLOCK2 §2 — the two answers are not two errors, they are an answer and a
            // non-answer. `WrongObject` through a resumable frame is the canonical outcome;
            // `Internal` through a frame that is NOT this task's would return into a task the
            // scheduler has parked, which is the contradiction this slice removes.
            // U9-RECV-BLOCK2 §2 — ONE answer, encoded first. `WrongObject` is what the canonical
            // owner raises for both `ReceiverAlreadyWaiting` and `InvalidEndpoint`, and it is the
            // answer whether or not the unwind could put this incarnation back; what the outcome
            // decides is where the answered frame goes.
            return BlockingLaneOutcome::Settled(recv_settle_after_unwind(
                cpu,
                frame,
                block_identity(wait_generation).entering(),
                unwound,
                crate::kernel::syscall::SyscallError::WrongObject,
                "publish_refused",
            ));
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
            // U9-RECV-BLOCK2 §2 — this is the ONE post-PUBLICATION failure in the route, so it
            // owes more than the task unwind. Phase C published this receiver as the endpoint's
            // waiter; unwinding the task without retracting that record would leave a published
            // waiter naming a receiver that is no longer blocked, and a sender finding it would
            // consume a message into a delivery `complete_blocked_recv_for_waiter` then refuses.
            // The retraction is identity-keyed through the existing rank-3 owner, so a competitor
            // that displaced this waiter under last-receiver-WINS is never detached.
            shared.retract_recv_waiter_exact_split(endpoint_idx, tid, receiver_asid);
            let unwound =
                shared.recv_block_unwind_exact_split(cpu, block_identity(wait_generation));
            crate::kernel::boot::d2_recv_dispatch_clear(cpu_idx);
            crate::yarm_log!(
                "IPC_RECV_BLOCK_SPLIT_SETTLED cpu={} tid={} endpoint={} reason=deadline_reservation outcome={}",
                cpu.0,
                tid,
                endpoint_idx,
                unwound.slug()
            );
            // U9-RECV-BLOCK1 §3 — the terminal armed but its deadline token could not be
            // reserved, so parking would leave a caller holding a deadline it cannot identify.
            // The whole block is reversed and the receive answers as a receive that could not
            // proceed: `WouldBlock`, the same lane the ownership-busy race answers on, and the
            // one the caller's own retry loop already handles. This is capacity contention on a
            // bounded store, not a fault, so it is not an error return.
            //
            // U9-RECV-BLOCK2 §2 — that answer is encoded whether or not the unwind could put the
            // incarnation back, and the outcome decides where the answered frame goes.
            return BlockingLaneOutcome::Settled(recv_settle_after_unwind(
                cpu,
                frame,
                block_identity(wait_generation).entering(),
                unwound,
                crate::kernel::syscall::SyscallError::WouldBlock,
                "deadline_reservation",
            ));
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
    //
    // U9-RECV-BLOCK1 §1(a) — `if let Some(state)`, because the broad arm's three publish calls
    // are nested INSIDE `if recv_v2_request { … }`. A legacy NR 2 parks with no writeback record
    // and publishes nothing; every one of these bodies would refuse it on the recv-v2 term
    // anyway, but refusing inside a body is not the same claim as never calling it, and the
    // acknowledgement stores are not things to call speculatively.
    if let Some(state) = state {
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
        let server_ack = crate::kernel::boot::publish_ipccall_direct_blocked_server_ack_with(
            tid,
            asid,
            endpoint,
            &state,
            live_waiter,
        );
        // U9-RECV-BLOCK1 §2 — the AUTHORITATIVE cross-CPU blocked-server marker, from the same
        // committed point and through the same body the broad arm now drives. This was the last
        // step that demanded a broad `&KernelState`, for five reads and nothing else; carrying
        // those five as facts is what lets the route publish the evidence instead of yielding the
        // whole receive to produce it. Every condition, its conjunction, the one-shot fuse and
        // the marker text are the body's, unchanged — this side only answers the reads.
        #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
        if let Some((ack_index, ack_generation, ack_seq)) = server_ack {
            let (saved_frame, home_cpu) = shared.smp_blocked_server_task_facts_split_read(tid);
            crate::kernel::boot::emit_ipccall_direct_smp_server_blocked_with(
                tid,
                ack_index,
                ack_generation,
                ack_seq,
                crate::kernel::boot::SmpServerBlockedFacts {
                    saved_frame,
                    present_in_any_runqueue: shared
                        .receiver_has_scheduler_membership_split_read(tid),
                    home_cpu,
                    asid,
                    waiter: live_waiter(ack_index),
                },
            );
        }
        let _ = server_ack;
        let _ = crate::kernel::boot::publish_ipcreply_direct_blocked_caller_ack_with(
            tid,
            asid,
            endpoint,
            &state,
            live_waiter,
        );
        // U9-RECV-BLOCK1 §2 — and the shared-region oracle's acknowledgement, from the SAME
        // committed point, through the same shared body the broad arm now drives. Until this
        // existed the route yielded the whole receive on a COMPILE-TIME feature term, so an
        // oracle-enabled build sent every ordinary NR 2 blocking receive to the terminal
        // acquisition for work none of them had anything to do with.
        #[cfg(feature = "shared-region-direct-oracle")]
        crate::kernel::boot::publish_shared_region_blocked_recv_ack_with(
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
    BlockingLaneOutcome::Settled(D::QueueAdvanceCommitted)
}

#[cfg(feature = "hosted-dev")]
fn try_split_blocking_ipc_recv_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _cpu_idx: usize,
    _tid: u64,
    _frame: &mut TrapFrame,
    _authority: crate::runtime::DispatchAuthority,
) -> SplitDispatchDisposition {
    SplitDispatchDisposition::NotHandled
}

/// U9-TM §2 / U9-TIMER1 §2 — service an ORDINARY `TimerInterrupt` off the broad lock.
///
/// U9-TM shipped the non-preempting half and recorded the other half as unprovable: no profile
/// witnessed a timer-driven preemption (zero `preempt=1` across 77 recorded profiles), so the
/// preempting branch declined to the terminal broad dispatcher. U9-TIMER1 establishes that the
/// zero was an observability limit — the quantum and the hardware deadline were one constant in
/// incompatible units, putting a preempting tick millions of interrupts out of reach on two ports
/// — separates them behind a default-off boot knob (§3), and converts the branch.
///
/// # The three outcomes this route now owns
///
/// | tick | this CPU | what runs |
/// |---|---|---|
/// | preempting | has a current task | `run_yield_transaction`, then tick/claim/re-arm, then `QueueAdvanceCommitted` — the post-lock drain performs the switch |
/// | preempting | idle, run queue empty | tick/claim/re-arm, then `PostWorkCommitted` — there is nothing to preempt, and the broad arm's selection provably answers `None` for this state |
/// | non-preempting | either | the unchanged atomic no-switch seam, tick/claim/re-arm, `PostWorkCommitted` |
///
/// In every one of them: exactly ONE tick through the single `SchedulerTimer` policy; claim/ack
/// and re-arm through the SAME lock-free adapters the `Hal` methods delegate to, with the same
/// deadline constant the broad arm passes (on RISC-V the single SBI `set_timer` is itself the
/// completion, and PLIC source 0 is never touched); and no broad acquisition.
///
/// # What still declines to the broad arm
///
/// Each is taken BEFORE anything is claimed, ticked or mutated, so a decline reaches the
/// unchanged broad arm having changed exactly nothing:
///
/// 1. **the yield transaction declines** — `ArchGateOff`, `DeferralHeld`, `NotRunning`,
///    `ReenqueueRefused`, and the topology refusals (`CpuOutOfRange`, `NoTrapDrainer`,
///    `MultiDispatcher`). The broad arm performs the identical decision through `yield_current`,
///    which drives the SAME transaction through `BroadYieldOwners`.
///
/// The two entries that stood beside it are gone rather than moved. U9-TIMER3 and U9-TIMER4
/// relocated all five `maybe_run_*` diagnostic hooks to the first-dispatch driver, so no proof
/// knob is a reason to decline; and U9-TIMER5 gives the **idle CPU with a non-empty run queue**
/// its landing on every port, so that population commits `TimerIdleQueueAdvance` here instead of
/// declining under `no_user_return_path`.
/// U9-TIMER5 §1/§2 — EVERY PORT NOW HAS AN IDLE-BOUNDARY LANDING, so this route no longer asks
/// which one does.
///
/// U9-TIMER2 asked the question through `IDLE_BOUNDARY_TIMER_CAN_RESUME_USER`, a constant that
/// was true only on RISC-V, and refused the queued-work settlement on the other two under
/// `reason=no_user_return_path`. The constant is retired here together with its refusal, because
/// the thing it reported absent has been built:
///
/// * **RISC-V — unchanged.** `riscv_s_mode_timer_trap` is a real trap ENTRY: it builds a minimal
///   `TrapFrame`, runs the arch-neutral pipeline, and then, when a task became current during the
///   trap, clears `SPP`, sanitizes `sstatus`, populates the hardware frame from that task's saved
///   context, selects one of its three resume conventions and activates the ASID
///   (`RISCV_S_MODE_TIMER_DISPATCH`). It CONSTRUCTS a user return rather than converting a kernel
///   one, which is why it needs no authentication and is not touched by U9-TIMER5.
///
/// * **x86_64 and AArch64 — built by U9-TIMER5 §2.** Both idle landings are non-returning kernel
///   halt loops (`idle_halt_loop`'s ring-0 `sti; hlt`, `idle_no_eret_loop`'s EL1 `wfi`), and a
///   timer taken there interrupts KERNEL state. The shared bridge now performs the authoritative
///   advance for that trap and hands the architecture tail a debt, and each tail establishes the
///   user frame it owns: x86_64 rewrites the full `iretq` frame — `CS`/`SS` at DPL 3, user `RIP`
///   and `RSP`, `RFLAGS` with `IF` set — and AArch64 rewrites `SPSR_EL1` to `EL0t` alongside the
///   `ELR_EL1`/`SP_EL0` its epilogue already restores.
///
/// # Why the decision is not made here any more
///
/// It could not be. What separates a resumable idle-boundary timer from an unresumable kernel
/// interruption is not the port — it is whether THIS trap interrupted the authenticated boundary,
/// which only `crate::kernel::idle_boundary` knows and only the bridge can ask. `current == None`
/// does not answer it: the boot path before first dispatch and the window between a terminal
/// transition and its drain are both `current`-empty kernel states, and converting a frame taken
/// in one of those would resume a task on kernel code's continuation.
///
/// So this route keeps exactly the part it owns — one tick, one claim, one re-arm, and the
/// observation that work is queued — and the bridge decides whether a landing exists for this
/// particular trap. The settlement below is therefore `TimerIdleQueueAdvance` on every port, and
/// an unauthenticated trap is settled by the bridge under its own name rather than by a
/// port-wide constant here.
/// U9-TIMER-FINAL §2 — what a RECOGNIZED timer interrupt settles as. There is no `NotHandled`
/// here, and that absence is the whole point of the type.
///
/// Every earlier stage left the recognized timer body returning [`SplitDispatchDisposition`],
/// whose `NotHandled` is an entry into the terminal broad dispatcher. Each stage then argued that
/// its particular `NotHandled` was rare, or unreachable, or someone else's. This type removes the
/// argument: a recognized timer cannot ask for broad handling because it has no way to say so.
/// The family filter that rejects a NON-timer event still exists, and still answers `NotHandled` —
/// but it runs in [`try_split_timer_dispatch`], outside this body, where "this is not a timer" is
/// the only thing it can mean.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimerSettlement {
    /// The preempting transaction COMMITTED: the interrupted task is `Runnable`, re-enqueued at
    /// its priority tail exactly once, `current` is clear, and the one-shot Yield deferral carries
    /// its identity. The bridge's existing queue-advance drain owes the switch.
    QueueAdvanceCommitted,
    /// An IDLE-BOUNDARY tick. Nothing was preempted and nothing was published, because there was
    /// no current task; what is owed is one authoritative queue advance with no outgoing side.
    IdleQueueAdvance,
    /// The current task CONTINUES on this CPU. The tick, the claim and the re-arm have happened;
    /// no scheduler state changed, or what changed was rolled back exactly.
    ///
    /// This is the settlement for every refusal, and it is a real outcome rather than a
    /// disguised failure: a quantum boundary that cannot switch simply extends the quantum. The
    /// interrupted task is still `Running`, still `current`, and still owns the frame this trap
    /// will return through — which is precisely why returning to it is safe.
    ContinueCurrent,
    /// **THE ONE SETTLEMENT THAT HANDS A RECOGNIZED TIMER AWAY, and the honest residual of
    /// U9-TIMER-FINAL.**
    ///
    /// It exists because on x86_64 the `yarm.d6_switch_proof` / `yarm.d6_switch_a` knobs make an
    /// ACTIVE diagnostic the owner of the switch path, and in that mode the only code that can
    /// complete a preempting timer is the broad in-lock dispatch inside `yield_current`. §2
    /// forbids disabling a supported knob to manufacture closure, and forbids a dropped
    /// preemption — and settling this population as [`Self::ContinueCurrent`] would be exactly a
    /// dropped preemption, because under those knobs EVERY tick declines identically, so the
    /// switch would never happen at all rather than happening one tick later.
    ///
    /// Three properties make it a residual rather than a hole:
    ///
    /// * **It is decided BEFORE any work.** `settle_recognized_timer` returns it as its first
    ///   act, so no tick, acknowledgement or re-arm has been taken and the broad arm performs all
    ///   three exactly once — the same sequence it performed before this package.
    /// * **It is unreachable in production.** Both predicates are default-off boot knobs, and the
    ///   gate is additionally `cfg(target_arch = "x86_64")`. With no knob armed,
    ///   `d6_genuine_enabled()` is true and this settlement cannot be produced.
    /// * **Its removal has a named prerequisite**, which is D6's and not the timer's: the
    ///   `DispatchSwitchPlan` stash is produced from inside the broad acquisition, so a split
    ///   route cannot publish one without becoming a SECOND switch owner in the one mode whose
    ///   entire contract is that there is exactly one. Re-homing that production onto the split
    ///   seams is what would retire this variant.
    DiagnosticSwitchOwner,
}

#[cfg(not(feature = "hosted-dev"))]
impl TimerSettlement {
    /// How the bridge sees it. Total, and deliberately written as a `match` over a closed enum so
    /// a new settlement cannot be added without choosing a disposition for it.
    fn disposition(self) -> SplitDispatchDisposition {
        match self {
            Self::QueueAdvanceCommitted => SplitDispatchDisposition::QueueAdvanceCommitted,
            Self::IdleQueueAdvance => SplitDispatchDisposition::TimerIdleQueueAdvance,
            // The tick and the re-arm are done and no scheduler state changed, so the broad
            // dispatcher must be skipped — entering it would tick a SECOND time — while the
            // architecture tail still runs the production timeout pipeline.
            Self::ContinueCurrent => SplitDispatchDisposition::PostWorkCommitted {
                finalize_syscall: false,
            },
            // The ONE broad hand-off, and it is taken before any work — see the variant's own
            // documentation for why it is a named residual rather than an escape.
            Self::DiagnosticSwitchOwner => SplitDispatchDisposition::NotHandled,
        }
    }
}

/// Does an ACTIVE diagnostic own this architecture's switch path?
///
/// The exact complement of `d6_genuine_enabled()`, which is what
/// [`crate::kernel::syscall::yield_txn::yield_deferral_arch_gate`] tests on x86_64 — so asking it
/// here answers "would the transaction decline with `ArchGateOff`, for the D6 reason?" WITHOUT
/// running the transaction, which is what lets the hand-off be taken before the tick.
///
/// It is deliberately NOT the whole of `ArchGateOff`. The other spelling of that decline is
/// `not_bsp` on AArch64 and RISC-V, which no timer reaches (no AP arms a timer on any port) and
/// which would in any case be wrong to hand to the broad arm: a non-bootstrap CPU has no
/// diagnostic owner waiting for it.
#[cfg(not(feature = "hosted-dev"))]
fn diagnostic_owns_switch_path() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        crate::kernel::boot::d6_controlled_switch_proof_enabled()
            || crate::kernel::boot::d6_switch_a_enabled()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// U9-TIMER-FINAL §2 — THE RECOGNIZED TIMER BODY. One tick, one claim, one re-arm, and exactly
/// one of three settlements — plus, before any of that, one hand-off that exists only while a
/// default-off diagnostic owns the switch path.
///
/// # The tick is taken FIRST, and that is a correction
///
/// Every stage up to U9-TIMER5 ran the yield transaction BEFORE the tick, so that a refusal could
/// hand an untouched CPU to the broad arm. That ordering was load-bearing only while a refusal had
/// somewhere to go. It does not any more, and keeping it cost two things:
///
/// * a SECOND read of the quantum. The route asked `timer_would_preempt_split_read` and the tick
///   seam asked `would_preempt_next()` again, in a separate rank-1 acquisition. `Timer`'s own
///   contract says the lookahead and the tick "must happen under one acquisition, or the answer is
///   stale", and between the route's two acquisitions another dispatching CPU can call
///   `reset_quantum` — which is reachable on x86_64 under `yarm.ap_user_dispatch=1`. The route
///   carried a `reason=would_preempt` fail-safe for exactly that disagreement, and answered it
///   with `NotHandled`.
/// * an ordering obligation that no longer buys anything: a decline after the tick was said to
///   force a choice "between double-ticking in the broad arm and silently dropping a quantum's
///   preemption". With no broad arm in the picture, neither horn exists.
///
/// So the tick is now the ONE authority on whether this interrupt preempts. `scheduler_tick_split
/// _mut` ticks and reports, in a single acquisition, and the body branches on its report. The
/// lookahead is gone, and with it the discrepancy it existed to detect: there is nothing left for
/// two reads to disagree about.
///
/// # Every refusal settles here
///
/// `run_yield_transaction` is the one scheduling policy, and this body drives it through the same
/// `SharedYieldOwners` the split NR 0 does. What changed is where its refusals go. Each is settled
/// as `ContinueCurrent`, and each is settleable that way because of a property the transaction
/// itself guarantees — every decline is either pre-mutation or exactly reversed:
///
/// | decline | post-state | why continuing is correct |
/// |---|---|---|
/// | `ArchGateOff` (`not_bsp`) | nothing written | a non-bootstrap CPU, which arms no timer on any port — unreachable, and listed for totality |
/// | `DeferralHeld` | nothing written | a NAMED cell is set and a NAMED drain consumes it — see below |
/// | `RouteNotAdmitted` | nothing written | no drainer, or a CPU with no cells |
/// | `NotRunning` | reservation released, no field written | **fail-closed, for every verdict**, exactly as the delivered broad path was. The entering incarnation's frame authority is read only to name WHICH state the fatal report should cite. See below |
/// | `ReenqueueRefused` | `current` restored by the primitive, rank-2 transition rolled back through its named inverse | the caller is `Running` and `current` again |
///
/// `NoCurrent` is not in that table because it is not a refusal: it means the CPU is at its
/// idle boundary, and it has its own settlement.
///
/// # U9-PAGEFAULT1 §0 — `DeferralHeld` names its obligation
///
/// "Some deferral exists" is not a reason to continue: continuing is correct only if something
/// will actually consume what is already published. `colliding_deferral_pending_for` has THREE
/// producers and they are not equivalent:
///
/// | producer | the cell | the drain that consumes it |
/// |---|---|---|
/// | `yield_dispatch_is_deferred(cpu)` | the one-shot Yield deferral | the post-lock Yield drain in `arch/trap_entry.rs` (RISC-V: its own bridge), which is running in THIS trap |
/// | `futex_wait_dispatch_is_deferred(cpu)` — RISC-V only | the NR 9 deferral | the same RISC-V tail, which drains all three cells |
/// | `riscv_queue_switch_foundation_is_deferred(cpu)` — RISC-V only | the 196D foundation deferral | the same RISC-V tail |
/// | `cpu_idx >= MAX_CPUS` | **no cell at all** | **nothing** — see below |
///
/// The first three are genuine: the publishing route reserved a one-shot cell in the same trap
/// this timer interrupted, and the architecture tail this trap returns through is the drain. The
/// interrupted task is `Running` and `current` because the transaction wrote nothing, so
/// continuing hands the frame back to the route that owns the pending switch.
///
/// The fourth is NOT a deferral and must not be justified as one. An out-of-range CPU index makes
/// the predicate answer `true` because there are no cells to consult — the same fact
/// `RouteNotAdmitted(CpuOutOfRange)` reports, wearing the wrong name. Continuing is still correct,
/// for that reason and not for "a drain will run": nothing was written, and this CPU has no
/// per-CPU state for anything to be owed to. It is unreachable from a timer in any case, because
/// step (1)'s `scheduler_tick_split_mut` and the `current_tid_authoritative` read that precedes
/// the gate both validate the CPU first.
///
/// # U9-PAGEFAULT1 §0 — `NotRunning` is asked of the owners, not inferred
///
/// A pre-mutation refusal proves the WORLD is untouched. It does not prove the FRAME may be
/// returned through, and those are different predicates: the CPU is about to `iret`/`eret` into
/// whatever `current` names. **"The TCB exists" and even "the task is live" are not the second
/// predicate.**
///
/// ## What base did, and what this route now does
///
/// The delivered broad path answered this refusal with `Err(KernelError::TaskMissing)`, the broad
/// timer arm propagated it with `?`, and BOTH ISRs treat that as fatal — x86_64 `halt_forever()`,
/// AArch64 a `wfe` loop. So base halted for **every** status. **This route does the same**, and
/// that is a deliberate reversal of an earlier draft.
///
/// That draft continued when the entering incarnation was still this CPU's `current`, and cited
/// `recv_block_unwind_exact_split` as precedent. **The precedent does not hold.**
/// `RecvUnwindOutcome::Restored` requires the TCB to say `Running`, established by an
/// exact-incarnation `apply_task_transition` COMMIT after the rank-1 restore, and
/// `restore_entering_incarnation_exact_split` records why in its own words: *"A task whose TCB
/// says `Runnable` is, to every dispatch transition in the tree, a task that has not been selected
/// to run. Reporting `Restored` for it permitted userspace execution from a status that says
/// otherwise."* That was a defect U9-RECV-BLOCK2 §2 closed — not a pattern to copy.
///
/// The timer route commits nothing, so returning through a `Runnable` task's frame is the state
/// that documentation calls out rather than the state it permits. Buying the continue with a
/// `Runnable → Running` commit would put a mutation into a route whose whole contract is that a
/// decline writes nothing. So the optional change is dropped.
///
/// ## The authority read is DIAGNOSTIC, and is justified as that
///
/// `entering_frame_authority_split_read` no longer decides anything. It composes the two owners
/// that already answer the question — `task_incarnation_is_resumable_split_read` for the exact
/// `{tid, asid}` and a `Runnable | Running` status, and `SmpScheduler::placement_of` for the
/// actual placement — so the fatal report names WHICH of six states the CPU was in:
/// `owns_entering_frame`, `incarnation_not_resumable`, `current_on_another_cpu`,
/// `second_placement`, `queued_for_dispatch` or `placed_nowhere`.
///
/// That is worth a read on a path that is about to halt the kernel: "task missing" is what base
/// printed, and it does not distinguish a blocked task from a replacement incarnation from a task
/// another CPU is running. It is not worth a behaviour change, which is the distinction this
/// section exists to keep.
///
/// The incarnation is still captured at step (0b), before the tick, for the same reason it always
/// was: a re-read at the refusal would name whatever holds the numeric TID by then.
///
/// What `ContinueCurrent` explicitly is NOT: it is not a fabricated success (the switch did not
/// happen and nothing claims it did), it is not a panic (contention is normal), and it is not a
/// syscall error — a timer has no caller to answer, and handing one an error code would describe
/// an unfinished SCHEDULING transition as a failed SYSCALL. The preemption is deferred to the
/// next tick, which is the same thing the quantum does for a task that yields early.
///
/// # `ArchGateOff` is an ACTIVE gate, and is kept
///
/// On x86_64 the gate is `!d6_genuine_enabled()`, true while `yarm.d6_switch_proof` or
/// `yarm.d6_switch_a` is armed. It is not a stale restriction: under those two knobs the Yield
/// DRAIN in `arch/trap_entry.rs` is disabled by the same predicate, because the switch path is
/// owned by the `DispatchSwitchPlan` stash instead. Publishing a deferral there would strand the
/// caller behind a drain that cannot run, and draining it here would make this route a SECOND
/// switch owner in a mode whose whole purpose is that there is exactly one.
///
/// So the gate stays — and, MEASURED rather than assumed, it does not settle as `ContinueCurrent`
/// either. Under `D6_SWITCH_A=1 yarm.sched_quantum_ticks=1`, every one of 74 preempting ticks
/// declines for this same reason, so "the next tick will preempt" is false: continuing locally
/// would drop the preemption permanently rather than defer it, and the boot stops making progress
/// (base reaches `KSPAWN_ENTER`, a `ContinueCurrent` head does not). That is what §2 forbids when
/// it says normal contention must not become a dropped preemption, and it is why this one
/// population is handed to the diagnostic's own owner through
/// [`TimerSettlement::DiagnosticSwitchOwner`], before any work is taken.
///
/// On AArch64 and RISC-V the gate is `!is_bootstrap_cpu(cpu)`, and it is UNREACHABLE for a timer:
/// no AP arms a timer on either port. `start_bsp_periodic_timer` programs only the bootstrap CPU,
/// `yarm_aarch64_secondary_cpu_boot` records that "APs do NOT arm a timer" because
/// `SchedulerState.timer` is one shared counter, and RISC-V parks its secondary harts outright.
/// Measured rather than inferred: in the x86_64 `-smp 2` AP profile every `TIMER_SPLIT_*` marker
/// carries `cpu=0`.
///
/// # What is still handed away, and where
///
/// Exactly two things, and neither is a recognized production timer:
///
/// 1. `!is_timer`, in `try_split_timer_into_frame`. That is the family filter — an event that is
///    not a timer at all — and it is not a timer escape.
/// 2. [`TimerSettlement::DiagnosticSwitchOwner`], from step (0) below, reachable only on x86_64
///    with `yarm.d6_switch_proof=1` or `yarm.d6_switch_a=1`. Both default off, so with no knob
///    armed this body has no path to `NotHandled` at all.
///
/// **Acceptance, stated exactly:** no recognized PRODUCTION `TimerInterrupt` can reach the
/// terminal broad acquisition. The residual is a default-off diagnostic, and its prerequisite for
/// removal is D6's own: re-home `DispatchSwitchPlan` production onto the split seams.
#[cfg(not(feature = "hosted-dev"))]
fn settle_recognized_timer(shared: &SharedKernel, cpu: CpuId) -> TimerSettlement {
    // ── (0) THE DIAGNOSTIC SWITCH OWNER — decided FIRST, and that position is the contract ───
    //
    // Under `yarm.d6_switch_proof` / `yarm.d6_switch_a` the switch path belongs to the
    // `DispatchSwitchPlan` stash, produced from inside the broad acquisition. A preempting tick
    // in that mode can only be completed there, so this body hands the whole interrupt over
    // untouched.
    //
    // UNTOUCHED is the point. If this test sat after the prologue, the broad arm would tick,
    // acknowledge and re-arm a SECOND time — §2's exactly-once obligation, broken by the very
    // hand-off meant to preserve the diagnostic. Deciding it here means the broad arm services
    // the interrupt exactly as it did before this package, byte for byte.
    //
    // It also cannot be replaced by settling `ArchGateOff` locally as `ContinueCurrent`: under
    // these knobs EVERY tick declines for the same reason, so "the next tick will preempt" is
    // false and the preemption would be dropped forever rather than deferred.
    if diagnostic_owns_switch_path() {
        crate::yarm_log!(
            "TIMER_SPLIT_DIAGNOSTIC_SWITCH_OWNER cpu={} reason=d6_genuine_off ticked=0 rearm=0 settlement=broad_owner",
            cpu.0
        );
        return TimerSettlement::DiagnosticSwitchOwner;
    }

    // ── (0b) THE ENTERING INCARNATION, captured once, before anything is decided ─────────────
    //
    // U9-PAGEFAULT1 §0. The frame this trap will return through belongs to whoever was executing
    // when the interrupt arrived, so the identity that authorizes that return has to be read HERE
    // — before the tick, before the transaction, before any window in which another CPU could
    // replace it. Re-reading it at the refusal would compare a later observation against itself
    // and could authenticate against a replacement task that reused the numeric TID.
    //
    // `Asid(0)` is the normalization the exact-incarnation owner itself uses for a task with no
    // address space, so a kernel-task entry compares equal to what it was entered with.
    let entering_tid = shared.current_tid_split_read(cpu).unwrap_or(u64::MAX);
    let entering_asid = shared
        .task_asid_option_split_read(entering_tid)
        .unwrap_or(crate::kernel::vm::Asid(0));

    // ── (1) THE TICK, and it is the authority ────────────────────────────────────────────────
    //
    // One acquisition, which both advances the quantum and reports whether this interrupt
    // preempts. Nothing else in this body reads the quantum, so nothing can disagree with it.
    let ticked = shared.scheduler_tick_split_mut(cpu);
    let (tick, preempting) = match ticked {
        crate::runtime::SchedulerTickOutcome::Preempt { tick, .. } => (tick, true),
        crate::runtime::SchedulerTickOutcome::NoSwitch { tick, .. } => (tick, false),
    };

    // ── (2) CLAIM/ACK and (3) RE-ARM — the same free functions the `Hal` methods delegate to,
    // and the same deadline constant the broad arm passes. Taken here, once, for every settlement
    // below: they are what this INTERRUPT earns, and they do not depend on what the scheduler then
    // decides. On RISC-V the single SBI `set_timer` is itself the completion.
    crate::arch::hal_adapters::acknowledge_interrupt(cpu, 0);
    crate::arch::hal_adapters::program_timer_deadline(
        cpu,
        crate::arch::platform_constants::BOOTSTRAP_TIMER_DEADLINE_TICKS,
    );

    // ── (4) A NON-PREEMPTING TICK ────────────────────────────────────────────────────────────
    if !preempting {
        // U9-TIMER5 §2 — a parked CPU is examined on every tick, not once per quantum. The
        // quantum decides whether to cut a RUNNING task short; with `current` empty it has no
        // subject, which is why the broad arm's own `yield_current` takes `NoCurrent` and
        // dispatches for this state. Scoped to the two ports whose bridge authenticates the idle
        // boundary — see the U9-TIMER5 record for the RISC-V derivation.
        #[cfg(not(target_arch = "riscv64"))]
        if !matches!(shared.current_tid_split_read(cpu), Some(tid) if tid != 0) {
            crate::yarm_log!(
                "TIMER_SPLIT_IDLE_ADVANCE_COMMITTED cpu={} tick={} observed_runnable={} preempt=0 rearm=1 broad_lock=0",
                cpu.0,
                tick,
                shared.runnable_count_on_cpu_split_read(cpu)
            );
            return TimerSettlement::IdleQueueAdvance;
        }
        crate::yarm_log!(
            "TIMER_SPLIT_TICK_OK cpu={} tick={} preempt=0 rearm=1",
            cpu.0,
            tick
        );
        return TimerSettlement::ContinueCurrent;
    }

    // ── (5) A PREEMPTING TICK ────────────────────────────────────────────────────────────────
    //
    // A preempting timer IS a yield with a different provenance: it re-enqueues the running task
    // at its priority tail, clears `current`, and defers the selection to the post-lock drain —
    // exactly what NR 0 publishes. So it drives the SAME transaction through the SAME owners, and
    // no second scheduling policy is introduced: the arch gate, the topology admission, the
    // deferral reservation, the exact `Running -> Runnable` transition and its inverse are all the
    // ones NR 0 already uses.
    //
    // What it must NOT do, and does not: encode a syscall return. NR 0 finishes with
    // `frame.set_ok(0, 0, 0)` because a yield is a syscall whose caller observes a result. An
    // interrupted task has no syscall in flight; its PC is the interrupted instruction, and the
    // shared bridge has ALREADY captured that frame into the outgoing TCB before this route runs
    // (`capture_outgoing_user_context_split`, keyed on `current_tid_authoritative`). Writing a
    // result here would corrupt the resumed register file.
    let mut owners = crate::kernel::syscall::yield_txn::SharedYieldOwners { shared };
    match crate::kernel::syscall::yield_txn::run_yield_transaction(&mut owners, cpu) {
        Ok(preempted) => {
            // The publish-side vocabulary NR 0 emits, so the post-lock drain's own markers read
            // identically whichever route published the deferral.
            crate::kernel::syscall::yield_txn::log_yield_deferred(cpu, preempted.outgoing);
            // The broad timer arm reached `yield_current`, whose first act is this increment. The
            // converted route owes it for the same reason NR 0's split route does: the broad path
            // no longer runs, so the count would otherwise be lost.
            shared.count_yield_split_mut();
            crate::yarm_log!(
                "TIMER_SPLIT_PREEMPT_COMMITTED cpu={} tick={} outgoing={} preempt=1 rearm=1 broad_lock=0",
                cpu.0,
                tick,
                preempted.outgoing
            );
            TimerSettlement::QueueAdvanceCommitted
        }
        // A PREEMPTING TICK WITH NOTHING TO PREEMPT — the dominant live population.
        //
        // `NoCurrent` is unreachable for a userspace NR 0 (a syscall always has a caller), so it
        // was documented as an unreachable decline. A TIMER has a different provenance: it fires
        // on a CPU that has already parked in its idle halt, where `current` is empty by
        // construction. The settlement is the idle-boundary advance, and the run-queue count it
        // reports is an OBSERVATION — the bridge's drain re-asks authoritatively, after the
        // off-lock timeout pipeline has published this trap's wakes.
        Err(crate::kernel::syscall::yield_txn::YieldDecline::NoCurrent) => {
            crate::yarm_log!(
                "TIMER_SPLIT_IDLE_ADVANCE_COMMITTED cpu={} tick={} observed_runnable={} preempt=1 rearm=1 broad_lock=0",
                cpu.0,
                tick,
                shared.runnable_count_on_cpu_split_read(cpu)
            );
            // The broad arm reached `yield_current` for this state, and its first act is this
            // increment. The converted route owes it for the same reason the committed-preempt arm
            // above does: the broad path no longer runs.
            shared.count_yield_split_mut();
            TimerSettlement::IdleQueueAdvance
        }
        // U9-PAGEFAULT1 §0 — `NotRunning` is the one decline whose post-state is not enough.
        //
        // Every other decline says something about the WORLD (no gate, no drainer, someone else's
        // deferral) and leaves the interrupted task exactly as it found it. `NotRunning` says
        // something about the FRAME this trap is about to return through: `current` names a task
        // that `PreemptOutgoing` would not accept, and "nothing was written" does not establish
        // that returning into it is safe. Those are different predicates.
        //
        // THE DELIVERED BEHAVIOUR THIS REPLACES was a fatal halt for EVERY status, not only the
        // terminal ones: the broad `yield_current` answered this same refusal with
        // `Err(KernelError::TaskMissing)`, the broad timer arm propagated it with `?`, and both
        // ISRs treat that as fatal — x86_64 `halt_forever()`, AArch64 a `wfe` loop. So continuing
        // here is a deliberate IMPROVEMENT over base, admitted only where it is provable, and not
        // a parity claim.
        //
        // What makes it provable is that the authority is asked of the owners that already answer
        // it — the exact-incarnation reader and the scheduler's placement reader — against the
        // incarnation this trap ENTERED on, captured before the transaction ran.
        Err(crate::kernel::syscall::yield_txn::YieldDecline::NotRunning) => {
            let authority =
                shared.entering_frame_authority_split_read(cpu, entering_tid, entering_asid);
            crate::yarm_log!(
                "TIMER_SPLIT_CURRENT_NOT_RUNNING cpu={} tick={} tid={} asid={} verdict={} preempt=1 rearm=1 broad_lock=0",
                cpu.0,
                tick,
                entering_tid,
                entering_asid.0,
                authority.marker()
            );
            // EVERY verdict is fail-closed, including `OwnsEnteringFrame`. The verdict is
            // DIAGNOSTIC here, not a control decision: it names which of six states the CPU was
            // actually in, so a fatal report says that instead of "task missing".
            //
            // An earlier draft continued on `OwnsEnteringFrame`, and justified it by claiming
            // `recv_block_unwind_exact_split` sets the same precedent. IT DOES NOT, and the
            // difference is exactly the hazard U9-RECV-BLOCK2 §2 was written to close.
            // `RecvUnwindOutcome::Restored` requires the TCB to say `Running`, established by an
            // exact-incarnation `apply_task_transition` COMMIT after the rank-1 restore — and
            // `restore_entering_incarnation_exact_split`'s own documentation records why:
            //
            //     "A task whose TCB says `Runnable` is, to every dispatch transition in the tree,
            //      a task that has not been selected to run. Reporting `Restored` for it
            //      permitted userspace execution from a status that says otherwise."
            //
            // The timer route commits nothing. Returning through the frame of a `Runnable` task
            // is the state that doc calls out, not the state it permits. Continuing was an
            // OPTIONAL change — the delivered broad path answered this refusal with
            // `Err(KernelError::TaskMissing)`, which both ISRs treat as fatal — so it is dropped
            // rather than kept on an unproven claim or bought with a new mutation this route has
            // no business performing.
            panic!(
                "timer: cpu {} entered on tid {entering_tid} asid {} whose frame is not \
                 resumable ({})",
                cpu.0,
                entering_asid.0,
                authority.marker()
            );
        }
        // EVERY OTHER DECLINE — settled here, on this CPU, with the interrupted task continuing.
        //
        // See the table in this function's documentation for the exact post-state each one leaves
        // and why continuing is the correct answer to it. The reason is named so the population
        // stays countable rather than inferred, and `TIMER_SPLIT_PREEMPT_DEFERRED` is a DIFFERENT
        // marker from the retired `TIMER_SPLIT_PREEMPT_REFUSED`: a refusal used to mean "the broad
        // arm will do this instead", and nothing will now.
        Err(decline) => {
            crate::yarm_log!(
                "TIMER_SPLIT_PREEMPT_DEFERRED cpu={} tick={} reason={} preempt=1 rearm=1 broad_lock=0 settlement=continue_current",
                cpu.0,
                tick,
                crate::kernel::syscall::yield_txn::legacy_reason(decline)
            );
            TimerSettlement::ContinueCurrent
        }
    }
}

#[cfg(not(feature = "hosted-dev"))]
fn try_split_timer_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    is_timer: bool,
) -> SplitDispatchDisposition {
    // THE FAMILY FILTER, and the only `NotHandled` a timer trap can produce. It says "this event
    // is not a TimerInterrupt", never "this timer is someone else's problem".
    if !is_timer {
        return SplitDispatchDisposition::NotHandled;
    }
    settle_recognized_timer(shared, cpu).disposition()
}

#[cfg(feature = "hosted-dev")]
fn try_split_timer_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _is_timer: bool,
) -> SplitDispatchDisposition {
    SplitDispatchDisposition::NotHandled
}

/// U9-FUTEX-WAIT-FINAL — the CLOSED `FutexWait` (NR 9) route.
///
/// # What the type change means
///
/// U9-QA §2 migrated NR 9's semantics onto the split primitives and left EIGHT recognized-NR9
/// exits answering `NotHandled` — the terminal broad dispatcher. Those were not "this trap is not
/// a FutexWait"; they were a recognized NR 9 being handed away, and the broad arm it landed in has
/// its own in-lock deferral machinery, so the family had two blocking implementations reachable
/// from one syscall. Giving the recognized body its own return type is what makes that
/// unrepeatable, exactly as it did for NR 1, NR 2/NR 5 and NR 7.
///
/// # The eight, and what each became
///
/// 1. **CPU out of range.** Mechanically unreachable for an authority-bearing caller: the
///    authority is minted by `TrapPathWindow::establish` from a hardware-identified CPU, and an
///    index at or past `MAX_CPUS` yields `DispatchAuthority::none`, which is never live. Settled
///    as `WrongObject` — the error `with_cpu` raises for a CPU it cannot bind.
/// 2. **Argument decode.** Genuinely reachable from userspace, and canonical: `InvalidArgs`,
///    the error `handle_futex_wait`'s two `u32::try_from` produce.
/// 3. **No current task.** Canonical `TaskMissing` — but raised in the canonical ORDER, which is
///    not where this route used to ask it; see below.
/// 4. **Value/address validation.** Four canonical errors the `Option` erased into one `None`.
///    `futex_wait_decide_split_read` answers them typed.
/// 5. **Existing deferral** and 7. **reservation failure.** Both mechanically unreachable: the
///    deferral is per CPU, this route is its only split producer, traps nest on no architecture,
///    and every decline past the reservation clears it.
/// 6. **Queue-advance admission.** `MultiCpu` and `CpuNotAuthoritative` are the AMBIENT contract,
///    and they are what kept this route to a single dispatching CPU; the trap's own authority
///    replaces them. `NoTrapDrainer` and `OutgoingIdentityStale` are refuted by
///    `TrapPathWindow::establish`, which sets the drainer flag and opens the window that minted
///    this authority, in that order. `ArchUnsupported` and `StashOccupied` are scoped to
///    `StashedKernelSwitch`, and this caller passes `ExactTokenResume`.
/// 8. **Publication failure.** A competing-winner settlement, not a `bool`; see
///    `futex_wait_park_exact_split`.
///
/// # The canonical validation ORDER, which this route did not have
///
/// `futex_wait_current` calls `validate_current_user_futex_word` FIRST, and that owner checks the
/// address before it resolves the caller: `addr == 0` is `WrongObject` and a kernel-range address
/// is `UserMemoryFault` **even when there is no current task**. This route read
/// `current_tid_authoritative` before the value check and fell back on its `None`, which was
/// correct only because the broad handler then re-derived the whole thing in the right order.
/// With the fall-back gone the order has to be right here, so it is: decode, address range,
/// caller, readability, then the caller's own comparison.
///
/// # The ABI is unchanged
///
/// `expected` and `observed` are both the CALLER'S arguments and the kernel compares them; it
/// reads no futex word to decide whether to block, only to prove the address is readable. No
/// timeout, no bitset, no requeue, no PI, no new flag, no new lane. The success encoding is
/// `set_ok(usize::from(blocked), 0, 0)`, byte-for-byte the broad handler's.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_futex_wait_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
    authority: crate::runtime::DispatchAuthority,
) -> SplitDispatchDisposition {
    use crate::kernel::syscall::{SYSCALL_ARG_CAP, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR};

    if frame.syscall_num() != crate::kernel::syscall::SYSCALL_FUTEX_WAIT_NR {
        // THE ONLY `NotHandled` this family can produce, and it is not a fall-through: the trap is
        // a different syscall, about which this route has no opinion.
        return SplitDispatchDisposition::NotHandled;
    }
    try_split_futex_wait_recognized(shared, cpu, frame, authority).into_dispatch()
}

/// U9-FUTEX-WAIT-FINAL §2 — the recognized NR 9 body. Every outcome is settled here.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_futex_wait_recognized(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
    authority: crate::runtime::DispatchAuthority,
) -> SplitBlockingDisposition {
    use crate::kernel::syscall::sched::{FutexParkOutcome, FutexWaitDecision};
    use crate::kernel::syscall::{SYSCALL_ARG_CAP, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR, SyscallError};
    use SplitBlockingDisposition as D;

    let cpu_idx = cpu.0 as usize;
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        // (1) Established impossible: `DispatchAuthority::none` is what an out-of-range index
        // mints, and it is never live. `WrongObject` is the error `with_cpu` raises for a CPU it
        // cannot bind, so the answer is the broad arm's even though the owner is not.
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_INVARIANT cpu={} reason=cpu_out_of_range result=failed_closed",
            cpu.0
        );
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::WrongObject)));
    }

    // (2) ABI, identical to `handle_futex_wait` — and ANSWERED, because `InvalidArgs` is what that
    // handler's `u32::try_from` produces and no later owner will produce it for us.
    let addr = frame.arg(SYSCALL_ARG_CAP);
    let (Ok(expected), Ok(observed)) = (
        u32::try_from(frame.arg(SYSCALL_ARG_PTR)),
        u32::try_from(frame.arg(SYSCALL_ARG_LEN)),
    ) else {
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_DONE tid=0 addr={} result=invalid_args cpu={}",
            addr,
            cpu.0
        );
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::InvalidArgs)));
    };

    // (3) THE ADDRESS RANGE COMES FIRST. `validate_current_user_futex_word` refuses a null or
    // kernel-range word before it ever asks who the caller is, so a trap with no current task
    // still gets the address error. This is the same function that owner calls.
    if let Err(err) = crate::kernel::syscall::sched::futex_word_range_check(addr) {
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_DONE tid=0 addr={} result=range_refused err={:?} cpu={}",
            addr,
            err,
            cpu.0
        );
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::from(err))));
    }

    // (4) Only now the caller, and its absence is the canonical `TaskMissing`.
    let Some(tid) = shared.current_tid_authoritative(cpu) else {
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_DONE tid=0 addr={} result=no_current_task cpu={}",
            addr,
            cpu.0
        );
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::from(
            crate::kernel::boot::KernelError::TaskMissing,
        ))));
    };

    // (5) Readability, then the CALLER'S comparison — typed, so each canonical error is answered
    // by the owner that decided it rather than re-derived by the broad dispatcher.
    let decision = match shared.futex_wait_decide_split_read(tid, addr, expected, observed) {
        Ok(decision) => decision,
        Err(err) => {
            crate::yarm_log!(
                "FUTEX_WAIT_SPLIT_DONE tid={} addr={} result=value_check err={:?} cpu={}",
                tid,
                addr,
                err,
                cpu.0
            );
            return D::Complete(Err(TrapHandleError::Syscall(SyscallError::from(err))));
        }
    };
    if matches!(decision, FutexWaitDecision::Proceed) {
        // The futex word already moved. No transition, no switch, no drain — the canonical
        // `set_ok(usize::from(false), 0, 0)`.
        frame.set_ok(0, 0, 0);
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_DONE tid={} addr={} result=not_blocked",
            tid,
            addr
        );
        return D::Complete(Ok(()));
    }

    // (6) ADMISSION, on the trap's own authority.
    //
    // The ambient form refused `MultiCpu` whenever more than one CPU was dispatching, which is why
    // this route never ran on an SMP boot and why every NR 9 there reached the terminal
    // acquisition. A trap authority answers both ambient questions unforgeably — it names this CPU
    // and the selection owner authenticates against the same value — so neither the comparison nor
    // the single-dispatcher restriction applies to it. Every other precondition is the shared
    // body's and is unchanged.
    if crate::kernel::boot::futex_wait_dispatch_is_deferred(cpu_idx) {
        return settle_futex_cannot_park(cpu, tid, "already_deferred");
    }
    if let Err(refusal) = shared.queue_advance_admit_with_authority_split(
        authority,
        crate::kernel::boot::QueueAdvanceApply::ExactTokenResume,
    ) && !matches!(
        refusal,
        crate::kernel::boot::QueueAdvanceRefusal::IncomingUnavailable
    ) {
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_INVARIANT cpu={} tid={} reason=admission detail={:?} result=failed_closed",
            cpu.0,
            tid,
            refusal
        );
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::Internal)));
    }
    // (7) Reserve before the irreversible publication, so a reservation failure stays
    // pre-mutation.
    if !crate::kernel::boot::futex_wait_dispatch_try_defer(cpu_idx, tid) {
        return settle_futex_cannot_park(cpu, tid, "defer_unavailable");
    }
    crate::yarm_log!("FUTEX_WAIT_SPLIT_BEGIN");

    // (8) THE PARK. Exact incarnation, compare-and-clear, wake detected.
    //
    // The ASID is read while the caller is still this CPU's current — the one interval in which
    // its TCB cannot be reclaimed — so what the transaction authenticates against is the
    // incarnation this trap entered from and not whatever later answers to the number.
    let asid = crate::kernel::vm::Asid(shared.task_asid_for_tid_split_read(tid) as u16);
    match shared.futex_wait_park_exact_split(cpu, tid, asid, addr) {
        FutexParkOutcome::Parked { .. } => {
            crate::yarm_log!(
                "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=futex_wait_switch_required tid={} cpu={}",
                tid,
                cpu_idx
            );
            // The syscall's own result, into the outgoing frame, before the switch: the caller
            // observes it when it is later resumed.
            frame.set_ok(1, 0, 0);
            D::QueueAdvanceCommitted
        }
        // A waker won during publication. The caller is NOT parked, so nothing is owed to a drain
        // — release the reservation — and the answer is still `1`: it blocked and was woken, which
        // is what `set_ok(usize::from(blocked), 0, 0)` reports for a caller that parked at all.
        FutexParkOutcome::WokenDuringPublication {
            entering,
            recovered,
        } => {
            crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
            frame.set_ok(1, 0, 0);
            if recovered.may_resume_entering_frame() {
                crate::yarm_log!(
                    "FUTEX_WAIT_SPLIT_DONE tid={} addr={} result=woken_during_publish recovery={}",
                    tid,
                    addr,
                    recovered.slug()
                );
                return D::Complete(Ok(()));
            }
            // The placement could not be restored, so the entering frame is not this task's to
            // return through. The BRIDGE settles it — the same owner the receive family's
            // post-clear settlements use, which captures the completed continuation, publishes it
            // and lands this CPU.
            D::Unsettled(SplitBlockUnsettled {
                entering,
                outcome: recovered,
            })
        }
        // Both established-impossible, and both PRE-MUTATION by construction: the registration is
        // undone exactly on a victim mismatch, and an incarnation mismatch writes nothing at all.
        // The caller is therefore still this CPU's current and its frame is still its own.
        FutexParkOutcome::VictimChanged => {
            crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
            settle_futex_cannot_park(cpu, tid, "phase_a_victim_changed")
        }
        FutexParkOutcome::IncarnationMoved => {
            crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
            crate::yarm_log!(
                "FUTEX_WAIT_SPLIT_DONE tid={} addr={} result=task_missing cpu={}",
                tid,
                addr,
                cpu.0
            );
            D::Complete(Err(TrapHandleError::Syscall(SyscallError::from(
                crate::kernel::boot::KernelError::TaskMissing,
            ))))
        }
    }
}

/// U9-FUTEX-WAIT-FINAL §2 — settle an NR 9 that must park but whose CPU could not commit the park.
///
/// # Every reason is PRE-MUTATION, and every one is established-impossible
///
/// All three are decided before the park transaction registers anything, or by a transaction that
/// undid its registration exactly. The caller is therefore still this CPU's current task and the
/// entering frame is still its own, so answering through that frame loses nothing and parks
/// nothing.
///
/// * **`already_deferred` / `defer_unavailable`.** The FutexWait deferral is per CPU. This route
///   is its only split producer, it reserves at step (7), and every decline after that point
///   clears it. Traps do not nest on any of the three architectures, so no second reservation can
///   exist on this CPU while this trap is running.
/// * **`phase_a_victim_changed`.** The current slot was read by `current_tid_authoritative(cpu)`
///   in this same trap. Installing a current on a CPU requires running on that CPU, and this trap
///   is what is running on it.
///
/// Fail-closed rather than divergent, for the reason the receive family's twin records: divergence
/// is licensed for an established impossibility but is not required, and it is the worse choice
/// when the task is current, runnable and resumable. What is NOT available is handing the trap to
/// the broad dispatcher, which would be this slice's own escape wearing a different name.
#[cfg(not(feature = "hosted-dev"))]
fn settle_futex_cannot_park(
    cpu: CpuId,
    tid: u64,
    reason: &'static str,
) -> SplitBlockingDisposition {
    crate::yarm_log!(
        "FUTEX_WAIT_SPLIT_INVARIANT cpu={} tid={} reason={} result=failed_closed",
        cpu.0,
        tid,
        reason
    );
    SplitBlockingDisposition::Complete(Err(TrapHandleError::Syscall(
        crate::kernel::syscall::SyscallError::Internal,
    )))
}

#[cfg(feature = "hosted-dev")]
fn try_split_futex_wait_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
    _authority: crate::runtime::DispatchAuthority,
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
    // but they ARE admitted to the direct request/reply gates below.
    //
    // U9-YIELD2 §1 — the sentence that used to end this note was stale. It read "since WA1-GATE
    // that admission requires the explicit proof gate on EVERY architecture, x86_64 included —
    // the production term is `false` everywhere — so every normal boot stays byte-identical to
    // the legacy path". `ipccall_direct_production_enabled()` is
    // `cfg!(x86_64) || cfg!(aarch64) || cfg!(riscv64)`, so the production term is `true` on all
    // three and `ipccall_direct_admission_enabled()` short-circuits before the proof gate is
    // consulted. NR 6 and NR 7 are admitted here on EVERY ordinary boot.
    //
    // U9-IPC-RESIDUAL2 §2 — and the sentence that ended this note in turn ("what they still do on
    // a decline is fall back to the legacy broad handler, which is a residual arm") no longer
    // holds either: neither route can express a decline. NR 6 does not reach this predicate at
    // all any more — it is serviced by the switching dispatcher above, because its full-endpoint
    // arm parks the caller — so in practice this term now admits NR 7 alone. It is kept naming
    // both because it is the canonical admission predicate for the family, and narrowing it to
    // one NR would make the two routes answer different questions about the same gate.
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
    // U9-RECV-FINAL §1 — NR 2 no longer appears here, and is no longer on the NR whitelist
    // above either.
    //
    // It used to be routed to the queued-plain helper from the NON-SWITCHING dispatcher, whose
    // contract is that every class on it may be early-returned through the caller's own frame.
    // That was never true of a receive: an empty endpoint parks the caller. The two halves of the
    // family were also separate entry points, each free to answer `NotHandled` after recognizing
    // the syscall.
    //
    // `try_split_ipc_recv_family_into_frame` is now the ONE entry, consulted with the other
    // switching classes, and the queued-plain helper is one of its internal lanes. A decline from
    // that lane reaches the next pre-lock owner instead of a second dispatcher entry.

    // Stage 114: `VmBrk` (NR 14) is routed to the dedicated brk-shrink helper
    // for the same reason `IpcRecv` is above — eligibility (group leader,
    // page-crossing shrink, single CPU online) can only be decided inside the
    // helper. Every case it cannot service returns `None`, which propagates
    // UNCHANGED back to the global-lock fallback below.
    // U9-VM-ENTRY1: NR 3, NR 13 and NR 14 are routed to their TOTAL owners. Unlike every other
    // route above, none of these may answer `None` after the NR gate: a family with a reachable
    // broad fallback is not closed, and after a frame is taken or a page installed a `None` would
    // hand a partially executed transaction to a dispatcher that knows nothing about it.
    if matches!(syscall, Syscall::VmMap) {
        return try_split_vm_map_into_frame(shared, cpu, frame);
    }
    if matches!(syscall, Syscall::VmAnonMap) {
        return try_split_vm_anon_map_into_frame(shared, cpu, frame);
    }
    if matches!(syscall, Syscall::VmBrk) {
        return try_split_vm_brk_into_frame(shared, cpu, frame);
    }
    // U9-XFER1 §3: NR 4 joins them, and for the same reason — its route is TOTAL after this gate.
    if matches!(syscall, Syscall::TransferRelease) {
        return try_split_transfer_release_into_frame(shared, cpu, frame);
    }
    // U9-XFER2 §3: NR 30 joins them. Its user copies have off-lock owners, so the whole ABI —
    // including every error path — is serviced here.
    if matches!(syscall, Syscall::RecvSharedV3) {
        return try_split_recv_shared_v3_into_frame(shared, cpu, frame);
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

    // U9-IPC-RESIDUAL2 §2: IpcCall (NR 6) is no longer serviced from here. It became a SWITCHING
    // class the moment its full-endpoint arm started parking the caller, so it is tried in
    // `try_split_dispatch_into_frame` alongside the other four, whose contract admits a caller
    // that must not be early-returned through its own frame.

    // Stage 199A2B3: IpcReply (NR 7) direct reply. The helper snapshots the reply payload
    // off-lock (owned) and drives the accepted off-lock reply transaction (reserve →
    // caller-copy → exact-waiter claim → record Consumed → single enqueue).
    //
    // U9-YIELD2 §1 — the "(proof-gated, default-OFF)" label that used to head this note is stale
    // for the same reason NR 6's is: admission short-circuits on the production term, which is
    // `true` on all three architectures. The gate is open on every ordinary boot.
    //
    // U9-IPC-RESIDUAL2 §2 — and the route now answers `Result`, not `Option`. "For any case it
    // cannot service it returns `None`" was the fall-through this package removed; there is no
    // longer a value the route can produce that means "let the broad dispatcher have this trap",
    // so the measurement below counts only the arm that never runs on a supported port.
    #[cfg(not(feature = "hosted-dev"))]
    if matches!(syscall, Syscall::IpcReply) {
        if crate::kernel::boot::ipccall_direct_admission_enabled() {
            return Some(try_split_ipcreply_direct_into_frame(shared, cpu, frame));
        }
        // U9-IPC-RESIDUAL1 §1 — the NR7 twin of the measurement above. Reachable only where the
        // production admission term is false, which is no supported architecture.
        crate::kernel::direct_ipc_counters::REPLY.note_broad_entry();
        crate::yarm_log!(
            "IPCREPLY_SPLIT_UNROUTED cpu={} nr=7 reason=admission_disabled result=broad_entry",
            cpu.0
        );
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

/// Stage 199A2B2F: pre-lock NR6 direct-request snapshot publication + off-lock transaction drain.
///
/// U9-YIELD2 §1: this used to be labelled "x86 … (proof-gated)". Neither half holds. Admission is
/// `ipccall_direct_admission_enabled()` = `production || proof`, and the production term is true on
/// all three architectures, so the gate is open on every ordinary boot and the route is reached on
/// x86_64, AArch64 and RISC-V alike.
///
/// Runs ENTIRELY off the broad `KernelState` lock and off any ranked lock during the source copy:
///   read args → capture caller `{tid,asid}` → validate `len<=128` → copy the request
///   payload through `copy_from_user_asid_split_read` (NO lock held) → build the owned
///   `IpcCallDirectSnapshot` → CLAIM the exact published blocked-server acknowledgement
///   → build one owned `DirectRequestPostWork` → drain it through the accepted
///   `SharedKernel::ipc_call_direct_request_txn`. No userspace payload pointer survives the
///   snapshot.
///
/// # U9-IPC-RESIDUAL2 §2 — the boundary is TOTAL
///
/// Every sentence above described what this route DOES; what it used to do besides was answer
/// `None` from eleven places, and each of those handed a recognized NR 6 to the terminal broad
/// acquisition. U9-IPC-RESIDUAL1 measured that traffic and closed two shapes; it did not make the
/// boundary total, and its record said so less plainly than it should have.
///
/// A recognized NR 6 now leaves this route by exactly one of four doors, and `NotHandled` is not
/// among them once the syscall is recognized:
///
/// | door | disposition | when |
/// |---|---|---|
/// | the typed refusal | `Complete(Ok)` with the frame carrying the broad path's own error | every validation the broad handler would have failed |
/// | the user fault | `Complete(Ok)` with the fault recorded and `PageFault` framed | a source copy the caller's address space refused |
/// | the delivery | `Complete(Ok)` / `PostWorkCommitted{finalize_syscall:true}` | direct hand-off, buffered enqueue, or blocked-waiter delivery |
/// | the park | `PostWorkCommitted{finalize_syscall:false}` | a full endpoint — the blocking origin |
///
/// Internal hand-off between the three lanes (direct → buffered → park) is not a door: it is
/// this route choosing which of its own owners answers, which is what §2 permits and what the
/// broad handler does inside one acquisition.
///
/// Two preflight verdicts are answered with a typed invariant error rather than a lane, on the
/// precedent NR 1 set for the same two classes (199G-C4 §4): `SynchronousMode` and
/// `EndpointNotAdmitted` are both unreachable from any supported configuration, and handing an
/// impossible class to the broad dispatcher would be the one edge this package exists to remove.
/// The unreachability is source-derived, not observational — see
/// `u9_ipc_residual2_closure::{no_production_endpoint_is_synchronous, direct_production_admission_is_statically_total}`.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_ipccall_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> SplitDispatchDisposition {
    use SplitDispatchDisposition as D;
    if !matches!(Syscall::decode(frame.syscall_num()), Ok(Syscall::IpcCall)) {
        return D::NotHandled;
    }
    // U9-IPC-RESIDUAL1 §1 / U9-IPC-RESIDUAL2 §2 — THE terminal-entry measurement for NR 6, once
    // per trap and in exactly one place, so `broad_entries=0` means what it says.
    //
    // Every way a RECOGNIZED NR 6 can leave this route without being serviced goes through this
    // one closure. There are exactly TWO, and neither is reachable on a supported port:
    // admission's first term is a `const fn` that is true on x86_64, AArch64 and RISC-V, and the
    // CPU index is bounded by the trap entry that produced it.
    //
    // There used to be a third — the body answering `None` — and it is gone at the type level:
    // `try_split_ipccall_direct_into_frame` returns `SplitDispatchDisposition`, so there is no
    // value it can produce that means "the broad dispatcher should service this trap". The two
    // that remain are kept rather than deleted, because the closure guards assert that no
    // reachable path takes them, and a guard about a door that does not exist proves nothing.
    let unrouted = |reason: &str| -> D {
        crate::kernel::direct_ipc_counters::REQUEST.note_broad_entry();
        crate::yarm_log!(
            "IPCCALL_SPLIT_UNROUTED cpu={} nr=6 reason={} result=broad_entry",
            cpu.0,
            reason
        );
        D::NotHandled
    };
    if !crate::kernel::boot::ipccall_direct_admission_enabled() {
        return unrouted("admission_disabled");
    }
    if cpu.0 as usize >= crate::kernel::scheduler::MAX_CPUS {
        return unrouted("cpu_out_of_range");
    }
    try_split_ipccall_direct_into_frame(shared, cpu, frame)
}

/// U9-IPC-RESIDUAL3 §1 — **THE NR 6 validation policy**, in `handle_ipc_call`'s own order, asked
/// from the FACTS.
///
/// U9-IPC-RESIDUAL2 §2 claimed to do this and did not. Its resolver keyed step (1) on the
/// classifier's *verdict* — `if let V::SendCapUnresolved(err) = verdict` — but the classifier
/// checks `payload_len` first and returns `PayloadTooLong` **before it ever resolves the send
/// capability**. So a call with an invalid send capability, a valid reply capability and an
/// oversized payload skipped the send-capability question entirely and answered `InvalidArgs`,
/// where `handle_ipc_call` answers `InvalidCapability`. The same hole swallowed a stale endpoint
/// incarnation behind an oversized payload, which broad reports as `WrongObject`.
///
/// A verdict is not an ordering. It says only that *a* refusal is owed; which one is a question
/// about the facts, and the facts are what this asks:
///
/// ```text
/// validate_endpoint_right(send_cap, SEND)?      // InvalidCapability / WrongObject / MissingRight
/// validate_endpoint_right(reply_recv_cap, RECEIVE)?
/// current_tid()?                                // Internal
/// resolve_endpoint_index(endpoint)?             // WrongObject / StaleCapability -> WrongObject
/// len > Message::MAX_PAYLOAD                    // InvalidArgs
/// ```
///
/// Both NR 6 callers go through here — the eligible path and the refusal resolver — so there is
/// one policy rather than two that have to be kept in step. On the eligible path every question
/// but the reply capability has already been answered by the classifier, so the call is a
/// consistency check that costs one lock-free read; on the refusal path it is the whole answer.
///
/// `RequesterUnavailable` needs no arm of its own: with no current task the call site sets
/// `facts.send_cap` to `Err(InvalidCapability)`, which is exactly what `validate_endpoint_right`
/// produces for a task with no cnode, so step (1) answers it first — as broad does. Step (3) is
/// therefore unreachable and is kept only because `current_tid()?` is a real statement of the
/// broad sequence.
///
/// Nothing here mutates. Every step is a read.
///
/// It is `pub(crate)` and compiled on BOTH profiles so the differential cases can drive it
/// directly against `syscall::dispatch`'s own answer for the same arguments. A policy whose only
/// coverage is a source scan for its name is not covered.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) fn nr6_validate_in_broad_order(
    shared: &SharedKernel,
    facts: &crate::kernel::direct_eligibility::DirectRequestFacts,
    tid: Option<u64>,
    reply_recv_cap: crate::kernel::capabilities::CapId,
) -> Result<(), (crate::kernel::syscall::SyscallError, &'static str)> {
    use crate::kernel::capabilities::{CapObject, CapRights};
    use crate::kernel::syscall::SyscallError;

    // (1) `validate_endpoint_right(send_cap, SEND)?`, all four of its questions.
    let object = match facts.send_cap {
        Ok(object) => object,
        Err(err) => return Err((SyscallError::from(err), "send_cap")),
    };
    // U9-IPC-RESIDUAL3 §1 — the LIVENESS question, which the send-side resolver does not ask.
    //
    // `validate_endpoint_right` folds it into its first answer:
    // `slot_result.and_then(|c| capability_object_live(c.object).map(|_| c)).ok_or(InvalidCapability)`.
    // `resolve_endpoint_send_cap_split_read` checks the slot, the object kind and the `SEND`
    // right, and NOT the generation — so a capability naming a recycled endpoint incarnation
    // resolves `Ok` there while broad has already refused it with `InvalidCapability`.
    //
    // Without this, a stale send capability was answered `WrongObject` by step (4) below, which
    // is the error broad gives for a DIFFERENT condition. The differential case
    // `a_stale_endpoint_identity_is_reported_even_behind_an_oversized_payload` found it by
    // running `dispatch` and comparing, which is the only way a mismatch like this surfaces:
    // both answers are plausible in isolation.
    if shared.capability_object_live_split(object).is_none() {
        return Err((SyscallError::InvalidCapability, "send_cap_stale"));
    }
    // Unreachable through this route — the resolver refuses a non-`Endpoint` object with
    // `WrongObject` before it returns — but it is the broad handler's own next question, and
    // stating it keeps the sequence complete rather than relying on a resolver's internals
    // staying as they are.
    if !matches!(object, CapObject::Endpoint { .. }) {
        return Err((SyscallError::WrongObject, "not_an_endpoint"));
    }
    // (2) `validate_endpoint_right(reply_recv_cap, RECEIVE)?`
    if let Err(err) = shared.validate_endpoint_right_split_read(
        tid.unwrap_or(0),
        reply_recv_cap,
        CapRights::RECEIVE,
    ) {
        return Err((err, "reply_cap"));
    }
    // (3) `current_tid(kernel)?` — `Internal`, which is what `current_tid` maps `None` to.
    if tid.is_none() {
        return Err((SyscallError::Internal, "no_requester"));
    }
    // (4) `resolve_endpoint_index(endpoint)?` — an absent slot is `WrongObject` and a stale
    // incarnation `StaleCapability`; both reach userspace as `WrongObject`. The split facts
    // collapse the two into "no live mode for this incarnation", which is the same question.
    if facts.endpoint_mode.is_none() {
        return Err((SyscallError::WrongObject, "incarnation_gone"));
    }
    // (5) `len > Message::MAX_PAYLOAD` → `InvalidArgs`. `IPC_DIRECT_PAYLOAD_MAX` is defined AS
    // `Message::MAX_PAYLOAD`, so this is the broad refusal and not a narrower limit wearing its
    // error.
    if facts.payload_len > crate::kernel::ipccall_direct::IPC_DIRECT_PAYLOAD_MAX {
        return Err((SyscallError::InvalidArgs, "payload_too_long"));
    }
    Ok(())
}

/// U9-IPC-RESIDUAL2 §2 — resolve a NR 6 preflight refusal to the error the BROAD handler would
/// have produced, **in the broad handler's own order**.
///
/// `classify_direct_request_eligibility` is a pure classifier and its order is its own: it asks
/// the cheapest question first (`payload_len`) so that an over-long payload never reaches a
/// capability resolution. That is the right order for a classifier and the wrong order for an
/// answer, because `handle_ipc_call` validates capabilities first:
///
/// ```text
/// validate_endpoint_right(send_cap, SEND)?      // InvalidCapability / WrongObject / MissingRight
/// validate_endpoint_right(reply_recv_cap, RECEIVE)?
/// current_tid()?
/// resolve_endpoint_index(endpoint)?             // WrongObject / StaleCapability
/// len > Message::MAX_PAYLOAD                    // InvalidArgs
/// ```
///
/// A call that is wrong in more than one way must be told about the FIRST thing that is wrong,
/// or the two routes disagree for exactly the malformed calls that are hardest to debug. So this
/// re-asks the questions in the broad order rather than translating the classifier's verdict
/// one-to-one — the verdict decides only that a refusal is owed, never which one.
///
/// Nothing here mutates: every step is a read, and the refusal is delivered by writing the frame,
/// which is what the broad handler's `?` would have done one lock later.
#[cfg(not(feature = "hosted-dev"))]
#[allow(clippy::too_many_arguments)]
fn nr6_refuse_preflight(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
    verdict: crate::kernel::direct_eligibility::DirectRequestEligibility,
    facts: &crate::kernel::direct_eligibility::DirectRequestFacts,
    tid: Option<u64>,
    send_cap: crate::kernel::capabilities::CapId,
    reply_recv_cap: crate::kernel::capabilities::CapId,
) -> SplitDispatchDisposition {
    use crate::kernel::direct_eligibility::DirectRequestEligibility as V;
    use crate::kernel::syscall::SyscallError;
    use SplitDispatchDisposition as D;

    let answer = |frame: &mut TrapFrame, err: SyscallError, reason: &str| -> D {
        crate::yarm_log!(
            "IPCCALL_DIRECT_REFUSED_PRE_LOCK tid={} send_cap={} reason={} err={:?} copies=0 enqueues=0 mutations=0 result=ok",
            tid.unwrap_or(0),
            send_cap.0,
            reason,
            err
        );
        frame.set_err(err.code());
        D::Complete(Ok(()))
    };

    // U9-IPC-RESIDUAL3 §1 — ONE policy, asked from the FACTS, in the broad handler's order.
    //
    // The verdict is not consulted for ordering at all. It told the caller that a refusal is
    // owed; which refusal is a question about the facts, and asking the verdict instead is what
    // let an invalid send capability hide behind an oversized payload.
    let requester = tid.unwrap_or(0);
    if let Err((err, reason)) = nr6_validate_in_broad_order(shared, facts, tid, reply_recv_cap) {
        return answer(frame, err, reason);
    }

    // Everything the broad sequence asks has now passed, so the only refusals left are the two
    // classes the broad path answers by doing something the split route does not implement.
    match verdict {
        // The two impossible classes, refused with a typed invariant error rather than a
        // fallback — the precedent NR 1 set for exactly these two (199G-C4 §4).
        //
        // `Synchronous`: every production endpoint is created by `create_endpoint(depth)` or by
        // `spawn_image_txn`'s explicit `EndpointMode::Buffered`, and there is no endpoint-creation
        // syscall at all, so userspace cannot obtain one. `EndpointNotAdmitted`:
        // `ipccall_direct_production_enabled()` is a `const fn` returning true on x86_64, AArch64
        // and RISC-V, and it is the first term of the admission predicate, so on every supported
        // port the confining branch is statically dead.
        V::SynchronousMode => {
            crate::yarm_log!(
                "IPCCALL_SPLIT_INVARIANT cpu={} tid={} reason=synchronous_endpoint result=failed_closed",
                cpu.0,
                requester
            );
            answer(frame, SyscallError::WrongObject, "synchronous_endpoint")
        }
        V::EndpointNotAdmitted => {
            crate::yarm_log!(
                "IPCCALL_SPLIT_INVARIANT cpu={} tid={} reason=endpoint_not_admitted result=failed_closed",
                cpu.0,
                requester
            );
            answer(frame, SyscallError::Internal, "endpoint_not_admitted")
        }
        // Answered by the shared policy above, which reaches each of them from the facts rather
        // than from the verdict. Listed so a new verdict cannot inherit an arm by wildcard, and
        // reachable only if the policy and the classifier ever disagree — which is itself the
        // bug this arm should report rather than paper over.
        V::SendCapUnresolved(_)
        | V::RequesterUnavailable
        | V::NotAnEndpoint
        | V::EndpointIncarnationGone
        | V::PayloadTooLong => {
            debug_assert!(
                false,
                "the shared validation policy must answer every fact-derived verdict"
            );
            answer(frame, SyscallError::Internal, "policy_classifier_disagree")
        }
        V::Eligible { .. } => {
            debug_assert!(
                false,
                "an eligible verdict never reaches the refusal resolver"
            );
            answer(frame, SyscallError::Internal, "eligible_in_refusal")
        }
    }
}

/// U9-IPC-RESIDUAL2 §2 — the NR 6 body, whose return type is the proof.
///
/// It answers `SplitDispatchDisposition` and not `Option<..>`: there is no value it can produce
/// that means "the broad dispatcher should service this trap". The one disposition that would
/// mean that, `NotHandled`, appears nowhere in this function or in anything it calls — which is
/// checked by `u9_ipc_residual2_closure::no_family_function_can_fall_through`, and is a
/// statement about types rather than about text.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_ipccall_direct_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> SplitDispatchDisposition {
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
        return nr6_refuse_preflight(
            shared, cpu, frame, verdict, &facts, tid, send_cap, reply_cap,
        );
    };
    REQUEST_COUNTERS.note_eligible();
    let tid = tid.expect("eligibility requires an available requester");
    let _ = IPC_DIRECT_PAYLOAD_MAX;
    // U9-IPC-RESIDUAL2 §2 — the reply-receive capability is validated HERE for the eligible path
    // too, because the broad handler validates it before it copies anything.
    //
    // U9-IPC-RESIDUAL1 checked it only inside the buffered lane, and in the lane's own order
    // (resolve -> right -> kind) rather than the broad order (resolve+live -> kind -> right). Two
    // consequences, both user-visible: a direct hand-off with a bad reply capability copied the
    // payload and claimed an acknowledgement before anyone noticed, and a capability that was
    // both the wrong object AND missing `RECEIVE` reported the wrong one of the two.
    //
    // U9-IPC-RESIDUAL3 §1 — and it is asked through THE shared policy, not through a private
    // copy of one of its steps. Every other question in that policy has already been answered by
    // the classifier on this path, so what this adds is the reply capability plus a consistency
    // check; having both callers go through one function is what stops the eligible path and the
    // refusal path drifting into different orders again.
    if let Err((err, reason)) = nr6_validate_in_broad_order(shared, &facts, Some(tid), reply_cap) {
        crate::yarm_log!(
            "IPCCALL_DIRECT_REFUSED_PRE_LOCK tid={} send_cap={} reason={} err={:?} copies=0 enqueues=0 mutations=0 result=ok",
            tid,
            send_cap.0,
            reason,
            err
        );
        frame.set_err(err.code());
        REQUEST_COUNTERS.note_failed(err);
        return SplitDispatchDisposition::Complete(Ok(()));
    }
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
        REQUEST_COUNTERS.note_failed(crate::kernel::syscall::SyscallError::WouldBlock);
        return SplitDispatchDisposition::Complete(Ok(()));
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
    // U9-IPC-RESIDUAL2 §2 — THE SOURCE PAYLOAD, through the same three shapes the broad handler
    // distinguishes. `copy_from_user_asid_split_read` answers `None` for three unrelated
    // conditions and the old code read all three as "decline to broad":
    //
    // * `asid_raw == 0` — a KERNEL-ASID caller. `handle_ipc_call` does not copy at all for one:
    //   `current_task_has_user_asid` is false and the payload rides in the argument registers
    //   (`inline_payload_from_frame`). Treating that as a fault would have reported `PageFault`
    //   for a call whose payload was never in user memory.
    // * `len == 0` — an EMPTY payload, which is legal and which the broad path sends happily.
    // * anything else — a genuine user-memory fault.
    let mut payload_buf = [0u8; crate::kernel::ipc::Message::MAX_PAYLOAD];
    if asid_raw == 0 {
        // The kernel-task shape, through the owner the broad handler uses for it.
        let Some(regs) = crate::kernel::syscall::split_inline_payload_from_frame(frame, len) else {
            crate::yarm_log!(
                "IPCCALL_DIRECT_REFUSED_PRE_LOCK tid={} send_cap={} reason=inline_payload err=InvalidArgs copies=0 enqueues=0 mutations=0 result=ok",
                tid,
                send_cap.0
            );
            frame.set_err(crate::kernel::syscall::SyscallError::InvalidArgs.code());
            REQUEST_COUNTERS.note_failed(crate::kernel::syscall::SyscallError::InvalidArgs);
            return SplitDispatchDisposition::Complete(Ok(()));
        };
        payload_buf[..len].copy_from_slice(&regs[..len]);
    } else if len > 0 {
        let Some(bytes) = shared.copy_from_user_asid_split_read(asid_raw, user_ptr, len) else {
            // The ESTABLISHED user-fault path, not `InvalidArgs`: record the fault, then frame
            // `PageFault`, exactly as `record_user_fault(.., FaultAccess::Read)` does one lock
            // later, and return success from the syscall's point of view.
            let _ = shared.record_split_source_read_fault(cpu, frame, user_ptr);
            crate::yarm_log!(
                "IPCCALL_DIRECT_SOURCE_FAULT tid={} user_ptr={:#x} len={} access=read copies=0 enqueues=0 mutations=0 result=ok",
                tid,
                user_ptr,
                len
            );
            REQUEST_COUNTERS.note_failed(crate::kernel::syscall::SyscallError::PageFault);
            return SplitDispatchDisposition::Complete(Ok(()));
        };
        payload_buf[..len].copy_from_slice(&bytes[..len]);
    }
    let payload = payload_buf;
    let Some(snapshot) = IpcCallDirectSnapshot::build(caller, send_cap, reply_cap, &payload[..len])
    else {
        // The only way the owned snapshot refuses is a length its buffer cannot carry, which the
        // preflight already bounded — so this is the broad path's `Message::with_header` failure,
        // and it answers the same `InvalidArgs`.
        crate::yarm_log!(
            "IPCCALL_DIRECT_REFUSED_PRE_LOCK tid={} send_cap={} reason=snapshot_build err=InvalidArgs copies=0 enqueues=0 mutations=0 result=ok",
            tid,
            send_cap.0
        );
        frame.set_err(crate::kernel::syscall::SyscallError::InvalidArgs.code());
        REQUEST_COUNTERS.note_failed(crate::kernel::syscall::SyscallError::InvalidArgs);
        return SplitDispatchDisposition::Complete(Ok(()));
    };
    // Consume the acknowledgement published for EXACTLY this endpoint incarnation, at most
    // once (Stage 199D endpoint-keyed, generation-bearing store). A pair belonging to any
    // other endpoint, any other endpoint generation, or already consumed by a duplicate
    // trap yields `None` — no copy result is used, nothing is mutated, NR6 stays legacy.
    let Some((ack, ack_seq)) = crate::kernel::boot::ipccall_direct_ack::claim(send_eidx, send_egen)
    else {
        // U9-IPC-RESIDUAL1 §2 — CHOOSE THE MODE, before any mutation.
        //
        // "No claimable acknowledgement" is not a generic decline. Together with an endpoint
        // that has no parked receiver it is the positive signature of the BUFFERED shape: the
        // server has not blocked in recv-v2, so there is nothing to deliver into and the
        // request is enqueued for a later receive. That is what the authoritative send path
        // does for this exact state, and treating the signature as "let the broad path have it"
        // is what left one terminal-broad NR 6 entry on every boot of every port.
        //
        // The queued lane decides its own admission from live state inside the acquisition that
        // publishes, so a decline there is still mutation-free.
        return try_split_ipccall_queued_into_frame(
            shared,
            cpu,
            frame,
            tid,
            asid_raw,
            send_eidx,
            send_egen,
            send_cap,
            reply_cap,
            &payload[..len],
        );
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
    // U9-IPC-RESIDUAL2 §2 — A PRISTINE TRANSACTION OUTCOME IS A LANE QUESTION, NOT A FALLBACK.
    //
    // `DeclinedBeforeMutation` is the transaction's own word for "nothing was delivered and
    // nothing was left behind": `WouldBlock`, `LeaseNotClaimed`, `CallerGone`, `SendEndpoint`,
    // `ReplyEndpoint`, `EndpointGenerationChanged`, `RecordFull`, `ServerCnodeMissing`,
    // `MintFailed`. `apply_direct_disposition` answers `None` for it, which used to mean the
    // broad dispatcher — and the broad dispatcher's answer for a request it cannot hand
    // straight to a blocked server is not an error at all. It is the buffered enqueue, or, if
    // the queue is full, parking the sender.
    //
    // So a pristine outcome goes to the lane that implements exactly that, with the request as
    // re-sendable as it arrived. The lane re-validates the endpoint, its incarnation and its
    // admission inside the acquisitions that mutate, so nothing here is assumed to still hold.
    //
    // Nothing is counted at this hand-off: the attempt is still in flight and the lane it moves
    // to supplies the one terminal bucket the balance invariant requires.
    if matches!(
        disposition,
        crate::kernel::direct_disposition::DirectDisposition::DeclinedBeforeMutation
    ) {
        crate::yarm_log!(
            "IPCCALL_DIRECT_TO_BUFFERED tid={} endpoint={} endpoint_generation={} reason={:?} result=ok",
            tid,
            send_eidx,
            send_egen,
            outcome.as_ref().err()
        );
        return try_split_ipccall_queued_into_frame(
            shared,
            cpu,
            frame,
            tid,
            asid_raw,
            send_eidx,
            send_egen,
            send_cap,
            reply_cap,
            &payload[..len],
        );
    }
    crate::kernel::direct_ipc_counters::note_disposition(&REQUEST_COUNTERS, disposition);
    // Stage 199D HARD-STOP C: the frame is encoded by the SHARED encoder, which reproduces
    // the legacy `set_ok(0, 0, 0)` + `encode_transfer_cap_ret(frame, None)` success lanes
    // (`ret2 = SYSCALL_NO_TRANSFER_CAP`), zeroes every lane on failure, and leaves the frame
    // untouched on a decline so the legacy global-lock IpcCall runs against a pristine frame.
    // NR6 is request-send-only: success returns now (the caller blocks via a later recv).
    //
    // U9-IPC-RESIDUAL2 §2: the decline arm above is taken before this point, so the encoder can
    // only be reached with `Completed` or `Failed`, both of which it answers `Some(())`.
    match crate::kernel::direct_disposition::apply_direct_disposition(frame, disposition) {
        Some(()) => SplitDispatchDisposition::Complete(Ok(())),
        None => {
            // Unreachable: the decline arm above returned already, and the encoder answers
            // `Some` for `Completed` and `Failed` alike. Named rather than unwrapped, so the
            // impossible case can never become a silent fall-through.
            debug_assert!(false, "the encoder was reached with a decline");
            SplitDispatchDisposition::Complete(Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::Internal,
            )))
        }
    }
}

/// U9-IPC-RESIDUAL1 §2/§3, U9-IPC-RESIDUAL2 §2/§3 — **NR 6's BUFFERED lane**, entirely off the
/// broad lock, and TOTAL.
///
/// Reached whenever the direct hand-off cannot serve the request: no claimable acknowledgement,
/// or a transaction outcome that delivered nothing and left nothing behind. Both of those are the
/// same question — "the server is not waiting for this right now" — and the broad handler's
/// answer to it is not an error. It is `kernel.ipc_send(cap, msg)`, which delivers to a parked
/// receiver, enqueues into a buffered endpoint, or parks the sender when the queue is full.
///
/// This lane reproduces those three arms through the owners that already implement them; nothing
/// here is a second implementation of the reply-record lifecycle, the envelope stash, the message
/// framing, the endpoint queue, the blocked-waiter delivery or the blocking-send transaction.
///
/// # The three arms, and who owns each
///
/// | live state | broad behaviour | owner here |
/// |---|---|---|
/// | a recv-v2 blocked receiver | direct delivery | `produce_blocked_waiter_reply_cap_delivery_split` |
/// | no waiter, room in the queue | enqueue | `enqueue_request_if_no_waiter_split` |
/// | no waiter, queue full | **park the sender** | `BlockingSendCommitSnapshot` + the U6 drain |
///
/// # Ownership
///
/// **Publication point: the enqueue** — or, on the full-queue arm, the sender-waiter commit.
/// `enqueue_request_if_no_waiter_split` validates the exact endpoint incarnation, re-asks the
/// admission question and enqueues in ONE rank-3 acquisition, so neither a receiver that parks
/// nor a slot that is destroyed and reissued between the pre-lock reads and the publication can
/// be missed.
///
/// Before that point this transaction owns — and on every refusal returns — exactly three
/// things, in reverse order of acquisition:
///
/// | resource | acquired | compensation |
/// |---|---|---|
/// | transfer envelope | `stash_transfer_envelope_split` | `take_transfer_envelope_facts_split` |
/// | caller `Reply` cap | `mint_capability_with_memory_ref_split` | `rollback_minted_cap_split` |
/// | reply record slot | `reserve_reply_record_split` | `free_reserved_reply_record_split` |
///
/// The stash only RESOLVES the source capability, so a compensated request is genuinely
/// re-sendable: nothing userspace holds was consumed. After a successful publication nothing is
/// compensated — the message is the receiver's and the caller owns its reply capability, exactly
/// as on the broad path.
///
/// The park arm is the one publication that can still be refused after the resources are taken,
/// because the rank-ordered commit re-validates the sender's incarnation, its runnability and the
/// endpoint. That refusal is not this function's to perform — the commit runs in the post-lock
/// drain — so all three resources travel to it: the envelope in `transfer_envelope`, and the mint
/// and the record in `reply_authority`, each settled there through the very same owners named
/// above. §3's requirement that a refusal settle all three is met by the drain, not by hoping the
/// refusal cannot happen.
///
/// No responder is bound on the enqueue arm (there is no waiter), so no `ServerReplyLink` is
/// registered — the same decision the broad owner makes when `endpoint_waiter_tid` is `None`.
#[cfg(not(feature = "hosted-dev"))]
#[allow(clippy::too_many_arguments)]
fn try_split_ipccall_queued_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
    tid: u64,
    asid_raw: u64,
    send_eidx: usize,
    send_egen: u64,
    send_cap: crate::kernel::capabilities::CapId,
    reply_recv_cap: crate::kernel::capabilities::CapId,
    payload: &[u8],
) -> SplitDispatchDisposition {
    use crate::kernel::boot::{EndpointSendAdmission, QueuedRequestOutcome};
    use crate::kernel::capabilities::{CapObject, CapRights, Capability};
    use crate::kernel::direct_ipc_counters::REQUEST as REQUEST_COUNTERS;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::syscall::SyscallError;
    use SplitDispatchDisposition as D;

    // U9-IPC-RESIDUAL2 §2 — a refusal is an ANSWER now, not a hand-over.
    //
    // Every arm below reached this state having mutated nothing that is not compensated first,
    // so the canonical error is exactly the one the broad handler would have raised from the
    // corresponding `?`. The frame carries it; the counters record one terminal.
    let refuse = |frame: &mut TrapFrame, err: SyscallError, reason: &str| -> D {
        REQUEST_COUNTERS.note_failed(err);
        crate::yarm_log!(
            "IPCCALL_QUEUED_SPLIT_REFUSED reason={} tid={} endpoint={} endpoint_generation={} err={:?} result=ok",
            reason,
            tid,
            send_eidx,
            send_egen,
            err
        );
        frame.set_err(err.code());
        D::Complete(Ok(()))
    };

    // (B) rank 2 → 4 read — the caller's reply-receive endpoint, through THE validation owner,
    // in the broad handler's order. The direct route already validated it for this trap; asking
    // again here costs one lock-free read and keeps the lane correct when it is entered from the
    // transaction-decline path, where the world may have moved.
    let reply_endpoint =
        match shared.validate_endpoint_right_split_read(tid, reply_recv_cap, CapRights::RECEIVE) {
            Ok(object) => object,
            Err(err) => return refuse(frame, err, "reply_cap"),
        };

    // (C) rank 2 read — the caller's incarnation and its CNode, the mint target.
    let caller_asid = crate::kernel::vm::Asid(asid_raw as u16);
    let Some(caller_cnode) = shared.task_cnode_split(tid) else {
        // `create_reply_cap_for_caller` resolves the caller's cnode too, and a caller with none
        // is a task that has gone; the broad path answers its `KernelError::TaskMissing`.
        return refuse(frame, SyscallError::Internal, "caller_cnode_missing");
    };

    // (A) rank 3 read — which of the three arms this request belongs to.
    //
    // A parked recv-v2 receiver is the DELIVERY arm, not a decline: `ipc_send_routed` hands the
    // message straight to it. U9-IPC-RESIDUAL1 declined here, which is how a receiver parking
    // between the acknowledgement probe and this read sent an ordinary NR 6 to the broad path.
    if let EndpointSendAdmission::ReceiverWaiter(waiter) =
        shared.endpoint_send_admission_split_read(send_eidx)
        && shared.is_task_recv_v2_blocked_split_read(waiter.tid.0)
    {
        return nr6_deliver_to_blocked_waiter(
            shared,
            frame,
            tid,
            caller_asid,
            caller_cnode,
            send_eidx,
            send_egen,
            reply_endpoint,
            waiter.tid,
            payload,
        );
    }

    // (R) rank 3 — reserve the record. `responder_tid` is `None`: nobody is parked, which is
    // precisely why this is the queued shape.
    let Ok((slot, generation)) =
        shared.reserve_reply_record_split(ThreadId(tid), caller_asid, reply_endpoint, None, None)
    else {
        // `create_reply_cap_for_caller` maps a full record table to `KernelError::CapabilityFull`,
        // which the syscall boundary reports as `Internal`. Same table, same exhaustion, same
        // answer — given here without entering the broad acquisition to be told.
        return refuse(frame, SyscallError::Internal, "reply_record_full");
    };
    let reply_object = CapObject::Reply {
        index: slot,
        generation,
    };

    // (M) rank 4 — mint the caller's one-shot Reply cap. A `Reply` object holds no memory
    // reference, so the rank-6 half of this seam is a no-op for it.
    let Ok(reply_cap_id) = shared.mint_capability_with_memory_ref_split(
        caller_cnode,
        Capability::new(reply_object, CapRights::SEND),
    ) else {
        shared.free_reserved_reply_record_split(slot, generation);
        return refuse(frame, SyscallError::Internal, "reply_cap_mint_failed");
    };

    // (P) rank 3 — persist the minted CapId into THAT record incarnation.
    if !shared.persist_reply_caller_cap_split(slot, generation, reply_cap_id) {
        shared.rollback_minted_cap_split(caller_cnode, reply_cap_id, reply_object);
        shared.free_reserved_reply_record_split(slot, generation);
        return refuse(frame, SyscallError::Internal, "reply_record_recycled");
    }

    // (S) rank 3 (sequential) — stash the envelope that carries the reply cap to the receiver,
    // bound to the SEND endpoint and to no particular receiver, exactly as the broad stash binds
    // it when there is no waiter.
    let send_endpoint = CapObject::Endpoint {
        index: send_eidx,
        generation: send_egen,
    };
    let unwind_mint = |shared: &SharedKernel| {
        shared.rollback_minted_cap_split(caller_cnode, reply_cap_id, reply_object);
        shared.free_reserved_reply_record_split(slot, generation);
    };
    let Ok(stashed) = shared.stash_transfer_envelope_split(
        ThreadId(tid),
        reply_cap_id,
        send_endpoint,
        None,
        None,
    ) else {
        unwind_mint(shared);
        // `stash_transfer_handle(..)?` in the broad handler; a stash that cannot resolve the
        // capability answers `InvalidCapability`, as NR 1's split route answers it.
        return refuse(
            frame,
            SyscallError::InvalidCapability,
            "envelope_stash_failed",
        );
    };
    let unwind_all = |shared: &SharedKernel| {
        let _ = shared.take_transfer_envelope_facts_split(stashed.handle, send_eidx, ThreadId(tid));
        unwind_mint(shared);
    };

    // (F) pure — THE request framing, shared with the broad handler.
    let Ok(msg) =
        crate::kernel::syscall::ipc_abi::frame_call_request_message(tid, payload, stashed.handle)
    else {
        unwind_all(shared);
        // `Message::with_header(..).map_err(|_| SyscallError::InvalidArgs)?`.
        return refuse(frame, SyscallError::InvalidArgs, "message_framing_failed");
    };

    // (Q) rank 3 — PUBLISH. The endpoint incarnation and the admission question are both terms
    // of this same acquisition.
    match shared.enqueue_request_if_no_waiter_split(send_eidx, send_egen, msg) {
        QueuedRequestOutcome::Enqueued => {
            crate::yarm_log!(
                "IPCCALL_QUEUED_SPLIT_OK tid={} endpoint={} endpoint_generation={} reply_cap={} record_index={} record_generation={} len={} result=ok",
                tid,
                send_eidx,
                send_egen,
                reply_cap_id.0,
                slot,
                generation,
                payload.len()
            );
            crate::kernel::direct_ipc_counters::note_disposition(
                &REQUEST_COUNTERS,
                crate::kernel::direct_disposition::DirectDisposition::Completed,
            );
            // A legacy receiver waiting on this endpoint is woken through the one shared owner,
            // exactly as NR 1's split enqueue arm wakes it. `ipc_send_routed` performs the same
            // wake after its own enqueue, and omitting it would leave a pre-recv-v2 waiter asleep
            // behind a message that is now queued for it.
            let _ = shared.wake_waiter_for_endpoint_split(cpu, send_eidx);
            // The same three return lanes the broad handler ends with: `set_ok(0, 0, 0)` then
            // `encode_transfer_cap_ret(frame, None)`. `IpcCall` is request-send only — the
            // caller receives the reply through its own explicit receive on `reply_recv_cap`.
            // `Completed` always encodes, so the encoder's `Option` is `Some` here. Named
            // rather than unwrapped: an impossible case that is spelled out cannot become a
            // silent fall-through if the encoder's contract ever changes.
            match crate::kernel::direct_disposition::apply_direct_disposition(
                frame,
                crate::kernel::direct_disposition::DirectDisposition::Completed,
            ) {
                Some(()) => D::Complete(Ok(())),
                None => {
                    debug_assert!(false, "a completed disposition always encodes");
                    D::Complete(Err(TrapHandleError::Syscall(SyscallError::Internal)))
                }
            }
        }
        // U9-IPC-RESIDUAL2 §2 — A FULL ENDPOINT PARKS THE SENDER. It does not answer.
        //
        // This is the arm U9-IPC-RESIDUAL1 called "the BLOCKING origin ... out of scope" and
        // handed to the broad path. The directive is explicit that answering `WouldBlock` here
        // would be wrong: the broad operation blocks the caller until the queue drains, and a
        // caller told `WouldBlock` instead would spin or fail a send that is merely delayed.
        //
        // Nothing is unwound. The message — carrying the reply capability's transfer envelope —
        // rides with the sender waiter exactly as it does on the in-lock route, and the caller
        // keeps the `Reply` cap it will use when the reply arrives. The rank-ordered commit is
        // the EXISTING U6 transaction; its refusal path settles all three resources.
        QueuedRequestOutcome::QueueFull => nr6_park_sender(
            shared,
            cpu,
            tid,
            caller_asid,
            caller_cnode,
            send_eidx,
            send_egen,
            send_cap,
            msg,
            stashed.handle,
            reply_cap_id,
            reply_object,
            slot,
            generation,
            unwind_all,
        ),
        // A receiver parked inside the publication window. Compensate and take the delivery arm,
        // which is where the broad path would have gone had it seen this state.
        QueuedRequestOutcome::WaiterAppeared => {
            unwind_all(shared);
            match shared.endpoint_send_admission_split_read(send_eidx) {
                EndpointSendAdmission::ReceiverWaiter(waiter)
                    if shared.is_task_recv_v2_blocked_split_read(waiter.tid.0) =>
                {
                    nr6_deliver_to_blocked_waiter(
                        shared,
                        frame,
                        tid,
                        caller_asid,
                        caller_cnode,
                        send_eidx,
                        send_egen,
                        reply_endpoint,
                        waiter.tid,
                        payload,
                    )
                }
                // A SENDER waiter, or a receiver that is not recv-v2 blocked. Both mean the
                // queue's ordering belongs to the full send, and this request must go behind
                // what is already there — which is what parking does.
                _ => {
                    crate::yarm_log!(
                        "IPCCALL_QUEUED_SPLIT_ORDERING tid={} endpoint={} reason=waiter_appeared result=would_block",
                        tid,
                        send_eidx
                    );
                    refuse(frame, SyscallError::WouldBlock, "waiter_ordering")
                }
            }
        }
        QueuedRequestOutcome::EndpointMissing => {
            unwind_all(shared);
            refuse(frame, SyscallError::WrongObject, "endpoint_missing")
        }
        // U9-IPC-RESIDUAL2 §3 — the slot was destroyed and reissued between preparation and
        // publication. `resolve_endpoint_index` answers `StaleCapability` for exactly this, which
        // the syscall boundary reports as `WrongObject`.
        QueuedRequestOutcome::EndpointIncarnationChanged { expected, observed } => {
            unwind_all(shared);
            crate::yarm_log!(
                "IPCCALL_QUEUED_SPLIT_INCARNATION tid={} endpoint={} expected={} observed={} enqueues=0 result=ok",
                tid,
                send_eidx,
                expected,
                observed.unwrap_or(u64::MAX)
            );
            refuse(
                frame,
                SyscallError::WrongObject,
                "endpoint_incarnation_changed",
            )
        }
    }
}

/// U9-IPC-RESIDUAL2 §2 — NR 6's DELIVERY arm: a recv-v2 blocked receiver is parked on this
/// endpoint, so the request is handed straight to it.
///
/// This is `handle_ipc_call`'s `IpcEndpointSendResult::ReceiverWaiterFound` branch, composed from
/// the same producer Stage 188E wired for it. The reply record is reserved with the responder
/// BOUND — that is the whole difference from the enqueue arm, and it is what registers the
/// reverse link the reply will later travel back along.
#[cfg(not(feature = "hosted-dev"))]
#[allow(clippy::too_many_arguments)]
fn nr6_deliver_to_blocked_waiter(
    shared: &SharedKernel,
    frame: &mut TrapFrame,
    tid: u64,
    caller_asid: crate::kernel::vm::Asid,
    caller_cnode: crate::kernel::capabilities::CNodeId,
    send_eidx: usize,
    send_egen: u64,
    reply_endpoint: crate::kernel::capabilities::CapObject,
    waiter_tid: crate::kernel::ipc::ThreadId,
    payload: &[u8],
) -> SplitDispatchDisposition {
    use crate::kernel::capabilities::{CapObject, CapRights, Capability};
    use crate::kernel::direct_ipc_counters::REQUEST as REQUEST_COUNTERS;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::syscall::SyscallError;
    use SplitDispatchDisposition as D;

    let refuse = |frame: &mut TrapFrame, err: SyscallError, reason: &str| -> D {
        REQUEST_COUNTERS.note_failed(err);
        crate::yarm_log!(
            "IPCCALL_DELIVER_SPLIT_REFUSED reason={} tid={} endpoint={} waiter_tid={} err={:?} result=ok",
            reason,
            tid,
            send_eidx,
            waiter_tid.0,
            err
        );
        frame.set_err(err.code());
        D::Complete(Ok(()))
    };

    // The record binds the responder, exactly as `create_reply_cap_for_caller(.., responder_tid)`
    // does when `endpoint_waiter_tid` answered `Some`.
    let Ok((slot, generation)) = shared.reserve_reply_record_split(
        ThreadId(tid),
        caller_asid,
        reply_endpoint,
        Some(waiter_tid),
        shared.task_asid_opt_split_read(waiter_tid.0),
    ) else {
        return refuse(frame, SyscallError::Internal, "reply_record_full");
    };
    let reply_object = CapObject::Reply {
        index: slot,
        generation,
    };
    let Ok(reply_cap_id) = shared.mint_capability_with_memory_ref_split(
        caller_cnode,
        Capability::new(reply_object, CapRights::SEND),
    ) else {
        shared.free_reserved_reply_record_split(slot, generation);
        return refuse(frame, SyscallError::Internal, "reply_cap_mint_failed");
    };
    let unwind_mint = |shared: &SharedKernel| {
        shared.rollback_minted_cap_split(caller_cnode, reply_cap_id, reply_object);
        shared.free_reserved_reply_record_split(slot, generation);
    };
    if !shared.persist_reply_caller_cap_split(slot, generation, reply_cap_id) {
        unwind_mint(shared);
        return refuse(frame, SyscallError::Internal, "reply_record_recycled");
    }
    // The envelope is bound to the waiter, as `stash_transfer_handle` binds it when a waiter was
    // present at stash time — the binding the error paths must match to take it back.
    let send_endpoint = CapObject::Endpoint {
        index: send_eidx,
        generation: send_egen,
    };
    let Ok(stashed) = shared.stash_transfer_envelope_split(
        ThreadId(tid),
        reply_cap_id,
        send_endpoint,
        Some(waiter_tid),
        None,
    ) else {
        unwind_mint(shared);
        return refuse(
            frame,
            SyscallError::InvalidCapability,
            "envelope_stash_failed",
        );
    };
    let unwind_all = |shared: &SharedKernel| {
        let _ = shared.take_transfer_envelope_facts_split(stashed.handle, send_eidx, waiter_tid);
        unwind_mint(shared);
    };
    let Ok(msg) =
        crate::kernel::syscall::ipc_abi::frame_call_request_message(tid, payload, stashed.handle)
    else {
        unwind_all(shared);
        return refuse(frame, SyscallError::InvalidArgs, "message_framing_failed");
    };
    crate::yarm_log!(
        "IPC_SEND_REPLY_CAP_BOUNDARY_SPLIT_BEGIN waiter_tid={} endpoint={}",
        waiter_tid.0,
        send_eidx
    );
    match shared.produce_blocked_waiter_reply_cap_delivery_split(waiter_tid.0, send_eidx, &msg) {
        Ok(true) => {
            crate::yarm_log!(
                "IPCCALL_DELIVER_SPLIT_OK tid={} endpoint={} waiter_tid={} reply_cap={} record_index={} record_generation={} len={} result=ok",
                tid,
                send_eidx,
                waiter_tid.0,
                reply_cap_id.0,
                slot,
                generation,
                payload.len()
            );
            crate::kernel::direct_ipc_counters::note_disposition(
                &REQUEST_COUNTERS,
                crate::kernel::direct_disposition::DirectDisposition::Completed,
            );
            // The CALLER's syscall is finished — `IpcCall` is request-send only. The drain does
            // the receiver's copy, materialization, slot clear and single wake.
            frame.set_ok(0, 0, 0);
            frame.set_ret2(
                usize::try_from(crate::kernel::syscall::SYSCALL_NO_TRANSFER_CAP).unwrap_or(0),
            );
            D::PostWorkCommitted {
                finalize_syscall: true,
            }
        }
        // The producer declined having consumed nothing: the waiter is not the shape it claimed
        // to be. Compensate and answer, rather than re-running the whole call under the broad
        // lock — which would mint a SECOND reply capability for one syscall.
        Ok(false) => {
            unwind_all(shared);
            refuse(frame, SyscallError::WouldBlock, "no_delivery_owner")
        }
        Err(err) => {
            unwind_all(shared);
            refuse(frame, err, "delivery_error")
        }
    }
}

/// U9-IPC-RESIDUAL2 §2/§3 — NR 6's PARK arm: the endpoint queue is full, which is the blocking
/// origin, so the caller becomes a sender waiter carrying its message.
///
/// The proposal is stashed for the post-lock drain rather than committed here, for the same
/// reason NR 1's park arm stashes it: the commit clears this CPU's `current`, and the trap
/// wrapper must learn that through `PostWorkCommitted { finalize_syscall: false }` so it does
/// NOT write a syscall result or advance the PC of a task that is now parked. A parked sender's
/// answer arrives from the completion its waker publishes.
///
/// All three of this transaction's resources travel with the proposal, so the drain's refusal
/// path is exactly as complete as the pre-publication compensation: the envelope through
/// `settle_blocked_send_envelope_split`, and the mint and record through
/// `rollback_minted_cap_split` and `free_reserved_reply_record_split`.
#[cfg(not(feature = "hosted-dev"))]
#[allow(clippy::too_many_arguments)]
fn nr6_park_sender(
    shared: &SharedKernel,
    cpu: CpuId,
    tid: u64,
    caller_asid: crate::kernel::vm::Asid,
    caller_cnode: crate::kernel::capabilities::CNodeId,
    send_eidx: usize,
    send_egen: u64,
    send_cap: crate::kernel::capabilities::CapId,
    msg: crate::kernel::ipc::Message,
    envelope_handle: u64,
    reply_cap_id: crate::kernel::capabilities::CapId,
    reply_object: crate::kernel::capabilities::CapObject,
    record_index: usize,
    record_generation: u64,
    unwind_all: impl Fn(&SharedKernel),
) -> SplitDispatchDisposition {
    use crate::kernel::direct_ipc_counters::REQUEST as REQUEST_COUNTERS;
    use crate::kernel::syscall::SyscallError;
    use SplitDispatchDisposition as D;

    let cpu_idx = cpu.0 as usize;
    // The publication route needs a live post-lock drainer on this CPU and an empty stash; the
    // same four conditions the broad producer checks, asked here because there is no broad
    // producer on this path. When any of them does not hold there is nothing that would ever run
    // the commit, so the request is compensated and answered rather than silently dropped.
    let drainer_live = crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu_idx]
        .load(core::sync::atomic::Ordering::Relaxed);
    // SAFETY: local-CPU trap path, interrupts disabled — the same discipline as every store.
    let stash_free =
        !unsafe { crate::kernel::boot::DISPATCH_POST_WORK_STASH[cpu_idx].is_occupied() };
    if !crate::kernel::boot::queue_advancing_dispatch_enabled()
        || !drainer_live
        || !stash_free
        || crate::kernel::boot::d2_send_dispatch_is_deferred(cpu_idx)
    {
        unwind_all(shared);
        REQUEST_COUNTERS.note_failed(SyscallError::WouldBlock);
        crate::yarm_log!(
            "IPCCALL_PARK_SPLIT_REFUSED tid={} endpoint={} reason=no_publication_route drainer={} stash_free={} result=ok",
            tid,
            send_eidx,
            u8::from(drainer_live),
            u8::from(stash_free)
        );
        // NR 1's park arm answers the same `WouldBlock` for the same condition. The frame is
        // encoded by the trap wrapper from this typed error, as it is for every `Complete(Err)`.
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::WouldBlock)));
    }
    let snapshot = crate::kernel::dispatch_post_work::BlockingSendCommitSnapshot {
        cpu,
        sender_tid: tid,
        sender_asid: caller_asid,
        endpoint_idx: send_eidx,
        endpoint_generation: send_egen,
        send_cap,
        msg,
        // `IpcCall` carries no send timeout in its ABI, so the park is untimed — the same
        // `deadline: None` the broad `kernel.ipc_send(cap, msg)` produces for it.
        deadline: None,
        transfer_envelope: Some(
            crate::kernel::dispatch_post_work::BlockingSendEnvelopeCleanup {
                handle: envelope_handle,
                endpoint_idx: send_eidx,
                // The envelope was stashed unbound (no waiter), so the cleanup identity is the
                // sender, matching `stash_bound_receiver_tid.unwrap_or(sender)`.
                cleanup_tid: crate::kernel::ipc::ThreadId(tid),
            },
        ),
        reply_authority: Some(
            crate::kernel::dispatch_post_work::BlockingSendReplyAuthorityCleanup {
                caller_cnode,
                reply_cap_id,
                reply_object,
                record_index,
                record_generation,
            },
        ),
    };
    // SAFETY: local-CPU trap path, interrupts disabled, no concurrent access — identical
    // discipline to every other producer's store.
    unsafe {
        crate::kernel::boot::DISPATCH_POST_WORK_STASH[cpu_idx].store(
            crate::kernel::dispatch_post_work::DispatchPostWork::BlockingSendCommit(snapshot),
        );
    }
    // The park IS this attempt's terminal: the request was published as a sender waiter and the
    // caller is no longer running. Counted as completed for the same reason the enqueue arm is —
    // the syscall did what NR 6 promises, and its reply will arrive on the caller's own receive.
    crate::kernel::direct_ipc_counters::note_disposition(
        &REQUEST_COUNTERS,
        crate::kernel::direct_disposition::DirectDisposition::Completed,
    );
    crate::yarm_log!(
        "IPCCALL_PARK_SPLIT_PUBLISHED tid={} endpoint={} endpoint_generation={} reply_cap={} record_index={} record_generation={} result=blocking_publication_pending",
        tid,
        send_eidx,
        send_egen,
        reply_cap_id.0,
        record_index,
        record_generation
    );
    D::PostWorkCommitted {
        finalize_syscall: false,
    }
}

#[cfg(feature = "hosted-dev")]
fn try_split_ipccall_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> SplitDispatchDisposition {
    SplitDispatchDisposition::NotHandled
}

#[cfg(feature = "hosted-dev")]
#[allow(dead_code)]
fn try_split_ipccall_direct_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    // Hosted: the off-lock user-read seam uses the direct map (real targets only). The
    // drain + transaction are exercised directly by the stage199a2b2f hosted tests.
    None
}

/// Stage 199A2B3: intercept `IpcReply` (NR 7) BEFORE the broad `KernelState` lock and drive the
/// accepted off-lock direct-reply transaction.
///
/// U9-YIELD2 §1: the "(proof-gated, default-OFF)" label this carried is stale for the same reason
/// NR 6's was — admission short-circuits on a production term that is true on all three
/// architectures, so this route is reached on every ordinary boot.
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
) -> Result<(), TrapHandleError> {
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
            return Ok(());
        }
        return nr7_refuse_preflight(shared, cpu, frame, verdict, tid, reply_cap);
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
            return Ok(());
        }
        if !crate::kernel::boot::ipcreply_direct_ack::is_claimable(reply_eidx, reply_egen) {
            crate::kernel::boot::ipcreply_direct_smp_note_duplicate_refused();
            crate::yarm_log!(
                "IPCREPLY_DIRECT_SMP_DUPLICATE_REFUSED arch=x86_64 reason=consumed_barrier reply_copies=1 caller_wakes=1 ipis=1 result=ok"
            );
            frame.set_err(SyscallError::WrongObject.code());
            return Ok(());
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
        // Unreachable: eligibility required a resolved record. Fail closed rather than assume —
        // U9-IPC-RESIDUAL2 §2: with a typed answer, not by handing the trap to the broad path,
        // which would resolve the same capability and reach the same conclusion one lock later.
        Err(err) => {
            return nr7_refuse(
                frame,
                tid,
                0,
                0,
                crate::kernel::syscall::SyscallError::from(err),
                "reply_object_unresolved",
            );
        }
    };
    // Source copy OFF-LOCK (no broad/ranked lock held). A fault mutates nothing. As on the
    // NR6 twin, every decline from here to the ack claim is eligible-but-pre-transaction and
    // is counted as such — the oracle server's bounded pre-acknowledgement retries live here.
    // U9-IPC-RESIDUAL2 §2 — the three payload shapes, as on the NR 6 twin. `handle_ipc_reply`
    // takes the register source for a kernel-ASID replier, sends an empty payload happily, and
    // answers a faulting copy with `record_user_fault(.., FaultAccess::Read)` — never
    // `InvalidArgs`, and never a fall-through.
    let mut payload_buf = [0u8; crate::kernel::ipc::Message::MAX_PAYLOAD];
    if asid_raw == 0 {
        let Some(regs) = crate::kernel::syscall::split_inline_payload_from_frame(frame, len) else {
            return nr7_refuse(
                frame,
                tid,
                rec_idx,
                rec_gen,
                crate::kernel::syscall::SyscallError::InvalidArgs,
                "inline_payload",
            );
        };
        payload_buf[..len].copy_from_slice(&regs[..len]);
    } else if len > 0 {
        let Some(bytes) = shared.copy_from_user_asid_split_read(asid_raw, user_ptr, len) else {
            let _ = shared.record_split_source_read_fault(cpu, frame, user_ptr);
            crate::yarm_log!(
                "IPCREPLY_DIRECT_SOURCE_FAULT tid={} record_index={} record_generation={} user_ptr={:#x} len={} access=read reply_copies=0 caller_wakes=0 mutations=0 result=ok",
                tid,
                rec_idx,
                rec_gen,
                user_ptr,
                len
            );
            REPLY_COUNTERS.note_failed(crate::kernel::syscall::SyscallError::PageFault);
            return Ok(());
        };
        payload_buf[..len].copy_from_slice(&bytes[..len]);
    }
    let payload = payload_buf;
    let Some(snapshot) = IpcReplyDirectSnapshot::build(replier, reply_cap, &payload[..len]) else {
        return nr7_refuse(
            frame,
            tid,
            rec_idx,
            rec_gen,
            crate::kernel::syscall::SyscallError::InvalidArgs,
            "snapshot_build",
        );
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
    ) {
        // U9-IPC-RESIDUAL1 §2 — the cap-bearing queued shape is served, not declined.
        //
        // The note this replaces said a cap-bearing reply to an UNBLOCKED caller would be
        // "enqueued with a capability nothing would materialize". That is not what the source
        // does: the queued message carries `FLAG_CAP_TRANSFER_PLAIN`, and the receive-side
        // materialization arm every route reaches — broad and split alike — treats that flag
        // identically to `FLAG_CAP_TRANSFER` and materializes the envelope on the caller's next
        // receive. The capability is delivered by the same owner that delivers every other
        // queued transfer.
        //
        // So the mode is chosen on the terminal alone, and the capability decides only which
        // framing the queued message carries.
        crate::kernel::direct_eligibility::DirectReplyMode::QueueUnblocked
    } else {
        // An armed terminal with no claimable acknowledgement is neither delivery mode: the
        // record is mid-transaction or settling. Refuse pre-mutation rather than guess.
        // U9-IPC-RESIDUAL2 §2 — an armed terminal with no claimable acknowledgement means the
        // record is mid-transaction or settling: another claimant owns this one-shot and will
        // complete it. The reply authority this replier holds is therefore spent, which is the
        // same conclusion a duplicate reply reaches and gets the same canonical answer. Handing
        // it to the broad path would have it resolve the same record and refuse identically.
        crate::yarm_log!(
            "IPCREPLY_DIRECT_MODE_INDETERMINATE terminal={:?} tid={} record_index={} record_generation={}",
            facts.terminal,
            tid,
            rec_idx,
            rec_gen
        );
        return nr7_refuse(
            frame,
            tid,
            rec_idx,
            rec_gen,
            crate::kernel::syscall::SyscallError::WrongObject,
            "mode_indeterminate",
        );
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
            // `transfer_cap_arg(kernel, frame)?` in the broad handler: the argument says a
            // capability is present and it does not decode.
            return nr7_refuse(
                frame,
                tid,
                rec_idx,
                rec_gen,
                crate::kernel::syscall::SyscallError::InvalidCapability,
                "transfer_cap_arg",
            );
        };
        // The caller this reply settles, read from the record itself — never re-derived.
        let Some(caller) = shared.reply_record_caller_split_read(rec_idx, rec_gen) else {
            return nr7_refuse(
                frame,
                tid,
                rec_idx,
                rec_gen,
                crate::kernel::syscall::SyscallError::WrongObject,
                "record_caller_missing",
            );
        };
        // (1) The one-shot authority identities, snapshotted BEFORE any mutation so a recycled
        // record can never hand out another transaction's slots.
        let Some(authority) = shared.reply_authority_slots_split_read(rec_idx, rec_gen) else {
            return nr7_refuse(
                frame,
                tid,
                rec_idx,
                rec_gen,
                crate::kernel::syscall::SyscallError::WrongObject,
                "authority_slots_missing",
            );
        };
        // (2) Reserve the record for this exact replier: `Available → Reserved`. Refused
        // pre-mutation if the record is not this replier's to answer — the transaction's own
        // `ReservePreconditionFailed`, whose canonical answer is the stale-authority error.
        if !shared.reserve_existing_reply_record_split(rec_idx, rec_gen, replier) {
            return nr7_refuse(
                frame,
                tid,
                rec_idx,
                rec_gen,
                crate::kernel::syscall::SyscallError::WrongObject,
                "reserve_precondition_failed",
            );
        }
        // (3) THE EXCLUSIVE CLAIM, through the same single authority every other terminal
        // claimant uses. A loser mutates nothing and does NOT fall back to the broad
        // dispatcher: the record has an owner and that owner will complete it.
        let claim = shared
            .claim_direct_reply_terminal_split(rec_idx, rec_gen, replier, reply_eidx, reply_egen);
        let terminal_owner = match claim {
            crate::kernel::boot::DirectReplyTerminalClaim::Won(owner) => owner,
            crate::kernel::boot::DirectReplyTerminalClaim::NotArmed => {
                // The ack was claimable, so a terminal must be armed. Restore and refuse — with
                // the same answer a lost claim gets, since either way this replier's authority
                // is not the one that will settle the record.
                let _ = shared.release_reply_record_split(rec_idx, rec_gen);
                return nr7_refuse(
                    frame,
                    tid,
                    rec_idx,
                    rec_gen,
                    crate::kernel::syscall::SyscallError::WrongObject,
                    "terminal_not_armed",
                );
            }
            crate::kernel::boot::DirectReplyTerminalClaim::Lost(class) => {
                let _ = shared.release_reply_record_split(rec_idx, rec_gen);
                REPLY_COUNTERS.note_failed(crate::kernel::syscall::SyscallError::WrongObject);
                crate::yarm_log!(
                    "IPCREPLY_DIRECT_TERMINAL_LOST record_index={} record_generation={} replier_tid={} reason={:?} reply_copies=0 caller_wakes=0 result=ok",
                    rec_idx,
                    rec_gen,
                    tid,
                    class
                );
                frame.set_err(crate::kernel::syscall::SyscallError::WrongObject.code());
                return Ok(());
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
            return nr7_refuse(
                frame,
                tid,
                rec_idx,
                rec_gen,
                crate::kernel::syscall::SyscallError::InvalidCapability,
                "envelope_stash_failed",
            );
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
            return nr7_refuse(
                frame,
                tid,
                rec_idx,
                rec_gen,
                crate::kernel::syscall::SyscallError::InvalidArgs,
                "message_framing_failed",
            );
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
        let produced_outcome = shared.produce_blocked_waiter_ordinary_cap_delivery_split(
            caller.tid.0,
            reply_eidx,
            &msg,
            Some(continuation),
        );
        match produced_outcome {
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
                return match crate::kernel::direct_disposition::apply_direct_disposition(
                    frame,
                    crate::kernel::direct_disposition::DirectDisposition::Completed,
                ) {
                    Some(()) => Ok(()),
                    None => {
                        debug_assert!(false, "a completed disposition always encodes");
                        Err(TrapHandleError::Syscall(
                            crate::kernel::syscall::SyscallError::Internal,
                        ))
                    }
                };
            }
            // Declined or failed having consumed nothing irreversible: hand the reply back
            // re-sendable. The envelope is dropped with it, and the replier still holds the
            // source capability it only ever resolved.
            Ok(false) | Err(_) => {
                // Declined or failed having consumed nothing irreversible. Everything is given
                // back through its exact owner and the reply is answered here: re-running it
                // under the broad lock would stash a SECOND envelope for one syscall.
                let _ = shared.take_transfer_envelope_facts_split(
                    stashed.handle,
                    reply_eidx,
                    caller.tid,
                );
                let _ = shared.release_direct_reply_terminal_split(rec_idx, &terminal_owner);
                let _ = shared.release_reply_record_split(rec_idx, rec_gen);
                let err = match &produced_outcome {
                    Err(e) => *e,
                    Ok(_) => crate::kernel::syscall::SyscallError::WouldBlock,
                };
                return nr7_refuse(frame, tid, rec_idx, rec_gen, err, "cap_delivery_declined");
            }
        }
    } else if mode == crate::kernel::direct_eligibility::DirectReplyMode::QueueUnblocked {
        // U9-IPC-RESIDUAL3 §2 — the lane moved out so a second caller can reach it; see
        // `nr7_queued_reply_lane`. What it does is unchanged.
        return nr7_queued_reply_lane(
            shared,
            cpu,
            frame,
            tid,
            rec_idx,
            rec_gen,
            replier,
            reply_eidx,
            reply_egen,
            facts.transfer_cap_present,
            &payload[..len],
        );
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
            // Eligible, but the exclusive claim was lost. It is deliberately NOT a preflight
            // decline: preflight passed, and the arbitration outcome is reported by the marker
            // below rather than folded into the preflight subset.
            //
            // U9-IPC-RESIDUAL2 §2: counted as a FAILED terminal, not a pre-transaction decline.
            // This arm already answered userspace with a typed error — it was never a decline in
            // the sense that word had, which was "the broad path will now service this trap".
            REPLY_COUNTERS.note_failed(crate::kernel::syscall::SyscallError::WrongObject);
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
            return Ok(());
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
            // unresolved claim. U9-IPC-RESIDUAL1 §1: it is still a TERMINAL — a typed answer
            // given to userspace — so it lands in a bucket like every other one, or the balance
            // invariant silently tolerates a shape it cannot see.
            crate::kernel::direct_ipc_counters::note_disposition(
                &REPLY_COUNTERS,
                crate::kernel::direct_disposition::DirectDisposition::Failed(
                    crate::kernel::syscall::SyscallError::WrongObject,
                ),
            );
            frame.set_err(crate::kernel::syscall::SyscallError::WrongObject.code());
            return Ok(());
        }
        // U9-IPC-RESIDUAL2 §2 — the claim was restored, so the record is exactly as it was
        // found and this replier's authority is unspent. It is still answered here: the
        // acknowledgement it needs is gone, and the broad path would resolve the same record
        // and reach the same refusal.
        return nr7_refuse(
            frame,
            tid,
            rec_idx,
            rec_gen,
            crate::kernel::syscall::SyscallError::WrongObject,
            "ack_vanished_after_claim",
        );
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
    // U9-IPC-RESIDUAL3 §2 — a PRISTINE reply outcome is SETTLED FROM WHAT IT LEFT BEHIND.
    //
    // U9-IPC-RESIDUAL2 §2 mapped the five pristine variants straight to canonical errors, on the
    // reasoning that the acknowledgement was spent and the authority with it. Neither follows —
    // see `nr7_settle_declined_transaction`, which asks the three owners instead of assuming.
    if matches!(
        disposition,
        crate::kernel::direct_disposition::DirectDisposition::DeclinedBeforeMutation
    ) {
        return nr7_settle_declined_transaction(
            shared,
            cpu,
            frame,
            tid,
            rec_idx,
            rec_gen,
            replier,
            reply_eidx,
            reply_egen,
            facts.transfer_cap_present,
            &payload[..len],
            &outcome,
        );
    }
    crate::kernel::direct_ipc_counters::note_disposition(&REPLY_COUNTERS, disposition);
    // Same shared encoder as the NR6 twin: legacy `handle_ipc_reply` ends with the identical
    // `set_ok(0, 0, 0)` + `encode_transfer_cap_ret(frame, None)` pair, so NR7's success lanes
    // are the same three values. NR7 delivers the reply and wakes the caller; the replier
    // itself returns Ok.
    //
    // U9-IPC-RESIDUAL2 §2: the decline arm above is taken first, so the encoder is reached only
    // with `Completed` or `Failed`, both of which it answers `Some(())`.
    match crate::kernel::direct_disposition::apply_direct_disposition(frame, disposition) {
        Some(()) => Ok(()),
        None => {
            // Unreachable: the decline arm above returned already, and the encoder answers
            // `Some` for `Completed` and `Failed` alike. Named rather than unwrapped, so the
            // impossible case can never become a silent fall-through.
            debug_assert!(false, "the encoder was reached with a decline");
            Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::Internal,
            ))
        }
    }
}

/// U9-IPC-RESIDUAL2 §2 — resolve a NR 7 PREFLIGHT refusal in the broad handler's own order.
///
/// `handle_ipc_reply` validates in this sequence, and a reply that is wrong in more than one way
/// must be told about the first thing that is wrong:
///
/// ```text
/// current_tid()?                      // Internal
/// transfer_cap_arg(frame)?            // the transferred capability decodes
/// validate_transfer_cap(c)?           // ... and resolves     -> InvalidCapability
/// len > Message::MAX_PAYLOAD          // InvalidArgs
/// (payload copy)
/// kernel.ipc_reply(..)                // resolves the one-shot -> StaleCapability -> WrongObject
/// ```
///
/// The reply-record verdicts therefore come LAST, after the transfer capability and the length,
/// which is the opposite of the classifier's order. The existing DIRECT3-CAP-FINAL §7 refusal
/// (a record that is no longer externally invokable) is unchanged and is applied by the caller
/// before this function is reached; everything else that used to decline is resolved here.
#[cfg(not(feature = "hosted-dev"))]
fn nr7_refuse_preflight(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
    verdict: crate::kernel::direct_eligibility::DirectReplyEligibility,
    tid: Option<u64>,
    reply_cap: crate::kernel::capabilities::CapId,
) -> Result<(), TrapHandleError> {
    use crate::kernel::direct_eligibility::DirectReplyEligibility as V;
    use crate::kernel::syscall::SyscallError;

    let requester = tid.unwrap_or(0);
    let answer = |frame: &mut TrapFrame,
                  err: SyscallError,
                  reason: &str|
     -> Result<(), TrapHandleError> {
        crate::kernel::direct_ipc_counters::REPLY.note_failed(err);
        crate::yarm_log!(
            "IPCREPLY_DIRECT_REFUSED_PRE_LOCK record_index={} record_generation={} replier_tid={} reason={} err={:?} reply_copies=0 caller_wakes=0 mutations=0 result=ok",
            usize::MAX,
            u64::MAX,
            requester,
            reason,
            err
        );
        frame.set_err(err.code());
        Ok(())
    };

    // (1) `current_tid(kernel)?` — `Internal`, which is what `current_tid` maps `None` to.
    if matches!(verdict, V::RequesterUnavailable) {
        return answer(frame, SyscallError::Internal, "no_requester");
    }
    // (2) The transferred capability, before anything about the record. `transfer_cap_arg`
    // decodes it and `validate_transfer_cap` resolves it in the replier's cspace; a capability
    // that does not resolve is `InvalidCapability`.
    if crate::kernel::syscall::ipc_abi::transfer_cap_arg_present(frame) {
        match crate::kernel::syscall::ipc_abi::transfer_cap_arg_value(frame) {
            None => return answer(frame, SyscallError::InvalidCapability, "transfer_cap_arg"),
            Some(tc) => {
                if shared
                    .resolve_capability_for_task_split(requester, tc)
                    .is_err()
                {
                    return answer(
                        frame,
                        SyscallError::InvalidCapability,
                        "transfer_cap_unresolved",
                    );
                }
            }
        }
    }
    // (3) The payload length. `IPC_DIRECT_PAYLOAD_MAX` is defined AS `Message::MAX_PAYLOAD`, so
    // this is the broad refusal rather than a narrower limit wearing its error.
    if matches!(verdict, V::PayloadTooLong) {
        return answer(frame, SyscallError::InvalidArgs, "payload_too_long");
    }
    match verdict {
        // (4) The one-shot itself, last. Its resolver's `StaleCapability` and `WrongObject` both
        // reach userspace as `WrongObject`, and a terminal this reply cannot claim means the
        // authority has been settled by another claimant — the same answer a duplicate gets.
        V::ReplyCapUnresolved(err) => answer(frame, SyscallError::from(err), "reply_cap"),
        V::ReplyEndpointGone => answer(frame, SyscallError::WrongObject, "reply_endpoint_gone"),
        V::TerminalUnavailable(class) => {
            crate::yarm_log!(
                "IPCREPLY_DIRECT_TERMINAL_UNAVAILABLE replier_tid={} class={:?} reply_copies=0 caller_wakes=0 mutations=0",
                requester,
                class
            );
            answer(frame, SyscallError::WrongObject, "terminal_unavailable")
        }
        // `TransferCapUnsupported` is produced by no production path since U9-IPC-RESIDUAL1 §2 —
        // both lanes carry a capability — and step (2) above already answered every shape of bad
        // transfer capability. Kept exhaustive so a revival cannot inherit an arm by wildcard.
        V::TransferCapUnsupported => answer(
            frame,
            SyscallError::InvalidCapability,
            "transfer_cap_unsupported",
        ),
        // Statically unreachable on every supported port: the first term of the admission
        // predicate is a `const fn` returning true on x86_64, AArch64 and RISC-V. Refused with a
        // typed invariant error on NR 1's precedent, never handed to the broad dispatcher.
        V::EndpointNotAdmitted => {
            crate::yarm_log!(
                "IPCREPLY_SPLIT_INVARIANT cpu={} tid={} reason=endpoint_not_admitted result=failed_closed",
                cpu.0,
                requester
            );
            answer(frame, SyscallError::Internal, "endpoint_not_admitted")
        }
        // Handled above; listed so a new verdict cannot inherit an arm by wildcard.
        V::RequesterUnavailable | V::PayloadTooLong => {
            answer(frame, SyscallError::Internal, "unreachable_verdict")
        }
        V::Eligible { .. } => {
            debug_assert!(
                false,
                "an eligible verdict never reaches the refusal resolver"
            );
            let _ = reply_cap;
            answer(frame, SyscallError::Internal, "eligible_in_refusal")
        }
    }
}

/// U9-IPC-RESIDUAL3 §2 — **settle a declined NR 7 transaction from what it ACTUALLY left.**
///
/// U9-IPC-RESIDUAL2 §2 mapped the five pristine variants straight to canonical errors, reasoning
/// that "the acknowledgement is spent, so the queued mode's precondition no longer holds, and
/// re-running the reply under the broad lock would resolve the same one-shot and refuse
/// identically". Both halves are wrong for some of these variants:
///
/// * the acknowledgement is **not** necessarily spent. `settle_reply_pre_reserve` RESTORES the
///   lease whenever the exact caller is still blocked, and `drain_direct_reply_post_work`
///   republishes that restoration (`if lease.is_available() { ipcreply_direct_ack::restore(..) }`).
///   `WouldBlock` does not touch the lease at all — it returns before the claim is even checked.
/// * the reply authority is **not** necessarily consumed. None of the five reaches the record
///   reservation, so the record stays `Available` and externally invokable, and the terminal
///   claim this route took was RELEASED rather than committed by the block above.
///
/// Owning an acknowledgement is not proof that reply authority was consumed. So this asks, and
/// the three questions each have an existing owner:
///
/// | question | owner |
/// |---|---|
/// | is the reply authority still usable? | `reply_record_externally_invokable_split_read` |
/// | who owns the terminal now? | `classify_direct_reply_terminal_split_read` |
/// | is the caller blocked on its reply endpoint? | `ipcreply_direct_ack::is_claimable` |
///
/// and the disposition follows from the answers:
///
/// * **authority gone** — spent, or its record recycled. Canonical `WrongObject`, the answer a
///   duplicate reply gets.
/// * **authority live, terminal not admitting** — a competing claimant owns or has settled it
///   and will complete it. Same canonical error, and deliberately NOT revived: §2 forbids
///   reviving authority another terminal claimant has settled, and "live record" is exactly the
///   state in which that mistake would be easy to make.
/// * **authority live, terminal admits, caller NOT blocked** — the QUEUED mode's precondition
///   exactly. The reply continues through that lane's own owner. This is the case the previous
///   package turned into an error: a deliverable reply, refused.
/// * **authority live, terminal admits, caller blocked again** — the blocked mode applies, and
///   the transaction that would service it is the one that just declined. Re-entering it would
///   loop, so the replier gets `WouldBlock` — the canonical "not now, retry" this route already
///   gives a caller that has not blocked yet, and which the oracle server's bounded retry
///   already handles.
///
/// A resolution failure keeps its own typed error regardless of how healthy the record looks: it
/// is a statement about the capability the replier named, not about the record's state.
///
/// No arm is a fall-through; every one answers the trap.
#[cfg(not(feature = "hosted-dev"))]
#[allow(clippy::too_many_arguments)]
fn nr7_settle_declined_transaction(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
    tid: u64,
    rec_idx: usize,
    rec_gen: u64,
    replier: crate::kernel::boot::ReceiverWaiterIdentity,
    reply_eidx: usize,
    reply_egen: u64,
    transfer_cap_present: bool,
    payload: &[u8],
    outcome: &Result<
        crate::kernel::ipccall_direct_txn::IpcReplyDirectSuccess,
        crate::kernel::ipccall_direct_txn::IpcReplyDirectError,
    >,
) -> Result<(), TrapHandleError> {
    use crate::kernel::direct_eligibility::DirectReplyTerminal;
    use crate::kernel::ipccall_direct_txn::IpcReplyDirectError as E;
    use crate::kernel::syscall::SyscallError;

    // (1) Is the reply authority still usable? The same predicate DIRECT3-CAP-FINAL §7 uses for
    // its pre-lock refusal, so "settled" means here exactly what it means there.
    let authority_live = shared.reply_record_externally_invokable_split_read(rec_idx, rec_gen);
    // (2) Who owns the terminal now? This route's claim was released above, so an admitting
    // answer means nobody else has taken it since.
    let terminal = shared.classify_direct_reply_terminal_split_read(
        rec_idx, rec_gen, replier, reply_eidx, reply_egen,
    );
    // (3) Is the caller blocked on its reply endpoint — did the settle restore the lease?
    let caller_blocked =
        crate::kernel::boot::ipcreply_direct_ack::is_claimable(reply_eidx, reply_egen);

    crate::yarm_log!(
        "IPCREPLY_DIRECT_DECLINE_SETTLED record_index={} record_generation={} replier_tid={} authority_live={} terminal={:?} caller_blocked={} result=ok",
        rec_idx,
        rec_gen,
        tid,
        u8::from(authority_live),
        terminal,
        u8::from(caller_blocked)
    );

    if !authority_live {
        return nr7_refuse(
            frame,
            tid,
            rec_idx,
            rec_gen,
            SyscallError::WrongObject,
            "authority_settled",
        );
    }
    if !terminal.admits_direct_reply() {
        return nr7_refuse(
            frame,
            tid,
            rec_idx,
            rec_gen,
            SyscallError::WrongObject,
            "terminal_owned_by_competitor",
        );
    }
    if let Err(E::ReplyCapResolve(kernel_error)) = outcome {
        return nr7_refuse(
            frame,
            tid,
            rec_idx,
            rec_gen,
            SyscallError::from(*kernel_error),
            "reply_cap_unresolved",
        );
    }
    if caller_blocked {
        return nr7_refuse(
            frame,
            tid,
            rec_idx,
            rec_gen,
            SyscallError::WouldBlock,
            "caller_blocked_retry",
        );
    }
    debug_assert!(
        matches!(
            terminal,
            DirectReplyTerminal::Unarmed | DirectReplyTerminal::AvailableExact
        ),
        "an admitting terminal is one of the two admitting classifications"
    );
    crate::yarm_log!(
        "IPCREPLY_DIRECT_DECLINE_TO_QUEUED record_index={} record_generation={} replier_tid={} endpoint={} result=ok",
        rec_idx,
        rec_gen,
        tid,
        reply_eidx
    );
    nr7_queued_reply_lane(
        shared,
        cpu,
        frame,
        tid,
        rec_idx,
        rec_gen,
        replier,
        reply_eidx,
        reply_egen,
        transfer_cap_present,
        payload,
    )
}

/// U9-IPC-RESIDUAL3 §2 — **NR 7's QUEUED lane**, extracted so it can be REACHED, not copied.
///
/// This body was inline in the mode selection. §2 needs a second caller: a transaction that
/// declined without consuming the reply authority can leave that authority valid and the caller
/// un-blocked, which is precisely this lane's precondition. Reaching it from there is
/// "continue through the existing pre-lock owner"; re-deriving the enqueue at the decline site
/// would be a second implementation of the reply lifecycle.
///
/// Nothing about the lane changed in the extraction. Its publication is still
/// `commit_queued_reply_split`'s single rank-3 acquisition, which enqueues and spends the
/// one-shot together and re-validates the record, its binding and the endpoint incarnation
/// against live state; its only pre-publication resource is the transfer envelope, returned on
/// every refusal.
#[cfg(not(feature = "hosted-dev"))]
#[allow(clippy::too_many_arguments)]
fn nr7_queued_reply_lane(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
    tid: u64,
    rec_idx: usize,
    rec_gen: u64,
    replier: crate::kernel::boot::ReceiverWaiterIdentity,
    reply_eidx: usize,
    reply_egen: u64,
    transfer_cap_present: bool,
    payload: &[u8],
) -> Result<(), TrapHandleError> {
    use crate::kernel::direct_ipc_counters::REPLY as REPLY_COUNTERS;
    let len = payload.len();
    // U9-IPC-RESIDUAL1 §2/§3 — the queued mode now carries a capability when the reply has
    // one, through the SAME envelope owner the blocked cap lane uses and the SAME framing
    // helper the legacy reply handler uses.
    //
    // Ownership: the envelope is the only resource this lane acquires before its
    // publication (`commit_queued_reply_split`'s single rank-3 acquisition, which enqueues
    // and spends the one-shot together). Every refusal below returns it, and the stash only
    // RESOLVES the replier's source capability — so a compensated reply is genuinely
    // re-sendable and the replier still holds the cap it named.
    let stashed_cap = if transfer_cap_present {
        let Some(transfer_cap) = crate::kernel::syscall::ipc_abi::transfer_cap_arg_value(frame)
        else {
            return nr7_refuse(
                frame,
                tid,
                rec_idx,
                rec_gen,
                crate::kernel::syscall::SyscallError::InvalidCapability,
                "transfer_cap_arg",
            );
        };
        // The caller this reply settles, read from the record itself — never re-derived.
        let Some(caller) = shared.reply_record_caller_split_read(rec_idx, rec_gen) else {
            return nr7_refuse(
                frame,
                tid,
                rec_idx,
                rec_gen,
                crate::kernel::syscall::SyscallError::WrongObject,
                "record_caller_missing",
            );
        };
        let reply_endpoint_object = crate::kernel::capabilities::CapObject::Endpoint {
            index: reply_eidx,
            generation: reply_egen,
        };
        let Ok(stashed) = shared.stash_transfer_envelope_split(
            crate::kernel::ipc::ThreadId(tid),
            transfer_cap,
            reply_endpoint_object,
            Some(caller.tid),
            None,
        ) else {
            return nr7_refuse(
                frame,
                tid,
                rec_idx,
                rec_gen,
                crate::kernel::syscall::SyscallError::InvalidCapability,
                "envelope_stash_failed",
            );
        };
        Some((stashed.handle, caller.tid))
    } else {
        None
    };
    let unwind_envelope = |shared: &SharedKernel| {
        if let Some((handle, caller_tid)) = stashed_cap {
            let _ = shared.take_transfer_envelope_facts_split(handle, reply_eidx, caller_tid);
        }
    };
    // Plain framing when there is no capability, exactly as the broad path builds it with no
    // transfer handle; the shared cap-bearing framing when there is one.
    let framed = match stashed_cap {
        Some((handle, _)) => {
            crate::kernel::syscall::ipc_abi::frame_reply_message_with_cap(tid, payload, handle).ok()
        }
        None => crate::kernel::ipc::Message::new(tid, payload).ok(),
    };
    let Some(msg) = framed else {
        unwind_envelope(shared);
        return nr7_refuse(
            frame,
            tid,
            rec_idx,
            rec_gen,
            crate::kernel::syscall::SyscallError::InvalidArgs,
            "message_framing_failed",
        );
    };
    let authority = shared.reply_authority_slots_split_read(rec_idx, rec_gen);
    return match shared
        .commit_queued_reply_split(rec_idx, rec_gen, replier, reply_eidx, reply_egen, msg)
    {
        Ok(woken) => {
            // The one-shot is spent, so its authority slots are reclaimed through the same
            // owner the blocked mode uses. A transferred capability, when there is one, is
            // owed nothing further here: its envelope now belongs to the queued message and
            // is consumed by the receive-side materialization on the caller's next receive.
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
            // U9-IPC-RESIDUAL1 §1 — COUNT THE TERMINAL. This lane applied its disposition
            // without recording it, so every successful queued reply was an attempt with no
            // bucket: `terminals_balance()` read false and
            // `IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL` reported `nr7_ok=0 result=fail` on every
            // ordinary boot, on all three architectures. The missing count is also exactly
            // what makes "attempts − completed" look like terminal-broad traffic when it is
            // not — the defect this package was asked to measure around.
            crate::kernel::direct_ipc_counters::note_disposition(
                &REPLY_COUNTERS,
                crate::kernel::direct_disposition::DirectDisposition::Completed,
            );
            match crate::kernel::direct_disposition::apply_direct_disposition(
                frame,
                crate::kernel::direct_disposition::DirectDisposition::Completed,
            ) {
                Some(()) => Ok(()),
                None => {
                    debug_assert!(false, "a completed disposition always encodes");
                    Err(TrapHandleError::Syscall(
                        crate::kernel::syscall::SyscallError::Internal,
                    ))
                }
            }
        }
        Err(err) => {
            // Nothing was mutated: the record is still `Available` and the reply is exactly
            // re-sendable. The envelope goes back with it, so the replier's source capability
            // is untouched.
            //
            // U9-IPC-RESIDUAL2 §2: the answer is given HERE with the commit's own typed
            // error. Handing the trap to the broad path instead was not "offering a retry" —
            // the broad `ipc_reply` consumes the record before it ever reaches the queue, so
            // it could not reproduce this state; it would simply take the same reply through
            // a different implementation, which is the fall-through this package removes.
            unwind_envelope(shared);
            nr7_refuse(
                frame,
                tid,
                rec_idx,
                rec_gen,
                crate::kernel::syscall::SyscallError::from(err),
                "queued_commit_refused",
            )
        }
    };
}

/// U9-IPC-RESIDUAL2 §2 — THE NR 7 refusal, for every point past the preflight.
///
/// One helper rather than nineteen copies of "record the terminal, log it, frame the error". The
/// marker carries the record identity and the reason so a refusal is attributable to the exact
/// step that produced it, and `note_failed` puts it in the one terminal bucket the counters'
/// balance invariant requires — where `note_declined_pre_transaction` used to put it while the
/// trap went on to be serviced a second time by the broad dispatcher.
#[cfg(not(feature = "hosted-dev"))]
fn nr7_refuse(
    frame: &mut TrapFrame,
    tid: u64,
    rec_idx: usize,
    rec_gen: u64,
    err: crate::kernel::syscall::SyscallError,
    reason: &str,
) -> Result<(), TrapHandleError> {
    crate::kernel::direct_ipc_counters::REPLY.note_failed(err);
    crate::yarm_log!(
        "IPCREPLY_SPLIT_REFUSED reason={} replier_tid={} record_index={} record_generation={} err={:?} reply_copies=0 caller_wakes=0 result=ok",
        reason,
        tid,
        rec_idx,
        rec_gen,
        err
    );
    frame.set_err(err.code());
    Ok(())
}

// Hosted: NR 7 has no split route at all. The off-lock user-read seam uses the direct map,
// which exists only on real targets, so the dispatcher arm above is `cfg`-ed out rather than
// stubbed — a stub would have to answer SOMETHING, and every answer is wrong for a profile that
// never routes. The drain and the transaction are exercised directly by the stage199a2b3 hosted
// tests, which call them without going through the dispatcher.

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
) -> crate::kernel::syscall::RecvImmediateOutcome {
    // Number-only default-deny gate: only IpcRecv is considered here.
    if !matches!(Syscall::decode(frame.syscall_num()), Ok(Syscall::IpcRecv)) {
        // U9-RECV-BLOCK2b §1 — the defensive re-decode. The family entry matched the same field
        // before this lane was called, so no take is attempted and nothing about the queue is
        // known.
        return crate::kernel::syscall::RecvImmediateOutcome::NoTakeAttempted("not_nr2");
    }
    shared.try_split_ipc_recv_queued_plain_into_frame(cpu, frame)
}

/// U9-VM-ENTRY1 — the pre-lock NR 3 / NR 13 / NR 14 routes.
///
/// # These three are TOTAL
///
/// Every other route in this file may answer `None` before its first mutation and let the broad
/// handler service the case. These three may not, and the reason is what "closed" means: a family
/// that still has a reachable broad fallback is not closed, however rare the fallback is. So each
/// route below produces EVERY outcome its NR admits — success, every validation refusal, every
/// resource failure — from the split transaction itself, and returns `Some(..)` unconditionally
/// once the NR gate has matched.
///
/// That is also what makes compensation honest. After a frame is taken, a capability minted or a
/// page installed, answering `None` would hand a partially executed transaction to a dispatcher
/// that knows nothing about it. The transaction settles its own phases through the narrow owners
/// in `vm_txn`, and there is no path on which it does not.
///
/// The NR gate itself is the one place a `None` remains, and it is not a fallback: it is the
/// defensive re-decode every route in this file performs, and it can only fail for a frame whose
/// syscall number is not the one the dispatcher already matched.
fn split_vm_caller_tid(shared: &SharedKernel, cpu: CpuId) -> Result<u64, TrapHandleError> {
    shared.current_tid_authoritative(cpu).ok_or({
        TrapHandleError::Syscall(crate::kernel::syscall::SyscallError::from(
            crate::kernel::boot::KernelError::TaskMissing,
        ))
    })
}

/// NR 3 — `VmMap`. The target address space is the one the caller's CAPABILITY names; the
/// provisional frame capabilities are still minted in the caller's own cnode.
pub(crate) fn try_split_vm_map_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::syscall::vm_txn::{MapTarget, run_vm_map_transaction};
    let syscall = Syscall::decode(frame.syscall_num()).ok()?;
    if !matches!(syscall, Syscall::VmMap) {
        return None;
    }
    let tid = match split_vm_caller_tid(shared, cpu) {
        Ok(tid) => tid,
        Err(e) => return Some(Err(e)),
    };
    let cap = crate::kernel::capabilities::CapId(
        frame.arg(crate::kernel::syscall::SYSCALL_ARG_CAP) as u64,
    );
    let addr = frame.arg(crate::kernel::syscall::SYSCALL_ARG_PTR);
    let len = frame.arg(crate::kernel::syscall::SYSCALL_ARG_LEN);
    let prot = frame.arg(crate::kernel::syscall::SYSCALL_ARG_INLINE_PAYLOAD0);
    let mut owners = crate::kernel::syscall::vm_split::SplitVmOwners { shared, tid, cpu };
    Some(
        match run_vm_map_transaction(&mut owners, MapTarget::Capability(cap), addr, len, prot) {
            Ok((base, map_len)) => {
                frame.set_ok(base, map_len, 0);
                Ok(())
            }
            Err(e) => Err(TrapHandleError::Syscall(e)),
        },
    )
}

/// NR 13 — `VmAnonMap`. Same transaction, caller's own address space.
pub(crate) fn try_split_vm_anon_map_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::syscall::vm_txn::{MapTarget, run_vm_map_transaction};
    let syscall = Syscall::decode(frame.syscall_num()).ok()?;
    if !matches!(syscall, Syscall::VmAnonMap) {
        return None;
    }
    let tid = match split_vm_caller_tid(shared, cpu) {
        Ok(tid) => tid,
        Err(e) => return Some(Err(e)),
    };
    let addr = frame.arg(crate::kernel::syscall::SYSCALL_ARG_PTR);
    let len = frame.arg(crate::kernel::syscall::SYSCALL_ARG_LEN);
    let prot = frame.arg(crate::kernel::syscall::SYSCALL_ARG_INLINE_PAYLOAD0);
    let mut owners = crate::kernel::syscall::vm_split::SplitVmOwners { shared, tid, cpu };
    Some(
        match run_vm_map_transaction(&mut owners, MapTarget::CallerAddressSpace, addr, len, prot) {
            Ok((base, map_len)) => {
                frame.set_ok(base, map_len, 0);
                Ok(())
            }
            Err(e) => Err(TrapHandleError::Syscall(e)),
        },
    )
}

/// NR 14 — `VmBrk`, COMPLETE.
///
/// Stage 114's `M2_SEAM_LIVE_D3_BRK_SHRINK` serviced one shape — a page-crossing shrink at at
/// most one CPU online — and declined the query, growth, the no-op, a within-page shrink, a
/// non-leader caller, every validation failure and every multi-CPU boot. All of those reached the
/// broad handler, and on RISC-V so did the one shape it did service, because NR 14 was never on
/// that architecture's whitelist.
///
/// This route services all five shapes at any CPU count. The topology restriction is not ignored:
/// it was a consequence of the only unmap cascade the old route could reach needing the ipc(3)
/// domain for its shootdown, and `unmap_range_two_phase_split` — rank 5, then the coordinator
/// with NO lock held, then rank 6 — is the owner that answered it.
pub(crate) fn try_split_vm_brk_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::syscall::vm_txn::run_vm_brk_transaction;
    let syscall = Syscall::decode(frame.syscall_num()).ok()?;
    if !matches!(syscall, Syscall::VmBrk) {
        return None;
    }
    let tid = match split_vm_caller_tid(shared, cpu) {
        Ok(tid) => tid,
        Err(e) => return Some(Err(e)),
    };
    let requested = frame.arg(crate::kernel::syscall::SYSCALL_ARG_CAP);
    let mut owners = crate::kernel::syscall::vm_split::SplitVmOwners { shared, tid, cpu };
    Some(match run_vm_brk_transaction(&mut owners, requested) {
        Ok(result) => {
            frame.set_ok(result, 0, 0);
            Ok(())
        }
        Err(e) => Err(TrapHandleError::Syscall(e)),
    })
}

/// U9-XFER2 §3 — the pre-lock NR 30 (`RecvSharedV3`) route.
///
/// TOTAL after the NR gate. Every refusal the ABI can produce — a short record, a bad version, a
/// nonzero timeout, an undersized metadata buffer, a missing right, a dead endpoint, an empty
/// queue, a refused mapping plan, a lost commit race, a copy fault — is produced by the shared
/// transaction, so there is nothing left for a broad fallback to service.
///
/// This route exists because NR 30's user copies have owners off the broad lock:
/// `copy_from_user_split` and `copy_to_user_split`, the rank-5/6 mirrors of
/// `KernelState::copy_from_user` / `copy_to_user`.
fn try_split_recv_shared_v3_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::recv_core::recv_shared_v3::{V3_MIN_REQUEST_LEN, validate_v3_request};
    use crate::kernel::syscall::SyscallError;
    use crate::kernel::syscall::recv_v3_txn::{V3Delivery, run_recv_v3_transaction};

    let syscall = Syscall::decode(frame.syscall_num()).ok()?;
    if !matches!(syscall, Syscall::RecvSharedV3) {
        return None;
    }
    let tid = match split_vm_caller_tid(shared, cpu) {
        Ok(tid) => tid,
        Err(e) => return Some(Err(e)),
    };
    let req_ptr = frame.arg(0);
    let req_len = frame.arg(1);
    if req_len < V3_MIN_REQUEST_LEN as usize {
        return Some(Err(TrapHandleError::Syscall(SyscallError::InvalidArgs)));
    }
    let Some(asid) = shared.task_asid_option_split_read(tid) else {
        return Some(Err(TrapHandleError::Syscall(SyscallError::InvalidArgs)));
    };
    let read_len = req_len.min(80);
    let Ok(wide) =
        shared.copy_from_user_split(asid, crate::kernel::vm::VirtAddr(req_ptr as u64), read_len)
    else {
        return Some(Err(TrapHandleError::Syscall(SyscallError::PageFault)));
    };
    let mut req_bytes = [0u8; 80];
    req_bytes[..read_len].copy_from_slice(&wide[..read_len]);
    let req = crate::kernel::syscall::recv_shared_v3::parse_v3_request_bytes(&req_bytes);
    if validate_v3_request(&req).is_err() {
        return Some(Err(TrapHandleError::Syscall(SyscallError::InvalidArgs)));
    }
    // Blocking is unimplemented on BOTH routes and stays that way — adding a blocking mode is
    // out of scope, and answering `WouldBlock` here is what the broad handler already answers.
    if req.timeout_ticks != 0 {
        return Some(Err(TrapHandleError::Syscall(SyscallError::WouldBlock)));
    }
    if req.map_intent != 0
        && req.metadata_len < crate::kernel::recv_core::recv_shared_v3::V3_LIVE_OUTPUT_LEN as u64
    {
        return Some(Err(TrapHandleError::Syscall(SyscallError::InvalidArgs)));
    }

    let mut owners = crate::kernel::syscall::recv_v3_split::SplitRecvV3Owners { shared, tid, cpu };
    Some(match run_recv_v3_transaction(&mut owners, &req) {
        Ok(V3Delivery::Mapped {
            sender_tid,
            xfer_cap,
        }) => {
            frame.set_ok(
                usize::try_from(sender_tid).unwrap_or(0),
                0,
                usize::try_from(xfer_cap).unwrap_or(usize::MAX),
            );
            Ok(())
        }
        Ok(V3Delivery::Plain {
            sender_tid,
            payload_len,
            xfer_cap,
        }) => {
            frame.set_ok(
                usize::try_from(sender_tid).unwrap_or(0),
                payload_len,
                usize::try_from(xfer_cap).unwrap_or(usize::MAX),
            );
            Ok(())
        }
        Ok(V3Delivery::PayloadFault { user_ptr }) => {
            // §58 semantics, unchanged: the message IS consumed, a user fault is recorded, and
            // the syscall returns Ok without a result lane.
            // U9-PAGEFAULT1 §2: the receiver's own buffer pointer, faulted on its behalf.
            shared.record_fault_split_mut(crate::kernel::trap::FaultInfo::user(
                crate::kernel::vm::VirtAddr(user_ptr as u64),
                crate::kernel::trap::FaultAccess::Write,
            ));
            frame.set_err(SyscallError::PageFault.code());
            Ok(())
        }
        Err(e) => Err(TrapHandleError::Syscall(e)),
    })
}

/// U9-XFER1 §3 — the pre-lock NR 4 (`TransferRelease`) route.
///
/// TOTAL after the NR gate, for the same reason NR 3 / NR 13 / NR 14 are: after the range is
/// unmapped and the capability revoked, answering `None` would hand a partially executed
/// transaction to a dispatcher that knows nothing about it — and here that dispatcher would then
/// run the whole release a SECOND time against an already-released range.
///
/// It never needs to. `xfer_txn` raises every refusal in phase V, from reads only, so a refused
/// request has changed nothing and is returned as the exact error the broad handler produced for
/// the same input. There is no branch between the first mutation and the return.
fn try_split_transfer_release_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::syscall::xfer_txn::run_transfer_release_transaction;
    let syscall = Syscall::decode(frame.syscall_num()).ok()?;
    if !matches!(syscall, Syscall::TransferRelease) {
        return None;
    }
    let tid = match split_vm_caller_tid(shared, cpu) {
        Ok(tid) => tid,
        Err(e) => return Some(Err(e)),
    };
    let transfer_cap = crate::kernel::capabilities::CapId(
        frame.arg(crate::kernel::syscall::SYSCALL_ARG_CAP) as u64,
    );
    let base_arg = frame.arg(crate::kernel::syscall::SYSCALL_ARG_PTR);
    let len_arg = frame.arg(crate::kernel::syscall::SYSCALL_ARG_LEN);
    let mut owners = crate::kernel::syscall::xfer_split::SplitXferOwners { shared, tid, cpu };
    Some(
        match run_transfer_release_transaction(&mut owners, transfer_cap, base_arg, len_arg) {
            Ok(map_len) => {
                frame.set_ok(map_len, 0, 0);
                Ok(())
            }
            Err(e) => Err(TrapHandleError::Syscall(e)),
        },
    )
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
fn try_split_exit_current_task(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &TrapFrame,
) -> SplitDispatchDisposition {
    use crate::kernel::boot::exit_claim::ExitDisposition;
    use crate::kernel::syscall::exit_txn::{SharedExitOwners, run_exit_transaction};
    type D = SplitDispatchDisposition;

    // The FIRST gate, and the one every other switching class states the same way: this route
    // services NR 16 and nothing else. Without it the transaction runs on every trap that reaches
    // this seam and exits the caller of whatever syscall it actually made — which is exactly what
    // the first live x86_64 run showed, three tasks retiring on their first `DebugLog`.
    if frame.syscall_num() != crate::kernel::syscall::SYSCALL_EXIT_CURRENT_TASK_NR {
        return D::NotHandled;
    }
    // U9-EXIT1 §6 — the split half of the terminal-edge measurement, emitted at the route's OWN
    // entry for the same reason `EXIT_TASK_BROAD_ENTER` is: an edge that were counted only on
    // success would report a route that declines every trap as a route that was never reached.
    // Paired with `EXIT_TASK_SPLIT_DECLINED`, this is what makes the retirement claim falsifiable.
    let entering = shared.current_tid_authoritative(cpu).unwrap_or(0);
    crate::yarm_log!(
        "EXIT_TASK_SPLIT_ENTER tid={} asid={} result=ok",
        entering,
        shared
            .task_asid_opt_split_read(entering)
            .unwrap_or(crate::kernel::vm::Asid(0))
            .0
    );
    let mut owners = SharedExitOwners { shared };
    match run_exit_transaction(
        &mut owners,
        cpu,
        crate::kernel::syscall::EXIT_STATUS_SELF_REQUESTED,
    ) {
        Ok(outcome) => {
            let claim = &outcome.claim;
            // The same markers the broad handler emits, so an observer sees one vocabulary for one
            // syscall regardless of route.
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
        // U9-EXIT2 §4 — THE total settlement. Three arms, none of them `NotHandled`: once NR 16
        // is recognized this route owns the answer, and the terminal broad dispatcher is not a
        // destination any production outcome can reach.
        Err(failure) => match failure.disposition {
            // Class C. The broad handler's own answer, reproduced exactly: a task that still owes
            // a reply and finds no free deferred slot gets `WouldBlock` and stays Running, with
            // its reverse link still attached. Same marker text, same meaning, mutation-free.
            ExitDisposition::InvalidPreLock => {
                crate::yarm_log!(
                    "EXIT_TASK_SYSCALL_DECLINED tid={} reason={} link_retained=1 result=would_block",
                    entering,
                    failure.refusal.marker()
                );
                D::Complete(Err(TrapHandleError::Syscall(
                    crate::kernel::syscall::SyscallError::WouldBlock,
                )))
            }
            // Class B. A state the U9-EXIT2 §5 guards prove cannot exist at a userspace NR 16
            // boundary. Handing it to the broad dispatcher would be the one edge this stage
            // removes, taken for a reason that cannot occur — so it is a typed invariant error
            // instead, pre-mutation, exactly as the impossible IpcSend classes are refused.
            ExitDisposition::ImpossibleState => {
                crate::yarm_log!(
                    "EXIT_TASK_SPLIT_IMPOSSIBLE cpu={} reason={} task_mutation=none result=fail",
                    cpu.0,
                    failure.refusal.marker()
                );
                D::Complete(Err(TrapHandleError::Syscall(
                    crate::kernel::syscall::SyscallError::Internal,
                )))
            }
            // Class D, RESTORED. The transaction proved the victim was still Running, still ours
            // and placed on no CPU, and restored it as this CPU's current. The entering frame is
            // therefore its own again, which is the ONLY state in which a `Complete` may return
            // through it: the bridge delivers this error by RESUMING that task.
            //
            // `Internal` rather than `Ok(())` because the exit did not happen and the task must
            // not act as if it had.
            ExitDisposition::PostClearRestored => {
                crate::yarm_log!(
                    "EXIT_TASK_SPLIT_POST_CLEAR_RESTORED cpu={} reason={} result=fail",
                    cpu.0,
                    failure.refusal.marker()
                );
                D::Complete(Err(TrapHandleError::Syscall(
                    crate::kernel::syscall::SyscallError::Internal,
                )))
            }
            // Class D, ADVANCED. Another owner made the victim terminal, removed or replaced, so
            // there is no frame to return through: the entering task must never run again. The
            // already-reserved deferral names the old incarnation and the EXISTING drain selects
            // somebody else — the same answer the success path gives, reached for a different
            // reason.
            //
            // This is the arm U9-EXIT2 got wrong. It answered `Complete(Err(Internal))`, and the
            // bridge delivers that by resuming the entering frame with `current[cpu] == None` —
            // and, when a reap or a fault won the claim, by resuming a task that is already
            // terminal.
            ExitDisposition::PostClearAdvance => {
                crate::yarm_log!(
                    "EXIT_TASK_SPLIT_POST_CLEAR_ADVANCE cpu={} reason={} result=fail",
                    cpu.0,
                    failure.refusal.marker()
                );
                D::QueueAdvanceCommitted
            }
        },
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
        // U9-RECV-FINAL §1: IpcRecv (NR 2) is NO LONGER on this whitelist.
        //
        // Stage 32B admitted it here so the seam could attempt the queued-plain split. The
        // whitelist's contract, though, is that every class on it may be early-returned through
        // the caller's own frame — and that is false of a receive, which parks when the endpoint
        // is empty. NR 2 is now consulted with the other SWITCHING classes, through
        // `try_split_ipc_recv_family_into_frame`, which owns both of the family's lanes.
        //
        // Leaving it here as well would give one syscall two entry points into the dispatcher,
        // and the second would be consulted precisely when the first had already declined.
        // U9-VM-ENTRY1: the three VM entries. Stage 114 admitted NR 14 here so the seam could
        // ATTEMPT a page-crossing shrink, with every other shape declining to the broad handler.
        // That conditional admission is gone: all three routes are TOTAL, so passing this gate is
        // the whole decision and no VM entry has a reachable broad fallback left.
        //
        // NR 3 and NR 13 were never admitted at all, so every call reached the terminal broad
        // dispatcher on every port.
        Syscall::VmMap => Some(syscall),
        Syscall::VmAnonMap => Some(syscall),
        Syscall::VmBrk => Some(syscall),
        // U9-XFER1 §3: TransferRelease (NR 4). Like the three above it, it had NO split route at
        // all — every call on every architecture reached the terminal broad dispatcher. Its route
        // is TOTAL after this gate: `xfer_txn` raises every refusal in a read-only preflight, so
        // nothing can be refused once the range has been unmapped or the capability revoked.
        Syscall::TransferRelease => Some(syscall),
        // U9-XFER2 §3: RecvSharedV3 (NR 30) — the second half of the IPC/transfer residual. Its
        // route is TOTAL after this gate for the same reason NR 4's is.
        Syscall::RecvSharedV3 => Some(syscall),
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

    /// U9-VM-ENTRY1 re-derivation of `stage29_vm_map_not_eligible`.
    ///
    /// Stage 29 pinned NR 3 as global-lock-only so a later stage could not silently whitelist a
    /// MINTING, MAPPING class while its off-lock compensation was undefined. That protection did
    /// its job, and it is retired the way it was meant to be retired: by a mission that derived
    /// the closure first. What the case now holds is the positive fact and the property that
    /// made the admission legitimate.
    ///
    /// The route is TOTAL — that is the whole difference. Stage 29's assertion was
    /// `legacy() == None`, i.e. "the dispatcher hands this to the broad handler". A family with a
    /// reachable broad fallback is not closed, so NR 3 must now answer for EVERY input,
    /// including the invalid one this fixture supplies.
    #[test]
    fn stage29_vm_map_is_eligible_and_total() {
        let (kernel, _r, _t) = shared_with_control_plane_requester();
        assert!(
            classify_split_eligible_nr_only(decode(SYSCALL_VM_MAP_NR)).is_some(),
            "NR 3 passes the NR-only gate since U9-VM-ENTRY1"
        );
        // The SAME frame Stage 29 used: args (1, 2, 3, …) are not a valid (addr, len, prot)
        // triple, so this is the invalid-input case. It must be ANSWERED, not deferred.
        let mut frame = TrapFrame::new(SYSCALL_VM_MAP_NR, [1, 2, 3, 4, 5, 6]);
        let outcome = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame).legacy();
        assert!(
            outcome.is_some(),
            "NR 3 must never fall through to the broad dispatcher — a reachable fallback is \
             exactly what stops the family being closed"
        );
        assert!(
            matches!(
                outcome,
                Some(Err(TrapHandleError::Syscall(
                    crate::kernel::syscall::SyscallError::InvalidArgs
                )))
            ),
            "and the error is the delivered one: addr=2 is not page-aligned"
        );
        // NR 13 is the same transaction with the other authority, and NR 14 is total too.
        for nr in [
            crate::kernel::syscall::SYSCALL_VM_ANON_MAP_NR,
            crate::kernel::syscall::SYSCALL_VM_BRK_NR,
        ] {
            assert!(
                classify_split_eligible_nr_only(decode(nr)).is_some(),
                "NR {nr} passes the NR-only gate"
            );
            let mut frame = TrapFrame::new(nr, [1, 2, 3, 4, 5, 6]);
            assert!(
                try_split_dispatch_into_frame(&kernel, CPU0, &mut frame)
                    .legacy()
                    .is_some(),
                "NR {nr} must answer rather than defer"
            );
        }
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
            // U9-RECV-FINAL §1: NR 2 left this set. It is a SWITCHING class now — it parks
            // when the endpoint is empty — so it is consulted before this gate, exactly as
            // NR 5 always was. Its absence here is the claim, not an omission.
            if nr == SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR
                // U9-VM-ENTRY1: the three VM entries. NR 14 was already here for its one
                // conditional shape; NR 3 and NR 13 join it, and all three are now TOTAL.
                || nr == crate::kernel::syscall::SYSCALL_VM_MAP_NR
                || nr == crate::kernel::syscall::SYSCALL_VM_ANON_MAP_NR
                || nr == crate::kernel::syscall::SYSCALL_VM_BRK_NR
                || nr == crate::kernel::syscall::SYSCALL_DEBUG_LOG_NR
                || nr == crate::kernel::syscall::SYSCALL_FUTEX_WAKE_NR
                || nr == crate::kernel::syscall::SYSCALL_CREATE_INITRAMFS_FILE_SLICE_MO_NR
                || nr == crate::kernel::syscall::SYSCALL_SPAWN_THREAD_NR
                || nr == SYSCALL_SPAWN_PROCESS_NR
                || nr == crate::kernel::syscall::SYSCALL_SPAWN_FROM_MEMORY_OBJECT_NR
                || nr == crate::kernel::syscall::SYSCALL_FORK_NR
                || nr == crate::kernel::syscall::SYSCALL_REAP_FAULTED_TASK_NR
                // U9-XFER1 §3: NR 4 `TransferRelease` — the first of the IPC/transfer residual.
                || nr == crate::kernel::syscall::SYSCALL_TRANSFER_RELEASE_NR
                // U9-XFER2 §3: NR 30 `RecvSharedV3` — the second, and the last of it.
                || nr == crate::kernel::syscall::SYSCALL_RECV_SHARED_V3_NR
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
        // U9-RECV-FINAL §1 INVERTS this guard, on the reasoning its NR 5 sibling below has
        // carried all along.
        //
        // Stage 32B admitted NR 2 to the NR-only gate so the seam could attempt the queued-plain
        // split. That gate's contract is that everything on it is non-switching and may be
        // early-returned through the caller's own frame — and a receive on an empty endpoint
        // parks the caller. NR 2 is now consulted with the switching classes, through the family
        // entry that owns BOTH of its lanes, so it must no longer pass this gate: leaving it
        // here would give one syscall two entry points, the second consulted exactly when the
        // first had declined.
        assert!(
            classify_split_eligible_nr_only(decode(SYSCALL_IPC_RECV_NR)).is_none(),
            "IpcRecv (NR 2) is a switching class and must not be on the NR-only gate"
        );
        // And the arg-level classifier maps it to the IpcRecvKernelTask variant.
        assert_eq!(
            classify_split_eligible(decode(SYSCALL_IPC_RECV_NR), 1, [0; 6]),
            Some(SplitEligibleSyscall::IpcRecvKernelTask)
        );
    }

    #[test]
    fn stage32b_ipc_recv_timeout_nr_not_in_whitelist() {
        // IpcRecvTimeout (NR 5) must NOT be on the NR-ONLY whitelist: that gate's contract is
        // that everything on it is non-switching and may be early-returned through the caller's
        // own frame, and a blocking timed receive may not.
        //
        // U9-YIELD2 §1 — this is NOT the statement "NR 5 has no pre-lock route", which is what
        // the old comment here ("it stays on the global-lock path") said and what U9-RESIDUAL1
        // §2's matrix copied. Stage 199G-B §2 gave NR 5 the SWITCHING route
        // `try_split_blocking_ipc_recv_into_frame`, admitted on all three architectures, which
        // runs BEFORE this gate. What stays broad is the non-blocking half of NR 5 — a receive
        // that would not block, and every typed refusal — which is a residual arm, not an absent
        // route.
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
        // The sender-side IPC syscalls stay default-deny AT THE NR-ONLY GATE.
        //
        // U9-YIELD2 §1 — that is all this asserts, and the old one-line comment ("stay
        // default-deny") read as though these three had no pre-lock route at all. All three do:
        // NR 1 through `try_split_ipc_send_into_frame` (199G-C4, a switching class tried before
        // this gate), NR 6 and NR 7 through the direct request/reply handlers, whose admission
        // predicate `ipccall_direct_admission_enabled()` is `true` on all three architectures.
        // They are absent from THIS whitelist because its contract is non-switching
        // early-returnable classes, not because they are unrouted.
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

/// U9-RESIDUAL1 §3 — the syscall number this trap ACTUALLY carries, per architecture.
///
/// x86_64 and RISC-V decode the number into the frame at entry, so `frame.syscall_num()` is
/// authoritative there. AArch64 does not: `pre_split_import_syscall_abi` imports the decoded ABI
/// only for an allowlisted number, and an unlisted syscall leaves the frame reading `nr = 0`.
///
/// For every other split class that is harmless — their numbers are non-zero, so an unimported
/// frame simply declines. **NR 0 is different**: 0 is Yield's own number, so on AArch64
/// `frame.syscall_num() == 0` is true for a Yield *and* for every unlisted syscall, and a route
/// gated on it alone would fire for `VmMap`, `IpcCall`, `IpcReply` and the rest.
///
/// So this reads the raw `x8` on AArch64 — the same register `pre_split_import_syscall_abi` peeks
/// to make its own decision, and the authoritative source of the trapped number before any import.
#[cfg(not(feature = "hosted-dev"))]
fn trapped_syscall_nr(frame: &TrapFrame) -> usize {
    #[cfg(target_arch = "aarch64")]
    {
        frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X8)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        frame.syscall_num()
    }
}

/// U9-RESIDUAL1 §3 — service `Yield` (NR 0) off the broad lock, on all three architectures.
///
/// # What this adds, and what it does not
///
/// It adds no policy. The decision is `yield_txn::run_yield_transaction`, the same one
/// `KernelState::yield_current` drives through `BroadYieldOwners`; this route drives it through
/// `SharedYieldOwners`, whose methods take one domain lock each with the broad
/// `SpinLock<KernelState>` released. Every gate, every ordering and every marker is that
/// transaction's.
///
/// # Why declining is always safe here
///
/// Every `YieldDecline` is pre-mutation — the one step that can fail after a write rolls that write
/// back through the named inverse — so `NotHandled` hands a byte-for-byte unchanged world to the
/// broad path, which then runs the identical decision through the identical owners and produces the
/// identical outcome. This is a fallback BEFORE consumption, never after: past the commit the
/// caller is queued exactly once, `current` is empty, and the deferral is published.
///
/// # The two things this route owns
///
/// The telemetry increment (the broad path counts on entry; this route counts only what it commits,
/// so a decline is counted exactly once by the broad path that then runs) and the syscall's own
/// result, written into the outgoing frame before the drain switches away from it.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_yield_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> SplitDispatchDisposition {
    use crate::kernel::syscall::yield_txn;
    use SplitDispatchDisposition as D;

    if trapped_syscall_nr(frame) != crate::kernel::syscall::SYSCALL_YIELD_NR {
        return D::NotHandled;
    }
    if (cpu.0 as usize) >= crate::kernel::scheduler::MAX_CPUS {
        return D::NotHandled;
    }
    let mut owners = yield_txn::SharedYieldOwners { shared };
    match yield_txn::run_yield_transaction(&mut owners, cpu) {
        Ok(outcome) => {
            // The publish-side vocabulary, in this architecture's exact delivered strings. The
            // broad path never runs for a committed yield, so this is emitted once per NR 0 just
            // as it always was.
            yield_txn::log_yield_deferred(cpu, outcome.outgoing);
            // Telemetry: the broad path's entry increment is not reached on this route, so the
            // count is owed here. Exactly one per yield, either way.
            shared.count_yield_split_mut();
            crate::yarm_log!(
                "YIELD_SPLIT_COMMITTED cpu={} tid={}",
                cpu.0,
                outcome.outgoing
            );
            // (7) The syscall's own result, into the OUTGOING frame, before the switch — the same
            // `frame.set_ok(0, 0, 0)` `handle_yield` performs, at the same point relative to the
            // deferral.
            frame.set_ok(0, 0, 0);
            D::QueueAdvanceCommitted
        }
        Err(decline) => {
            // A DISTINCT marker from the in-lock fallback's. The broad path will now run the same
            // decision and emit its own `*_INLOCK_DISPATCH_FALLBACK`, so reusing that string here
            // would double-count it; this names the split attempt, which is a different event.
            crate::yarm_log!(
                "YIELD_SPLIT_REFUSED cpu={} reason={}",
                cpu.0,
                yield_txn::legacy_reason(decline)
            );
            D::NotHandled
        }
    }
}

#[cfg(feature = "hosted-dev")]
fn try_split_yield_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> SplitDispatchDisposition {
    SplitDispatchDisposition::NotHandled
}
