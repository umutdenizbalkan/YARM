// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199A2D2B — GENERIC x86_64 AP dispatch-on-wake mechanism.
//!
//! This module carries the architecture-neutral (asm-free) core of the AP's ability to run
//! SCHEDULER-SELECTED userspace tasks — the piece the accepted AP scaffold (Stages 189D–190B)
//! lacked. It is deliberately NOT oracle-specific: the plan is built from a CPU's real run queue
//! and distinguishes a FRESH ring-3 entry (a never-run task) from a BLOCKED-task CONTINUATION
//! RESUME (a task that ran, blocked in a syscall, and was later made Runnable again). The
//! blocked-resume path is exactly what a cross-CPU IPC receiver needs: it is woken on a remote CPU
//! and must continue its recv-v2 syscall — not restart at a fresh entry point.
//!
//! Three concerns live here, each independently testable:
//!   1. the per-CPU RESCHEDULE-PENDING flag the remote-wake IPI sets (bounded interrupt work only);
//!   2. the LOST-WAKEUP-SAFE idle decision (`cli` → inspect → dispatch-or-`sti;hlt`);
//!   3. the OWNED dispatch plan + its FreshUserEntry/BlockedUserResume classification, carrying
//!      only values that survive after the scheduler/task locks are released.
//!
//! The final userspace return (loading CR3, installing per-CPU TSS RSP0 / GS / FS, and either a
//! fresh `iretq` or a full context-restore `iretq`) is the arch asm half; this module produces the
//! OWNED plan that half consumes, so no task/scheduler/capability reference ever escapes a guard.

#![allow(dead_code)]

use crate::kernel::scheduler::CpuId;
use core::sync::atomic::{AtomicBool, Ordering};

/// How the AP returns to userspace for a selected task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApReturnMode {
    /// A never-run task: enter ring 3 at its ELF/thread entry with a fresh user stack and zeroed
    /// GPRs (plus any startup args). This is the accepted fresh-entry path.
    FreshUserEntry,
    /// A task that ran, blocked in a syscall, and is Runnable again: RESTORE its saved continuation
    /// (full user GPRs + the saved post-syscall RIP/RSP) so it continues the blocked syscall exactly
    /// once — never a fresh restart. This is the cross-CPU receiver's resume path.
    BlockedUserResume,
}

/// An OWNED, guard-free plan the AP executes lock-free to return to userspace. Every field is a
/// plain value copied out under the scheduler/task lock; no `&Task`, no scheduler handle, no
/// capability reference is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApUserDispatchPlan {
    pub(crate) mode: ApReturnMode,
    /// Authoritative task identity — the plan is refused if it is ever installed against a
    /// different current-CPU/task ownership than this.
    pub(crate) tid: u64,
    pub(crate) asid: u16,
    /// The CPU this task is HOME to; the AP asserts `home_cpu == self` before installing (a plan
    /// for another CPU's task is never run here).
    pub(crate) home_cpu: u8,
    /// Address space root (CR3) to load. `0` = resolve at install time from `asid` (non-hosted).
    pub(crate) cr3: u64,
    /// FreshUserEntry: the entry RIP. BlockedUserResume: the saved post-syscall RIP.
    pub(crate) entry_rip: u64,
    /// FreshUserEntry: the fresh user stack top. BlockedUserResume: the saved user RSP.
    pub(crate) user_rsp: u64,
    /// BlockedUserResume: the 16 restored user GPRs (rax..r15). FreshUserEntry: all zero.
    pub(crate) user_gprs: [u64; 16],
    /// The ring-0 stack this CPU installs as syscall RSP0 + TSS RSP0 before the return.
    pub(crate) kernel_rsp0: u64,
    /// User FS base (TLS). `0` when the task has no TLS.
    pub(crate) fs_base: u64,
}

/// The AP idle dispatcher's decision after inspecting its run queue under the CPU-1 scheduler lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApDispatchDecision {
    /// Dispatch this scheduler-selected task (fresh or resume).
    Dispatch(ApUserDispatchPlan),
    /// Nothing runnable — return to `sti; hlt`.
    Idle,
}

// ── (1) Per-CPU reschedule-pending flag (set by the remote-wake IPI; coalescing) ──────────────

