// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-FORK1 §3/§4 — THE fork transaction, over [`SpawnTxnOwners`].
//!
//! # Why this is a spawn transaction and not a second one
//!
//! Fork creates the same things a spawn creates — a TID, a process CNode with its PID association,
//! a class, a kernel context, a TCB, an address space, capabilities, a published user context and
//! a run-queue entry — and it must undo the same things in the same order. The one thing it does
//! differently is where the address space and the user context come from: a spawn builds them from
//! an image, a fork copies them from a parent. So this is not a new owner interface. It is the
//! delivered [`SpawnTxnOwners`] plus the six operations §1 proved missing, and the reservation
//! lifecycle underneath it is the one `reserve_task_for_spawn` already provides.
//!
//! That reuse is what closes the ledger. The broad fork this replaces called
//! `register_task_with_class`, which creates an **already-live** task, and then returned `Err`
//! from seven later steps without undoing it. A fork that failed at `set_process_cnode_for_pid`,
//! at capability inheritance, at the TCB write, at the brk copy or at the enqueue left behind a
//! registered, `Runnable`, CNode-owning task that no run queue held and whose TID could never be
//! reallocated — and the enqueue arm additionally destroyed that task's address space on the way
//! out, leaving a runnable TCB pointing at a freed ASID. A reservation is `TaskStatus::Reserved`:
//! it cannot be dispatched, enqueued, woken, blocked, joined or published, and
//! `cancel_spawn_reservation` removes it exactly.
//!
//! # Order
//!
//! ```text
//! 1  rank 2   parent snapshot (ONE acquisition: class, asid, tls, entry, stack, context, brk)
//! 2  rank 2   allocate the child TID
//! 3  rank 4→2 reserve the child  ── CNode + PID + class + kernel context, exactly compensated
//! 4  rank 5→6 COW clone          ── ONE acquisition; parent downgraded, child mapped, no copy
//! 5  no lock  the parent's owed TLB shootdown          ── fail closed if it cannot complete
//! 6  rank 4   capability inheritance, delegation by delegation
//! 7  rank 2   PUBLICATION: the inherited context AND the Reserved→LiveSpawned commit, together
//! 8  rank 1   enqueue, last
//! ```
//!
//! Every step from 3 on has an exact inverse, and every failure runs the inverses of the steps
//! that completed, in reverse order. Nothing is published before step 7, so the child is not
//! observable while any of that can still happen; nothing is enqueued before step 8, so it is not
//! dispatchable until everything else is done.
//!
//! # Where the child's register context comes from
//!
//! From the parent's LIVE trap frame, passed in as `parent_context` — never from the parent's
//! TCB. The TCB copy is refreshed by `sync_current_thread_from_frame`, which a split route runs
//! AFTER the dispatcher, so a child built from the TCB inherits the parent's context from its
//! PREVIOUS trap. On RISC-V that is the `ecall` address rather than `ecall + 4`, and the child
//! resumes onto its own fork instruction and forks again — a re-fork loop bounded only by
//! capacity, which is exactly what a first live matrix showed (12, 9 and 2 forks across three
//! otherwise identical boots). The frame is authoritative for the in-flight syscall on every
//! architecture and on both routes, so both pass it and the two cannot diverge.
//!
//! `None` is the no-frame caller — hosted bring-up and the focused tests — which falls back to
//! the TCB snapshot explicitly rather than ambiently.
//!
//! # Where the parent's identity comes from
//!
//! Step 1 and nowhere else. After it, the transaction reads no ambient current task: every later
//! step is handed the exact `parent_tid`, `parent_pid` and `ForkParentFacts` that snapshot
//! produced. Under the broad lock the difference is invisible; off it, a second read could observe
//! a different task.

use crate::kernel::boot::KernelError;
use crate::kernel::boot::cow_clone::{CowCloneRollback, CowCloneToken};
use crate::kernel::capabilities::{CapId, CapRights};
use crate::kernel::spawn_reservation::SpawnReservationToken;
use crate::kernel::task::{TaskClass, UserRegisterContext};
use crate::kernel::vm::{Asid, VirtAddr};

use super::spawn_txn::{SpawnTxnOwners, cancel_spawn_reservation, reserve_task_for_spawn};

