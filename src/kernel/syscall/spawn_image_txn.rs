// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-SPAWN1 SP-3 — THE image-loading spawn transaction, and its provisional-resource ledger.
//!
//! # The defect this repairs
//!
//! Every image-loading spawn handler — `SpawnProcess` (NR 23), `SpawnProcessFromUserBuf`
//! (NR 24) and `SpawnFromMemoryObject` (NR 29), plus the NR 26 handler that U9-ASPACE1 §2 later
//! retired — acquired the same resources in the same order and returned through a bare `?` at
//! each step:
//!
//! ```text
//!   allocate_thread_id → create_user_address_space → load ELF → create_endpoint ×2
//!     → grant_capability_task_to_task_with_rights → reserve_task_for_spawn_with_class
//!     → spawn_user_task_from_image
//! ```
//!
//! Every one of those `?`s abandoned everything already acquired, and the arms were not equal:
//!
//! | failing step                  | leaked                                                        |
//! |-------------------------------|---------------------------------------------------------------|
//! | ELF load                      | one address space and its partially installed mappings         |
//! | reservation                   | + two endpoints, four minted capabilities, one parent delegation |
//! | `spawn_user_task_from_image`  | + the reservation itself                                       |
//!
//! The last row is the subtle one. `spawn_user_task_from_image` is transactional *about the
//! reservation*: on failure it restores the same incarnation to `ReservedUnstarted` so the
//! caller's token stays valid. No caller ever used that. The reserved TCB slot, its class slot,
//! its kernel stack and its process CNode stayed occupied for the rest of the boot.
//!
//! # The rule
//!
//! One transaction acquires provisional resources in ledger order and, on any failure, releases
//! them in exactly the reverse order — each through the resource's ACTUAL owner.
//!
//! "Actual" carries the weight. The natural guess — that [`KernelState::cancel_spawn_reservation`]
//! undoes a spawn — is wrong. It owns the reserved TCB, its class slot, its kernel context and
//! (only when no other thread still owns the process) its process CNode. It does not own the
//! address space, the endpoints or the capabilities, because it never created them.
//!
//! | resource class                         | actual owner                                        |
//! |----------------------------------------|-----------------------------------------------------|
//! | task reservation; task/process records | [`KernelState::cancel_spawn_reservation`]           |
//! | ASID / address space                   | [`KernelState::destroy_user_address_space_by_asid`] |
//! | ELF mappings and their backing         | the same call — draining an ASID drains every mapping installed in it, reclaims each frame, and runs the U9-MO2 backing-aware `MemoryObject` reclaim per frame |
//! | MemoryObject / refcount state          | the same call, plus the refcount adjust inside capability revocation |
//! | endpoints                              | [`KernelState::destroy_endpoint`]                   |
//! | installed / delegated capabilities     | [`KernelState::revoke_capability_in_cnode`], which cascades to every delegated descendant — so revoking the spawner's send capability also removes the copy delegated into the parent's cspace, and the ledger records only the ROOT |
//!
//! # Ordering
//!
//! The reservation is taken FIRST, before the first provisional address-space allocation. Two
//! reasons: it is the resource whose absence made the last failure arm unrecoverable, and taking
//! it first means no later phase can hand the same TID to a second transaction. Nothing becomes
//! reachable early by doing so — a reservation is `TaskStatus::Reserved`, which cannot be
//! dispatched, enqueued, woken, blocked, joined or published as an endpoint waiter. The single
//! step that makes a live, reachable task is `spawn_user_task_from_image`, and it is last.
//!
//! # Inertness
//!
//! [`KernelState::unwind_spawn_ledger`] consumes the ledger by value, so a second unwind of one
//! transaction cannot be written. Within an unwind every owner is already refusal-safe: a stale
//! [`SpawnReservationToken`] is refused with zero mutation, an empty endpoint slot returns
//! `WrongObject`, an absent capability returns `InvalidCapability`. Release failures are logged
//! and never abort the rest of the unwind — a resource that is already gone must not strand the
//! ones that are not.
//!
//! # What this transaction deliberately does NOT change
//!
//! Endpoint creation and parent delegation stay TOLERANT of failure: the spawn proceeds with
//! capability id 0 and the child comes up without its service endpoint, exactly as before. That
//! is a pre-existing policy decision about what a spawn means, not a leak, and SP-3 is a
//! compensation repair.

