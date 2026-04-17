// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(not(feature = "hosted-dev"))]
mod non_hosted {
    use core::alloc::{GlobalAlloc, Layout};
    use core::mem::size_of;
    use core::ptr::null_mut;

    use crate::arch::platform_layout;
    use crate::kernel::frame_allocator::{
        alloc_pt_contiguous_frames, alloc_pt_frame, free_pt_contiguous_frames, free_pt_frame,
    };
    use crate::kernel::lock::SpinLockIrq;
    use crate::kernel::vm::PAGE_SIZE;

    const SLAB_CLASS_SIZES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];
    const SMALL_ALLOC_MAX: usize = 2048;
    const FREE_NONE: u16 = u16::MAX;
    const SLAB_MAGIC: u64 = 0x5941_524d_534c_4142; // "YARMSLAB"
    const LARGE_MAGIC: u64 = 0x5941_524d_4c41_5247; // "YARMLARG"

    #[derive(Clone, Copy)]
    struct AllocatorState {
        class_heads_phys: [u64; SLAB_CLASS_SIZES.len()],
    }

    impl AllocatorState {
        const fn new() -> Self {
            Self {
                class_heads_phys: [0; SLAB_CLASS_SIZES.len()],
            }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SlabPageHeader {
        magic: u64,
        class_idx: u16,
        obj_size: u16,
        capacity: u16,
        free_head: u16,
        used: u16,
        _reserved: u16,
        next_page_phys: u64,
        alloc_bitmap: [u64; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct LargeAllocHeader {
        magic: u64,
        pages: u64,
    }

    static ALLOCATOR_LOCK: SpinLockIrq<AllocatorState> = SpinLockIrq::new(AllocatorState::new());

    #[derive(Clone, Copy)]
    pub struct KernelGlobalAllocator;

    impl KernelGlobalAllocator {
        fn phys_to_ptr(phys: u64) -> *mut u8 {
            #[cfg(target_arch = "x86_64")]
            {
                let Some(virt) = platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE.checked_add(phys) else {
                    return null_mut();
                };
                return virt as usize as *mut u8;
            }
            #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
            {
                return phys as usize as *mut u8;
            }
            #[cfg(not(any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )))]
            {
                let Some(virt) = platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE.checked_add(phys) else {
                    return null_mut();
                };
                return virt as usize as *mut u8;
            }
        }

        fn ptr_to_phys(ptr: *const u8) -> u64 {
            #[cfg(target_arch = "x86_64")]
            {
                return (ptr as usize as u64)
                    .saturating_sub(platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE);
            }
            #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
            {
                return ptr as usize as u64;
            }
            #[cfg(not(any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )))]
            {
                return (ptr as usize as u64)
                    .saturating_sub(platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE);
            }
        }

        fn align_up(value: usize, align: usize) -> usize {
            if align == 0 {
                return value;
            }
            (value + align - 1) & !(align - 1)
        }

        fn slab_class_for(layout: Layout) -> Option<usize> {
            if layout.size() == 0 || !layout.align().is_power_of_two() {
                return None;
            }
            if layout.align() > PAGE_SIZE {
                return None;
            }
            let needed = layout.size().max(layout.align());
            if needed > SMALL_ALLOC_MAX {
                return None;
            }
            SLAB_CLASS_SIZES.iter().position(|&size| size >= needed)
        }

        fn bit_is_set(map: &[u64; 4], idx: usize) -> bool {
            let word = idx / 64;
            let bit = idx % 64;
            (word < map.len()) && ((map[word] & (1u64 << bit)) != 0)
        }

        fn set_bit(map: &mut [u64; 4], idx: usize) {
            let word = idx / 64;
            let bit = idx % 64;
            if word < map.len() {
                map[word] |= 1u64 << bit;
            }
        }

        fn clear_bit(map: &mut [u64; 4], idx: usize) {
            let word = idx / 64;
            let bit = idx % 64;
            if word < map.len() {
                map[word] &= !(1u64 << bit);
            }
        }

        fn slab_object_start(obj_size: usize) -> usize {
            Self::align_up(size_of::<SlabPageHeader>(), obj_size)
        }

        unsafe fn slab_init_page(page_ptr: *mut u8, class_idx: usize, next_page_phys: u64) -> bool {
            let obj_size = SLAB_CLASS_SIZES[class_idx];
            let start = Self::slab_object_start(obj_size);
            if start >= PAGE_SIZE {
                return false;
            }
            let capacity = (PAGE_SIZE - start) / obj_size;
            if capacity == 0 || capacity > 256 {
                return false;
            }

            let header = SlabPageHeader {
                magic: SLAB_MAGIC,
                class_idx: class_idx as u16,
                obj_size: obj_size as u16,
                capacity: capacity as u16,
                free_head: 0,
                used: 0,
                _reserved: 0,
                next_page_phys,
                alloc_bitmap: [0; 4],
            };
            unsafe { core::ptr::write(page_ptr as *mut SlabPageHeader, header) };

            for idx in 0..capacity {
                let next = if idx + 1 < capacity {
                    (idx + 1) as u16
                } else {
                    FREE_NONE
                };
                let slot = unsafe { page_ptr.add(start + idx * obj_size) };
                unsafe { core::ptr::write_unaligned(slot as *mut u16, next) };
            }
            true
        }

        unsafe fn slab_alloc_from_page(page_ptr: *mut u8) -> *mut u8 {
            let header = unsafe { &mut *(page_ptr as *mut SlabPageHeader) };
            if header.free_head == FREE_NONE {
                return null_mut();
            }

            let idx = header.free_head as usize;
            if idx >= header.capacity as usize {
                return null_mut();
            }

            let obj_size = header.obj_size as usize;
            let start = Self::slab_object_start(obj_size);
            let slot = unsafe { page_ptr.add(start + idx * obj_size) };
            let next = unsafe { core::ptr::read_unaligned(slot as *const u16) };
            header.free_head = next;
            header.used = header.used.saturating_add(1);
            Self::set_bit(&mut header.alloc_bitmap, idx);
            slot
        }

        unsafe fn slab_dealloc_from_page(page_ptr: *mut u8, ptr: *mut u8) -> bool {
            let header = unsafe { &mut *(page_ptr as *mut SlabPageHeader) };
            if header.magic != SLAB_MAGIC {
                return false;
            }
            let class_idx = header.class_idx as usize;
            if class_idx >= SLAB_CLASS_SIZES.len() {
                return false;
            }
            if header.obj_size as usize != SLAB_CLASS_SIZES[class_idx] {
                return false;
            }

            let obj_size = header.obj_size as usize;
            let start = Self::slab_object_start(obj_size);
            let page_addr = page_ptr as usize;
            let ptr_addr = ptr as usize;
            if ptr_addr < page_addr + start || ptr_addr >= page_addr + PAGE_SIZE {
                return false;
            }
            let rel = ptr_addr - (page_addr + start);
            if rel % obj_size != 0 {
                return false;
            }
            let idx = rel / obj_size;
            if idx >= header.capacity as usize {
                return false;
            }
            if !Self::bit_is_set(&header.alloc_bitmap, idx) {
                return false;
            }

            Self::clear_bit(&mut header.alloc_bitmap, idx);
            let slot = ptr as *mut u16;
            unsafe { core::ptr::write_unaligned(slot, header.free_head) };
            header.free_head = idx as u16;
            header.used = header.used.saturating_sub(1);
            true
        }

        unsafe fn alloc_small(state: &mut AllocatorState, class_idx: usize) -> *mut u8 {
            let mut phys = state.class_heads_phys[class_idx];
            while phys != 0 {
                let page_ptr = Self::phys_to_ptr(phys);
                if page_ptr.is_null() {
                    break;
                }
                let header = unsafe { &*(page_ptr as *const SlabPageHeader) };
                if header.magic == SLAB_MAGIC
                    && header.class_idx as usize == class_idx
                    && header.free_head != FREE_NONE
                {
                    return unsafe { Self::slab_alloc_from_page(page_ptr) };
                }
                phys = header.next_page_phys;
            }

            let Ok(new_phys) = alloc_pt_frame() else {
                return null_mut();
            };
            let page_ptr = Self::phys_to_ptr(new_phys);
            if page_ptr.is_null() {
                let _ = free_pt_frame(new_phys);
                return null_mut();
            }

            if !unsafe { Self::slab_init_page(page_ptr, class_idx, state.class_heads_phys[class_idx]) } {
                let _ = free_pt_frame(new_phys);
                return null_mut();
            }
            state.class_heads_phys[class_idx] = new_phys;
            unsafe { Self::slab_alloc_from_page(page_ptr) }
        }

        unsafe fn alloc_large(layout: Layout) -> *mut u8 {
            if layout.align() > PAGE_SIZE || !layout.align().is_power_of_two() {
                return null_mut();
            }
            let payload_plus_header = layout.size().saturating_add(size_of::<LargeAllocHeader>());
            let payload_pages = payload_plus_header.div_ceil(PAGE_SIZE).max(1);
            let total_pages = payload_pages.saturating_add(1);
            let Ok(base_phys) = alloc_pt_contiguous_frames(total_pages) else {
                return null_mut();
            };
            let base_ptr = Self::phys_to_ptr(base_phys);
            if base_ptr.is_null() {
                let _ = free_pt_contiguous_frames(base_phys, total_pages);
                return null_mut();
            }
            let header = LargeAllocHeader {
                magic: LARGE_MAGIC,
                pages: total_pages as u64,
            };
            unsafe { core::ptr::write(base_ptr as *mut LargeAllocHeader, header) };
            unsafe { base_ptr.add(PAGE_SIZE) }
        }
    }

    unsafe impl GlobalAlloc for KernelGlobalAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if layout.size() == 0 {
                return null_mut();
            }

            let mut state = ALLOCATOR_LOCK.lock();
            if let Some(class_idx) = Self::slab_class_for(layout) {
                return unsafe { Self::alloc_small(&mut state, class_idx) };
            }
            unsafe { Self::alloc_large(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
            if ptr.is_null() {
                return;
            }

            let _state = ALLOCATOR_LOCK.lock();

            let page_base = (ptr as usize) & !(PAGE_SIZE - 1);
            let page_ptr = page_base as *mut u8;
            if !page_ptr.is_null() {
                let magic = unsafe { core::ptr::read_unaligned(page_ptr as *const u64) };
                if magic == SLAB_MAGIC {
                    let _ = unsafe { Self::slab_dealloc_from_page(page_ptr, ptr) };
                    return;
                }
            }

            if !(ptr as usize).is_multiple_of(PAGE_SIZE) {
                return;
            }

            let header_ptr = unsafe { ptr.sub(PAGE_SIZE) as *const LargeAllocHeader };
            #[cfg(target_arch = "x86_64")]
            if (header_ptr as usize as u64) < platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE {
                return;
            }

            let header = unsafe { core::ptr::read(header_ptr) };
            if header.magic != LARGE_MAGIC {
                return;
            }
            let pages = header.pages as usize;
            if pages < 2 {
                return;
            }
            let base_phys = Self::ptr_to_phys(header_ptr as *const u8);
            let _ = free_pt_contiguous_frames(base_phys, pages);
        }
    }

    pub static KERNEL_GLOBAL_ALLOCATOR: KernelGlobalAllocator = KernelGlobalAllocator;
}

#[cfg(feature = "hosted-dev")]
mod hosted {
    pub struct KernelGlobalAllocator;
    pub static KERNEL_GLOBAL_ALLOCATOR: KernelGlobalAllocator = KernelGlobalAllocator;
}

#[cfg(not(feature = "hosted-dev"))]
pub use non_hosted::KERNEL_GLOBAL_ALLOCATOR;
#[cfg(not(feature = "hosted-dev"))]
pub use non_hosted::KernelGlobalAllocator;
#[cfg(feature = "hosted-dev")]
pub use hosted::KERNEL_GLOBAL_ALLOCATOR;
#[cfg(feature = "hosted-dev")]
pub use hosted::KernelGlobalAllocator;

#[cfg(all(test, feature = "hosted-dev"))]
mod hosted_dev_allocator_model_tests {
    use std::collections::BTreeSet;
    use std::vec::Vec;

    const PAGE_SIZE: usize = 4096;
    const CLASS_SIZES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];
    const SMALL_MAX: usize = 2048;

    #[derive(Default)]
    struct ModelAlloc {
        next_addr: usize,
        free_small: [Vec<usize>; CLASS_SIZES.len()],
        live_small: BTreeSet<usize>,
        live_large: BTreeSet<usize>,
    }

    impl ModelAlloc {
        fn new() -> Self {
            Self {
                next_addr: 0x1000_0000,
                ..Default::default()
            }
        }

        fn class_for(size: usize, align: usize) -> Option<usize> {
            if size == 0 || !align.is_power_of_two() || align > PAGE_SIZE {
                return None;
            }
            let need = size.max(align);
            if need > SMALL_MAX {
                return None;
            }
            CLASS_SIZES.iter().position(|&c| c >= need)
        }

        fn alloc(&mut self, size: usize, align: usize) -> Option<usize> {
            if let Some(class_idx) = Self::class_for(size, align) {
                if let Some(ptr) = self.free_small[class_idx].pop() {
                    self.live_small.insert(ptr);
                    return Some(ptr);
                }
                let step = CLASS_SIZES[class_idx];
                let ptr = (self.next_addr + (step - 1)) & !(step - 1);
                self.next_addr = ptr + step;
                self.live_small.insert(ptr);
                return Some(ptr);
            }
            if align > PAGE_SIZE || !align.is_power_of_two() {
                return None;
            }
            let ptr = (self.next_addr + (PAGE_SIZE - 1)) & !(PAGE_SIZE - 1);
            let pages = (size.div_ceil(PAGE_SIZE)).max(1) + 1;
            self.next_addr = ptr + pages * PAGE_SIZE;
            self.live_large.insert(ptr);
            Some(ptr)
        }

        fn dealloc(&mut self, size: usize, align: usize, ptr: usize) -> bool {
            if let Some(class_idx) = Self::class_for(size, align) {
                if !self.live_small.remove(&ptr) {
                    return false;
                }
                self.free_small[class_idx].push(ptr);
                return true;
            }
            self.live_large.remove(&ptr)
        }
    }

    #[test]
    fn many_tiny_allocations_and_frees() {
        let mut a = ModelAlloc::new();
        let mut ptrs = Vec::new();
        for _ in 0..20_000 {
            ptrs.push(a.alloc(1, 1).expect("alloc"));
        }
        for p in ptrs {
            assert!(a.dealloc(1, 1, p));
        }
    }

    #[test]
    fn mixed_small_allocations_work() {
        let mut a = ModelAlloc::new();
        let mut live = Vec::new();
        for size in [1usize, 7, 19, 63, 64, 65, 255, 400, 777, 1023, 1600, 2048] {
            live.push((size, a.alloc(size, 8).expect("alloc")));
        }
        for (size, ptr) in live {
            assert!(a.dealloc(size, 8, ptr));
        }
    }

    #[test]
    fn vec_like_grow_shrink_small_sizes() {
        let mut a = ModelAlloc::new();
        let mut cap = 8usize;
        let mut current = a.alloc(cap, 8).expect("alloc");
        for _ in 0..32 {
            let next = a.alloc(cap * 2, 8).expect("grow");
            assert!(a.dealloc(cap, 8, current));
            cap *= 2;
            current = next;
        }
        for _ in 0..32 {
            let next_cap = (cap / 2).max(8);
            let next = a.alloc(next_cap, 8).expect("shrink");
            assert!(a.dealloc(cap, 8, current));
            cap = next_cap;
            current = next;
        }
        assert!(a.dealloc(cap, 8, current));
    }

    #[test]
    fn boundary_between_small_and_large_paths() {
        let mut a = ModelAlloc::new();
        let s = a.alloc(SMALL_MAX, 8).expect("small");
        let l = a.alloc(SMALL_MAX + 1, 8).expect("large");
        assert!(a.live_small.contains(&s));
        assert!(a.live_large.contains(&l));
        assert!(a.dealloc(SMALL_MAX, 8, s));
        assert!(a.dealloc(SMALL_MAX + 1, 8, l));
    }

    #[test]
    fn large_allocations_still_work() {
        let mut a = ModelAlloc::new();
        let mut large = Vec::new();
        for sz in [4097usize, 8192, 16_384, 65_000] {
            large.push((sz, a.alloc(sz, 64).expect("alloc")));
        }
        for (sz, ptr) in large {
            assert!(a.dealloc(sz, 64, ptr));
        }
    }

    #[test]
    fn alignment_sensitive_allocations() {
        let mut a = ModelAlloc::new();
        for align in [1usize, 2, 4, 8, 16, 32, 64, 256, 1024, 2048] {
            let ptr = a.alloc(33, align).expect("alloc");
            assert_eq!(ptr % align, 0);
            assert!(a.dealloc(33, align, ptr));
        }
    }

    #[test]
    fn slab_free_reuse_happens() {
        let mut a = ModelAlloc::new();
        let p1 = a.alloc(24, 8).expect("alloc1");
        let p2 = a.alloc(24, 8).expect("alloc2");
        assert!(a.dealloc(24, 8, p1));
        let p3 = a.alloc(24, 8).expect("alloc3");
        assert_eq!(p3, p1);
        assert!(a.dealloc(24, 8, p2));
        assert!(a.dealloc(24, 8, p3));
    }

    #[test]
    fn invalid_free_and_double_free_rejected_when_detectable() {
        let mut a = ModelAlloc::new();
        let p = a.alloc(32, 8).expect("alloc");
        assert!(!a.dealloc(32, 8, p + 8));
        assert!(a.dealloc(32, 8, p));
        assert!(!a.dealloc(32, 8, p));
    }
}
