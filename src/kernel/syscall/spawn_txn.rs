// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-SPAWN-TXN2 §2 — **THE** image-spawn transaction policy, stated once, over an explicit
//! owner interface.
//!
//! # Why this exists
//!
//! Before this module the spawn policy could only be executed by a caller holding
//! `&mut KernelState` — the broad lock. Routing NR 23 / NR 29 off the terminal dispatchers needs
//! the same policy executed by a caller holding no broad lock at all, and there are only two ways
//! to get that: (a) transcribe the policy into a second split implementation, or (b) make the
//! policy generic over *how* it reaches each domain. (a) is what U9 has been undoing everywhere
//! else — six transcriptions of the enqueue policy had already drifted, and one of them had
//! silently dropped the spawn-reservation refusal. So this is (b).
//!
//! [`SpawnTxnOwners`] names every domain operation the transaction performs, and nothing else.
//! Each method is ONE acquisition of ONE domain, wrapping a rank-local body that already exists
//! and that the broad path already delegated to. The phase order, every validation, the whole
//! rollback ledger and the publication point live HERE, in generic code neither adapter can
//! restate.
//!
//! # What the interface is deliberately not
//!
//! The trait exposes no `&mut KernelState`, no lock guard, and no closure taking domain storage.
//! An implementation is free to acquire domains *sequentially* — that is the whole point — but
//! the interface gives it no way to hand two domains to the policy at once, so the policy cannot
//! be written in a way that depends on holding two locks together. What carries consistency
//! across the gaps is not a lock: it is the exact reservation token, the generation-bearing
//! endpoint and delegation grants, and the revalidation each rank-local body performs before it
//! mutates.
//!
//! # The order, and where the child becomes real
//!
//! ```text
//!   plan/snapshot         caller identity + capacity, before any mutation
//!   reserve               the child exists but is `Reserved`: unreachable
//!   VM / image / stack    an address space no TCB names yet
//!   process CNode         the child's cspace association
//!   endpoint / caps       the service endpoint and its delegations
//!   context               user context installed on a still-`Spawning` TCB
//!   PUBLICATION           `Spawning -> LiveSpawned` — the child becomes real HERE
//!   enqueue               rank 1, last, the only step that touches a run queue
//! ```
//!
//! Nothing before the publication is reachable: `refuse_reservation_locked` keeps a `Reserved`
//! TID out of every run queue, and the ASID belongs to no TCB until the bind. Every failure
//! before the publication compensates exactly once, in reverse ownership order. After the
//! publication there is no fallback and nothing left that can fail.

use crate::kernel::boot::exec_state::SpawnedImagePublication;
use crate::kernel::boot::process_cnode_txn::{ProcessCNodeGrant, ProcessCNodeRequest};
use crate::kernel::boot::provisional_cap::{
    MAX_PROVISIONAL_DESCENDANTS, ProvisionalCap, ProvisionalCapRelease,
};
use crate::kernel::boot::spawn_image_provision::{
    ImageProvision, ImageProvisionRequest, ImageSource, ProvisionToken,
};
use crate::kernel::boot::spawn_ipc_cap_txn::{
    DelegationGrant, EndpointRemoval, ServiceEndpointGrant, ServiceEndpointRequest,
};
use crate::kernel::boot::{KernelError, RuntimeCapacityConfig, SpawnedUserTask, UserImageSpec};
use crate::kernel::boot::defs::DelegatedCapabilityLink;
use crate::kernel::capabilities::{CNodeId, CapId, CapObject, CapRights, Capability};
use crate::kernel::scheduler::CpuId;
use crate::kernel::spawn_reservation::{ReservationRefusal, SpawnBaseline, SpawnReservationToken};
use crate::kernel::task::{TaskClass, TaskStatus};
use crate::kernel::vm::{Asid, VirtAddr};

/// Every domain operation the spawn transaction performs.
///
/// Each method is one acquisition of one domain. None of them contains phase order, validation
/// sequencing, rollback policy or a publication decision — all of that is in this module's
/// generic functions, so the broad and split adapters cannot disagree about any of it.
pub(crate) trait SpawnTxnOwners {
    // ── Snapshot. Read-only, taken before any mutation. ─────────────────────────────────
    /// The CPU this transaction is running on.
    fn current_cpu(&self) -> CpuId;
    /// The spawning task, or `None` when there is no current task (bootstrap).
    fn current_tid(&self) -> Option<u64>;
    /// The spawning task's cspace, or `None` when it has none.
    fn current_task_cnode(&self) -> Option<CNodeId>;
    /// The runtime capacity profile every growth limit is derived from.
    fn capacity_limits(&self) -> RuntimeCapacityConfig;

    // ── Task domain (rank 2). ───────────────────────────────────────────────────────────
    /// `None` means the TID names nothing at all — reserved, spawning and live all return
    /// `Some`, which is what makes the reservation refusal total.
    fn task_status(&self, tid: u64) -> Option<TaskStatus>;
    /// How many TCB slots are occupied, for the capacity refusal.
    fn live_task_count(&self) -> usize;
    /// Advance the monotonic TID cursor. Deliberately never rewound: a TID is not reused, so a
    /// failed spawn has nothing to give back.
    fn allocate_thread_id(&mut self) -> Result<u64, KernelError>;
    /// Issue the next spawn-reservation generation, under the lock that also serializes the TCB
    /// insert it stamps.
    fn stamp_spawn_generation(&mut self) -> u64;
    /// Insert the reserved TCB and its class entry together. `None` means the table is full.
    fn insert_reservation(
        &mut self,
        tid: u64,
        reservation: crate::kernel::task::SpawnReservation,
    ) -> Option<usize>;
    /// Clear a reserved slot and its class entry together — the exact inverse of the insert.
    fn clear_reservation_at(&mut self, index: usize);
    /// Derive and install the child's kernel stack and switch frame. Validates the whole
    /// derivation before its first write, so a refusal leaves no partially initialized TCB.
    fn provision_default_kernel_context(&mut self, tid: u64) -> Result<(), KernelError>;
    /// Release the kernel context a reservation owns, before its slot goes away.
    fn release_kernel_context(&mut self, tid: u64);
    /// Validate a cancellable reservation. Read-only, so every refusal leaves the reservation —
    /// or the replacement a stale token failed to name — byte-for-byte unchanged.
    fn validate_cancellable(
        &mut self,
        token: &SpawnReservationToken,
    ) -> Result<(usize, u64), ReservationRefusal>;
    /// Claim `ReservedUnstarted -> Spawning`, returning the pre-claim baseline to restore on
    /// failure. Mutates nothing on any refusal.
    fn claim_reservation(
        &mut self,
        token: &SpawnReservationToken,
    ) -> Result<SpawnBaseline, ReservationRefusal>;
    /// Restore the SAME incarnation after a failed spawn, keeping its generation. Every field the
    /// spawn body may have written goes back to its exact pre-claim value.
    fn restore_after_failed_spawn(
        &mut self,
        token: &SpawnReservationToken,
        baseline: SpawnBaseline,
    ) -> bool;
    /// Bind the child's address space to its TCB. This is what makes the ASID attributable, and
    /// therefore what the ledger's address-space arm inverts.
    fn bind_spawned_task_asid(&mut self, tid: u64, asid: Asid) -> Result<(), KernelError>;
    /// Install the whole user context and commit `Spawning -> LiveSpawned`, under ONE
    /// acquisition. THE publication point.
    fn publish_spawned_image(
        &mut self,
        reservation: &SpawnReservationToken,
        publication: &SpawnedImagePublication,
    ) -> Result<(), ReservationRefusal>;
    /// How many live TCBs carry this TID, for the Stage 175 post-spawn invariant.
    fn live_tcb_count_for(&self, tid: u64) -> usize;
    /// Whether this TID's TCB is `Exited`/`Dead`, for the Stage 175 zombie check.
    fn is_zombie(&self, tid: u64) -> bool;
    /// The child's kernel switch-frame stack top, for the x86_64 bootstrap markers.
    fn thread_kernel_stack_top(&self, tid: u64) -> u64;
    /// Whether the child's kernel context has been initialized, for the x86_64 retry gate.
    fn thread_kernel_context_initialized(&self, tid: u64) -> bool;
    /// x86_64 bootstrap only: publish an initialized kernel switch frame.
    fn initialize_thread_kernel_switch_frame(
        &mut self,
        tid: u64,
        entry: usize,
    ) -> Result<(), KernelError>;

    // ── Scheduler (rank 1), always last. ────────────────────────────────────────────────
    /// Enqueue where told. Used for the bootstrap-CPU pin.
    fn enqueue_on_cpu(&mut self, cpu: CpuId, tid: u64) -> Result<CpuId, KernelError>;
    /// Enqueue by the balanced policy, honouring driver affinity.
    fn enqueue_balanced(&mut self, tid: u64) -> Result<CpuId, KernelError>;