use super::{SyscallError, current_tid};
use crate::kernel::boot::spawn_image_provision::{ImageProvision, ImageSource};
use crate::kernel::boot::spawn_ipc_cap_txn::{CnodeGrowthLimits, ServiceEndpointRequest};
use crate::kernel::boot::{KernelError, KernelState, UserImageSpec};
use crate::kernel::capabilities::{CNodeId, CapId, CapRights};
use crate::kernel::ipc::EndpointMode;
use crate::kernel::spawn_reservation::SpawnReservationToken;
use crate::kernel::task::TaskClass;
use crate::kernel::vm::Asid;

/// The user stack every image-loading spawn gets, in pages.
///
/// The value is the one `spawn_image_after_claim` has always passed; U9-SPAWN-VM1 only moved the
/// decision to the caller that now owns the stack's rollback, so it is stated once.
const SPAWN_USER_STACK_PAGES: usize = 64;

/// Queue depth of each service endpoint a spawn creates. The value `create_endpoint` was always
/// called with; stated once now that the request is built explicitly.
const SERVICE_ENDPOINT_DEPTH: usize = 8;

/// One provisional resource, with the identity its owner needs to release it.
///
/// Exhaustive by construction: a phase that acquires a NEW class of resource has to add a
/// variant here, and [`KernelState::release_provisional_spawn_resource`]'s match fails to
/// compile until that class is given an owner.
#[derive(Debug)]
pub(crate) enum ProvisionalSpawnResource {
    /// The reserved, unstarted task incarnation: TCB slot, class slot, kernel context, and the
    /// process CNode when no other thread still owns it.
    Reservation(SpawnReservationToken),
    /// A user address space: the ASID, every mapping installed in it (ELF PT_LOAD segments,
    /// zero-copy initramfs grants, the read-only initrd window NR 23 maps for `initramfs_srv`),
    /// the frames behind them, and their MemoryObject backing.
    ///
    /// # Why this entry names a NEVER-PUBLISHED address space (U9-SPAWN-IC1 §4)
    ///
    /// The variant is constructed at exactly one site: immediately after
    /// `provision_spawn_image` returns. U9-SPAWN-VM1 established that the ASID it returns is bound
    /// to no TCB — the only writes of `tcb.asid` in this path are inside `spawn_image_after_claim`,
    /// which runs later — and the child is `ReservedUnstarted`, so it cannot be dispatched and
    /// cannot make its own ASID resident.
    ///
    /// That remains true at every point the ledger can unwind, INCLUDING after a failed commit:
    /// `spawn_user_task_from_image` restores the same incarnation through
    /// `restore_after_failed_spawn`, whose `SpawnBaseline` captures and replays `tcb.asid`, so a
    /// commit that got as far as binding the ASID has unbound it again by the time the ledger runs.
    ///
    /// The release does not take that on trust. It re-checks the TCB table and only then uses the
    /// never-resident owner; a carrier found there is a contract violation, and the release says so
    /// and falls back to the live teardown rather than skipping a shootdown that might be owed.
    AddressSpace(Asid),
    /// One provisional service endpoint AND the two capabilities naming it, as one grant.
    ///
    /// U9-SPAWN-IC1 replaced a bare `Endpoint(usize)` with the grant, because an index is not an
    /// identity: endpoint slots are recycled, so a stale unwind naming a reused index would have
    /// destroyed a REPLACEMENT endpoint belonging to someone else. The grant carries the
    /// generation, and the release refuses unless the incarnation still matches.
    Endpoint(crate::kernel::boot::spawn_ipc_cap_txn::ServiceEndpointGrant),
    /// One capability delegated from the spawner into the parent's cspace, with the exact
    /// identity needed to take it back: the destination cspace, the capability id AND the object
    /// it carries. The object is what makes a stale release safe — a recycled slot holds someone
    /// else's capability, and the release refuses rather than revoking it.
    ///
    /// Recorded even though the revoke cascade from the service send capability would also remove
    /// it: an explicit entry means the ledger states what it owns instead of relying on a side
    /// effect of another entry, and because entries are released in reverse acquisition order this
    /// one runs FIRST, before the cascade could make it a no-op.
    Delegation(crate::kernel::boot::spawn_ipc_cap_txn::DelegationGrant),
    /// One capability minted into a named CNode — and, through the revoke cascade, every
    /// descendant delegated from it.
    Capability { cnode: CNodeId, cap: CapId },
}

