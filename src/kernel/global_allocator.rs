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

    const HEADER_SIZE: usize = core::mem::size_of::<u64>();
    const ALLOC_ALIGN_LIMIT: usize = PAGE_SIZE;

    #[derive(Debug, Clone, Copy)]
    struct AllocationHeader {
        pages: u64,
    }

    static ALLOCATOR_LOCK: SpinLockIrq<()> = SpinLockIrq::new(());

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
    }

    unsafe impl GlobalAlloc for KernelGlobalAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if layout.size() == 0 {
                return null_mut();
            }
            if layout.align() > ALLOC_ALIGN_LIMIT {
                return null_mut();
            }

            let _guard = ALLOCATOR_LOCK.lock();
            let user_bytes = layout.size().saturating_add(HEADER_SIZE);
            let pages = user_bytes.div_ceil(PAGE_SIZE).max(1);
            let total_pages = pages.saturating_add(1);
            let Ok(base_phys) = alloc_pt_contiguous_frames(total_pages) else {
                return null_mut();
            };
            let base_ptr = Self::phys_to_ptr(base_phys);
            if base_ptr.is_null() {
                let _ = free_pt_contiguous_frames(base_phys, total_pages);
                return null_mut();
            }

            let header = AllocationHeader {
                pages: total_pages as u64,
            };
            core::ptr::write(base_ptr as *mut AllocationHeader, header);
            base_ptr.add(PAGE_SIZE)
        }

        unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
            if ptr.is_null() {
                return;
            }
            let _guard = ALLOCATOR_LOCK.lock();
            let header_ptr = ptr.sub(PAGE_SIZE) as *const AllocationHeader;
            let header = core::ptr::read(header_ptr);
            let pages = header.pages as usize;
            if pages == 0 {
                return;
            }

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
