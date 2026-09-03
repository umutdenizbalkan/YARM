// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-FORK1 §4 — the broad acquisitions behind the seven fork owners.
//!
//! Each function here takes exactly one domain lock (or, for the clone, the one legal VM→memory
//! pair) and delegates to a rank-local body. None of them contains policy: the policy is
//! [`crate::kernel::syscall::fork_txn::fork_process_cow`], which both the broad and the split
//! adapter execute.

use super::{KernelError, KernelState};
use crate::kernel::capabilities::CapId;
use crate::kernel::spawn_reservation::ReservationRefusal;
use crate::kernel::syscall::fork_txn::{
    ForkChildPublication, ForkParentFacts, InheritableCaps, fork_should_inherit_capability,
};
use crate::kernel::task::{ThreadControlBlock, ThreadGroupId};

impl KernelState {
    /// rank 2 — every fact about the parent a fork needs, under ONE acquisition.
    ///
    /// The brk bounds are read in the SAME acquisition as the TCB. They used to be a third,
    /// separate lookup performed after the child was already registered, which off the broad lock
    /// could observe a parent that had since called `brk`.
    pub(crate) fn fork_parent_snapshot(&mut self, tid: u64) -> Option<ForkParentFacts> {
        // `&mut self` because a single task-domain acquisition that reaches BOTH the TCB array
        // and the class array is spelled `with_task_enqueue_policy_mut` in this codebase. The
        // closure only reads; what matters is that the class comes from the same acquisition as
        // the TCB, so a parent replaced between them cannot contribute one task's context and
        // another's class.
        let facts = self.with_task_enqueue_policy_mut(|tcbs, classes| {
            let idx = tcbs
                .iter()
                .position(|slot| slot.as_ref().is_some_and(|tcb| tcb.tid.0 == tid))?;
            let tcb: &ThreadControlBlock = tcbs[idx].as_ref()?;
            let class = (*classes.get(idx)?)?;
            let asid = tcb.asid?;
            Some(ForkParentFacts {
                tid,
                class,
                asid,
                tls_ptr: tcb.tls_ptr,
                user_entry: tcb.user_entry,
                user_stack_top: tcb.user_stack_top,
                user_context: tcb.user_context,
                // Filled in below, from the memory domain.
                brk_bounds: None,
            })
        })?;
        // rank 6, after rank 2 released: brk bounds live in the memory domain, and 2 → 6 is the
        // legal order. Nothing between the two reads can move the parent's identity, because the
        // identity (`tid`, `asid`) is already captured and the fork proceeds against that.
        Some(ForkParentFacts {
            brk_bounds: self.task_brk_bounds(tid),
            ..facts
        })
    }

    /// rank 4 — the capabilities a fork inherits, by the existing exhaustive policy.
    ///
    /// The policy itself is `fork_should_inherit_capability`, stated once in `fork_txn.rs`; this
    /// owner only applies it to the snapshot.
    pub(crate) fn snapshot_inheritable_caps(
        &self,
        tid: u64,
    ) -> Result<InheritableCaps, KernelError> {
        let snapshot = self.snapshot_live_capabilities_for_task(tid)?;
        let mut out: InheritableCaps = alloc::vec::Vec::new();
        for (cap, capability) in snapshot {
            if fork_should_inherit_capability(capability.object) {
                out.push((CapId(cap.0), capability.rights()));
            }
        }
        Ok(out)
    }

    /// Account for the TLB work a parent write-protection owes — the BROAD path's half.
    ///
    /// The local invalidation already happened inside the VM lock: every architecture's
    /// `map_page` ends in its own `invalidate_page`. What can be left is the remote half, and
    /// what this path can do about it is bounded by where it runs.
    ///
    /// * **Nothing owed.** An empty plan (no run was downgraded), or AArch64, whose
    ///   `tlbi vaae1is` is inner-shareable and therefore already broadcast. Both are complete.
    /// * **Owed, and this path cannot discharge it.** The broad route holds the broad lock for
    ///   the whole syscall, and §14.4 D3 forbids waiting for an ACK under any lock. It therefore
    ///   DEFERS, through the same `VM_TLB_SHOOTDOWN_DEFERRED` accounting the broad COW handler
    ///   already uses, and reports complete — which is exactly the behaviour this route had
    ///   before U9-FORK1 and no weaker. The split route, which holds no lock, PERFORMS it.
    ///
    /// Returning `false` here would refuse every broad fork on a multi-threaded process, which is
    /// a regression rather than a fix; the fix is the split route, and the deferral is what makes
    /// the difference between the two visible in a log instead of silent.
    pub(crate) fn complete_cow_write_protect_shootdown(
        &mut self,
        plan: &super::cow_clone::CowShootdownPlan,
    ) -> bool {
        if plan.is_empty() {
            return true;
        }
        if !super::cow_clone::remote_write_protect_work_is_owed() {
            crate::yarm_log!(
                "FORK_COW_SHOOTDOWN asid={} runs={} pages={} result=broadcast_by_hardware",
                plan.asid.0,
                plan.runs.len(),
                plan.pages()
            );
            return true;
        }
        crate::yarm_log!(
            "VM_TLB_SHOOTDOWN_DEFERRED reason=broad_lock_held asid={} runs={} pages={}",
            plan.asid.0,
            plan.runs.len(),
            plan.pages()
        );
        crate::yarm_log!(
            "FORK_COW_SHOOTDOWN asid={} runs={} pages={} result=deferred_broad",
            plan.asid.0,
            plan.runs.len(),
            plan.pages()
        );
        true
    }