/// The most provisional resources one image-loading spawn holds at once:
/// 1 reservation + 1 address space + 1 address-space capability + 2 endpoint grants
/// + 1 parent delegation = 6.
///
/// U9-SPAWN-IC1 lowered this from 9. Each endpoint and its two capabilities used to take three
/// entries because they were three independently-acquired resources; they are now one grant
/// acquired in one transaction, so they take one. The parent delegation gained an entry of its
/// own — it used to rely on being a descendant of the service send capability and disappearing in
/// that revoke's cascade, which is true but left the ledger silent about something it owned.
pub(crate) const MAX_PROVISIONAL_SPAWN_RESOURCES: usize = 6;

/// The provisional resources one in-flight spawn owns, in acquisition order.
#[derive(Debug)]
pub(crate) struct SpawnLedger {
    entries: [Option<ProvisionalSpawnResource>; MAX_PROVISIONAL_SPAWN_RESOURCES],
    len: usize,
}

impl SpawnLedger {
    pub(crate) const fn new() -> Self {
        Self {
            entries: [const { None }; MAX_PROVISIONAL_SPAWN_RESOURCES],
            len: 0,
        }
    }

    /// Record a resource the transaction now owns.
    ///
    /// Overflow would be a leak rather than a benign truncation, so it is logged loudly instead
    /// of silently dropped. `MAX_PROVISIONAL_SPAWN_RESOURCES` is proved sufficient for every
    /// production spawn shape by the SP-3 ledger-capacity guard.
    pub(crate) fn record(&mut self, resource: ProvisionalSpawnResource) {
        if self.len >= MAX_PROVISIONAL_SPAWN_RESOURCES {
            crate::yarm_log!(
                "SPAWN_LEDGER_OVERFLOW capacity={} dropped={:?}",
                MAX_PROVISIONAL_SPAWN_RESOURCES,
                resource
            );
            return;
        }
        self.entries[self.len] = Some(resource);
        self.len += 1;
    }

    /// How many provisional resources the transaction currently owns.
    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    /// Give up ownership WITHOUT releasing anything: the transaction committed, and every
    /// resource in the ledger now belongs to the live task.
    ///
    /// Consuming `self` is the entire mechanism. The ledger holds identifiers, not handles, so
    /// dropping it releases nothing; what it buys is that a committed transaction can no longer
    /// be unwound, because [`KernelState::unwind_spawn_ledger`] needs a ledger by value and
    /// there is no longer one to give it.
    pub(crate) fn commit(self) {
        crate::yarm_log!("SPAWN_LEDGER_COMMITTED held={}", self.len);
    }
}

impl KernelState {
    /// Release ONE provisional resource through its actual owner.
    fn release_provisional_spawn_resource(&mut self, resource: ProvisionalSpawnResource) {
        match resource {
            ProvisionalSpawnResource::Reservation(token) => {
                let tid = token.tid();
                let ok = self.cancel_spawn_reservation(token).is_ok();
                crate::yarm_log!(
                    "SPAWN_LEDGER_RELEASE class=reservation tid={} ok={}",
                    tid,
                    u8::from(ok)
                );
            }
            ProvisionalSpawnResource::AddressSpace(asid) => {
                // U9-SPAWN-IC1 §4 — the two cases are decided here and never conflated.
                //
                // A never-resident ASID owes no TLB shootdown and must NOT take a retired-ASID
                // slot: that array is `MAX_ADDRESS_SPACES` deep and drains only on CPU
                // acknowledgement, so a rollback path consuming one per failed spawn would
                // eventually make the teardown refuse — an exact rollback turning into a leak.
                // A published one owes the full live teardown, and gets it.
                let carrier = self.with_tcbs(|tcbs| {
                    tcbs.iter()
                        .flatten()
                        .find(|tcb| tcb.asid == Some(asid))
                        .map(|tcb| tcb.tid.0)
                });
                match carrier {
                    None => {
                        let ok = self
                            .with_vm_then_memory_mut(|vm, memory| {
                                crate::kernel::boot::vm_image_locked::destroy_unresident_address_space_locked(
                                    vm, memory, asid,
                                )
                            })
                            .is_ok();
                        crate::yarm_log!(
                            "SPAWN_LEDGER_RELEASE class=address_space asid={} owner=unresident ok={}",
                            asid.0,
                            u8::from(ok)
                        );
                    }
                    Some(tid) => {
                        // The ledger's own contract says this cannot happen. Report it and take
                        // the safe path rather than skip a shootdown that might be owed.
                        crate::yarm_log!(
                            "SPAWN_LEDGER_ASID_STILL_BOUND asid={} tid={} result=live_teardown",
                            asid.0,
                            tid
                        );
                        let ok = self.destroy_user_address_space_by_asid(asid).is_ok();
                        crate::yarm_log!(
                            "SPAWN_LEDGER_RELEASE class=address_space asid={} owner=live ok={}",
                            asid.0,
                            u8::from(ok)
                        );
                    }
                }
            }
            ProvisionalSpawnResource::Endpoint(grant) => {
                // Capabilities first, then the endpoint — and only if it is still the same
                // unpublished incarnation. See `KernelState::release_service_endpoint_grant`.
                let removal = self.release_service_endpoint_grant(&grant);
                crate::yarm_log!(
                    "SPAWN_LEDGER_RELEASE class=endpoint index={} generation={} outcome={:?}",
                    grant.endpoint_index,
                    grant.endpoint_generation,
                    removal
                );
            }
            ProvisionalSpawnResource::Delegation(grant) => {
                let released = self.release_delegation(&grant);
                crate::yarm_log!(
                    "SPAWN_LEDGER_RELEASE class=delegation dest_cnode={} dest_cap={} released={}",
                    grant.identity.dest_cnode.0,
                    grant.dest_cap.0,
                    u8::from(released)
                );
            }
            ProvisionalSpawnResource::Capability { cnode, cap } => {
                let ok = self.revoke_capability_in_cnode(cnode, cap).is_ok();
                crate::yarm_log!(
                    "SPAWN_LEDGER_RELEASE class=capability cnode={} cap={} ok={}",
                    cnode.0,
                    cap.0,
                    u8::from(ok)
                );
            }
        }
    }