    // ── Capability domain (rank 4). ─────────────────────────────────────────────────────
    /// Provision the process cspace and its PID association as ONE transaction, so a failure of
    /// the second cannot leave the first behind.
    fn provision_process_cnode(
        &mut self,
        request: &ProcessCNodeRequest,
    ) -> Result<ProcessCNodeGrant, KernelError>;
    /// Release exactly what [`Self::provision_process_cnode`] created.
    fn release_process_cnode_grant(
        &mut self,
        request: &ProcessCNodeRequest,
        grant: &ProcessCNodeGrant,
    );
    /// The no-alloc process-CNode reap, which only proceeds when no other thread owns the process.
    fn reap_process_cnode_if_unused(&mut self, pid: u64) -> bool;
    /// The cspace a task owns.
    fn task_cnode(&self, tid: u64) -> Option<CNodeId>;
    /// Associate a PID with a cspace.
    fn set_process_cnode_for_pid(&mut self, pid: u64, cnode: CNodeId) -> Result<(), KernelError>;
    /// Mint one capability into a named cspace.
    fn mint_capability_in_cnode(
        &mut self,
        cnode: CNodeId,
        capability: Capability,
    ) -> Result<CapId, KernelError>;
    /// Build the exact identity token for a capability this spawn minted. Capability rank 4.
    fn provisional_cap_token(
        &mut self,
        cnode: CNodeId,
        cap: CapId,
    ) -> Result<ProvisionalCap, KernelError>;
    /// Phase A of the rollback: the numeric-cap link closure, read at capability rank 4.
    /// `None` means it is wider than the bound and the rollback must refuse.
    fn collect_cap_link_closure(&mut self, cap: CapId) -> Option<(LinkClosure, usize)>;
    /// Phase B: resolve each candidate link's `source_tid` / `dest_tid` to its process, at task
    /// rank 2 — between the two capability acquisitions, never inside one.
    fn resolve_link_pids(&self, links: &LinkClosure) -> ResolvedLinkPids;
    /// Phase C: remove the delegated children, then the source slot. Capability rank 4.
    fn release_provisional_cap(
        &mut self,
        token: &ProvisionalCap,
        links: &LinkClosure,
        resolved: &ResolvedLinkPids,
    ) -> ProvisionalCapRelease;
    /// Delegate an attenuated copy, returning the exact rollback token. Refuses if the source
    /// slot no longer holds the object the caller resolved, so a recycled slot cannot be
    /// delegated by mistake.
    fn delegate_capability(
        &mut self,
        source_tid: u64,
        source_cap: CapId,
        dest_tid: u64,
        rights: CapRights,
    ) -> Result<DelegationGrant, KernelError>;
    /// Undo one delegation, refusing if the destination slot no longer holds the exact object the
    /// token names.
    fn release_delegation(&mut self, grant: &DelegationGrant) -> bool;

    // ── IPC (rank 3) then capability (rank 4). ──────────────────────────────────────────
    /// Create one service endpoint and mint both its capabilities as ONE transaction, so a mint
    /// failure cannot leave an endpoint installed that nothing names.
    fn provision_service_endpoint(
        &mut self,
        request: &ServiceEndpointRequest,
    ) -> Result<ServiceEndpointGrant, KernelError>;
    /// Remove the endpoint INCARNATION, and only if it is still the same unpublished one the
    /// generation names. IPC rank 3. The capabilities naming it are removed separately, by the
    /// provisional-cap rollback, because that is where their delegation graph lives.
    fn remove_unpublished_endpoint(&mut self, index: usize, generation: u64) -> EndpointRemoval;

