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
                    *self.free.get() = null_mut();
                    self.initialized.store(true, Ordering::Release);
                    return;
                }

                let head = aligned_base as *mut BlockHeader;
                (*head).size = HEAP_SIZE - offset;
                (*head).next = null_mut();
                *self.free.get() = head;
                self.initialized.store(true, Ordering::Release);
            }

            unsafe fn alloc_inner(&self, layout: Layout) -> *mut u8 {
                self.ensure_initialized();

                let request_align = layout.align().max(align_of::<usize>());
                let mut prev: *mut BlockHeader = null_mut();
                let mut cur: *mut BlockHeader = *self.free.get();

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
                    let block_end = match block_start.checked_add((*cur).size) {
                        Some(v) => v,
                        None => return null_mut(),
                    };

                    if payload_end <= block_end {
                        let alloc_header_addr = payload_start - ALLOC_HEADER_SIZE;
                        let remaining = block_end - payload_end;
                        let next = (*cur).next;

                        if remaining >= MIN_SPLIT {
                            let new_free = payload_end as *mut BlockHeader;
                            (*new_free).size = remaining;
                            (*new_free).next = next;
                            if prev.is_null() {
                                *self.free.get() = new_free;
                            } else {
                                (*prev).next = new_free;
                            }
                            (*cur).size = payload_end - block_start;
                        } else if prev.is_null() {
                            *self.free.get() = next;
                        } else {
                            (*prev).next = next;
                        }

                        let alloc_header = alloc_header_addr as *mut AllocHeader;
                        (*alloc_header).block_start = block_start;
                        (*alloc_header).block_size = payload_end - block_start;

                        return payload_start as *mut u8;
                    }

                    prev = cur;
                    cur = (*cur).next;
                }

                null_mut()
            }

            unsafe fn dealloc_inner(&self, ptr: *mut u8) {
                if ptr.is_null() {
                    return;
                }

                self.ensure_initialized();

                let alloc_header_addr = (ptr as usize).saturating_sub(ALLOC_HEADER_SIZE);
                let alloc_header = alloc_header_addr as *const AllocHeader;
                let block = (*alloc_header).block_start as *mut BlockHeader;
                (*block).size = (*alloc_header).block_size;
                let mut prev: *mut BlockHeader = null_mut();
                let mut cur: *mut BlockHeader = *self.free.get();

                while !cur.is_null() && (cur as usize) < (block as usize) {
                    prev = cur;
                    cur = (*cur).next;
                }

                (*block).next = cur;
                if prev.is_null() {
                    *self.free.get() = block;
                } else {
                    (*prev).next = block;
                }

                self.coalesce_with_next(block);
                if !prev.is_null() {
                    self.coalesce_with_next(prev);
                }
            }

            unsafe fn coalesce_with_next(&self, block: *mut BlockHeader) {
                let next = (*block).next;
                if next.is_null() {
                    return;
                }
                if (block as usize).saturating_add((*block).size) == next as usize {
                    (*block).size = (*block).size.saturating_add((*next).size);
                    (*block).next = (*next).next;
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

#[cfg(test)]
mod tests {
    extern crate std;

    use std::alloc::{Layout, alloc, dealloc};

    #[test]
    fn alloc_free_reuse_same_block() {
        let layout = Layout::from_size_align(64, 8).unwrap();
        let a = unsafe { alloc(layout) };
        assert!(!a.is_null());
        unsafe { dealloc(a, layout) };
        let b = unsafe { alloc(layout) };
        assert!(!b.is_null());
        assert_eq!(a, b);
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
