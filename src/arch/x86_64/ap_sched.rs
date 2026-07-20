// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199A2D2B/199A2D2C1 — GENERIC x86_64 AP dispatch-on-wake mechanism.
//!
//! This module carries the architecture-neutral (asm-free) core of the AP's ability to run
//! SCHEDULER-SELECTED userspace tasks — the piece the accepted AP scaffold (Stages 189D–190B)
//! lacked. It is deliberately NOT oracle-specific: the plan is built from a CPU's real run queue.
//!
//! The return mode is chosen from an EXPLICIT owned return source (Stage 199A2D2C1) — NOT from a
//! `has_run_before` heuristic:
//!   * a never-entered task with a canonical initial frame → [`ApUserReturnSource::FreshEntry`];
//!   * a Runnable task with a COMMITTED saved userspace return frame →
//!     [`ApUserReturnSource::SavedUserFrame`] (continues its blocked syscall exactly once);
//!   * a blocked task whose wake finalization is not committed → NOT dispatchable;
//!   * a Runnable task with neither a valid fresh nor a committed saved source → fail closed.
//!
//! Three concerns live here, each independently testable:
//!   1. the per-CPU RESCHEDULE-PENDING flag the remote-wake IPI sets (bounded interrupt work only);
//!   2. the LOST-WAKEUP-SAFE idle decision (`cli` → inspect → dispatch-or-`sti;hlt`);
//!   3. the OWNED dispatch plan + its explicit return-source selection, carrying only values that
//!      survive after the scheduler/task locks are released.
//!
//! The saved-frame layout REUSES the canonical BSP [`TrapFrame`](crate::kernel::trapframe::TrapFrame)
//! representation (saved_pc/saved_sp/user_gprs + ret0/1/2/error) plus the canonical user segment
//! selectors + RFLAGS — never a second AP-only trap-frame ABI. The final userspace return (loading
//! CR3, installing per-CPU TSS RSP0 / GS / FS, and the fresh or context-restore `iretq`) is the arch
//! asm half; this module produces the OWNED plan that half consumes, so no task/scheduler/capability
//! reference ever escapes a guard.

#![allow(dead_code)]

use crate::kernel::scheduler::CpuId;
use crate::kernel::trapframe::TrapFrame;
use core::sync::atomic::{AtomicBool, Ordering};

/// Canonical ring-3 user segment selectors + RFLAGS the BSP return path installs (see
/// `descriptor_tables`: user CS = 0x23, user SS = 0x1b, RFLAGS = 0x202). Reused verbatim so the AP
/// return uses the SAME frame ABI as the BSP.
pub(crate) const USER_CS: u16 = 0x23;
pub(crate) const USER_SS: u16 = 0x1b;
pub(crate) const USER_RFLAGS: u64 = 0x202;

/// Canonical initial ring-3 frame for a never-entered task (fresh entry). Built through the same
/// entry inputs (`entry`, `stack_top`, up to 6 startup args) the ordinary x86 userspace startup uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FreshUserEntryState {
    pub(crate) entry_rip: u64,
    pub(crate) user_rsp: u64,
    pub(crate) args: [u64; 6],
}

/// A COMMITTED saved userspace return frame (a task that entered ring 3 and left via a
/// trap/syscall, whose wake finalization has committed the resume state). Reuses the canonical BSP
/// [`TrapFrame`] representation: `rip`=saved_pc, `rsp`=saved_sp, `user_gprs`=the 16 x86 GPRs, and the
/// `syscall_ret`/`syscall_err` = ret0/1/2/error syscall-result lanes. Segment selectors + RFLAGS are
/// the canonical user values. `committed` gates whether this frame is a valid resume source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SavedUserReturnFrame {
    pub(crate) rip: u64,
    pub(crate) rsp: u64,
    pub(crate) rflags: u64,
    pub(crate) cs: u16,
    pub(crate) ss: u16,
    pub(crate) user_gprs: [u64; 16],
    pub(crate) syscall_ret: [u64; 3],
    pub(crate) syscall_err: u64,
    pub(crate) committed: bool,
}

