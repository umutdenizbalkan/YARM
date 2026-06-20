// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::lock::SpinLockIrq;
use crate::kernel::vm::PAGE_SIZE;

const PAGE_SIZE_U64: u64 = PAGE_SIZE as u64;
#[cfg(feature = "hosted-dev")]
const MAX_FREE_EXTENTS: usize = 256;
#[cfg(not(feature = "hosted-dev"))]
const MAX_FREE_EXTENTS: usize = 512;
#[cfg(feature = "hosted-dev")]
const MAX_TRACKED_FRAME_REFS: usize = 256;
#[cfg(not(feature = "hosted-dev"))]
const MAX_TRACKED_FRAME_REFS: usize = 8192;
#[cfg(feature = "hosted-dev")]
const CONTIG_SIZE_CLASSES: [usize; 8] = [1, 2, 4, 8, 16, 32, 64, 128];
#[cfg(not(feature = "hosted-dev"))]
const CONTIG_SIZE_CLASSES: [usize; 10] = [1, 2, 4, 8, 16, 32, 64, 128, 256, 512];

// TODO(frame-allocator-scale): the fixed hash table below removes the previous
// linear phys→refcount scan, but it is still bounded by MAX_TRACKED_FRAME_REFS.
// A production-scale design should size metadata from the boot RAM map: use an
// inverted page map or radix metadata for O(1)/O(log n) shared-frame/COW/fork
// lookups, and eventually move free extents into intrusive records stored in
// free physical pages. Keep the rollback guarantees below when replacing this.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegion {
    pub start: u64,
    pub len: u64,
    pub usable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAllocError {
    InvalidMemoryMap,
    OutOfMemory,
    CapacityExceeded,
    Misaligned,
    OutOfRange,
    AlreadyFree,
    Uninitialized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FreeExtent {
    start_phys: u64,
    pages: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameRefCount {
    phys: u64,
    refs: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameRefSlot {
    Empty,
    Tombstone,
    Occupied(FrameRefCount),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct FrameRefTelemetry {
    lookup_probes_total: u64,
    lookup_max_probes: usize,
    lookup_misses: u64,
    insert_probes_total: u64,
    insert_max_probes: usize,
    insert_capacity_failures: u64,
}

// Stage 136: Copy removed to prevent accidental 209 KB stack copies.
// Clone is kept for test helpers (hosted-dev has small MAX_TRACKED_FRAME_REFS).
// Production code that needs to copy a PFA uses pfa_clone_to().
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalFrameAllocator {
    base_phys: u64,
    end_phys_exclusive: u64,
    total_frames: usize,
    free_frames: usize,
    initialized: bool,
    extents: [Option<FreeExtent>; MAX_FREE_EXTENTS],
    largest_free_run_pages: usize,
    run_hint_by_class: [Option<usize>; CONTIG_SIZE_CLASSES.len()],
    single_page_hint_idx: Option<usize>,
    frame_refs: [FrameRefSlot; MAX_TRACKED_FRAME_REFS],
    frame_ref_telemetry: FrameRefTelemetry,
}

impl PhysicalFrameAllocator {
    pub const fn new_uninit() -> Self {
        Self {
            base_phys: 0,
            end_phys_exclusive: 0,
            total_frames: 0,
            free_frames: 0,
            initialized: false,
            extents: [const { None }; MAX_FREE_EXTENTS],
            largest_free_run_pages: 0,
            run_hint_by_class: [const { None }; CONTIG_SIZE_CLASSES.len()],
            single_page_hint_idx: None,
            frame_refs: [const { FrameRefSlot::Empty }; MAX_TRACKED_FRAME_REFS],
            frame_ref_telemetry: FrameRefTelemetry {
                lookup_probes_total: 0,
                lookup_max_probes: 0,
                lookup_misses: 0,
                insert_probes_total: 0,
                insert_max_probes: 0,
                insert_capacity_failures: 0,
            },
        }
    }

    pub fn init_from_memory_map(
        &mut self,
        regions: &[MemoryRegion],
    ) -> Result<(), FrameAllocError> {
        let mut min_phys = u64::MAX;
        let mut max_phys = 0u64;
        let mut have_usable = false;

        for region in regions {
            if !region.usable || region.len == 0 {
                continue;
            }
            have_usable = true;
            let end = region
                .start
                .checked_add(region.len)
                .ok_or(FrameAllocError::InvalidMemoryMap)?;
            let aligned_end = align_up_checked(end).ok_or(FrameAllocError::InvalidMemoryMap)?;
            min_phys = min_phys.min(align_down(region.start));
            max_phys = max_phys.max(aligned_end);
        }

        if !have_usable || max_phys <= min_phys {
            return Err(FrameAllocError::InvalidMemoryMap);
        }

        self.base_phys = min_phys;
        self.end_phys_exclusive = max_phys;
        self.total_frames = 0;
        self.free_frames = 0;
        self.initialized = true;
        self.extents = [const { None }; MAX_FREE_EXTENTS];
        self.largest_free_run_pages = 0;
        self.run_hint_by_class = [const { None }; CONTIG_SIZE_CLASSES.len()];
        self.single_page_hint_idx = None;
        self.frame_refs = [const { FrameRefSlot::Empty }; MAX_TRACKED_FRAME_REFS];
        self.frame_ref_telemetry = FrameRefTelemetry::default();

        for region in regions {
            if !region.usable || region.len == 0 {
                continue;
            }
            let Some(start) = align_up_checked(region.start) else {
                continue;
            };
            let end = region
                .start
                .checked_add(region.len)
                .ok_or(FrameAllocError::InvalidMemoryMap)?;
            let end = align_down(end);
            if end <= start {
                continue;
            }
            let pages = ((end - start) / PAGE_SIZE_U64) as usize;
            self.insert_extent(start, pages)?;
            self.total_frames = self.total_frames.saturating_add(pages);
            self.free_frames = self.free_frames.saturating_add(pages);
        }

        if self.total_frames == 0 {
            return Err(FrameAllocError::InvalidMemoryMap);
        }
        self.refresh_run_metadata();

        Ok(())
    }

    pub fn alloc_frame(&mut self) -> Result<u64, FrameAllocError> {
        if !self.initialized {
            return Err(FrameAllocError::InvalidMemoryMap);
        }
        if self.free_frames == 0 {
            return Err(FrameAllocError::OutOfMemory);
        }
        if !self.ensure_frame_ref_capacity(1) {
            return Err(FrameAllocError::CapacityExceeded);
        }

        let hint_idx = self
            .single_page_hint_idx
            .filter(|&idx| self.extents[idx].is_some_and(|extent| extent.pages > 0))
            .or_else(|| self.find_extent_index(1));
        let Some(idx) = hint_idx else {
            return Err(FrameAllocError::OutOfMemory);
        };
        let candidate = self.extents[idx].expect("allocation extent").start_phys;
        if self.frame_ref_slot(candidate).is_some() {
            return Err(FrameAllocError::AlreadyFree);
        }

        let (alloc_phys, old_pages, new_pages) = self.split_extent_for_allocation(idx, 1)?;
        self.free_frames = self.free_frames.saturating_sub(1);
        self.update_hints_after_allocation(idx, old_pages, new_pages);
        self.track_new_frame_ref(alloc_phys)?;
        #[cfg(not(feature = "hosted-dev"))]
        if let Some((rs, re)) = GLOBAL_RESERVED_RANGES.lock().find_overlap(alloc_phys) {
            crate::yarm_log!(
                "PMEM_ALLOC_RESERVED_BUG pa=0x{:x} range=0x{:x}..0x{:x}",
                alloc_phys,
                rs,
                re
            );
            panic!("PMEM_ALLOC_RESERVED_BUG: allocated frame overlaps reserved range");
        }
        Ok(alloc_phys)
    }

    pub fn alloc_contiguous(&mut self, pages: usize) -> Result<u64, FrameAllocError> {
        if !self.initialized {
            return Err(FrameAllocError::InvalidMemoryMap);
        }
        if pages == 0 || pages > self.free_frames || pages > self.largest_free_run_pages {
            return Err(FrameAllocError::OutOfMemory);
        }
        if !self.ensure_frame_ref_capacity(pages) {
            return Err(FrameAllocError::CapacityExceeded);
        }

        if let Some(idx) = self
            .fast_path_extent_index(pages)
            .or_else(|| self.find_extent_index(pages))
        {
            #[cfg(all(not(feature = "hosted-dev"), feature = "trace_frame_alloc"))]
            if let Some(ext) = self.extents[idx] {
                crate::yarm_log!(
                    "CONTIG_ALLOC_CANDIDATE start=0x{:x} pages={}",
                    ext.start_phys,
                    pages
                );
            }
            let alloc_start = self.extents[idx].expect("allocation extent").start_phys;
            for page in 0..pages {
                let phys = alloc_start.saturating_add((page as u64).saturating_mul(PAGE_SIZE_U64));
                if self.frame_ref_slot(phys).is_some() {
                    return Err(FrameAllocError::AlreadyFree);
                }
            }

            let (alloc_phys, _, _) = self.split_extent_for_allocation(idx, pages)?;
            self.free_frames = self.free_frames.saturating_sub(pages);
            self.refresh_run_metadata();
            #[cfg(all(not(feature = "hosted-dev"), feature = "trace_frame_alloc"))]
            crate::yarm_log!(
                "CONTIG_ALLOC_COMMIT start=0x{:x} pages={}",
                alloc_phys,
                pages
            );
            for page in 0..pages {
                let phys = alloc_phys.saturating_add((page as u64).saturating_mul(PAGE_SIZE_U64));
                self.track_new_frame_ref(phys)?;
                #[cfg(not(feature = "hosted-dev"))]
                if let Some((rs, re)) = GLOBAL_RESERVED_RANGES.lock().find_overlap(phys) {
                    crate::yarm_log!(
                        "PMEM_ALLOC_RESERVED_BUG_CONTIG pa=0x{:x} range=0x{:x}..0x{:x} pages={}",
                        phys,
                        rs,
                        re,
                        pages
                    );
                    panic!(
                        "PMEM_ALLOC_RESERVED_BUG_CONTIG: allocated contiguous frame overlaps reserved range"
                    );
                }
            }
            return Ok(alloc_phys);
        }

        Err(FrameAllocError::OutOfMemory)
    }

    pub fn free_frame(&mut self, phys: u64) -> Result<(), FrameAllocError> {
        self.free_contiguous(phys, 1)
    }

    pub fn free_contiguous(
        &mut self,
        start_phys: u64,
        pages: usize,
    ) -> Result<(), FrameAllocError> {
        if !self.initialized {
            return Err(FrameAllocError::InvalidMemoryMap);
        }
        if pages == 0 {
            return Ok(());
        }
        if !start_phys.is_multiple_of(PAGE_SIZE_U64) {
            return Err(FrameAllocError::Misaligned);
        }
        if start_phys < self.base_phys {
            return Err(FrameAllocError::OutOfRange);
        }
        let span = (pages as u64).saturating_mul(PAGE_SIZE_U64);
        let end_phys = start_phys.saturating_add(span);
        if end_phys > self.end_phys_exclusive || end_phys <= start_phys {
            return Err(FrameAllocError::OutOfRange);
        }

        // Stage 136: work on a .bss scratch copy rather than a 209 KB stack copy.
        // Transactional semantics are preserved: self is only updated on success.
        // Lock order: caller holds the per-allocator SpinLockIrq; we now take
        // FREE_CONTIG_SCRATCH — consistent ordering prevents deadlock.
        let mut scratch = FREE_CONTIG_SCRATCH.lock();
        pfa_clone_to(self, &mut scratch);

        let mut pending_run_start = 0u64;
        let mut pending_run_pages = 0usize;
        let mut freed_pages = 0usize;

        for page in 0..pages {
            let phys = start_phys.saturating_add((page as u64).saturating_mul(PAGE_SIZE_U64));
            let remaining = scratch.untrack_frame_ref(phys)?;
            if remaining == 0 {
                if pending_run_pages == 0 {
                    pending_run_start = phys;
                    pending_run_pages = 1;
                } else {
                    pending_run_pages += 1;
                }
            } else if pending_run_pages != 0 {
                scratch.insert_extent_unsorted(pending_run_start, pending_run_pages)?;
                freed_pages += pending_run_pages;
                pending_run_pages = 0;
            }
        }

        if pending_run_pages != 0 {
            scratch.insert_extent_unsorted(pending_run_start, pending_run_pages)?;
            freed_pages += pending_run_pages;
        }

        scratch.free_frames = scratch.free_frames.saturating_add(freed_pages);
        scratch.sort_extents();
        scratch.refresh_run_metadata();
        pfa_clone_to(&scratch, self);
        Ok(())
    }

    pub fn reserve_frame(&mut self, phys: u64) -> Result<(), FrameAllocError> {
        let idx = self.frame_index(phys)?;
        if idx >= self.total_frames {
            return Err(FrameAllocError::OutOfRange);
        }

        let Some(slot_idx) = self.extent_containing_frame(phys) else {
            // The frame is already outside the free list. Treat duplicate
            // reservations of already allocated/reserved frames as idempotent.
            return Ok(());
        };
        if self.frame_ref_slot(phys).is_some() {
            return Err(FrameAllocError::AlreadyFree);
        }
        if !self.ensure_frame_ref_capacity(1) {
            return Err(FrameAllocError::CapacityExceeded);
        }

        let extent = self.extents[slot_idx].expect("extent");
        if extent.pages == 0 {
            return Ok(());
        }

        let extent_end = extent
            .start_phys
            .saturating_add((extent.pages as u64).saturating_mul(PAGE_SIZE_U64));
        let only_page = extent.pages == 1;
        let first_page = phys == extent.start_phys;
        let last_page = phys.saturating_add(PAGE_SIZE_U64) == extent_end;

        if only_page {
            self.extents[slot_idx] = None;
        } else if first_page {
            self.extents[slot_idx] = Some(FreeExtent {
                start_phys: extent.start_phys.saturating_add(PAGE_SIZE_U64),
                pages: extent.pages - 1,
            });
        } else if last_page {
            self.extents[slot_idx] = Some(FreeExtent {
                start_phys: extent.start_phys,
                pages: extent.pages - 1,
            });
        } else {
            // Middle split needs one additional extent slot. Precheck before
            // mutating so a capacity failure leaves accounting/free list intact.
            let Some(right_slot_idx) = self.empty_extent_slot_except(slot_idx) else {
                return Err(FrameAllocError::CapacityExceeded);
            };
            let left_pages = ((phys - extent.start_phys) / PAGE_SIZE_U64) as usize;
            let right_start = phys.saturating_add(PAGE_SIZE_U64);
            let right_pages = ((extent_end - right_start) / PAGE_SIZE_U64) as usize;
            self.extents[slot_idx] = Some(FreeExtent {
                start_phys: extent.start_phys,
                pages: left_pages,
            });
            self.extents[right_slot_idx] = Some(FreeExtent {
                start_phys: right_start,
                pages: right_pages,
            });
            self.sort_extents();
        }

        self.free_frames = self.free_frames.saturating_sub(1);
        self.track_new_frame_ref(phys)?;
        self.refresh_run_metadata();
        Ok(())
    }

    pub fn retain_frame(&mut self, phys: u64) -> Result<u16, FrameAllocError> {
        self.frame_index(phys)?;
        self.inc_frame_ref(phys)
    }

    pub fn frame_refcount(&self, phys: u64) -> Result<u16, FrameAllocError> {
        self.frame_index(phys)?;
        Ok(self
            .frame_ref_slot_readonly(phys)
            .map(|idx| match self.frame_refs[idx] {
                FrameRefSlot::Occupied(entry) => entry.refs,
                FrameRefSlot::Empty | FrameRefSlot::Tombstone => 0,
            })
            .unwrap_or(0))
    }

    pub fn total_frames(&self) -> usize {
        self.total_frames
    }

    pub fn free_frames(&self) -> usize {
        self.free_frames
    }

    fn frame_index(&self, phys: u64) -> Result<usize, FrameAllocError> {
        if !self.initialized {
            return Err(FrameAllocError::InvalidMemoryMap);
        }
        if !phys.is_multiple_of(PAGE_SIZE_U64) {
            return Err(FrameAllocError::Misaligned);
        }
        if phys < self.base_phys || phys >= self.end_phys_exclusive {
            return Err(FrameAllocError::OutOfRange);
        }
        Ok(((phys - self.base_phys) / PAGE_SIZE_U64) as usize)
    }

    fn frame_ref_hash(&self, phys: u64) -> usize {
        let page = phys / PAGE_SIZE_U64;
        (page.wrapping_mul(11_400_714_819_323_198_485u64) as usize) % MAX_TRACKED_FRAME_REFS
    }

    fn record_frame_ref_lookup(&mut self, probes: usize, found: bool) {
        self.frame_ref_telemetry.lookup_probes_total = self
            .frame_ref_telemetry
            .lookup_probes_total
            .saturating_add(probes as u64);
        self.frame_ref_telemetry.lookup_max_probes =
            self.frame_ref_telemetry.lookup_max_probes.max(probes);
        if !found {
            self.frame_ref_telemetry.lookup_misses =
                self.frame_ref_telemetry.lookup_misses.saturating_add(1);
        }
    }

    fn record_frame_ref_insert(&mut self, probes: usize, capacity_failed: bool) {
        self.frame_ref_telemetry.insert_probes_total = self
            .frame_ref_telemetry
            .insert_probes_total
            .saturating_add(probes as u64);
        self.frame_ref_telemetry.insert_max_probes =
            self.frame_ref_telemetry.insert_max_probes.max(probes);
        if capacity_failed {
            self.frame_ref_telemetry.insert_capacity_failures = self
                .frame_ref_telemetry
                .insert_capacity_failures
                .saturating_add(1);
        }
    }

    fn frame_ref_slot_readonly(&self, phys: u64) -> Option<usize> {
        let start = self.frame_ref_hash(phys);
        for probe in 0..MAX_TRACKED_FRAME_REFS {
            let idx = (start + probe) % MAX_TRACKED_FRAME_REFS;
            match self.frame_refs[idx] {
                FrameRefSlot::Empty => return None,
                FrameRefSlot::Tombstone => continue,
                FrameRefSlot::Occupied(entry) if entry.phys == phys => return Some(idx),
                FrameRefSlot::Occupied(_) => continue,
            }
        }
        None
    }

    fn frame_ref_slot(&mut self, phys: u64) -> Option<usize> {
        let start = self.frame_ref_hash(phys);
        for probe in 0..MAX_TRACKED_FRAME_REFS {
            let idx = (start + probe) % MAX_TRACKED_FRAME_REFS;
            match self.frame_refs[idx] {
                FrameRefSlot::Empty => {
                    self.record_frame_ref_lookup(probe + 1, false);
                    return None;
                }
                FrameRefSlot::Tombstone => continue,
                FrameRefSlot::Occupied(entry) if entry.phys == phys => {
                    self.record_frame_ref_lookup(probe + 1, true);
                    return Some(idx);
                }
                FrameRefSlot::Occupied(_) => continue,
            }
        }
        self.record_frame_ref_lookup(MAX_TRACKED_FRAME_REFS, false);
        // Stage 136: compaction deliberately omitted from the lookup path.
        // Lookup correctness is unaffected by tombstones (probing skips them).
        // Compaction runs on the insert path (find_frame_ref_insert_slot) where
        // it is needed to recover probing headroom.
        None
    }

    fn find_frame_ref_insert_slot(&mut self, phys: u64) -> Result<usize, FrameAllocError> {
        let start = self.frame_ref_hash(phys);
        let mut first_tombstone = None;
        for probe in 0..MAX_TRACKED_FRAME_REFS {
            let idx = (start + probe) % MAX_TRACKED_FRAME_REFS;
            match self.frame_refs[idx] {
                FrameRefSlot::Empty => {
                    let insert_idx = first_tombstone.unwrap_or(idx);
                    self.record_frame_ref_insert(probe + 1, false);
                    return Ok(insert_idx);
                }
                FrameRefSlot::Tombstone => {
                    if first_tombstone.is_none() {
                        first_tombstone = Some(idx);
                    }
                }
                FrameRefSlot::Occupied(entry) if entry.phys == phys => {
                    self.record_frame_ref_insert(probe + 1, false);
                    return Err(FrameAllocError::AlreadyFree);
                }
                FrameRefSlot::Occupied(_) => {}
            }
        }
        if first_tombstone.is_some() {
            self.record_frame_ref_insert(MAX_TRACKED_FRAME_REFS, false);
            self.compact_frame_refs();
            self.find_frame_ref_insert_slot(phys)
        } else {
            self.record_frame_ref_insert(MAX_TRACKED_FRAME_REFS, true);
            Err(FrameAllocError::CapacityExceeded)
        }
    }

    fn track_new_frame_ref(&mut self, phys: u64) -> Result<(), FrameAllocError> {
        let slot = self.find_frame_ref_insert_slot(phys)?;
        self.frame_refs[slot] = FrameRefSlot::Occupied(FrameRefCount { phys, refs: 1 });
        Ok(())
    }

    fn inc_frame_ref(&mut self, phys: u64) -> Result<u16, FrameAllocError> {
        let Some(slot) = self.frame_ref_slot(phys) else {
            return Err(FrameAllocError::AlreadyFree);
        };
        let FrameRefSlot::Occupied(mut entry) = self.frame_refs[slot] else {
            return Err(FrameAllocError::AlreadyFree);
        };
        entry.refs = entry.refs.saturating_add(1);
        self.frame_refs[slot] = FrameRefSlot::Occupied(entry);
        Ok(entry.refs)
    }

    fn untrack_frame_ref(&mut self, phys: u64) -> Result<u16, FrameAllocError> {
        let Some(slot) = self.frame_ref_slot(phys) else {
            return Err(FrameAllocError::AlreadyFree);
        };
        let FrameRefSlot::Occupied(mut entry) = self.frame_refs[slot] else {
            return Err(FrameAllocError::AlreadyFree);
        };
        if entry.refs > 1 {
            entry.refs -= 1;
            self.frame_refs[slot] = FrameRefSlot::Occupied(entry);
            return Ok(entry.refs);
        }
        self.frame_refs[slot] = FrameRefSlot::Tombstone;
        Ok(0)
    }

    fn insert_extent(&mut self, start_phys: u64, pages: usize) -> Result<(), FrameAllocError> {
        self.insert_extent_unsorted(start_phys, pages)?;
        self.sort_extents();
        Ok(())
    }

    fn insert_extent_unsorted(
        &mut self,
        start_phys: u64,
        pages: usize,
    ) -> Result<(), FrameAllocError> {
        if pages == 0 {
            return Ok(());
        }
        let mut start = start_phys;
        let mut end = start_phys.saturating_add((pages as u64).saturating_mul(PAGE_SIZE_U64));
        let mut slot = None;

        for idx in 0..self.extents.len() {
            if let Some(extent) = self.extents[idx] {
                let extent_end = extent
                    .start_phys
                    .saturating_add((extent.pages as u64).saturating_mul(PAGE_SIZE_U64));
                if end < extent.start_phys || start > extent_end {
                    continue;
                }
                if end == extent.start_phys
                    || start == extent_end
                    || (start < extent_end && end > extent.start_phys)
                {
                    start = start.min(extent.start_phys);
                    end = end.max(extent_end);
                    self.extents[idx] = None;
                    if slot.is_none() {
                        slot = Some(idx);
                    }
                    continue;
                }
            }
            if slot.is_none() && self.extents[idx].is_none() {
                slot = Some(idx);
            }
        }
        let slot = slot.or_else(|| self.extents.iter().position(|entry| entry.is_none()));
        let Some(slot_idx) = slot else {
            return Err(FrameAllocError::CapacityExceeded);
        };
        self.extents[slot_idx] = Some(FreeExtent {
            start_phys: start,
            pages: ((end - start) / PAGE_SIZE_U64) as usize,
        });
        Ok(())
    }

    fn extent_containing_frame(&self, phys: u64) -> Option<usize> {
        self.extents
            .iter()
            .enumerate()
            .find_map(|(slot_idx, slot)| {
                let extent = (*slot)?;
                let extent_end = extent
                    .start_phys
                    .saturating_add((extent.pages as u64).saturating_mul(PAGE_SIZE_U64));
                (phys >= extent.start_phys && phys < extent_end).then_some(slot_idx)
            })
    }

    fn empty_extent_slot_except(&self, excluded: usize) -> Option<usize> {
        self.extents
            .iter()
            .enumerate()
            .find_map(|(idx, slot)| (idx != excluded && slot.is_none()).then_some(idx))
    }

    fn frame_ref_counts(&self) -> (usize, usize, usize) {
        let mut occupied = 0usize;
        let mut tombstones = 0usize;
        let mut empty = 0usize;
        for slot in &self.frame_refs {
            match slot {
                FrameRefSlot::Empty => empty += 1,
                FrameRefSlot::Tombstone => tombstones += 1,
                FrameRefSlot::Occupied(_) => occupied += 1,
            }
        }
        (occupied, tombstones, empty)
    }

    fn ensure_frame_ref_capacity(&mut self, slots: usize) -> bool {
        let (occupied, tombstones, empty) = self.frame_ref_counts();
        if MAX_TRACKED_FRAME_REFS.saturating_sub(occupied) < slots {
            return false;
        }
        if tombstones != 0 && (empty < slots || tombstones > MAX_TRACKED_FRAME_REFS / 4) {
            self.compact_frame_refs();
        }
        true
    }

    // Stage 136: #[inline(never)] prevents the COMPACT_SCRATCH lock acquisition and
    // frame_refs array operations from bloating callers' stack frames.
    #[inline(never)]
    fn compact_frame_refs(&mut self) {
        // Acquire the module-level scratch buffer (in .bss, not stack).
        // copy_from_slice is a memcpy — no 192 KB stack temporary.
        let mut scratch = COMPACT_SCRATCH.lock();
        scratch.copy_from_slice(&self.frame_refs);
        self.frame_refs = [const { FrameRefSlot::Empty }; MAX_TRACKED_FRAME_REFS];
        for slot in scratch.iter() {
            let FrameRefSlot::Occupied(entry) = *slot else {
                continue;
            };
            let start = self.frame_ref_hash(entry.phys);
            for probe in 0..MAX_TRACKED_FRAME_REFS {
                let idx = (start + probe) % MAX_TRACKED_FRAME_REFS;
                if matches!(self.frame_refs[idx], FrameRefSlot::Empty) {
                    self.frame_refs[idx] = FrameRefSlot::Occupied(entry);
                    break;
                }
            }
        }
        for slot in scratch.iter_mut() {
            *slot = FrameRefSlot::Empty;
        }
    }

    fn split_extent_for_allocation(
        &mut self,
        idx: usize,
        pages: usize,
    ) -> Result<(u64, usize, usize), FrameAllocError> {
        let Some(mut extent) = self.extents[idx] else {
            return Err(FrameAllocError::OutOfMemory);
        };
        if extent.pages < pages {
            return Err(FrameAllocError::OutOfMemory);
        }
        let old_pages = extent.pages;
        let alloc_phys = extent.start_phys;
        extent.start_phys = extent
            .start_phys
            .saturating_add((pages as u64).saturating_mul(PAGE_SIZE_U64));
        extent.pages -= pages;
        let new_pages = extent.pages;
        if extent.pages == 0 {
            self.extents[idx] = None;
        } else {
            self.extents[idx] = Some(extent);
        }
        Ok((alloc_phys, old_pages, new_pages))
    }

    fn find_extent_index(&self, pages: usize) -> Option<usize> {
        self.extents
            .iter()
            .enumerate()
            .find_map(|(idx, extent)| extent.filter(|entry| entry.pages >= pages).map(|_| idx))
    }

    fn fast_path_extent_index(&self, pages: usize) -> Option<usize> {
        let class = CONTIG_SIZE_CLASSES.iter().position(|&size| size >= pages)?;
        self.run_hint_by_class[class]
            .filter(|&idx| self.extents[idx].is_some_and(|extent| extent.pages >= pages))
    }

    fn refresh_run_metadata(&mut self) {
        self.largest_free_run_pages = 0;
        self.run_hint_by_class = [const { None }; CONTIG_SIZE_CLASSES.len()];
        self.single_page_hint_idx = None;
        for (idx, extent) in self.extents.iter().enumerate() {
            let Some(extent) = extent else {
                continue;
            };
            self.largest_free_run_pages = self.largest_free_run_pages.max(extent.pages);
            if self.single_page_hint_idx.is_none() {
                self.single_page_hint_idx = Some(idx);
            }
            for (class_idx, class_pages) in CONTIG_SIZE_CLASSES.iter().enumerate() {
                if extent.pages >= *class_pages && self.run_hint_by_class[class_idx].is_none() {
                    self.run_hint_by_class[class_idx] = Some(idx);
                }
            }
        }
    }

    fn update_hints_after_allocation(&mut self, _idx: usize, _old_pages: usize, _new_pages: usize) {
        self.refresh_run_metadata();
    }

    fn sort_extents(&mut self) {
        let mut write = 0usize;
        for idx in 0..self.extents.len() {
            if let Some(extent) = self.extents[idx] {
                self.extents[write] = Some(extent);
                if write != idx {
                    self.extents[idx] = None;
                }
                write += 1;
            }
        }
        for idx in write..self.extents.len() {
            self.extents[idx] = None;
        }
        for i in 0..write {
            for j in (i + 1)..write {
                let left = self.extents[i].expect("left");
                let right = self.extents[j].expect("right");
                if right.start_phys < left.start_phys {
                    self.extents[i] = Some(right);
                    self.extents[j] = Some(left);
                }
            }
        }
    }
}

// Stage 136: scratch buffer for compact_frame_refs — lives in .bss (not stack).
// Sized to match the frame_refs table. Lock order: always acquire the per-allocator
// SpinLockIrq BEFORE this lock so the two allocators (PT + main) serialise here.
static COMPACT_SCRATCH: SpinLockIrq<[FrameRefSlot; MAX_TRACKED_FRAME_REFS]> =
    SpinLockIrq::new([const { FrameRefSlot::Empty }; MAX_TRACKED_FRAME_REFS]);

// Stage 136: scratch buffer for free_contiguous two-phase commit — lives in .bss.
// Lock order: always acquire the per-allocator SpinLockIrq BEFORE this lock.
static FREE_CONTIG_SCRATCH: SpinLockIrq<PhysicalFrameAllocator> =
    SpinLockIrq::new(PhysicalFrameAllocator::new_uninit());

// Copy all fields of `src` into `dst` without creating a 209 KB stack temporary.
// Large array fields are copied via copy_from_slice (memcpy), small fields directly.
// Used by free_contiguous for transactional two-phase commit under FREE_CONTIG_SCRATCH.
#[inline(never)]
fn pfa_clone_to(src: &PhysicalFrameAllocator, dst: &mut PhysicalFrameAllocator) {
    dst.base_phys = src.base_phys;
    dst.end_phys_exclusive = src.end_phys_exclusive;
    dst.total_frames = src.total_frames;
    dst.free_frames = src.free_frames;
    dst.initialized = src.initialized;
    dst.extents.copy_from_slice(&src.extents);
    dst.largest_free_run_pages = src.largest_free_run_pages;
    dst.run_hint_by_class
        .copy_from_slice(&src.run_hint_by_class);
    dst.single_page_hint_idx = src.single_page_hint_idx;
    dst.frame_refs.copy_from_slice(&src.frame_refs);
    dst.frame_ref_telemetry = src.frame_ref_telemetry;
}

// Stage 135: static holds an uninitialized PhysicalFrameAllocator in .bss;
// ensure_pt_allocator_initialized calls init_from_memory_map in-place, avoiding
// the ~209 KB stack frame that a local `let mut allocator = new_uninit()` would create.
static PT_FRAME_ALLOCATOR: SpinLockIrq<PhysicalFrameAllocator> =
    SpinLockIrq::new(PhysicalFrameAllocator::new_uninit());

const MAX_GLOBAL_RESERVED: usize = 12;

#[derive(Copy, Clone)]
struct GlobalReservedRanges {
    starts: [u64; MAX_GLOBAL_RESERVED],
    ends: [u64; MAX_GLOBAL_RESERVED],
    count: usize,
}

impl GlobalReservedRanges {
    const fn new() -> Self {
        Self {
            starts: [0u64; MAX_GLOBAL_RESERVED],
            ends: [0u64; MAX_GLOBAL_RESERVED],
            count: 0,
        }
    }

    fn add(&mut self, start: u64, end: u64) -> Result<(), FrameAllocError> {
        if end <= start {
            return Ok(());
        }
        // Deduplicate: skip if this exact range is already registered.
        for i in 0..self.count {
            if self.starts[i] == start && self.ends[i] == end {
                return Ok(());
            }
        }
        if self.count >= MAX_GLOBAL_RESERVED {
            return Err(FrameAllocError::CapacityExceeded);
        }
        self.starts[self.count] = start;
        self.ends[self.count] = end;
        self.count += 1;
        Ok(())
    }

    fn find_overlap(&self, pa: u64) -> Option<(u64, u64)> {
        for i in 0..self.count {
            if pa >= self.starts[i] && pa < self.ends[i] {
                return Some((self.starts[i], self.ends[i]));
            }
        }
        None
    }
}

static GLOBAL_RESERVED_RANGES: SpinLockIrq<GlobalReservedRanges> =
    SpinLockIrq::new(GlobalReservedRanges::new());

/// Tracks the physical address ranges given exclusively to PT_FRAME_ALLOCATOR.
/// Checked only by main-allocator entry points to detect cross-contamination.
/// NOT added to GLOBAL_RESERVED_RANGES — PT allocator uses these pages legitimately.
static PT_POOL_RANGES: SpinLockIrq<GlobalReservedRanges> =
    SpinLockIrq::new(GlobalReservedRanges::new());

pub fn register_reserved_range(start: u64, end: u64) -> Result<(), FrameAllocError> {
    if end <= start {
        return Ok(());
    }
    GLOBAL_RESERVED_RANGES.lock().add(start, end)
}

pub fn register_pt_pool_range(start: u64, end: u64) -> Result<(), FrameAllocError> {
    if end <= start {
        return Ok(());
    }
    PT_POOL_RANGES.lock().add(start, end)
}

pub fn is_pa_reserved(pa: u64) -> Option<(u64, u64)> {
    GLOBAL_RESERVED_RANGES.lock().find_overlap(pa)
}

pub fn is_pa_in_pt_pool(pa: u64) -> Option<(u64, u64)> {
    PT_POOL_RANGES.lock().find_overlap(pa)
}

fn default_pt_allocator_regions() -> [MemoryRegion; 1] {
    #[cfg(feature = "hosted-dev")]
    let len = 128 * 1024 * 1024;
    #[cfg(all(not(feature = "hosted-dev"), not(target_arch = "x86_64")))]
    let len = 16 * 1024 * 1024;

    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    {
        // Keep default page-table frames inside the early higher-half direct map.
        [MemoryRegion {
            start: crate::arch::platform_constants::NEXT_ANON_PHYS_BASE,
            len: 512 * 1024 * 1024,
            usable: true,
        }]
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    {
        [MemoryRegion {
            // Conservative fallback only: keep allocator seed near the bootstrap
            // image window until DTB RAM parsing can provide exact bounds.
            start: crate::arch::platform_constants::KERNEL_BOOTSTRAP_PHYS_BASE + 32 * 1024 * 1024,
            len,
            usable: true,
        }]
    }
    #[cfg(any(
        feature = "hosted-dev",
        all(not(target_arch = "x86_64"), not(target_arch = "aarch64"))
    ))]
    {
        [MemoryRegion {
            // Keep the default allocator in a conservative window immediately after
            // NEXT_ANON_PHYS_BASE so low-memory board/QEMU configs do not walk into
            // unmapped physical space when DTB/RAM probing is unavailable.
            start: crate::arch::platform_constants::NEXT_ANON_PHYS_BASE,
            len,
            usable: true,
        }]
    }
}

fn ensure_pt_allocator_initialized() -> Result<(), FrameAllocError> {
    let mut guard = PT_FRAME_ALLOCATOR.lock();
    if guard.initialized {
        return Ok(());
    }
    crate::yarm_log!("PT_ALLOCATOR_INIT_BEGIN");
    // PT_ALLOCATOR_INIT_NO_STACK_SCRATCH: init runs in-place on static storage;
    // no local PhysicalFrameAllocator is constructed on the stack.
    guard.init_from_memory_map(&default_pt_allocator_regions())?;
    crate::yarm_log!("PT_ALLOCATOR_INIT_NO_STACK_SCRATCH");
    crate::yarm_log!("PT_ALLOCATOR_INIT_DONE");
    Ok(())
}

pub fn init_pt_frame_allocator(regions: &[MemoryRegion]) -> Result<(), FrameAllocError> {
    PT_FRAME_ALLOCATOR.lock().init_from_memory_map(regions)
}

pub fn alloc_pt_frame() -> Result<u64, FrameAllocError> {
    ensure_pt_allocator_initialized()?;
    let mut guard = PT_FRAME_ALLOCATOR.lock();
    let pa = guard.alloc_frame()?;
    #[cfg(all(not(feature = "hosted-dev"), feature = "trace_frame_alloc"))]
    crate::yarm_log!("PT_ALLOC_FRAME pa=0x{:x}", pa);
    Ok(pa)
}

pub fn alloc_pt_contiguous_frames(pages: usize) -> Result<u64, FrameAllocError> {
    if pages == 0 {
        return Err(FrameAllocError::InvalidMemoryMap);
    }
    ensure_pt_allocator_initialized()?;
    PT_FRAME_ALLOCATOR.lock().alloc_contiguous(pages)
}

pub fn free_pt_frame(phys: u64) -> Result<(), FrameAllocError> {
    ensure_pt_allocator_initialized()?;
    PT_FRAME_ALLOCATOR.lock().free_frame(phys)
}

pub fn free_pt_contiguous_frames(base_phys: u64, pages: usize) -> Result<(), FrameAllocError> {
    if pages == 0 {
        return Err(FrameAllocError::InvalidMemoryMap);
    }
    ensure_pt_allocator_initialized()?;
    PT_FRAME_ALLOCATOR.lock().free_contiguous(base_phys, pages)
}

const fn align_down(value: u64) -> u64 {
    value & !(PAGE_SIZE_U64 - 1)
}

const fn align_up_checked(value: u64) -> Option<u64> {
    let mask = PAGE_SIZE_U64 - 1;
    if value & mask == 0 {
        Some(value)
    } else if let Some(rounded) = value.checked_add(mask) {
        Some(rounded & !mask)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::std::vec::Vec;

    fn extent_end(extent: FreeExtent) -> u64 {
        extent
            .start_phys
            .saturating_add((extent.pages as u64).saturating_mul(PAGE_SIZE_U64))
    }

    fn live_extents(alloc: &PhysicalFrameAllocator) -> Vec<FreeExtent> {
        alloc.extents.iter().filter_map(|entry| *entry).collect()
    }

    fn extent_count(alloc: &PhysicalFrameAllocator) -> usize {
        live_extents(alloc).len()
    }

    fn is_in_free_extent(alloc: &PhysicalFrameAllocator, phys: u64) -> bool {
        live_extents(alloc)
            .into_iter()
            .any(|extent| phys >= extent.start_phys && phys < extent_end(extent))
    }

    fn assert_allocator_invariants(alloc: &PhysicalFrameAllocator) {
        let extents = live_extents(alloc);
        let sum_free: usize = extents.iter().map(|extent| extent.pages).sum();
        assert_eq!(
            alloc.free_frames, sum_free,
            "free_frames must equal extent sum"
        );

        let mut largest = 0usize;
        let mut prev_end = None;
        for extent in &extents {
            assert_ne!(extent.pages, 0, "zero-length free extent");
            assert!(extent.start_phys.is_multiple_of(PAGE_SIZE_U64));
            let end = extent_end(*extent);
            assert!(end <= alloc.end_phys_exclusive);
            if let Some(prev) = prev_end {
                assert!(
                    prev < extent.start_phys,
                    "extents must be sorted, non-overlapping, and non-adjacent/coalesced"
                );
            }
            prev_end = Some(end);
            largest = largest.max(extent.pages);
        }
        assert_eq!(alloc.largest_free_run_pages, largest);

        if alloc.free_frames == 0 {
            assert_eq!(alloc.single_page_hint_idx, None);
        } else {
            let idx = alloc.single_page_hint_idx.expect("single-page hint");
            assert!(alloc.extents[idx].is_some_and(|extent| extent.pages > 0));
        }
        for (class_idx, class_pages) in CONTIG_SIZE_CLASSES.iter().enumerate() {
            if let Some(idx) = alloc.run_hint_by_class[class_idx] {
                assert!(alloc.extents[idx].is_some_and(|extent| extent.pages >= *class_pages));
            } else {
                assert!(largest < *class_pages);
            }
        }

        let refs: Vec<FrameRefCount> = alloc
            .frame_refs
            .iter()
            .filter_map(|entry| match entry {
                FrameRefSlot::Occupied(frame_ref) => Some(*frame_ref),
                FrameRefSlot::Empty | FrameRefSlot::Tombstone => None,
            })
            .collect();
        assert_eq!(
            refs.len(),
            alloc.total_frames.saturating_sub(alloc.free_frames),
            "one ref entry per allocated/reserved frame"
        );
        for (idx, entry) in refs.iter().enumerate() {
            assert_ne!(entry.refs, 0);
            assert!(entry.phys.is_multiple_of(PAGE_SIZE_U64));
            assert!(entry.phys >= alloc.base_phys && entry.phys < alloc.end_phys_exclusive);
            assert!(
                !is_in_free_extent(alloc, entry.phys),
                "tracked frame must not also be free"
            );
            for other in refs.iter().skip(idx + 1) {
                assert_ne!(entry.phys, other.phys, "duplicate frame_ref entry");
            }
        }
    }

    fn assert_allocator_state_eq_ignoring_telemetry(
        left: &PhysicalFrameAllocator,
        right: &PhysicalFrameAllocator,
    ) {
        // Clone is fine in hosted-dev tests (MAX_TRACKED_FRAME_REFS=256, ~12 KB).
        let mut left = left.clone();
        let mut right = right.clone();
        left.frame_ref_telemetry = FrameRefTelemetry::default();
        right.frame_ref_telemetry = FrameRefTelemetry::default();
        assert_eq!(left, right);
    }

    fn tracked_ref_count(alloc: &PhysicalFrameAllocator) -> usize {
        alloc
            .frame_refs
            .iter()
            .filter(|entry| matches!(entry, FrameRefSlot::Occupied(_)))
            .count()
    }

    fn tombstone_count(alloc: &PhysicalFrameAllocator) -> usize {
        alloc
            .frame_refs
            .iter()
            .filter(|entry| matches!(entry, FrameRefSlot::Tombstone))
            .count()
    }

    #[test]
    fn allocation_refreshes_largest_metadata_for_equal_largest_extents() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[
                MemoryRegion {
                    start: 0x3100_0000,
                    len: 8 * PAGE_SIZE_U64,
                    usable: true,
                },
                MemoryRegion {
                    start: 0x3102_0000,
                    len: 8 * PAGE_SIZE_U64,
                    usable: true,
                },
            ])
            .expect("init");

        assert_eq!(alloc.largest_free_run_pages, 8);
        let first = alloc
            .alloc_frame()
            .expect("alloc first page from first largest run");
        assert_eq!(first, 0x3100_0000);
        assert_eq!(alloc.largest_free_run_pages, 8);
        assert_allocator_invariants(&alloc);

        let second_run = alloc
            .alloc_contiguous(8)
            .expect("equal largest run must not be hidden by stale low max");
        assert_eq!(second_run, 0x3102_0000);
        assert_allocator_invariants(&alloc);
    }

    #[test]
    fn allocation_refreshes_largest_metadata_when_largest_is_exhausted() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[
                MemoryRegion {
                    start: 0x3200_0000,
                    len: PAGE_SIZE_U64,
                    usable: true,
                },
                MemoryRegion {
                    start: 0x3201_0000,
                    len: 4 * PAGE_SIZE_U64,
                    usable: true,
                },
            ])
            .expect("init");

        let small = alloc.alloc_frame().expect("exhaust first extent");
        assert_eq!(small, 0x3200_0000);
        assert_eq!(alloc.largest_free_run_pages, 4);
        assert_allocator_invariants(&alloc);
    }

    // Stage 136: frame_ref_slot no longer compacts on lookup (compaction moved to
    // insert path only). This test verifies the new behaviour: lookup is correct
    // with tombstones, but does NOT trigger compaction.
    #[test]
    fn frame_ref_slot_lookup_correct_with_tombstones_no_compact() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc.base_phys = 0;
        alloc.end_phys_exclusive = ((MAX_TRACKED_FRAME_REFS + 2) as u64) * PAGE_SIZE_U64;
        alloc.total_frames = 1;
        alloc.free_frames = 0;
        alloc.initialized = true;
        for slot in &mut alloc.frame_refs {
            *slot = FrameRefSlot::Tombstone;
        }
        let live_phys = PAGE_SIZE_U64;
        let live_slot = alloc.frame_ref_hash(live_phys);
        alloc.frame_refs[live_slot] = FrameRefSlot::Occupied(FrameRefCount {
            phys: live_phys,
            refs: 1,
        });

        assert_eq!(tombstone_count(&alloc), MAX_TRACKED_FRAME_REFS - 1);
        // Lookup of a missing entry: returns None without compacting.
        assert!(alloc.frame_ref_slot(2 * PAGE_SIZE_U64).is_none());
        // Stage 136: tombstones are NOT cleared by frame_ref_slot.
        assert_eq!(tombstone_count(&alloc), MAX_TRACKED_FRAME_REFS - 1);
        // The live entry is still found correctly despite tombstones.
        assert_eq!(tracked_ref_count(&alloc), 1);
        assert_eq!(alloc.frame_refcount(live_phys).expect("live ref"), 1);
    }

    // Compaction via compact_frame_refs (called from the insert path) still works.
    #[test]
    fn frame_ref_compaction_purges_tombstones_and_preserves_entries() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc.base_phys = 0;
        alloc.end_phys_exclusive = ((MAX_TRACKED_FRAME_REFS + 2) as u64) * PAGE_SIZE_U64;
        alloc.total_frames = 1;
        alloc.free_frames = 0;
        alloc.initialized = true;
        for slot in &mut alloc.frame_refs {
            *slot = FrameRefSlot::Tombstone;
        }
        let live_phys = PAGE_SIZE_U64;
        let live_slot = alloc.frame_ref_hash(live_phys);
        alloc.frame_refs[live_slot] = FrameRefSlot::Occupied(FrameRefCount {
            phys: live_phys,
            refs: 1,
        });

        assert_eq!(tombstone_count(&alloc), MAX_TRACKED_FRAME_REFS - 1);
        alloc.compact_frame_refs();
        assert_eq!(tombstone_count(&alloc), 0);
        assert_eq!(tracked_ref_count(&alloc), 1);
        assert_eq!(alloc.frame_refcount(live_phys).expect("live ref"), 1);

        // After compaction the table has Empty slots; a miss terminates early.
        let before_probes = alloc.frame_ref_telemetry.lookup_probes_total;
        assert!(alloc.frame_ref_slot(3 * PAGE_SIZE_U64).is_none());
        let miss_probes = alloc.frame_ref_telemetry.lookup_probes_total - before_probes;
        assert!(
            miss_probes < MAX_TRACKED_FRAME_REFS as u64,
            "post-compact miss should stop at an Empty slot, not scan the full table"
        );
    }

    #[test]
    fn frame_ref_tombstone_churn_compacts_before_capacity_failure() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[MemoryRegion {
                start: 0x3300_0000,
                len: 64 * PAGE_SIZE_U64,
                usable: true,
            }])
            .expect("init");

        for _ in 0..32 {
            let frame = alloc.alloc_frame().expect("alloc churn frame");
            alloc.free_frame(frame).expect("free churn frame");
        }
        assert!(tombstone_count(&alloc) > 0);
        alloc.compact_frame_refs();
        assert_eq!(tombstone_count(&alloc), 0);
        assert_eq!(tracked_ref_count(&alloc), 0);
        assert_allocator_invariants(&alloc);
    }

    #[test]
    fn global_reserved_ranges_report_capacity_instead_of_dropping() {
        let mut ranges = GlobalReservedRanges::new();
        for idx in 0..MAX_GLOBAL_RESERVED {
            let start = 0x9000_0000 + (idx as u64) * 0x2000;
            ranges
                .add(start, start + PAGE_SIZE_U64)
                .expect("capacity fill succeeds");
        }
        assert_eq!(ranges.count, MAX_GLOBAL_RESERVED);
        assert_eq!(
            ranges.add(0xA000_0000, 0xA000_0000 + PAGE_SIZE_U64),
            Err(FrameAllocError::CapacityExceeded)
        );
        assert!(ranges.find_overlap(0xA000_0000).is_none());
        ranges
            .add(0x9000_0000, 0x9000_0000 + PAGE_SIZE_U64)
            .expect("duplicate remains idempotent at capacity");
        ranges
            .add(0xB000_1000, 0xB000_0000)
            .expect("invalid empty range remains ignored");
    }

    #[test]
    fn align_up_checked_rejects_overflow_instead_of_wrapping() {
        assert_eq!(align_up_checked(u64::MAX), None);
        assert_eq!(align_up_checked(u64::MAX - 1), None);
        assert_eq!(align_up_checked(u64::MAX - PAGE_SIZE_U64 + 2), None);
        assert_eq!(
            align_up_checked(u64::MAX - PAGE_SIZE_U64 + 1),
            Some(u64::MAX - PAGE_SIZE_U64 + 1)
        );
    }

    #[test]
    fn memory_map_overflow_region_is_rejected() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        assert_eq!(
            alloc.init_from_memory_map(&[MemoryRegion {
                start: u64::MAX - PAGE_SIZE_U64,
                len: PAGE_SIZE_U64 + 1,
                usable: true,
            }]),
            Err(FrameAllocError::InvalidMemoryMap)
        );
        assert_eq!(alloc.base_phys, 0);
        assert_eq!(alloc.end_phys_exclusive, 0);
    }

    #[test]
    fn allocates_and_frees_from_usable_region() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[MemoryRegion {
                start: 0x1000_0000,
                len: 0x20_000,
                usable: true,
            }])
            .expect("init");

        let a = alloc.alloc_frame().expect("a");
        let b = alloc.alloc_frame().expect("b");
        assert_eq!(a, 0x1000_0000);
        assert_eq!(b, 0x1000_1000);
        alloc.free_frame(a).expect("free");
        let c = alloc.alloc_frame().expect("c");
        assert_eq!(c, a);
    }

    #[test]
    fn respects_holes_in_memory_map() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[
                MemoryRegion {
                    start: 0x1000_0000,
                    len: 0x1000,
                    usable: true,
                },
                MemoryRegion {
                    start: 0x1000_1000,
                    len: 0x1000,
                    usable: false,
                },
                MemoryRegion {
                    start: 0x1000_2000,
                    len: 0x1000,
                    usable: true,
                },
            ])
            .expect("init");

        let first = alloc.alloc_frame().expect("first");
        let second = alloc.alloc_frame().expect("second");
        assert_eq!(first, 0x1000_0000);
        assert_eq!(second, 0x1000_2000);
    }

    #[test]
    fn alloc_and_free_contiguous_ranges() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[MemoryRegion {
                start: 0x2000_0000,
                len: 0x10_000,
                usable: true,
            }])
            .expect("init");

        let base = alloc.alloc_contiguous(4).expect("alloc 4 pages");
        assert_eq!(base, 0x2000_0000);
        alloc.free_contiguous(base, 4).expect("free contiguous");
        let next = alloc.alloc_contiguous(2).expect("alloc 2 pages");
        assert_eq!(next, 0x2000_0000);
    }

    #[test]
    fn run_metadata_tracks_largest_free_extent() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[
                MemoryRegion {
                    start: 0x3000_0000,
                    len: 0x4_000,
                    usable: true,
                },
                MemoryRegion {
                    start: 0x3001_0000,
                    len: 0x20_000,
                    usable: true,
                },
            ])
            .expect("init");

        assert!(alloc.largest_free_run_pages >= 0x20_000 / PAGE_SIZE);
        let _ = alloc.alloc_contiguous(8).expect("alloc 8 pages");
        assert!(alloc.largest_free_run_pages >= 4);
    }

    #[test]
    fn single_page_hint_survives_fragmentation_pressure() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[MemoryRegion {
                start: 0x4000_0000,
                len: 0x20_000,
                usable: true,
            }])
            .expect("init");

        for step in 0..8usize {
            let keep = alloc.alloc_frame().expect("keep");
            let scratch = alloc.alloc_frame().expect("scratch");
            alloc.free_frame(scratch).expect("free scratch");
            assert!(alloc.single_page_hint_idx.is_some(), "step={step}");
            assert!(alloc.free_frames() > 0);
            let _ = keep;
        }
    }

    #[test]
    fn long_run_fragmentation_stress_keeps_allocator_usable() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[MemoryRegion {
                start: 0x5000_0000,
                len: 0x200_000,
                usable: true,
            }])
            .expect("init");

        let initial_free = alloc.free_frames();
        let mut held: Vec<u64> = Vec::new();

        for cycle in 0..64usize {
            while held.len() < 256 {
                held.push(alloc.alloc_frame().expect("alloc"));
            }

            for idx in (cycle % 2..held.len()).step_by(2) {
                let phys = held[idx];
                alloc.free_frame(phys).expect("free");
            }
            held = held
                .into_iter()
                .enumerate()
                .filter_map(|(idx, phys)| {
                    if idx % 2 == cycle % 2 {
                        None
                    } else {
                        Some(phys)
                    }
                })
                .collect();

            let run = alloc
                .alloc_contiguous(8)
                .expect("contig after fragmentation");
            alloc.free_contiguous(run, 8).expect("free contig");
        }

        for phys in held {
            alloc.free_frame(phys).expect("final free");
        }
        assert_eq!(alloc.free_frames(), initial_free);
    }

    #[test]
    fn throughput_smoke_many_alloc_free_operations() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[MemoryRegion {
                start: 0x6000_0000,
                len: 0x400_000,
                usable: true,
            }])
            .expect("init");

        let mut seed: u64 = 0x1234_5678_9abc_def0;
        let mut held: Vec<u64> = Vec::new();
        let initial_free = alloc.free_frames();

        for _ in 0..20_000usize {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let do_alloc = (seed & 1) == 0 || held.is_empty();
            if do_alloc {
                if let Ok(frame) = alloc.alloc_frame() {
                    held.push(frame);
                }
            } else {
                let idx = (seed as usize) % held.len();
                let phys = held.swap_remove(idx);
                alloc.free_frame(phys).expect("free");
            }
        }

        while let Some(phys) = held.pop() {
            alloc.free_frame(phys).expect("drain");
        }
        assert_eq!(alloc.free_frames(), initial_free);
    }

    #[test]
    fn reserve_frame_middle_split_tracks_once_and_preserves_accounting() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[MemoryRegion {
                start: 0x7000_0000,
                len: 0x5_000,
                usable: true,
            }])
            .expect("init");
        let initial_free = alloc.free_frames();
        let reserved = 0x7000_2000;

        alloc.reserve_frame(reserved).expect("reserve middle");

        assert_eq!(alloc.free_frames(), initial_free - 1);
        assert_eq!(alloc.frame_refcount(reserved).expect("refcount"), 1);
        assert_eq!(
            live_extents(&alloc),
            crate::std::vec![
                FreeExtent {
                    start_phys: 0x7000_0000,
                    pages: 2,
                },
                FreeExtent {
                    start_phys: 0x7000_3000,
                    pages: 2,
                },
            ]
        );
        assert_allocator_invariants(&alloc);
    }

    #[test]
    fn reserve_frame_first_last_and_only_page_cases() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[MemoryRegion {
                start: 0x7100_0000,
                len: 0x3_000,
                usable: true,
            }])
            .expect("init");

        alloc.reserve_frame(0x7100_0000).expect("reserve first");
        assert_eq!(alloc.frame_refcount(0x7100_0000).expect("ref"), 1);
        assert_eq!(
            live_extents(&alloc),
            crate::std::vec![FreeExtent {
                start_phys: 0x7100_1000,
                pages: 2,
            }]
        );
        assert_allocator_invariants(&alloc);

        alloc.reserve_frame(0x7100_2000).expect("reserve last");
        assert_eq!(alloc.frame_refcount(0x7100_2000).expect("ref"), 1);
        assert_eq!(
            live_extents(&alloc),
            crate::std::vec![FreeExtent {
                start_phys: 0x7100_1000,
                pages: 1,
            }]
        );
        assert_allocator_invariants(&alloc);

        alloc.reserve_frame(0x7100_1000).expect("reserve only");
        assert_eq!(alloc.frame_refcount(0x7100_1000).expect("ref"), 1);
        assert_eq!(alloc.free_frames(), 0);
        assert!(live_extents(&alloc).is_empty());
        assert_allocator_invariants(&alloc);
    }

    #[test]
    fn reserve_frame_middle_split_capacity_failure_is_atomic() {
        let mut regions = Vec::new();
        regions.push(MemoryRegion {
            start: 0x7200_0000,
            len: 0x3_000,
            usable: true,
        });
        for idx in 1..MAX_FREE_EXTENTS {
            regions.push(MemoryRegion {
                start: 0x7210_0000 + (idx as u64) * 0x2_000,
                len: 0x1_000,
                usable: true,
            });
        }

        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&regions)
            .expect("init full extents");
        assert_eq!(extent_count(&alloc), MAX_FREE_EXTENTS);
        let before = alloc.clone();

        assert_eq!(
            alloc.reserve_frame(0x7200_1000),
            Err(FrameAllocError::CapacityExceeded)
        );
        assert_allocator_state_eq_ignoring_telemetry(&alloc, &before);
        assert_allocator_invariants(&alloc);
    }

    #[test]
    fn free_contiguous_returns_large_block_as_single_extent() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[MemoryRegion {
                start: 0x7300_0000,
                len: 0x20_000,
                usable: true,
            }])
            .expect("init");
        let initial_free = alloc.free_frames();
        let base = alloc.alloc_contiguous(32).expect("alloc 32");
        assert_eq!(alloc.free_frames(), 0);

        alloc.free_contiguous(base, 32).expect("free 32");

        assert_eq!(alloc.free_frames(), initial_free);
        assert_eq!(
            live_extents(&alloc),
            crate::std::vec![FreeExtent {
                start_phys: base,
                pages: 32,
            }]
        );
        assert_allocator_invariants(&alloc);
    }

    #[test]
    fn free_contiguous_partial_refcounts_return_only_zero_ref_runs() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[MemoryRegion {
                start: 0x7400_0000,
                len: 0x4_000,
                usable: true,
            }])
            .expect("init");
        let base = alloc.alloc_contiguous(4).expect("alloc 4");
        alloc.retain_frame(base + PAGE_SIZE_U64).expect("retain p1");
        alloc
            .retain_frame(base + 2 * PAGE_SIZE_U64)
            .expect("retain p2");

        alloc.free_contiguous(base, 4).expect("partial free");

        assert_eq!(alloc.free_frames(), 2);
        assert_eq!(alloc.frame_refcount(base).expect("p0"), 0);
        assert_eq!(alloc.frame_refcount(base + PAGE_SIZE_U64).expect("p1"), 1);
        assert_eq!(
            alloc.frame_refcount(base + 2 * PAGE_SIZE_U64).expect("p2"),
            1
        );
        assert_eq!(
            alloc.frame_refcount(base + 3 * PAGE_SIZE_U64).expect("p3"),
            0
        );
        assert_eq!(
            live_extents(&alloc),
            crate::std::vec![
                FreeExtent {
                    start_phys: base,
                    pages: 1,
                },
                FreeExtent {
                    start_phys: base + 3 * PAGE_SIZE_U64,
                    pages: 1,
                },
            ]
        );
        assert_allocator_invariants(&alloc);

        alloc
            .free_contiguous(base + PAGE_SIZE_U64, 2)
            .expect("free retained middle");
        assert_eq!(
            live_extents(&alloc),
            crate::std::vec![FreeExtent {
                start_phys: base,
                pages: 4,
            }]
        );
        assert_allocator_invariants(&alloc);
    }

    #[test]
    fn free_contiguous_insert_failure_preserves_state() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc.base_phys = 0x7500_0000;
        alloc.end_phys_exclusive =
            alloc.base_phys + ((MAX_FREE_EXTENTS as u64) * 0x2_000) + 0x10_000;
        alloc.total_frames = MAX_FREE_EXTENTS + 1;
        alloc.free_frames = MAX_FREE_EXTENTS;
        alloc.initialized = true;
        for idx in 0..MAX_FREE_EXTENTS {
            alloc.extents[idx] = Some(FreeExtent {
                start_phys: alloc.base_phys + 0x10_000 + (idx as u64) * 0x2_000,
                pages: 1,
            });
        }
        alloc.frame_refs[0] = FrameRefSlot::Occupied(FrameRefCount {
            phys: alloc.base_phys,
            refs: 1,
        });
        alloc.refresh_run_metadata();
        let before = alloc.clone();

        assert_eq!(
            alloc.free_contiguous(alloc.base_phys, 1),
            Err(FrameAllocError::CapacityExceeded)
        );
        assert_allocator_state_eq_ignoring_telemetry(&alloc, &before);
    }

    #[test]
    fn frame_ref_hash_tracks_many_unique_frames_with_bounded_probes() {
        let pages = MAX_TRACKED_FRAME_REFS.min(128);
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[MemoryRegion {
                start: 0x7700_0000,
                len: (pages as u64) * PAGE_SIZE_U64,
                usable: true,
            }])
            .expect("init");

        let base = alloc.alloc_contiguous(pages).expect("alloc many");
        assert_eq!(tracked_ref_count(&alloc), pages);
        assert!(alloc.frame_ref_telemetry.insert_probes_total >= pages as u64);
        assert!(alloc.frame_ref_telemetry.insert_max_probes >= 1);
        assert_allocator_invariants(&alloc);

        alloc.free_contiguous(base, pages).expect("free many");
        assert_eq!(tracked_ref_count(&alloc), 0);
        assert_allocator_invariants(&alloc);
    }

    #[test]
    fn frame_ref_hash_handles_collision_heavy_inc_dec_and_tombstones() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc.base_phys = 0;
        alloc.end_phys_exclusive = (MAX_TRACKED_FRAME_REFS as u64) * PAGE_SIZE_U64 * 64;
        alloc.total_frames = 32;
        alloc.free_frames = 0;
        alloc.initialized = true;
        let colliding = MAX_TRACKED_FRAME_REFS.min(32);

        for idx in 0..colliding {
            let phys = (idx as u64) * (MAX_TRACKED_FRAME_REFS as u64) * PAGE_SIZE_U64;
            alloc.track_new_frame_ref(phys).expect("track collision");
        }
        assert!(alloc.frame_ref_telemetry.insert_max_probes >= colliding);

        let middle = ((colliding / 2) as u64) * (MAX_TRACKED_FRAME_REFS as u64) * PAGE_SIZE_U64;
        let tail = ((colliding - 1) as u64) * (MAX_TRACKED_FRAME_REFS as u64) * PAGE_SIZE_U64;
        assert_eq!(alloc.retain_frame(tail).expect("retain tail"), 2);
        assert_eq!(alloc.untrack_frame_ref(middle).expect("delete middle"), 0);
        alloc.extents[0] = Some(FreeExtent {
            start_phys: middle,
            pages: 1,
        });
        alloc.free_frames = 1;
        alloc.refresh_run_metadata();
        assert_eq!(alloc.frame_refcount(tail).expect("tail lookup"), 2);
        assert_eq!(alloc.untrack_frame_ref(tail).expect("drop tail retain"), 1);
        assert_eq!(alloc.frame_refcount(tail).expect("tail after drop"), 1);
        assert!(alloc.frame_ref_telemetry.lookup_max_probes >= colliding);
        assert_allocator_invariants(&alloc);
    }

    #[test]
    fn frame_ref_hash_capacity_failure_does_not_insert() {
        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc.base_phys = 0;
        alloc.end_phys_exclusive = ((MAX_TRACKED_FRAME_REFS + 1) as u64) * PAGE_SIZE_U64;
        alloc.total_frames = MAX_TRACKED_FRAME_REFS;
        alloc.free_frames = 0;
        alloc.initialized = true;

        for idx in 0..MAX_TRACKED_FRAME_REFS {
            alloc
                .track_new_frame_ref((idx as u64) * PAGE_SIZE_U64)
                .expect("fill frame ref table");
        }
        let before = alloc.clone();
        assert_eq!(
            alloc.track_new_frame_ref((MAX_TRACKED_FRAME_REFS as u64) * PAGE_SIZE_U64),
            Err(FrameAllocError::CapacityExceeded)
        );
        assert_allocator_state_eq_ignoring_telemetry(&alloc, &before);
        assert_eq!(tracked_ref_count(&alloc), MAX_TRACKED_FRAME_REFS);
        assert!(alloc.frame_ref_telemetry.insert_capacity_failures > 0);
        assert_allocator_invariants(&alloc);
    }

    #[test]
    fn pt_contiguous_free_uses_block_free_accounting_roundtrip() {
        let base = 0x7600_0000;
        init_pt_frame_allocator(&[MemoryRegion {
            start: base,
            len: 0x8_000,
            usable: true,
        }])
        .expect("init pt allocator");

        let run = alloc_pt_contiguous_frames(4).expect("alloc pt run");
        assert_eq!(run, base);
        free_pt_contiguous_frames(run, 4).expect("free pt run");
        let run_again = alloc_pt_contiguous_frames(4).expect("alloc pt run again");
        assert_eq!(run_again, base);
        free_pt_contiguous_frames(run_again, 4).expect("free pt run again");
    }

    // ── PT/main pool split regression tests ─────────────────────────────────
    //
    // These tests exercise the is_pa_in_pt_pool / is_pa_reserved query helpers
    // and verify that allocators seeded from disjoint ranges never return
    // frames from the wrong pool.  They use PA ranges in the 0xE/0xF_0000_0000
    // space to avoid colliding with the earlier single-allocator tests.

    #[test]
    fn pt_pool_registration_and_query() {
        // Registering a PT pool range makes is_pa_in_pt_pool return Some for
        // addresses inside it and None for addresses strictly outside.
        let pt_start: u64 = 0xE000_0000;
        let pt_end: u64 = pt_start + 256 * 0x1000;
        register_pt_pool_range(pt_start, pt_end).expect("register PT pool");

        // First and last pages of the registered range must be found.
        assert!(
            is_pa_in_pt_pool(pt_start).is_some(),
            "start of PT pool must be found"
        );
        assert!(
            is_pa_in_pt_pool(pt_end - 0x1000).is_some(),
            "last page of PT pool must be found"
        );
        // Addresses outside the range must not be found.
        assert!(
            is_pa_in_pt_pool(pt_end).is_none(),
            "exclusive end must not be in PT pool"
        );
        assert!(
            is_pa_in_pt_pool(pt_start.wrapping_sub(0x1000)).is_none(),
            "address before PT pool must not be found"
        );
    }

    #[test]
    fn alloc_from_pt_seeded_range_stays_in_pt_pool() {
        // Every frame returned by an allocator seeded from a PT-pool range must
        // lie within that range.
        let pt_start: u64 = 0xE100_0000;
        let pt_end: u64 = pt_start + 8 * 0x1000;
        let mut pt_alloc = PhysicalFrameAllocator::new_uninit();
        pt_alloc
            .init_from_memory_map(&[MemoryRegion {
                start: pt_start,
                len: pt_end - pt_start,
                usable: true,
            }])
            .expect("init pt alloc");
        while pt_alloc.free_frames() > 0 {
            let pa = pt_alloc.alloc_frame().expect("alloc");
            assert!(
                pa >= pt_start && pa < pt_end,
                "PT frame {:#x} outside PT pool [{:#x}..{:#x})",
                pa,
                pt_start,
                pt_end
            );
        }
    }

    #[test]
    fn alloc_from_main_seeded_range_stays_in_main_pool() {
        // Every frame returned by a main allocator seeded from a main-pool range
        // must lie within that range.
        let main_start: u64 = 0xE200_0000;
        let main_end: u64 = main_start + 8 * 0x1000;
        let mut main_alloc = PhysicalFrameAllocator::new_uninit();
        main_alloc
            .init_from_memory_map(&[MemoryRegion {
                start: main_start,
                len: main_end - main_start,
                usable: true,
            }])
            .expect("init main alloc");
        while main_alloc.free_frames() > 0 {
            let pa = main_alloc.alloc_frame().expect("alloc");
            assert!(
                pa >= main_start && pa < main_end,
                "main frame {:#x} outside main pool [{:#x}..{:#x})",
                pa,
                main_start,
                main_end
            );
        }
    }

    #[test]
    fn main_allocator_never_returns_frame_inside_pt_pool_range() {
        // Set up two strictly disjoint ranges: PT pool immediately before main
        // pool.  Exhaust the main allocator and verify none of its frames fall
        // in the PT pool range (as reported by is_pa_in_pt_pool).
        let pt_start: u64 = 0xE300_0000;
        let pt_end: u64 = pt_start + 16 * 0x1000;
        let main_start: u64 = pt_end; // immediately after PT pool, no gap
        let main_end: u64 = main_start + 16 * 0x1000;

        register_pt_pool_range(pt_start, pt_end).expect("register PT pool");

        let mut main_alloc = PhysicalFrameAllocator::new_uninit();
        main_alloc
            .init_from_memory_map(&[MemoryRegion {
                start: main_start,
                len: main_end - main_start,
                usable: true,
            }])
            .expect("init");

        while main_alloc.free_frames() > 0 {
            let pa = main_alloc.alloc_frame().expect("alloc");
            assert!(
                is_pa_in_pt_pool(pa).is_none(),
                "main allocator returned PT-pool address {:#x} (PT pool [{:#x}..{:#x}))",
                pa,
                pt_start,
                pt_end
            );
        }
    }

    #[test]
    fn reserved_range_registration_and_query() {
        // Registering a reserved range makes is_pa_reserved return Some for
        // addresses within it and None for addresses outside.
        let res_start: u64 = 0xE400_0000;
        let res_end: u64 = res_start + 4 * 0x1000;
        register_reserved_range(res_start, res_end).expect("register reserved range");

        assert!(
            is_pa_reserved(res_start).is_some(),
            "start of reserved range must be found"
        );
        assert!(
            is_pa_reserved(res_end - 0x1000).is_some(),
            "last page of reserved range must be found"
        );
        assert!(
            is_pa_reserved(res_end).is_none(),
            "exclusive end must not be reserved"
        );
        assert!(
            is_pa_reserved(res_start.wrapping_sub(0x1000)).is_none(),
            "address before reserved range must not be found"
        );
    }

    #[test]
    fn allocator_seeded_below_reserved_range_does_not_touch_reserved() {
        // Allocator seeded strictly below a reserved range must never return
        // an address the reserved-range query considers reserved.
        let usable_start: u64 = 0xE500_0000;
        let usable_end: u64 = usable_start + 8 * 0x1000;
        let reserved_start: u64 = usable_end; // reserved range starts right after usable
        let reserved_end: u64 = reserved_start + 8 * 0x1000;
        register_reserved_range(reserved_start, reserved_end).expect("register reserved range");

        let mut alloc = PhysicalFrameAllocator::new_uninit();
        alloc
            .init_from_memory_map(&[MemoryRegion {
                start: usable_start,
                len: usable_end - usable_start,
                usable: true,
            }])
            .expect("init");

        while alloc.free_frames() > 0 {
            let pa = alloc.alloc_frame().expect("alloc");
            assert!(
                is_pa_reserved(pa).is_none(),
                "allocator returned reserved address {:#x} (reserved [{:#x}..{:#x}))",
                pa,
                reserved_start,
                reserved_end
            );
        }
    }

    #[test]
    #[cfg(feature = "hosted-dev")]
    fn hosted_profile_uses_conservative_allocator_knobs() {
        assert_eq!(MAX_FREE_EXTENTS, 256);
        assert_eq!(CONTIG_SIZE_CLASSES, [1, 2, 4, 8, 16, 32, 64, 128]);
    }

    #[test]
    #[cfg(not(feature = "hosted-dev"))]
    fn non_hosted_profile_uses_throughput_allocator_knobs() {
        assert_eq!(MAX_FREE_EXTENTS, 512);
        assert_eq!(CONTIG_SIZE_CLASSES, [1, 2, 4, 8, 16, 32, 64, 128, 256, 512]);
    }
}