/// Everything about the parent a fork needs, read under ONE task-domain acquisition.
///
/// Taken once, before any mutation. The three separate reads it replaces — the TCB clone, the
/// class lookup and the brk-bounds lookup — could each observe a different incarnation off the
/// broad lock, and the child would then be assembled from two parents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ForkParentFacts {
    pub(crate) tid: u64,
    pub(crate) class: TaskClass,
    pub(crate) asid: Asid,
    pub(crate) tls_ptr: Option<VirtAddr>,
    pub(crate) user_entry: Option<VirtAddr>,
    pub(crate) user_stack_top: Option<VirtAddr>,
    /// The parent's LAST SAVED context. It is authoritative only for a caller that has no live
    /// trap frame; a syscall route must pass its frame's own capture instead. See
    /// [`fork_process_cow`]'s `parent_context`.
    pub(crate) user_context: UserRegisterContext,
    pub(crate) brk_bounds: Option<(usize, usize)>,
}

/// The child's whole published state, installed under ONE task-domain acquisition together with
/// the reservation commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ForkChildPublication {
    pub(crate) tid: u64,
    pub(crate) asid: Asid,
    pub(crate) tls_ptr: Option<VirtAddr>,
    pub(crate) user_entry: Option<VirtAddr>,
    pub(crate) user_stack_top: Option<VirtAddr>,
    /// The parent's register context, with the fork return lane already zeroed by
    /// [`fork_child_context`]. Installed verbatim, so the child resumes at the instruction after
    /// the syscall with the parent's stack, TLS and general registers.
    pub(crate) user_context: UserRegisterContext,
    pub(crate) brk_bounds: Option<(usize, usize)>,
}

/// THE child register context: the parent's, with the return lane zeroed.
///
/// Both lanes are zeroed, and both are load-bearing on at least one architecture:
///
/// * `user_gprs[0]` is the AUTHORITATIVE lane for a **resumed** task. On x86_64 a resumed task is
///   restored by `write_task_gprs_to_saved_regs` with `rax = user_gpr(0)`, and at syscall entry
///   `user_gpr(0)` holds `rax` — the syscall NUMBER, 12. A child that inherited the parent's
///   snapshot verbatim would return 12 and classify itself as the parent.
/// * `arg0` is the lane the NEW-task dispatch path injects, and it is what an architecture that
///   builds a first-dispatch frame reads.
///
/// Zeroing both is what makes "parent receives the child identity, child receives zero" hold on
/// all three architectures without a per-architecture branch.
pub(crate) fn fork_child_context(parent: &UserRegisterContext) -> UserRegisterContext {
    let mut context = *parent;
    context.user_gprs[0] = 0;
    context.arg0 = 0;
    context
}

