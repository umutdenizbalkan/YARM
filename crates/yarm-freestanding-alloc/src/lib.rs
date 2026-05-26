// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

#[macro_export]
macro_rules! install {
    ($heap_size:expr, $oom_message:expr) => {
        use core::alloc::{GlobalAlloc, Layout};
        use core::cell::UnsafeCell;
        use core::mem::{align_of, size_of};
        use core::ptr::null_mut;
        use core::sync::atomic::{AtomicBool, Ordering};

        const HEAP_SIZE: usize = $heap_size;

        #[repr(C)]
        struct BlockHeader {
            size: usize,
            next: *mut BlockHeader,
        }

        #[repr(C)]
        struct AllocHeader {
            block_start: usize,
            block_size: usize,
        }

        const HEADER_SIZE: usize = size_of::<BlockHeader>();
        const ALLOC_HEADER_SIZE: usize = size_of::<AllocHeader>();
        const MIN_SPLIT: usize = HEADER_SIZE + align_of::<usize>();

        struct RuntimeAllocator {
            heap: UnsafeCell<[u8; HEAP_SIZE]>,
            free: UnsafeCell<*mut BlockHeader>,
            initialized: AtomicBool,
            lock: AtomicBool,
        }

        unsafe impl Sync for RuntimeAllocator {}

        static ALLOC: RuntimeAllocator = RuntimeAllocator {
            heap: UnsafeCell::new([0; HEAP_SIZE]),
            free: UnsafeCell::new(null_mut()),
            initialized: AtomicBool::new(false),
            lock: AtomicBool::new(false),
        };

        #[inline]
        const fn align_up(value: usize, align: usize) -> usize {
            (value + (align - 1)) & !(align - 1)
        }

        impl RuntimeAllocator {
            fn lock(&self) {
                while self
                    .lock
                    .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
                {
                    core::hint::spin_loop();
                }
            }

            fn unlock(&self) {
                self.lock.store(false, Ordering::Release);
            }

            unsafe fn ensure_initialized(&self) {
                if self.initialized.load(Ordering::Acquire) {
                    return;
                }

                let base = self.heap.get().cast::<u8>() as usize;
                let aligned_base = align_up(base, align_of::<BlockHeader>());
                let offset = aligned_base - base;

                if offset + HEADER_SIZE > HEAP_SIZE {
                    unsafe { *self.free.get() = null_mut() };
                    self.initialized.store(true, Ordering::Release);
                    return;
                }

                let head = aligned_base as *mut BlockHeader;
                unsafe {
                    (*head).size = HEAP_SIZE - offset;
                    (*head).next = null_mut();
                    *self.free.get() = head;
                }
                self.initialized.store(true, Ordering::Release);
            }

            unsafe fn alloc_inner(&self, layout: Layout) -> *mut u8 {
                unsafe { self.ensure_initialized() };

                let request_align = layout.align().max(align_of::<usize>());
                let mut prev: *mut BlockHeader = null_mut();
                let mut cur: *mut BlockHeader = unsafe { *self.free.get() };

                while !cur.is_null() {
                    let block_start = cur as usize;
                    let payload_start = match block_start
                        .checked_add(HEADER_SIZE + ALLOC_HEADER_SIZE)
                        .map(|v| align_up(v, request_align))
                    {
                        Some(v) => v,
                        None => return null_mut(),
                    };
                    let payload_end = match payload_start.checked_add(layout.size()) {
                        Some(v) => v,
                        None => return null_mut(),
                    };
                    let block_end = match block_start.checked_add(unsafe { (*cur).size }) {
                        Some(v) => v,
                        None => return null_mut(),
                    };

                    if payload_end <= block_end {
                        let alloc_header_addr = payload_start - ALLOC_HEADER_SIZE;

                        // Bug fix #2: align the split point to align_of::<BlockHeader>()
                        // so the new free-block header is never written through a
                        // misaligned pointer (UB; bus-error on strict-alignment arches
                        // like AArch64 when size is not a multiple of the header align).
                        let split_at = align_up(payload_end, align_of::<BlockHeader>());
                        let remaining = if split_at <= block_end {
                            block_end - split_at
                        } else {
                            0
                        };
                        let next = unsafe { (*cur).next };

                        // Bug fix #1: when no split happens the allocation owns the
                        // ENTIRE original block (whole block removed from free list).
                        // Storing payload_end-block_start here would permanently leak
                        // the tail bytes because dealloc only restores block_size bytes.
                        let alloc_block_size;

                        if remaining >= MIN_SPLIT {
                            let new_free = split_at as *mut BlockHeader;
                            unsafe {
                                (*new_free).size = remaining;
                                (*new_free).next = next;
                            }
                            if prev.is_null() {
                                unsafe { *self.free.get() = new_free };
                            } else {
                                unsafe { (*prev).next = new_free };
                            }
                            // Allocation owns [block_start, split_at).
                            alloc_block_size = split_at - block_start;
                        } else {
                            // No split: allocation owns the full original block.
                            if prev.is_null() {
                                unsafe { *self.free.get() = next };
                            } else {
                                unsafe { (*prev).next = next };
                            }
                            alloc_block_size = unsafe { (*cur).size };
                        }

                        let alloc_header = alloc_header_addr as *mut AllocHeader;
                        unsafe {
                            (*alloc_header).block_start = block_start;
                            (*alloc_header).block_size = alloc_block_size;
                        }

                        return payload_start as *mut u8;
                    }

                    prev = cur;
                    cur = unsafe { (*cur).next };
                }

                null_mut()
            }

            unsafe fn dealloc_inner(&self, ptr: *mut u8) {
                if ptr.is_null() {
                    return;
                }

                unsafe { self.ensure_initialized() };

                let alloc_header_addr = (ptr as usize).saturating_sub(ALLOC_HEADER_SIZE);
                let alloc_header = alloc_header_addr as *const AllocHeader;
                let block = unsafe { (*alloc_header).block_start } as *mut BlockHeader;
                unsafe { (*block).size = (*alloc_header).block_size };
                let mut prev: *mut BlockHeader = null_mut();
                let mut cur: *mut BlockHeader = unsafe { *self.free.get() };

                while !cur.is_null() && (cur as usize) < (block as usize) {
                    prev = cur;
                    cur = unsafe { (*cur).next };
                }

                unsafe { (*block).next = cur };
                if prev.is_null() {
                    unsafe { *self.free.get() = block };
                } else {
                    unsafe { (*prev).next = block };
                }

                unsafe { self.coalesce_with_next(block) };
                if !prev.is_null() {
                    unsafe { self.coalesce_with_next(prev) };
                }
            }

            unsafe fn coalesce_with_next(&self, block: *mut BlockHeader) {
                let next = unsafe { (*block).next };
                if next.is_null() {
                    return;
                }
                if (block as usize).saturating_add(unsafe { (*block).size }) == next as usize {
                    unsafe {
                        (*block).size = (*block).size.saturating_add((*next).size);
                        (*block).next = (*next).next;
                    }
                }
            }
        }

        struct RuntimeGlobalAlloc;

        unsafe impl GlobalAlloc for RuntimeGlobalAlloc {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                if layout.size() == 0 {
                    return layout.align() as *mut u8;
                }
                ALLOC.lock();
                let ptr = unsafe { ALLOC.alloc_inner(layout) };
                ALLOC.unlock();
                ptr
            }

            unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
                if ptr.is_null() || layout.size() == 0 {
                    return;
                }
                ALLOC.lock();
                unsafe { ALLOC.dealloc_inner(ptr) };
                ALLOC.unlock();
            }
        }

        #[global_allocator]
        static RUNTIME_GLOBAL_ALLOCATOR: RuntimeGlobalAlloc = RuntimeGlobalAlloc;

        #[unsafe(no_mangle)]
        extern "C" fn __rust_alloc_error_handler(_size: usize, _align: usize) -> ! {
            panic!($oom_message)
        }
    };
}