    // ── VM (rank 5) then memory (rank 6). ───────────────────────────────────────────────
    /// Create the address space, load the image, map NR 23's initrd window and allocate the user
    /// stack — as one owning phase with one exact rollback.
    fn provision_image(
        &mut self,
        request: &ImageProvisionRequest<'_>,
    ) -> Result<ProvisionToken, KernelError>;
    /// Roll one provisioning back to nothing. Exact because the ASID is never CPU-resident.
    fn rollback_provision(&mut self, asid: Asid, phase: &'static str, err: KernelError);
    /// Whether an address space exists.
    fn address_space_exists(&self, asid: Asid) -> bool;
    /// Tear down an address space no TCB names. Owes no shootdown, and must NOT consume a
    /// retired-ASID slot.
    fn destroy_unresident_address_space(&mut self, asid: Asid) -> bool;
    /// Tear down a published address space, with the full live teardown it owes.
    fn destroy_live_address_space(&mut self, asid: Asid) -> bool;
    /// The TID whose TCB names this ASID, if any. The ledger's address-space arm asks this to
    /// choose between the two teardowns above.
    fn asid_carrier_tid(&self, asid: Asid) -> Option<u64>;
    /// Allocate and map a user stack. Only the bring-up callers reach this; a transaction that
    /// provisioned its image already owns one.
    fn allocate_user_stack_with_guard(
        &mut self,
        tid: u64,
        pages: usize,
    ) -> Result<VirtAddr, KernelError>;
    /// Copy bytes into a user address space.
    fn copy_to_user(&mut self, asid: Asid, va: VirtAddr, bytes: &[u8]) -> Result<(), KernelError>;
}

const BOOTSTRAP_FIRST_USER_TID: u64 = 1;
const BOOTSTRAP_SUPERVISOR_TID: u64 = 2;
const DEBUG_DISPATCH_CONTEXT_LOG: bool = false;

fn task_missing_with_site(site: &'static str, cpu: u8) -> KernelError {
    if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
        crate::yarm_log!("TASK_MISSING site={} cpu={}", site, cpu);
    }
    KernelError::TaskMissing
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// Phase 1: the reservation. The child exists, and is unreachable.
// ─────────────────────────────────────────────────────────────────────────────────────────

/// Reserve `tid` for a future spawn, in `process_pid`.
///
/// This is the counterpart to ordinary registration, NOT a variant of it. Registration creates an
/// already-live task, which is the right thing when that is genuinely what the caller means; this
/// creates a **non-live reservation** that only [`spawn_user_task_from_image`] can turn into a
/// live task, and only once.
///
/// It reuses the same capacity, CNode-slot, class and kernel-context provisioning as ordinary
/// registration — that policy is not duplicated here — so a reservation owns exactly the pre-spawn
/// resources bootstrap needs: CNode/process association, class, and the kernel stack/kernel
/// context. What it does NOT own is liveness: the TCB is `TaskStatus::Reserved`, so it cannot be
/// dispatched, enqueued, woken, blocked, joined, or published as an endpoint waiter.
///
/// Refuses without mutation if `tid` is already anything at all — reserved, spawning or live.
/// Registration's idempotence is deliberately NOT inherited: it is precisely what allowed a spawn
/// to overwrite an existing task.
///
/// Compensation is exact and in reverse order: a full task table releases the CNode grant; a
/// kernel-context failure clears the slot *and* releases the grant.
pub(crate) fn reserve_task_for_spawn<O: SpawnTxnOwners>(
    owners: &mut O,
    tid: u64,
    class: TaskClass,
    process_pid: u64,
) -> Result<SpawnReservationToken, KernelError> {
    if let Some(observed) = owners.task_status(tid) {
        crate::yarm_log!(
            "SPAWN_RESERVE_REFUSED tid={} reason=tid_occupied observed={:?}",
            tid,
            observed
        );
        return Err(KernelError::TaskTableFull);
    }
    let limits = owners.capacity_limits();
    if owners.live_task_count() >= limits.max_tasks {
        return Err(KernelError::TaskTableFull);
    }
    // The CNode space and its PID association are ONE transaction, taken through the shared
    // owner. They used to be two independent rank-4 entries, and a failure of the second left the
    // first behind: a capability space provisioned for a process that did not exist.
    //
    // The PID is passed explicitly and comes from this reservation's own argument — there is no
    // ambient current-task fallback here, on either path.
    let cnode_request = ProcessCNodeRequest {
        pid: process_pid,
        tid,
        class,
    };
    let cnode_grant = owners.provision_process_cnode(&cnode_request)?;
    // Monotonic, never derived from the TID: a token for an earlier occupant of this numeric TID
    // cannot match this reservation. Taken under the task lock, which is what makes "no two
    // reservations share a value" enforceable at all.
    let generation = owners.stamp_spawn_generation();
    let reservation = crate::kernel::task::SpawnReservation {
        generation,
        class,
        process_pid,
        phase: crate::kernel::task::SpawnPhase::ReservedUnstarted,
    };
    // The TCB slot and its class entry go in under ONE task-domain acquisition.
    let Some(inserted_idx) = owners.insert_reservation(tid, reservation) else {
        // The CNode transaction happened; this reservation did not. Release exactly what it
        // created, through the owner that holds it.
        owners.release_process_cnode_grant(&cnode_request, &cnode_grant);
        return Err(KernelError::TaskTableFull);
    };
    if let Err(err) = owners.provision_default_kernel_context(tid) {
        owners.clear_reservation_at(inserted_idx);
        owners.release_process_cnode_grant(&cnode_request, &cnode_grant);
        return Err(err);
    }
    crate::yarm_log!(
        "SPAWN_RESERVE_OK tid={} class={:?} pid={} generation={}",
        tid,
        class,
        process_pid,
        generation
    );
    Ok(crate::kernel::spawn_reservation::mint_reservation(
        tid,
        generation,
        class,
        process_pid,
    ))
}

/// Cancel an unstarted reservation, for when pre-spawn setup fails BEFORE the spawn is invoked.
///
/// Validates the exact TID, generation, class, process and `ReservedUnstarted` phase before
/// touching anything, so a stale token, a token naming a replacement occupant of the same numeric
/// TID, a reservation already claimed by an in-flight spawn, and a live task are all refused with
/// **zero mutation**. On success the reserved TCB is removed and its reservation-owned kernel
/// resources are released through the EXISTING cleanup mechanisms — no second cleanup policy is
/// introduced. After cancellation the token names nothing and can authorize nothing.
pub(crate) fn cancel_spawn_reservation<O: SpawnTxnOwners>(
    owners: &mut O,
    token: SpawnReservationToken,
) -> Result<(), KernelError> {
    let tid = token.tid();
    // Validate first. This is read-only, so every refusal leaves the reservation — or the
    // replacement task the stale token failed to name — byte-for-byte unchanged.
    let (index, process_pid) = owners.validate_cancellable(&token).map_err(|refusal| {
        crate::kernel::spawn_reservation::log_reservation_refusal(
            "cancel_spawn_reservation",
            tid,
            refusal,
        );
        KernelError::WrongObject
    })?;
    // Release the kernel context BEFORE the slot goes away: the existing primitive resolves the
    // task by TID and would find nothing afterwards.
    owners.release_kernel_context(tid);
    // Slot and class cleared together, the exact inverse of the paired insert above.
    owners.clear_reservation_at(index);
    let cnode_reaped = owners.reap_process_cnode_if_unused(process_pid);
    crate::yarm_log!(
        "SPAWN_RESERVATION_CANCELLED tid={} generation={} pid={} cnode_reaped={}",
        tid,
        token.generation(),
        process_pid,
        u8::from(cnode_reaped)
    );
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// Phases 2+3: the child's whole image, and the capability that names its address space.
// ─────────────────────────────────────────────────────────────────────────────────────────

/// Provision the child's address space, image, initrd window and user stack, and mint the
/// capability naming the address space into the CALLER's cspace.
///
/// Two owning phases with one exact rollback between them:
///
/// 1. **VM(5) → memory(6)**, held for the whole provisioning and released before step 2. On
///    failure the body has already rolled itself back to nothing.
/// 2. **capability(4)**, with no VM or memory lock held. On failure the exact token from step 1 is
///    rolled back and the MINT's error is propagated — not the rollback's.
///
/// The caller's cspace is resolved explicitly rather than through an ambient current-task lookup
/// inside the mint, which is what lets the split path state the same identity the broad one does.
pub(crate) fn provision_spawn_image<O: SpawnTxnOwners>(
    owners: &mut O,
    tid: u64,
    image_path: &str,
    source: ImageSource<'_>,
    map_initrd_window: bool,
    startup_args: &mut [u64; 18],
    stack_pages: usize,
) -> Result<ImageProvision, KernelError> {
    let request = ImageProvisionRequest {
        tid,
        source,
        map_initrd_window,
        stack_pages,
    };
    // ── 1. VM(5) → memory(6), held for the whole provisioning and released here. ────
    let token = owners.provision_image(&request)?;

    // ── 2. The capability naming the address space, minted with no VM/memory held. ──
    let aspace_cap = match owners
        .current_task_cnode()
        .ok_or(KernelError::TaskMissing)
        .and_then(|cnode| {
            owners.mint_capability_in_cnode(
                cnode,
                Capability::new(
                    CapObject::AddressSpace { asid: token.asid.0 },
                    CapRights::MAP | CapRights::READ | CapRights::WRITE,
                ),
            )
        }) {
        Ok(cap) => cap,
        // ── 3. Reacquire and roll the exact token back; propagate the MINT's error. ─
        Err(err) => {
            owners.rollback_provision(token.asid, "aspace_cap_mint", err);
            return Err(err);
        }
    };

    if let Some((user_ptr, len)) = token.initrd_user_window {
        startup_args[15] = user_ptr;
        startup_args[16] = len;
    }
    crate::yarm_log!(
        "SPAWN_IMAGE_PROVISIONED tid={} path={} asid={} entry=0x{:x} stack_top=0x{:x} \
         first_vaddr=0x{:x} end=0x{:x} zc_pages={} copied_pages={}",
        tid,
        image_path,
        token.asid.0,
        token.entry,
        token.stack_top.0,
        token.first_vaddr,
        token.last_vaddr_end,
        token.zc_pages,
        token.copied_pages
    );
    Ok(ImageProvision {
        asid: token.asid,
        aspace_cap,
        entry: token.entry,
        stack_top: token.stack_top,
        zc_pages: token.zc_pages,
        copied_pages: token.copied_pages,
    })
}

/// The bounded numeric-cap link closure phase A produces.
pub(crate) type LinkClosure = [Option<DelegatedCapabilityLink>; MAX_PROVISIONAL_DESCENDANTS];
/// Each candidate link's `(source_pid, dest_pid)`, resolved at task rank 2.
pub(crate) type ResolvedLinkPids = [Option<(u64, u64)>; MAX_PROVISIONAL_DESCENDANTS];

/// THE provisional-capability rollback, composed once for both adapters.
///
/// Four steps, three acquisitions, none held across another:
///
/// ```text
///   cap 4   token: the exact cnode/pid/CapId+generation/object incarnation
///   cap 4   phase A: the numeric-cap link closure (a superset; needs no pid)
///   task 2  phase B: resolve each candidate link's tids to processes
///   cap 4   phase C: remove delegated children, then the source slot
/// ```
///
/// Phase B sits BETWEEN two capability acquisitions rather than inside one, which is what keeps
/// rank 4 and rank 2 from ever being held together. Phase A deliberately over-collects so that
/// the only thing phase B has to do is interpret tids — phase C then narrows the superset with
/// the resolved pids, and treats an unresolvable tid as a non-match (fail closed).
///
/// Performs NO object destruction. The endpoint incarnation and the address space are the
/// ledger's, held as their own generation-bearing tokens.
pub(crate) fn release_provisional_capability<O: SpawnTxnOwners>(
    owners: &mut O,
    cnode: CNodeId,
    cap: CapId,
) -> ProvisionalCapRelease {
    let Ok(token) = owners.provisional_cap_token(cnode, cap) else {
        return ProvisionalCapRelease::AlreadyGone;
    };
    let Some((links, _len)) = owners.collect_cap_link_closure(cap) else {
        crate::yarm_log!(
            "SPAWN_PROVCAP_RELEASE cnode={} cap={} outcome=residue reason=closure_too_wide",
            cnode.0,
            cap.0
        );
        return ProvisionalCapRelease::Residue;
    };
    let resolved = owners.resolve_link_pids(&links);
    let outcome = owners.release_provisional_cap(&token, &links, &resolved);
    crate::yarm_log!(
        "SPAWN_PROVCAP_RELEASE cnode={} cap={} object={:?} outcome={}",
        cnode.0,
        cap.0,
        token.object,
        outcome.tag()
    );
    outcome
}

/// Release one whole service-endpoint grant: both capabilities through the provisional-cap
/// rollback, THEN the endpoint incarnation through its own rank-3 owner.
///
/// Capabilities before the object, and each capability's delegated children before it. Removing
/// the endpoint first would leave capabilities naming an object that no longer exists.
pub(crate) fn release_service_endpoint_grant<O: SpawnTxnOwners>(
    owners: &mut O,
    grant: &ServiceEndpointGrant,
) -> EndpointRemoval {
    let send = release_provisional_capability(owners, grant.owner_cnode, grant.send_cap);
    let recv = release_provisional_capability(owners, grant.owner_cnode, grant.recv_cap);
    let removal = owners.remove_unpublished_endpoint(grant.endpoint_index, grant.endpoint_generation);
    crate::yarm_log!(
        "SPAWN_EP_TXN_RELEASED slot={} generation={} send_revoked={} recv_revoked={} endpoint={:?}",
        grant.endpoint_index,
        grant.endpoint_generation,
        u8::from(send.released()),
        u8::from(recv.released()),
        removal
    );
    removal
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// Phase 6: the publication. THE spawn commit, for every caller on every path.
// ─────────────────────────────────────────────────────────────────────────────────────────

/// Spawn a user task by consuming an EXACT one-shot reservation.
///
/// `reservation` is the authority, not `spec.tid`. Before any spawn-specific mutation the
/// reservation is validated and claimed atomically under one rank-2 acquisition — exact TID, exact
/// generation, expected class, expected process, and phase `ReservedUnstarted` — so a stale token,
/// a duplicate token, a wrong class, a wrong process, or a TID whose TCB is live, runnable,
/// blocked, faulted, exited or dead all fail closed BEFORE any side effect.
///
/// The task becomes scheduler-visible only after the typed `Spawning -> LiveSpawned` commit, which
/// is also what clears `TaskStatus::Reserved` — so the enqueue cannot see it any earlier. On an
/// ordinary returned error the SAME reservation incarnation is restored to `ReservedUnstarted`,
/// keeping its generation, so the caller's token remains exactly as valid as it was and the
/// lifecycle never stays `Spawning`.
pub(crate) fn spawn_user_task_from_image<O: SpawnTxnOwners>(
    owners: &mut O,
    reservation: SpawnReservationToken,
    spec: UserImageSpec,
) -> Result<SpawnedUserTask, KernelError> {
    if spec.tid != reservation.tid() {
        crate::yarm_log!(
            "SPAWN_RESERVATION_REFUSED site=spawn_user_task_from_image tid={} reason=spec_tid_mismatch reservation_tid={}",
            spec.tid,
            reservation.tid()
        );
        return Err(KernelError::WrongObject);
    }
    // Claim the reservation FIRST: everything below this line is spawn-specific mutation.
    let baseline = owners.claim_reservation(&reservation).map_err(|refusal| {
        crate::kernel::spawn_reservation::log_reservation_refusal(
            "spawn_user_task_from_image",
            spec.tid,
            refusal,
        );
        KernelError::WrongObject
    })?;
    let tid = spec.tid;
    match commit_spawned_image(owners, &reservation, spec) {
        Ok(spawned) => Ok(spawned),
        Err(err) => {
            // The reservation lifecycle is transactional: restore the SAME incarnation, so it
            // never stays `Spawning` after an ordinary returned error and the caller's token stays
            // exactly as valid as it was.
            let restored = owners.restore_after_failed_spawn(&reservation, baseline);
            crate::yarm_log!(
                "SPAWN_FAILED_RESERVATION_RESTORED tid={} restored={} err={:?}",
                tid,
                u8::from(restored),
                err
            );
            Err(err)
        }
    }
}

/// The spawn body proper, entered only with the reservation already claimed (`Spawning`).
///
/// Every `?` here returns to [`spawn_user_task_from_image`], which restores the reservation.
fn commit_spawned_image<O: SpawnTxnOwners>(
    owners: &mut O,
    reservation: &SpawnReservationToken,
    mut spec: UserImageSpec,
) -> Result<SpawnedUserTask, KernelError> {
    let cpu = owners.current_cpu();
    // The destination is the exact reservation claimed above, which by construction was never a
    // live task. Registration idempotence is not spawn authorization.
    if spec.entry == 0 {
        return Err(KernelError::WrongObject);
    }
    let asid = spec.asid.ok_or(KernelError::UserMemoryFault)?;
    if !owners.address_space_exists(asid) {
        return Err(KernelError::UserMemoryFault);
    }

    crate::yarm_log!(
        "SPAWN_TASK_ENTER tid={} asid={} entry=0x{:x}",
        spec.tid,
        asid.0,
        spec.entry
    );
    if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
        crate::yarm_log!(
            "FIRST_USER_CREATE_BEGIN cpu={} tid={} asid={} entry=0x{:x}",
            cpu.0,
            spec.tid,
            asid.0,
            spec.entry
        );
    }
    // Stage 175 (SPAWN-LIFECYCLE): default-off lifecycle phase markers. Every register / cnode /
    // cap / thread step below is UNCHANGED — these only expose the phase boundaries.
    //
    // Stage 175B: a duplicate-TID is NOT detectable by a pre-register presence scan — the
    // bootstrap tasks legitimately pre-reserve their TCB slot before this spawn runs, so a
    // pre-check flags every bootstrap spawn as a false duplicate. The only true duplicate is a
    // *second live TCB* for the same tid, which the post-spawn invariant below detects.
    let spawn_lc = crate::kernel::boot::spawn_lifecycle_enabled();
    // No registration here. The TCB, CNode, class and kernel context were provisioned by the
    // reservation, and the reservation is already claimed — so this spawn cannot create, and
    // cannot overwrite, anything.
    crate::yarm_log!("SPAWN_TASK_REGISTER_OK tid={}", spec.tid);
    if spawn_lc {
        crate::yarm_log!("SPAWN_LIFECYCLE_TCB_ALLOC_OK tid={}", spec.tid);
        // The address space was created upstream and is bound to this task; a missing address
        // space here would be an aspace-setup violation.
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_ASPACE_CREATE_OK tid={} asid={}",
            spec.tid,
            asid.0
        );
    }

    // Stage 119 Part A: minimal task-pair init — x86_64 only, tid=1 (init server) and tid=2
    // (supervisor). Sets `kernel_context.initialized` for both so the first timer preemption of
    // tid=1 dispatching tid=2 as incoming produces a real `switch_frames` call and the
    // first-resume handler can prove lock reacquisition via `post_switch_restore`.
    #[cfg(target_arch = "x86_64")]
    if spec.tid == BOOTSTRAP_FIRST_USER_TID || spec.tid == BOOTSTRAP_SUPERVISOR_TID {
        let entry = crate::kernel::boot::thread_state::kernel_switch_frame_trampoline_ip();
        crate::yarm_log!("D6_KERNEL_SWITCH_FRAME_INIT_BEGIN tid={}", spec.tid);
        match owners.initialize_thread_kernel_switch_frame(spec.tid, entry) {
            Ok(()) => {
                let stack = owners.thread_kernel_stack_top(spec.tid);
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_FRAME_INIT_DONE tid={} entry=0x{:x} stack=0x{:x}",
                    spec.tid,
                    entry,
                    stack,
                );
            }
            Err(e) => {
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_FRAME_INIT_DEFERRED reason=init_failed tid={} err={:?}",
                    spec.tid,
                    e,
                );
            }
        }
    }

    // The child's startup capabilities. Delegation failure is TOLERATED here exactly as it always
    // has been: the slot stays 0 and the child comes up without that capability. That is a
    // pre-existing policy decision about what a spawn means, not a leak — the caps live in the
    // child's own cspace, which the reservation owns and the ledger releases.
    if spec.spawner_tid != 0 && spec.service_recv_cap != 0 {
        match owners.delegate_capability(
            spec.spawner_tid,
            CapId(spec.service_recv_cap),
            spec.tid,
            CapRights::RECEIVE,
        ) {
            Ok(grant) => {
                spec.startup_args[12] = grant.dest_cap.0;
                crate::yarm_log!(
                    "KSPAWN_RECV_CAP_DELEGATED tid={} local_cap={}",
                    spec.tid,
                    grant.dest_cap.0
                );
            }
            Err(e) => {
                crate::yarm_log!("KSPAWN_RECV_CAP_DELEGATE_FAIL tid={} err={:?}", spec.tid, e);
            }
        }
    }
    if spec.spawner_tid != 0 && spec.service_reply_recv_cap != 0 {
        match owners.delegate_capability(
            spec.spawner_tid,
            CapId(spec.service_reply_recv_cap),
            spec.tid,
            CapRights::RECEIVE,
        ) {
            Ok(grant) => {
                spec.startup_args[2] = grant.dest_cap.0;
                crate::yarm_log!(
                    "SPAWN_SERVICE_REPLY_RECV_CAP_CHILD child_tid={} cap={} rights=RECEIVE",
                    spec.tid,
                    grant.dest_cap.0
                );
                crate::yarm_log!(
                    "SPAWN_STARTUP_SLOT_2_REPLY_RECV child_tid={} value={}",
                    spec.tid,
                    spec.startup_args[2]
                );
            }
            Err(e) => {
                crate::yarm_log!(
                    "KSPAWN_REPLY_RECV_CAP_DELEGATE_FAIL tid={} err={:?}",
                    spec.tid,
                    e
                );
            }
        }
    }
    for i in 0..spec.extra_send_caps.len() {
        let raw_cap = spec.extra_send_caps[i];
        if raw_cap != 0 && spec.spawner_tid != 0 {
            match owners.delegate_capability(
                spec.spawner_tid,
                CapId(raw_cap),
                spec.tid,
                CapRights::SEND,
            ) {
                Ok(grant) => {
                    spec.startup_args[13 + i] = grant.dest_cap.0;
                    crate::yarm_log!(
                        "KSPAWN_EXTRA_CAP_DELEGATED tid={} slot={} local_cap={}",
                        spec.tid,
                        13 + i,
                        grant.dest_cap.0
                    );
                }
                Err(e) => {
                    crate::yarm_log!(
                        "KSPAWN_EXTRA_CAP_DELEGATE_FAIL tid={} slot={} err={:?}",
                        spec.tid,
                        13 + i,
                        e
                    );
                }
            }
        }
    }

    let cnode = owners.task_cnode(spec.tid).ok_or(task_missing_with_site(
        "spawn_user_task_from_image/task_cnode",
        cpu.0,
    ))?;
    crate::yarm_log!(
        "SPAWN_TASK_CAP_CHECK name=task_cnode cap={} object=cnode result=ok",
        cnode.0
    );
    if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
        crate::yarm_log!(
            "FIRST_USER_LOOKUP cpu={} tid={} cnode={} status=found",
            cpu.0,
            spec.tid,
            cnode.0
        );
    }
    owners.set_process_cnode_for_pid(spec.tid, cnode)?;
    if spawn_lc {
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_CNODE_SETUP_OK tid={} cnode={}",
            spec.tid,
            cnode.0
        );
        // The bootstrap/service caps were delegated into this cnode above.
        crate::yarm_log!("SPAWN_LIFECYCLE_BOOTSTRAP_CAPS_OK tid={}", spec.tid);
    }
    owners.bind_spawned_task_asid(spec.tid, asid).map_err(|_| {
        task_missing_with_site("spawn_user_task_from_image/set_asid_tcb_lookup", cpu.0)
    })?;

    // Stage 127: Stage 126 correctly refused to publish x86_64 initialized switch frames without
    // a mapped kernel switch-stack page, but the first attempt above can run before the target
    // task ASID is bound. Retry at the first point where the target ASID/root is known, so the
    // mapping gate uses the target task root rather than temporal active-ASID presence.
    #[cfg(target_arch = "x86_64")]
    if (spec.tid == BOOTSTRAP_FIRST_USER_TID || spec.tid == BOOTSTRAP_SUPERVISOR_TID)
        && !owners.thread_kernel_context_initialized(spec.tid)
    {
        let entry = crate::kernel::boot::thread_state::kernel_switch_frame_trampoline_ip();
        crate::yarm_log!("D6_KERNEL_SWITCH_FRAME_INIT_RETRY tid={}", spec.tid);
        match owners.initialize_thread_kernel_switch_frame(spec.tid, entry) {
            Ok(()) => {
                let stack = owners.thread_kernel_stack_top(spec.tid);
                crate::yarm_log!("D6_KERNEL_SWITCH_FRAME_INIT_RETRY_DONE tid={}", spec.tid);
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_FRAME_INIT_DONE tid={} entry=0x{:x} stack=0x{:x}",
                    spec.tid,
                    entry,
                    stack,
                );
            }
            Err(e) => {
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_FRAME_INIT_DEFERRED reason=retry_failed tid={} err={:?}",
                    spec.tid,
                    e,
                );
            }
        }
    }
    if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
        crate::yarm_log!("BOOTSTRAP_STAGE: before stack allocation");
    }
    // U9-SPAWN-VM1: a caller that provisioned the image as one transaction already allocated and
    // mapped this stack in `asid`, BEFORE the commit, which is what makes a stack-allocation
    // failure rollable-back. Allocating again here would install a second stack at the same slot —
    // the overlap check inside the allocator would refuse it — so the provisioned one is consumed,
    // not re-derived. `None` is every caller that did not provision one (the nine architecture
    // bring-up sites), and they allocate exactly as before.
    let stack_top = match spec.provisioned_stack_top {
        Some(top) => top,
        None => match owners.allocate_user_stack_with_guard(spec.tid, 64) {
            Ok(top) => top,
            Err(err) => {
                crate::yarm_log!(
                    "SPAWN_TASK_STACK_FAIL tid={} asid={} err={:?}",
                    spec.tid,
                    asid.0,
                    err
                );
                if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                    crate::yarm_log!("BOOTSTRAP_ERROR: {:?}", err);
                }
                return Err(err);
            }
        },
    };
    crate::yarm_log!(
        "SPAWN_TASK_STACK_OK tid={} stack_top=0x{:x}",
        spec.tid,
        stack_top.0
    );
    if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
        crate::yarm_log!("BOOTSTRAP_STAGE: after stack allocation");
        crate::yarm_log!("BOOTSTRAP_STAGE: before entry setup");
        crate::yarm_log!("USER_ENTRY rip=0x{:x}", spec.entry);
    }
    let startup_slots_len = spec.startup_args.len();
    let startup_slots_bytes_len = startup_slots_len * core::mem::size_of::<u64>();
    let startup_slots_start =
        (stack_top.0 as usize).saturating_sub(startup_slots_bytes_len) & !0x7usize;
    let startup_stack_ptr = startup_slots_start & !0xFusize;
    // x86-64 SysV ABI: at the very first instruction of a function the stack pointer must satisfy
    // RSP ≡ 8 (mod 16), because a CALL would normally have pushed an 8-byte return address. We
    // enter user tasks via IRETQ / SYSRETQ (no return-address push), so we pre-subtract 8 here.
    // The trap return path and the initial-IRETQ path both read the stack pointer directly from
    // `user_context`, so the adjustment only needs to appear here.
    // AArch64 requires 16-byte SP alignment at function entry — no pre-subtraction is needed.
    #[cfg(target_arch = "x86_64")]
    let startup_stack_ptr = startup_stack_ptr.wrapping_sub(8);
    #[allow(unused_variables)]
    #[cfg(not(target_arch = "x86_64"))]
    let startup_stack_ptr = startup_stack_ptr;
    // Ensure slot[0] (task_id) is always the actual allocated TID. PM does not know the new task's
    // TID at SpawnV5 call time and sends `startup_args[0] = 0`. Filling it now makes (a) the
    // user-visible slot[0] hold the correct task_id, and (b) `user_context.arg0 = spec.tid != 0`,
    // which satisfies the x86_64 new-task detection check so the startup ABI registers are
    // properly injected on the task's very first dispatch.
    if spec.startup_args[0] == 0 {
        spec.startup_args[0] = spec.tid;
    }
    let startup_slots_ptr = VirtAddr(startup_slots_start as u64);
    let mut startup_slots_bytes = [0u8; core::mem::size_of::<u64>() * 18];
    for (index, slot) in spec.startup_args.iter().copied().enumerate() {
        let begin = index * core::mem::size_of::<u64>();
        startup_slots_bytes[begin..begin + core::mem::size_of::<u64>()]
            .copy_from_slice(&slot.to_le_bytes());
    }
    owners.copy_to_user(
        asid,
        startup_slots_ptr,
        &startup_slots_bytes[..startup_slots_bytes_len],
    )?;
    crate::yarm_log!(
        "YARM_FIRST_USER_STARTUP_BLOCK va=0x{:x} count={} mapped=true",
        startup_slots_start,
        startup_slots_len
    );

    // ── THE PUBLICATION. The whole user context AND the one-shot `Spawning -> LiveSpawned`
    //    commit, under ONE task-lock acquisition.
    //
    // These were two separate acquisitions. Between them a TCB could be observed with a fully
    // installed user context and a still-`Spawning` reservation, and a commit refusal left exactly
    // that: a published context on a task that never became runnable, which nothing removed. The
    // owner validates the reservation BEFORE it writes anything, so a refusal costs no partial
    // publication and the commit that follows cannot fail.
    owners
        .publish_spawned_image(
            reservation,
            &SpawnedImagePublication {
                tid: spec.tid,
                class: spec.class,
                asid,
                entry: spec.entry,
                stack_top,
                startup_stack_ptr,
                startup_slots_start,
                startup_slots_len,
                startup_args: spec.startup_args,
            },
        )
        .map_err(|refusal| {
            crate::kernel::spawn_reservation::log_reservation_refusal(
                "spawn_user_task_from_image/publish",
                spec.tid,
                refusal,
            );
            KernelError::WrongObject
        })?;
    crate::yarm_log!("SPAWN_TASK_LIVE_COMMIT_OK tid={}", spec.tid);
    crate::yarm_log!("SPAWN_TASK_CONTEXT_OK tid={}", spec.tid);
    if spawn_lc {
        crate::yarm_log!("SPAWN_LIFECYCLE_THREAD_READY tid={}", spec.tid);
    }
    let bootstrap_cpu = CpuId(crate::arch::platform_constants::BOOTSTRAP_CPU_ID);
    // Pin all SystemServer tasks (supervisor, PM, init) to CPU 0 so the scheduler queue on the
    // bootstrap CPU has them in spawn order:
    //   [idle/TID0, supervisor/TID2, PM/TID3, init/TID1]
    // This guarantees supervisor and PM reach their recv() before init runs.
    let should_pin =
        matches!(spec.class, TaskClass::SystemServer) || spec.tid == BOOTSTRAP_FIRST_USER_TID;
    if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
        crate::yarm_log!(
            "FIRST_USER_ENQUEUE_DECISION cpu={} tid={} chosen_cpu={} reason={}",
            cpu.0,
            spec.tid,
            bootstrap_cpu.0,
            if should_pin {
                "bootstrap_pin"
            } else {
                "scheduler_default"
            }
        );
    }

    let enqueued_cpu = if should_pin {
        let chosen_cpu = bootstrap_cpu;
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!(
                "FINAL_FIRST_USER_ENQUEUE_SITE cpu={} tid={} chosen_cpu={} bootstrap_pin={}",
                cpu.0,
                spec.tid,
                chosen_cpu.0,
                should_pin as u8
            );
        }
        if chosen_cpu != bootstrap_cpu
            && cfg!(not(feature = "hosted-dev"))
            && DEBUG_DISPATCH_CONTEXT_LOG
        {
            crate::yarm_log!(
                "FIRST_USER_PIN_VIOLATION cpu={} tid={} chosen_cpu={}",
                cpu.0,
                spec.tid,
                chosen_cpu.0
            );
        }
        assert_eq!(chosen_cpu.0, bootstrap_cpu.0);
        owners.enqueue_on_cpu(chosen_cpu, spec.tid)?;
        chosen_cpu
    } else {
        owners.enqueue_balanced(spec.tid)?
    };
    if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
        crate::yarm_log!(
            "FIRST_USER_ENQUEUE cpu={} tid={} target_cpu={} status=ok",
            cpu.0,
            spec.tid,
            enqueued_cpu.0
        );
        crate::yarm_log!("BOOTSTRAP_FIRST_USER tid={} enqueued=true", spec.tid);
    }
    if spawn_lc {
        crate::yarm_log!("SPAWN_LIFECYCLE_PROCESS_READY tid={}", spec.tid);
        // Post-spawn invariant: exactly one live TCB for this tid, bound to the spawn ASID and not
        // already exited (a zombie). Any deviation is a leak.
        let tcb_count = owners.live_tcb_count_for(spec.tid);
        let zombie = owners.is_zombie(spec.tid);
        if tcb_count == 0 {
            crate::yarm_log!("SPAWN_LIFECYCLE_TCB_LEAK tid={} count=0", spec.tid);
        } else if tcb_count > 1 {
            crate::yarm_log!(
                "SPAWN_LIFECYCLE_DUPLICATE_TID tid={} count={}",
                spec.tid,
                tcb_count
            );
        } else if zombie {
            crate::yarm_log!("SPAWN_LIFECYCLE_ZOMBIE_LEAK tid={}", spec.tid);
        } else {
            crate::yarm_log!("SPAWN_LIFECYCLE_INVARIANT_OK tid={}", spec.tid);
        }
    }
    Ok(SpawnedUserTask {
        tid: spec.tid,
        entry: spec.entry,
        asid: Some(asid),
    })
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// The BROAD adapter: one acquisition per method, taken through `&mut KernelState`.
// ─────────────────────────────────────────────────────────────────────────────────────────