impl SavedUserReturnFrame {
    /// Adapt a canonical BSP [`TrapFrame`] into the saved-return representation — the SAME layout,
    /// no re-encoding. The first 16 GPR lanes are the x86 RAX..R15 snapshot.
    pub(crate) fn from_trap_frame(tf: &TrapFrame, committed: bool) -> Self {
        let mut gprs = [0u64; 16];
        for (i, g) in gprs.iter_mut().enumerate() {
            *g = tf.user_gprs[i] as u64;
        }
        Self {
            rip: tf.saved_pc as u64,
            rsp: tf.saved_sp as u64,
            rflags: USER_RFLAGS,
            cs: USER_CS,
            ss: USER_SS,
            user_gprs: gprs,
            syscall_ret: [tf.ret0 as u64, tf.ret1 as u64, tf.ret2 as u64],
            syscall_err: tf.error as u64,
            committed,
        }
    }

    /// A saved frame is a valid resume source only when it is committed and carries a non-null
    /// RIP + RSP (a zeroed/malformed frame is refused — never resumed onto a null continuation).
    pub(crate) fn is_valid_resume_source(&self) -> bool {
        self.committed && self.rip != 0 && self.rsp != 0
    }
}

/// The EXPLICIT owned return source (Stage 199A2D2C1). Copy — no borrow escapes a guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApUserReturnSource {
    FreshEntry(FreshUserEntryState),
    SavedUserFrame(SavedUserReturnFrame),
}

/// The authoritative task-dispatch state used to choose a return source. Derived from the real task
/// status + saved-frame commitment under the task lock — never from a `has_run_before` heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskDispatchState {
    /// Never entered userspace (no saved frame) — canonical fresh entry.
    NeverEntered,
    /// Runnable with a COMMITTED saved userspace return frame.
    RunnableSaved,
    /// Blocked, wake finalization NOT committed — not dispatchable this pass.
    BlockedUnfinalized,
    /// Runnable but neither a valid fresh nor a committed saved source — fail closed.
    Invalid,
}

/// Why a selection was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DispatchReject {
    /// Nothing runnable / idle task selected.
    Idle,
    /// The task is homed to a different CPU (never dispatched here, never migrated).
    WrongHomeCpu,
    /// Blocked without committed wake finalization.
    NotFinalized,
    /// Runnable but no valid return source (missing/malformed) — fail closed.
    NoValidSource,
    /// Identity/CPU revalidation drifted between selection and install.
    IdentityDrift,
}

/// Choose the return source for a selected task from its authoritative state and the available
/// (fresh, saved) candidates. This is the SOLE authority for the fresh-vs-resume distinction.
pub(crate) fn select_return_source(
    state: TaskDispatchState,
    fresh: Option<FreshUserEntryState>,
    saved: Option<SavedUserReturnFrame>,
) -> Result<ApUserReturnSource, DispatchReject> {
    match state {
        TaskDispatchState::NeverEntered => match fresh {
            Some(f) if f.entry_rip != 0 && f.user_rsp != 0 => Ok(ApUserReturnSource::FreshEntry(f)),
            _ => Err(DispatchReject::NoValidSource),
        },
        TaskDispatchState::RunnableSaved => match saved {
            Some(s) if s.is_valid_resume_source() => Ok(ApUserReturnSource::SavedUserFrame(s)),
            // A previously-run task WITHOUT a committed/valid saved frame is refused — it is NOT
            // silently treated as a blocked continuation.
            _ => Err(DispatchReject::NoValidSource),
        },
        TaskDispatchState::BlockedUnfinalized => Err(DispatchReject::NotFinalized),
        TaskDispatchState::Invalid => Err(DispatchReject::NoValidSource),
    }
}

