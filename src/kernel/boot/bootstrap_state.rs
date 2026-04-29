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
static BOOT_RESERVED_START: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static BOOT_RESERVED_END: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

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

    fn default_reserved_ranges() -> [(u64, u64); 2] {
        let extra = (
            BOOT_RESERVED_START.load(core::sync::atomic::Ordering::Acquire),
            BOOT_RESERVED_END.load(core::sync::atomic::Ordering::Acquire),
        );
        #[cfg(not(feature = "hosted-dev"))]
        {
            let page = crate::kernel::vm::PAGE_SIZE as u64;
            let kernel_start = (core::ptr::addr_of!(__kernel_start) as u64) & !(page - 1);
            let kernel_end_raw = core::ptr::addr_of!(__kernel_end) as u64;
            let kernel_end = (kernel_end_raw + (page - 1)) & !(page - 1);
            return [(kernel_start, kernel_end), extra];
        }

        #[cfg(feature = "hosted-dev")]
        {
            let kernel_start = platform_constants::KERNEL_BOOTSTRAP_PHYS_BASE;
            let kernel_end = platform_constants::KERNEL_BOOTSTRAP_PHYS_BASE
                + crate::kernel::vm::PAGE_SIZE as u64;
            [(kernel_start, kernel_end), extra]
        }
    }

    pub fn default_reserved_ranges_for_arch_boot() -> [(u64, u64); 2] {
        Self::default_reserved_ranges()
    }

    pub fn install_boot_reserved_range(start: u64, end: u64) {
        BOOT_RESERVED_START.store(start, core::sync::atomic::Ordering::Release);
        BOOT_RESERVED_END.store(end, core::sync::atomic::Ordering::Release);
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

    pub const fn default_capacity_profile() -> KernelCapacityProfile {
        KernelCapacityProfile::HostedDefault
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
        let mut frame_allocator = PhysicalFrameAllocator::new_uninit();
        let (sanitized, sanitized_len) = Self::apply_reserved_ranges(boot_regions, reserved_ranges);
        let sanitized = &sanitized[..sanitized_len];
        frame_allocator
            .init_from_memory_map(sanitized)
            .map_err(|_| KernelError::MemoryObjectFull)?;
        init_pt_frame_allocator(sanitized).map_err(|_| KernelError::MemoryObjectFull)?;
        crate::arch::selected_isa::page_table::reset_state();

        let mut kernel_aspace = AddressSpace::new_kernel();
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
                    cow_pages: store_kernel_value([None; MAX_COW_PAGES]),
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

            state.register_task(0)?;
            state.dispatch_next_task()?;
            Ok(state)
        }
    }
}