static AP_RESCHEDULE_PENDING: [AtomicBool; crate::arch::platform_constants::MAX_CPUS] =
    [const { AtomicBool::new(false) }; crate::arch::platform_constants::MAX_CPUS];

#[inline]
fn cpu_idx(cpu: CpuId) -> usize {
    (cpu.0 as usize).min(crate::arch::platform_constants::MAX_CPUS - 1)
}

/// Set this CPU's reschedule-pending flag (idempotent — repeated IPIs COALESCE into one pending
/// request; no lost request, no per-IPI queue growth). Published Release so the idle dispatcher's
/// Acquire load observes the enqueue that motivated the wake.
pub(crate) fn set_reschedule_pending(cpu: CpuId) {
    AP_RESCHEDULE_PENDING[cpu_idx(cpu)].store(true, Ordering::Release);
}

/// Observe (without consuming) whether a reschedule is pending on `cpu`.
pub(crate) fn reschedule_pending(cpu: CpuId) -> bool {
    AP_RESCHEDULE_PENDING[cpu_idx(cpu)].load(Ordering::Acquire)
}

/// Atomically CONSUME the pending flag (returns its prior value). Consumed exactly once by the
/// idle dispatcher before it inspects the run queue; a concurrent set after the consume re-arms it
/// so the next idle iteration re-checks (no lost wake).
pub(crate) fn take_reschedule_pending(cpu: CpuId) -> bool {
    AP_RESCHEDULE_PENDING[cpu_idx(cpu)].swap(false, Ordering::AcqRel)
}

/// Test/boot reset.
pub(crate) fn reset_reschedule_pending(cpu: CpuId) {
    AP_RESCHEDULE_PENDING[cpu_idx(cpu)].store(false, Ordering::Release);
}

// ── (2) Lost-wakeup-safe idle decision ────────────────────────────────────────────────────────

/// The idle-loop step, expressed as a pure decision so it is independently testable. The LIVE loop
/// runs it with interrupts DISABLED (`cli`): if it returns `true` the loop consumes the pending
/// flag, releases the scheduler guard, and dispatches; if `false` the loop does `sti; hlt` as ONE
/// atomic sequence. Because an enqueue path both (a) sets the pending flag and (b) sends the wake
/// IPI, an enqueue landing AFTER this check but BEFORE the `hlt` leaves the IPI pending in the
/// LAPIC; `sti; hlt` then returns immediately (the classic cli/sti;hlt no-lost-wakeup pattern). The
/// decision therefore never needs the IPI itself as the memory-ordering primitive — the pending
/// flag (Release/Acquire) carries the happens-before.
#[inline]
pub(crate) fn ap_idle_should_dispatch(reschedule_pending: bool, has_runnable: bool) -> bool {
    reschedule_pending || has_runnable
}

// ── (3) Owned dispatch-plan classification ──────────────────────────────────────────────────────

/// The minimal task facts the classifier needs, all copied out under the task lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectedTaskContext {
    pub(crate) tid: u64,
    pub(crate) asid: u16,
    pub(crate) home_cpu: u8,
    /// True once the task has entered ring 3 at least once (so a later dispatch is a RESUME, never a
    /// fresh entry). A never-run task is `false`.
    pub(crate) has_run_before: bool,
    /// The saved continuation captured when the task last left ring 3 (blocked syscall / preemption).
    /// For a fresh task these are the ELF entry + initial stack.
    pub(crate) rip: u64,
    pub(crate) rsp: u64,
    pub(crate) user_gprs: [u64; 16],
    pub(crate) cr3: u64,
    pub(crate) kernel_rsp0: u64,
    pub(crate) fs_base: u64,
}

/// Classify the return mode for a scheduler-selected task: a task that has run before RESUMES its
/// saved continuation; a never-run task takes a FRESH entry. This is the ONLY input to the
/// distinction — it never keys off an oracle TID or a hardcoded entry point.
#[inline]
pub(crate) fn classify_return_mode(ctx: &SelectedTaskContext) -> ApReturnMode {
    if ctx.has_run_before {
        ApReturnMode::BlockedUserResume
    } else {
        ApReturnMode::FreshUserEntry
    }
}

