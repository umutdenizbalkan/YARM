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

        #[inline]
        const fn checked_align_up(value: usize, align: usize) -> Option<usize> {
            match value.checked_add(align - 1) {
                Some(rounded) => Some(rounded & !(align - 1)),
                None => None,
            }
        }

        impl RuntimeAllocator {
            fn lock(&self) {
                loop {
                    while self.lock.load(Ordering::Relaxed) {
                        core::hint::spin_loop();
                    }
                    if self
                        .lock
                        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                        .is_ok()
                    {
                        return;
                    }
                    core::hint::spin_loop();
                }
            }

            fn unlock(&self) {
                self.lock.store(false, Ordering::Release);
            }

            unsafe fn ensure_initialized_locked(&self) {
                if self.initialized.load(Ordering::Relaxed) {
                    return;
                }

                let base = self.heap.get().cast::<u8>() as usize;
                let Some(aligned_base) = checked_align_up(base, align_of::<BlockHeader>()) else {
                    unsafe { *self.free.get() = null_mut() };
                    self.initialized.store(true, Ordering::Release);
                    return;
                };
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

            fn heap_bounds(&self) -> (usize, usize) {
                let start = self.heap.get().cast::<u8>() as usize;
                let end = start.saturating_add(HEAP_SIZE);
                (start, end)
            }

            fn ptr_may_have_alloc_header(&self, ptr: *mut u8) -> bool {
                let ptr_addr = ptr as usize;
                let (heap_start, heap_end) = self.heap_bounds();
                if ptr_addr < heap_start || ptr_addr >= heap_end {
                    return false;
                }
                let Some(header_addr) = ptr_addr.checked_sub(ALLOC_HEADER_SIZE) else {
                    return false;
                };
                let Some(header_end) = header_addr.checked_add(ALLOC_HEADER_SIZE) else {
                    return false;
                };
                header_addr >= heap_start && header_end <= heap_end
            }

            fn alloc_header_is_plausible(&self, header: &AllocHeader) -> bool {
                let (heap_start, heap_end) = self.heap_bounds();
                if header.block_start < heap_start || header.block_start >= heap_end {
                    return false;
                }
                if !header.block_start.is_multiple_of(align_of::<BlockHeader>()) {
                    return false;
                }
                if header.block_size < HEADER_SIZE || header.block_size > HEAP_SIZE {
                    return false;
                }
                let Some(block_end) = header.block_start.checked_add(header.block_size) else {
                    return false;
                };
                block_end <= heap_end
            }

            unsafe fn alloc_inner(&self, layout: Layout) -> *mut u8 {
                let request_align = layout.align().max(align_of::<usize>());
                let mut prev: *mut BlockHeader = null_mut();
                let mut cur: *mut BlockHeader = unsafe { *self.free.get() };

                while !cur.is_null() {
                    let block_start = cur as usize;
                    let payload_start = match block_start
                        .checked_add(HEADER_SIZE + ALLOC_HEADER_SIZE)
                        .and_then(|v| checked_align_up(v, request_align))
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

                        // High-align requests can create a sizable gap between the free block's
                        // header and the allocation header. If the gap is large enough to be a
                        // useful free block, split it off instead of trapping it inside the active
                        // allocation until dealloc. Smaller gaps stay in the allocation block so
                        // the free list never contains an unusable fragment.
                        let leading = alloc_header_addr - block_start;
                        let front_split = leading >= MIN_SPLIT;
                        let alloc_block_start = if front_split {
                            alloc_header_addr
                        } else {
                            block_start
                        };

                        // Bug fix #2: align the split point to align_of::<BlockHeader>()
                        // so the new free-block header is never written through a
                        // misaligned pointer (UB; bus-error on strict-alignment arches
                        // like AArch64 when size is not a multiple of the header align).
                        let Some(split_at) =
                            checked_align_up(payload_end, align_of::<BlockHeader>())
                        else {
                            return null_mut();
                        };
                        let tail_remaining = if split_at <= block_end {
                            block_end - split_at
                        } else {
                            0
                        };
                        let tail_split = tail_remaining >= MIN_SPLIT;
                        let alloc_block_end = if tail_split { split_at } else { block_end };
                        let next = unsafe { (*cur).next };

                        if front_split {
                            unsafe {
                                (*cur).size = leading;
                            }
                        }

                        let tail_free = if tail_split {
                            let new_free = split_at as *mut BlockHeader;
                            unsafe {
                                (*new_free).size = tail_remaining;
                                (*new_free).next = next;
                            }
                            new_free
                        } else {
                            next
                        };

                        if front_split {
                            unsafe { (*cur).next = tail_free };
                            if prev.is_null() {
                                unsafe { *self.free.get() = cur };
                            } else {
                                unsafe { (*prev).next = cur };
                            }
                        } else if prev.is_null() {
                            unsafe { *self.free.get() = tail_free };
                        } else {
                            unsafe { (*prev).next = tail_free };
                        }

                        let alloc_header = alloc_header_addr as *mut AllocHeader;
                        unsafe {
                            (*alloc_header).block_start = alloc_block_start;
                            (*alloc_header).block_size = alloc_block_end - alloc_block_start;
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

                if !self.ptr_may_have_alloc_header(ptr) {
                    debug_assert!(false, "invalid freestanding allocator dealloc pointer");
                    return;
                }

                let Some(alloc_header_addr) = (ptr as usize).checked_sub(ALLOC_HEADER_SIZE) else {
                    debug_assert!(false, "invalid freestanding allocator dealloc pointer");
                    return;
                };
                let alloc_header = alloc_header_addr as *const AllocHeader;
                let header = unsafe { &*alloc_header };
                if !self.alloc_header_is_plausible(header) {
                    debug_assert!(false, "invalid freestanding allocator allocation header");
                    return;
                }
                let block = header.block_start as *mut BlockHeader;
                unsafe { (*block).size = header.block_size };
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
                unsafe { ALLOC.ensure_initialized_locked() };
                let ptr = unsafe { ALLOC.alloc_inner(layout) };
                ALLOC.unlock();
                ptr
            }

            unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
                if ptr.is_null() || layout.size() == 0 {
                    return;
                }
                ALLOC.lock();
                unsafe { ALLOC.ensure_initialized_locked() };
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
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

    fn checked_align_up(v: usize, a: usize) -> Option<usize> {
        v.checked_add(a - 1).map(|rounded| rounded & !(a - 1))
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

        fn free_block_count(&self) -> usize {
            let mut count = 0usize;
            let mut cur = self.free_head;
            while !cur.is_null() {
                count += 1;
                cur = unsafe { (*cur).next };
            }
            count
        }

        fn heap_bounds(&self) -> (usize, usize) {
            let start = self.heap.as_ptr() as usize;
            (start, start + self.heap.len())
        }

        fn ptr_may_have_alloc_header(&self, ptr: *mut u8) -> bool {
            let ptr_addr = ptr as usize;
            let (heap_start, heap_end) = self.heap_bounds();
            if ptr_addr < heap_start || ptr_addr >= heap_end {
                return false;
            }
            let Some(header_addr) = ptr_addr.checked_sub(ALLOC_HDR_SIZE) else {
                return false;
            };
            let Some(header_end) = header_addr.checked_add(ALLOC_HDR_SIZE) else {
                return false;
            };
            header_addr >= heap_start && header_end <= heap_end
        }

        fn alloc_header_is_plausible(&self, h: &AHdr) -> bool {
            let (heap_start, heap_end) = self.heap_bounds();
            if h.block_start < heap_start || h.block_start >= heap_end {
                return false;
            }
            if !h.block_start.is_multiple_of(align_of::<BHdr>()) {
                return false;
            }
            if h.block_size < HEADER_SIZE || h.block_size > self.heap.len() {
                return false;
            }
            let Some(block_end) = h.block_start.checked_add(h.block_size) else {
                return false;
            };
            block_end <= heap_end
        }

        fn free_blocks(&self) -> std::vec::Vec<(usize, usize)> {
            let mut blocks = std::vec::Vec::new();
            let mut cur = self.free_head;
            while !cur.is_null() {
                blocks.push((cur as usize, unsafe { (*cur).size }));
                cur = unsafe { (*cur).next };
            }
            blocks
        }

        fn alloc_header_for(&self, ptr: *mut u8) -> AHdr {
            assert!(self.ptr_may_have_alloc_header(ptr));
            let ah_addr = (ptr as usize).checked_sub(ALLOC_HDR_SIZE).unwrap();
            let ah = ah_addr as *const AHdr;
            AHdr {
                block_start: unsafe { (*ah).block_start },
                block_size: unsafe { (*ah).block_size },
            }
        }

        fn front_split_layout(&self, size: usize) -> (Layout, usize) {
            let block_start = self.free_head as usize;
            let block_end = block_start + unsafe { (*self.free_head).size };
            for align in [64usize, 128, 256, 512, 1024, 2048, 4096, 8192, 16_384] {
                let payload_start =
                    checked_align_up(block_start + HEADER_SIZE + ALLOC_HDR_SIZE, align)
                        .expect("align");
                let alloc_header_addr = payload_start - ALLOC_HDR_SIZE;
                let leading = alloc_header_addr - block_start;
                if leading >= MIN_SPLIT && payload_start + size <= block_end {
                    return (Layout::from_size_align(size, align).unwrap(), leading);
                }
            }
            panic!("could not find front-splitting alignment for test heap");
        }

        fn front_split_layout_with_min_leading(
            &self,
            size: usize,
            min_leading: usize,
        ) -> (Layout, usize) {
            let block_start = self.free_head as usize;
            let block_end = block_start + unsafe { (*self.free_head).size };
            for align in [64usize, 128, 256, 512, 1024, 2048, 4096, 8192, 16_384] {
                let payload_start =
                    checked_align_up(block_start + HEADER_SIZE + ALLOC_HDR_SIZE, align)
                        .expect("align");
                let alloc_header_addr = payload_start - ALLOC_HDR_SIZE;
                let leading = alloc_header_addr - block_start;
                if leading >= min_leading && payload_start + size <= block_end {
                    return (Layout::from_size_align(size, align).unwrap(), leading);
                }
            }
            panic!("could not find requested front-splitting alignment for test heap");
        }

        fn front_split_no_tail_layout(&self) -> (Layout, usize) {
            let block_start = self.free_head as usize;
            let block_end = block_start + unsafe { (*self.free_head).size };
            for align in [64usize, 128, 256, 512, 1024, 2048, 4096, 8192, 16_384] {
                let payload_start =
                    checked_align_up(block_start + HEADER_SIZE + ALLOC_HDR_SIZE, align)
                        .expect("align");
                let alloc_header_addr = payload_start - ALLOC_HDR_SIZE;
                let leading = alloc_header_addr - block_start;
                if leading < MIN_SPLIT || payload_start >= block_end {
                    continue;
                }
                let size = block_end - payload_start;
                if let Ok(layout) = Layout::from_size_align(size, align) {
                    return (layout, leading);
                }
            }
            panic!("could not find front-split/no-tail layout for test heap");
        }

        unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
            let request_align = layout.align().max(align_of::<usize>());
            let mut prev: *mut BHdr = core::ptr::null_mut();
            let mut cur: *mut BHdr = self.free_head;

            while !cur.is_null() {
                let block_start = cur as usize;
                let payload_start = match block_start
                    .checked_add(HEADER_SIZE + ALLOC_HDR_SIZE)
                    .and_then(|v| checked_align_up(v, request_align))
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
                    let leading = alloc_header_addr - block_start;
                    let front_split = leading >= MIN_SPLIT;
                    let alloc_block_start = if front_split {
                        alloc_header_addr
                    } else {
                        block_start
                    };

                    // Bug fix #2: align split point to BHdr alignment.
                    let Some(split_at) = checked_align_up(payload_end, align_of::<BHdr>()) else {
                        return core::ptr::null_mut();
                    };
                    let tail_remaining = if split_at <= block_end {
                        block_end - split_at
                    } else {
                        0
                    };
                    let tail_split = tail_remaining >= MIN_SPLIT;
                    let alloc_block_end = if tail_split { split_at } else { block_end };
                    let next = unsafe { (*cur).next };

                    if front_split {
                        unsafe { (*cur).size = leading };
                    }

                    let tail_free = if tail_split {
                        let new_free = split_at as *mut BHdr;
                        unsafe {
                            (*new_free).size = tail_remaining;
                            (*new_free).next = next;
                        }
                        new_free
                    } else {
                        next
                    };

                    if front_split {
                        unsafe { (*cur).next = tail_free };
                        if prev.is_null() {
                            self.free_head = cur;
                        } else {
                            unsafe { (*prev).next = cur };
                        }
                    } else if prev.is_null() {
                        self.free_head = tail_free;
                    } else {
                        unsafe { (*prev).next = tail_free };
                    }

                    let ah = alloc_header_addr as *mut AHdr;
                    unsafe {
                        (*ah).block_start = alloc_block_start;
                        (*ah).block_size = alloc_block_end - alloc_block_start;
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
            if !self.ptr_may_have_alloc_header(ptr) {
                return;
            }
            let Some(ah_addr) = (ptr as usize).checked_sub(ALLOC_HDR_SIZE) else {
                return;
            };
            let ah = ah_addr as *const AHdr;
            let header = unsafe { &*ah };
            if !self.alloc_header_is_plausible(header) {
                return;
            }
            let block_start = header.block_start;
            let block_size = header.block_size;
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
            if !next.is_null() && unsafe { block as usize + (*block).size } == next as usize {
                unsafe {
                    (*block).size = (*block).size.saturating_add((*next).size);
                    (*block).next = (*next).next;
                }
            }
            // Coalesce prev with block.
            if !prev.is_null() && unsafe { prev as usize + (*prev).size } == block as usize {
                unsafe {
                    (*prev).size = (*prev).size.saturating_add((*block).size);
                    (*prev).next = (*block).next;
                }
            }
        }
    }

    struct TestInitAllocator {
        heap: std::vec::Vec<u8>,
        free_head: *mut BHdr,
        initialized: bool,
        init_count: usize,
    }

    impl TestInitAllocator {
        fn new(size: usize) -> Self {
            let mut heap = std::vec::Vec::<u8>::with_capacity(size);
            unsafe { heap.set_len(size) };
            Self {
                heap,
                free_head: core::ptr::null_mut(),
                initialized: false,
                init_count: 0,
            }
        }

        fn ensure_initialized_locked(&mut self) {
            if self.initialized {
                return;
            }
            self.init_count += 1;
            let base = self.heap.as_mut_ptr() as usize;
            let Some(aligned) = checked_align_up(base, align_of::<BHdr>()) else {
                self.initialized = true;
                return;
            };
            let offset = aligned - base;
            if offset + HEADER_SIZE > self.heap.len() {
                self.free_head = core::ptr::null_mut();
                self.initialized = true;
                return;
            }
            let head = aligned as *mut BHdr;
            unsafe {
                (*head).size = self.heap.len() - offset;
                (*head).next = core::ptr::null_mut();
            }
            self.free_head = head;
            self.initialized = true;
        }
    }

    struct TestTtasLock {
        locked: AtomicBool,
        cas_attempts: AtomicUsize,
    }

    impl TestTtasLock {
        const fn new(locked: bool) -> Self {
            Self {
                locked: AtomicBool::new(locked),
                cas_attempts: AtomicUsize::new(0),
            }
        }

        fn try_lock_once(&self) -> bool {
            if self.locked.load(Ordering::Relaxed) {
                return false;
            }
            self.cas_attempts.fetch_add(1, Ordering::Relaxed);
            self.locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        }

        fn unlock(&self) {
            self.locked.store(false, Ordering::Release);
        }
    }

    #[test]
    fn initialization_happens_once_under_allocator_lock_model() {
        let mut a = TestInitAllocator::new(4096);
        a.ensure_initialized_locked();
        let first_head = a.free_head;
        let first_size = unsafe { (*first_head).size };
        a.ensure_initialized_locked();
        assert_eq!(a.init_count, 1);
        assert_eq!(a.free_head, first_head);
        assert_eq!(unsafe { (*a.free_head).size }, first_size);
    }

    #[test]
    fn ttas_lock_does_not_cas_while_obviously_locked() {
        let lock = TestTtasLock::new(true);
        assert!(!lock.try_lock_once());
        assert_eq!(lock.cas_attempts.load(Ordering::Relaxed), 0);
        lock.unlock();
        assert!(lock.try_lock_once());
        assert_eq!(lock.cas_attempts.load(Ordering::Relaxed), 1);
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
            assert_eq!(
                (p as usize) % align,
                0,
                "alignment violated size={size} align={align}"
            );
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

        let cases: &[(usize, usize)] = &[
            (17, 1),
            (31, 1),
            (33, 8),
            (65, 16),
            (100, 16),
            (1, 1),
            (7, 1),
        ];

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

    #[test]
    fn high_align_front_split_creates_front_free_block_and_tail() {
        const HEAP: usize = 64 * 1024;
        let mut a = TestFreeList::new(HEAP);
        let free_start = a.free_head as usize;
        let free_at_start = a.total_free_bytes();
        let (layout, leading) = a.front_split_layout(128);
        let p = unsafe { a.alloc(layout) };
        assert!(!p.is_null());
        assert_eq!((p as usize) % layout.align(), 0);

        let header = a.alloc_header_for(p);
        assert_eq!(header.block_start, (p as usize) - ALLOC_HDR_SIZE);
        assert_eq!(header.block_start - free_start, leading);
        let blocks = a.free_blocks();
        assert_eq!(blocks.len(), 2, "front and tail free blocks should remain");
        assert_eq!(blocks[0], (free_start, leading));
        assert_eq!(a.total_free_bytes() + header.block_size, free_at_start);

        unsafe { a.dealloc(p) };
        assert_eq!(a.free_block_count(), 1);
        assert_eq!(a.total_free_bytes(), free_at_start);
    }

    #[test]
    fn small_leading_padding_stays_in_allocation_until_dealloc() {
        const HEAP: usize = 4096;
        let mut a = TestFreeList::new(HEAP);
        let free_start = a.free_head as usize;
        let free_at_start = a.total_free_bytes();
        let layout = Layout::from_size_align(64, align_of::<usize>()).unwrap();
        let p = unsafe { a.alloc(layout) };
        assert!(!p.is_null());
        let header = a.alloc_header_for(p);
        let leading = (p as usize) - ALLOC_HDR_SIZE - free_start;
        assert!(leading < MIN_SPLIT);
        assert_eq!(header.block_start, free_start);

        unsafe { a.dealloc(p) };
        assert_eq!(a.free_block_count(), 1);
        assert_eq!(a.total_free_bytes(), free_at_start);
    }

    #[test]
    fn front_split_tail_split_preserves_full_coverage() {
        const HEAP: usize = 64 * 1024;
        let mut a = TestFreeList::new(HEAP);
        let free_at_start = a.total_free_bytes();
        let (layout, _) = a.front_split_layout(1024);
        let p = unsafe { a.alloc(layout) };
        assert!(!p.is_null());
        let header = a.alloc_header_for(p);
        let blocks = a.free_blocks();
        assert_eq!(
            blocks.len(),
            2,
            "front split should coexist with tail split"
        );
        assert_eq!(a.total_free_bytes() + header.block_size, free_at_start);
        assert_eq!(blocks[0].0 + blocks[0].1, header.block_start);
        assert_eq!(header.block_start + header.block_size, blocks[1].0);

        unsafe { a.dealloc(p) };
        assert_eq!(a.free_block_count(), 1);
        assert_eq!(a.total_free_bytes(), free_at_start);
    }

    #[test]
    fn too_large_high_alignment_fails_without_corrupting_freelist() {
        const HEAP: usize = 4096;
        let mut a = TestFreeList::new(HEAP);
        let free_at_start = a.total_free_bytes();
        let layout = Layout::from_size_align(1, 1 << 20).unwrap();
        let p = unsafe { a.alloc(layout) };
        assert!(p.is_null());
        assert_eq!(a.free_block_count(), 1);
        assert_eq!(a.total_free_bytes(), free_at_start);
    }

    fn ranges_overlap(a_start: usize, a_size: usize, b_start: usize, b_size: usize) -> bool {
        let a_end = a_start + a_size;
        let b_end = b_start + b_size;
        a_start < b_end && b_start < a_end
    }

    #[test]
    fn front_split_without_tail_links_front_directly_to_original_next() {
        const HEAP: usize = 64 * 1024;
        let mut a = TestFreeList::new(HEAP);
        let free_start = a.free_head as usize;
        let free_at_start = a.total_free_bytes();
        let (layout, leading) = a.front_split_no_tail_layout();
        let p = unsafe { a.alloc(layout) };
        assert!(!p.is_null());
        let header = a.alloc_header_for(p);
        let blocks = a.free_blocks();
        assert_eq!(blocks, std::vec![(free_start, leading)]);
        assert_eq!(header.block_start, free_start + leading);
        assert_eq!(
            header.block_start + header.block_size,
            free_start + free_at_start
        );
        assert_eq!(a.total_free_bytes() + header.block_size, free_at_start);

        unsafe { a.dealloc(p) };
        assert_eq!(a.free_block_count(), 1);
        assert_eq!(a.total_free_bytes(), free_at_start);
    }

    #[test]
    fn subsequent_front_allocation_cannot_overlap_live_high_align_block() {
        const HEAP: usize = 64 * 1024;
        let mut a = TestFreeList::new(HEAP);
        let (layout, _) = a.front_split_layout_with_min_leading(1024, 512);
        let high = unsafe { a.alloc(layout) };
        assert!(!high.is_null());
        let high_header = a.alloc_header_for(high);

        let small_layout = Layout::from_size_align(32, align_of::<usize>()).unwrap();
        let small = unsafe { a.alloc(small_layout) };
        assert!(!small.is_null());
        let small_header = a.alloc_header_for(small);
        assert!(small_header.block_start < high_header.block_start);
        assert!(!ranges_overlap(
            small_header.block_start,
            small_header.block_size,
            high_header.block_start,
            high_header.block_size,
        ));

        unsafe { a.dealloc(small) };
        unsafe { a.dealloc(high) };
        assert_eq!(a.free_block_count(), 1);
    }

    #[test]
    fn subsequent_tail_allocation_cannot_overlap_live_high_align_block() {
        const HEAP: usize = 64 * 1024;
        let mut a = TestFreeList::new(HEAP);
        let (layout, _) = a.front_split_layout_with_min_leading(1024, 512);
        let high = unsafe { a.alloc(layout) };
        assert!(!high.is_null());
        let high_header = a.alloc_header_for(high);

        // Request more payload bytes than the front block can satisfy including allocator
        // metadata, forcing first-fit search to skip the front block and use the tail block.
        let front_free_size = a.free_blocks()[0].1;
        let tail_layout = Layout::from_size_align(front_free_size, align_of::<usize>()).unwrap();
        let tail = unsafe { a.alloc(tail_layout) };
        assert!(!tail.is_null());
        let tail_header = a.alloc_header_for(tail);
        assert!(tail_header.block_start >= high_header.block_start + high_header.block_size);
        assert!(!ranges_overlap(
            tail_header.block_start,
            tail_header.block_size,
            high_header.block_start,
            high_header.block_size,
        ));

        unsafe { a.dealloc(tail) };
        unsafe { a.dealloc(high) };
        assert_eq!(a.free_block_count(), 1);
    }

    #[test]
    fn front_split_next_pointer_never_bypasses_live_allocation() {
        const HEAP: usize = 64 * 1024;
        let mut a = TestFreeList::new(HEAP);
        let (layout, _) = a.front_split_layout(512);
        let high = unsafe { a.alloc(layout) };
        assert!(!high.is_null());
        let high_header = a.alloc_header_for(high);
        let blocks = a.free_blocks();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0 + blocks[0].1, high_header.block_start);
        assert_eq!(
            high_header.block_start + high_header.block_size,
            blocks[1].0
        );

        let first = a.free_head;
        let next = unsafe { (*first).next };
        assert_eq!(next as usize, blocks[1].0);
        assert!(next as usize >= high_header.block_start + high_header.block_size);

        unsafe { a.dealloc(high) };
        assert_eq!(a.free_block_count(), 1);
    }

    #[test]
    fn high_alignment_churn_returns_to_single_free_block() {
        const HEAP: usize = 64 * 1024;
        let mut a = TestFreeList::new(HEAP);
        let free_at_start = a.total_free_bytes();
        let cases = [
            Layout::from_size_align(1, 64).unwrap(),
            Layout::from_size_align(97, 128).unwrap(),
            Layout::from_size_align(4096, 4096).unwrap(),
            Layout::from_size_align(33, 256).unwrap(),
        ];
        let mut ptrs = std::vec::Vec::new();
        for layout in cases {
            let p = unsafe { a.alloc(layout) };
            assert!(!p.is_null());
            assert_eq!((p as usize) % layout.align(), 0);
            ptrs.push((layout, p));
        }
        for (_, p) in ptrs.into_iter().rev() {
            unsafe { a.dealloc(p) };
        }
        assert_eq!(a.total_free_bytes(), free_at_start);
        assert_eq!(a.free_block_count(), 1);
    }

    #[test]
    fn zero_size_sentinel_wrong_layout_is_rejected_before_header_read() {
        let mut a = TestFreeList::new(4096);
        let free_at_start = a.total_free_bytes();
        let sentinel = align_of::<usize>() as *mut u8;
        unsafe { a.dealloc(sentinel) };
        assert_eq!(a.total_free_bytes(), free_at_start);
        assert_eq!(a.free_block_count(), 1);
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
