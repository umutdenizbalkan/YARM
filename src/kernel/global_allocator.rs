// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(not(feature = "hosted-dev"))]
mod non_hosted {
    use core::alloc::{GlobalAlloc, Layout};
    use core::ptr::null_mut;

    use crate::arch::platform_layout;
    use crate::kernel::frame_allocator::{
        alloc_pt_contiguous_frames, free_pt_contiguous_frames,
    };
    use crate::kernel::lock::SpinLockIrq;
    use crate::kernel::vm::PAGE_SIZE;

    // Design note (current transitional allocator contract):
    // - Accepted alignment: power-of-two alignments up to PAGE_SIZE; larger alignments are rejected.
    // - Layout: one metadata header page at base, one-or-more user pages immediately after base + PAGE_SIZE.
    // - Invalid-free detection: best-effort only (pointer shape + header magic/page-count sanity), not full
    //   ownership-proofing against all forged/stale pointers.
    // - Small-allocation waste: worst case is (2 * PAGE_SIZE - 1) bytes wasted for a 1-byte allocation
    //   (one dedicated header page + one user page minimum).
    // - Planned direction: replace with slab/small-object allocation for sub-page objects while preserving
    //   contiguous-page backing for large allocations.
    const HEADER_SIZE: usize = core::mem::size_of::<AllocationHeader>();
    const ALLOC_ALIGN_LIMIT: usize = PAGE_SIZE;
    const ALLOCATION_MAGIC: u64 = 0x5941_524d_4741_4c4c; // "YARMGALL"

    #[derive(Debug, Clone, Copy)]
    struct AllocationHeader {
        magic: u64,
        pages: u64,
    }

    static ALLOCATOR_LOCK: SpinLockIrq<()> = SpinLockIrq::new(());

    pub struct KernelGlobalAllocator;

    impl KernelGlobalAllocator {
        fn is_supported_alignment(align: usize) -> bool {
            align.is_power_of_two() && align <= ALLOC_ALIGN_LIMIT
        }

        fn allocation_pages_for(layout: Layout) -> Option<usize> {
            if layout.size() == 0 || !Self::is_supported_alignment(layout.align()) {
                return None;
            }
            let user_bytes = layout.size().saturating_add(HEADER_SIZE);
            let pages = user_bytes.div_ceil(PAGE_SIZE).max(1);
            Some(pages.saturating_add(1))
        }

        fn is_valid_user_pointer(ptr: *mut u8) -> bool {
            !ptr.is_null() && (ptr as usize).is_multiple_of(PAGE_SIZE)
        }

        fn header_pages_if_valid(header: AllocationHeader) -> Option<usize> {
            if header.magic != ALLOCATION_MAGIC {
                return None;
            }
            let pages = header.pages as usize;
            if !(2..=u64::MAX as usize).contains(&pages) {
                return None;
            }
            Some(pages)
        }

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
    }

    unsafe impl GlobalAlloc for KernelGlobalAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let Some(total_pages) = Self::allocation_pages_for(layout) else {
                return null_mut();
            };

            let _guard = ALLOCATOR_LOCK.lock();
            let Ok(base_phys) = alloc_pt_contiguous_frames(total_pages) else {
                return null_mut();
            };
            let base_ptr = Self::phys_to_ptr(base_phys);
            if base_ptr.is_null() {
                let _ = free_pt_contiguous_frames(base_phys, total_pages);
                return null_mut();
            }