/// Build the OWNED dispatch plan for a scheduler-selected task on `cpu`. Returns `Idle` when the
/// selection is the idle task (tid 0) or absent. Refuses (`Idle`) when the task's home CPU is not
/// `cpu` — a plan for another CPU's task is NEVER built here, so the IPI/idle path can never
/// dispatch a task assigned to a different CPU.
pub(crate) fn build_dispatch_plan(
    cpu: CpuId,
    selected: Option<SelectedTaskContext>,
) -> ApDispatchDecision {
    let ctx = match selected {
        Some(c) if c.tid != 0 => c,
        _ => return ApDispatchDecision::Idle,
    };
    if ctx.home_cpu != cpu.0 {
        // A task pinned to another CPU must not be dispatched here (never migrate to prove wake).
        return ApDispatchDecision::Idle;
    }
    let mode = classify_return_mode(&ctx);
    let user_gprs = match mode {
        ApReturnMode::BlockedUserResume => ctx.user_gprs,
        ApReturnMode::FreshUserEntry => [0u64; 16],
    };
    ApDispatchDecision::Dispatch(ApUserDispatchPlan {
        mode,
        tid: ctx.tid,
        asid: ctx.asid,
        home_cpu: ctx.home_cpu,
        cr3: ctx.cr3,
        entry_rip: ctx.rip,
        user_rsp: ctx.rsp,
        user_gprs,
        kernel_rsp0: ctx.kernel_rsp0,
        fs_base: ctx.fs_base,
    })
}

