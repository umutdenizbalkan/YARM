// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

/// SHARED-REGION-LIFETIME1 — the shared-region oracle object's id, published once at provisioning
/// so the live lifetime witness can read that ONE object's counters back from the kernel's own
/// accounting. `u64::MAX` until provisioned. Witness-only: compiled solely into the oracle kernel
/// feature, and it only reports — no counter is read from it.
#[cfg(all(feature = "shared-region-direct-oracle", not(feature = "hosted-dev")))]
pub(crate) static SHARED_REGION_ORACLE_OBJECT_ID: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(u64::MAX);

/// Report the oracle object's counters after `op` changed them at `phys`. A no-op for every other
/// object and in every build without the oracle feature.
#[inline]
pub(crate) fn trace_oracle_object_refs(_object: &MemoryObject, _op: &str, _phys: PhysAddr) {
    #[cfg(all(feature = "shared-region-direct-oracle", not(feature = "hosted-dev")))]
    if _object.id == SHARED_REGION_ORACLE_OBJECT_ID.load(core::sync::atomic::Ordering::Relaxed) {
        crate::yarm_log!(
            "SHARED_REGION_ORACLE_REFS op={} obj_id={} off=0x{:x} cap={} map={} pin={}",
            _op,
            _object.id,
            _phys.0.wrapping_sub(_object.phys.0),
            _object.cap_refcount,
            _object.map_refcount,
            _object.pin_refcount
        );
    }
}

/// 199G-C §2 — proof that ONE transfer-object pin was acquired, and the authority to release
/// exactly that one.
///
/// Unforgeable: the field is private to this module and the only constructor is
/// [`KernelState::acquire_transfer_pin_locked`], which mints a token only after it has actually
/// incremented the refcount. So a token cannot name a pin that was never taken, and a release
/// driven by a token cannot underflow a counter this acquire never raised.
///
/// It is `Copy` for the same reason every other by-value proof in the split transactions is:
/// the transaction records must be snapshottable. Single-use is enforced by the ENVELOPE
/// lifecycle — an envelope owes exactly one pin and is consumed exactly once — not by move
/// semantics, which is the same discipline `take_transfer_envelope`'s `-1` has always relied on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransferPinToken {
    object: CapObject,
}

impl TransferPinToken {
    /// The exact object whose pin this token releases.
    #[must_use]
    pub(crate) const fn object(self) -> CapObject {
        self.object
    }
}

/// 199G-C §2 — why an acquire refused. Every variant is raised BEFORE any mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransferPinRefusal {
    /// Not a pinnable class. `MemoryObject` and `DmaRegion` are the only objects that carry a
    /// `pin_refcount`; every other capability class reaches the pin path as a no-op, which is
    /// why this is a distinct answer from a failure.
    NotPinnable,
    /// No live memory object carries this id any more — a stale or already-reclaimed object.
    Stale,
    /// The refcount is at `u32::MAX`. The broad path SATURATES here, silently returning a pin
    /// the matching release would then under-drop; the split acquire refuses instead, before
    /// touching the counter.
    Overflow,
}

impl KernelState {
    pub(crate) fn memory_object_slot_by_id(&self, id: u64) -> Option<usize> {
        self.with_memory_state(|memory| {
            memory
                .memory_objects
                .iter()
                .position(|entry| entry.is_some_and(|mem| mem.id == id))
        })
    }

