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

use super::*;
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
}

impl Drop for ClearedCurrentToken {
    fn drop(&mut self) {
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
    /// The CPU whose current slot is empty.
    pub(crate) const fn cpu(&self) -> CpuId {
        self.cpu
    }
    /// The incarnation that was removed from it.
    pub(crate) const fn tid(&self) -> u64 {
        self.tid
    }
    pub(crate) const fn asid(&self) -> Option<Asid> {
        self.asid
    }

    /// Consume without running `Drop`. Private: every public exit from the token goes through one
    /// of the three settlements below.
    fn consume(self) -> (CpuId, u64, Option<Asid>, TaskPriority) {
        let parts = (self.cpu, self.tid, self.asid, self.priority);
        core::mem::forget(self);
        parts
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
        self,
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
        let _ = self.consume();
        Ok(ClearedCurrentSettlement::Restored)
    }

    /// **Settlement 2** — hand the CPU to the existing queue-advance drain.
    ///
    /// `authority` is what makes this legal, and it is unforgeable in both forms: either this
    /// transaction's own [`ExitClaim`] made the victim terminal, or a competing owner did and the
    /// drain will honour the result. The already-reserved U9-QA deferral is used, never a second
    /// one, and this is the only place the exit cell is named for a post-clear settlement.
    pub(crate) fn publish_queue_advance<O: ClearedCurrentOwners>(
        self,
        owners: &mut O,
        authority: AdvanceAuthority<'_>,
    ) -> ClearedCurrentSettlement {
        let (cpu, tid, asid, _) = (self.cpu, self.tid, self.asid, self.priority);
        let published = owners.publish_advance_for(cpu, tid, asid);
        crate::yarm_log!(
            "CLEARED_CURRENT_ADVANCE cpu={} tid={} asid={} authority={} published={} result=ok",
            cpu.0,
            tid,
            asid.unwrap_or(Asid(0)).0,
            authority.marker(),
            u8::from(published)
        );
        let _ = self.consume();
        ClearedCurrentSettlement::AdvanceCommitted
    }

    /// **Settlement 3** — neither restoration nor a safe advance can be proven.
    ///
    /// Every alternative would run a frame the scheduler does not own, or leave a CPU with an
    /// empty current slot and no selection. Halting with a diagnosable marker is the only correct
    /// disposition, which is the same conclusion `dispatch_torn_fatal` reached for the same class
    /// of disagreement. Never returns.
    pub(crate) fn fatal(self, reason: &'static str) -> ! {
        let (cpu, tid, asid, _) = self.consume();
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
    })
}
