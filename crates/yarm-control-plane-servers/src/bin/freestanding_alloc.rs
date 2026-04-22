// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![allow(dead_code)]

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicUsize, Ordering};

const HEAP_SIZE: usize = 2 * 1024 * 1024;
static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
static NEXT: AtomicUsize = AtomicUsize::new(0);

#[inline]
const fn align_up(value: usize, align: usize) -> usize {
    (value + (align - 1)) & !(align - 1)
}

struct RuntimeBumpAllocator;

// SAFETY: Allocation state is monotonic and updated atomically, preventing overlap.
unsafe impl GlobalAlloc for RuntimeBumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() == 0 {
            return layout.align() as *mut u8;
        }

        let result = NEXT.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cursor| {
            let start = align_up(cursor, layout.align());
            let end = start.checked_add(layout.size())?;
            if end <= HEAP_SIZE { Some(end) } else { None }
        });

        match result {
            Ok(start) => {
                // SAFETY: start is bounded by HEAP_SIZE and comes from monotonic bump allocation.
                unsafe { core::ptr::addr_of_mut!(HEAP).cast::<u8>().add(start) }
            }
            Err(_) => null_mut(),
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static RUNTIME_GLOBAL_ALLOCATOR: RuntimeBumpAllocator = RuntimeBumpAllocator;

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    panic!("control-plane server freestanding allocator OOM")
}