/// [`SpawnTxnOwners`] over the broad `KernelState`.
///
/// Every method here is a one-line delegation to the entry `KernelState` already had, which in
/// turn delegates to a rank-local body. The adapter adds no policy of its own, and exists only so
/// the generic transaction can be executed by a caller that holds the broad lock — exactly as it
/// always was.
pub(crate) struct BroadSpawnOwners<'a> {
    pub(crate) kernel: &'a mut crate::kernel::boot::KernelState,
}

impl SpawnTxnOwners for BroadSpawnOwners<'_> {
    fn current_cpu(&self) -> CpuId {
        self.kernel.current_cpu()
    }
    fn current_tid(&self) -> Option<u64> {
        self.kernel.current_tid()
    }
    fn current_task_cnode(&self) -> Option<CNodeId> {
        self.kernel.current_task_cnode()
    }
    fn capacity_limits(&self) -> RuntimeCapacityConfig {
        self.kernel.runtime_capacity_config()
    }

    fn task_status(&self, tid: u64) -> Option<TaskStatus> {
        self.kernel.task_status(tid)
    }
    fn live_task_count(&self) -> usize {
        self.kernel.with_tcbs(|tcbs| tcbs.iter().flatten().count())
    }
    fn allocate_thread_id(&mut self) -> Result<u64, KernelError> {
        self.kernel.allocate_thread_id()
    }
    fn stamp_spawn_generation(&mut self) -> u64 {
        self.kernel
            .with_task_spawn_generation_mut(|_tcbs, generation| {
                let issued = *generation;
                *generation = generation.saturating_add(1);
                issued
            })
    }
    fn insert_reservation(
        &mut self,
        tid: u64,
        reservation: crate::kernel::task::SpawnReservation,
    ) -> Option<usize> {
        self.kernel.with_task_enqueue_policy_mut(|tcbs, classes| {
            let class = reservation.class;
            let idx = tcbs.iter().position(|slot| slot.is_none())?;
            tcbs[idx] = Some(crate::kernel::task::ThreadControlBlock::reserved(
                crate::kernel::ipc::ThreadId(tid),
                reservation,
            ));
            classes[idx] = Some(class);
            Some(idx)
        })
    }
    fn clear_reservation_at(&mut self, index: usize) {
        self.kernel.with_task_enqueue_policy_mut(|tcbs, classes| {
            crate::kernel::spawn_reservation::clear_reservation_slot(tcbs, index);
            classes[index] = None;
        });
    }
    fn provision_default_kernel_context(&mut self, tid: u64) -> Result<(), KernelError> {
        self.kernel.provision_default_kernel_context(tid)
    }
    fn release_kernel_context(&mut self, tid: u64) {
        let _ = self.kernel.release_kernel_context(tid);
    }
    fn validate_cancellable(
        &mut self,
        token: &SpawnReservationToken,
    ) -> Result<(usize, u64), ReservationRefusal> {
        self.kernel.with_tcbs_mut(|tcbs| {
            crate::kernel::spawn_reservation::validate_cancellable(tcbs, token)
        })
    }
    fn claim_reservation(
        &mut self,
        token: &SpawnReservationToken,
    ) -> Result<SpawnBaseline, ReservationRefusal> {
        self.kernel
            .with_tcbs_mut(|tcbs| crate::kernel::spawn_reservation::claim_for_spawn(tcbs, token))
    }
    fn restore_after_failed_spawn(
        &mut self,
        token: &SpawnReservationToken,
        baseline: SpawnBaseline,
    ) -> bool {
        self.kernel.with_tcbs_mut(|tcbs| {
            crate::kernel::spawn_reservation::restore_after_failed_spawn(tcbs, token, baseline)
                .is_ok()
        })
    }
    fn bind_spawned_task_asid(&mut self, tid: u64, asid: Asid) -> Result<(), KernelError> {
        self.kernel.with_tcbs_mut(|tcbs| {
            crate::kernel::boot::exec_state::bind_spawned_task_asid_locked(tcbs, tid, asid)
        })
    }
    fn publish_spawned_image(
        &mut self,
        reservation: &SpawnReservationToken,
        publication: &SpawnedImagePublication,
    ) -> Result<(), ReservationRefusal> {
        self.kernel.with_tcbs_mut(|tcbs| {
            crate::kernel::boot::exec_state::publish_spawned_image_locked(
                tcbs,
                reservation,
                publication,
            )
        })
    }
    fn live_tcb_count_for(&self, tid: u64) -> usize {
        self.kernel
            .with_tcbs(|tcbs| tcbs.iter().flatten().filter(|tcb| tcb.tid.0 == tid).count())
    }
    fn is_zombie(&self, tid: u64) -> bool {
        self.kernel.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| matches!(tcb.status, TaskStatus::Exited(_) | TaskStatus::Dead))
                .unwrap_or(false)
        })
    }
    fn thread_kernel_stack_top(&self, tid: u64) -> u64 {
        self.kernel.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .and_then(|tcb| tcb.kernel_context.stack_top)
                .map(|t| t.0)
                .unwrap_or(0)
        })
    }
    fn thread_kernel_context_initialized(&self, tid: u64) -> bool {
        self.kernel
            .thread_kernel_context(tid)
            .is_some_and(|ctx| ctx.initialized)
    }
    fn initialize_thread_kernel_switch_frame(
        &mut self,
        tid: u64,
        entry: usize,
    ) -> Result<(), KernelError> {
        self.kernel
            .initialize_thread_kernel_switch_frame(tid, entry)
    }

    fn enqueue_on_cpu(&mut self, cpu: CpuId, tid: u64) -> Result<CpuId, KernelError> {
        self.kernel.enqueue_on_cpu(cpu, tid).map(|()| cpu)
    }
    fn enqueue_balanced(&mut self, tid: u64) -> Result<CpuId, KernelError> {
        self.kernel.enqueue_task(tid)
    }

    fn provision_process_cnode(
        &mut self,
        request: &ProcessCNodeRequest,
    ) -> Result<ProcessCNodeGrant, KernelError> {
        self.kernel.provision_process_cnode(request)
    }
    fn release_process_cnode_grant(
        &mut self,
        request: &ProcessCNodeRequest,
        grant: &ProcessCNodeGrant,
    ) {
        self.kernel.release_process_cnode_grant(request, grant);
    }
    fn reap_process_cnode_if_unused(&mut self, pid: u64) -> bool {
        self.kernel
            .maybe_cleanup_process_cnode_for_pid_noalloc_reap(pid)
    }
    fn task_cnode(&self, tid: u64) -> Option<CNodeId> {
        self.kernel.task_cnode(tid)
    }
    fn set_process_cnode_for_pid(&mut self, pid: u64, cnode: CNodeId) -> Result<(), KernelError> {
        self.kernel.set_process_cnode_for_pid(pid, cnode)
    }
    fn mint_capability_in_cnode(
        &mut self,
        cnode: CNodeId,
        capability: Capability,
    ) -> Result<CapId, KernelError> {
        self.kernel.mint_capability_in_cnode(cnode, capability)
    }
    fn provisional_cap_token(
        &mut self,
        cnode: CNodeId,
        cap: CapId,
    ) -> Result<ProvisionalCap, KernelError> {
        self.kernel.with_capability_state(|capability| {
            crate::kernel::boot::provisional_cap::provisional_cap_token_locked(
                capability, cnode, cap,
            )
        })
    }
    fn collect_cap_link_closure(&mut self, cap: CapId) -> Option<(LinkClosure, usize)> {
        self.kernel.with_capability_state(|capability| {
            crate::kernel::boot::provisional_cap::collect_link_closure_locked(capability, cap)
        })
    }
    fn resolve_link_pids(&self, links: &LinkClosure) -> ResolvedLinkPids {
        let mut out: ResolvedLinkPids = [None; MAX_PROVISIONAL_DESCENDANTS];
        for (slot, link) in out.iter_mut().zip(links.iter()) {
            if let Some(link) = link {
                let source = self.kernel.process_id(link.source_tid);
                let dest = self.kernel.process_id(link.dest_tid);
                *slot = match (source, dest) {
                    (Some(s), Some(d)) => Some((s, d)),
                    // A tid with no TCB is its own process by the same convention the link
                    // walkers have always used (`process_id(tid).unwrap_or(tid)`).
                    _ => Some((
                        source.unwrap_or(link.source_tid),
                        dest.unwrap_or(link.dest_tid),
                    )),
                };
            }
        }
        out
    }
    fn release_provisional_cap(
        &mut self,
        token: &ProvisionalCap,
        links: &LinkClosure,
        resolved: &ResolvedLinkPids,
    ) -> ProvisionalCapRelease {
        self.kernel.with_capability_state_mut(|capability| {
            crate::kernel::boot::provisional_cap::release_provisional_cap_locked(
                capability, token, links, resolved,
            )
        })
    }
    fn delegate_capability(
        &mut self,
        source_tid: u64,
        source_cap: CapId,
        dest_tid: u64,
        rights: CapRights,
    ) -> Result<DelegationGrant, KernelError> {
        self.kernel
            .delegate_capability(source_tid, source_cap, dest_tid, rights)
    }
    fn release_delegation(&mut self, grant: &DelegationGrant) -> bool {
        self.kernel.release_delegation(grant)
    }

    fn provision_service_endpoint(
        &mut self,
        request: &ServiceEndpointRequest,
    ) -> Result<ServiceEndpointGrant, KernelError> {
        self.kernel.provision_service_endpoint(request)
    }
    fn remove_unpublished_endpoint(&mut self, index: usize, generation: u64) -> EndpointRemoval {
        self.kernel.with_ipc_state_mut(|ipc| {
            crate::kernel::boot::spawn_ipc_cap_txn::remove_unpublished_endpoint_locked(
                ipc, index, generation,
            )
        })
    }

    fn provision_image(
        &mut self,
        request: &ImageProvisionRequest<'_>,
    ) -> Result<ProvisionToken, KernelError> {
        self.kernel.with_vm_then_memory_mut(|vm, memory| {
            crate::kernel::boot::spawn_image_provision::provision_image_locked(vm, memory, request)
        })
    }
    fn rollback_provision(&mut self, asid: Asid, phase: &'static str, err: KernelError) {
        self.kernel.with_vm_then_memory_mut(|vm, memory| {
            crate::kernel::boot::spawn_image_provision::rollback_provision_locked(
                vm, memory, asid, phase, err,
            )
        });
    }
    fn address_space_exists(&self, asid: Asid) -> bool {
        self.kernel
            .with_user_spaces(|spaces| spaces.get(asid).is_some())
    }
    fn destroy_unresident_address_space(&mut self, asid: Asid) -> bool {
        self.kernel
            .with_vm_then_memory_mut(|vm, memory| {
                crate::kernel::boot::vm_image_locked::destroy_unresident_address_space_locked(
                    vm, memory, asid,
                )
            })
            .is_ok()
    }
    fn destroy_live_address_space(&mut self, asid: Asid) -> bool {
        self.kernel.destroy_user_address_space_by_asid(asid).is_ok()
    }
    fn asid_carrier_tid(&self, asid: Asid) -> Option<u64> {
        self.kernel.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.asid == Some(asid))
                .map(|tcb| tcb.tid.0)
        })
    }
    fn allocate_user_stack_with_guard(
        &mut self,
        tid: u64,
        pages: usize,
    ) -> Result<VirtAddr, KernelError> {
        self.kernel.allocate_user_stack_with_guard(tid, pages)
    }
    fn copy_to_user(&mut self, asid: Asid, va: VirtAddr, bytes: &[u8]) -> Result<(), KernelError> {
        self.kernel.copy_to_user(asid, va, bytes)
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// The SPLIT adapter: one acquisition per method, taken through `SharedKernel` seams, with no
// broad lock held anywhere.
// ─────────────────────────────────────────────────────────────────────────────────────────

/// [`SpawnTxnOwners`] over `SharedKernel` — the pre-lock spawn route's acquisition layer.
///
/// Every method is one acquisition of one domain through a seam that already existed, wrapping
/// the same rank-local body its broad twin wraps. The adapter contributes acquisitions and
/// nothing else: no phase order, no validation sequencing, no rollback decision — those are the
/// generic policy's, which is why the two adapters cannot disagree about them.
///
/// # Three methods refuse rather than act, and that is deliberate
///
/// `allocate_user_stack_with_guard`, `initialize_thread_kernel_switch_frame` and
/// `destroy_live_address_space` are unreachable from the split route by construction, and each
/// refuses loudly instead of carrying a second, unexercised implementation of something delicate:
///
/// * the stack allocator is reached only when `provisioned_stack_top` is `None`, and the
///   transaction always provisions one (that is what makes a stack failure rollable-back);
/// * the x86_64 switch-frame init is reached only for TID 1 or 2, and NR 23 / NR 29 allocate
///   dynamic TIDs, never a bootstrap one;
/// * the live address-space teardown is reached only through the ledger's
///   `SPAWN_LEDGER_ASID_STILL_BOUND` arm, which cannot happen because the reservation restore
///   un-binds the ASID before the ledger unwinds.
///
/// Refusing is also the direction `AI_AGENT_RULES` §14.4 requires of the third one: a teardown
/// that cannot complete its shootdown must leave the frame UNAVAILABLE rather than recycle memory
/// a remote CPU may still translate. A guard pins that the split route cannot reach any of them.
pub(crate) struct SharedSpawnOwners<'a> {
    pub(crate) shared: &'a crate::runtime::SharedKernel,
    /// The spawning task, snapshotted ONCE before the transaction begins.
    ///
    /// U9-SPAWN-IC1's rule: the caller identity is established up front and passed explicitly,
    /// never re-read from an ambient current-task lookup partway through, because between two
    /// phases the current task can change.
    pub(crate) spawner_tid: Option<u64>,
    pub(crate) spawner_cnode: Option<CNodeId>,
    pub(crate) cpu: CpuId,
}