/// An OWNED, guard-free plan the AP executes lock-free to return to userspace. Every field is a
/// plain value copied out under the scheduler/task lock; no `&Task`, no scheduler handle, no
/// capability reference is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApUserDispatchPlan {
    /// The explicit return source (fresh initial frame or committed saved frame).
    pub(crate) source: ApUserReturnSource,
    /// Authoritative task identity — the plan is refused if installed against a different
    /// current-CPU/task ownership than this.
    pub(crate) tid: u64,
    pub(crate) asid: u16,
    /// The CPU this task is HOME to; `home_cpu == self` is asserted before install.
    pub(crate) home_cpu: u8,
    /// Address space root (CR3). `0` = resolve at install time from `asid` (non-hosted).
    pub(crate) cr3: u64,
    /// The ring-0 stack installed as syscall RSP0 + TSS RSP0 before the return.
    pub(crate) kernel_rsp0: u64,
    /// User FS base (TLS). `0` = none.
    pub(crate) fs_base: u64,
}

/// The AP idle dispatcher's decision after inspecting its run queue under the CPU scheduler lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApDispatchDecision {
    Dispatch(ApUserDispatchPlan),
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

/// Atomically CONSUME the pending flag (returns its prior value). Consumed once by the idle
/// dispatcher before it inspects the run queue; a concurrent set after the consume re-arms it so the
/// next idle iteration re-checks (no lost wake).
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
/// atomic sequence. An enqueue path both sets the pending flag AND sends the wake IPI, so an enqueue
/// after this check but before `hlt` leaves the IPI pending in the LAPIC; `sti; hlt` returns
/// immediately (the classic cli/sti;hlt no-lost-wakeup pattern). The pending flag's Release/Acquire
/// carries the happens-before — the IPI is never the sole memory-ordering primitive.
#[inline]
pub(crate) fn ap_idle_should_dispatch(reschedule_pending: bool, has_runnable: bool) -> bool {
    reschedule_pending || has_runnable
}

// ── (3) Owned scheduler-selected dispatch plan ─────────────────────────────────────────────────

/// The minimal task facts the plan builder needs, all copied out under the task lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectedTaskContext {
    pub(crate) tid: u64,
    pub(crate) asid: u16,
    pub(crate) home_cpu: u8,
    pub(crate) state: TaskDispatchState,
    pub(crate) fresh: Option<FreshUserEntryState>,
    pub(crate) saved: Option<SavedUserReturnFrame>,
    pub(crate) cr3: u64,
    pub(crate) kernel_rsp0: u64,
    pub(crate) fs_base: u64,
}

/// Build the OWNED dispatch plan for a scheduler-selected task on `cpu`. `Err` classifies the
/// refusal: idle/no selection, a task homed to another CPU (never dispatched here), a blocked
/// unfinalized task, or a runnable task with no valid return source (fail closed). The return-source
/// selection is the SOLE fresh-vs-resume authority — never `has_run_before`.
pub(crate) fn build_dispatch_plan(
    cpu: CpuId,
    selected: Option<SelectedTaskContext>,
) -> Result<ApUserDispatchPlan, DispatchReject> {
    let ctx = match selected {
        Some(c) if c.tid != 0 => c,
        _ => return Err(DispatchReject::Idle),
    };
    if ctx.home_cpu != cpu.0 {
        return Err(DispatchReject::WrongHomeCpu);
    }
    let source = select_return_source(ctx.state, ctx.fresh, ctx.saved)?;
    Ok(ApUserDispatchPlan {
        source,
        tid: ctx.tid,
        asid: ctx.asid,
        home_cpu: ctx.home_cpu,
        cr3: ctx.cr3,
        kernel_rsp0: ctx.kernel_rsp0,
        fs_base: ctx.fs_base,
    })
}