    /// rank 2 — THE fork publication: the child's inherited state and the reservation commit,
    /// under ONE acquisition.
    ///
    /// Sibling of `publish_spawned_image_locked` and identical in shape: validate before writing
    /// anything, install, then commit — and the commit cannot fail, because validation checked
    /// every condition it re-checks and the task lock is held across all three. The difference is
    /// what gets installed: a spawn builds a fresh context from an image, a fork installs the
    /// parent's verbatim with the return lane already zeroed.
    ///
    /// The child is NOT scheduler-visible when this returns; the enqueue is rank 1 and separate.
    pub(crate) fn publish_forked_child(
        &mut self,
        reservation: &crate::kernel::spawn_reservation::SpawnReservationToken,
        publication: &ForkChildPublication,
    ) -> Result<(), ReservationRefusal> {
        let result = self.with_tcbs_mut(|tcbs| {
            // ── 1. Validate, before a single field is written.
            crate::kernel::spawn_reservation::validate_commit_ready(tcbs, reservation)?;

            // ── 2. Install.
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == publication.tid)
                .ok_or(ReservationRefusal::TaskMissing)?;
            // A fork child is its own process: it leads its own thread group.
            tcb.thread_group_id = ThreadGroupId(publication.tid);
            tcb.asid = Some(publication.asid);
            tcb.tls_ptr = publication.tls_ptr;
            tcb.user_entry = publication.user_entry;
            tcb.user_stack_top = publication.user_stack_top;
            tcb.user_context = publication.user_context;

            // ── 3. Commit. Step 1 proved every condition this re-checks.
            crate::kernel::spawn_reservation::commit_live_spawn(tcbs, reservation)
        });
        result?;
        // rank 2 released. The remaining per-task bookkeeping is other domains', and none of it
        // can fail the publication: a TLS restore slot and a robust-futex slot are claims on
        // fixed arrays, and the brk copy is a memory-domain write for a task that now exists.
        if publication.tls_ptr.is_some()
            && let Some(slot) = self.tls_restore_pending.iter_mut().find(|slot| {
                slot.is_some_and(|pending| pending.0 == publication.tid) || slot.is_none()
            })
        {
            *slot = Some(crate::kernel::ipc::ThreadId(publication.tid));
        }
        for slot in self.robust_futex.iter_mut() {
            if slot.is_some_and(|entry| entry.tid.0 == publication.tid) {
                *slot = None;
            }
        }
        if let Some((base, end)) = publication.brk_bounds {
            let _ = self.set_task_brk_bounds(publication.tid, base, end);
        }
        crate::yarm_log!(
            "FORK_CHILD_PUBLISHED tid={} asid={} pc=0x{:x} sp=0x{:x} ret0={} arg0={}",
            publication.tid,
            publication.asid.0,
            publication.user_context.instruction_ptr.0,
            publication.user_context.stack_ptr.0,
            publication.user_context.user_gprs[0],
            publication.user_context.arg0
        );
        Ok(())
    }

    /// rank 2 — remove a child that WAS published, for the one failure arm after the commit.
    ///
    /// Reached through `unregister_thread_incarnation_locked`, the same inverse a failed thread
    /// registration uses, so the TCB slot is cleared by its single owner rather than by a second
    /// slot-clearing site. The expected group is the child's own TID, which is what the
    /// publication set — so this cannot remove a task that is not the one this fork published.
    /// rank 4 — the global reserved CNode-slot budget, for the capacity diagnostic.
    pub(crate) fn reserved_cnode_slot_total(&self) -> usize {
        self.with_capability_state(|capability| {
            capability
                .cnode_spaces
                .iter()
                .flatten()
                .map(|space| space.slot_capacity)
                .sum::<usize>()
        })
    }

    pub(crate) fn remove_published_fork_child(&mut self, tid: u64) -> bool {
        let removed = self.with_task_enqueue_policy_mut(|tcbs, classes| {
            super::spawn_thread_core::unregister_thread_incarnation_locked(
                tcbs,
                classes,
                tid,
                ThreadGroupId(tid),
            )
        });
        crate::yarm_log!(
            "FORK_CHILD_UNPUBLISHED tid={} removed={}",
            tid,
            u8::from(removed)
        );
        removed
    }
}