// ---------------------------------------------------------------------------
// Test module
//
// The install! macro expands to a #[global_allocator] which conflicts with
// std's default allocator in test binaries.  Instead the tests define a
// self-contained TestFreeList struct that implements the same alloc/dealloc
// logic over a Vec<u8>-backed heap.  This lets us exercise the exact same
// code paths (including the two bug fixes) without the global-allocator
// registration.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    extern crate std;

    use core::mem::{align_of, size_of};
    use std::alloc::{Layout, alloc, dealloc};

    // -----------------------------------------------------------------------
    // Inline reimplementation of the free-list allocator logic for testing.
    // Mirrors the macro expansion exactly; both fixes are included here.
    // -----------------------------------------------------------------------

    const HEADER_SIZE: usize = size_of::<BHdr>();
    const ALLOC_HDR_SIZE: usize = size_of::<AHdr>();
    // MIN_SPLIT: smallest useful free block (header + one minimum-aligned word).
    const MIN_SPLIT: usize = HEADER_SIZE + align_of::<usize>();

    #[repr(C)]
    struct BHdr {
        size: usize,
        next: *mut BHdr,
    }

    #[repr(C)]
    struct AHdr {
        block_start: usize,
        block_size: usize,
    }

    fn align_up(v: usize, a: usize) -> usize {
        (v + (a - 1)) & !(a - 1)
    }

    /// Free-list allocator backed by a Vec<u8>.  Not thread-safe; for tests only.
    #[allow(dead_code)]
    struct TestFreeList {
        heap: std::vec::Vec<u8>,
        free_head: *mut BHdr,
    }

    impl TestFreeList {
        fn new(size: usize) -> Self {
            assert!(size >= HEADER_SIZE + 64, "heap too small for tests");
            let mut heap = std::vec::Vec::<u8>::with_capacity(size);
            // SAFETY: bytes used only through the free-list allocator below;
            // no uninitialized bytes are ever exposed to safe code.
            unsafe { heap.set_len(size) };
            let base = heap.as_mut_ptr() as usize;
            let aligned = align_up(base, align_of::<BHdr>());
            let offset = aligned - base;
            let free_head = if offset + HEADER_SIZE <= size {
                let h = aligned as *mut BHdr;
                unsafe {
                    (*h).size = size - offset;
                    (*h).next = core::ptr::null_mut();
                }
                h
            } else {
                core::ptr::null_mut()
            };
            TestFreeList { heap, free_head }
        }

        /// Sum of all free block sizes — used to detect permanent heap shrinkage.
        fn total_free_bytes(&self) -> usize {
            let mut total = 0usize;
            let mut cur = self.free_head;
            while !cur.is_null() {
                total += unsafe { (*cur).size };
                cur = unsafe { (*cur).next };
            }
            total
        }

        unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
            let request_align = layout.align().max(align_of::<usize>());
            let mut prev: *mut BHdr = core::ptr::null_mut();
            let mut cur: *mut BHdr = self.free_head;

            while !cur.is_null() {
                let block_start = cur as usize;
                let payload_start = match block_start
                    .checked_add(HEADER_SIZE + ALLOC_HDR_SIZE)
                    .map(|v| align_up(v, request_align))
                {
                    Some(v) => v,
                    None => return core::ptr::null_mut(),
                };
                let payload_end = match payload_start.checked_add(layout.size()) {
                    Some(v) => v,
                    None => return core::ptr::null_mut(),
                };
                let block_end = match block_start.checked_add(unsafe { (*cur).size }) {
                    Some(v) => v,
                    None => return core::ptr::null_mut(),
                };

                if payload_end <= block_end {
                    let alloc_header_addr = payload_start - ALLOC_HDR_SIZE;

                    // Bug fix #2: align split point to BHdr alignment.
                    let split_at = align_up(payload_end, align_of::<BHdr>());
                    let remaining = if split_at <= block_end {
                        block_end - split_at
                    } else {
                        0
                    };
                    let next = unsafe { (*cur).next };

                    // Bug fix #1: no-split path records the full original block size.
                    let alloc_block_size;

                    if remaining >= MIN_SPLIT {
                        let new_free = split_at as *mut BHdr;
                        unsafe {
                            (*new_free).size = remaining;
                            (*new_free).next = next;
                        }
                        if prev.is_null() {
                            self.free_head = new_free;
                        } else {
                            unsafe { (*prev).next = new_free };
                        }
                        alloc_block_size = split_at - block_start;
                    } else {
                        if prev.is_null() {
                            self.free_head = next;
                        } else {
                            unsafe { (*prev).next = next };
                        }
                        // Full original block — do NOT store payload_end-block_start.
                        alloc_block_size = unsafe { (*cur).size };
                    }

                    let ah = alloc_header_addr as *mut AHdr;
                    unsafe {
                        (*ah).block_start = block_start;
                        (*ah).block_size = alloc_block_size;
                    }

                    return payload_start as *mut u8;
                }

                prev = cur;
                cur = unsafe { (*cur).next };
            }
            core::ptr::null_mut()
        }

        unsafe fn dealloc(&mut self, ptr: *mut u8) {
            if ptr.is_null() {
                return;
            }
            let ah_addr = (ptr as usize).saturating_sub(ALLOC_HDR_SIZE);
            let ah = ah_addr as *const AHdr;
            let block_start = unsafe { (*ah).block_start };
            let block_size = unsafe { (*ah).block_size };
            let block = block_start as *mut BHdr;
            unsafe { (*block).size = block_size };

            let mut prev: *mut BHdr = core::ptr::null_mut();
            let mut cur: *mut BHdr = self.free_head;
            while !cur.is_null() && (cur as usize) < (block as usize) {
                prev = cur;
                cur = unsafe { (*cur).next };
            }
            unsafe { (*block).next = cur };
            if prev.is_null() {
                self.free_head = block;
            } else {
                unsafe { (*prev).next = block };
            }

            // Coalesce block with next.
            let next = unsafe { (*block).next };
            if !next.is_null()
                && unsafe { block as usize + (*block).size } == next as usize
            {
                unsafe {
                    (*block).size = (*block).size.saturating_add((*next).size);
                    (*block).next = (*next).next;
                }
            }
            // Coalesce prev with block.
            if !prev.is_null()
                && unsafe { prev as usize + (*prev).size } == block as usize
            {
                unsafe {
                    (*prev).size = (*prev).size.saturating_add((*block).size);
                    (*prev).next = (*block).next;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Bug 1 tests: no-split path must restore the full original block size.
    //
    // Setup: construct an allocation whose remaining bytes after the payload
    // are < MIN_SPLIT so no split occurs.  This means the whole free block is
    // consumed.  After dealloc the full original block size must be restored.
    //
    // With the old bug, block_size = payload_end - block_start (truncated).
    // On dealloc, only that shorter size is restored, permanently leaking the
    // tail bytes.
    // -----------------------------------------------------------------------

    /// Pick a payload size that leaves exactly `align_of::<BHdr>()` bytes
    /// after the payload end (aligned to BHdr).  That remainder is < MIN_SPLIT
    /// so no split occurs.  Returns (payload_size, free_at_start).
    fn no_split_payload(free_at_start: usize) -> Option<usize> {
        // Overhead: the BHdr at block_start plus the AHdr before payload_start.
        // With default align=8 and 8-byte aligned block_start, no extra padding.
        let overhead = HEADER_SIZE + ALLOC_HDR_SIZE; // 32 on 64-bit
        // We want: block_end - split_at = align_of::<BHdr>()
        // split_at = payload_end (when payload is already BHdr-aligned)
        // payload_end = block_start + overhead + payload_size
        // block_end   = block_start + free_at_start
        // => free_at_start - overhead - payload_size = align_of::<BHdr>()
        // => payload_size = free_at_start - overhead - align_of::<BHdr>()
        let target = free_at_start.checked_sub(overhead + align_of::<BHdr>())?;
        // Round down to a multiple of align_of::<usize>() so Layout succeeds.
        let payload = (target / align_of::<usize>()) * align_of::<usize>();
        if payload == 0 {
            return None;
        }
        // Verify the remainder would truly be < MIN_SPLIT.
        let remaining = free_at_start.checked_sub(overhead + payload)?;
        assert!(
            remaining < MIN_SPLIT,
            "remaining={remaining} not < MIN_SPLIT={MIN_SPLIT}"
        );
        assert!(
            remaining <= align_of::<BHdr>(),
            "remaining={remaining} > align_of BHdr"
        );
        Some(payload)
    }

    #[test]
    fn no_split_full_block_size_restored_after_dealloc() {
        const HEAP: usize = 4096;
        let mut a = TestFreeList::new(HEAP);
        let free_at_start = a.total_free_bytes();
        assert!(free_at_start > 0);

        let payload = match no_split_payload(free_at_start) {
            Some(p) => p,
            None => {
                // Heap is too small or alignment makes this impossible; skip.
                return;
            }
        };

        let layout = Layout::from_size_align(payload, align_of::<usize>()).unwrap();
        let p = unsafe { a.alloc(layout) };
        assert!(!p.is_null(), "initial alloc must succeed");

        // The no-split path removed the whole block from the free list.
        assert_eq!(
            a.total_free_bytes(),
            0,
            "whole block consumed — free list must be empty after no-split alloc"
        );

        unsafe { a.dealloc(p) };

        // After dealloc the full original block size must be restored.
        // Old bug: block_size = payload_end - block_start (< free_at_start),
        //   so total_free_bytes() < free_at_start (tail bytes permanently lost).
        // Fix: block_size = (*cur).size = free_at_start (entire original block).
        assert_eq!(
            a.total_free_bytes(),
            free_at_start,
            "dealloc must restore full original block size — no tail-byte leak"
        );

        // Verify the same allocation succeeds again (would OOM with the bug
        // after many cycles because the freed block keeps shrinking).
        let p2 = unsafe { a.alloc(layout) };
        assert!(!p2.is_null(), "re-alloc after dealloc must succeed");
        unsafe { a.dealloc(p2) };
    }

    #[test]
    fn no_gradual_heap_shrink_over_many_no_split_cycles() {
        const HEAP: usize = 4096;
        let mut a = TestFreeList::new(HEAP);
        let free_at_start = a.total_free_bytes();

        let payload = match no_split_payload(free_at_start) {
            Some(p) => p,
            None => return,
        };
        let layout = Layout::from_size_align(payload, align_of::<usize>()).unwrap();

        for cycle in 0..200 {
            let p = unsafe { a.alloc(layout) };
            assert!(!p.is_null(), "alloc failed on cycle {cycle}");
            unsafe { a.dealloc(p) };
            assert_eq!(
                a.total_free_bytes(),
                free_at_start,
                "heap shrank on cycle {cycle} — no-split tail-byte leak"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Bug 2 tests: split point must be aligned to align_of::<BHdr>().
    //
    // With the old code, new_free = payload_end as *mut BHdr where payload_end
    // is not aligned when size is not a multiple of align_of::<BHdr>().
    // Writing (*new_free).size through that pointer is UB (bus-error on AArch64).
    //
    // With the fix, split_at = align_up(payload_end, align_of::<BHdr>()) so
    // the new free-block header is always properly aligned.
    // -----------------------------------------------------------------------

    #[test]
    fn odd_size_alloc_dealloc_aligned_split_header() {
        // A 64 KiB heap gives plenty of room for repeated odd-size allocations.
        const HEAP: usize = 64 * 1024;
        let mut a = TestFreeList::new(HEAP);
        let free_at_start = a.total_free_bytes();

        // These sizes are NOT multiples of align_of::<BHdr>() (typically 8).
        // Without fix: split_at = payload_end = payload_start + size, unaligned
        // for odd sizes → UB when writing BlockHeader at that address.
        for &(size, align) in &[
            (17usize, 1usize),
            (31, 1),
            (33, 8),
            (65, 16),
            (1, 1),
            (3, 1),
            (7, 1),
            (9, 1),
            (15, 1),
        ] {
            let layout = Layout::from_size_align(size, align).unwrap();
            let p = unsafe { a.alloc(layout) };
            assert!(!p.is_null(), "alloc failed size={size} align={align}");
            assert_eq!((p as usize) % align, 0, "alignment violated size={size} align={align}");
            unsafe { a.dealloc(p) };
        }

        // All memory must be returned after freeing everything.
        let free_after = a.total_free_bytes();
        // Allow for BHdr alignment padding at the very start of the heap.
        assert!(
            free_after >= free_at_start.saturating_sub(align_of::<BHdr>()),
            "heap not recovered after odd-size allocs (free_after={free_after} start={free_at_start})"
        );
    }

    #[test]
    fn mixed_odd_size_alloc_dealloc_cycles() {
        const HEAP: usize = 64 * 1024;
        let mut a = TestFreeList::new(HEAP);

        let cases: &[(usize, usize)] =
            &[(17, 1), (31, 1), (33, 8), (65, 16), (100, 16), (1, 1), (7, 1)];

        // First pass: allocate all.
        let mut ptrs = std::vec::Vec::new();
        for &(size, align) in cases {
            let layout = Layout::from_size_align(size, align).unwrap();
            let p = unsafe { a.alloc(layout) };
            assert!(!p.is_null(), "alloc failed size={size} align={align}");
            ptrs.push((layout, p));
        }
        // Free in reverse order.
        for (_, p) in ptrs.iter().rev() {
            unsafe { a.dealloc(*p) };
        }
        // Second pass: all allocations must succeed again.
        let mut ptrs2 = std::vec::Vec::new();
        for &(size, align) in cases {
            let layout = Layout::from_size_align(size, align).unwrap();
            let p = unsafe { a.alloc(layout) };
            assert!(!p.is_null(), "realloc failed size={size} align={align}");
            ptrs2.push((layout, p));
        }
        for (_, p) in ptrs2 {
            unsafe { a.dealloc(p) };
        }
    }

    /// High-alignment allocations (align > align_of::<usize>()).
    /// split_at rounding stays correct because align_of::<BHdr>() ≤ payload align.
    #[test]
    fn high_alignment_alloc_dealloc_roundtrip_freelist() {
        const HEAP: usize = 64 * 1024;
        let mut a = TestFreeList::new(HEAP);
        for &(size, align) in &[(100usize, 64usize), (64, 64), (1, 64), (33, 32)] {
            let layout = Layout::from_size_align(size, align).unwrap();
            let p = unsafe { a.alloc(layout) };
            assert!(!p.is_null(), "alloc failed size={size} align={align}");
            assert_eq!((p as usize) % align, 0, "alignment violated");
            unsafe { a.dealloc(p) };
        }
    }

    // -----------------------------------------------------------------------
    // Existing tests — use std's allocator via std::alloc::{alloc, dealloc}.
    // These are kept as interface-contract regression tests.
    // -----------------------------------------------------------------------

    #[test]
    fn alloc_free_reuse_same_block() {
        let layout = Layout::from_size_align(64, 8).unwrap();
        let a = unsafe { alloc(layout) };
        assert!(!a.is_null());
        unsafe { dealloc(a, layout) };
        let b = unsafe { alloc(layout) };
        assert!(!b.is_null());
        unsafe { dealloc(b, layout) };
    }

    #[test]
    fn alloc_many_free_many_reuse() {
        let small = Layout::from_size_align(1024, 8).unwrap();
        let mut ptrs = std::vec::Vec::new();
        for _ in 0..64 {
            let p = unsafe { alloc(small) };
            assert!(!p.is_null());
            ptrs.push(p);
        }
        for p in ptrs.drain(..) {
            unsafe { dealloc(p, small) };
        }
        let large = Layout::from_size_align(48 * 1024, 8).unwrap();
        let p = unsafe { alloc(large) };
        assert!(!p.is_null());
        unsafe { dealloc(p, large) };
    }

    #[test]
    fn alignment_is_respected() {
        for align in [1, 8, 16, 64] {
            let layout = Layout::from_size_align(96, align).unwrap();
            let p = unsafe { alloc(layout) };
            assert!(!p.is_null());
            assert_eq!((p as usize) % align, 0);
            unsafe { dealloc(p, layout) };
        }
    }

    #[test]
    fn split_and_coalesce() {
        let large = Layout::from_size_align(4096, 16).unwrap();
        let a = unsafe { alloc(large) };
        let b = unsafe { alloc(large) };
        let c = unsafe { alloc(large) };
        assert!(!a.is_null() && !b.is_null() && !c.is_null());
        unsafe { dealloc(b, large) };
        let small = Layout::from_size_align(1024, 16).unwrap();
        let b2 = unsafe { alloc(small) };
        assert!(!b2.is_null());
        unsafe {
            dealloc(a, large);
            dealloc(b2, small);
            dealloc(c, large);
        }
        let big = Layout::from_size_align(14 * 1024, 16).unwrap();
        let p = unsafe { alloc(big) };
        assert!(!p.is_null());
        unsafe { dealloc(p, big) };
    }

    #[test]
    fn high_alignment_alloc_dealloc_roundtrip() {
        for (size, align) in [(100usize, 64usize), (4096usize, 4096usize)] {
            let layout = Layout::from_size_align(size, align).unwrap();
            let p = unsafe { alloc(layout) };
            assert!(!p.is_null());
            assert_eq!((p as usize) % align, 0);
            unsafe { core::ptr::write_bytes(p, 0xA5, size) };
            unsafe { dealloc(p, layout) };
            let p2 = unsafe { alloc(layout) };
            assert!(!p2.is_null());
            assert_eq!((p2 as usize) % align, 0);
            unsafe { dealloc(p2, layout) };
        }
    }

    #[test]
    fn interleaved_alignments_reuse_and_coalesce() {
        let l1 = Layout::from_size_align(24, 8).unwrap();
        let l2 = Layout::from_size_align(100, 64).unwrap();
        let l3 = Layout::from_size_align(17, 16).unwrap();
        let a = unsafe { alloc(l1) };
        let b = unsafe { alloc(l2) };
        let c = unsafe { alloc(l3) };
        assert!(!a.is_null() && !b.is_null() && !c.is_null());
        unsafe { dealloc(b, l2) };
        let l2b = Layout::from_size_align(80, 64).unwrap();
        let b2 = unsafe { alloc(l2b) };
        assert!(!b2.is_null());
        unsafe {
            dealloc(a, l1);
            dealloc(b2, l2b);
            dealloc(c, l3);
        }
        let big = Layout::from_size_align(16 * 1024, 16).unwrap();
        let p = unsafe { alloc(big) };
        assert!(!p.is_null());
        unsafe { dealloc(p, big) };
    }

    #[test]
    fn vec_string_like_large_then_small_then_large() {
        let large = Layout::from_size_align(80 * 1024, 16).unwrap();
        let p1 = unsafe { alloc(large) };
        assert!(!p1.is_null());
        unsafe { dealloc(p1, large) };
        let small = Layout::from_size_align(512, 8).unwrap();
        let mut ptrs = std::vec::Vec::new();
        for _ in 0..32 {
            let p = unsafe { alloc(small) };
            assert!(!p.is_null());
            ptrs.push(p);
        }
        for p in ptrs {
            unsafe { dealloc(p, small) };
        }
        let p2 = unsafe { alloc(large) };
        assert!(!p2.is_null());
        unsafe { dealloc(p2, large) };
    }

    #[test]
    fn pm_like_large_temp_sequence() {
        for size in [85 * 1024, 95 * 1024, 84 * 1024] {
            let layout = Layout::from_size_align(size, 16).unwrap();
            let p = unsafe { alloc(layout) };
            assert!(!p.is_null());
            unsafe { dealloc(p, layout) };
        }
    }
}
