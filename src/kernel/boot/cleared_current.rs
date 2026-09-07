// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-EXIT3 §2 — the authority to have emptied a CPU's current slot, and the obligation to settle
//! it.
//!
//! # The boundary this exists for
//!
//! Between the rank-1 clear and the rank-2 claim, `current[cpu]` is empty and a task's trap frame
//! is still live on the stack. U9-EXIT2 settled a failure there with
//! `Complete(Err(SyscallError::Internal))`, and reading what the trap bridge actually does with
//! that shows why it was wrong:
//!
//! ```text
//!   Err(TrapHandleError::Syscall(e)) => {
//!       frame.set_err(e.code());
//!       finalize_split_handled_syscall(...);
//!       return Ok(());               // -> the arch tail irets/erets/srets THROUGH `frame`
//!   }
//! ```
//!
//! The error reaches userspace by RESUMING the entering task — with `current[cpu] == None`. The
//! scheduler then believes nothing runs on this CPU while a task runs on it: the task is on no run
//! queue, so once it blocks it is never selected again; every later trap resolves `current_tid()`
//! to `None`; and when the victim lost its claim to a reap or a fault, the frame resumed belongs to
//! a task that is already terminal and whose address space is being torn down.
//!
//! Frame finalization is not scheduler ownership. A settlement may return through the entering
//! frame ONLY if that exact incarnation is this CPU's current again.
//!
//! # The token
//!
//! [`ClearedCurrentToken`] is minted by, and only by, [`clear_current_exact`], which performs a
//! COMPARE-and-clear: it mutates nothing unless the slot named exactly `{tid}`. It carries the
//! priority the removed task held, which is the only record of its placement and therefore the only
//! thing that makes an exact restore possible rather than invented.
//!
//! It is `#[must_use]`, has no `Clone`/`Copy`, exposes no fields, and its `Drop` diverges. So it
//! cannot be dropped, duplicated or forged, and the three consuming methods are the complete set of
//! ways a post-clear state can end:
//!
//! 1. [`ClearedCurrentToken::restore_current_exact`] — the victim is provably still ours and
//!    resumable; it becomes current again and the trap may return through its frame;
//! 2. [`ClearedCurrentToken::publish_queue_advance`] — another owner made the victim
//!    non-resumable, or this transaction's own claim made it terminal; the already-reserved
//!    U9-QA deferral is published and the EXISTING drain selects and applies somebody else;
//! 3. [`ClearedCurrentToken::fatal`] — neither could be proven. Diverges.
//!
//! # U9-EXIT4 §3 — what "linear" is made to mean here
//!
//! The type-level half (no `Clone`, no `Copy`, no public constructor, no field access, `#[must_use]`)
//! was U9-EXIT3's. Three properties it did NOT have are added here, because a convention a caller
//! must remember is not a property the token has:
//!
//! * **No `mem::forget` anywhere, including inside this module.** U9-EXIT3's private `consume`
//!   defused the bomb with `core::mem::forget(self)`. That works, but it makes "this module contains
//!   no abandonment primitive" unprovable by inspection — a reader cannot tell the sanctioned forget
//!   from a smuggled one. The bomb is now *conditional* on a private `settled` flag instead, so the
//!   crate contains no `mem::forget`, `ManuallyDrop` or `ptr::read` naming this type at all and a
//!   guard can say so mechanically (§4, `the_token_is_never_abandoned_through_a_forget_primitive`).
//!   A settled token's `Drop` is a no-op, so the three settlements may run it freely — including
//!   [`ClearedCurrentToken::fatal`], whose `panic!` unwinds through `self` under a hosted test.
//! * **The drain's admission is the token's, not the caller's.** U9-EXIT3 let
//!   `publish_queue_advance` publish whatever it was handed and relied on `settle_failed_claim` to
//!   have asked `victim_is_drain_honourable` first. So "cannot publish an advance the drain will
//!   refuse" held only for the one call site that remembered. The check now lives INSIDE the
//!   settlement, which returns the token on refusal — a caller that skips it does not get an
//!   advance, it gets its obligation back. See [`AdvanceRefusal`].
//! * **`fatal` names an impossible state, not a lost race.** With the admission moved inward, the
//!   only way to reach `fatal` is for the victim to be simultaneously not-restorable and
//!   not-drain-honourable, which U9-EXIT4 §1 proves no production writer can produce.