/// Convenience decision wrapper: `Idle` refusals collapse to [`ApDispatchDecision::Idle`], other
/// refusals propagate as `Err` so the caller can fail closed loudly.
pub(crate) fn decide_dispatch(
    cpu: CpuId,
    selected: Option<SelectedTaskContext>,
) -> Result<ApDispatchDecision, DispatchReject> {
    match build_dispatch_plan(cpu, selected) {
        Ok(plan) => Ok(ApDispatchDecision::Dispatch(plan)),
        Err(DispatchReject::Idle) => Ok(ApDispatchDecision::Idle),
        Err(e) => Err(e),
    }
}

/// Fail-closed identity check performed by the LIVE install path immediately before the user return:
/// the plan may install on `cpu` ONLY for the exact `{tid, asid}` re-resolved from the current-CPU
/// ownership under the lock, still Runnable and still homed to this CPU. Any drift refuses.
#[inline]
pub(crate) fn plan_install_permitted(
    plan: &ApUserDispatchPlan,
    cpu: CpuId,
    current_tid: u64,
    current_asid: u16,
    still_runnable: bool,
) -> bool {
    plan.home_cpu == cpu.0 && plan.tid == current_tid && plan.asid == current_asid && still_runnable
}

/// True iff a plan is a fresh entry (for the LIVE installer to pick the fresh vs restore asm).
#[inline]
pub(crate) fn plan_is_fresh(plan: &ApUserDispatchPlan) -> bool {
    matches!(plan.source, ApUserReturnSource::FreshEntry(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh(rip: u64, rsp: u64) -> FreshUserEntryState {
        FreshUserEntryState {
            entry_rip: rip,
            user_rsp: rsp,
            args: [0; 6],
        }
    }

    fn trap_with(
        rip: usize,
        rsp: usize,
        gprs: [usize; 32],
        ret: [usize; 3],
        err: usize,
    ) -> TrapFrame {
        let mut tf = TrapFrame::zeroed();
        tf.saved_pc = rip;
        tf.saved_sp = rsp;
        tf.user_gprs = gprs;
        tf.ret0 = ret[0];
        tf.ret1 = ret[1];
        tf.ret2 = ret[2];
        tf.error = err;
        tf
    }

    fn ctx(
        tid: u64,
        home: u8,
        state: TaskDispatchState,
        fresh: Option<FreshUserEntryState>,
        saved: Option<SavedUserReturnFrame>,
    ) -> SelectedTaskContext {
        SelectedTaskContext {
            tid,
            asid: (tid as u16) + 100,
            home_cpu: home,
            state,
            fresh,
            saved,
            cr3: 0x1000 * tid,
            kernel_rsp0: 0x9000 + tid,
            fs_base: 0x7000 + tid,
        }
    }

    // ── reschedule-pending (Part 3) ──
    #[test]
    fn reschedule_pending_sets_and_coalesces() {
        let cpu = CpuId(1);
        reset_reschedule_pending(cpu);
        assert!(!reschedule_pending(cpu));
        set_reschedule_pending(cpu);
        set_reschedule_pending(cpu);
        set_reschedule_pending(cpu);
        assert!(
            reschedule_pending(cpu),
            "repeated IPIs coalesce to one pending"
        );
        assert!(take_reschedule_pending(cpu));
        assert!(!reschedule_pending(cpu), "consumed once");
        assert!(!take_reschedule_pending(cpu), "second consume empty");
        reset_reschedule_pending(cpu);
    }

    #[test]
    fn reschedule_pending_is_per_cpu() {
        reset_reschedule_pending(CpuId(0));
        reset_reschedule_pending(CpuId(1));
        set_reschedule_pending(CpuId(1));
        assert!(reschedule_pending(CpuId(1)));
        assert!(!reschedule_pending(CpuId(0)));
        reset_reschedule_pending(CpuId(1));
    }

    // ── idle decision (Part 4) ──
    #[test]
    fn idle_decision_never_loses_a_wake() {
        assert!(ap_idle_should_dispatch(true, false));
        assert!(ap_idle_should_dispatch(false, true));
        assert!(!ap_idle_should_dispatch(false, false));
        assert!(ap_idle_should_dispatch(true, true));
    }

    // (8.8) fresh vs saved-frame distinguished WITHOUT has_run_before (explicit state).
    #[test]
    fn fresh_and_saved_distinguished_by_explicit_state() {
        let f = build_dispatch_plan(
            CpuId(1),
            Some(ctx(
                2,
                1,
                TaskDispatchState::NeverEntered,
                Some(fresh(0x4000, 0x8000)),
                None,
            )),
        )
        .expect("fresh plan");
        assert!(plan_is_fresh(&f));
        let saved = SavedUserReturnFrame::from_trap_frame(
            &trap_with(0x4444, 0x8888, [7; 32], [1, 2, 3], 0),
            true,
        );
        let r = build_dispatch_plan(
            CpuId(1),
            Some(ctx(
                2,
                1,
                TaskDispatchState::RunnableSaved,
                None,
                Some(saved),
            )),
        )
        .expect("saved plan");
        assert!(!plan_is_fresh(&r));
        assert!(matches!(r.source, ApUserReturnSource::SavedUserFrame(_)));
    }

    // (8.1/8.2/8.3) saved frame preserves RIP/RSP/RFLAGS/CS/SS.
    #[test]
    fn saved_frame_preserves_rip_rsp_rflags_cs_ss() {
        let s = SavedUserReturnFrame::from_trap_frame(
            &trap_with(0xDEAD, 0xBEEF, [0; 32], [0; 3], 0),
            true,
        );
        assert_eq!(s.rip, 0xDEAD);
        assert_eq!(s.rsp, 0xBEEF);
        assert_eq!(s.rflags, USER_RFLAGS);
        assert_eq!(s.cs, USER_CS);
        assert_eq!(s.ss, USER_SS);
    }

    // (8.3/8.10) all required GPRs preserved and NOT zeroed for a resume.
    #[test]
    fn saved_frame_preserves_all_gprs_not_zeroed() {
        let mut gprs = [0usize; 32];
        for (i, g) in gprs.iter_mut().enumerate() {
            *g = 0x100 + i;
        }
        let s = SavedUserReturnFrame::from_trap_frame(&trap_with(1, 1, gprs, [0; 3], 0), true);
        for i in 0..16 {
            assert_eq!(
                s.user_gprs[i],
                (0x100 + i) as u64,
                "GPR {i} preserved, not zeroed"
            );
        }
    }

    // (8.4) encoded syscall result state preserved.
    #[test]
    fn saved_frame_preserves_syscall_result_state() {
        let s = SavedUserReturnFrame::from_trap_frame(
            &trap_with(1, 1, [0; 32], [0xAA, 0xBB, 0xCC], 0xEE),
            true,
        );
        assert_eq!(s.syscall_ret, [0xAA, 0xBB, 0xCC]);
        assert_eq!(s.syscall_err, 0xEE);
    }

    // (8.5) missing/malformed saved frames rejected (not silently resumed).
    #[test]
    fn missing_or_malformed_saved_frame_rejected() {
        // A previously-run task without a committed saved frame is rejected — NOT a blocked resume.
        assert_eq!(
            select_return_source(TaskDispatchState::RunnableSaved, None, None),
            Err(DispatchReject::NoValidSource)
        );
        // Uncommitted saved frame rejected.
        let uncommitted =
            SavedUserReturnFrame::from_trap_frame(&trap_with(0x1, 0x2, [0; 32], [0; 3], 0), false);
        assert_eq!(
            select_return_source(TaskDispatchState::RunnableSaved, None, Some(uncommitted)),
            Err(DispatchReject::NoValidSource)
        );
        // Committed but null RIP/RSP (malformed) rejected.
        let malformed =
            SavedUserReturnFrame::from_trap_frame(&trap_with(0, 0, [0; 32], [0; 3], 0), true);
        assert_eq!(
            select_return_source(TaskDispatchState::RunnableSaved, None, Some(malformed)),
            Err(DispatchReject::NoValidSource)
        );
        // A blocked unfinalized task is not dispatchable.
        assert_eq!(
            select_return_source(TaskDispatchState::BlockedUnfinalized, None, None),
            Err(DispatchReject::NotFinalized)
        );
    }

    // (8.6) identity replacement rejected at install.
    #[test]
    fn install_rejects_identity_replacement_or_wrong_cpu() {
        let plan = build_dispatch_plan(
            CpuId(1),
            Some(ctx(
                2,
                1,
                TaskDispatchState::NeverEntered,
                Some(fresh(0x4000, 0x8000)),
                None,
            )),
        )
        .expect("plan");
        assert!(plan_install_permitted(&plan, CpuId(1), 2, 102, true));
        assert!(
            !plan_install_permitted(&plan, CpuId(1), 9, 102, true),
            "wrong tid"
        );
        assert!(
            !plan_install_permitted(&plan, CpuId(1), 2, 999, true),
            "wrong asid (replacement)"
        );
        assert!(
            !plan_install_permitted(&plan, CpuId(0), 2, 102, true),
            "wrong CPU"
        );
        assert!(
            !plan_install_permitted(&plan, CpuId(1), 2, 102, false),
            "no longer runnable"
        );
    }

    // (8.7) wrong-home-CPU selection rejected.
    #[test]
    fn wrong_home_cpu_selection_rejected() {
        assert_eq!(
            build_dispatch_plan(
                CpuId(1),
                Some(ctx(
                    3,
                    0,
                    TaskDispatchState::NeverEntered,
                    Some(fresh(0x1, 0x1)),
                    None
                ))
            ),
            Err(DispatchReject::WrongHomeCpu)
        );
        // idle-task / no selection → Idle refusal.
        assert!(matches!(
            decide_dispatch(CpuId(1), None),
            Ok(ApDispatchDecision::Idle)
        ));
        assert!(matches!(
            decide_dispatch(
                CpuId(1),
                Some(ctx(
                    0,
                    1,
                    TaskDispatchState::NeverEntered,
                    Some(fresh(1, 1)),
                    None
                ))
            ),
            Ok(ApDispatchDecision::Idle)
        ));
    }

    // (8.9) same canonical return-frame layout as the BSP (TrapFrame adaptation is lossless for the
    // fields the return path consumes).
    #[test]
    fn saved_frame_uses_canonical_bsp_layout() {
        let mut tf = TrapFrame::zeroed();
        tf.saved_pc = 0x1234;
        tf.saved_sp = 0x5678;
        tf.user_gprs[0] = 0xAAA; // rax
        tf.user_gprs[15] = 0xF15; // r15
        let s = SavedUserReturnFrame::from_trap_frame(&tf, true);
        assert_eq!(s.rip as usize, tf.saved_pc);
        assert_eq!(s.rsp as usize, tf.saved_sp);
        assert_eq!(s.user_gprs[0] as usize, tf.user_gprs[0]);
        assert_eq!(s.user_gprs[15] as usize, tf.user_gprs[15]);
    }

    // Fresh entry requires a valid initial frame (a NeverEntered task with no fresh source fails
    // closed rather than being treated as a resume).
    #[test]
    fn fresh_entry_requires_valid_initial_frame() {
        assert_eq!(
            select_return_source(TaskDispatchState::NeverEntered, None, None),
            Err(DispatchReject::NoValidSource)
        );
        assert_eq!(
            select_return_source(TaskDispatchState::NeverEntered, Some(fresh(0, 0)), None),
            Err(DispatchReject::NoValidSource)
        );
    }
}
