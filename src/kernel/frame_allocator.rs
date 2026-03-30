use crate::kernel::vm::PAGE_SIZE;
use crate::kernel::lock::SpinLock;

const PAGE_SIZE_U64: u64 = PAGE_SIZE as u64;
const MAX_TRACKED_FRAMES: usize = 131_072;
const BITMAP_WORDS: usize = MAX_TRACKED_FRAMES / 64;

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

#[derive(Debug, Clone, Copy)]
pub struct PhysicalFrameAllocator {
    base_phys: u64,
    total_frames: usize,
    used_frames: usize,
    next_hint: usize,
    initialized: bool,
    bitmap: [u64; BITMAP_WORDS],
}

impl PhysicalFrameAllocator {
    pub const fn new_uninit() -> Self {
        Self {
            base_phys: 0,
            total_frames: 0,
            used_frames: 0,
            next_hint: 0,
            initialized: false,
            bitmap: [u64::MAX; BITMAP_WORDS],
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

        let total_frames = ((max_phys - min_phys) / PAGE_SIZE_U64) as usize;
        if total_frames == 0 {
            return Err(FrameAllocError::InvalidMemoryMap);
        }
        if total_frames > MAX_TRACKED_FRAMES {
            return Err(FrameAllocError::CapacityExceeded);
        }

        self.base_phys = min_phys;
        self.total_frames = total_frames;
        self.used_frames = total_frames;
        self.next_hint = 0;
        self.initialized = true;
        self.bitmap.fill(u64::MAX);

        for region in regions {
            if !region.usable || region.len == 0 {
                continue;
            }
            let start = align_up(region.start);
            let end = align_down(region.start.saturating_add(region.len));
            if end <= start {
                continue;
            }
            let start_idx = ((start - min_phys) / PAGE_SIZE_U64) as usize;
            let end_idx = ((end - min_phys) / PAGE_SIZE_U64) as usize;
            for idx in start_idx..end_idx.min(total_frames) {
                self.set_used(idx, false);
            }
        }

        self.used_frames = self.count_used();
        Ok(())
    }

    pub fn alloc_frame(&mut self) -> Result<u64, FrameAllocError> {
        if !self.initialized {
            return Err(FrameAllocError::InvalidMemoryMap);
        }
        if self.used_frames >= self.total_frames {
            return Err(FrameAllocError::OutOfMemory);
        }

        let start = self.next_hint.min(self.total_frames.saturating_sub(1));
        for pass in 0..2 {
            let range = if pass == 0 {
                start..self.total_frames
            } else {
                0..start
            };
            for idx in range {
                if !self.is_used(idx) {
                    self.set_used(idx, true);
                    self.used_frames = self.used_frames.saturating_add(1);
                    self.next_hint = idx.saturating_add(1);
                    return Ok(self.base_phys + (idx as u64 * PAGE_SIZE_U64));
                }
            }
        }

        Err(FrameAllocError::OutOfMemory)
    }

    pub fn free_frame(&mut self, phys: u64) -> Result<(), FrameAllocError> {
        let idx = self.frame_index(phys)?;
        if !self.is_used(idx) {
            return Err(FrameAllocError::AlreadyFree);
        }
        self.set_used(idx, false);
        self.used_frames = self.used_frames.saturating_sub(1);
        self.next_hint = self.next_hint.min(idx);
        Ok(())
    }

    pub fn reserve_frame(&mut self, phys: u64) -> Result<(), FrameAllocError> {
        let idx = self.frame_index(phys)?;
        if !self.is_used(idx) {
            self.set_used(idx, true);
            self.used_frames = self.used_frames.saturating_add(1);
        }
        Ok(())
    }

    pub fn total_frames(&self) -> usize {
        self.total_frames
    }

    pub fn free_frames(&self) -> usize {
        self.total_frames.saturating_sub(self.used_frames)
    }

    fn frame_index(&self, phys: u64) -> Result<usize, FrameAllocError> {
        if !self.initialized {
            return Err(FrameAllocError::InvalidMemoryMap);
        }
        if !phys.is_multiple_of(PAGE_SIZE_U64) {
            return Err(FrameAllocError::Misaligned);
        }
        if phys < self.base_phys {
            return Err(FrameAllocError::OutOfRange);
        }
        let idx = ((phys - self.base_phys) / PAGE_SIZE_U64) as usize;
        if idx >= self.total_frames {
            return Err(FrameAllocError::OutOfRange);
        }
        Ok(idx)
    }

    fn is_used(&self, idx: usize) -> bool {
        let word = idx / 64;
        let bit = idx % 64;
        ((self.bitmap[word] >> bit) & 1) != 0
    }

    fn set_used(&mut self, idx: usize, used: bool) {
        let word = idx / 64;
        let bit = idx % 64;
        let mask = 1u64 << bit;
        if used {
            self.bitmap[word] |= mask;
        } else {
            self.bitmap[word] &= !mask;
        }
    }

    fn count_used(&self) -> usize {
        let mut count = 0usize;
        for idx in 0..self.total_frames {
            if self.is_used(idx) {
                count += 1;
            }
        }
        count
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
}