use crate::kernel::scheduler::CpuId;
use crate::kernel::scheduler::TaskPriority;
use crate::kernel::vm::Asid;

/// What a settled post-clear state licenses the caller to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClearedCurrentSettlement {
    /// The exact incarnation is this CPU's current again. The entering frame is its own, so the
    /// trap may return through it.
    Restored,
    /// The reserved deferral now names an incarnation the existing drain will honour. The trap
    /// must answer `QueueAdvanceCommitted` and must NOT return through the entering frame.
    AdvanceCommitted,
}

/// Why a restore was refused. Recorded so a live log distinguishes "the victim moved on" from
/// "the slot was taken", which are different bugs if either ever appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestoreRefusal {
    /// The TCB is gone, or a replacement incarnation holds the numeric TID.
    IncarnationGone,
    /// Present and ours, but no longer `Running` — some terminal owner claimed it.
    NotRunning,
    /// Present and Running, but current or queued somewhere: another owner already placed it.
    PlacedElsewhere,
    /// The scheduler refused the exact restore (slot re-taken, or the task was re-queued between
    /// the proof and the write).
    SchedulerRefused,
}

impl RestoreRefusal {
    pub(crate) const fn marker(self) -> &'static str {
        match self {
            Self::IncarnationGone => "incarnation_gone",
            Self::NotRunning => "not_running",
            Self::PlacedElsewhere => "placed_elsewhere",
            Self::SchedulerRefused => "scheduler_refused",
        }
    }
}

/// U9-EXIT4 §2/§3 — why an advance was refused **by the settlement itself**.
///
/// U9-EXIT3 had no such type: `publish_queue_advance` was infallible and the drain's admission was
/// evaluated by the one caller that remembered to. Both refusals below are states in which
/// publishing would hand the drain something it rejects, and a rejected drain leaves this CPU with
/// an empty current slot and no selection — the exact failure the token exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdvanceRefusal {
    /// The victim is neither terminal nor removed. `exit_reverify_ok` would answer `Contradicted`,
    /// the drain would decline, and the trap would fall through to a frame whose task the scheduler
    /// does not own. This is the `Runnable`/requeued and `Faulted` shape.
    DrainWouldRefuse,
    /// A deferral is already published on this CPU. A second one is a duplicate, and the cell
    /// refuses rather than overwriting — so the advance this token owes was never taken.
    AlreadyPublished,
}

impl AdvanceRefusal {
    pub(crate) const fn marker(self) -> &'static str {
        match self {
            Self::DrainWouldRefuse => "drain_would_refuse",
            Self::AlreadyPublished => "already_published",
        }
    }
}

/// The acquisitions a post-clear settlement needs. Each method is ONE owner-local acquisition;
/// none of them decides anything.
pub(crate) trait ClearedCurrentOwners {
    /// rank 2 — is this EXACT `{tid, asid}` incarnation present and `Running`?
    fn victim_is_running_exact(&mut self, tid: u64, asid: Option<Asid>) -> bool;
    /// rank 2 — is it terminal or gone, i.e. exactly what the existing exit drain will honour?
    /// Anything else must not be advanced through, because the drain would refuse and the CPU
    /// would be left with an empty current slot and no selection.
    fn victim_is_drain_honourable(&mut self, tid: u64, asid: Option<Asid>) -> bool;
    /// rank 1 — is `tid` current on, or queued on, ANY CPU?
    fn tid_placed_anywhere(&mut self, tid: u64) -> bool;
    /// rank 1 — restore this exact task as `cpu`'s current at `priority`. `false` if the
    /// scheduler refused, which it does unless the slot is still empty and the task unqueued.
    fn restore_current_exact(&mut self, cpu: CpuId, tid: u64, priority: TaskPriority) -> bool;
    /// Name the exact incarnation this CPU's already-reserved deferral advances past. `false`
    /// means one is already published, which is a duplicate.
    fn publish_advance_for(&mut self, cpu: CpuId, tid: u64, asid: Option<Asid>) -> bool;
}