/// THE fork transaction. One policy, executed identically by the broad and the split adapter.
///
/// Returns the child TID. The caller places it in the parent's return lane; the child's zero was
/// installed by [`fork_child_context`] at publication.
pub(crate) fn fork_process_cow<O: SpawnTxnOwners>(
    owners: &mut O,
    parent_tid: u64,
    parent_context: Option<UserRegisterContext>,
) -> Result<u64, KernelError> {
    // Proof-gated step diagnostics, active only under the sender-wake sub-knob. They name the
    // SAME phases the pre-U9-FORK1 fork named, so the existing oracles read unchanged; what
    // changed underneath is that each phase now has an inverse.
    let proof = crate::kernel::boot::ipc_recv_proof_sender_wake_active();

    // ── 1. The parent, once. Every refusal below this line is still pre-mutation. ───────
    let Some(parent) = owners.fork_parent_snapshot(parent_tid) else {
        if proof {
            crate::yarm_log!("FORK_PROOF_PRECHECK_FAIL reason=parent_snapshot_missing");
        }
        crate::yarm_log!(
            "FORK_TXN_REFUSED parent_tid={} reason=parent_snapshot_missing",
            parent_tid
        );
        return Err(KernelError::TaskMissing);
    };
    if !owners.address_space_exists(parent.asid) {
        if proof {
            crate::yarm_log!("FORK_PROOF_PRECHECK_FAIL reason=parent_asid_missing");
        }
        crate::yarm_log!(
            "FORK_TXN_REFUSED parent_tid={} reason=parent_asid_missing asid={}",
            parent_tid,
            parent.asid.0
        );
        return Err(KernelError::UserMemoryFault);
    }

    if proof {
        crate::yarm_log!("FORK_PROOF_PRECHECK_OK parent_tid={}", parent_tid);
        crate::yarm_log!("FORK_PROOF_ALLOC_CHILD_BEGIN");
    }

    // ── 2. The child TID. ──────────────────────────────────────────────────────────────
    let child_tid = match owners.allocate_thread_id() {
        Ok(tid) => tid,
        Err(err) => {
            if proof {
                crate::yarm_log!("FORK_PROOF_ALLOC_CHILD_FAIL reason={:?} step=tid", err);
            }
            return Err(err);
        }
    };

    // ── 3. FIRST MUTATION WITH AN INVERSE: the reservation. A fork child is its own process,
    //       so its process PID is its own TID — the same convention the broad fork used when it
    //       set `thread_group_id = ThreadGroupId(child_tid)`.
    if proof {
        crate::yarm_log!("FORK_PROOF_CNODE_BEGIN");
    }
    let reservation = match reserve_task_for_spawn(owners, child_tid, parent.class, child_tid) {
        Ok(reservation) => reservation,
        Err(err) => {
            if proof {
                // The reservation owns the child's CNode, its class and its kernel context, so a
                // refusal here is exactly the capacity failure the old `register_task_with_class`
                // arm reported — and it reports the same three budgets, from the same owners.
                let limits = owners.capacity_limits();
                crate::yarm_log!(
                    "FORK_PROOF_ALLOC_CHILD_CAPACITY step=reserve reason={:?} live_tasks={} max_tasks={} reserved_cnode_slots={} max_total_cnode_slots={}",
                    err,
                    owners.live_task_count(),
                    limits.max_tasks,
                    owners.reserved_cnode_slot_total(),
                    limits.max_total_cnode_slots
                );
                report_capacity_breakdown(owners, parent.class, proof);
                crate::yarm_log!("FORK_PROOF_ALLOC_CHILD_FAIL reason={:?} step=reserve", err);
            }
            return Err(err);
        }
    };
    if proof {
        crate::yarm_log!("FORK_PROOF_ALLOC_CHILD_OK child_tid={}", child_tid);
    }

    // ── 3b. CLAIM it, before the first fork-specific mutation — the same one-shot
    //       `ReservedUnstarted -> Spawning` transition a spawn makes, and for the same reason: it
    //       is what authorizes the publication, and it can be consumed exactly once. Every unwind
    //       below restores this incarnation to `ReservedUnstarted` before cancelling it, so the
    //       lifecycle never stays `Spawning` after a returned error.
    let baseline = match owners.claim_reservation(&reservation) {
        Ok(baseline) => baseline,
        Err(refusal) => {
            crate::kernel::spawn_reservation::log_reservation_refusal(
                "fork_process_cow/claim",
                child_tid,
                refusal,
            );
            let _ = cancel_spawn_reservation(owners, reservation);
            return Err(KernelError::WrongObject);
        }
    };

    // ── 4. The address space. ──────────────────────────────────────────────────────────
    if proof {
        crate::yarm_log!("FORK_PROOF_COW_BEGIN");
    }
    let clone_generation = owners.stamp_spawn_generation();
    let clone = match owners.clone_address_space_cow(parent.asid, clone_generation) {
        Ok(clone) => clone,
        Err(err) => {
            if proof {
                crate::yarm_log!("FORK_PROOF_COW_FAIL reason={:?}", err);
                // A clone failure is a capacity failure far more often than not — the child's
                // page tables come from the same PT-frame pool the slab heap draws from — so the
                // same breakdown that explains a refused reservation explains a refused clone.
                report_capacity_breakdown(owners, parent.class, proof);
            }
            unwind(
                owners,
                child_tid,
                reservation,
                baseline,
                None,
                &[],
                "cow_clone",
                err,
            );
            return Err(err);
        }
    };

    // ── 5. The parent's owed TLB work, with NO lock held. ──────────────────────────────
    //
    // Write-protection is a permission downgrade: a CPU still holding a writable translation for
    // one of these pages writes through to the frame the child now shares, and the child observes
    // it. If the remote half cannot be completed, the fork is refused — a silently broken COW is
    // strictly worse than a refused fork, and this is the fail-safe direction §14.4 D3 names.
    if !owners.complete_cow_shootdown(&clone.shootdown) {
        crate::yarm_log!(
            "FORK_TXN_SHOOTDOWN_INCOMPLETE parent_tid={} asid={} runs={} pages={}",
            parent_tid,
            clone.shootdown.asid.0,
            clone.shootdown.runs.len(),
            clone.shootdown.pages()
        );
        unwind(
            owners,
            child_tid,
            reservation,
            baseline,
            Some(&clone),
            &[],
            "shootdown",
            KernelError::WouldBlock,
        );
        return Err(KernelError::WouldBlock);
    }

    // ── 6. Capability inheritance, one delegation at a time. ───────────────────────────
    //
    // The allow/refuse policy is the existing exhaustive one and lives in the owner; what is new
    // is that each grant is a `DelegationGrant` token, so the inverse is `release_delegation` —
    // which refuses to remove a slot that no longer holds the exact object the token names. The
    // previous unwind used a direct in-cspace revoke, which had no such check.
    let inheritable = match owners.snapshot_inheritable_caps(parent_tid) {
        Ok(caps) => caps,
        Err(err) => {
            unwind(
                owners,
                child_tid,
                reservation,
                baseline,
                Some(&clone),
                &[],
                "snapshot_caps",
                err,
            );
            return Err(err);
        }
    };
    let mut grants = alloc::vec::Vec::new();
    for (cap, rights) in inheritable {
        match owners.delegate_capability(parent_tid, cap, child_tid, rights) {
            Ok(grant) => grants.push(grant),
            Err(err) => {
                unwind(
                    owners,
                    child_tid,
                    reservation,
                    baseline,
                    Some(&clone),
                    &grants,
                    "inherit_caps",
                    err,
                );
                return Err(err);
            }
        }
    }

    // ── 7. THE PUBLICATION. The child's whole state and the Reserved → LiveSpawned commit,
    //       under one acquisition. Before this the child is not observable; after it, it is
    //       runnable but not yet queued.
    let publication = ForkChildPublication {
        tid: child_tid,
        asid: clone.child_asid,
        tls_ptr: parent.tls_ptr,
        user_entry: parent.user_entry,
        user_stack_top: parent.user_stack_top,
        user_context: fork_child_context(&parent_context.unwrap_or(parent.user_context)),
        brk_bounds: parent.brk_bounds,
    };
    if let Err(refusal) = owners.publish_forked_child(&reservation, &publication) {
        crate::kernel::spawn_reservation::log_reservation_refusal(
            "fork_process_cow/publish",
            child_tid,
            refusal,
        );
        unwind(
            owners,
            child_tid,
            reservation,
            baseline,
            Some(&clone),
            &grants,
            "publish",
            KernelError::WrongObject,
        );
        return Err(KernelError::WrongObject);
    }

    if proof {
        crate::yarm_log!(
            "FORK_PROOF_CHILD_RET_SET child_tid={} ret0=0 user_gpr0={} arg0={} err=0",
            child_tid,
            publication.user_context.user_gprs[0],
            publication.user_context.arg0
        );
        crate::yarm_log!(
            "FORK_PROOF_PARENT_RET_SET parent_tid={} child_tid={} ret0={} err=0",
            parent_tid,
            child_tid,
            child_tid
        );
        crate::yarm_log!(
            "FORK_PROOF_CHILD_FRAME_BEFORE_ENQUEUE tid={} rip=0x{:x} rsp=0x{:x} rax={} ret0=0 err=0",
            child_tid,
            publication.user_context.instruction_ptr.0,
            publication.user_context.stack_ptr.0,
            publication.user_context.user_gprs[0]
        );
        crate::yarm_log!("FORK_PROOF_CHILD_ENQUEUE_BEGIN child_tid={}", child_tid);
    }

    // ── 8. The enqueue, rank 1 and last. ───────────────────────────────────────────────
    //
    // On the CURRENT CPU deliberately: a balanced placement puts the child on the least-loaded
    // CPU, and when the parent then blocks, the local CPU idles and nothing wakes the child on the
    // remote one. Same CPU as the parent means the same run queue and the next dispatch picks it
    // up. This is the one arm whose inverse must undo a PUBLISHED task, which is why the
    // publication records what it needs to be reversible.
    let cpu = owners.current_cpu();
    if let Err(err) = owners.enqueue_on_cpu(cpu, child_tid) {
        if proof {
            crate::yarm_log!("FORK_PROOF_CHILD_ENQUEUE_FAIL reason={:?}", err);
        }
        unwind_published(owners, child_tid, &clone, &grants, err);
        return Err(err);
    }

    if proof {
        crate::yarm_log!(
            "FORK_PROOF_CHILD_ENQUEUE_OK child_tid={} cpu={} reason={}",
            child_tid,
            cpu.0,
            "parent_cpu"
        );
    }
    crate::yarm_log!(
        "FORK_TXN_COMMITTED parent_tid={} child_tid={} parent_asid={} child_asid={} cpu={} inherited_caps={} wp_runs={}",
        parent_tid,
        child_tid,
        parent.asid.0,
        clone.child_asid.0,
        cpu.0,
        grants.len(),
        clone.wp.len()
    );
    Ok(child_tid)
}

