// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-SPAWN1 SP-3 — THE image-loading spawn transaction, and its provisional-resource ledger.
//!
//! # The defect this repairs
//!
//! All four image-loading spawn handlers — `SpawnProcess` (NR 23), `SpawnProcessFromUserBuf`
//! (NR 24), `SpawnFromInitramfsFile` (NR 26) and `SpawnFromMemoryObject` (NR 29) — acquired the
//! same resources in the same order and returned through a bare `?` at each step:
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
use crate::kernel::boot::{KernelError, KernelState, UserImageSpec};
use crate::kernel::capabilities::{CNodeId, CapId, CapRights};
use crate::kernel::spawn_reservation::SpawnReservationToken;
use crate::kernel::task::TaskClass;
use crate::kernel::vm::{Asid, CachePolicy, Mapping, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};

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
    AddressSpace(Asid),
    /// One endpoint object slot.
    Endpoint(usize),
    /// One capability minted into a named CNode — and, through the revoke cascade, every
    /// descendant delegated from it.
    Capability { cnode: CNodeId, cap: CapId },
}

/// The most provisional resources one image-loading spawn holds at once:
/// 1 reservation + 1 address space + 1 address-space capability + 2 endpoints
/// + 2×(send, recv) endpoint capabilities = 9.
///
/// The capability delegated into the parent's cspace needs no entry of its own: it is a
/// descendant of the service send capability, and revoking that root cascades to it.
pub(crate) const MAX_PROVISIONAL_SPAWN_RESOURCES: usize = 9;

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
                let ok = self.destroy_user_address_space_by_asid(asid).is_ok();
                crate::yarm_log!(
                    "SPAWN_LEDGER_RELEASE class=address_space asid={} ok={}",
                    asid.0,
                    u8::from(ok)
                );
            }
            ProvisionalSpawnResource::Endpoint(index) => {
                let ok = self.destroy_endpoint(index).is_ok();
                crate::yarm_log!(
                    "SPAWN_LEDGER_RELEASE class=endpoint index={} ok={}",
                    index,
                    u8::from(ok)
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

/// Map the boot initrd read-only into `asid` and publish it in startup slots 15/16.
///
/// Failure is TOLERATED, exactly as before: `initramfs_srv` falls back to its syscall bridge when
/// the window is absent, so a mapping failure must not fail the spawn. Pages installed before a
/// failure belong to `asid`, which the ledger already owns.
fn map_boot_initrd_window(kernel: &mut KernelState, asid: Asid, startup_args: &mut [u64; 18]) {
    const INITRD_USER_VA_BASE: u64 = 0x0C00_0000;
    let Some(initrd) = crate::kernel::boot::Bootstrap::boot_initrd_bytes() else {
        crate::yarm_log!("INITRAMFS_INITRD_MAP_SKIP reason=no_boot_initrd");
        return;
    };
    let initrd_virt_raw = initrd.as_ptr() as u64;
    let virt_base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE;
    let phys_base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_PHYS_BASE;
    let initrd_phys_raw = if virt_base > phys_base && initrd_virt_raw >= virt_base {
        match initrd_virt_raw
            .checked_sub(virt_base)
            .and_then(|off| phys_base.checked_add(off))
        {
            Some(phys) => phys,
            None => {
                crate::yarm_log!(
                    "INITRAMFS_INITRD_ADDR_INVALID raw_ptr=0x{:x} virt_base=0x{:x} phys_base=0x{:x}",
                    initrd_virt_raw,
                    virt_base,
                    phys_base
                );
                return;
            }
        }
    } else if initrd_virt_raw < virt_base || virt_base == phys_base {
        initrd_virt_raw
    } else {
        crate::yarm_log!(
            "INITRAMFS_INITRD_ADDR_INVALID raw_ptr=0x{:x} virt_base=0x{:x} phys_base=0x{:x}",
            initrd_virt_raw,
            virt_base,
            phys_base
        );
        return;
    };
    let initrd_len = initrd.len() as u64;
    let mut first6 = [0u8; 6];
    let first6_len = core::cmp::min(initrd.len(), first6.len());
    first6[..first6_len].copy_from_slice(&initrd[..first6_len]);
    crate::yarm_log!(
        "INITRAMFS_INITRD_SOURCE_RANGE raw_ptr=0x{:x} phys_start=0x{:x} len={}",
        initrd_virt_raw,
        initrd_phys_raw,
        initrd_len
    );
    crate::yarm_log!("INITRAMFS_INITRD_FIRST6 bytes={:?}", first6);
    let page: u64 = PAGE_SIZE as u64;
    let phys_start = initrd_phys_raw & !(page - 1);
    let phys_end = (initrd_phys_raw + initrd_len + page - 1) & !(page - 1);
    let pages_to_map = ((phys_end - phys_start) / page) as usize;
    let initrd_offset_in_first_page = initrd_phys_raw - phys_start;
    crate::yarm_log!(
        "INITRAMFS_INITRD_MAP_BEGIN phys_start=0x{:x} phys_end=0x{:x} len={} pages={}",
        phys_start,
        phys_end,
        initrd_len,
        pages_to_map
    );
    let initrd_flags = PageFlags {
        read: true,
        write: false,
        execute: false,
        user: true,
        cache_policy: CachePolicy::WriteBack,
    };
    for i in 0..pages_to_map {
        let virt = VirtAddr(INITRD_USER_VA_BASE + (i as u64) * page);
        let phys = PhysAddr(phys_start + (i as u64) * page);
        if let Err(e) = kernel.map_user_page_in_asid_raw(
            asid,
            virt,
            Mapping {
                phys,
                flags: initrd_flags,
            },
        ) {
            crate::yarm_log!(
                "INITRAMFS_INITRD_MAP_FAIL page={} virt=0x{:x} err={:?}",
                i,
                virt.0,
                e
            );
            return;
        }
    }
    let user_initrd_ptr = INITRD_USER_VA_BASE + initrd_offset_in_first_page;
    startup_args[15] = user_initrd_ptr;
    startup_args[16] = initrd_len;
    crate::yarm_log!(
        "INITRAMFS_INITRD_MAP_DONE user_ptr=0x{:x} len={} rights=ro",
        user_initrd_ptr,
        initrd_len
    );
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

    // ── Phase 2: the address space, and the capability that names it. ────────────────────
    //
    // `create_user_address_space` returns TWO resources, not one: the ASID, and a MAP/READ/WRITE
    // capability over it minted into the caller's cspace. Every handler discarded that
    // capability as `_aspace_cap`, and `destroy_user_address_space_by_asid` does not own it — it
    // frees the address space, not the caller's cspace slot naming it. So a failed spawn left a
    // capability behind on EVERY failure arm, including the earliest one. The SP-3 hosted
    // failure-injection proof is what surfaced it.
    let outcome = kernel.create_user_address_space();
    let ((asid, aspace_cap), mut ledger) = advance(kernel, ledger, outcome, "create_asid")?;
    ledger.record(ProvisionalSpawnResource::AddressSpace(asid));
    if let Some(cnode) = kernel.current_task_cnode() {
        ledger.record(ProvisionalSpawnResource::Capability {
            cnode,
            cap: aspace_cap,
        });
    }
    crate::yarm_log!("KSPAWN_ASID_OK tid={} asid={}", tid, asid.0);
    if lifecycle_markers {
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_ASPACE_CREATE_OK tid={} asid={}",
            tid,
            asid.0
        );
        crate::yarm_log!("SPAWN_LIFECYCLE_ELF_LOAD_BEGIN tid={} asid={}", tid, asid.0);
    }

    // ── Phase 3: the image. ──────────────────────────────────────────────────────────────
    let (entry, mut ledger) = match source {
        SpawnImageSource::PtLoadSegments { elf, entry } => {
            let outcome = kernel.load_elf_pt_load_segments(asid, elf);
            let (_loaded, ledger) = advance(kernel, ledger, outcome, "load_elf")?;
            (entry, ledger)
        }
        SpawnImageSource::ZeroCopyInitramfsSlice {
            elf,
            initrd_phys_base,
            file_initrd_offset,
        } => {
            let outcome = kernel.load_elf_with_mo_zero_copy(
                image_id,
                asid,
                elf,
                initrd_phys_base,
                file_initrd_offset,
            );
            let ((entry, _first_vaddr, _heap_base, zc_pages, copied_pages), ledger) =
                advance(kernel, ledger, outcome, "load_elf_zero_copy")?;
            crate::yarm_log!(
                "PM_ELF_ZC_DONE image_id={} path={} zc_pages={} copied_pages={}",
                image_id,
                image_path,
                zc_pages,
                copied_pages
            );
            (entry, ledger)
        }
    };
    crate::yarm_log!("KSPAWN_LOAD_OK tid={}", tid);
    if lifecycle_markers {
        crate::yarm_log!("SPAWN_LIFECYCLE_ELF_LOAD_OK tid={} asid={}", tid, asid.0);
        crate::yarm_log!("SPAWN_LIFECYCLE_ZC_LOAD_OK tid={} asid={}", tid, asid.0);
    }
    if map_initrd_window {
        map_boot_initrd_window(kernel, asid, &mut startup_args);
    }

    // ── Phase 4: the two service endpoints and their capabilities. ───────────────────────
    let spawner_tid = current_tid(kernel).unwrap_or(0);
    let spawner_cnode = kernel.current_task_cnode();
    let record_minted = |ledger: &mut SpawnLedger, cap: CapId| {
        if let Some(cnode) = spawner_cnode {
            ledger.record(ProvisionalSpawnResource::Capability { cnode, cap });
        }
    };
    let (service_send_cap, service_recv_cap) = match kernel.create_endpoint(8) {
        Ok((endpoint_idx, send_cap, recv_cap)) => {
            ledger.record(ProvisionalSpawnResource::Endpoint(endpoint_idx));
            record_minted(&mut ledger, send_cap);
            record_minted(&mut ledger, recv_cap);
            crate::yarm_log!(
                "KSPAWN_EP_CREATED spawner_tid={} send_cap={} recv_cap={}",
                spawner_tid,
                send_cap.0,
                recv_cap.0
            );
            (send_cap.0, recv_cap.0)
        }
        Err(e) => {
            crate::yarm_log!("KSPAWN_EP_CREATE_FAIL err={:?}", e);
            (0u64, 0u64)
        }
    };
    let service_reply_recv_cap = match kernel.create_endpoint(8) {
        Ok((endpoint_idx, send_cap, recv_cap)) => {
            ledger.record(ProvisionalSpawnResource::Endpoint(endpoint_idx));
            record_minted(&mut ledger, send_cap);
            record_minted(&mut ledger, recv_cap);
            crate::yarm_log!(
                "SPAWN_SERVICE_REPLY_RECV_CAP_CREATED endpoint={} cap={}",
                endpoint_idx,
                recv_cap.0
            );
            recv_cap.0
        }
        Err(e) => {
            crate::yarm_log!("KSPAWN_REPLY_EP_CREATE_FAIL err={:?}", e);
            0u64
        }
    };

    // ── Phase 5: delegate the parent's send capability. ──────────────────────────────────
    //
    // The delegated copy is a descendant of `service_send_cap`, which the ledger already owns,
    // so revoking that root on unwind removes this copy too.
    let caller_send_cap = if parent_pid != 0 && service_send_cap != 0 {
        match kernel.grant_capability_task_to_task_with_rights(
            spawner_tid,
            CapId(service_send_cap),
            parent_pid,
            CapRights::SEND,
        ) {
            Ok(cap) => {
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