/// U9-EXIT3 §2 — proof that this CPU's current slot was emptied of EXACTLY this incarnation, and
/// the obligation to settle it.
///
/// No public constructor, no `Clone`, no `Copy`, no field access, and a diverging `Drop`. The only
/// ways out are the three consuming methods below.
#[must_use = "a cleared current slot must be settled: restore, advance, or fatal"]
#[derive(Debug)]
pub(crate) struct ClearedCurrentToken {
    cpu: CpuId,
    tid: u64,
    asid: Option<Asid>,
    priority: TaskPriority,
    /// U9-EXIT4 §3 — has one of the three settlements run? Private, never read outside this module,
    /// and the ONLY thing that defuses [`Drop`]. It replaces U9-EXIT3's `core::mem::forget(self)`
    /// so that no abandonment primitive appears anywhere in the crate for this type.
    settled: bool,
}

impl Drop for ClearedCurrentToken {
    fn drop(&mut self) {
        // U9-EXIT4 §3: a SETTLED token has already handed its obligation to a restore, an advance
        // or a divergence, so running its destructor is a no-op. This is what lets the module hold
        // no `mem::forget`: the bomb is conditional rather than defused by forgetting.
        if self.settled {
            return;
        }
        // Reached only if a future edit adds a path that leaves the token unconsumed. There is no
        // safe continuation from here: the CPU's current slot is empty, the entering frame is live,
        // and nothing has decided which of the two it belongs to.
        crate::yarm_log!(
            "CLEARED_CURRENT_TOKEN_DROPPED cpu={} tid={} reason=unsettled_post_clear_state",
            self.cpu.0,
            self.tid
        );
        panic!("cleared current slot left unsettled");
    }
}

impl ClearedCurrentToken {
    /// Mark this token settled and hand back the identity it carried. Private: every public exit
    /// from the token goes through one of the three settlements below.
    ///
    /// Takes `&mut self` rather than `self` deliberately — a by-value `consume` would need
    /// `mem::forget` (or `ManuallyDrop`) to stop `Drop` running, and §3's whole point is that no
    /// such primitive exists for this type. The `debug_assert` states the linearity the flag
    /// enforces: a token settles exactly once, and no settlement calls another.
    fn settle(&mut self) -> (CpuId, u64, Option<Asid>, TaskPriority) {
        debug_assert!(
            !self.settled,
            "a cleared-current token settles exactly once"
        );
        self.settled = true;
        (self.cpu, self.tid, self.asid, self.priority)
    }

