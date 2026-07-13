// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

#[cfg(not(feature = "hosted-dev"))]
unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

static mut BOOTSTRAP_KERNEL_STATE: core::mem::MaybeUninit<KernelState> =
    core::mem::MaybeUninit::uninit();

// Canonical boot-owned SharedKernel; written exactly once by init_shared_static*.
// After write, only init_shared_static* touches this storage — no &'static mut KernelState
// alias may be live at the same time (enforced by calling site convention, see §Phase L2A).
//
// Accessed only via addr_of_mut!/addr_of! + raw pointer operations (matching the
// BOOTSTRAP_KERNEL_STATE pattern) to avoid the static_mut_refs lint.
static mut BOOTSTRAP_SHARED_KERNEL: core::mem::MaybeUninit<crate::runtime::SharedKernel> =
    core::mem::MaybeUninit::uninit();

// Three-state readiness flag for BOOTSTRAP_SHARED_KERNEL:
//   0 = uninit     — no initialization has started
//   1 = initializing — compare_exchange claimed ownership; write in progress
//   2 = ready      — ptr::write completed; safe to read via shared_static_ref
// Separating "initializing" from "ready" prevents shared_static_ref from returning
// Some before BOOTSTRAP_SHARED_KERNEL has been fully written (Phase L2A READY-state fix).
static BOOTSTRAP_SHARED_KERNEL_READY: core::sync::atomic::AtomicU8 =
    core::sync::atomic::AtomicU8::new(0);
const MAX_BOOT_EXTRA_RESERVED: usize = 8;
static BOOT_EXTRA_RESERVED_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
static BOOT_EXTRA_RESERVED_STARTS: [core::sync::atomic::AtomicU64; MAX_BOOT_EXTRA_RESERVED] =
    [const { core::sync::atomic::AtomicU64::new(0) }; MAX_BOOT_EXTRA_RESERVED];
static BOOT_EXTRA_RESERVED_ENDS: [core::sync::atomic::AtomicU64; MAX_BOOT_EXTRA_RESERVED] =
    [const { core::sync::atomic::AtomicU64::new(0) }; MAX_BOOT_EXTRA_RESERVED];
static BOOT_INITRD_PTR: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static BOOT_INITRD_LEN: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Boot5A1SchedulerProbe {
    pub present_bitmap: u64,
    pub online_bitmap: u64,
    pub wake_only_bitmap: u64,
    pub current_tid: Option<u64>,
    pub runnable_count: usize,
}

impl Bootstrap {
    #[inline(never)]
    fn default_boot_memory_map() -> ([MemoryRegion; MAX_BOOT_MEMORY_REGIONS], usize) {
        let mut staged = [MemoryRegion {
            start: 0,
            len: 0,
            usable: false,
        }; MAX_BOOT_MEMORY_REGIONS];
        if let Some(regions) = crate::arch::boot_entry::take_staged_ram_for_bootstrap(&mut staged) {
            let regions_len = regions.len();
            return (staged, regions_len);
        }
        let mut fallback = [MemoryRegion {
            start: 0,
            len: 0,
            usable: false,
        }; MAX_BOOT_MEMORY_REGIONS];
        fallback[0] = MemoryRegion {
            start: platform_constants::NEXT_ANON_PHYS_BASE,
            len: 512 * 1024 * 1024,
            usable: true,
        };
        (fallback, 1)
    }

    fn default_reserved_ranges() -> [(u64, u64); 10] {
        let mut result = [(0u64, 0u64); 10];
        // slot 0: kernel image range (computed from linker symbols)
        #[cfg(not(feature = "hosted-dev"))]
        {
            let page = crate::kernel::vm::PAGE_SIZE as u64;
            // The linker places the kernel image at a high VA (see
            // KERNEL_LINK_VIRT_BASE). Subtract the link base to recover
            // the physical extent the frame allocator needs to mark
            // reserved. On targets that link the kernel at PA = VMA
            // (aarch64, riscv64) KERNEL_LINK_VIRT_BASE is 0 and this
            // becomes a no-op subtraction.
            let link_base = platform_constants::KERNEL_LINK_VIRT_BASE;
            let kernel_start_virt = core::ptr::addr_of!(__kernel_start) as u64;
            let kernel_end_virt = core::ptr::addr_of!(__kernel_end) as u64;
            let kernel_start_phys = kernel_start_virt.saturating_sub(link_base);
            let kernel_end_phys = kernel_end_virt.saturating_sub(link_base);
            let kernel_start = kernel_start_phys & !(page - 1);
            let kernel_end = (kernel_end_phys + (page - 1)) & !(page - 1);
            result[0] = (kernel_start, kernel_end);
        }
        #[cfg(feature = "hosted-dev")]
        {
            let kernel_start = platform_constants::KERNEL_BOOTSTRAP_PHYS_BASE;
            let kernel_end = platform_constants::KERNEL_BOOTSTRAP_PHYS_BASE
                + crate::kernel::vm::PAGE_SIZE as u64;
            result[0] = (kernel_start, kernel_end);
        }
        // slots 1..=8: extra boot-reserved ranges (initrd, DTB, page tables, ...)
        let count = BOOT_EXTRA_RESERVED_COUNT
            .load(core::sync::atomic::Ordering::Acquire)
            .min(MAX_BOOT_EXTRA_RESERVED);
        for i in 0..count {
            let s = BOOT_EXTRA_RESERVED_STARTS[i].load(core::sync::atomic::Ordering::Acquire);
            let e = BOOT_EXTRA_RESERVED_ENDS[i].load(core::sync::atomic::Ordering::Acquire);
            result[1 + i] = (s, e);
        }
        result
    }