            let header = AllocationHeader {
                magic: ALLOCATION_MAGIC,
                pages: total_pages as u64,
            };
            core::ptr::write(base_ptr as *mut AllocationHeader, header);
            base_ptr.add(PAGE_SIZE)
        }

        unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
            if !Self::is_valid_user_pointer(ptr) {
                return;
            }
            let _guard = ALLOCATOR_LOCK.lock();
            let header_ptr = ptr.sub(PAGE_SIZE) as *const AllocationHeader;

            #[cfg(target_arch = "x86_64")]
            if (header_ptr as usize as u64) < platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE {
                return;
            }

            let header = core::ptr::read(header_ptr);
            let Some(pages) = Self::header_pages_if_valid(header) else {
                return;
            };

            #[cfg(target_arch = "x86_64")]
            let base_phys = (header_ptr as usize as u64)
                .saturating_sub(platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE);
            #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
            let base_phys = header_ptr as usize as u64;
            #[cfg(not(any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )))]
            let base_phys = (header_ptr as usize as u64)
                .saturating_sub(platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE);

            let _ = free_pt_contiguous_frames(base_phys, pages);
        }
    }

    pub static KERNEL_GLOBAL_ALLOCATOR: KernelGlobalAllocator = KernelGlobalAllocator;

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::std::collections::BTreeMap;
        use crate::std::vec::Vec;

        struct AllocSim {
            next_base: usize,
            live: BTreeMap<usize, usize>,
        }

        impl AllocSim {
            fn new(base: usize) -> Self {
                Self {
                    next_base: base,
                    live: BTreeMap::new(),
                }
            }

            fn alloc(&mut self, layout: Layout) -> Option<usize> {
                let total_pages = KernelGlobalAllocator::allocation_pages_for(layout)?;
                let base = self.next_base;
                self.next_base = self
                    .next_base
                    .saturating_add(total_pages.saturating_mul(PAGE_SIZE));
                self.live.insert(base, total_pages);
                Some(base.saturating_add(PAGE_SIZE))
            }

            fn dealloc(&mut self, user_ptr: usize) -> bool {
                if !KernelGlobalAllocator::is_valid_user_pointer(user_ptr as *mut u8) {
                    return false;
                }
                let base = user_ptr.saturating_sub(PAGE_SIZE);
                let Some(pages) = self.live.get(&base).copied() else {
                    return false;
                };
                let header = AllocationHeader {
                    magic: ALLOCATION_MAGIC,
                    pages: pages as u64,
                };
                let Some(valid_pages) = KernelGlobalAllocator::header_pages_if_valid(header) else {
                    return false;
                };
                if valid_pages != pages {
                    return false;
                }
                self.live.remove(&base).is_some()
            }
        }

        #[test]
        fn allocation_alignment_gate_accepts_only_power_of_two_up_to_page() {
            assert!(KernelGlobalAllocator::is_supported_alignment(1));
            assert!(KernelGlobalAllocator::is_supported_alignment(8));
            assert!(KernelGlobalAllocator::is_supported_alignment(PAGE_SIZE));
            assert!(!KernelGlobalAllocator::is_supported_alignment(0));
            assert!(!KernelGlobalAllocator::is_supported_alignment(3));
            assert!(!KernelGlobalAllocator::is_supported_alignment(PAGE_SIZE.saturating_mul(2)));
        }

        #[test]
        fn returned_user_pointer_is_page_aligned_for_supported_layouts() {
            let mut sim = AllocSim::new(0x4000_0000);
            for align in [1usize, 2, 4, 8, 16, 32, 64, 256, 1024, PAGE_SIZE] {
                let layout = Layout::from_size_align(33, align).expect("layout");
                let user_ptr = sim.alloc(layout).expect("alloc");
                assert_eq!(user_ptr % PAGE_SIZE, 0, "align={align}");
            }
        }

        #[test]
        fn many_small_alloc_free_cycles_leave_no_live_allocations() {
            let mut sim = AllocSim::new(0x5000_0000);
            for _ in 0..10_000usize {
                let layout = Layout::from_size_align(1, 1).expect("layout");
                let user_ptr = sim.alloc(layout).expect("alloc");
                assert!(sim.dealloc(user_ptr));
            }
            assert!(sim.live.is_empty());
        }

        #[test]
        fn mixed_size_allocations_and_frees_are_tracked_correctly() {
            let mut sim = AllocSim::new(0x6000_0000);
            let sizes = [1usize, 15, 128, PAGE_SIZE - 1, PAGE_SIZE, PAGE_SIZE * 3 + 17];
            let mut ptrs = Vec::new();
            for size in sizes {
                let layout = Layout::from_size_align(size, 8).expect("layout");
                ptrs.push(sim.alloc(layout).expect("alloc"));
            }
            for ptr in ptrs.into_iter().rev() {
                assert!(sim.dealloc(ptr));
            }
            assert!(sim.live.is_empty());
        }

        #[test]
        fn vec_like_grow_shrink_patterns_are_safe_under_allocator_model() {
            let mut sim = AllocSim::new(0x7000_0000);
            let mut current: Option<usize> = None;
            let mut cap = 8usize;

            for _ in 0..128usize {
                let grow = Layout::from_size_align(cap, 8).expect("layout");
                let next = sim.alloc(grow).expect("grow alloc");
                if let Some(old) = current.take() {
                    assert!(sim.dealloc(old));
                }
                current = Some(next);
                cap = cap.saturating_mul(2).min(PAGE_SIZE * 8);
            }

            for _ in 0..128usize {
                let shrink = Layout::from_size_align(cap.max(8), 8).expect("layout");
                let next = sim.alloc(shrink).expect("shrink alloc");
                if let Some(old) = current.take() {
                    assert!(sim.dealloc(old));
                }
                current = Some(next);
                cap = (cap / 2).max(8);
            }

            if let Some(last) = current {
                assert!(sim.dealloc(last));
            }
            assert!(sim.live.is_empty());
        }

        #[test]
        fn invalid_free_rejection_requires_page_aligned_known_pointer() {
            let mut sim = AllocSim::new(0x8000_0000);
            let layout = Layout::from_size_align(64, 8).expect("layout");
            let ptr = sim.alloc(layout).expect("alloc");
            assert!(!sim.dealloc(ptr + 1));
            assert!(!sim.dealloc(0x1234_5678));
            assert!(sim.dealloc(ptr));
        }

        #[test]
        fn double_free_is_rejected_by_live_set_tracking_model() {
            let mut sim = AllocSim::new(0x9000_0000);
            let layout = Layout::from_size_align(256, 16).expect("layout");
            let ptr = sim.alloc(layout).expect("alloc");
            assert!(sim.dealloc(ptr));
            assert!(!sim.dealloc(ptr));
        }

        #[test]
        fn alignment_sensitive_layouts_above_page_size_are_rejected() {
            let mut sim = AllocSim::new(0xa000_0000);
            let ok = Layout::from_size_align(128, PAGE_SIZE).expect("ok layout");
            assert!(sim.alloc(ok).is_some());

            let too_large = Layout::from_size_align(128, PAGE_SIZE.saturating_mul(2))
                .expect("too large align layout");
            assert!(sim.alloc(too_large).is_none());
        }
    }
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
    use core::alloc::Layout;
    use std::collections::BTreeMap;
    use std::vec::Vec;

    use crate::kernel::vm::PAGE_SIZE;

    const HEADER_SIZE: usize = core::mem::size_of::<u64>() * 2;
    const ALLOC_ALIGN_LIMIT: usize = PAGE_SIZE;
    const ALLOCATION_MAGIC: u64 = 0x5941_524d_4741_4c4c; // "YARMGALL"

    #[derive(Clone, Copy)]
    struct Header {
        magic: u64,
        pages: u64,
    }

    fn is_supported_alignment(align: usize) -> bool {
        align.is_power_of_two() && align <= ALLOC_ALIGN_LIMIT
    }

    fn allocation_pages_for(layout: Layout) -> Option<usize> {
        if layout.size() == 0 || !is_supported_alignment(layout.align()) {
            return None;
        }
        let user_bytes = layout.size().saturating_add(HEADER_SIZE);
        let pages = user_bytes.div_ceil(PAGE_SIZE).max(1);
        Some(pages.saturating_add(1))
    }

    fn is_valid_user_pointer(ptr: usize) -> bool {
        ptr != 0 && ptr.is_multiple_of(PAGE_SIZE)
    }

    fn header_pages_if_valid(header: Header) -> Option<usize> {
        if header.magic != ALLOCATION_MAGIC {
            return None;
        }
        let pages = header.pages as usize;
        if !(2..=u64::MAX as usize).contains(&pages) {
            return None;
        }
        Some(pages)
    }

    struct AllocSim {
        next_base: usize,
        live: BTreeMap<usize, usize>,
    }

    impl AllocSim {
        fn new(base: usize) -> Self {
            Self {
                next_base: base,
                live: BTreeMap::new(),
            }
        }

        fn alloc(&mut self, layout: Layout) -> Option<usize> {
            let total_pages = allocation_pages_for(layout)?;
            let base = self.next_base;
            self.next_base = self
                .next_base
                .saturating_add(total_pages.saturating_mul(PAGE_SIZE));
            self.live.insert(base, total_pages);
            Some(base.saturating_add(PAGE_SIZE))
        }

        fn dealloc(&mut self, user_ptr: usize) -> bool {
            if !is_valid_user_pointer(user_ptr) {
                return false;
            }
            let base = user_ptr.saturating_sub(PAGE_SIZE);
            let Some(pages) = self.live.get(&base).copied() else {
                return false;
            };
            let header = Header {
                magic: ALLOCATION_MAGIC,
                pages: pages as u64,
            };
            if header_pages_if_valid(header) != Some(pages) {
                return false;
            }
            self.live.remove(&base).is_some()
        }
    }

    #[test]
    fn allocation_alignment_gate_accepts_only_power_of_two_up_to_page() {
        assert!(is_supported_alignment(1));
        assert!(is_supported_alignment(8));
        assert!(is_supported_alignment(PAGE_SIZE));
        assert!(!is_supported_alignment(0));
        assert!(!is_supported_alignment(3));
        assert!(!is_supported_alignment(PAGE_SIZE.saturating_mul(2)));
    }

    #[test]
    fn returned_user_pointer_is_page_aligned_for_supported_layouts() {
        let mut sim = AllocSim::new(0x4000_0000);
        for align in [1usize, 2, 4, 8, 16, 32, 64, 256, 1024, PAGE_SIZE] {
            let layout = Layout::from_size_align(33, align).expect("layout");
            let user_ptr = sim.alloc(layout).expect("alloc");
            assert_eq!(user_ptr % PAGE_SIZE, 0, "align={align}");
        }
    }

    #[test]
    fn many_small_alloc_free_cycles_leave_no_live_allocations() {
        let mut sim = AllocSim::new(0x5000_0000);
        for _ in 0..10_000usize {
            let layout = Layout::from_size_align(1, 1).expect("layout");
            let user_ptr = sim.alloc(layout).expect("alloc");
            assert!(sim.dealloc(user_ptr));
        }
        assert!(sim.live.is_empty());
    }

    #[test]
    fn mixed_size_allocations_and_frees_are_tracked_correctly() {
        let mut sim = AllocSim::new(0x6000_0000);
        let sizes = [1usize, 15, 128, PAGE_SIZE - 1, PAGE_SIZE, PAGE_SIZE * 3 + 17];
        let mut ptrs = Vec::new();
        for size in sizes {
            let layout = Layout::from_size_align(size, 8).expect("layout");
            ptrs.push(sim.alloc(layout).expect("alloc"));
        }
        for ptr in ptrs.into_iter().rev() {
            assert!(sim.dealloc(ptr));
        }
        assert!(sim.live.is_empty());
    }

    #[test]
    fn vec_like_grow_shrink_patterns_are_safe_under_allocator_model() {
        let mut sim = AllocSim::new(0x7000_0000);
        let mut current: Option<usize> = None;
        let mut cap = 8usize;

        for _ in 0..128usize {
            let grow = Layout::from_size_align(cap, 8).expect("layout");
            let next = sim.alloc(grow).expect("grow alloc");
            if let Some(old) = current.take() {
                assert!(sim.dealloc(old));
            }
            current = Some(next);
            cap = cap.saturating_mul(2).min(PAGE_SIZE * 8);
        }

        for _ in 0..128usize {
            let shrink = Layout::from_size_align(cap.max(8), 8).expect("layout");
            let next = sim.alloc(shrink).expect("shrink alloc");
            if let Some(old) = current.take() {
                assert!(sim.dealloc(old));
            }
            current = Some(next);
            cap = (cap / 2).max(8);
        }

        if let Some(last) = current {
            assert!(sim.dealloc(last));
        }
        assert!(sim.live.is_empty());
    }

    #[test]
    fn invalid_free_rejection_requires_page_aligned_known_pointer() {
        let mut sim = AllocSim::new(0x8000_0000);
        let layout = Layout::from_size_align(64, 8).expect("layout");
        let ptr = sim.alloc(layout).expect("alloc");
        assert!(!sim.dealloc(ptr + 1));
        assert!(!sim.dealloc(0x1234_5678));
        assert!(sim.dealloc(ptr));
    }

    #[test]
    fn double_free_is_rejected_by_live_set_tracking_model() {
        let mut sim = AllocSim::new(0x9000_0000);
        let layout = Layout::from_size_align(256, 16).expect("layout");
        let ptr = sim.alloc(layout).expect("alloc");
        assert!(sim.dealloc(ptr));
        assert!(!sim.dealloc(ptr));
    }

    #[test]
    fn alignment_sensitive_layouts_above_page_size_are_rejected() {
        let mut sim = AllocSim::new(0xa000_0000);
        let ok = Layout::from_size_align(128, PAGE_SIZE).expect("ok layout");
        assert!(sim.alloc(ok).is_some());

        let too_large = Layout::from_size_align(128, PAGE_SIZE.saturating_mul(2))
            .expect("too large align layout");
        assert!(sim.alloc(too_large).is_none());
    }
}