    /// **Settlement 1** — restore the exact incarnation as this CPU's current.
    ///
    /// Permitted only when all four hold, checked in this order and each under its own
    /// acquisition: the exact `{tid, asid}` incarnation is present; it is still `Running`; it is
    /// current on and queued on NO CPU; and the scheduler accepts the exact restore, which it does
    /// only into a still-empty slot.
    ///
    /// On success the entering frame belongs to this CPU's current task again, which is the ONLY
    /// state in which a `Complete` disposition may return through it.
    ///
    /// On refusal the token is returned so the caller must still settle it — a failed restore is
    /// not a settlement.
    pub(crate) fn restore_current_exact<O: ClearedCurrentOwners>(
        mut self,
        owners: &mut O,
    ) -> Result<ClearedCurrentSettlement, (Self, RestoreRefusal)> {
        let (cpu, tid, asid, priority) = (self.cpu, self.tid, self.asid, self.priority);
        if !owners.victim_is_running_exact(tid, asid) {
            // One acquisition cannot tell "gone" from "no longer Running"; ask again narrowly so
            // the refusal names the state that actually holds.
            let refusal = if owners.victim_is_drain_honourable(tid, asid) {
                RestoreRefusal::IncarnationGone
            } else {
                RestoreRefusal::NotRunning
            };
            return Err((self, refusal));
        }
        if owners.tid_placed_anywhere(tid) {
            return Err((self, RestoreRefusal::PlacedElsewhere));
        }
        if !owners.restore_current_exact(cpu, tid, priority) {
            return Err((self, RestoreRefusal::SchedulerRefused));
        }
        crate::yarm_log!(
            "CLEARED_CURRENT_RESTORED cpu={} tid={} asid={} result=ok",
            cpu.0,
            tid,
            asid.unwrap_or(Asid(0)).0
        );
        let _ = self.settle();
        Ok(ClearedCurrentSettlement::Restored)
    }

    /// **Settlement 2** — hand the CPU to the existing queue-advance drain.
    ///
    /// `authority` is what makes this legal, and it is unforgeable in both forms: either this
    /// transaction's own [`ExitClaim`] made the victim terminal, or a competing owner did. The
    /// already-reserved U9-QA deferral is used, never a second one, and this is the only place the
    /// exit cell is named for a post-clear settlement.
    ///
    /// # U9-EXIT4 §2 — the drain's admission is checked HERE
    ///
    /// U9-EXIT3 published unconditionally and left `victim_is_drain_honourable` to the caller, so
    /// "cannot publish an advance the drain will refuse" was true of one call site rather than of
    /// the token. Both gates now run inside the settlement, in the order that matters:
    ///
    /// 1. the victim must be exactly what `exit_reverify_ok` honours — terminal or removed. A
    ///    `Runnable`/requeued or `Faulted` victim answers `Contradicted`, the drain declines, and
    ///    the trap falls through to a frame the scheduler does not own;
    /// 2. the per-CPU cell must actually accept the publication. It refuses a duplicate rather than
    ///    overwriting, and a refused publication means the advance this token owes did not happen.
    ///
    /// Either refusal hands the token BACK, so a caller that would have skipped the check does not
    /// get an advance — it gets its obligation returned and must still settle it. `AdvanceAuthority`
    /// therefore states *who* licensed the advance; it never substitutes for *whether* the drain
    /// will take it, including on the `Claimed` path, where the claim's own `Exited(code)` write is
    /// what makes gate (1) pass.
    ///
    /// Both gates are read-only up to the publication, so a refusal mutates nothing.
    pub(crate) fn publish_queue_advance<O: ClearedCurrentOwners>(
        mut self,
        owners: &mut O,
        authority: AdvanceAuthority<'_>,
    ) -> Result<ClearedCurrentSettlement, (Self, AdvanceRefusal)> {
        let (cpu, tid, asid) = (self.cpu, self.tid, self.asid);
        // The authority is not decoration: a claim may only license an advance past the exact
        // incarnation it claimed. Reading it here is what makes `Claimed` unable to stand in for
        // some other victim's settlement.
        if let AdvanceAuthority::Claimed(claim) = authority {
            debug_assert_eq!(claim.tid(), tid, "a claim licenses only its own victim");
            debug_assert_eq!(claim.asid(), asid, "and only its own incarnation");
        }
        if !owners.victim_is_drain_honourable(tid, asid) {
            return Err((self, AdvanceRefusal::DrainWouldRefuse));
        }
        if !owners.publish_advance_for(cpu, tid, asid) {
            return Err((self, AdvanceRefusal::AlreadyPublished));
        }
        crate::yarm_log!(
            "CLEARED_CURRENT_ADVANCE cpu={} tid={} asid={} authority={} published=1 result=ok",
            cpu.0,
            tid,
            asid.unwrap_or(Asid(0)).0,
            authority.marker()
        );
        let _ = self.settle();
        Ok(ClearedCurrentSettlement::AdvanceCommitted)
    }