    pub(crate) fn adjust_memory_object_cap_refcount(&mut self, object: CapObject, delta: i32) {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return,
        };
        let Some(slot) = self.memory_object_slot_by_id(id) else {
            return;
        };
        self.with_memory_state_mut(|memory| {
            if let Some(memory_object) = memory.memory_objects[slot].as_mut() {
                if delta > 0 {
                    memory_object.cap_refcount =
                        memory_object.cap_refcount.saturating_add(delta as u32);
                } else {
                    memory_object.cap_refcount =
                        memory_object.cap_refcount.saturating_sub((-delta) as u32);
                }
            }
        });
    }

    /// U9-D3 §6: `&mut MemorySubsystem` sibling of [`Self::adjust_memory_object_cap_refcount`] for
    /// use inside `SharedKernel::with_memory_split_mut` (rank 6 only). Byte-identical semantics —
    /// the broad form is three `with_memory_state(_mut)` cycles (slot lookup, then mutate) and this
    /// is the same find-by-id → saturating adjust with the one acquisition the seam already holds.
    /// `adjust_memory_object_cap_refcount` itself is left unmodified.
    pub(crate) fn adjust_memory_object_cap_refcount_locked(
        memory: &mut MemorySubsystem,
        object: CapObject,
        delta: i32,
    ) {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return,
        };
        if let Some(slot) = memory
            .memory_objects
            .iter()
            .position(|entry| entry.is_some_and(|mem| mem.id == id))
            && let Some(memory_object) = memory.memory_objects[slot].as_mut()
        {
            if delta > 0 {
                memory_object.cap_refcount =
                    memory_object.cap_refcount.saturating_add(delta as u32);
            } else {
                memory_object.cap_refcount =
                    memory_object.cap_refcount.saturating_sub((-delta) as u32);
            }
        }
    }

    /// U9-MO2 — THE release of one MemoryObject slot whose refcounts are already zero.
    ///
    /// Every reclaim path funnels here, so the backing-ownership rule is applied once instead
    /// of four times. Before this existed all four sites called `free_frame(object.phys)`
    /// unconditionally, which is correct only for allocator-owned backing:
    ///
    /// * `AllocatorOwned` — return the EXACT extent that was taken. The constructors allocate
    ///   `len / PAGE_SIZE` pages contiguously, so that is what is returned. The previous
    ///   `free_frame` returned a single page and under-freed every multi-page object.
    /// * `Borrowed` — remove the registry slot and touch the allocator NEVER. An initramfs
    ///   slice's phys is inside the boot initrd; handing it to the allocator would insert
    ///   memory it never owned into the free list.
    ///
    /// The caller has already verified exact identity and all three zero refcounts; this
    /// performs the release and nothing else, so the "may I?" and "do it" halves stay separate.
    pub(crate) fn release_memory_object_slot_locked(
        memory: &mut MemorySubsystem,
        slot_index: usize,
    ) {
        let Some(object) = memory.memory_objects[slot_index] else {
            return;
        };
        match object.kind.backing() {
            crate::kernel::boot::MemoryBacking::AllocatorOwned => {
                let pages = object.len / crate::kernel::vm::PAGE_SIZE;
                if pages > 0 {
                    let _ = kernel_mut(&mut memory.frame_allocator)
                        .free_contiguous(object.phys.0, pages);
                }
            }
            // Borrowed: the extent outlives this object and belongs to something else.
            crate::kernel::boot::MemoryBacking::Borrowed => {}
        }
        memory.memory_objects[slot_index] = None;
    }

    /// U9-D3 §6: `&mut MemorySubsystem` sibling of [`Self::reclaim_memory_object_if_unreferenced`]
    /// for use inside `SharedKernel::with_memory_split_mut` (rank 6 only). Same class gate, same
    /// all-three-refcounts-zero condition, same `free_frame` + slot clear.
    /// `reclaim_memory_object_if_unreferenced` itself is left unmodified.
    pub(crate) fn reclaim_memory_object_if_unreferenced_locked(
        memory: &mut MemorySubsystem,
        object: CapObject,
    ) {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return,
        };
        let Some(slot_index) = memory
            .memory_objects
            .iter()
            .position(|entry| entry.is_some_and(|mem| mem.id == id))
        else {
            return;
        };
        let Some(memory_object) = memory.memory_objects[slot_index] else {
            return;
        };
        if memory_object.cap_refcount != 0
            || memory_object.map_refcount != 0
            || memory_object.pin_refcount != 0
        {
            return;
        }
        Self::release_memory_object_slot_locked(memory, slot_index);
    }

    pub(crate) fn adjust_memory_object_pin_refcount(&mut self, object: CapObject, delta: i32) {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return,
        };
        let Some(slot) = self.memory_object_slot_by_id(id) else {
            return;
        };
        self.with_memory_state_mut(|memory| {
            if let Some(memory_object) = memory.memory_objects[slot].as_mut() {
                if delta > 0 {
                    memory_object.pin_refcount =
                        memory_object.pin_refcount.saturating_add(delta as u32);
                } else {
                    memory_object.pin_refcount =
                        memory_object.pin_refcount.saturating_sub((-delta) as u32);
                }
            }
        });
    }

    /// SHARED-REGION-LIFETIME1 — THE owner of a mapped frame: the slot of the live
    /// ALLOCATOR-OWNED MemoryObject whose extent `[phys, phys + len)` contains the frame.
    ///
    /// Every mapping reference, every displaced-page hold and every reclaim-by-address asks
    /// this, and nothing else, so the acquire, the release and the reclaim of one page always
    /// name the same object.
    ///
    /// * **Containment, not the base address.** A mapping may name any page of an object — a
    ///   shared-region selection starts wherever its object-relative offset says, and a
    ///   multi-page grant's pages 1.. are interior at offset 0 too. Before this, every owner asked
    ///   `object.phys == phys`, so an interior page's mapping took no reference, a teardown
    ///   freed it as if nobody owned it, and its displacement could not be held.
    /// * **Unique.** An allocator-owned extent is one `alloc_contiguous` run, and the frame
    ///   allocator never issues a frame that is still charged to a live extent, so live
    ///   allocator-owned extents are disjoint and at most one contains a frame. Object ids are
    ///   never reused (`next_memory_object_id` only advances), so a hold that names an id can
    ///   never be mistaken for a later object at the same address.
    /// * **Borrowed backing is not owned here.** An initramfs file slice borrows the boot
    ///   initrd: nothing frees it, so no mapping needs a reference to keep it, and the address
    ///   alone cannot name ONE slice — two slices over the same file, or adjacent files that share
    ///   a page, overlap, and the zero-copy loader maps initrd pages that may predate any slice
    ///   over them. A borrowed slice's registry slot therefore lives by its capabilities and pins
    ///   only.
    pub(crate) fn frame_owner_slot_locked(
        memory: &MemorySubsystem,
        phys: PhysAddr,
    ) -> Option<usize> {
        memory.memory_objects.iter().position(|entry| {
            entry.is_some_and(|object| {
                matches!(
                    object.kind.backing(),
                    crate::kernel::boot::MemoryBacking::AllocatorOwned
                ) && Self::object_extent_contains(&object, phys)
            })
        })
    }

    /// Whether `phys` lies inside `object`'s backing extent.
    pub(crate) fn object_extent_contains(object: &MemoryObject, phys: PhysAddr) -> bool {
        phys.0
            .checked_sub(object.phys.0)
            .is_some_and(|off| off < object.len as u64)
    }

    /// Whether ANY live object — allocator-owned or borrowed — describes the frame at `phys`.
    /// Teardown's "nobody owns this, return it to the allocator" decision asks this: a frame
    /// inside any object's extent belongs to that object's lifecycle, never to the drained
    /// address space.
    pub(crate) fn frame_described_by_object_locked(
        memory: &MemorySubsystem,
        phys: PhysAddr,
    ) -> bool {
        memory
            .memory_objects
            .iter()
            .flatten()
            .any(|object| Self::object_extent_contains(object, phys))
    }

    pub(crate) fn note_mapping_inserted(&mut self, phys: PhysAddr) {
        self.with_memory_state_mut(|memory| Self::note_mapping_inserted_locked(memory, phys));
    }

    /// Stage 198E3B2A: `&mut MemorySubsystem` sibling of [`Self::note_mapping_inserted`] for use
    /// inside `SharedKernel::with_memory_split_mut` (rank 6 only). SHARED-REGION-LIFETIME1: one
    /// reference per mapped PAGE, taken on the page's owner ([`Self::frame_owner_slot_locked`]).
    pub(crate) fn note_mapping_inserted_locked(memory: &mut MemorySubsystem, phys: PhysAddr) {
        if let Some(slot) = Self::frame_owner_slot_locked(memory, phys)
            && let Some(memory_object) = memory.memory_objects[slot].as_mut()
        {
            memory_object.map_refcount = memory_object.map_refcount.saturating_add(1);
            trace_oracle_object_refs(memory_object, "map", phys);
        }
    }

    /// Stage 198E3B2A: `&mut MemorySubsystem` sibling of [`Self::adjust_memory_object_pin_refcount`]
    /// for use inside `SharedKernel::with_memory_split_mut` (rank 6 only). Byte-identical semantics.
    pub(crate) fn adjust_memory_object_pin_refcount_locked(
        memory: &mut MemorySubsystem,
        object: CapObject,
        delta: i32,
    ) {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return,
        };
        if let Some(slot) = memory
            .memory_objects
            .iter()
            .position(|entry| entry.is_some_and(|mem| mem.id == id))
            && let Some(memory_object) = memory.memory_objects[slot].as_mut()
        {
            if delta > 0 {
                memory_object.pin_refcount =
                    memory_object.pin_refcount.saturating_add(delta as u32);
            } else {
                memory_object.pin_refcount =
                    memory_object.pin_refcount.saturating_sub((-delta) as u32);
            }
        }
    }

    /// 199G-C §2 — THE rank-6 pin ACQUIRE, and the exact counterpart of the `-1` that
    /// `take_transfer_envelope` and `sr_release_pin_split` perform.
    ///
    /// Reachable through `SharedKernel::with_memory_split_mut` (rank 6 only), exactly as the
    /// release already is. What it does, in full:
    ///
    /// 1. classify the object — only `MemoryObject` / `DmaRegion` carry a `pin_refcount`;
    /// 2. revalidate that exact object id against the live memory table;
    /// 3. CHECKED increment;
    /// 4. mint the release authority.
    ///
    /// What it does NOT do, and what makes the AI_AGENT_RULES §14.4 D3 fence inapplicable: no
    /// page-table read or write, no map or unmap, no TLB operation, no frame allocation or
    /// reclaim, no address-space or cross-address-space inspection, and no scheduler / IPC /
    /// capability mutation while rank 6 is held. It moves one `u32` in `MemorySubsystem` and
    /// nothing else. The fence governs a seam that must reach a shootdown before reclaiming a
    /// frame; a pin raises a refcount and reclaims nothing, so there is no shootdown for it to
    /// owe. (It is also not a §14.5 live-wiring: `with_memory_split_mut` was already live for
    /// this very counter in the release direction.)
    ///
    /// The one deliberate divergence from the broad `+1` is the overflow answer. The broad path
    /// `saturating_add`s, so at `u32::MAX` it hands back a pin the matching `-1` would then
    /// under-drop; this refuses before mutation instead. Refusing is the fail-safe direction —
    /// the send fails and no envelope is created — whereas saturating silently breaks the
    /// pairing.
    pub(crate) fn acquire_transfer_pin_locked(
        memory: &mut MemorySubsystem,
        object: CapObject,
    ) -> Result<TransferPinToken, TransferPinRefusal> {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return Err(TransferPinRefusal::NotPinnable),
        };
        let slot = memory
            .memory_objects
            .iter()
            .position(|entry| entry.is_some_and(|mem| mem.id == id))
            .ok_or(TransferPinRefusal::Stale)?;
        let memory_object = memory.memory_objects[slot]
            .as_mut()
            .ok_or(TransferPinRefusal::Stale)?;
        // Checked, and checked BEFORE the store: on refusal the counter is untouched.
        let next = memory_object
            .pin_refcount
            .checked_add(1)
            .ok_or(TransferPinRefusal::Overflow)?;
        memory_object.pin_refcount = next;
        Ok(TransferPinToken { object })
    }

    /// 199G-C §2 — the release half driven by a token, so a release cannot name an object no
    /// acquire ever pinned. Delegates to the SAME counter update the broad `-1` uses; the token
    /// only decides *which* object, never *whether*.
    pub(crate) fn release_transfer_pin_locked(
        memory: &mut MemorySubsystem,
        token: TransferPinToken,
    ) {
        Self::adjust_memory_object_pin_refcount_locked(memory, token.object(), -1);
    }

    /// 199G-C §2 — THE pin policy, shared by the broad and split envelope stashes.
    ///
    /// A transfer envelope owes exactly one object pin **iff it carries a shared-region
    /// descriptor** — not because it carries a transfer capability. Every other transfer class
    /// (an ordinary Endpoint or Notification cap, a Reply cap, and even a `MemoryObject` /
    /// `DmaRegion` cap sent with a payload small enough to take an inline arm) creates an
    /// envelope with no pin at all. `stash_transfer_envelope`'s `+1` and
    /// `take_transfer_envelope`'s `-1` are both already spelled this way; naming it once is what
    /// stops the split stash from drifting into pinning "all transfer caps".
    #[must_use]
    pub(crate) const fn transfer_envelope_owes_pin(
        shared_region: Option<TransferSharedRegion>,
    ) -> bool {
        shared_region.is_some()
    }

    /// Stage 198E3B2A: `&MemorySubsystem` physical-base lookup of a frozen shared-region object for
    /// use inside `SharedKernel::with_memory_split_mut` (rank 6 only).
    ///
    /// SHARED-REGION-OFFSET1: no mapping path asks this any more — every receive family resolves
    /// each page through [`Self::shared_region_page_phys_locked`], which applies the descriptor's
    /// object offset. What remains are the witness provisioners that fill or zero an object they
    /// just created, and the hosted proofs, so it is compiled only where one of them is.
    #[cfg(any(
        test,
        all(
            not(feature = "hosted-dev"),
            any(
                feature = "shared-region-direct-oracle",
                feature = "riscv-uart-irq-witness",
                feature = "aarch64-pl011-irq-witness",
                feature = "x86_64-uart-irq-witness"
            )
        )
    ))]
    pub(crate) fn shared_region_phys_base_locked(
        memory: &MemorySubsystem,
        object: CapObject,
    ) -> Option<PhysAddr> {
        let (id, offset) = match object {
            CapObject::MemoryObject { id } => (id, 0u64),
            CapObject::DmaRegion { id, offset, .. } => (id, offset),
            _ => return None,
        };
        memory
            .memory_objects
            .iter()
            .flatten()
            .find(|e| e.id == id)
            .map(|e| PhysAddr(e.phys.0 + offset))
    }

    /// SHARED-REGION-OFFSET1 — the byte length of the object a `MemoryObject` or `DmaRegion`
    /// capability is backed by, or `None` for a stale object or any other capability class. The
    /// one lookup the range policy needs that a caller cannot compute.
    pub(crate) fn shared_region_object_len_locked(
        memory: &MemorySubsystem,
        object: CapObject,
    ) -> Option<usize> {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return None,
        };
        memory
            .memory_objects
            .iter()
            .flatten()
            .find(|e| e.id == id)
            .map(|e| e.len)
    }

    /// BL1 / SHARED-REGION-OFFSET1 — THE frame behind one page of a shared region: the page
    /// `page_offset` bytes into the selection `region` names, resolved against the backing of the
    /// `MemoryObject` or `DmaRegion` capability `object`.
    ///
    /// A `MemoryObject` is ONE physical range, `[phys, phys + len)`: the record carries a single
    /// base and a single length and no per-page frame list, and every constructor produces such a
    /// range (`alloc_contiguous(pages)` for anonymous objects, one slice of the physically
    /// contiguous initrd for initramfs objects, a single frame for a COW private copy). That is
    /// the representation guarantee this offset arithmetic relies on. `region.offset` is
    /// OBJECT-relative, so the frame is `phys + region.offset + page_offset` for both capability
    /// classes — a `DmaRegion` window bounds the selection, it does not rebase it.
    ///
    /// Every call re-applies the WHOLE range policy ([`shared_region_selection`]) to the WHOLE
    /// selection, not just to this page, so a receive loop's first page already refuses a
    /// selection any later page of which would fall outside the authorized backing — nothing is
    /// installed for a selection that cannot be installed completely. Refuses, rather than
    /// computing a frame outside the authority the capability conveys: a page offset that is not
    /// page-aligned or lies at or past the page-rounded selection's end, a selection the range
    /// policy refuses, a stale object, or any other capability class.
    pub(crate) fn shared_region_page_phys_locked(
        memory: &MemorySubsystem,
        object: CapObject,
        region: TransferSharedRegion,
        page_offset: usize,
    ) -> Result<PhysAddr, KernelError> {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return Err(KernelError::WrongObject),
        };
        if !page_offset.is_multiple_of(crate::kernel::vm::PAGE_SIZE) {
            return Err(KernelError::Vm(VmError::Misaligned));
        }
        let entry = memory
            .memory_objects
            .iter()
            .flatten()
            .find(|e| e.id == id)
            .copied()
            .ok_or(KernelError::MemoryObjectMissing)?;
        let object_len = u64::try_from(entry.len).map_err(|_| KernelError::WrongObject)?;
        let selection = super::shared_region_selection(object, region, object_len)
            .map_err(super::shared_region_refusal_error)?;
        let page_offset = page_offset as u64;
        if page_offset >= selection.mapped_len {
            return Err(KernelError::WrongObject);
        }
        selection
            .object_offset
            .checked_add(page_offset)
            .and_then(|off| entry.phys.0.checked_add(off))
            .map(PhysAddr)
            .ok_or(KernelError::WrongObject)
    }

    pub(crate) fn note_mapping_removed(&mut self, phys: PhysAddr) {
        self.with_memory_state_mut(|memory| Self::note_mapping_removed_locked(memory, phys));
    }

    /// Stage 114 / D-NEXT-2: sibling of [`Self::note_mapping_removed`] taking
    /// `&mut MemorySubsystem` directly for use inside `SharedKernel::with_memory_split_mut`'s
    /// closure. SHARED-REGION-LIFETIME1: returns exactly the reference
    /// [`Self::note_mapping_inserted_locked`] took for this page, on the same owner.
    pub(crate) fn note_mapping_removed_locked(memory: &mut MemorySubsystem, phys: PhysAddr) {
        if let Some(slot) = Self::frame_owner_slot_locked(memory, phys)
            && let Some(memory_object) = memory.memory_objects[slot].as_mut()
        {
            memory_object.map_refcount = memory_object.map_refcount.saturating_sub(1);
            trace_oracle_object_refs(memory_object, "unmap", phys);
        }
    }

    pub(crate) fn reclaim_memory_object_if_unreferenced(&mut self, object: CapObject) {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return,
        };

        let Some(slot_index) = self.memory_object_slot_by_id(id) else {
            return;
        };
        self.with_memory_state_mut(|memory| {
            let Some(memory_object) = memory.memory_objects[slot_index] else {
                return;
            };
            if memory_object.cap_refcount != 0
                || memory_object.map_refcount != 0
                || memory_object.pin_refcount != 0
            {
                return;
            }
            Self::release_memory_object_slot_locked(memory, slot_index);
        });
    }

    pub(crate) fn reclaim_memory_object_for_phys(&mut self, phys: PhysAddr) {
        self.with_memory_state_mut(|memory| {
            Self::reclaim_memory_object_for_phys_locked(memory, phys);
        });
    }

    /// Stage 114 / D-NEXT-2: sibling of [`Self::reclaim_memory_object_for_phys`] taking
    /// `&mut MemorySubsystem` directly for use inside `SharedKernel::with_memory_split_mut`'s
    /// closure: reclaim the page's owner ([`Self::frame_owner_slot_locked`]) if it is
    /// unreferenced — its WHOLE extent, once, through the backing-aware release. A borrowed
    /// frame has no owner here and is never reclaimed by address.
    pub(crate) fn reclaim_memory_object_for_phys_locked(
        memory: &mut MemorySubsystem,
        phys: PhysAddr,
    ) {
        let Some(slot_index) = Self::frame_owner_slot_locked(memory, phys) else {
            return;
        };
        let Some(memory_object) = memory.memory_objects[slot_index] else {
            return;
        };
        if memory_object.cap_refcount != 0
            || memory_object.map_refcount != 0
            || memory_object.pin_refcount != 0
        {
            return;
        }
        Self::release_memory_object_slot_locked(memory, slot_index);
    }

    /// U6/199C test accessor: `(cap_refcount, map_refcount, pin_refcount)` for the MemoryObject
    /// with `id`, or `None` if it has been reclaimed. Keyed by id because the shared-region
    /// blocking-send proofs hold a `CapObject::MemoryObject { id }`, not a physical address.
    #[cfg(test)]
    pub(crate) fn memory_object_refcounts_by_id(&self, id: u64) -> Option<(u32, u32, u32)> {
        let slot = self.memory_object_slot_by_id(id)?;
        self.with_memory_state(|memory| {
            memory.memory_objects[slot]
                .map(|obj| (obj.cap_refcount, obj.map_refcount, obj.pin_refcount))
        })
    }

    /// Return `(cap_refcount, map_refcount, pin_refcount)` for the MemoryObject
    /// backing `phys`, or `None` if the object has been reclaimed.
    #[cfg(test)]
    pub(crate) fn memory_object_refcounts(&self, phys: PhysAddr) -> Option<(u32, u32, u32)> {
        self.with_memory_state(|memory| {
            memory
                .memory_objects
                .iter()
                .flatten()
                .find(|obj| obj.phys == phys)
                .map(|obj| (obj.cap_refcount, obj.map_refcount, obj.pin_refcount))
        })
    }

    /// Stage 198E3C1 test accessor: the byte length of the MemoryObject backing `phys` (used to
    /// prove the shared-region oracle provisions exactly a two-page object).
    #[cfg(test)]
    pub(crate) fn memory_object_len_for_test(&self, phys: PhysAddr) -> Option<usize> {
        self.with_memory_state(|memory| {
            memory
                .memory_objects
                .iter()
                .flatten()
                .find(|obj| obj.phys == phys)
                .map(|obj| obj.len)
        })
    }

    /// Return true if a MemoryObject slot exists for `phys` (i.e., not reclaimed).
    #[cfg(test)]
    pub(crate) fn memory_object_exists_for_phys(&self, phys: PhysAddr) -> bool {
        self.memory_object_refcounts(phys).is_some()
    }

    /// Stage 198E3C1B rollback accessor: the number of live (non-reclaimed) MemoryObject slots.
    /// A leak-free provisioning rollback must leave this UNCHANGED versus the pre-attempt count.
    #[cfg(test)]
    pub(crate) fn live_memory_object_count_for_test(&self) -> usize {
        self.with_memory_state(|memory| memory.memory_objects.iter().flatten().count())
    }
}