/// The PT-pool and per-owner CNode breakdown behind a capacity-shaped fork failure.
///
/// It lives here rather than in `handle_fork` because `handle_fork` is the BROAD entry, and once
/// NR 12 routes through the split dispatcher no production fork reaches it — a diagnostic left
/// there would be source-present and permanently unreachable. `reserved_cnode_slots` well under
/// `max_total_cnode_slots` means the failure is the slab heap behind the PT pool, not the slot
/// budget; `pt_pool_free_frames` near zero confirms it.
fn report_capacity_breakdown<O: SpawnTxnOwners>(owners: &mut O, class: TaskClass, proof: bool) {
    // Gated HERE as well as at every call site. The gate is what makes "no FORK_PROOF_ line ever
    // reaches a normal boot" checkable lexically, and a helper that relied on its callers being
    // gated would be the one hole in that.
    if proof {
        let limits = owners.capacity_limits();
        let child_requested = match class {
            TaskClass::Driver => limits.driver_cnode_slot_capacity,
            _ => limits.default_cnode_slot_capacity,
        };
        let breakdown = owners.cnode_capacity_breakdown();
        crate::yarm_log!(
            "FORK_PROOF_ALLOC_CHILD_POOL child_class={:?} child_requested_slots={} pt_pool_free_frames={} live_cnodes={}",
            class,
            child_requested,
            crate::kernel::frame_allocator::pt_pool_free_frames(),
            breakdown.len()
        );
        crate::yarm_log!(
            "FORK_PROOF_ALLOC_CHILD_CAPACITY step=capacity live_tasks={} max_tasks={} reserved_cnode_slots={} max_total_cnode_slots={}",
            owners.live_task_count(),
            limits.max_tasks,
            owners.reserved_cnode_slot_total(),
            limits.max_total_cnode_slots
        );
        for (id, reserved, occupied) in breakdown {
            crate::yarm_log!(
                "FORK_PROOF_ALLOC_CHILD_CNODE_OWNER id={} reserved={} occupied={}",
                id,
                reserved,
                occupied
            );
        }
    }
}