    /// **Settlement 3** — neither restoration nor a safe advance can be proven.
    ///
    /// Every alternative would run a frame the scheduler does not own, or leave a CPU with an
    /// empty current slot and no selection. Halting with a diagnosable marker is the only correct
    /// disposition, which is the same conclusion `dispatch_torn_fatal` reached for the same class
    /// of disagreement. Never returns.
    ///
    /// # U9-EXIT4 §2 — what reaching this means
    ///
    /// The victim is simultaneously not restorable (absent, not `Running`, or placed on some CPU)
    /// and not drain-honourable (present and NOT terminal). U9-EXIT4 §1 enumerates every production
    /// writer of the exiting `{tid, asid}` and shows none can produce that combination inside the
    /// post-clear window, so this is a settlement for a source-proven-impossible state rather than
    /// for a legitimate restart or fault race. §4's forced interleavings construct the state in the
    /// hosted harness — where the window can be opened by injection — precisely so that the
    /// divergence is exercised without ever being production-reachable.
    ///
    /// `self` is settled before the panic, so unwinding through it under a hosted `should_panic`
    /// test runs a no-op destructor rather than a second, misleading `TOKEN_DROPPED` marker.
    pub(crate) fn fatal(mut self, reason: &'static str) -> ! {
        let (cpu, tid, asid, _) = self.settle();
        crate::yarm_log!(
            "CLEARED_CURRENT_FATAL cpu={} tid={} asid={} reason={}",
            cpu.0,
            tid,
            asid.unwrap_or(Asid(0)).0,
            reason
        );
        panic!("cleared current slot cannot be settled");
    }
}

/// What licenses a queue advance past a cleared incarnation.
#[derive(Debug, Clone, Copy)]
pub(crate) enum AdvanceAuthority<'a> {
    /// THIS transaction claimed the victim terminal. The claim is unforgeable, so this arm cannot
    /// be reached on a path where the exit did not happen.
    Claimed(&'a crate::kernel::boot::exit_claim::ExitClaim),
    /// A COMPETING owner made the victim non-resumable, and `victim_is_drain_honourable` confirmed
    /// the existing drain will honour it.
    VictimNonResumable,
}

impl AdvanceAuthority<'_> {
    pub(crate) const fn marker(self) -> &'static str {
        match self {
            Self::Claimed(_) => "own_claim",
            Self::VictimNonResumable => "victim_non_resumable",
        }
    }
}

/// U9-EXIT3 §2 — THE mint, and the only one.
///
/// A compare-and-clear: nothing is mutated unless `cpu`'s current slot names exactly `tid`. That
/// is the repair for U9-EXIT2's unconditional clear, which removed whatever was current and then
/// refused — leaving a task that was never this transaction's business current nowhere and queued
/// nowhere.
///
/// `None` therefore means "the scheduler had already moved on, and nothing changed", which is a
/// PRE-mutation refusal rather than a post-clear state needing settlement.
pub(crate) fn clear_current_exact(
    sched: &mut crate::kernel::scheduler::SmpScheduler,
    cpu: CpuId,
    tid: u64,
    asid: Option<Asid>,
) -> Option<ClearedCurrentToken> {
    let priority = sched.block_current_exact_on(cpu, crate::kernel::ipc::ThreadId(tid))?;
    Some(ClearedCurrentToken {
        cpu,
        tid,
        asid,
        priority,
        // U9-EXIT4 §3: a freshly minted token is an unsettled obligation. Nothing outside this
        // module can construct one, and nothing anywhere can set this field except `settle`.
        settled: false,
    })
}