/// Fail-closed identity check performed by the LIVE install path immediately before the user
/// return: the plan may be installed on `cpu` ONLY for the exact `{tid, asid}` the caller
/// re-resolved from the current-CPU ownership under the lock. A drift (wrong tid, wrong asid, or
/// wrong CPU) refuses the return.
#[inline]
pub(crate) fn plan_install_permitted(
    plan: &ApUserDispatchPlan,
    cpu: CpuId,
    current_tid: u64,
    current_asid: u16,
) -> bool {
    plan.home_cpu == cpu.0 && plan.tid == current_tid && plan.asid == current_asid
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(tid: u64, home: u8, has_run: bool) -> SelectedTaskContext {
        SelectedTaskContext {
            tid,
            asid: (tid as u16) + 100,
            home_cpu: home,
            has_run_before: has_run,
            rip: 0x4000 + tid,
            rsp: 0x8000 + tid,
            user_gprs: [tid; 16],
            cr3: 0x1000 * tid,
            kernel_rsp0: 0x9000 + tid,
            fs_base: 0x7000 + tid,
        }
    }

    // (8.1) The reschedule IPI sets pending state; (8.2) repeated IPIs coalesce.
    #[test]
    fn reschedule_pending_sets_and_coalesces() {
        let cpu = CpuId(1);
        reset_reschedule_pending(cpu);
        assert!(!reschedule_pending(cpu));
        set_reschedule_pending(cpu);
        set_reschedule_pending(cpu); // coalesces — still one pending
        set_reschedule_pending(cpu);
        assert!(reschedule_pending(cpu));
        // Consumed exactly once.
        assert!(take_reschedule_pending(cpu));
        assert!(
            !reschedule_pending(cpu),
            "consumed → no residual duplicate wakes"
        );
        assert!(!take_reschedule_pending(cpu), "second consume is empty");
        reset_reschedule_pending(cpu);
    }

    // Pending is per-CPU: an IPI to CPU 1 never marks CPU 0.
    #[test]
    fn reschedule_pending_is_per_cpu() {
        reset_reschedule_pending(CpuId(0));
        reset_reschedule_pending(CpuId(1));
        set_reschedule_pending(CpuId(1));
        assert!(reschedule_pending(CpuId(1)));
        assert!(
            !reschedule_pending(CpuId(0)),
            "CPU 0 not marked by a CPU-1 IPI"
        );
        reset_reschedule_pending(CpuId(1));
    }

    // (8.3) The idle check-to-halt decision cannot lose a remote wake.
    #[test]
    fn idle_decision_never_loses_a_wake() {
        // A pending reschedule (even with no locally-visible runnable yet) forces a dispatch pass.
        assert!(ap_idle_should_dispatch(true, false));
        // A runnable task forces dispatch.
        assert!(ap_idle_should_dispatch(false, true));
        // Only a truly idle CPU halts.
        assert!(!ap_idle_should_dispatch(false, false));
        // Both → dispatch.
        assert!(ap_idle_should_dispatch(true, true));
    }

    // (8.6) The plan is scheduler-selected, not probe-hardcoded; (8.7) fresh vs resume distinct.
    #[test]
    fn fresh_and_resume_plans_are_distinct_and_scheduler_selected() {
        let fresh = build_dispatch_plan(CpuId(1), Some(ctx(2, 1, false)));
        let resume = build_dispatch_plan(CpuId(1), Some(ctx(2, 1, true)));
        match (fresh, resume) {
            (ApDispatchDecision::Dispatch(f), ApDispatchDecision::Dispatch(r)) => {
                assert_eq!(f.mode, ApReturnMode::FreshUserEntry);
                assert_eq!(r.mode, ApReturnMode::BlockedUserResume);
                assert_eq!(
                    f.tid, 2,
                    "plan tid comes from the selection, not a constant"
                );
                // (8.8) Blocked resume uses the SAVED task context (GPRs restored).
                assert_eq!(r.user_gprs, [2u64; 16], "resume restores saved GPRs");
                assert_eq!(r.entry_rip, 0x4000 + 2, "resume uses saved RIP");
                // Fresh entry zeroes GPRs (no stale continuation).
                assert_eq!(f.user_gprs, [0u64; 16], "fresh entry zeroes GPRs");
            }
            _ => panic!("expected two dispatch plans"),
        }
    }

    // (8.5) A task pinned to another CPU is never selected here.
    #[test]
    fn plan_refuses_task_homed_to_another_cpu() {
        // A CPU-0-homed task offered to CPU 1's dispatcher is refused (Idle).
        assert_eq!(
            build_dispatch_plan(CpuId(1), Some(ctx(3, 0, true))),
            ApDispatchDecision::Idle,
            "CPU 1 never dispatches a CPU-0-homed task"
        );
        // The idle-task selection (tid 0) is Idle.
        assert_eq!(
            build_dispatch_plan(CpuId(1), Some(ctx(0, 1, true))),
            ApDispatchDecision::Idle
        );
        // No selection is Idle.
        assert_eq!(
            build_dispatch_plan(CpuId(1), None),
            ApDispatchDecision::Idle
        );
    }

    // (8.9) A wrong {tid,asid} install is rejected.
    #[test]
    fn install_rejects_wrong_identity_or_cpu() {
        let plan = match build_dispatch_plan(CpuId(1), Some(ctx(2, 1, true))) {
            ApDispatchDecision::Dispatch(p) => p,
            _ => panic!("plan"),
        };
        // Correct identity + CPU permitted.
        assert!(plan_install_permitted(&plan, CpuId(1), 2, 102));
        // Wrong tid rejected.
        assert!(!plan_install_permitted(&plan, CpuId(1), 9, 102));
        // Wrong asid rejected (numeric-TID reuse with a different ASID).
        assert!(!plan_install_permitted(&plan, CpuId(1), 2, 999));
        // Wrong CPU rejected.
        assert!(!plan_install_permitted(&plan, CpuId(0), 2, 102));
    }

    // (8.10) The plan carries CPU/task-correct CR3, RSP0 and FS (per-task, not global).
    #[test]
    fn plan_carries_per_task_cr3_rsp0_fs() {
        let a = match build_dispatch_plan(CpuId(1), Some(ctx(2, 1, true))) {
            ApDispatchDecision::Dispatch(p) => p,
            _ => panic!(),
        };
        let b = match build_dispatch_plan(CpuId(1), Some(ctx(5, 1, true))) {
            ApDispatchDecision::Dispatch(p) => p,
            _ => panic!(),
        };
        assert_ne!(a.cr3, b.cr3, "CR3 is per-task");
        assert_ne!(a.kernel_rsp0, b.kernel_rsp0, "RSP0 is per-task");
        assert_ne!(a.fs_base, b.fs_base, "FS base is per-task");
        assert_eq!(a.asid, 102);
        assert_eq!(b.asid, 105);
    }
}