/// The reverse compensation for a failure BEFORE the publication.
///
/// Exact reverse order of the forward steps that completed:
/// delegations (newest first) → the address-space clone, which restores every parent permission
/// this attempt changed and tears the never-resident child down → the parent's restoration
/// shootdown, owed once the VM and memory locks are released → the reservation, which releases
/// the kernel context, the TCB slot, the class entry, the CNode grant and the TID.
fn unwind<O: SpawnTxnOwners>(
    owners: &mut O,
    child_tid: u64,
    reservation: SpawnReservationToken,
    baseline: crate::kernel::spawn_reservation::SpawnBaseline,
    clone: Option<&CowCloneToken>,
    grants: &[crate::kernel::boot::spawn_ipc_cap_txn::DelegationGrant],
    phase: &'static str,
    err: KernelError,
) {
    let mut released = 0usize;
    for grant in grants.iter().rev() {
        if owners.release_delegation(grant) {
            released += 1;
        }
    }
    let rollback = clone.map(|clone| {
        let outcome = owners.rollback_cow_clone(clone);
        // The restoration is owed whether or not the rollback restored anything: if it did, the
        // upgraded permissions need invalidating; if it refused, nothing was changed and the plan
        // is empty. Both are stated by the same call.
        if matches!(outcome, CowCloneRollback::Restored { .. }) {
            let _ = owners.complete_cow_shootdown(&clone.restoration_shootdown());
        }
        outcome
    });
    // Restore the SAME incarnation to `ReservedUnstarted` before cancelling it: the cancellation
    // validates that phase, and leaving a reservation `Spawning` after a returned error is the
    // stuck state the spawn lifecycle exists to prevent.
    let restored = owners.restore_after_failed_spawn(&reservation, baseline);
    let cancelled = cancel_spawn_reservation(owners, reservation).is_ok();
    crate::yarm_log!(
        "FORK_TXN_UNWOUND child_tid={} phase={} err={:?} delegations_released={} clone_rollback={:?} reservation_restored={} reservation_cancelled={}",
        child_tid,
        phase,
        err,
        released,
        rollback,
        u8::from(restored),
        u8::from(cancelled)
    );
}

