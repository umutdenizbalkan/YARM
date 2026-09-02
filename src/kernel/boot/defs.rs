// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

#[cfg(feature = "hosted-dev")]
pub(crate) type KernelStorage<T> = alloc::boxed::Box<T>;
#[cfg(not(feature = "hosted-dev"))]
pub(crate) type KernelStorage<T> = T;

#[cfg(feature = "hosted-dev")]
pub(crate) fn store_kernel_value<T>(value: T) -> KernelStorage<T> {
    alloc::boxed::Box::new(value)
}
#[cfg(not(feature = "hosted-dev"))]
pub(crate) fn store_kernel_value<T>(value: T) -> KernelStorage<T> {
    value
}

#[cfg(feature = "hosted-dev")]
pub(crate) fn kernel_ref<T>(value: &KernelStorage<T>) -> &T {
    value.as_ref()
}

#[cfg(not(feature = "hosted-dev"))]
pub(crate) fn kernel_ref<T>(value: &KernelStorage<T>) -> &T {
    value
}

#[cfg(feature = "hosted-dev")]
pub(crate) fn kernel_mut<T>(value: &mut KernelStorage<T>) -> &mut T {
    value.as_mut()
}

#[cfg(not(feature = "hosted-dev"))]
pub(crate) fn kernel_mut<T>(value: &mut KernelStorage<T>) -> &mut T {
    value
}

/// Discriminant for `MemoryObject` backing type.
/// Phase 3A adds `InitramfsFileSlice` to enable read-only page grants from
/// initramfs_srv to PM without a kernel-mediated cross-ASID copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryObjectKind {
    /// Anonymous memory backed by a contiguous physical frame allocation.
    Anonymous,
    /// Read-only slice of the boot initramfs CPIO, backed by the initrd mapping.
    /// `initrd_offset` is the byte offset of the file data within the initrd blob.
    /// `file_len` is the exact file data length (NOT rounded up).
    InitramfsFileSlice { initrd_offset: u64, file_len: u64 },
}

/// U9-MO2 — who owns the physical backing of a [`MemoryObject`], and therefore what releasing
/// one is allowed to do to the frame allocator.
///
/// This distinction was implicit and unstated, and every reclaim path acted as though it were
/// always [`Self::AllocatorOwned`]: they called `free_frame(object.phys)` with no reference to
/// the kind. That is latent corruption rather than a leak — an initramfs slice's physical
/// address is inside the boot initrd, memory the allocator never handed out, so freeing it
/// would insert someone else's memory into the free list. It has never fired only because no
/// `InitramfsFileSlice` object is reclaimed in production today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryBacking {
    /// The frame allocator handed this extent out and must get it back — exactly once, and
    /// exactly the extent that was taken.
    AllocatorOwned,
    /// The extent belongs to something else that outlives the object (the boot initrd blob).
    /// Releasing the object removes it from the registry and touches the allocator NEVER.
    Borrowed,
}