    /// Undo an in-flight spawn: release every provisional resource in REVERSE acquisition order,
    /// each through its actual owner, restoring the baseline the transaction started from.
    ///
    /// Consumes the ledger, so one transaction cannot be unwound twice.
    pub(crate) fn unwind_spawn_ledger(&mut self, mut ledger: SpawnLedger) {
        let held = ledger.len;
        for slot_index in (0..held).rev() {
            if let Some(resource) = ledger.entries[slot_index].take() {
                self.release_provisional_spawn_resource(resource);
            }
        }
        crate::yarm_log!("SPAWN_LEDGER_UNWOUND released={} result=baseline", held);
    }
}

/// Carry a phase's outcome forward, or unwind the whole transaction and fail.
fn advance<T>(
    kernel: &mut KernelState,
    ledger: SpawnLedger,
    outcome: Result<T, KernelError>,
    name: &'static str,
) -> Result<(T, SpawnLedger), SyscallError> {
    match outcome {
        Ok(value) => Ok((value, ledger)),
        Err(err) => {
            crate::yarm_log!("KSPAWN_FAIL phase={} err={:?} result=unwinding", name, err);
            kernel.unwind_spawn_ledger(ledger);
            Err(SyscallError::from(err))
        }
    }
}

/// How this spawn's image reaches the new address space. The only genuine variation between the
/// four handlers.
pub(crate) enum SpawnImageSource<'a> {
    /// NR 23 / 24 / 26: PT_LOAD segments staged from a kernel-side ELF slice, with the entry
    /// point taken from the caller's already-parsed header.
    PtLoadSegments { elf: &'a [u8], entry: usize },
    /// NR 29: the initramfs-backed zero-copy loader, which reports the entry point itself.
    ZeroCopyInitramfsSlice {
        elf: &'a [u8],
        initrd_phys_base: u64,
        file_initrd_offset: u64,
    },
}

/// Everything one image-loading spawn needs that its caller has already established.
pub(crate) struct SpawnImageRequest<'a> {
    pub(crate) image_id: u64,
    pub(crate) image_path: &'static str,
    pub(crate) source: SpawnImageSource<'a>,
    pub(crate) class: TaskClass,
    pub(crate) parent_pid: u64,
    pub(crate) startup_args: [u64; 18],
    pub(crate) extra_send_caps: [u64; 4],
    /// NR 23 only: after loading, map the boot initrd read-only into the new address space and
    /// publish the user pointer/length in startup slots 15/16.
    pub(crate) map_initrd_window: bool,
    /// NR 26 only: emit the Stage 175 default-off `SPAWN_LIFECYCLE_*` phase markers. Kept
    /// per-caller rather than made universal so no other syscall's marker set changes.
    pub(crate) lifecycle_markers: bool,
}