/// The reverse compensation for the ONE failure after the publication: the enqueue.
///
/// The reservation is spent — the task is live — so `cancel_spawn_reservation` cannot be used and
/// must not be: it validates `ReservedUnstarted` and would refuse. The published child is removed
/// through the same owner an ordinary failed registration uses, and only then is its address space
/// destroyed. That order matters: destroying the ASID first is exactly what the previous
/// implementation did, and it left a runnable TCB naming a freed address space.
fn unwind_published<O: SpawnTxnOwners>(
    owners: &mut O,
    child_tid: u64,
    clone: &CowCloneToken,
    grants: &[crate::kernel::boot::spawn_ipc_cap_txn::DelegationGrant],
    err: KernelError,
) {
    // 1. The task, before anything it names is torn down.
    let removed = owners.remove_published_fork_child(child_tid);
    // 2. Its delegated capabilities, newest first.
    let mut released = 0usize;
    for grant in grants.iter().rev() {
        if owners.release_delegation(grant) {
            released += 1;
        }
    }
    // 3. The address space, and the parent's restored permissions.
    let rollback = owners.rollback_cow_clone(clone);
    if matches!(rollback, CowCloneRollback::Restored { .. }) {
        let _ = owners.complete_cow_shootdown(&clone.restoration_shootdown());
    }
    // 4. The process CNode, once nothing holds it.
    let cnode_reaped = owners.reap_process_cnode_if_unused(child_tid);
    crate::yarm_log!(
        "FORK_TXN_UNWOUND_PUBLISHED child_tid={} phase=enqueue err={:?} task_removed={} delegations_released={} clone_rollback={:?} cnode_reaped={}",
        child_tid,
        err,
        u8::from(removed),
        released,
        rollback,
        u8::from(cnode_reaped)
    );
}

/// The capability-inheritance policy, unchanged and stated once.
///
/// Ordinary userspace IPC and memory-object capabilities are inherited; privileged and global
/// classes are not. Exhaustive over `CapObject` by construction, so a new object kind cannot
/// silently default into either answer.
pub(crate) fn fork_should_inherit_capability(
    object: crate::kernel::capabilities::CapObject,
) -> bool {
    use crate::kernel::capabilities::CapObject;
    match object {
        CapObject::Endpoint { .. }
        | CapObject::Notification { .. }
        | CapObject::Reply { .. }
        | CapObject::MemoryObject { .. } => true,
        CapObject::Kernel
        | CapObject::Irq { .. }
        | CapObject::IovaSpace { .. }
        | CapObject::DmaRegion { .. }
        | CapObject::AddressSpace { .. } => false,
    }
}

/// The inheritable set, as the transaction consumes it.
pub(crate) type InheritableCaps = alloc::vec::Vec<(CapId, CapRights)>;
