// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(not(feature = "hosted-dev"))]
mod non_hosted {
    use core::alloc::{GlobalAlloc, Layout};
    use core::mem::size_of;
    use core::ptr::null_mut;

    #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
    use crate::arch::platform_layout;
    use crate::kernel::frame_allocator::{
        alloc_pt_contiguous_frames, alloc_pt_frame, free_pt_contiguous_frames, free_pt_frame,
    };
    use crate::kernel::lock::SpinLockIrq;
    use crate::kernel::vm::PAGE_SIZE;

    // Allocator design note (final cleanup):
    // - Slab size classes: 16, 32, 64, 128, 256, 512, 1024, 2048 bytes.
    // - Small/large split: allocations with max(size, align) <= 2048 use slabs; larger
    //   requests use the large contiguous-page path.
    // - Warm empty page policy: keep up to one fully empty slab page per class.
    // - Empty-page reclamation: if a class has more than one fully empty slab page, dealloc can
    //   unlink and return extra empty pages to the frame allocator.
    // - Known limitations: invalid-free detection is best-effort (magic/shape/bitmap checks),
    //   and empty-page reclamation does a linear scan over class pages.
    const SLAB_CLASS_SIZES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];
    const SMALL_ALLOC_MAX: usize = 2048;
    const FREE_NONE: u16 = u16::MAX;
    const KEEP_ONE_WARM_EMPTY_PAGE_PER_CLASS: bool = true;
    const SLAB_MAGIC: u64 = 0x5941_524d_534c_4142; // "YARMSLAB"
    const LARGE_MAGIC: u64 = 0x5941_524d_4c41_5247; // "YARMLARG"

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

    // Locking / SMP discipline (final note):
    // - Each slab size class has its own SpinLockIrq<u64> guarding that class's page list head
    //   and all metadata mutations for pages belonging to the class (free-list, bitmap, used count,
    //   unlink/reclaim decisions).
    // - The slab class lock must not be held while calling the frame allocator. Small-allocation
    //   slow paths drop the class lock before alloc_pt_frame/free_pt_frame, then reacquire and
    //   rescan before linking a new page. This preserves lock layering: slab class locks sit above
    //   raw frame allocation and never nest around it.
    // - Large allocation path uses a separate lock to serialize large-header lifecycle operations.
    // - No nested allocator locks are taken (class lock and large lock are disjoint paths).
    // IRQ context note:
    // - SpinLockIrq disables local IRQs while held, so allocator lock holders cannot be preempted by
    //   IRQ handlers on the same CPU acquiring the same lock.
    // Test limitation note:
    // - Current allocator tests in this repository are interleaving/model based; true multi-core
    //   parallel race execution is not exercised in the hosted-dev unit-test harness.
    static SLAB_CLASS_LOCK_0: SpinLockIrq<u64> = SpinLockIrq::new(0);
    static SLAB_CLASS_LOCK_1: SpinLockIrq<u64> = SpinLockIrq::new(0);
    static SLAB_CLASS_LOCK_2: SpinLockIrq<u64> = SpinLockIrq::new(0);
    static SLAB_CLASS_LOCK_3: SpinLockIrq<u64> = SpinLockIrq::new(0);
    static SLAB_CLASS_LOCK_4: SpinLockIrq<u64> = SpinLockIrq::new(0);
    static SLAB_CLASS_LOCK_5: SpinLockIrq<u64> = SpinLockIrq::new(0);
    static SLAB_CLASS_LOCK_6: SpinLockIrq<u64> = SpinLockIrq::new(0);
    static SLAB_CLASS_LOCK_7: SpinLockIrq<u64> = SpinLockIrq::new(0);
    static LARGE_ALLOC_LOCK: SpinLockIrq<()> = SpinLockIrq::new(());

    #[derive(Clone, Copy)]
    pub struct KernelGlobalAllocator;

    impl KernelGlobalAllocator {
        fn class_lock(class_idx: usize) -> &'static SpinLockIrq<u64> {
            match class_idx {
                0 => &SLAB_CLASS_LOCK_0,
                1 => &SLAB_CLASS_LOCK_1,
                2 => &SLAB_CLASS_LOCK_2,
                3 => &SLAB_CLASS_LOCK_3,
                4 => &SLAB_CLASS_LOCK_4,
                5 => &SLAB_CLASS_LOCK_5,
                6 => &SLAB_CLASS_LOCK_6,
                7 => &SLAB_CLASS_LOCK_7,
                _ => &SLAB_CLASS_LOCK_0,
            }
        }

        fn phys_to_ptr(phys: u64) -> *mut u8 {
            #[cfg(target_arch = "x86_64")]
            {
                let Some(virt) = platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE.checked_add(phys)
                else {
                    return null_mut();
                };
                return virt as usize as *mut u8;
            }
            // AArch64 and RISC-V currently access allocator frames through an early
            // identity-mapped lower-memory window, so the physical frame address is the
            // virtual pointer used by the kernel allocator. Do not replace this with
            // KERNEL_BOOTSTRAP_VIRT_BASE + phys: that constant is an image/bootstrap anchor
            // on these ports, not a proven direct-map offset for every allocator frame.
            // Future higher-half ports should provide an arch-owned phys<->kernel-virt helper
            // and update this conversion together with the corresponding bootstrap mappings.
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
                let Some(virt) = platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE.checked_add(phys)
                else {
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
            // alloc_bitmap is [u64; 4] == 256 bits; valid slot indices are 0..=255.
            // capacity == 256 is acceptable (index 255 maps to bit 63 of word 3).
            // capacity > 256 would overflow the bitmap, so we reject it.
            // In practice the largest capacity (16-byte class) is 252, well within bounds.
            if capacity == 0 || capacity > 256 {
                return false;
            }
            debug_assert!(
                capacity <= core::mem::size_of::<[u64; 4]>() * 8,
                "slab capacity {capacity} overflows alloc_bitmap"
            );

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

        unsafe fn unlink_slab_page(class_head: &mut u64, target_phys: u64) -> bool {
            let mut prev_phys = 0u64;
            let mut phys = *class_head;
            while phys != 0 {
                let page_ptr = Self::phys_to_ptr(phys);
                if page_ptr.is_null() {
                    return false;
                }
                let header = unsafe { &*(page_ptr as *const SlabPageHeader) };
                let next = header.next_page_phys;
                if phys == target_phys {
                    if prev_phys == 0 {
                        *class_head = next;
                    } else {
                        let prev_ptr = Self::phys_to_ptr(prev_phys);
                        if prev_ptr.is_null() {
                            return false;
                        }
                        let prev_header = unsafe { &mut *(prev_ptr as *mut SlabPageHeader) };
                        prev_header.next_page_phys = next;
                    }
                    return true;
                }
                prev_phys = phys;
                phys = next;
            }
            false
        }

        unsafe fn try_unlink_empty_slab_page(
            class_head: &mut u64,
            class_idx: usize,
            target_page_ptr: *mut u8,
        ) -> Option<u64> {
            let target_phys = Self::ptr_to_phys(target_page_ptr as *const u8);
            let mut empty_pages = 0usize;
            let mut phys = *class_head;
            while phys != 0 {
                let page_ptr = Self::phys_to_ptr(phys);
                if page_ptr.is_null() {
                    return None;
                }
                let header = unsafe { &*(page_ptr as *const SlabPageHeader) };
                if header.magic == SLAB_MAGIC
                    && header.class_idx as usize == class_idx
                    && header.used == 0
                {
                    empty_pages = empty_pages.saturating_add(1);
                }
                phys = header.next_page_phys;
            }

            let should_keep_warm = KEEP_ONE_WARM_EMPTY_PAGE_PER_CLASS && empty_pages <= 1;
            if should_keep_warm {
                return None;
            }

            if unsafe { Self::unlink_slab_page(class_head, target_phys) } {
                return Some(target_phys);
            }
            None
        }

        unsafe fn alloc_small_from_list(class_head: &mut u64, class_idx: usize) -> *mut u8 {
            let mut phys = *class_head;
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
            null_mut()
        }

        unsafe fn alloc_small(class_idx: usize) -> *mut u8 {
            {
                let mut class_head = Self::class_lock(class_idx).lock();
                let ptr = unsafe { Self::alloc_small_from_list(&mut class_head, class_idx) };
                if !ptr.is_null() {
                    return ptr;
                }
            }

            // Slow path: never hold the slab class lock while asking the frame allocator for a
            // page. alloc_pt_frame() takes the frame-allocator lock, and keeping the class lock
            // across that call would invert allocator lock layering on future multicore paths.
            let Ok(new_phys) = alloc_pt_frame() else {
                return null_mut();
            };
            let page_ptr = Self::phys_to_ptr(new_phys);
            if page_ptr.is_null() {
                let _ = free_pt_frame(new_phys);
                return null_mut();
            }

            if !unsafe { Self::slab_init_page(page_ptr, class_idx, 0) } {
                let _ = free_pt_frame(new_phys);
                return null_mut();
            }

            let mut unused_new_page = false;
            let allocated = {
                let mut class_head = Self::class_lock(class_idx).lock();
                let existing = unsafe { Self::alloc_small_from_list(&mut class_head, class_idx) };
                if !existing.is_null() {
                    unused_new_page = true;
                    existing
                } else {
                    let header = unsafe { &mut *(page_ptr as *mut SlabPageHeader) };
                    header.next_page_phys = *class_head;
                    *class_head = new_phys;
                    unsafe { Self::slab_alloc_from_page(page_ptr) }
                }
            };

            if unused_new_page {
                let _ = free_pt_frame(new_phys);
            }
            allocated
        }

        unsafe fn alloc_large(layout: Layout) -> *mut u8 {
            if layout.align() > PAGE_SIZE || !layout.align().is_power_of_two() {
                return null_mut();
            }
            // Large allocation layout:
            //   Page 0 (base_ptr .. base_ptr+PAGE_SIZE): LargeAllocHeader at offset 0.
            //     Fields: magic (LARGE_MAGIC) + pages (total_pages as u64, >= 2).
            //   Page 1 .. Page N (base_ptr+PAGE_SIZE ..): user payload, returned to caller.
            //
            // total_pages = 1 (header page) + payload_pages, where payload_pages is
            // ceil(size + sizeof(LargeAllocHeader) / PAGE_SIZE) rounded up to at least 1.
            // The extra header-overhead in payload_pages is conservative; it guarantees the
            // caller's layout.size() bytes fit entirely within pages 1..N.
            //
            // Invariant: alloc_pt_contiguous_frames returns physically and virtually
            // contiguous pages (the frame allocator maintains this), so base_ptr+PAGE_SIZE
            // is a valid mapped pointer and the returned slice covers exactly pages 1..N.
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

            if let Some(class_idx) = Self::slab_class_for(layout) {
                return unsafe { Self::alloc_small(class_idx) };
            }
            let _large_guard = LARGE_ALLOC_LOCK.lock();
            unsafe { Self::alloc_large(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
            if ptr.is_null() {
                return;
            }

            let page_base = (ptr as usize) & !(PAGE_SIZE - 1);
            let page_ptr = page_base as *mut u8;
            if !page_ptr.is_null() {
                let magic = unsafe { core::ptr::read_unaligned(page_ptr as *const u64) };
                if magic == SLAB_MAGIC {
                    let class_idx =
                        unsafe { (&*(page_ptr as *const SlabPageHeader)).class_idx as usize };
                    if class_idx < SLAB_CLASS_SIZES.len() {
                        let reclaim_phys = {
                            let mut class_head = Self::class_lock(class_idx).lock();
                            let ok = unsafe { Self::slab_dealloc_from_page(page_ptr, ptr) };
                            if ok {
                                let header = unsafe { &*(page_ptr as *const SlabPageHeader) };
                                if header.used == 0 {
                                    unsafe {
                                        Self::try_unlink_empty_slab_page(
                                            &mut class_head,
                                            class_idx,
                                            page_ptr,
                                        )
                                    }
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        };
                        if let Some(phys) = reclaim_phys {
                            let _ = free_pt_frame(phys);
                        }
                        return;
                    }
                    debug_assert!(
                        false,
                        "corrupt slab class index in kernel allocator dealloc"
                    );
                    return;
                }
            }

            if !(ptr as usize).is_multiple_of(PAGE_SIZE) {
                debug_assert!(
                    false,
                    "invalid kernel allocator dealloc pointer: non-slab, non-page-aligned"
                );
                return;
            }

            let _large_guard = LARGE_ALLOC_LOCK.lock();
            let header_ptr = unsafe { ptr.sub(PAGE_SIZE) as *const LargeAllocHeader };
            #[cfg(target_arch = "x86_64")]
            if (header_ptr as usize as u64) < platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE {
                return;
            }

            let header = unsafe { core::ptr::read(header_ptr) };
            if header.magic != LARGE_MAGIC {
                debug_assert!(
                    false,
                    "invalid kernel allocator dealloc pointer: missing large magic"
                );
                return;
            }
            let pages = header.pages as usize;
            if pages < 2 {
                debug_assert!(
                    false,
                    "corrupt large allocation page count in kernel allocator dealloc"
                );
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

#[cfg(feature = "hosted-dev")]
pub use hosted::KERNEL_GLOBAL_ALLOCATOR;
#[cfg(feature = "hosted-dev")]
pub use hosted::KernelGlobalAllocator;
#[cfg(not(feature = "hosted-dev"))]
pub use non_hosted::KERNEL_GLOBAL_ALLOCATOR;
#[cfg(not(feature = "hosted-dev"))]
pub use non_hosted::KernelGlobalAllocator;

#[cfg(all(test, feature = "hosted-dev"))]
mod hosted_dev_allocator_model_tests {
    use std::array;
    use std::collections::BTreeSet;
    use std::vec::Vec;

    const PAGE_SIZE: usize = 4096;
    const CLASS_SIZES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];
    const SMALL_MAX: usize = 2048;
    const SLAB_HEADER_BYTES: usize = 64;

    fn align_up(value: usize, align: usize) -> usize {
        (value + align - 1) & !(align - 1)
    }

    fn class_capacity(class_idx: usize) -> usize {
        let obj = CLASS_SIZES[class_idx];
        let start = align_up(SLAB_HEADER_BYTES, obj);
        (PAGE_SIZE - start) / obj
    }
    const KEEP_ONE_WARM_EMPTY_PAGE_PER_CLASS: bool = true;

    #[derive(Default)]
    struct ClassPool {
        free: Vec<usize>,
        live: BTreeSet<usize>,
        pages: usize,
    }

    struct ModelAlloc {
        next_addr: usize,
        small: [ClassPool; CLASS_SIZES.len()],
        live_large: BTreeSet<usize>,
    }

    #[derive(Default)]
    struct ReclaimPage {
        free: Vec<usize>,
        live: BTreeSet<usize>,
    }

    #[derive(Default)]
    struct ReclaimClassPool {
        pages: Vec<ReclaimPage>,
        reclaimed_pages: usize,
    }

    struct ReclaimModelAlloc {
        next_addr: usize,
        classes: [ReclaimClassPool; CLASS_SIZES.len()],
    }

    impl ReclaimModelAlloc {
        fn new() -> Self {
            Self {
                next_addr: 0x2000_0000,
                classes: array::from_fn(|_| ReclaimClassPool::default()),
            }
        }

        fn class_for(size: usize, align: usize) -> Option<usize> {
            ModelAlloc::class_for(size, align)
        }

        fn alloc(&mut self, size: usize, align: usize) -> Option<usize> {
            let class_idx = Self::class_for(size, align)?;
            let obj = CLASS_SIZES[class_idx];
            for page in &mut self.classes[class_idx].pages {
                if let Some(ptr) = page.free.pop() {
                    page.live.insert(ptr);
                    return Some(ptr);
                }
            }

            let cap = class_capacity(class_idx);
            let start = align_up(SLAB_HEADER_BYTES, obj);
            let base = align_up(self.next_addr, PAGE_SIZE);
            self.next_addr = base + PAGE_SIZE;
            let mut page = ReclaimPage::default();
            for idx in 0..cap {
                page.free.push(base + start + idx * obj);
            }
            let ptr = page.free.pop()?;
            page.live.insert(ptr);
            self.classes[class_idx].pages.push(page);
            Some(ptr)
        }

        fn dealloc(&mut self, size: usize, align: usize, ptr: usize) -> bool {
            let Some(class_idx) = Self::class_for(size, align) else {
                return false;
            };
            let mut target_page_idx = None;
            for (idx, page) in self.classes[class_idx].pages.iter_mut().enumerate() {
                if page.live.remove(&ptr) {
                    page.free.push(ptr);
                    target_page_idx = Some(idx);
                    break;
                }
            }
            let Some(page_idx) = target_page_idx else {
                return false;
            };

            let cap = class_capacity(class_idx);
            let page_is_empty = self.classes[class_idx].pages[page_idx].live.is_empty()
                && self.classes[class_idx].pages[page_idx].free.len() == cap;
            if !page_is_empty {
                return true;
            }

            let empty_pages = self.classes[class_idx]
                .pages
                .iter()
                .filter(|page| page.live.is_empty() && page.free.len() == cap)
                .count();
            if KEEP_ONE_WARM_EMPTY_PAGE_PER_CLASS && empty_pages <= 1 {
                return true;
            }

            self.classes[class_idx].pages.swap_remove(page_idx);
            self.classes[class_idx].reclaimed_pages += 1;
            true
        }
    }

    impl ModelAlloc {
        fn new() -> Self {
            Self {
                next_addr: 0x1000_0000,
                small: array::from_fn(|_| ClassPool::default()),
                live_large: BTreeSet::new(),
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
                if self.small[class_idx].free.is_empty() {
                    let obj = CLASS_SIZES[class_idx];
                    let start = align_up(SLAB_HEADER_BYTES, obj);
                    let cap = class_capacity(class_idx);
                    let page_base = align_up(self.next_addr, PAGE_SIZE);
                    self.next_addr = page_base + PAGE_SIZE;
                    self.small[class_idx].pages += 1;
                    for idx in 0..cap {
                        self.small[class_idx]
                            .free
                            .push(page_base + start + idx * obj);
                    }
                }
                if let Some(ptr) = self.small[class_idx].free.pop() {
                    self.small[class_idx].live.insert(ptr);
                    return Some(ptr);
                }
                return None;
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
                if !self.small[class_idx].live.remove(&ptr) {
                    return false;
                }
                self.small[class_idx].free.push(ptr);
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
        assert!(a.small[7].live.contains(&s));
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
    fn slab_slot_offsets_never_masquerade_as_large_allocations() {
        for class_idx in 0..CLASS_SIZES.len() {
            let obj = CLASS_SIZES[class_idx];
            let start = align_up(SLAB_HEADER_BYTES, obj);
            let cap = class_capacity(class_idx);
            assert!(cap > 0);
            for idx in 0..cap {
                let offset = start + idx * obj;
                assert!(offset > 0 && offset < PAGE_SIZE);
                assert_ne!(offset % PAGE_SIZE, 0);
            }
        }
    }

    #[test]
    fn large_dealloc_rejects_interior_pointer_but_accepts_returned_pointer() {
        let mut a = ModelAlloc::new();
        let ptr = a.alloc(8192, 64).expect("large alloc");
        assert_eq!(ptr % PAGE_SIZE, 0);
        assert!(!a.dealloc(8192, 64, ptr + PAGE_SIZE));
        assert!(a.dealloc(8192, 64, ptr));
    }

    #[test]
    fn slow_path_rescan_uses_existing_slot_and_does_not_leak_unused_page() {
        let mut a = ReclaimModelAlloc::new();
        let class_idx = ReclaimModelAlloc::class_for(64, 8).expect("class");
        let cap = class_capacity(class_idx);
        let mut full_page = Vec::new();
        for _ in 0..cap {
            full_page.push(a.alloc(64, 8).expect("fill page"));
        }
        assert_eq!(a.classes[class_idx].pages.len(), 1);

        // Thread A would drop the class lock and allocate a raw frame here. Before it
        // reacquires the lock, thread B frees a slot in the existing page.
        let speculative_raw_frame = align_up(a.next_addr, PAGE_SIZE);
        a.next_addr = speculative_raw_frame + PAGE_SIZE;
        let freed_by_other_cpu = full_page.pop().expect("one slot");
        assert!(a.dealloc(64, 8, freed_by_other_cpu));

        // The fixed slow path must rescan after reacquiring the class lock and consume the
        // existing free slot instead of linking the speculative page.
        let reused = a.alloc(64, 8).expect("rescan alloc");
        assert_eq!(reused, freed_by_other_cpu);
        assert_eq!(a.classes[class_idx].pages.len(), 1);

        for ptr in full_page {
            assert!(a.dealloc(64, 8, ptr));
        }
        assert!(a.dealloc(64, 8, reused));
    }

    #[test]
    fn invalid_free_and_double_free_rejected_when_detectable() {
        let mut a = ModelAlloc::new();
        let p = a.alloc(32, 8).expect("alloc");
        assert!(!a.dealloc(32, 8, p + 8));
        assert!(a.dealloc(32, 8, p));
        assert!(!a.dealloc(32, 8, p));
    }

    #[test]
    fn free_all_objects_from_page_then_reallocate_all() {
        let mut a = ModelAlloc::new();
        let class_idx = ModelAlloc::class_for(24, 8).expect("class");
        let cap = class_capacity(class_idx);
        let mut first_round = Vec::new();
        for _ in 0..cap {
            first_round.push(a.alloc(24, 8).expect("alloc"));
        }
        assert_eq!(a.small[class_idx].pages, 1);
        let first_set: BTreeSet<usize> = first_round.iter().copied().collect();
        for ptr in first_round {
            assert!(a.dealloc(24, 8, ptr));
        }
        let mut second_round = Vec::new();
        for _ in 0..cap {
            second_round.push(a.alloc(24, 8).expect("realloc"));
        }
        let second_set: BTreeSet<usize> = second_round.iter().copied().collect();
        assert_eq!(first_set, second_set);
    }

    #[test]
    fn exhausting_one_class_spans_multiple_pages() {
        let mut a = ModelAlloc::new();
        let class_idx = ModelAlloc::class_for(16, 8).expect("class");
        let cap = class_capacity(class_idx);
        let total = cap * 2 + 17;
        let mut ptrs = Vec::new();
        for _ in 0..total {
            ptrs.push(a.alloc(16, 8).expect("alloc"));
        }
        assert!(a.small[class_idx].pages >= 3);
        for ptr in ptrs {
            assert!(a.dealloc(16, 8, ptr));
        }
    }

    #[test]
    fn mixed_allocate_free_interleavings_stay_consistent() {
        let mut a = ModelAlloc::new();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut live = Vec::new();

        for _ in 0..20_000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let do_alloc = (seed & 1) == 0 || live.is_empty();
            if do_alloc {
                let bucket = (seed as usize) % 5;
                let size = match bucket {
                    0 => 8,
                    1 => 40,
                    2 => 96,
                    3 => 400,
                    _ => 1600,
                };
                let align = 1usize << (((seed >> 3) as usize) % 5);
                if let Some(ptr) = a.alloc(size, align) {
                    live.push((size, align, ptr));
                }
            } else {
                let idx = (seed as usize) % live.len();
                let (size, align, ptr) = live.swap_remove(idx);
                assert!(a.dealloc(size, align, ptr));
            }
        }

        for (size, align, ptr) in live {
            assert!(a.dealloc(size, align, ptr));
        }
    }

    #[test]
    fn repeated_vec_growth_transitions_size_classes() {
        let mut a = ModelAlloc::new();
        let elem_size = 8usize;
        let mut cap = 1usize;
        let mut current_ptr = a.alloc(cap * elem_size, elem_size).expect("alloc");
        let mut current_class = ModelAlloc::class_for(cap * elem_size, elem_size);
        let mut transitions = 0usize;

        for _ in 0..16 {
            let next_cap = cap.saturating_mul(2);
            let next_size = next_cap * elem_size;
            let next_ptr = a.alloc(next_size, elem_size).expect("grow");
            assert!(a.dealloc(cap * elem_size, elem_size, current_ptr));
            let next_class = ModelAlloc::class_for(next_size, elem_size);
            if next_class != current_class {
                transitions += 1;
            }
            current_class = next_class;
            cap = next_cap;
            current_ptr = next_ptr;
        }
        assert!(transitions >= 3);
        assert!(a.dealloc(cap * elem_size, elem_size, current_ptr));
    }

    #[test]
    fn empty_page_gets_reclaimed_when_multiple_empty_pages_exist() {
        let mut a = ReclaimModelAlloc::new();
        let class_idx = ReclaimModelAlloc::class_for(16, 8).expect("class");
        let cap = class_capacity(class_idx);
        let total = cap * 2;
        let mut ptrs = Vec::new();
        for _ in 0..total {
            ptrs.push(a.alloc(16, 8).expect("alloc"));
        }
        assert!(a.classes[class_idx].pages.len() >= 2);
        for ptr in ptrs {
            assert!(a.dealloc(16, 8, ptr));
        }
        assert_eq!(a.classes[class_idx].pages.len(), 1);
        assert!(a.classes[class_idx].reclaimed_pages >= 1);
    }

    #[test]
    fn warm_page_policy_keeps_one_empty_page_per_class() {
        let mut a = ReclaimModelAlloc::new();
        let class_idx = ReclaimModelAlloc::class_for(64, 8).expect("class");
        let cap = class_capacity(class_idx);
        let mut ptrs = Vec::new();
        for _ in 0..cap {
            ptrs.push(a.alloc(64, 8).expect("alloc"));
        }
        for ptr in ptrs {
            assert!(a.dealloc(64, 8, ptr));
        }
        assert_eq!(a.classes[class_idx].pages.len(), 1);
        assert_eq!(a.classes[class_idx].reclaimed_pages, 0);
    }

    #[test]
    fn repeated_reallocate_after_reclamation_still_works() {
        let mut a = ReclaimModelAlloc::new();
        let class_idx = ReclaimModelAlloc::class_for(32, 8).expect("class");
        let cap = class_capacity(class_idx);
        for _ in 0..32 {
            let mut ptrs = Vec::new();
            for _ in 0..(cap * 2 + 7) {
                ptrs.push(a.alloc(32, 8).expect("alloc"));
            }
            for ptr in ptrs {
                assert!(a.dealloc(32, 8, ptr));
            }
            assert!(a.classes[class_idx].pages.len() <= 1);
        }
    }

    #[test]
    fn reclamation_does_not_corrupt_other_classes() {
        let mut a = ReclaimModelAlloc::new();
        let class_a = ReclaimModelAlloc::class_for(16, 8).expect("class a");
        let class_b = ReclaimModelAlloc::class_for(512, 8).expect("class b");
        let cap_a = class_capacity(class_a);
        let mut a_ptrs = Vec::new();
        let mut b_ptrs = Vec::new();

        for _ in 0..(cap_a * 2 + 1) {
            a_ptrs.push(a.alloc(16, 8).expect("alloc a"));
        }
        for _ in 0..8 {
            b_ptrs.push(a.alloc(512, 8).expect("alloc b"));
        }
        for ptr in a_ptrs {
            assert!(a.dealloc(16, 8, ptr));
        }
        assert!(a.classes[class_a].reclaimed_pages >= 1);
        for ptr in b_ptrs {
            assert!(a.dealloc(512, 8, ptr));
        }
        assert!(a.classes[class_b].pages.len() <= 1);
        let again_b = a.alloc(512, 8).expect("realloc b");
        assert!(a.dealloc(512, 8, again_b));
    }

    #[test]
    fn interleaving_same_class_alloc_free_keeps_consistency() {
        let mut a = ModelAlloc::new();
        let class_idx = ModelAlloc::class_for(64, 8).expect("class");
        let mut t0_live = Vec::new();
        let mut t1_live = Vec::new();

        for step in 0..2_000usize {
            if step % 3 != 0 || t0_live.is_empty() {
                t0_live.push(a.alloc(64, 8).expect("t0 alloc"));
            } else {
                let ptr = t0_live.pop().expect("t0 pop");
                assert!(a.dealloc(64, 8, ptr));
            }

            if step % 5 != 0 || t1_live.is_empty() {
                t1_live.push(a.alloc(64, 8).expect("t1 alloc"));
            } else {
                let ptr = t1_live.pop().expect("t1 pop");
                assert!(a.dealloc(64, 8, ptr));
            }
        }

        for ptr in t0_live {
            assert!(a.dealloc(64, 8, ptr));
        }
        for ptr in t1_live {
            assert!(a.dealloc(64, 8, ptr));
        }

        assert!(a.small[class_idx].live.is_empty());
    }

    #[test]
    fn reclamation_interleaving_preserves_class_integrity() {
        let mut a = ReclaimModelAlloc::new();
        let class_idx = ReclaimModelAlloc::class_for(32, 8).expect("class");
        let cap = class_capacity(class_idx);
        let mut left = Vec::new();
        let mut right = Vec::new();

        for i in 0..(cap * 3) {
            let ptr = a.alloc(32, 8).expect("alloc");
            if i % 2 == 0 {
                left.push(ptr);
            } else {
                right.push(ptr);
            }
        }

        while let Some(ptr) = left.pop() {
            assert!(a.dealloc(32, 8, ptr));
        }
        while let Some(ptr) = right.pop() {
            assert!(a.dealloc(32, 8, ptr));
        }

        // At most one warm empty page should remain.
        assert!(a.classes[class_idx].pages.len() <= 1);
    }

    #[test]
    fn repeated_same_class_cycles_preserve_page_accounting() {
        let mut a = ReclaimModelAlloc::new();
        let class_idx = ReclaimModelAlloc::class_for(128, 8).expect("class");
        let cap = class_capacity(class_idx);

        for _ in 0..64 {
            let mut ptrs = Vec::new();
            for _ in 0..(cap * 2 + 11) {
                ptrs.push(a.alloc(128, 8).expect("alloc"));
            }
            for ptr in ptrs {
                assert!(a.dealloc(128, 8, ptr));
            }
            assert!(a.classes[class_idx].pages.len() <= 1);
        }
    }

    #[test]
    fn large_path_interleaving_bookkeeping_is_stable() {
        let mut a = ModelAlloc::new();
        let mut live_a = Vec::new();
        let mut live_b = Vec::new();

        for i in 0..512usize {
            if i % 2 == 0 {
                live_a.push(a.alloc(8192, 64).expect("alloc a"));
            } else {
                live_b.push(a.alloc(16_384, 64).expect("alloc b"));
            }
            if i % 7 == 0 && !live_a.is_empty() {
                let ptr = live_a.swap_remove(0);
                assert!(a.dealloc(8192, 64, ptr));
            }
            if i % 11 == 0 && !live_b.is_empty() {
                let ptr = live_b.swap_remove(0);
                assert!(a.dealloc(16_384, 64, ptr));
            }
        }

        for ptr in live_a {
            assert!(a.dealloc(8192, 64, ptr));
        }
        for ptr in live_b {
            assert!(a.dealloc(16_384, 64, ptr));
        }
        assert!(a.live_large.is_empty());
    }
}