/// What the caller writes into its reply frame.
pub(crate) struct SpawnImageCommitted {
    pub(crate) tid: u64,
    /// The spawned TID already narrowed to `usize`, computed BEFORE the commit so the only step
    /// after the task becomes live is writing the reply.
    pub(crate) reply_tid: usize,
    pub(crate) asid: Asid,
    pub(crate) packed_ret2: u64,
}

/// THE image-loading spawn transaction, shared by NR 23, NR 24, NR 26 and NR 29.
///
/// Phases, in ledger order:
///
/// 1. allocate the TID and RESERVE it (task rank 2);
/// 2. create the address space (VM rank 5);
/// 3. load the image into it, and — NR 23 only — map the initrd window;
/// 4. create the two service endpoints (IPC rank 3) and mint their capabilities (cap rank 4);
/// 5. delegate the parent's send capability;
/// 6. commit: consume the reservation and publish the live task.
///
/// Every failure in 1..5 unwinds the ledger to the transaction's baseline before returning, and
/// nothing reachable exists until 6 completes.
pub(crate) fn run_image_spawn_transaction(
    kernel: &mut KernelState,
    request: SpawnImageRequest<'_>,
) -> Result<SpawnImageCommitted, SyscallError> {
    let SpawnImageRequest {
        image_id,
        image_path,
        source,
        class,
        parent_pid,
        mut startup_args,
        extra_send_caps,
        map_initrd_window,
        lifecycle_markers,
    } = request;

    // ── Phase 1: the TID, then the reservation — the FIRST provisional resource. ──────────
    //
    // The TID itself is not a ledger entry. `allocate_thread_id` advances a monotonic cursor
    // that is deliberately never rewound: a TID is not reused, so there is nothing to restore.
    let tid = kernel.allocate_thread_id().map_err(|err| {
        crate::yarm_log!("KSPAWN_FAIL phase=allocate_tid err={:?}", err);
        SyscallError::from(err)
    })?;
    // Narrow the reply value NOW. After the commit the task is live, so no step that can still
    // fail may be left between the commit and the reply.
    let reply_tid = usize::try_from(tid).map_err(|_| SyscallError::Internal)?;

    let ledger = SpawnLedger::new();
    let outcome = kernel.reserve_task_for_spawn_with_class(tid, class);
    let (reservation, mut ledger) = advance(kernel, ledger, outcome, "reserve_task")?;
    ledger.record(ProvisionalSpawnResource::Reservation(reservation));

    // ── Phase 2+3: the child's whole image, through THE provisioner. ─────────────────────
    //
    // U9-SPAWN-VM1 replaced four separately-failing steps — create the address space, load the
    // ELF, map NR 23's initrd window, and (much later, past the commit) allocate the user stack —
    // with one `provision_spawn_image` call that owns them together and rolls all four back with
    // one exact unwind. Two consequences for this transaction:
    //
    //   * The failure arm is now the provisioner's. It destroys the address space AND revokes the
    //     capability naming it before returning, so nothing is recorded in the ledger on a failed
    //     provisioning — recording after the fact would double-release. The ledger records both
    //     resources only once the provisioning SUCCEEDED and ownership transferred here, and from
    //     then on it is what covers a later endpoint or commit failure.
    //   * The user stack now exists BEFORE the commit rather than inside it, which is what lets a
    //     stack-allocation failure roll back at all. `spawn_image_after_claim` consumes it through
    //     `UserImageSpec::provisioned_stack_top` instead of allocating its own.
    //
    // `create_user_address_space` returning TWO resources — the ASID and a MAP/READ/WRITE
    // capability over it minted into the CALLER's cspace — is the subtlety SP-3 surfaced: every
    // handler discarded that capability as `_aspace_cap`, and `destroy_user_address_space_by_asid`
    // does not own it, so a failed spawn leaked one on every arm.
    let zero_copy = matches!(source, SpawnImageSource::ZeroCopyInitramfsSlice { .. });
    let outcome = kernel.provision_spawn_image(
        tid,
        image_path,
        match source {
            SpawnImageSource::PtLoadSegments { elf, entry } => {
                ImageSource::PtLoadSegments { elf, entry }
            }
            SpawnImageSource::ZeroCopyInitramfsSlice {
                elf,
                initrd_phys_base,
                file_initrd_offset,
            } => ImageSource::ZeroCopyInitramfsSlice {
                image_id,
                elf,
                initrd_phys_base,
                file_initrd_offset,
            },
        },
        map_initrd_window,
        &mut startup_args,
        SPAWN_USER_STACK_PAGES,
    );
    let (provision, mut ledger) = advance(kernel, ledger, outcome, "provision_image")?;
    let ImageProvision {
        asid,
        aspace_cap,
        entry,
        stack_top,
        zc_pages,
        copied_pages,
    } = provision;
    ledger.record(ProvisionalSpawnResource::AddressSpace(asid));
    if let Some(cnode) = kernel.current_task_cnode() {
        ledger.record(ProvisionalSpawnResource::Capability {
            cnode,
            cap: aspace_cap,
        });
    }
    crate::yarm_log!("KSPAWN_ASID_OK tid={} asid={}", tid, asid.0);
    // The Stage 175 phase markers keep their exact relative order and spelling. They are all
    // emitted after the provisioning now rather than bracketing its steps, because the phases
    // they name happen inside one call — that is the only change, and the set is default-off.
    if lifecycle_markers {
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_ASPACE_CREATE_OK tid={} asid={}",
            tid,
            asid.0
        );
        crate::yarm_log!("SPAWN_LIFECYCLE_ELF_LOAD_BEGIN tid={} asid={}", tid, asid.0);
    }
    if zero_copy {
        crate::yarm_log!(
            "PM_ELF_ZC_DONE image_id={} path={} zc_pages={} copied_pages={}",
            image_id,
            image_path,
            zc_pages,
            copied_pages
        );
    }
    crate::yarm_log!("KSPAWN_LOAD_OK tid={}", tid);
    if lifecycle_markers {
        crate::yarm_log!("SPAWN_LIFECYCLE_ELF_LOAD_OK tid={} asid={}", tid, asid.0);
        crate::yarm_log!("SPAWN_LIFECYCLE_ZC_LOAD_OK tid={} asid={}", tid, asid.0);
    }

    // ── Phase 4: the two service endpoints and their capabilities. ───────────────────────
    //
    // U9-SPAWN-IC1: each endpoint and its two capabilities are now ONE transaction under IPC
    // rank 3 then capability rank 4, so a mint failure can no longer leave an endpoint installed
    // that nothing names. The ledger records the whole GRANT — index, generation, owning cspace
    // and both capability ids — rather than a bare index, which was not an identity: endpoint
    // slots are recycled, and a stale unwind naming a reused index would have destroyed a
    // replacement endpoint.
    //
    // The spawner's identity is resolved HERE, under task rank 2, before either owner is
    // acquired. The transaction body has no ambient current-task read of its own.
    let spawner_tid = current_tid(kernel).unwrap_or(0);
    let spawner_cnode = kernel.current_task_cnode();
    let endpoint_request = spawner_cnode.map(|owner_cnode| {
        let limits = kernel.runtime_capacity_config();
        ServiceEndpointRequest {
            owner_cnode,
            max_depth: SERVICE_ENDPOINT_DEPTH,
            mode: EndpointMode::Buffered,
            max_endpoints: limits.max_endpoints,
            cnode_limits: CnodeGrowthLimits {
                slot_capacity: crate::kernel::capabilities::MAX_CAPABILITIES_PER_CSPACE
                    .min(limits.max_capability_slots),
                max_total_cnode_slots: limits.max_total_cnode_slots,
            },
        }
    });

    // Endpoint creation and parent delegation stay TOLERANT of failure, exactly as before: the
    // spawn proceeds with capability id 0 and the child comes up without its service endpoint.
    // That is a pre-existing policy decision about what a spawn means, not a leak.
    let provision_endpoint = |kernel: &mut KernelState, ledger: &mut SpawnLedger| {
        let request = endpoint_request.as_ref()?;
        match kernel.provision_service_endpoint(request) {
            Ok(grant) => {
                ledger.record(ProvisionalSpawnResource::Endpoint(grant));
                Some(grant)
            }
            Err(e) => {
                crate::yarm_log!("KSPAWN_EP_CREATE_FAIL err={:?}", e);
                None
            }
        }
    };

    let (service_send_cap, service_recv_cap) = match provision_endpoint(kernel, &mut ledger) {
        Some(grant) => {
            crate::yarm_log!(
                "KSPAWN_EP_CREATED spawner_tid={} send_cap={} recv_cap={}",
                spawner_tid,
                grant.send_cap.0,
                grant.recv_cap.0
            );
            (grant.send_cap.0, grant.recv_cap.0)
        }
        None => (0u64, 0u64),
    };
    let service_reply_recv_cap = match provision_endpoint(kernel, &mut ledger) {
        Some(grant) => {
            crate::yarm_log!(
                "SPAWN_SERVICE_REPLY_RECV_CAP_CREATED endpoint={} cap={}",
                grant.endpoint_index,
                grant.recv_cap.0
            );
            grant.recv_cap.0
        }
        None => {
            crate::yarm_log!("KSPAWN_REPLY_EP_CREATE_FAIL err=endpoint_unavailable");
            0u64
        }
    };

    // ── Phase 5: delegate the parent's send capability. ──────────────────────────────────
    //
    // The delegated copy is a descendant of `service_send_cap`, whose grant the ledger already
    // owns, so releasing that grant removes this copy too through the revoke cascade.
    let caller_send_cap = if parent_pid != 0 && service_send_cap != 0 {
        match kernel.delegate_capability(
            spawner_tid,
            CapId(service_send_cap),
            parent_pid,
            CapRights::SEND,
        ) {
            Ok(grant) => {
                let cap = grant.dest_cap;
                ledger.record(ProvisionalSpawnResource::Delegation(grant));
                crate::yarm_log!(
                    "KSPAWN_PARENT_SEND_DELEGATED parent_tid={} cap={}",
                    parent_pid,
                    cap.0
                );
                cap.0
            }
            Err(e) => {
                crate::yarm_log!(
                    "KSPAWN_PARENT_SEND_DELEGATE_FAIL parent_tid={} err={:?}",
                    parent_pid,
                    e
                );
                service_send_cap
            }
        }
    } else {
        service_send_cap
    };

    // ── Phase 6: commit. The FIRST step that publishes a reachable task. ─────────────────
    crate::yarm_log!(
        "KSPAWN_BEFORE_SPAWN_TASK tid={} asid={} entry=0x{:x} parent_pid={} image_id={}",
        tid,
        asid.0,
        entry,
        parent_pid,
        image_id
    );
    let outcome = kernel.spawn_user_task_from_image(
        reservation,
        UserImageSpec {
            tid,
            entry,
            asid: Some(asid),
            class,
            startup_args,
            spawner_tid,
            service_recv_cap,
            service_reply_recv_cap,
            extra_send_caps,
            // Already allocated and mapped in `asid` by the provisioner, and already covered by
            // its rollback. The commit consumes it instead of allocating a second one.
            provisioned_stack_top: Some(stack_top),
        },
    );
    if let Err(err) = &outcome {
        // Preserved verbatim: this is the marker the live boot-blocker scanners look for.
        crate::yarm_log!(
            "KSPAWN_SPAWN_TASK_FAIL tid={} asid={} err={:?}",
            tid,
            asid.0,
            err
        );
    }
    // A failed spawn restores the exact incarnation the ledger's token names, so that token is
    // still the right one to cancel with.
    let (spawned, ledger) = advance(kernel, ledger, outcome, "spawn_user_task")?;
    ledger.commit();
    crate::yarm_log!("KSPAWN_TASK_READY tid={}", spawned.tid);
    if lifecycle_markers {
        // TIDs are monotonic, so a spawned service TID that regresses below a previously
        // observed one indicates a startup-order anomaly.
        use core::sync::atomic::{AtomicU64, Ordering};
        static LAST_SERVICE_TID: AtomicU64 = AtomicU64::new(0);
        let prev = LAST_SERVICE_TID.swap(spawned.tid, Ordering::Relaxed);
        if prev != 0 && spawned.tid < prev {
            crate::yarm_log!(
                "SPAWN_LIFECYCLE_SERVICE_ORDER_VIOLATION tid={} prev={}",
                spawned.tid,
                prev
            );
        }
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_SERVICE_READY tid={} image_id={}",
            spawned.tid,
            image_id
        );
    }

    let packed_ret2 =
        if parent_pid != 0 && service_send_cap != 0 && caller_send_cap != service_send_cap {
            ((service_send_cap as u64) << 32) | (caller_send_cap as u64)
        } else {
            caller_send_cap as u64
        };
    Ok(SpawnImageCommitted {
        tid: spawned.tid,
        reply_tid,
        asid,
        packed_ret2,
    })
}