    pub fn default_reserved_ranges_for_arch_boot() -> [(u64, u64); 10] {
        Self::default_reserved_ranges()
    }

    pub fn install_boot_reserved_range(start: u64, end: u64) {
        if end <= start {
            return;
        }
        let idx = BOOT_EXTRA_RESERVED_COUNT.load(core::sync::atomic::Ordering::Acquire);
        if idx < MAX_BOOT_EXTRA_RESERVED {
            BOOT_EXTRA_RESERVED_STARTS[idx].store(start, core::sync::atomic::Ordering::Release);
            BOOT_EXTRA_RESERVED_ENDS[idx].store(end, core::sync::atomic::Ordering::Release);
            BOOT_EXTRA_RESERVED_COUNT.store(idx + 1, core::sync::atomic::Ordering::Release);
        }
    }

    pub fn install_boot_extra_reserved_ranges(ranges: &[(u64, u64)]) {
        for &(start, end) in ranges {
            Self::install_boot_reserved_range(start, end);
        }
    }

    pub fn install_boot_initrd_bytes(bytes: &'static [u8]) {
        BOOT_INITRD_LEN.store(bytes.len(), core::sync::atomic::Ordering::Release);
        BOOT_INITRD_PTR.store(
            bytes.as_ptr() as usize,
            core::sync::atomic::Ordering::Release,
        );
    }