impl SpawnTxnOwners for SharedSpawnOwners<'_> {
    fn current_cpu(&self) -> CpuId {
        self.cpu
    }
    fn current_tid(&self) -> Option<u64> {
        self.spawner_tid
    }
    fn current_task_cnode(&self) -> Option<CNodeId> {
        self.spawner_cnode
    }
    fn capacity_limits(&self) -> RuntimeCapacityConfig {
        self.shared.runtime_capacity_config_split_read()
    }

    fn task_status(&self, tid: u64) -> Option<TaskStatus> {
        self.shared.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.status)
        })
    }
    fn live_task_count(&self) -> usize {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| tcbs.iter().flatten().count())
    }
    fn allocate_thread_id(&mut self) -> Result<u64, KernelError> {
        let max_tasks = self.capacity_limits().max_tasks;
        let policy = self.shared.tid_allocation_policy_split_read();
        let (tid, delta) = self.shared.with_spawn_thread_split_mut(
            |tcbs, _classes, cursor, _tls| {
                crate::kernel::boot::spawn_thread_core::allocate_dynamic_tid_locked(
                    tcbs,
                    cursor,
                    policy,
                    max_tasks,
                )
            },
        )?;
        // Telemetry is rank 10 and is applied with the task lock released, exactly as the broad
        // twin does — which is what keeps 2 → 10 from ever being held nested.
        self.shared.apply_tid_allocation_delta_split(delta);
        Ok(tid)
    }
    fn stamp_spawn_generation(&mut self) -> u64 {
        self.shared
            .with_task_spawn_generation_split_mut(|_tcbs, generation| {
                let issued = *generation;
                *generation = generation.saturating_add(1);
                issued
            })
    }
    fn insert_reservation(
        &mut self,
        tid: u64,
        reservation: crate::kernel::task::SpawnReservation,
    ) -> Option<usize> {
        self.shared
            .with_task_enqueue_policy_split_mut(|tcbs, classes| {
                let class = reservation.class;
                let idx = tcbs.iter().position(|slot| slot.is_none())?;
                tcbs[idx] = Some(crate::kernel::task::ThreadControlBlock::reserved(
                    crate::kernel::ipc::ThreadId(tid),
                    reservation,
                ));
                classes[idx] = Some(class);
                Some(idx)
            })
    }
    fn clear_reservation_at(&mut self, index: usize) {
        self.shared
            .with_task_enqueue_policy_split_mut(|tcbs, classes| {
                crate::kernel::spawn_reservation::clear_reservation_slot(tcbs, index);
                classes[index] = None;
            });
    }
    fn provision_default_kernel_context(&mut self, tid: u64) -> Result<(), KernelError> {
        self.shared
            .provision_default_kernel_context_split(tid)
            .map(|_| ())
    }
    fn release_kernel_context(&mut self, tid: u64) {
        let _ = self.shared.with_task_tcbs_split_mut(|tcbs| {
            crate::kernel::boot::thread_state::release_kernel_context_locked(tcbs, tid)
        });
    }
    fn validate_cancellable(
        &mut self,
        token: &SpawnReservationToken,
    ) -> Result<(usize, u64), ReservationRefusal> {
        self.shared.with_task_tcbs_split_mut(|tcbs| {
            crate::kernel::spawn_reservation::validate_cancellable(tcbs, token)
        })
    }
    fn claim_reservation(
        &mut self,
        token: &SpawnReservationToken,
    ) -> Result<SpawnBaseline, ReservationRefusal> {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| {
                crate::kernel::spawn_reservation::claim_for_spawn(tcbs, token)
            })
    }
    fn restore_after_failed_spawn(
        &mut self,
        token: &SpawnReservationToken,
        baseline: SpawnBaseline,
    ) -> bool {
        self.shared.with_task_tcbs_split_mut(|tcbs| {
            crate::kernel::spawn_reservation::restore_after_failed_spawn(tcbs, token, baseline)
                .is_ok()
        })
    }
    fn bind_spawned_task_asid(&mut self, tid: u64, asid: Asid) -> Result<(), KernelError> {
        self.shared.bind_spawned_task_asid_split(tid, asid)
    }
    fn publish_spawned_image(
        &mut self,
        reservation: &SpawnReservationToken,
        publication: &SpawnedImagePublication,
    ) -> Result<(), ReservationRefusal> {
        self.shared
            .publish_spawned_image_split(reservation, publication)
    }
    fn live_tcb_count_for(&self, tid: u64) -> usize {
        self.shared.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter().flatten().filter(|tcb| tcb.tid.0 == tid).count()
        })
    }
    fn is_zombie(&self, tid: u64) -> bool {
        self.shared.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| matches!(tcb.status, TaskStatus::Exited(_) | TaskStatus::Dead))
                .unwrap_or(false)
        })
    }
    fn thread_kernel_stack_top(&self, tid: u64) -> u64 {
        self.shared.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .and_then(|tcb| tcb.kernel_context.stack_top)
                .map(|t| t.0)
                .unwrap_or(0)
        })
    }
    fn thread_kernel_context_initialized(&self, tid: u64) -> bool {
        self.shared.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .is_some_and(|tcb| tcb.kernel_context.initialized)
        })
    }
    fn initialize_thread_kernel_switch_frame(
        &mut self,
        tid: u64,
        _entry: usize,
    ) -> Result<(), KernelError> {
        // UNREACHABLE from the split route: only TID 1 / TID 2 reach this, and NR 23 / NR 29
        // allocate dynamic TIDs. Refuse loudly rather than carry a second implementation of the
        // switch-frame publication. The policy already tolerates this failure — it logs
        // `D6_KERNEL_SWITCH_FRAME_INIT_DEFERRED` and continues — so a refusal is behaviourally
        // identical to the failure arm the broad path already has.
        crate::yarm_log!(
            "SPAWN_SPLIT_OWNER_REFUSED op=initialize_thread_kernel_switch_frame tid={} \
             reason=bootstrap_only",
            tid
        );
        Err(KernelError::WrongObject)
    }

    fn enqueue_on_cpu(&mut self, cpu: CpuId, tid: u64) -> Result<CpuId, KernelError> {
        self.shared.enqueue_on_cpu_split(cpu, tid)
    }
    fn enqueue_balanced(&mut self, tid: u64) -> Result<CpuId, KernelError> {
        self.shared.enqueue_task_split(self.cpu, tid)
    }

    fn provision_process_cnode(
        &mut self,
        request: &ProcessCNodeRequest,
    ) -> Result<ProcessCNodeGrant, KernelError> {
        // The growth limits are derived from the capacity profile OUTSIDE the capability lock,
        // exactly as the broad twin derives them, so the rank-4 body reads no config domain.
        let limits = self.capacity_limits();
        let max_total_cnode_slots = limits.max_total_cnode_slots;
        let requested = crate::kernel::boot::KernelState::requested_cnode_slot_capacity_for_class(
            request.class,
            limits,
            None,
        )?;
        let bounded =
            crate::kernel::boot::KernelState::normalize_requested_cnode_slots(requested, limits)?;
        self.shared.with_capability_state_split_mut(|capability| {
            crate::kernel::boot::process_cnode_txn::provision_process_cnode_locked(
                capability,
                request,
                bounded,
                max_total_cnode_slots,
            )
        })
    }
    fn release_process_cnode_grant(
        &mut self,
        request: &ProcessCNodeRequest,
        grant: &ProcessCNodeGrant,
    ) {
        self.shared.with_capability_state_split_mut(|capability| {
            crate::kernel::boot::process_cnode_txn::release_process_cnode_grant_locked(
                capability, request, grant,
            );
        });
    }
    fn reap_process_cnode_if_unused(&mut self, pid: u64) -> bool {
        self.shared.reap_process_cnode_split(pid)
    }
    fn task_cnode(&self, tid: u64) -> Option<CNodeId> {
        self.shared.task_cnode_split(tid)
    }
    fn set_process_cnode_for_pid(&mut self, pid: u64, cnode: CNodeId) -> Result<(), KernelError> {
        self.shared.set_process_cnode_for_pid_split(pid, cnode)
    }
    fn mint_capability_in_cnode(
        &mut self,
        cnode: CNodeId,
        capability: Capability,
    ) -> Result<CapId, KernelError> {
        let limits = self.capacity_limits();
        let growth = crate::kernel::boot::spawn_ipc_cap_txn::CnodeGrowthLimits {
            slot_capacity: crate::kernel::capabilities::MAX_CAPABILITIES_PER_CSPACE
                .min(limits.max_capability_slots),
            max_total_cnode_slots: limits.max_total_cnode_slots,
        };
        self.shared.with_capability_state_split_mut(|state| {
            crate::kernel::boot::spawn_ipc_cap_txn::mint_in_cnode_locked(
                state,
                cnode,
                capability,
                growth.slot_capacity,
                growth.max_total_cnode_slots,
            )
        })
    }

    fn provisional_cap_token(
        &mut self,
        cnode: CNodeId,
        cap: CapId,
    ) -> Result<ProvisionalCap, KernelError> {
        self.shared.with_capability_state_split_mut(|capability| {
            crate::kernel::boot::provisional_cap::provisional_cap_token_locked(
                capability, cnode, cap,
            )
        })
    }
    fn collect_cap_link_closure(&mut self, cap: CapId) -> Option<(LinkClosure, usize)> {
        self.shared.with_capability_state_split_mut(|capability| {
            crate::kernel::boot::provisional_cap::collect_link_closure_locked(capability, cap)
        })
    }
    fn resolve_link_pids(&self, links: &LinkClosure) -> ResolvedLinkPids {
        let mut out: ResolvedLinkPids = [None; MAX_PROVISIONAL_DESCENDANTS];
        for (slot, link) in out.iter_mut().zip(links.iter()) {
            if let Some(link) = link {
                *slot = Some((
                    self.shared
                        .process_id_split_read(link.source_tid)
                        .unwrap_or(link.source_tid),
                    self.shared
                        .process_id_split_read(link.dest_tid)
                        .unwrap_or(link.dest_tid),
                ));
            }
        }
        out
    }
    fn release_provisional_cap(
        &mut self,
        token: &ProvisionalCap,
        links: &LinkClosure,
        resolved: &ResolvedLinkPids,
    ) -> ProvisionalCapRelease {
        self.shared.with_capability_state_split_mut(|capability| {
            crate::kernel::boot::provisional_cap::release_provisional_cap_locked(
                capability, token, links, resolved,
            )
        })
    }
    fn delegate_capability(
        &mut self,
        source_tid: u64,
        source_cap: CapId,
        dest_tid: u64,
        rights: CapRights,
    ) -> Result<DelegationGrant, KernelError> {
        // Identities and the object being delegated are resolved FIRST, at ranks 2/3, and the
        // rank-4 body revalidates the object before it mints — so a slot recycled in the gap is
        // refused with `StaleCapability` rather than delegated.
        let capability = self
            .shared
            .resolve_capability_for_task_split(source_tid, source_cap)?;
        let source_cnode = self
            .shared
            .task_cnode_split(source_tid)
            .ok_or(KernelError::TaskMissing)?;
        let dest_cnode = self
            .shared
            .task_cnode_split(dest_tid)
            .ok_or(KernelError::TaskMissing)?;
        let identity = crate::kernel::boot::spawn_ipc_cap_txn::DelegationIdentity {
            source_tid,
            source_cnode,
            dest_tid,
            dest_cnode,
        };
        let limits = self.capacity_limits();
        let cnode_limits = crate::kernel::boot::spawn_ipc_cap_txn::CnodeGrowthLimits {
            slot_capacity: crate::kernel::boot::KernelState::normalize_requested_cnode_slots(
                crate::kernel::capabilities::MAX_CAPABILITIES_PER_CSPACE,
                limits,
            )?,
            max_total_cnode_slots: limits.max_total_cnode_slots,
        };
        let grant = self.shared.with_capability_state_split_mut(|state| {
            crate::kernel::boot::spawn_ipc_cap_txn::delegate_capability_locked(
                state,
                &identity,
                source_cap,
                rights,
                capability.object,
                cnode_limits,
            )
        })?;
        // The memory-object refcount is rank 6 and runs with rank 4 RELEASED. §1 proved this is
        // never owed for a spawn's capabilities — they are `Endpoint`/`AddressSpace` — but the
        // owner returns the fact rather than assuming it, so a future caller delegating a
        // MemoryObject through this adapter still gets the tail.
        if grant.owes_memory_refcount {
            self.shared.with_memory_split_mut(|memory| {
                crate::kernel::boot::KernelState::adjust_memory_object_cap_refcount_locked(
                    memory,
                    grant.object,
                    1,
                );
            });
        }
        Ok(grant)
    }
    fn release_delegation(&mut self, grant: &DelegationGrant) -> bool {
        let released = self.shared.with_capability_state_split_mut(|state| {
            crate::kernel::boot::spawn_ipc_cap_txn::release_delegation_grant_locked(state, grant)
        });
        if released && grant.owes_memory_refcount {
            self.shared.with_memory_split_mut(|memory| {
                crate::kernel::boot::KernelState::adjust_memory_object_cap_refcount_locked(
                    memory,
                    grant.object,
                    -1,
                );
            });
        }
        crate::yarm_log!(
            "SPAWN_DELEGATE_RELEASED dest_cnode={} dest_cap={} released={}",
            grant.identity.dest_cnode.0,
            grant.dest_cap.0,
            u8::from(released)
        );
        released
    }

    fn provision_service_endpoint(
        &mut self,
        request: &ServiceEndpointRequest,
    ) -> Result<ServiceEndpointGrant, KernelError> {
        self.shared
            .with_ipc_then_capability_split_mut(|ipc, capability| {
                crate::kernel::boot::spawn_ipc_cap_txn::provision_service_endpoint_locked(
                    ipc, capability, request,
                )
            })
    }
    fn remove_unpublished_endpoint(&mut self, index: usize, generation: u64) -> EndpointRemoval {
        self.shared.with_ipc_split_mut(|ipc| {
            crate::kernel::boot::spawn_ipc_cap_txn::remove_unpublished_endpoint_locked(
                ipc, index, generation,
            )
        })
    }

    fn provision_image(
        &mut self,
        request: &ImageProvisionRequest<'_>,
    ) -> Result<ProvisionToken, KernelError> {
        self.shared.with_vm_then_memory_split_mut(|vm, memory| {
            crate::kernel::boot::spawn_image_provision::provision_image_locked(vm, memory, request)
        })
    }
    fn rollback_provision(&mut self, asid: Asid, phase: &'static str, err: KernelError) {
        self.shared.with_vm_then_memory_split_mut(|vm, memory| {
            crate::kernel::boot::spawn_image_provision::rollback_provision_locked(
                vm, memory, asid, phase, err,
            )
        });
    }
    fn address_space_exists(&self, asid: Asid) -> bool {
        self.shared
            .with_vm_user_spaces_split_mut(|spaces| spaces.get(asid).is_some())
    }
    fn destroy_unresident_address_space(&mut self, asid: Asid) -> bool {
        self.shared
            .with_vm_then_memory_split_mut(|vm, memory| {
                crate::kernel::boot::vm_image_locked::destroy_unresident_address_space_locked(
                    vm, memory, asid,
                )
            })
            .is_ok()
    }
    fn destroy_live_address_space(&mut self, asid: Asid) -> bool {
        // UNREACHABLE from the split route: the ledger reaches this only through its
        // `SPAWN_LEDGER_ASID_STILL_BOUND` arm, and the reservation restore un-binds the ASID
        // before the ledger unwinds, so no TCB carries it. Refusing is also the direction
        // `AI_AGENT_RULES` §14.4 requires: a teardown that cannot complete its shootdown must
        // leave the frames UNAVAILABLE rather than recycle memory a remote CPU may translate.
        crate::yarm_log!(
            "SPAWN_SPLIT_OWNER_REFUSED op=destroy_live_address_space asid={} \
             reason=ledger_contract_violated",
            asid.0
        );
        false
    }
    fn asid_carrier_tid(&self, asid: Asid) -> Option<u64> {
        self.shared.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.asid == Some(asid))
                .map(|tcb| tcb.tid.0)
        })
    }
    fn allocate_user_stack_with_guard(
        &mut self,
        tid: u64,
        _pages: usize,
    ) -> Result<VirtAddr, KernelError> {
        // UNREACHABLE from the split route: the transaction always provisions the stack before
        // the commit — that is precisely what makes a stack-allocation failure rollable-back — so
        // `provisioned_stack_top` is always `Some` here.
        crate::yarm_log!(
            "SPAWN_SPLIT_OWNER_REFUSED op=allocate_user_stack_with_guard tid={} \
             reason=stack_is_pre_provisioned",
            tid
        );
        Err(KernelError::WrongObject)
    }
    fn copy_to_user(&mut self, asid: Asid, va: VirtAddr, bytes: &[u8]) -> Result<(), KernelError> {
        self.shared.copy_to_user_split(asid, va, bytes)
    }
}