impl MemoryObjectKind {
    /// THE backing-ownership rule, stated once for every kind.
    ///
    /// Exhaustive by construction: there is no wildcard arm, so a new `MemoryObjectKind`
    /// variant fails to compile until somebody decides who owns its backing. That is the
    /// point — the previous model let a new kind silently inherit "allocator-owned" and be
    /// freed into an allocator that never owned it.
    pub(crate) const fn backing(self) -> MemoryBacking {
        match self {
            // Both constructors (`alloc_anonymous_memory_object_with_len` and the COW
            // private-copy route) take the frame from `frame_allocator` before wrapping it.
            Self::Anonymous => MemoryBacking::AllocatorOwned,
            // The only constructor is NR 28, whose phys is `initrd_phys_base + offset` rounded
            // down to a page. Nothing allocated it; the initrd outlives every object over it.
            Self::InitramfsFileSlice { .. } => MemoryBacking::Borrowed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemoryObject {
    pub(crate) id: u64,
    pub(crate) phys: PhysAddr,
    pub(crate) len: usize,
    pub(crate) cap_refcount: u32,
    pub(crate) map_refcount: u32,
    pub(crate) pin_refcount: u32,
    /// Backing type — distinguishes anonymous from initramfs file-slice objects.
    pub(crate) kind: MemoryObjectKind,
}

#[derive(Debug)]
pub(crate) struct NotificationObject {
    pub(crate) irq_queue: [u16; crate::kernel::ipc::MAX_ENDPOINT_DEPTH],
    pub(crate) head: usize,
    pub(crate) len: usize,
    pub(crate) max_depth: usize,
}

impl NotificationObject {
    pub(crate) fn new(max_depth: usize) -> Result<Self, KernelError> {
        if max_depth == 0 || max_depth > crate::kernel::ipc::MAX_ENDPOINT_DEPTH {
            return Err(KernelError::WrongObject);
        }
        Ok(Self {
            irq_queue: [0; crate::kernel::ipc::MAX_ENDPOINT_DEPTH],
            head: 0,
            len: 0,
            max_depth,
        })
    }

    pub(crate) fn send_irq(&mut self, irq_line: u16) -> Result<(), KernelError> {
        if self.len >= self.max_depth {
            return Err(KernelError::EndpointQueueFull);
        }
        let tail = (self.head + self.len) & (crate::kernel::ipc::MAX_ENDPOINT_DEPTH - 1);
        self.irq_queue[tail] = irq_line;
        self.len += 1;
        Ok(())
    }

    pub(crate) fn recv(&mut self) -> Option<Message> {
        if self.len == 0 {
            return None;
        }
        let irq_line = self.irq_queue[self.head];
        self.head = (self.head + 1) & (crate::kernel::ipc::MAX_ENDPOINT_DEPTH - 1);
        self.len -= 1;
        let payload = irq_line.to_le_bytes();
        Message::with_header(0, irq_line, 0, None, &payload).ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DriverRecord {
    pub(crate) tid: ThreadId,
    pub(crate) irq_caps: [Option<CapId>; MAX_DRIVER_IRQ_CAPS],
    pub(crate) dma_caps: [Option<CapId>; MAX_DRIVER_DMA_CAPS],
    pub(crate) dma_iova_base: Option<usize>,
    pub(crate) dma_iova_len: Option<usize>,
    pub(crate) iova_space_cap: Option<CapId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BrkRegionRecord {
    pub(crate) tid: ThreadId,
    pub(crate) base: VirtAddr,
    pub(crate) end: VirtAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CowPageRecord {
    pub(crate) asid: Asid,
    pub(crate) virt: VirtAddr,
}

/// Stage 198E3B2B2 — GENERATION-BEARING endpoint receive-waiter identity. The endpoint waiter slot
/// stores this complete identity (never a bare numeric `ThreadId`) so numeric TID reuse cannot let a
/// stale finalizer/cleanup claim, clear, or restore a REPLACEMENT task's waiter. The `asid` is the
/// receiver's captured address-space (its task incarnation discriminator): a replacement task that
/// reuses the numeric TID always carries a different ASID, so an exact `==` on this struct
/// distinguishes incarnations. All endpoint-receive-waiter authority (publish / claim / clear /
/// cleanup / restore) compares the FULL identity — numeric TID alone is never sufficient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReceiverWaiterIdentity {
    pub(crate) tid: ThreadId,
    pub(crate) asid: Asid,
}

impl ReceiverWaiterIdentity {
    pub(crate) fn new(tid: ThreadId, asid: Asid) -> Self {
        Self { tid, asid }
    }
}

/// Stage 199D-WA3C1 — the AUTHORITATIVE published endpoint receive-waiter.
///
/// `ReceiverWaiterIdentity` alone cannot distinguish incarnations: the SAME `{tid, asid}` may
/// block on the SAME endpoint, unblock, and block again. Carrying the receiver's
/// `blocked_recv_generation` IN the published record — rather than in a parallel array — gives
/// the waiter and its generation one lifetime, so there is no second field that can be updated a
/// moment later, or forgotten.
///
/// WA3C1 scope note: this record is what a future ownership `WaiterKey` will be derived from, but
/// **no ownership arming happens yet**. The waiter table remains the single authority for who is
/// parked on an endpoint, and last-receiver-wins replacement is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EndpointWaiterRecord {
    pub(crate) receiver: ReceiverWaiterIdentity,
    /// The receiver's `blocked_recv_generation` at the moment it committed THIS blocking receive.
    pub(crate) wait_generation: u64,
}

impl EndpointWaiterRecord {
    pub(crate) fn new(receiver: ReceiverWaiterIdentity, wait_generation: u64) -> Self {
        Self {
            receiver,
            wait_generation,
        }
    }

    pub(crate) fn tid(&self) -> ThreadId {
        self.receiver.tid
    }

    pub(crate) fn asid(&self) -> Asid {
        self.receiver.asid
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RobustFutexRecord {
    pub(crate) tid: ThreadId,
    pub(crate) state: RobustFutexState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransferEnvelope {
    pub(crate) source_tid: ThreadId,
    pub(crate) source_cap: CapId,
    pub(crate) source_object: CapObject,
    pub(crate) endpoint: CapObject,
    pub(crate) receiver_tid: Option<ThreadId>,
    pub(crate) state: TransferState,
    pub(crate) shared_region: Option<TransferSharedRegion>,
    pub(crate) generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransferSharedRegion {
    pub(crate) offset: u64,
    pub(crate) len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum TransferState {
    Created,
    MappedReceiver,
    MappedBoth,
    Released,
    Revoked,
}

impl TransferEnvelope {
    pub(crate) fn transition(self, next: TransferState) -> Option<Self> {
        use TransferState::*;
        let legal = matches!(
            (self.state, next),
            (Created, MappedReceiver)
                | (Created, Released)
                | (Created, Revoked)
                | (MappedReceiver, MappedBoth)
                | (MappedReceiver, Released)
                | (MappedReceiver, Revoked)
                | (MappedBoth, Released)
                | (MappedBoth, Revoked)
                | (Released, Revoked)
        );
        if !legal {
            return None;
        }
        Some(Self {
            state: next,
            ..self
        })
    }
}

/// A sender parked on an endpoint because its message could not be delivered or queued.
///
/// U6 §7 — the waiter carries the EXACT BLOCKING CYCLE, not merely a numeric TID.
///
/// Before U6 a waiter was `{tid, msg}`, which was enough while nothing ever completed a
/// blocked sender: the receiver simply woke the numeric TID. It is not enough now that
/// consumption and timeout must PUBLISH a completion, because a numeric TID alone cannot
/// distinguish:
///
/// * a replacement task that reused the numeric TID (a different incarnation — it always
///   carries a different ASID), from the sender that actually parked here; and
/// * this blocking cycle from a LATER one by the same incarnation (woken, re-blocked on the
///   same endpoint), which advanced `blocked_send_generation`.
///
/// Publishing a completion against either of those would hand one cycle's result to another
/// cycle's caller. Carrying `{asid, send_generation}` makes the completion's identity exactly
/// the identity the resume boundary revalidates, so a stale waiter can only ever be discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SenderWaiter {
    pub(crate) tid: ThreadId,
    pub(crate) msg: Message,
    /// The blocking sender's address-space id at block time. `None` only for a sender that
    /// had no bound ASID, which cannot be completed (no exact incarnation exists to name).
    pub(crate) asid: Option<Asid>,
    /// The sender's `blocked_send_generation` for THIS blocking cycle.
    pub(crate) send_generation: u64,
}

impl SenderWaiter {
    /// The exact blocking cycle this waiter was parked with, as the wake sites consume it.
    pub(crate) fn wake_target(&self) -> crate::kernel::ipc::SenderWakeTarget {
        crate::kernel::ipc::SenderWakeTarget {
            tid: self.tid,
            asid: self.asid,
            send_generation: self.send_generation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActiveTransferMapping {
    pub(crate) owner_tid: ThreadId,
    pub(crate) transfer_cap: CapId,
    pub(crate) base: VirtAddr,
    pub(crate) len: usize,
}

/// Stage 199A2B2 — reservation lifecycle of a single reply-record slot. This state
/// LIVES IN the existing `reply_caps` slot (it is a field of [`ReplyCapRecord`]),
/// so there is exactly ONE persistent reply authority store and ONE authoritative
/// generation (`reply_cap_generations`). `Vacant` is the slot being `None`; the
/// remaining states are carried by a present record:
///
/// * `Available` — externally invokable (the legacy, immediately-usable record and
///   the committed direct record). Only `Available` records resolve for `ipc_reply`.
/// * `Reserved` — held by an in-flight direct NR6 request transaction; NOT yet
///   externally invokable. `resolve_reply_index` rejects it (`StaleCapability`) so a
///   reply can never be delivered against a record whose server delivery + reply-cap
///   materialization have not committed consistently.
/// * `Consumed` — reserved for the NR7 one-shot reply completion (Stage 199A2B3);
///   also not invokable.
/// * `Cancelled` — a transient marker used during rollback immediately before the
///   slot is cleared to `Vacant`; not invokable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplyRecordReservation {
    Available,
    Reserved,
    Consumed,
    Cancelled,
}

impl ReplyRecordReservation {
    /// Only an `Available` record is externally invokable by `ipc_reply`.
    pub(crate) const fn is_invokable(&self) -> bool {
        matches!(self, Self::Available)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplyCapRecord {
    /// Stage 199A2B2 — the slot's reservation lifecycle (single-authority; see
    /// [`ReplyRecordReservation`]). A record is externally invokable ONLY when this
    /// is `Available`; a `Reserved` (in-flight direct transaction) record is never
    /// resolvable for `ipc_reply`.
    pub(crate) reservation: ReplyRecordReservation,
    pub(crate) caller_tid: ThreadId,
    /// Stage 199A2A — INCARNATION discriminator for the caller (blocked requester).
    /// Captured from `task_asid(caller_tid)` at record creation. A replacement task
    /// that reuses the numeric caller `ThreadId` always carries a DIFFERENT ASID, so
    /// cleanup keyed on `{caller_tid, caller_asid}` can never clear a fresh
    /// incarnation's record on a stale numeric-TID sweep. `Asid(0)` when the caller
    /// had no address space (kernel task) — cleanup then falls back to the numeric
    /// TID, which is the safe (aggressive-free) direction for a one-shot record.
    pub(crate) caller_asid: Asid,
    pub(crate) reply_endpoint: CapObject,
    pub(crate) responder_tid: Option<ThreadId>,
    /// Stage 199A2A — INCARNATION discriminator for the bound replier (responder).
    /// Captured from `task_asid(responder_tid)` at record creation (`None` when the
    /// record is unbound, i.e. `responder_tid == None`, or the responder had no
    /// address space). `ipc_reply` authorizes a reply ONLY when the CURRENT replier's
    /// `{tid, asid}` matches this bound identity — a numeric replier TID reused by a
    /// replacement task (different ASID) is rejected. Numeric TID alone never
    /// authorizes a reply delivery/wake.
    pub(crate) replier_asid: Option<Asid>,
    /// CapId of the Reply cap that `create_reply_cap_for_caller` minted into the
    /// **caller's** cnode.  Stored here so that `ipc_reply` (which runs in the
    /// **replier's** context) can also revoke it from the caller's cnode, preventing
    /// cnode slot exhaustion on the caller side over many repeated IPC cycles.
    pub(crate) caller_cap_id: CapId,
    /// CapId of the Reply cap that `complete_blocked_recv_for_waiter` (or the
    /// immediate recv path) minted into the **waiter/replier's** cnode when the
    /// FLAG_REPLY_CAP message was delivered.  Stored here so that `ipc_reply`
    /// can fast-revoke the exact slot using a kernel-controlled CapId rather
    /// than relying solely on the user-supplied reply_cap argument.
    ///
    /// `None` if materialization has not yet occurred (e.g. the message is still
    /// queued in the endpoint buffer and the receiver has not yet called recv).
    pub(crate) waiter_cap_id: Option<CapId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveTlbShootdownWait {
    pub(crate) sequence: u64,
    pub(crate) pending_cpu_bitmap: u64,
    pub(crate) requester_cpu: CpuId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveTlbShootdownState {
    pub(crate) next_sequence: u64,
    pub(crate) active: Option<LiveTlbShootdownWait>,
}

#[derive(Debug)]
pub(crate) struct IpcSubsystem {
    pub(crate) cross_cpu_work: SmpMailbox,
    pub(crate) live_tlb_shootdown: LiveTlbShootdownState,
    pub(crate) endpoints: [Option<KernelStorage<Endpoint>>; ENDPOINT_WAITER_SLOTS],
    /// The **authoritative endpoint receive-waiter table**. Its length is the structural
    /// bound on how many endpoint incarnations can simultaneously hold a blocked receiver —
    /// and therefore on how many direct-IPC acknowledgement leases can be outstanding at
    /// once, since a lease exists exactly while one of these slots does.
    /// [`crate::kernel::direct_ack_store::DIRECT_ACK_STORE_CAPACITY`] is derived from the
    /// same constant, so the two can never drift.
    pub(crate) endpoint_waiters: [Option<EndpointWaiterRecord>; ENDPOINT_WAITER_SLOTS],
    pub(crate) endpoint_sender_waiters:
        [[Option<SenderWaiter>; MAX_ENDPOINT_SENDER_WAITERS]; ENDPOINT_WAITER_SLOTS],
    pub(crate) endpoint_generations: [u64; ENDPOINT_WAITER_SLOTS],
    pub(crate) notifications: [Option<NotificationObject>; MAX_NOTIFICATIONS],
    pub(crate) notification_waiters: [Option<ThreadId>; MAX_NOTIFICATIONS],
    pub(crate) notification_generations: [u64; MAX_NOTIFICATIONS],
    pub(crate) irq_routes: [Option<usize>; MAX_IRQ_LINES],
    pub(crate) transfer_envelopes: [Option<TransferEnvelope>; MAX_TRANSFER_ENVELOPES],
    pub(crate) transfer_envelope_generations: [u64; MAX_TRANSFER_ENVELOPES],
    pub(crate) active_transfer_mappings: [Option<ActiveTransferMapping>; MAX_TRANSFER_ENVELOPES],
    pub(crate) reply_caps: [Option<ReplyCapRecord>; MAX_REPLY_CAPS],
    pub(crate) reply_cap_generations: [u64; MAX_REPLY_CAPS],
    /// Stage 200A — the SINGLE persistent terminal-ownership authority store,
    /// co-located with (and indexed identically to) `reply_caps`. Every terminal
    /// outcome of a caller blocked on its reply endpoint — reply, timeout, peer
    /// death, caller exit and endpoint destruction — claims through the SAME cell
    /// for its record slot (`authority_stores = 1`; there is NO second timeout or
    /// peer-death table). The accepted NR7 reply reservation is expressed as one
    /// terminal claimant, not a competing authority. Dormant in production this
    /// stage (no live path arms it yet — see the Stage 200A hosted mechanism seal);
    /// later stages wire the live reserve/reply/timeout/exit paths onto it.
    pub(crate) reply_terminal_ownership:
        [crate::kernel::terminal_ownership::TerminalCell; MAX_REPLY_CAPS],
    /// Stage 200B — the SINGLE bounded deadline-registration store
    /// (`deadline_registration_stores = 1`). It tracks registration ownership for a
    /// blocked reply receive and mints generation-bearing fire tokens, but it is NOT
    /// a terminal-result authority: a fire owner must still win the co-located
    /// `reply_terminal_ownership` cell's timeout claim. Dormant in production this
    /// stage (no live timer arms it); later stages wire a real deadline queue onto it.
    pub(crate) reply_deadline_tokens:
        [crate::kernel::deadline_token::DeadlineTokenCell; MAX_DEADLINE_TOKENS],
    /// Stage 198E2A1: bounded generation-bearing cancellation requests for in-flight shared-region
    /// transactions (executor-owned cleanup protocol). Not a queue/CNode/ABI capacity — an internal
    /// signal table matched by (receiver TID **and** ASID).
    pub(crate) shared_region_cancel_requests:
        [Option<SharedRegionCancelReq>; MAX_SHARED_REGION_CANCEL_REQUESTS],
    /// Stage 198E2B: FAIL-CLOSED latch. Set when a cancellation request cannot be recorded (the
    /// table is full and no stale entry can be evicted). While set, every executor checkpoint treats
    /// cancellation as authoritative, so NO transaction can map further, write back, publish, or wake
    /// after an unrecordable cancellation — silent cancellation loss is impossible. It is a PERMANENT
    /// per-kernel-instance safety fuse: it never auto-clears, because the cancellation that overflowed
    /// was never recorded, so clearing the latch could let that receiver publish (silent loss). Reset
    /// only with the whole IpcState at kernel init.
    pub(crate) shared_region_cancel_overflow: bool,
    /// Stage 199D-WA2A-R1 — the SINGLE endpoint-waiter ownership table
    /// (`waiter_ownership_stores = 1`), indexed identically to `endpoint_waiters` and bounded by
    /// the same [`ENDPOINT_WAITER_SLOTS`], so the two can never drift.
    ///
    /// Deliberately **not** `pub(crate)`: `pub(in crate::kernel::boot)` is the tightest
    /// visibility Rust can express for a field declared here that the ownership module must also
    /// reach (`pub(in …)` requires an ancestor module, and `boot` is the nearest common one).
    /// The type carries no usable API outside
    /// [`crate::kernel::boot::waiter_ownership`] regardless — every claim/settle method is
    /// module-private there, and the only cross-module surface is the typed
    /// `IpcSubsystem::waiter_ownership_*` methods. Helper-only this stage: no production path
    /// claims through it yet, so it stays empty in every live build.
    pub(in crate::kernel::boot) waiter_ownership: super::waiter_ownership::WaiterOwnershipTable,
    pub(crate) telemetry: IpcPathTelemetry,
}

/// Stage 198E2A1: a generation-bearing cancellation request for a shared-region direct transaction.
/// Matched on BOTH the numeric receiver TID and the captured ASID, so a delayed lifecycle action
/// for an old TID cannot cancel a replacement process's transaction (different ASID).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SharedRegionCancelReq {
    pub(crate) tid: u64,
    pub(crate) asid: crate::kernel::vm::Asid,
}

pub(crate) const MAX_SHARED_REGION_CANCEL_REQUESTS: usize = 4;

#[cfg(feature = "hosted-dev")]
pub(crate) type UserMemoryStore = BTreeMap<(u16, u64), u8>;

#[derive(Debug)]
pub(crate) struct MemorySubsystem {
    #[cfg(feature = "hosted-dev")]
    pub(crate) user_memory: KernelStorage<UserMemoryStore>,
    pub(crate) memory_objects: [Option<MemoryObject>; MAX_MEMORY_OBJECTS],
    pub(crate) brk_regions: [Option<BrkRegionRecord>; MAX_TASKS],
    pub(crate) cow_pages: alloc::collections::BTreeMap<u16, alloc::collections::BTreeSet<u64>>,
    #[cfg(test)]
    pub(crate) cow_page_capacity_limit: Option<usize>,
    pub(crate) next_memory_object_id: u64,
    pub(crate) frame_allocator: KernelStorage<PhysicalFrameAllocator>,
}

#[derive(Debug)]
pub(crate) struct DriverSubsystem {
    pub(crate) driver_records: [Option<DriverRecord>; MAX_DRIVERS],
    pub(crate) next_iova_space_id: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct CNodeSpace {
    pub(crate) id: CNodeId,
    pub(crate) slot_capacity: usize,
    pub(crate) cspace: KernelStorage<CapabilitySpace>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessCNodeRecord {
    pub(crate) pid: u64,
    pub(crate) cnode: CNodeId,
}

#[derive(Debug)]
pub(crate) struct FaultSubsystem {
    pub(crate) last_fault: Option<FaultInfo>,
    pub(crate) last_fault_frame: Option<TrapFrame>,
    pub(crate) fault_handler_endpoint: Option<usize>,
    pub(crate) supervisor_endpoint: Option<usize>,
    /// Stage 77+78: kernel-side endpoint index for delivering task-exit events to PM.
    /// Registered via `set_pm_task_exit_endpoint_for_task`. `None` until wired.
    pub(crate) pm_task_exit_endpoint: Option<usize>,
    pub(crate) fault_policy: FaultPolicy,
}

#[derive(Debug)]
pub(crate) struct RestartSubsystem {
    pub(crate) next_restart_token: u64,
}

#[derive(Debug)]
pub(crate) struct CapabilitySubsystem {
    pub(crate) cnode_spaces: KernelStorage<[Option<CNodeSpace>; MAX_TASKS]>,
    pub(crate) process_cnodes: KernelStorage<[Option<ProcessCNodeRecord>; MAX_TASKS]>,
    pub(crate) delegated_capability_links:
        KernelStorage<[Option<DelegatedCapabilityLink>; MAX_DELEGATED_CAPABILITY_LINKS]>,
}

#[derive(Debug)]
pub(crate) struct TelemetrySubsystem {
    pub(crate) tlb_shootdown_count: u64,
    pub(crate) tlb_shootdown_timeout_count: u64,
    pub(crate) tid_allocation: TidAllocationTelemetry,
    /// Stage 114 / D-NEXT-2: counts invocations of the genuinely pre-`with_cpu`
    /// VmBrk-shrink split path (`SharedKernel::try_split_vm_brk_shrink_into_frame`).
    /// Lives here (rank 10, telemetry) rather than in `ipc.telemetry` (rank 3)
    /// specifically so the split path never needs an ipc-domain seam to record
    /// it — `with_telemetry_split_mut` already exists and acquires only the
    /// telemetry lock. Distinct from `ipc.telemetry.d3_vm_brk_shrink_calls`,
    /// which the unchanged global-lock `vm_brk_shrink_two_phase` path still
    /// increments for every shrink it services (including the ones the split
    /// path defers, e.g. multi-CPU-online).
    pub(crate) d3_vm_brk_shrink_split_live_calls: u64,
    pub(crate) d3_vm_brk_shrink_split_live_pages_unmapped: u64,
}

#[derive(Debug)]
pub(crate) struct BootConfigSubsystem {
    pub(crate) capacity_profile: KernelCapacityProfile,
}

#[derive(Debug)]
pub(crate) struct SchedulerState {
    pub(crate) scheduler: KernelStorage<SmpScheduler>,
    pub(crate) timer: Timer,
    pub(crate) current_cpu: CpuId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DelegatedCapabilityLink {
    pub(crate) source_tid: u64,
    pub(crate) source_cap: CapId,
    pub(crate) dest_tid: u64,
    pub(crate) dest_cap: CapId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DelegatedCapRef {
    pub(crate) pid: u64,
    pub(crate) cap: CapId,
}

pub(crate) fn map_scheduler_error(err: SchedulerError) -> KernelError {
    match err {
        SchedulerError::QueueFull => KernelError::SchedulerFull,
        SchedulerError::InvalidCpu | SchedulerError::CpuOffline | SchedulerError::WakeOnly => {
            KernelError::WrongObject
        }
        SchedulerError::AlreadyQueued => KernelError::WouldBlock,
    }
}

pub(crate) fn map_ipc_error(err: IpcError) -> KernelError {
    match err {
        IpcError::EndpointFull => KernelError::EndpointQueueFull,
        IpcError::PayloadTooLarge
        | IpcError::MissingCapTransferFlag
        | IpcError::InconsistentCapTransferFlag
        | IpcError::InvalidEndpointDepth => KernelError::WrongObject,
    }
}