    pub fn boot_initrd_bytes() -> Option<&'static [u8]> {
        let ptr = BOOT_INITRD_PTR.load(core::sync::atomic::Ordering::Acquire);
        let len = BOOT_INITRD_LEN.load(core::sync::atomic::Ordering::Acquire);
        if ptr == 0 || len == 0 {
            return None;
        }
        Some(unsafe { core::slice::from_raw_parts(ptr as *const u8, len) })
    }

    fn push_region(
        out: &mut [MemoryRegion; MAX_BOOT_MEMORY_REGIONS],
        out_len: &mut usize,
        start: u64,
        end: u64,
    ) {
        if end <= start || *out_len >= MAX_BOOT_MEMORY_REGIONS {
            return;
        }
        out[*out_len] = MemoryRegion {
            start,
            len: end - start,
            usable: true,
        };
        *out_len += 1;
    }

    #[inline(never)]
    pub(crate) fn apply_reserved_ranges(
        regions: &[MemoryRegion],
        reserved: &[(u64, u64)],
    ) -> ([MemoryRegion; MAX_BOOT_MEMORY_REGIONS], usize) {
        let mut out = [MemoryRegion {
            start: 0,
            len: 0,
            usable: false,
        }; MAX_BOOT_MEMORY_REGIONS];
        let mut out_len = 0usize;

        for region in regions {
            if !region.usable || region.len == 0 {
                continue;
            }
            let mut segment_list = [MemoryRegion {
                start: 0,
                len: 0,
                usable: false,
            }; MAX_BOOT_MEMORY_REGIONS];
            let mut seg_len = 1usize;
            segment_list[0] = *region;

            for &(res_start, res_end) in reserved {
                if res_end <= res_start {
                    continue;
                }
                let mut next = [MemoryRegion {
                    start: 0,
                    len: 0,
                    usable: false,
                }; MAX_BOOT_MEMORY_REGIONS];
                let mut next_len = 0usize;

                for seg in segment_list.iter().take(seg_len).copied() {
                    if seg.len == 0 {
                        continue;
                    }
                    let seg_start = seg.start;
                    let seg_end = seg.start.saturating_add(seg.len);

                    if res_end <= seg_start || res_start >= seg_end {
                        if next_len < MAX_BOOT_MEMORY_REGIONS {
                            next[next_len] = seg;
                            next_len += 1;
                        }
                        continue;
                    }

                    if res_start > seg_start && next_len < MAX_BOOT_MEMORY_REGIONS {
                        next[next_len] = MemoryRegion {
                            start: seg_start,
                            len: res_start - seg_start,
                            usable: true,
                        };
                        next_len += 1;
                    }
                    if res_end < seg_end && next_len < MAX_BOOT_MEMORY_REGIONS {
                        next[next_len] = MemoryRegion {
                            start: res_end,
                            len: seg_end - res_end,
                            usable: true,
                        };
                        next_len += 1;
                    }
                }

                segment_list = next;
                seg_len = next_len;
                if seg_len == 0 {
                    break;
                }
            }

            for seg in segment_list.iter().take(seg_len).copied() {
                let seg_start = seg.start;
                let seg_end = seg.start.saturating_add(seg.len);
                Self::push_region(&mut out, &mut out_len, seg_start, seg_end);
            }
        }

        (out, out_len)
    }

    /// Number of physical pages reserved exclusively for page-table frames.
    /// These pages form the PT_FRAME_ALLOCATOR's pool; the rest of sanitized
    /// memory goes to the main KernelState frame allocator.  The two pools
    /// are strictly disjoint — no physical page can appear in both.
    const PT_POOL_PAGES: usize = 256; // 1 MiB — enough for all page-table nodes

    /// Split `regions` (already sanitized, reserved ranges removed) into two
    /// disjoint sub-pools sorted by physical address:
    ///   - first PT_POOL_PAGES pages  → returned as (pt_out, pt_len)
    ///   - everything remaining       → returned as (main_out, main_len)
    fn split_sanitized_for_pt_pool(
        regions: &[MemoryRegion],
        pt_pages: usize,
    ) -> (
        [MemoryRegion; MAX_BOOT_MEMORY_REGIONS],
        usize,
        [MemoryRegion; MAX_BOOT_MEMORY_REGIONS],
        usize,
    ) {
        const PAGE: u64 = crate::kernel::vm::PAGE_SIZE as u64;

        // Normalise to page-aligned extents and collect.
        let mut sorted = [(0u64, 0u64); MAX_BOOT_MEMORY_REGIONS];
        let mut nsorted = 0usize;
        for region in regions {
            if !region.usable || region.len == 0 {
                continue;
            }
            let start = region.start.wrapping_add(PAGE - 1) & !(PAGE - 1);
            let end = region.start.saturating_add(region.len) & !(PAGE - 1);
            if end > start && nsorted < MAX_BOOT_MEMORY_REGIONS {
                sorted[nsorted] = (start, end - start);
                nsorted += 1;
            }
        }
        // Insertion sort by start address (N is small, no heap needed).
        for i in 1..nsorted {
            let key = sorted[i];
            let mut j = i;
            while j > 0 && sorted[j - 1].0 > key.0 {
                sorted[j] = sorted[j - 1];
                j -= 1;
            }
            sorted[j] = key;
        }

        let mut pt_out = [MemoryRegion {
            start: 0,
            len: 0,
            usable: false,
        }; MAX_BOOT_MEMORY_REGIONS];
        let mut main_out = [MemoryRegion {
            start: 0,
            len: 0,
            usable: false,
        }; MAX_BOOT_MEMORY_REGIONS];
        let mut pt_len = 0usize;
        let mut main_len = 0usize;
        let mut remaining_pt = (pt_pages as u64) * PAGE;

        for i in 0..nsorted {
            let (start, len) = sorted[i];
            if remaining_pt == 0 {
                if main_len < MAX_BOOT_MEMORY_REGIONS {
                    main_out[main_len] = MemoryRegion {
                        start,
                        len,
                        usable: true,
                    };
                    main_len += 1;
                }
            } else if len <= remaining_pt {
                if pt_len < MAX_BOOT_MEMORY_REGIONS {
                    pt_out[pt_len] = MemoryRegion {
                        start,
                        len,
                        usable: true,
                    };
                    pt_len += 1;
                }
                remaining_pt -= len;
            } else {
                // Split: first `remaining_pt` bytes → PT pool, remainder → main.
                if pt_len < MAX_BOOT_MEMORY_REGIONS {
                    pt_out[pt_len] = MemoryRegion {
                        start,
                        len: remaining_pt,
                        usable: true,
                    };
                    pt_len += 1;
                }
                let main_start = start + remaining_pt;
                let main_region_len = len - remaining_pt;
                if main_region_len > 0 && main_len < MAX_BOOT_MEMORY_REGIONS {
                    main_out[main_len] = MemoryRegion {
                        start: main_start,
                        len: main_region_len,
                        usable: true,
                    };
                    main_len += 1;
                }
                remaining_pt = 0;
            }
        }

        (pt_out, pt_len, main_out, main_len)
    }

    /// Verify that no region in `pt_regs` overlaps any region in `main_regs`.
    ///
    /// The split is designed to be strictly disjoint, but this assertion runs at
    /// every boot so any future regression in `split_sanitized_for_pt_pool` is
    /// caught before the allocators are initialised.  An overlap means the same
    /// physical page can be handed out by both allocators simultaneously, aliasing
    /// a page-table node with user data — a fatal memory-safety violation.
    fn assert_pools_disjoint(pt_regs: &[MemoryRegion], main_regs: &[MemoryRegion]) {
        for pt_r in pt_regs {
            if !pt_r.usable || pt_r.len == 0 {
                continue;
            }
            let pt_end = pt_r.start + pt_r.len;
            for main_r in main_regs {
                if !main_r.usable || main_r.len == 0 {
                    continue;
                }
                let main_end = main_r.start + main_r.len;
                if pt_r.start < main_end && pt_end > main_r.start {
                    crate::yarm_log!(
                        "PMEM_ALLOC_POOL_OVERLAP_BUG pt=0x{:x}..0x{:x} main=0x{:x}..0x{:x}",
                        pt_r.start,
                        pt_end,
                        main_r.start,
                        main_end
                    );
                    panic!(
                        "PMEM_ALLOC_POOL_OVERLAP_BUG: PT pool and main pool overlap — same physical page in both allocators"
                    );
                }
            }
        }
    }

    pub const fn default_capacity_profile() -> KernelCapacityProfile {
        KernelCapacityProfile::HostedDefault
    }

    pub fn static_kernel_state_storage_addr() -> usize {
        core::ptr::addr_of!(BOOTSTRAP_KERNEL_STATE) as *const _ as usize
    }

    pub fn static_kernel_state_storage_size() -> usize {
        core::mem::size_of::<KernelState>()
    }

    pub fn boot5a1_validate_single_cpu_scheduler(
        cpu: crate::kernel::scheduler::CpuId,
    ) -> Result<Boot5A1SchedulerProbe, KernelError> {
        let cpu_bit = 1u64
            .checked_shl(cpu.0 as u32)
            .ok_or(KernelError::TaskMissing)?;
        crate::arch::boot_entry::stage_present_cpu_bitmap_for_bootstrap(cpu_bit)
            .then_some(())
            .ok_or(KernelError::TaskMissing)?;

        let mut scheduler = SmpScheduler::default();
        scheduler.set_present_cpu_bitmap(cpu_bit);
        scheduler
            .validate_online_cpu(cpu)
            .map_err(map_scheduler_error)?;
        scheduler
            .enqueue_on(cpu, crate::kernel::ipc::ThreadId(0))
            .map_err(map_scheduler_error)?;
        let current = scheduler.dispatch_next_on(cpu).map(|tid| tid.0);
        if current != Some(0) {
            return Err(KernelError::TaskMissing);
        }
        if scheduler.runnable_count_on(cpu) != 0 {
            return Err(KernelError::TaskMissing);
        }
        if scheduler.online_cpu_bitmap() != cpu_bit
            || scheduler.present_cpu_bitmap() != cpu_bit
            || scheduler.wake_only_bitmap() != 0
        {
            return Err(KernelError::TaskMissing);
        }

        Ok(Boot5A1SchedulerProbe {
            present_bitmap: scheduler.present_cpu_bitmap(),
            online_bitmap: scheduler.online_cpu_bitmap(),
            wake_only_bitmap: scheduler.wake_only_bitmap(),
            current_tid: current,
            runnable_count: scheduler.runnable_count_on(cpu),
        })
    }

    pub fn init() -> Result<KernelState, KernelError> {
        Self::init_with_capacity_profile(Self::default_capacity_profile())
    }

    pub fn init_boxed() -> Result<alloc::boxed::Box<KernelState>, KernelError> {
        Self::init_boxed_with_capacity_profile(Self::default_capacity_profile())
    }

    pub fn init_static() -> Result<&'static mut KernelState, KernelError> {
        Self::init_static_with_capacity_profile(Self::default_capacity_profile())
    }

    pub fn init_static_with_capacity_profile(
        capacity_profile: KernelCapacityProfile,
    ) -> Result<&'static mut KernelState, KernelError> {
        let (boot_map, boot_map_len) = Self::default_boot_memory_map();
        let reserved = Self::default_reserved_ranges();
        Self::init_static_with_boot_memory_map(
            capacity_profile,
            &boot_map[..boot_map_len],
            &reserved,
        )
    }

    #[inline(never)]
    pub fn init_with_capacity_profile(
        capacity_profile: KernelCapacityProfile,
    ) -> Result<KernelState, KernelError> {
        let state = Self::init_boxed_with_capacity_profile(capacity_profile)?;
        Ok(*state)
    }

    #[inline(never)]
    pub fn init_boxed_with_capacity_profile(
        capacity_profile: KernelCapacityProfile,
    ) -> Result<alloc::boxed::Box<KernelState>, KernelError> {
        let (boot_map, boot_map_len) = Self::default_boot_memory_map();
        let reserved = Self::default_reserved_ranges();
        let state = Self::init_static_with_boot_memory_map(
            capacity_profile,
            &boot_map[..boot_map_len],
            &reserved,
        )?;
        let mut boxed = alloc::boxed::Box::new(core::mem::MaybeUninit::<KernelState>::uninit());
        unsafe {
            core::ptr::copy_nonoverlapping(state as *mut KernelState, boxed.as_mut_ptr(), 1);
            Ok(boxed.assume_init())
        }
    }

    #[inline(never)]
    pub fn init_static_with_boot_memory_map(
        capacity_profile: KernelCapacityProfile,
        boot_regions: &[MemoryRegion],
        reserved_ranges: &[(u64, u64)],
    ) -> Result<&'static mut KernelState, KernelError> {
        crate::arch::boot_entry::bootstrap_step("enter");
        // Register all reserved ranges in the global frame allocator guard and emit
        // PMEM_RESERVE_* diagnostics so boot logs show the full reserved map.
        crate::arch::boot_entry::bootstrap_step("reserved_ranges");
        for &(start, end) in reserved_ranges {
            if end <= start {
                continue;
            }
            crate::kernel::frame_allocator::register_reserved_range(start, end)
                .map_err(|_| KernelError::MemoryObjectFull)?;
            crate::yarm_log!("PMEM_RESERVE_RANGE start=0x{:x} end=0x{:x}", start, end);
        }

        let mut frame_allocator = PhysicalFrameAllocator::new_uninit();
        let (sanitized, sanitized_len) = Self::apply_reserved_ranges(boot_regions, reserved_ranges);
        let sanitized = &sanitized[..sanitized_len];

        // Split sanitized into two strictly disjoint pools (sorted by PA):
        //   [0 .. PT_POOL_PAGES)  → PT_FRAME_ALLOCATOR (page-table nodes)
        //   [PT_POOL_PAGES .. end) → KernelState frame_allocator (user data/stacks)
        // This prevents the previous bug where both allocators were seeded from
        // the identical sanitized range and handed out the same physical page.
        let (pt_regs, pt_regs_len, main_regs, main_regs_len) =
            Self::split_sanitized_for_pt_pool(sanitized, Self::PT_POOL_PAGES);
        let pt_slice = &pt_regs[..pt_regs_len];
        let main_slice = &main_regs[..main_regs_len];

        // Register PT pool in PT_POOL_RANGES (NOT GLOBAL_RESERVED_RANGES) so the main
        // allocator can detect cross-contamination without triggering false positives
        // when the PT allocator itself legitimately allocates from its own pool.
        for r in pt_slice {
            if r.usable && r.len > 0 {
                crate::kernel::frame_allocator::register_pt_pool_range(r.start, r.start + r.len)
                    .map_err(|_| KernelError::MemoryObjectFull)?;
                crate::yarm_log!(
                    "PT_POOL_RANGE start=0x{:x} end=0x{:x} pages={}",
                    r.start,
                    r.start + r.len,
                    r.len / crate::kernel::vm::PAGE_SIZE as u64
                );
            }
        }
        for r in main_slice {
            if r.usable && r.len > 0 {
                crate::yarm_log!(
                    "MAIN_POOL_RANGE start=0x{:x} end=0x{:x} pages={}",
                    r.start,
                    r.start + r.len,
                    r.len / crate::kernel::vm::PAGE_SIZE as u64
                );
                crate::yarm_log!(
                    "FRAME_ALLOC_INIT_RANGE start=0x{:x} end=0x{:x} pages={}",
                    r.start,
                    r.start + r.len,
                    r.len / crate::kernel::vm::PAGE_SIZE as u64
                );
            }
        }

        // Boot-time guard: the two pools must be strictly disjoint before the
        // allocators are handed their ranges.  PMEM_ALLOC_POOL_OVERLAP_BUG is
        // emitted and the kernel panics if the invariant is broken.
        Self::assert_pools_disjoint(pt_slice, main_slice);

        crate::arch::boot_entry::bootstrap_step("pt_frame_allocator");
        init_pt_frame_allocator(pt_slice).map_err(|_| KernelError::MemoryObjectFull)?;
        crate::arch::boot_entry::bootstrap_step("frame_allocator");
        frame_allocator
            .init_from_memory_map(main_slice)
            .map_err(|_| KernelError::MemoryObjectFull)?;
        crate::arch::boot_entry::bootstrap_step("page_table_reset_state");
        crate::arch::selected_isa::page_table::reset_state();

        crate::arch::boot_entry::bootstrap_step("kernel_aspace_new");
        let mut kernel_aspace = AddressSpace::new_kernel();
        crate::arch::boot_entry::bootstrap_step("kernel_aspace_map_page");
        kernel_aspace
            .map_page(
                VirtAddr(platform_constants::KERNEL_BOOTSTRAP_VIRT_BASE),
                Mapping {
                    phys: PhysAddr(platform_constants::KERNEL_BOOTSTRAP_PHYS_BASE),
                    flags: PageFlags::KERNEL_RW,
                },
            )
            .map_err(|err| match err {
                VmError::Full => KernelError::VmFull,
                other => KernelError::Vm(other),
            })?;

        crate::arch::boot_entry::bootstrap_step("scheduler_new");
        let mut scheduler = SmpScheduler::default();
        let present_cpu_bitmap =
            crate::arch::boot_entry::take_staged_present_cpu_bitmap_for_bootstrap()
                .unwrap_or_else(topology::default_present_cpu_bitmap);
        scheduler.set_present_cpu_bitmap(present_cpu_bitmap);
        scheduler
            .enqueue_on(
                CpuId(platform_constants::BOOTSTRAP_CPU_ID),
                crate::kernel::ipc::ThreadId(0),
            )
            .map_err(map_scheduler_error)?;

        crate::arch::boot_entry::bootstrap_step("kernel_state_write");
        unsafe {
            let state_ptr = core::ptr::addr_of_mut!(BOOTSTRAP_KERNEL_STATE).cast::<KernelState>();
            core::ptr::addr_of_mut!((*state_ptr).kernel_aspace).write(kernel_aspace);
            core::ptr::addr_of_mut!((*state_ptr).hal)
                .write(crate::arch::hal::SelectedIsaHal::default());
            core::ptr::addr_of_mut!((*state_ptr).user_spaces)
                .write(store_kernel_value(AddressSpaceManager::default()));
            core::ptr::addr_of_mut!((*state_ptr).scheduler_state).write(SpinLockIrq::new(
                SchedulerState {
                    scheduler: store_kernel_value(scheduler),
                    timer: Timer::new(platform_constants::BOOTSTRAP_TIMER_DEADLINE_TICKS),
                    current_cpu: CpuId(platform_constants::BOOTSTRAP_CPU_ID),
                },
            ));
            core::ptr::addr_of_mut!((*state_ptr).ipc_state_lock).write(SpinLockIrq::new(()));
            core::ptr::addr_of_mut!((*state_ptr).driver_state_lock).write(SpinLockIrq::new(()));
            core::ptr::addr_of_mut!((*state_ptr).fault_state_lock).write(SpinLockIrq::new(()));
            core::ptr::addr_of_mut!((*state_ptr).restart_state_lock).write(SpinLockIrq::new(()));
            core::ptr::addr_of_mut!((*state_ptr).capability_state_lock).write(SpinLockIrq::new(()));
            core::ptr::addr_of_mut!((*state_ptr).telemetry_state_lock).write(SpinLockIrq::new(()));
            core::ptr::addr_of_mut!((*state_ptr).boot_config_state_lock)
                .write(SpinLockIrq::new(()));
            core::ptr::addr_of_mut!((*state_ptr).vm_state_lock).write(SpinLockIrq::new(()));
            core::ptr::addr_of_mut!((*state_ptr).task_state_lock).write(SpinLockIrq::new(()));
            core::ptr::addr_of_mut!((*state_ptr).memory_state_lock).write(SpinLockIrq::new(()));
            core::ptr::addr_of_mut!((*state_ptr).ipc).write(store_kernel_value(IpcSubsystem {
                cross_cpu_work: SmpMailbox::default(),
                live_tlb_shootdown: LiveTlbShootdownState {
                    next_sequence: 1,
                    active: None,
                },
                endpoints: [const { None }; MAX_ENDPOINTS],
                endpoint_waiters: [None; MAX_ENDPOINTS],
                endpoint_sender_waiters: [[None; MAX_ENDPOINT_SENDER_WAITERS]; MAX_ENDPOINTS],
                endpoint_generations: [0; MAX_ENDPOINTS],
                notifications: [const { None }; MAX_NOTIFICATIONS],
                notification_waiters: [None; MAX_NOTIFICATIONS],
                notification_generations: [0; MAX_NOTIFICATIONS],
                irq_routes: [None; MAX_IRQ_LINES],
                transfer_envelopes: [const { None }; MAX_TRANSFER_ENVELOPES],
                transfer_envelope_generations: [0; MAX_TRANSFER_ENVELOPES],
                active_transfer_mappings: [const { None }; MAX_TRANSFER_ENVELOPES],
                reply_caps: [const { None }; MAX_REPLY_CAPS],
                reply_cap_generations: [0; MAX_REPLY_CAPS],
                telemetry: IpcPathTelemetry::default(),
            }));
            core::ptr::addr_of_mut!((*state_ptr).capability).write(CapabilitySubsystem {
                cnode_spaces: store_kernel_value([const { None }; MAX_TASKS]),
                process_cnodes: store_kernel_value([const { None }; MAX_TASKS]),
                delegated_capability_links: store_kernel_value(
                    [const { None }; MAX_DELEGATED_CAPABILITY_LINKS],
                ),
            });
            core::ptr::addr_of_mut!((*state_ptr).tid_allocation_policy).write(
                TidAllocationPolicy::new(STATIC_TID_UPPER_BOUND, INITIAL_DYNAMIC_TID),
            );
            core::ptr::addr_of_mut!((*state_ptr).tid_allocation_cursor).write(
                TidAllocationCursor::new(TidAllocationPolicy::new(
                    STATIC_TID_UPPER_BOUND,
                    INITIAL_DYNAMIC_TID,
                )),
            );
            core::ptr::addr_of_mut!((*state_ptr).tcbs)
                .write(store_kernel_value([const { None }; MAX_TASKS]));
            core::ptr::addr_of_mut!((*state_ptr).task_classes)
                .write(store_kernel_value([None; MAX_TASKS]));
            core::ptr::addr_of_mut!((*state_ptr).tls_restore_pending)
                .write(store_kernel_value([None; MAX_TASKS]));
            core::ptr::addr_of_mut!((*state_ptr).robust_futex)
                .write(store_kernel_value([None; MAX_TASKS]));
            core::ptr::addr_of_mut!((*state_ptr).memory).write(store_kernel_value(
                MemorySubsystem {
                    #[cfg(feature = "hosted-dev")]
                    user_memory: store_kernel_value(UserMemoryStore::default()),
                    memory_objects: [None; MAX_MEMORY_OBJECTS],
                    brk_regions: [None; MAX_TASKS],
                    cow_pages: alloc::collections::BTreeMap::new(),
                    #[cfg(test)]
                    cow_page_capacity_limit: None,
                    next_memory_object_id: 1,
                    frame_allocator: store_kernel_value(frame_allocator),
                },
            ));
            core::ptr::addr_of_mut!((*state_ptr).drivers).write(store_kernel_value(
                DriverSubsystem {
                    driver_records: [const { None }; MAX_DRIVERS],
                    next_iova_space_id: 1,
                },
            ));
            core::ptr::addr_of_mut!((*state_ptr).telemetry).write(store_kernel_value(
                TelemetrySubsystem {
                    tlb_shootdown_count: 0,
                    tlb_shootdown_timeout_count: 0,
                    tid_allocation: TidAllocationTelemetry::default(),
                    d3_vm_brk_shrink_split_live_calls: 0,
                    d3_vm_brk_shrink_split_live_pages_unmapped: 0,
                },
            ));
            core::ptr::addr_of_mut!((*state_ptr).boot_config)
                .write(store_kernel_value(BootConfigSubsystem { capacity_profile }));
            core::ptr::addr_of_mut!((*state_ptr).faults).write(store_kernel_value(
                FaultSubsystem {
                    last_fault: None,
                    last_fault_frame: None,
                    fault_handler_endpoint: None,
                    supervisor_endpoint: None,
                    pm_task_exit_endpoint: None,
                    fault_policy: FaultPolicy::KillTask,
                },
            ));
            core::ptr::addr_of_mut!((*state_ptr).restart).write(store_kernel_value(
                RestartSubsystem {
                    next_restart_token: 1,
                },
            ));

            let state = &mut *state_ptr;
            crate::yarm_log!(
                "YARM_TID_POLICY static_max={} dynamic_floor={} next_dynamic_tid={}",
                state.tid_allocation_policy.static_tid_upper_bound(),
                state.tid_allocation_policy.dynamic_tid_floor(),
                state
                    .tid_allocation_cursor
                    .next_dynamic_tid(state.tid_allocation_policy)
            );

            crate::arch::boot_entry::bootstrap_step("register_task_0");
            state.register_task(0)?;
            crate::arch::boot_entry::bootstrap_step("dispatch_next_task");
            state.dispatch_next_task()?;
            crate::arch::boot_entry::bootstrap_step("done");
            Ok(state)
        }
    }

    // ── Phase L2A: canonical boot-owned SharedKernel construction ──────────────
    //
    // These functions create the boot-time &'static SharedKernel without
    // installing any trap state pointer.  Trap migration happens in a later
    // phase.  The ownership contract is:
    //
    //   1. init_static_with_boot_memory_map writes BOOTSTRAP_KERNEL_STATE and
    //      returns a &'static mut KernelState aliasing that storage.
    //   2. We immediately ptr::read the bytes out, consuming the alias.
    //      After the read, the caller must not use the &'static mut ref again.
    //   3. The owned KernelState is moved into SharedKernel::new, which stores
    //      it inside a SpinLock inside BOOTSTRAP_SHARED_KERNEL.
    //   4. BOOTSTRAP_SHARED_KERNEL is written exactly once; READY flag is set.
    //   5. Neither install_trap_kernel_state nor install_trap_shared_kernel is
    //      called here.  Stage 2N remains fallback-active.

    /// Construct the canonical boot-owned `SharedKernel` using the default
    /// capacity profile and boot memory map.
    ///
    /// # Panics (in hosted-dev / test)
    /// Panics if called a second time when `BOOTSTRAP_SHARED_KERNEL_READY` is
    /// already set, matching the existing double-init guard pattern used by the
    /// x86_64 `install_trap_kernel_state`.
    pub fn init_shared_static() -> Result<&'static crate::runtime::SharedKernel, KernelError> {
        Self::init_shared_static_with_capacity_profile(Self::default_capacity_profile())
    }

    pub fn init_shared_static_with_capacity_profile(
        capacity_profile: KernelCapacityProfile,
    ) -> Result<&'static crate::runtime::SharedKernel, KernelError> {
        let (boot_map, boot_map_len) = Self::default_boot_memory_map();
        let reserved = Self::default_reserved_ranges();
        Self::init_shared_static_with_boot_memory_map(
            capacity_profile,
            &boot_map[..boot_map_len],
            &reserved,
        )
    }

    pub fn init_shared_static_with_boot_memory_map(
        capacity_profile: KernelCapacityProfile,
        boot_regions: &[MemoryRegion],
        reserved_ranges: &[(u64, u64)],
    ) -> Result<&'static crate::runtime::SharedKernel, KernelError> {
        use core::sync::atomic::Ordering;

        // Guard against double initialization with the same compare-exchange
        // pattern used by x86_64's install_trap_kernel_state.
        if BOOTSTRAP_SHARED_KERNEL_READY
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            panic!("init_shared_static called more than once");
        }

        // Step 1: initialize KernelState into BOOTSTRAP_KERNEL_STATE.
        let state_ref = Self::init_static_with_boot_memory_map(
            capacity_profile,
            boot_regions,
            reserved_ranges,
        )?;

        // Step 2: move the KernelState bytes out of BOOTSTRAP_KERNEL_STATE.
        // After this ptr::read the &'static mut ref must not be used again;
        // BOOTSTRAP_KERNEL_STATE holds logically-moved-from bytes.
        //
        // SAFETY: init_static_with_boot_memory_map fully initializes the
        // MaybeUninit storage and returns a valid &'static mut KernelState.
        // ptr::read produces an owned copy; we drop the reference immediately.
        let owned: KernelState = unsafe { core::ptr::read(state_ref as *const KernelState) };

        // Step 3: wrap in SharedKernel and write into BOOTSTRAP_SHARED_KERNEL.
        let shared = crate::runtime::SharedKernel::new(owned);
        // SAFETY: single-writer guaranteed by the compare_exchange guard above;
        // READY is still 1 (initializing), so no concurrent reader in
        // shared_static_ref can yet observe the write target (it gates on == 2).
        // We use addr_of_mut! + ptr::write to match the BOOTSTRAP_KERNEL_STATE
        // pattern and avoid the static_mut_refs lint.
        unsafe {
            let ptr = core::ptr::addr_of_mut!(BOOTSTRAP_SHARED_KERNEL)
                .cast::<crate::runtime::SharedKernel>();
            core::ptr::write(ptr, shared);
        }

        // Publish: transition 1 (initializing) → 2 (ready).  The Release store
        // pairs with the Acquire load in shared_static_ref, ensuring the
        // ptr::write above is visible to any reader that observes READY == 2.
        BOOTSTRAP_SHARED_KERNEL_READY.store(2, Ordering::Release);

        // SAFETY: ptr::write above fully initialized the storage; the addr_of!
        // cast to *const yields a pointer to a now-valid SharedKernel.
        Ok(unsafe {
            &*core::ptr::addr_of!(BOOTSTRAP_SHARED_KERNEL).cast::<crate::runtime::SharedKernel>()
        })
    }

    /// Return the already-initialized boot `SharedKernel`, or `None` if
    /// `init_shared_static*` has not been called yet.
    ///
    /// This accessor never initializes or mutates the shared kernel.
    pub fn shared_static_ref() -> Option<&'static crate::runtime::SharedKernel> {
        use core::sync::atomic::Ordering;
        if BOOTSTRAP_SHARED_KERNEL_READY.load(Ordering::Acquire) != 2 {
            return None;
        }
        // SAFETY: READY == 2 means ptr::write(BOOTSTRAP_SHARED_KERNEL) completed
        // and the Release store to READY=2 was observed by this Acquire load;
        // the write is therefore fully visible here.
        Some(unsafe {
            &*core::ptr::addr_of!(BOOTSTRAP_SHARED_KERNEL).cast::<crate::runtime::SharedKernel>()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::frame_allocator::MemoryRegion;

    const PAGE: u64 = 0x1000;

    fn region(start: u64, pages: usize) -> MemoryRegion {
        MemoryRegion {
            start,
            len: pages as u64 * PAGE,
            usable: true,
        }
    }

    // ── split_sanitized_for_pt_pool correctness ──────────────────────────────

    #[test]
    fn split_produces_disjoint_pools() {
        let input = [region(0x1000_0000, 512)];
        let (pt, pt_len, main, main_len) = Bootstrap::split_sanitized_for_pt_pool(&input, 256);
        let pt_s = &pt[..pt_len];
        let main_s = &main[..main_len];
        for pt_r in pt_s.iter().filter(|r| r.usable && r.len > 0) {
            let pt_end = pt_r.start + pt_r.len;
            for main_r in main_s.iter().filter(|r| r.usable && r.len > 0) {
                let main_end = main_r.start + main_r.len;
                assert!(
                    pt_end <= main_r.start || main_end <= pt_r.start,
                    "PT [{:#x}..{:#x}) overlaps main [{:#x}..{:#x})",
                    pt_r.start,
                    pt_end,
                    main_r.start,
                    main_end
                );
            }
        }
    }

    #[test]
    fn split_pt_slice_contains_exactly_pt_pool_pages() {
        let input = [region(0x2000_0000, 512)];
        let pt_pages: usize = 256;
        let (pt, pt_len, _, _) = Bootstrap::split_sanitized_for_pt_pool(&input, pt_pages);
        let total_pt: u64 = pt[..pt_len]
            .iter()
            .filter(|r| r.usable)
            .map(|r| r.len / PAGE)
            .sum();
        assert_eq!(total_pt, pt_pages as u64);
    }

    #[test]
    fn split_conserves_total_pages() {
        let input = [region(0x3000_0000, 128), region(0x3100_0000, 256)];
        let (pt, pt_len, main, main_len) = Bootstrap::split_sanitized_for_pt_pool(&input, 64);
        let total_in: u64 = input.iter().map(|r| r.len / PAGE).sum();
        let total_pt: u64 = pt[..pt_len]
            .iter()
            .filter(|r| r.usable)
            .map(|r| r.len / PAGE)
            .sum();
        let total_main: u64 = main[..main_len]
            .iter()
            .filter(|r| r.usable)
            .map(|r| r.len / PAGE)
            .sum();
        assert_eq!(total_pt + total_main, total_in);
    }

    #[test]
    fn split_pt_capped_when_input_smaller_than_pt_pool() {
        // Fewer input pages than requested PT quota: all pages go to PT, main is empty.
        let input = [region(0x4000_0000, 16)];
        let (pt, pt_len, main, main_len) = Bootstrap::split_sanitized_for_pt_pool(&input, 256);
        let total_pt: u64 = pt[..pt_len]
            .iter()
            .filter(|r| r.usable)
            .map(|r| r.len / PAGE)
            .sum();
        let total_main: u64 = main[..main_len]
            .iter()
            .filter(|r| r.usable)
            .map(|r| r.len / PAGE)
            .sum();
        assert_eq!(total_pt, 16, "all small input should be PT pool");
        assert_eq!(
            total_main, 0,
            "main pool should be empty when input exhausted by PT quota"
        );
    }

    #[test]
    fn split_across_multiple_regions_preserves_disjoint_invariant() {
        // Two physically separate regions totalling more than PT quota.
        let input = [region(0x5000_0000, 64), region(0x6000_0000, 512)];
        let (pt, pt_len, main, main_len) = Bootstrap::split_sanitized_for_pt_pool(&input, 100);
        let pt_s = &pt[..pt_len];
        let main_s = &main[..main_len];
        for pt_r in pt_s.iter().filter(|r| r.usable && r.len > 0) {
            let pt_end = pt_r.start + pt_r.len;
            for main_r in main_s.iter().filter(|r| r.usable && r.len > 0) {
                let main_end = main_r.start + main_r.len;
                assert!(
                    pt_end <= main_r.start || main_end <= pt_r.start,
                    "PT [{:#x}..{:#x}) overlaps main [{:#x}..{:#x}) after multi-region split",
                    pt_r.start,
                    pt_end,
                    main_r.start,
                    main_end
                );
            }
        }
    }

    // ── assert_pools_disjoint ────────────────────────────────────────────────

    #[test]
    fn assert_pools_disjoint_passes_for_split_output() {
        // The output of split_sanitized_for_pt_pool must always pass the
        // disjoint assertion — this is the contract the boot path depends on.
        let input = [region(0x7000_0000, 512)];
        let (pt, pt_len, main, main_len) = Bootstrap::split_sanitized_for_pt_pool(&input, 256);
        Bootstrap::assert_pools_disjoint(&pt[..pt_len], &main[..main_len]);
    }

    #[test]
    #[should_panic(expected = "PMEM_ALLOC_POOL_OVERLAP_BUG")]
    fn assert_pools_disjoint_panics_when_same_region_in_both() {
        let r = region(0x8000_0000, 4);
        Bootstrap::assert_pools_disjoint(&[r], &[r]);
    }

    #[test]
    #[should_panic(expected = "PMEM_ALLOC_POOL_OVERLAP_BUG")]
    fn assert_pools_disjoint_panics_on_partial_overlap() {
        // PT region [0x9000_0000 .. 0x9000_4000), main [0x9000_2000 .. 0x9000_6000)
        let pt_r = MemoryRegion {
            start: 0x9000_0000,
            len: 4 * PAGE,
            usable: true,
        };
        let main_r = MemoryRegion {
            start: 0x9000_2000,
            len: 4 * PAGE,
            usable: true,
        };
        Bootstrap::assert_pools_disjoint(&[pt_r], &[main_r]);
    }

    #[test]
    fn assert_pools_disjoint_passes_when_adjacent_but_not_overlapping() {
        // End of PT == start of main: must NOT be treated as overlap.
        let pt_r = region(0xA000_0000, 4);
        let main_r = MemoryRegion {
            start: 0xA000_0000 + 4 * PAGE,
            len: 4 * PAGE,
            usable: true,
        };
        Bootstrap::assert_pools_disjoint(&[pt_r], &[main_r]);
    }
}
