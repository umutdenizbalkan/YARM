use crate::kernel::lock::SpinLock;
use crate::kernel::vm::PAGE_SIZE;

const PAGE_SIZE_U64: u64 = PAGE_SIZE as u64;
const MAX_FREE_EXTENTS: usize = 512;
const CONTIG_SIZE_CLASSES: [usize; 10] = [1, 2, 4, 8, 16, 32, 64, 128, 256, 512];

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

#[derive(Debug, Clone, Copy)]
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
            min_phys = min_phys.min(align_down(region.start));
            max_phys = max_phys.max(align_up(region.start.saturating_add(region.len)));
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

        for region in regions {
            if !region.usable || region.len == 0 {
                continue;
            }
            let start = align_up(region.start);
            let end = align_down(region.start.saturating_add(region.len));
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

        let hint_idx = self
            .single_page_hint_idx
            .filter(|&idx| self.extents[idx].is_some_and(|extent| extent.pages > 0))
            .or_else(|| self.find_extent_index(1));
        let Some(idx) = hint_idx else {
            return Err(FrameAllocError::OutOfMemory);
        };

        let (alloc_phys, old_pages, new_pages) = self.split_extent_for_allocation(idx, 1)?;
        self.free_frames = self.free_frames.saturating_sub(1);
        self.update_hints_after_allocation(idx, old_pages, new_pages);
        Ok(alloc_phys)
    }

    pub fn alloc_contiguous(&mut self, pages: usize) -> Result<u64, FrameAllocError> {
        if !self.initialized {
            return Err(FrameAllocError::InvalidMemoryMap);
        }
        if pages == 0 || pages > self.free_frames || pages > self.largest_free_run_pages {
            return Err(FrameAllocError::OutOfMemory);
        }

        if let Some(idx) = self.fast_path_extent_index(pages).or_else(|| self.find_extent_index(pages)) {
            let (alloc_phys, _, _) = self.split_extent_for_allocation(idx, pages)?;
            self.free_frames = self.free_frames.saturating_sub(pages);
            self.refresh_run_metadata();
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
        for extent in self.extents.iter().flatten() {
            let extent_end = extent
                .start_phys
                .saturating_add((extent.pages as u64).saturating_mul(PAGE_SIZE_U64));
            let overlaps = start_phys < extent_end && end_phys > extent.start_phys;
            if overlaps {
                return Err(FrameAllocError::AlreadyFree);
            }
        }
        self.insert_extent(start_phys, pages)?;
        self.free_frames = self.free_frames.saturating_add(pages);
        self.refresh_run_metadata();
        Ok(())
    }

    pub fn reserve_frame(&mut self, phys: u64) -> Result<(), FrameAllocError> {
        let idx = self.frame_index(phys)?;
        if idx >= self.total_frames {
            return Err(FrameAllocError::OutOfRange);
        }
        let mut found = None;
        for (slot_idx, slot) in self.extents.iter_mut().enumerate() {
            let Some(extent) = slot else {
                continue;
            };
            let extent_end = extent
                .start_phys
                .saturating_add((extent.pages as u64).saturating_mul(PAGE_SIZE_U64));
            if phys >= extent.start_phys && phys < extent_end {
                found = Some(slot_idx);
                break;
            }
        }
        let Some(slot_idx) = found else {
            return Ok(());
        };
        let mut extent = self.extents[slot_idx].expect("extent");
        if extent.pages == 0 {
            return Ok(());
        }
        let extent_end = extent
            .start_phys
            .saturating_add((extent.pages as u64).saturating_mul(PAGE_SIZE_U64));
        if phys == extent.start_phys {
            extent.start_phys = extent.start_phys.saturating_add(PAGE_SIZE_U64);
            extent.pages -= 1;
        } else if phys + PAGE_SIZE_U64 == extent_end {
            extent.pages -= 1;
        } else {
            let left_pages = ((phys - extent.start_phys) / PAGE_SIZE_U64) as usize;
            let right_start = phys.saturating_add(PAGE_SIZE_U64);
            let right_pages = ((extent_end - right_start) / PAGE_SIZE_U64) as usize;
            extent.pages = left_pages;
            self.extents[slot_idx] = Some(extent);
            self.insert_extent(right_start, right_pages)?;
            self.free_frames = self.free_frames.saturating_sub(1);
            self.refresh_run_metadata();
            return Ok(());
        }
        if extent.pages == 0 {
            self.extents[slot_idx] = None;
        } else {
            self.extents[slot_idx] = Some(extent);
        }
        self.free_frames = self.free_frames.saturating_sub(1);
        self.refresh_run_metadata();
        Ok(())
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

    fn insert_extent(&mut self, start_phys: u64, pages: usize) -> Result<(), FrameAllocError> {
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
                if end == extent.start_phys || start == extent_end || (start < extent_end && end > extent.start_phys) {
                    start = start.min(extent.start_phys);
                    end = end.max(extent_end);
                    self.extents[idx] = None;
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
        self.sort_extents();
        Ok(())
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

    fn update_hints_after_allocation(&mut self, idx: usize, old_pages: usize, new_pages: usize) {
        self.single_page_hint_idx = if new_pages > 0 {
            Some(idx)
        } else {
            self.extents
                .iter()
                .enumerate()
                .find_map(|(probe, extent)| extent.map(|_| probe))
        };

        for (class_idx, class_pages) in CONTIG_SIZE_CLASSES.iter().enumerate() {
            if self.run_hint_by_class[class_idx] == Some(idx) && new_pages < *class_pages {
                self.run_hint_by_class[class_idx] = None;
            }
        }

        if old_pages == self.largest_free_run_pages && new_pages < old_pages {
            self.largest_free_run_pages = self.largest_free_run_pages.saturating_sub(1);
        }
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

static PT_FRAME_ALLOCATOR: SpinLock<Option<PhysicalFrameAllocator>> = SpinLock::new(None);

fn default_pt_allocator_regions() -> [MemoryRegion; 1] {
    [MemoryRegion {
        start: crate::arch::platform_layout::NEXT_ANON_PHYS_BASE + (512 * 1024 * 1024),
        len: 512 * 1024 * 1024,
        usable: true,
    }]
}

fn ensure_pt_allocator_initialized() -> Result<(), FrameAllocError> {
    let mut guard = PT_FRAME_ALLOCATOR.lock();
    if guard.is_some() {
        return Ok(());
    }
    let mut allocator = PhysicalFrameAllocator::new_uninit();
    allocator.init_from_memory_map(&default_pt_allocator_regions())?;
    *guard = Some(allocator);
    Ok(())
}

pub fn init_pt_frame_allocator(regions: &[MemoryRegion]) -> Result<(), FrameAllocError> {
    let mut allocator = PhysicalFrameAllocator::new_uninit();
    allocator.init_from_memory_map(regions)?;
    *PT_FRAME_ALLOCATOR.lock() = Some(allocator);
    Ok(())
}

pub fn alloc_pt_frame() -> Result<u64, FrameAllocError> {
    ensure_pt_allocator_initialized()?;
    let mut guard = PT_FRAME_ALLOCATOR.lock();
    guard
        .as_mut()
        .ok_or(FrameAllocError::Uninitialized)?
        .alloc_frame()
}

pub fn free_pt_frame(phys: u64) -> Result<(), FrameAllocError> {
    ensure_pt_allocator_initialized()?;
    let mut guard = PT_FRAME_ALLOCATOR.lock();
    guard
        .as_mut()
        .ok_or(FrameAllocError::Uninitialized)?
        .free_frame(phys)
}

const fn align_down(value: u64) -> u64 {
    value & !(PAGE_SIZE_U64 - 1)
}

const fn align_up(value: u64) -> u64 {
    (value + PAGE_SIZE_U64 - 1) & !(PAGE_SIZE_U64 - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::std::vec::Vec;

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

            let run = alloc.alloc_contiguous(8).expect("contig after fragmentation");
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
}
