// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use core::fmt;

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;
const MAX_PATH: usize = 192;
const MAX_DEPTH: usize = 32;
const MAX_DIAGNOSTIC_RANGES: usize = 8;
const MAX_DIAGNOSTIC_PCIE_CONTROLLERS: usize = 8;
const MAX_DIAGNOSTIC_BOOTARGS: usize = 256;
pub const MAX_STAGE1_KERNEL_RESERVED_RANGES: usize = MAX_DIAGNOSTIC_RANGES + 3;
pub const MAX_STAGE1_KERNEL_USABLE_RANGES: usize = 24;
pub const STAGE1_PT_POOL_SIZE: u64 = 256 * 1024;
pub const STAGE1_EARLY_HEAP_SIZE: u64 = 2 * 1024 * 1024;
const STAGE1_ALLOCATION_ALIGNMENT: u64 = 64 * 1024;
const STAGE1_PAGE_SIZE: u64 = 4096;
const STAGE1_MMU_MIN_TABLE_PAGES: u64 = 4;
const STAGE1_TTBR0_VA_LIMIT: u64 = 1 << 39;
const RPI5_FIRMWARE_LOW_RESERVED_END: u64 = 0x80000;
const RPI5_PREFERRED_UART: &[u8] = b"/soc@107c000000/serial@7d001000";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DetectedPlatform {
    QemuVirt,
    Rpi5Bcm2712,
    #[default]
    Unknown,
}

impl DetectedPlatform {
    pub const fn label(self) -> &'static str {
        match self {
            Self::QemuVirt => "qemu-virt",
            Self::Rpi5Bcm2712 => "rpi5-bcm2712",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DtbPath {
    bytes: [u8; MAX_PATH],
    len: usize,
}

impl DtbPath {
    pub const fn empty() -> Self {
        Self {
            bytes: [0; MAX_PATH],
            len: 0,
        }
    }

    fn set(&mut self, value: &[u8]) -> bool {
        if value.len() > self.bytes.len() {
            return false;
        }
        self.bytes.fill(0);
        self.bytes[..value.len()].copy_from_slice(value);
        self.len = value.len();
        true
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).unwrap_or("<non-utf8-dtb-path>")
    }
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Default for DtbPath {
    fn default() -> Self {
        Self::empty()
    }
}
impl fmt::Debug for DtbPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DtbPath").field(&self.as_str()).finish()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerialSelection {
    pub path: DtbPath,
    pub base: u64,
    pub size: u64,
    pub clock_hz: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlatformDtbInfo {
    pub platform: DetectedPlatform,
    pub memory_start: Option<u64>,
    pub memory_len: Option<u64>,
    pub reserved_count: u32,
    pub first_reserved_start: Option<u64>,
    pub first_reserved_len: Option<u64>,
    pub interrupt_controller_path: DtbPath,
    pub interrupt_controller_base: Option<u64>,
    pub initrd_start: Option<u64>,
    pub initrd_end: Option<u64>,
    pub stdout_path: DtbPath,
    pub serial: Option<SerialSelection>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiagnosticRange {
    pub start: u64,
    pub size: u64,
    pub no_map: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiagnosticPsciConduit {
    #[default]
    None,
    Smc,
    Hvc,
}

impl DiagnosticPsciConduit {
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Smc => "smc",
            Self::Hvc => "hvc",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiagnosticPcieController {
    pub path: DtbPath,
    pub base: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stage1KernelRange {
    pub start: u64,
    pub end: u64,
}

impl Stage1KernelRange {
    pub const fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }

    pub const fn is_empty(self) -> bool {
        self.start >= self.end
    }

    pub const fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage1KernelPlanFailure {
    #[default]
    InvalidKernelRange,
    InvalidDtbRange,
    FirmwareRangesTruncated,
    AddressOverflow,
    ReservedCapacity,
    UsableCapacity,
    NoPageTablePool,
    NoEarlyHeap,
}

impl Stage1KernelPlanFailure {
    pub const fn label(self) -> &'static str {
        match self {
            Self::InvalidKernelRange => "invalid_kernel_range",
            Self::InvalidDtbRange => "invalid_dtb_range",
            Self::FirmwareRangesTruncated => "firmware_ranges_truncated",
            Self::AddressOverflow => "address_overflow",
            Self::ReservedCapacity => "reserved_capacity",
            Self::UsableCapacity => "usable_capacity",
            Self::NoPageTablePool => "no_page_table_pool",
            Self::NoEarlyHeap => "no_early_heap",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stage1KernelPlan {
    pub reserved_ranges: [Stage1KernelRange; MAX_STAGE1_KERNEL_RESERVED_RANGES],
    pub reserved_range_count: usize,
    pub zero_reserved_skipped: [usize; MAX_DIAGNOSTIC_RANGES],
    pub zero_reserved_skipped_count: usize,
    pub usable_ranges: [Stage1KernelRange; MAX_STAGE1_KERNEL_USABLE_RANGES],
    pub usable_range_count: usize,
    pub page_table_pool: Stage1KernelRange,
    pub early_heap: Stage1KernelRange,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage1MmuMemoryType {
    #[default]
    Normal,
    DeviceNgnre,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stage1MmuMapping {
    pub range: Stage1KernelRange,
    pub memory_type: Stage1MmuMemoryType,
}

pub const MAX_STAGE1_MMU_MAPPINGS: usize = MAX_DIAGNOSTIC_RANGES + 1;
pub const MAX_STAGE1_ALLOC_RESERVED_RANGES: usize = MAX_STAGE1_KERNEL_RESERVED_RANGES + 2;
pub const MAX_STAGE1_ALLOC_USABLE_RANGES: usize = MAX_STAGE1_KERNEL_USABLE_RANGES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stage1MmuPlan {
    pub mappings: [Stage1MmuMapping; MAX_STAGE1_MMU_MAPPINGS],
    pub mapping_count: usize,
    pub pt_pool: Stage1KernelRange,
}

impl Default for Stage1MmuPlan {
    fn default() -> Self {
        Self {
            mappings: [Stage1MmuMapping::default(); MAX_STAGE1_MMU_MAPPINGS],
            mapping_count: 0,
            pt_pool: Stage1KernelRange::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage1MmuPlanFailure {
    #[default]
    InvalidPageTablePool,
    MappingCapacity,
    AddressOverflow,
    AddressOutsideTtbr0,
    AttributeOverlap,
    KernelNotMapped,
    CurrentStackNotMapped,
    DtbNotMapped,
    PageTablePoolNotMapped,
    EarlyHeapNotMapped,
}

impl Stage1MmuPlanFailure {
    pub const fn label(self) -> &'static str {
        match self {
            Self::InvalidPageTablePool => "invalid_pt_pool",
            Self::MappingCapacity => "mapping_capacity",
            Self::AddressOverflow => "address_overflow",
            Self::AddressOutsideTtbr0 => "address_outside_ttbr0",
            Self::AttributeOverlap => "attribute_overlap",
            Self::KernelNotMapped => "kernel_not_mapped",
            Self::CurrentStackNotMapped => "current_stack_not_mapped",
            Self::DtbNotMapped => "dtb_not_mapped",
            Self::PageTablePoolNotMapped => "pt_pool_not_mapped",
            Self::EarlyHeapNotMapped => "early_heap_not_mapped",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage1AllocatorReservationReason {
    #[default]
    LowFirmware,
    Kernel,
    Dtb,
    FirmwareReserved,
    PageTablePool,
    EarlyHeap,
}

impl Stage1AllocatorReservationReason {
    pub const fn label(self) -> &'static str {
        match self {
            Self::LowFirmware => "low_firmware",
            Self::Kernel => "kernel",
            Self::Dtb => "dtb",
            Self::FirmwareReserved => "firmware_reserved",
            Self::PageTablePool => "page_table_pool",
            Self::EarlyHeap => "early_heap",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stage1AllocatorReservation {
    pub range: Stage1KernelRange,
    pub reason: Stage1AllocatorReservationReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stage1AllocatorPlan {
    pub reserved: [Stage1AllocatorReservation; MAX_STAGE1_ALLOC_RESERVED_RANGES],
    pub reserved_count: usize,
    pub usable: [Stage1KernelRange; MAX_STAGE1_ALLOC_USABLE_RANGES],
    pub usable_count: usize,
    pub total_pages: u64,
}

impl Default for Stage1AllocatorPlan {
    fn default() -> Self {
        Self {
            reserved: [Stage1AllocatorReservation::default(); MAX_STAGE1_ALLOC_RESERVED_RANGES],
            reserved_count: 0,
            usable: [Stage1KernelRange::default(); MAX_STAGE1_ALLOC_USABLE_RANGES],
            usable_count: 0,
            total_pages: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage1AllocatorPlanFailure {
    #[default]
    ReservedCapacity,
    UsableCapacity,
    AddressOverflow,
    NoUsableFrames,
    UsableOverlapsReserved,
}

impl Stage1AllocatorPlanFailure {
    pub const fn label(self) -> &'static str {
        match self {
            Self::ReservedCapacity => "reserved_capacity",
            Self::UsableCapacity => "usable_capacity",
            Self::AddressOverflow => "address_overflow",
            Self::NoUsableFrames => "no_usable_frames",
            Self::UsableOverlapsReserved => "usable_overlaps_reserved",
        }
    }
}

impl Default for Stage1KernelPlan {
    fn default() -> Self {
        Self {
            reserved_ranges: [Stage1KernelRange::default(); MAX_STAGE1_KERNEL_RESERVED_RANGES],
            reserved_range_count: 0,
            zero_reserved_skipped: [0; MAX_DIAGNOSTIC_RANGES],
            zero_reserved_skipped_count: 0,
            usable_ranges: [Stage1KernelRange::default(); MAX_STAGE1_KERNEL_USABLE_RANGES],
            usable_range_count: 0,
            page_table_pool: Stage1KernelRange::default(),
            early_heap: Stage1KernelRange::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformDtbDiagnostics {
    pub memory_ranges: [DiagnosticRange; MAX_DIAGNOSTIC_RANGES],
    pub memory_range_count: usize,
    pub memory_ranges_truncated: bool,
    pub reserved_ranges: [DiagnosticRange; MAX_DIAGNOSTIC_RANGES],
    pub reserved_range_count: usize,
    pub reserved_ranges_truncated: bool,
    pub initrd_start: Option<u64>,
    pub initrd_end: Option<u64>,
    pub bootargs_len: usize,
    pub bootargs_truncated: bool,
    pub interrupt_controller_path: DtbPath,
    pub interrupt_controller_base: Option<u64>,
    pub interrupt_controller_compatible: DtbPath,
    pub l2_interrupt_controller_path: DtbPath,
    pub l2_interrupt_controller_base: Option<u64>,
    pub l2_interrupt_controller_compatible: DtbPath,
    pub gic_path: DtbPath,
    pub gic_compatible: DtbPath,
    pub gic_dist_base: Option<u64>,
    pub gic_redist_base: Option<u64>,
    pub psci_conduit: DiagnosticPsciConduit,
    pub cpu_bitmap: u64,
    pub pcie_controllers: [DiagnosticPcieController; MAX_DIAGNOSTIC_PCIE_CONTROLLERS],
    pub pcie_controller_count: usize,
    pub pcie_controllers_truncated: bool,
    pub rp1_controller_index: Option<usize>,
    pub rp1_node_path: DtbPath,
}

impl Default for PlatformDtbDiagnostics {
    fn default() -> Self {
        Self {
            memory_ranges: [DiagnosticRange::default(); MAX_DIAGNOSTIC_RANGES],
            memory_range_count: 0,
            memory_ranges_truncated: false,
            reserved_ranges: [DiagnosticRange::default(); MAX_DIAGNOSTIC_RANGES],
            reserved_range_count: 0,
            reserved_ranges_truncated: false,
            initrd_start: None,
            initrd_end: None,
            bootargs_len: 0,
            bootargs_truncated: false,
            interrupt_controller_path: DtbPath::empty(),
            interrupt_controller_base: None,
            interrupt_controller_compatible: DtbPath::empty(),
            l2_interrupt_controller_path: DtbPath::empty(),
            l2_interrupt_controller_base: None,
            l2_interrupt_controller_compatible: DtbPath::empty(),
            gic_path: DtbPath::empty(),
            gic_compatible: DtbPath::empty(),
            gic_dist_base: None,
            gic_redist_base: None,
            psci_conduit: DiagnosticPsciConduit::None,
            cpu_bitmap: 0,
            pcie_controllers: [DiagnosticPcieController::default();
                MAX_DIAGNOSTIC_PCIE_CONTROLLERS],
            pcie_controller_count: 0,
            pcie_controllers_truncated: false,
            rp1_controller_index: None,
            rp1_node_path: DtbPath::empty(),
        }
    }
}

impl PlatformDtbInfo {
    pub const fn has_initrd(&self) -> bool {
        matches!((self.initrd_start, self.initrd_end), (Some(start), Some(end)) if end > start)
    }
}

pub fn plan_rpi5_stage1_kernel_memory(
    info: &PlatformDtbDiagnostics,
    kernel: Stage1KernelRange,
    dtb: Stage1KernelRange,
) -> Result<Stage1KernelPlan, Stage1KernelPlanFailure> {
    if kernel.is_empty() {
        return Err(Stage1KernelPlanFailure::InvalidKernelRange);
    }
    if dtb.is_empty() {
        return Err(Stage1KernelPlanFailure::InvalidDtbRange);
    }
    if info.memory_ranges_truncated || info.reserved_ranges_truncated {
        return Err(Stage1KernelPlanFailure::FirmwareRangesTruncated);
    }

    let mut plan = Stage1KernelPlan::default();
    append_stage1_reserved(
        &mut plan,
        Stage1KernelRange::new(0, RPI5_FIRMWARE_LOW_RESERVED_END),
    )?;
    append_stage1_reserved(&mut plan, kernel)?;
    append_stage1_reserved(&mut plan, dtb)?;
    for (index, range) in info.reserved_ranges[..info.reserved_range_count]
        .iter()
        .enumerate()
    {
        if range.size == 0 {
            plan.zero_reserved_skipped[plan.zero_reserved_skipped_count] = index;
            plan.zero_reserved_skipped_count += 1;
            continue;
        }
        append_stage1_reserved(
            &mut plan,
            Stage1KernelRange::new(
                range.start,
                range
                    .start
                    .checked_add(range.size)
                    .ok_or(Stage1KernelPlanFailure::AddressOverflow)?,
            ),
        )?;
    }

    let mut free = [Stage1KernelRange::default(); MAX_STAGE1_KERNEL_USABLE_RANGES];
    let mut free_count = 0usize;
    for range in &info.memory_ranges[..info.memory_range_count] {
        if range.size == 0 {
            continue;
        }
        append_stage1_range(
            &mut free,
            &mut free_count,
            Stage1KernelRange::new(
                range.start,
                range
                    .start
                    .checked_add(range.size)
                    .ok_or(Stage1KernelPlanFailure::AddressOverflow)?,
            ),
        )?;
    }
    for reserved in &plan.reserved_ranges[..plan.reserved_range_count] {
        subtract_stage1_range(&mut free, &mut free_count, *reserved)?;
    }

    plan.page_table_pool = allocate_stage1_range(
        &mut free,
        &mut free_count,
        STAGE1_PT_POOL_SIZE,
        STAGE1_ALLOCATION_ALIGNMENT,
    )
    .ok_or(Stage1KernelPlanFailure::NoPageTablePool)?;
    plan.early_heap = allocate_stage1_range(
        &mut free,
        &mut free_count,
        STAGE1_EARLY_HEAP_SIZE,
        STAGE1_ALLOCATION_ALIGNMENT,
    )
    .ok_or(Stage1KernelPlanFailure::NoEarlyHeap)?;
    plan.usable_ranges[..free_count].copy_from_slice(&free[..free_count]);
    plan.usable_range_count = free_count;
    Ok(plan)
}

pub fn plan_rpi5_stage1_allocator_handoff(
    info: &PlatformDtbDiagnostics,
    kernel_plan: &Stage1KernelPlan,
) -> Result<Stage1AllocatorPlan, Stage1AllocatorPlanFailure> {
    let mut plan = Stage1AllocatorPlan::default();
    for (index, range) in kernel_plan.reserved_ranges[..kernel_plan.reserved_range_count]
        .iter()
        .copied()
        .enumerate()
    {
        let reason = match index {
            0 => Stage1AllocatorReservationReason::LowFirmware,
            1 => Stage1AllocatorReservationReason::Kernel,
            2 => Stage1AllocatorReservationReason::Dtb,
            _ => Stage1AllocatorReservationReason::FirmwareReserved,
        };
        append_stage1_allocator_reserved(&mut plan, range, reason)?;
    }
    append_stage1_allocator_reserved(
        &mut plan,
        kernel_plan.page_table_pool,
        Stage1AllocatorReservationReason::PageTablePool,
    )?;
    append_stage1_allocator_reserved(
        &mut plan,
        kernel_plan.early_heap,
        Stage1AllocatorReservationReason::EarlyHeap,
    )?;

    let mut free = [Stage1KernelRange::default(); MAX_STAGE1_ALLOC_USABLE_RANGES];
    let mut free_count = 0usize;
    for range in &info.memory_ranges[..info.memory_range_count] {
        if range.size == 0 {
            continue;
        }
        append_stage1_allocator_usable(
            &mut free,
            &mut free_count,
            Stage1KernelRange::new(
                range.start,
                range
                    .start
                    .checked_add(range.size)
                    .ok_or(Stage1AllocatorPlanFailure::AddressOverflow)?,
            ),
        )?;
    }
    for reserved in &plan.reserved[..plan.reserved_count] {
        subtract_stage1_allocator_range(&mut free, &mut free_count, reserved.range)?;
    }

    let mut aligned_count = 0usize;
    for range in free[..free_count].iter().copied() {
        let start = align_up_u64_policy(range.start, STAGE1_PAGE_SIZE)
            .ok_or(Stage1AllocatorPlanFailure::AddressOverflow)?;
        let end = align_down_u64_policy(range.end, STAGE1_PAGE_SIZE);
        if end <= start {
            continue;
        }
        plan.usable[aligned_count] = Stage1KernelRange::new(start, end);
        aligned_count += 1;
    }
    plan.usable_count = aligned_count;
    sort_stage1_ranges(&mut plan.usable, plan.usable_count);
    for usable in &plan.usable[..plan.usable_count] {
        if plan.reserved[..plan.reserved_count]
            .iter()
            .any(|reserved| usable.overlaps(reserved.range))
        {
            return Err(Stage1AllocatorPlanFailure::UsableOverlapsReserved);
        }
        plan.total_pages = plan
            .total_pages
            .checked_add((usable.end - usable.start) / STAGE1_PAGE_SIZE)
            .ok_or(Stage1AllocatorPlanFailure::AddressOverflow)?;
    }
    if plan.total_pages == 0 {
        return Err(Stage1AllocatorPlanFailure::NoUsableFrames);
    }
    Ok(plan)
}

fn append_stage1_allocator_reserved(
    plan: &mut Stage1AllocatorPlan,
    range: Stage1KernelRange,
    reason: Stage1AllocatorReservationReason,
) -> Result<(), Stage1AllocatorPlanFailure> {
    if range.is_empty()
        || plan.reserved[..plan.reserved_count]
            .iter()
            .any(|existing| existing.range == range)
    {
        return Ok(());
    }
    if plan.reserved_count == plan.reserved.len() {
        return Err(Stage1AllocatorPlanFailure::ReservedCapacity);
    }
    plan.reserved[plan.reserved_count] = Stage1AllocatorReservation { range, reason };
    plan.reserved_count += 1;
    Ok(())
}

fn append_stage1_allocator_usable<const N: usize>(
    ranges: &mut [Stage1KernelRange; N],
    count: &mut usize,
    range: Stage1KernelRange,
) -> Result<(), Stage1AllocatorPlanFailure> {
    if range.is_empty() {
        return Ok(());
    }
    if *count == N {
        return Err(Stage1AllocatorPlanFailure::UsableCapacity);
    }
    ranges[*count] = range;
    *count += 1;
    Ok(())
}

fn subtract_stage1_allocator_range<const N: usize>(
    ranges: &mut [Stage1KernelRange; N],
    count: &mut usize,
    reserved: Stage1KernelRange,
) -> Result<(), Stage1AllocatorPlanFailure> {
    let mut index = 0usize;
    while index < *count {
        let current = ranges[index];
        if !current.overlaps(reserved) {
            index += 1;
            continue;
        }
        let left = Stage1KernelRange::new(current.start, current.end.min(reserved.start));
        let right = Stage1KernelRange::new(current.start.max(reserved.end), current.end);
        if left.is_empty() {
            ranges[index] = right;
            if right.is_empty() {
                ranges.copy_within(index + 1..*count, index);
                *count -= 1;
            } else {
                index += 1;
            }
        } else {
            ranges[index] = left;
            index += 1;
            if !right.is_empty() {
                append_stage1_allocator_usable(ranges, count, right)?;
            }
        }
    }
    Ok(())
}

fn sort_stage1_ranges<const N: usize>(ranges: &mut [Stage1KernelRange; N], count: usize) {
    for index in 1..count {
        let current = ranges[index];
        let mut position = index;
        while position > 0 && ranges[position - 1].start > current.start {
            ranges[position] = ranges[position - 1];
            position -= 1;
        }
        ranges[position] = current;
    }
}

pub fn plan_rpi5_stage1_identity_map(
    info: &PlatformDtbDiagnostics,
    kernel_plan: &Stage1KernelPlan,
    kernel: Stage1KernelRange,
    current_stack: Stage1KernelRange,
    dtb: Stage1KernelRange,
    uart_base: u64,
) -> Result<Stage1MmuPlan, Stage1MmuPlanFailure> {
    if kernel_plan.page_table_pool.start % STAGE1_PAGE_SIZE != 0
        || kernel_plan.page_table_pool.end % STAGE1_PAGE_SIZE != 0
        || kernel_plan
            .page_table_pool
            .end
            .saturating_sub(kernel_plan.page_table_pool.start)
            < STAGE1_MMU_MIN_TABLE_PAGES * STAGE1_PAGE_SIZE
    {
        return Err(Stage1MmuPlanFailure::InvalidPageTablePool);
    }

    let mut plan = Stage1MmuPlan {
        pt_pool: kernel_plan.page_table_pool,
        ..Stage1MmuPlan::default()
    };
    for range in &info.memory_ranges[..info.memory_range_count] {
        if range.size == 0 {
            continue;
        }
        let start = align_down_u64_policy(range.start, STAGE1_PAGE_SIZE);
        let raw_end = range
            .start
            .checked_add(range.size)
            .ok_or(Stage1MmuPlanFailure::AddressOverflow)?;
        let end = align_up_u64_policy(raw_end, STAGE1_PAGE_SIZE)
            .ok_or(Stage1MmuPlanFailure::AddressOverflow)?;
        append_stage1_mmu_mapping(
            &mut plan,
            Stage1MmuMapping {
                range: Stage1KernelRange::new(start, end),
                memory_type: Stage1MmuMemoryType::Normal,
            },
        )?;
    }
    let uart_page = Stage1KernelRange::new(
        align_down_u64_policy(uart_base, STAGE1_PAGE_SIZE),
        align_down_u64_policy(uart_base, STAGE1_PAGE_SIZE)
            .checked_add(STAGE1_PAGE_SIZE)
            .ok_or(Stage1MmuPlanFailure::AddressOverflow)?,
    );
    append_stage1_mmu_mapping(
        &mut plan,
        Stage1MmuMapping {
            range: uart_page,
            memory_type: Stage1MmuMemoryType::DeviceNgnre,
        },
    )?;
    for base in [info.gic_dist_base, info.gic_redist_base]
        .into_iter()
        .flatten()
    {
        let page_start = align_down_u64_policy(base, STAGE1_PAGE_SIZE);
        append_stage1_mmu_mapping(
            &mut plan,
            Stage1MmuMapping {
                range: Stage1KernelRange::new(
                    page_start,
                    page_start
                        .checked_add(STAGE1_PAGE_SIZE)
                        .ok_or(Stage1MmuPlanFailure::AddressOverflow)?,
                ),
                memory_type: Stage1MmuMemoryType::DeviceNgnre,
            },
        )?;
    }

    for (index, mapping) in plan.mappings[..plan.mapping_count].iter().enumerate() {
        if mapping.range.end > STAGE1_TTBR0_VA_LIMIT {
            return Err(Stage1MmuPlanFailure::AddressOutsideTtbr0);
        }
        for other in &plan.mappings[index + 1..plan.mapping_count] {
            if mapping.memory_type != other.memory_type && mapping.range.overlaps(other.range) {
                return Err(Stage1MmuPlanFailure::AttributeOverlap);
            }
        }
    }
    for (range, failure) in [
        (kernel, Stage1MmuPlanFailure::KernelNotMapped),
        (current_stack, Stage1MmuPlanFailure::CurrentStackNotMapped),
        (dtb, Stage1MmuPlanFailure::DtbNotMapped),
        (
            kernel_plan.page_table_pool,
            Stage1MmuPlanFailure::PageTablePoolNotMapped,
        ),
        (
            kernel_plan.early_heap,
            Stage1MmuPlanFailure::EarlyHeapNotMapped,
        ),
    ] {
        if !stage1_mmu_normal_mapping_contains(&plan, range) {
            return Err(failure);
        }
    }
    Ok(plan)
}

pub const fn rpi5_stage1_timer_delta(begin: u64, end: u64) -> Option<u64> {
    match end.checked_sub(begin) {
        Some(0) | None => None,
        Some(delta) => Some(delta),
    }
}

fn append_stage1_mmu_mapping(
    plan: &mut Stage1MmuPlan,
    mapping: Stage1MmuMapping,
) -> Result<(), Stage1MmuPlanFailure> {
    if plan.mapping_count == plan.mappings.len() {
        return Err(Stage1MmuPlanFailure::MappingCapacity);
    }
    plan.mappings[plan.mapping_count] = mapping;
    plan.mapping_count += 1;
    Ok(())
}

fn stage1_mmu_normal_mapping_contains(plan: &Stage1MmuPlan, range: Stage1KernelRange) -> bool {
    !range.is_empty()
        && plan.mappings[..plan.mapping_count].iter().any(|mapping| {
            mapping.memory_type == Stage1MmuMemoryType::Normal
                && mapping.range.start <= range.start
                && mapping.range.end >= range.end
        })
}

const fn align_down_u64_policy(value: u64, alignment: u64) -> u64 {
    value & !(alignment - 1)
}

const fn align_up_u64_policy(value: u64, alignment: u64) -> Option<u64> {
    match value.checked_add(alignment - 1) {
        Some(added) => Some(added & !(alignment - 1)),
        None => None,
    }
}

fn append_stage1_reserved(
    plan: &mut Stage1KernelPlan,
    range: Stage1KernelRange,
) -> Result<(), Stage1KernelPlanFailure> {
    if plan.reserved_ranges[..plan.reserved_range_count].contains(&range) {
        return Ok(());
    }
    if plan.reserved_range_count == plan.reserved_ranges.len() {
        return Err(Stage1KernelPlanFailure::ReservedCapacity);
    }
    plan.reserved_ranges[plan.reserved_range_count] = range;
    plan.reserved_range_count += 1;
    Ok(())
}

fn append_stage1_range<const N: usize>(
    ranges: &mut [Stage1KernelRange; N],
    count: &mut usize,
    range: Stage1KernelRange,
) -> Result<(), Stage1KernelPlanFailure> {
    if range.is_empty() {
        return Ok(());
    }
    if *count == N {
        return Err(Stage1KernelPlanFailure::UsableCapacity);
    }
    ranges[*count] = range;
    *count += 1;
    Ok(())
}

fn subtract_stage1_range<const N: usize>(
    ranges: &mut [Stage1KernelRange; N],
    count: &mut usize,
    reserved: Stage1KernelRange,
) -> Result<(), Stage1KernelPlanFailure> {
    let mut index = 0usize;
    while index < *count {
        let current = ranges[index];
        if !current.overlaps(reserved) {
            index += 1;
            continue;
        }
        let left = Stage1KernelRange::new(current.start, current.end.min(reserved.start));
        let right = Stage1KernelRange::new(current.start.max(reserved.end), current.end);
        if left.is_empty() {
            ranges[index] = right;
            if right.is_empty() {
                ranges.copy_within(index + 1..*count, index);
                *count -= 1;
            } else {
                index += 1;
            }
        } else {
            ranges[index] = left;
            index += 1;
            if !right.is_empty() {
                append_stage1_range(ranges, count, right)?;
            }
        }
    }
    Ok(())
}

fn allocate_stage1_range<const N: usize>(
    ranges: &mut [Stage1KernelRange; N],
    count: &mut usize,
    size: u64,
    alignment: u64,
) -> Option<Stage1KernelRange> {
    for index in 0..*count {
        let candidate_start = align_up_u64(ranges[index].start, alignment)?;
        let candidate_end = candidate_start.checked_add(size)?;
        if candidate_end > ranges[index].end {
            continue;
        }
        let allocation = Stage1KernelRange::new(candidate_start, candidate_end);
        subtract_stage1_range(ranges, count, allocation).ok()?;
        return Some(allocation);
    }
    None
}

fn align_up_u64(value: u64, alignment: u64) -> Option<u64> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
}

pub fn parse_platform_dtb_diagnostics(bytes: &[u8]) -> Option<PlatformDtbDiagnostics> {
    let blocks = blocks(bytes)?;
    let mut out = PlatformDtbDiagnostics::default();
    let mut walker = Walker::new(blocks);
    let mut reserved_range = None;
    let mut reserved_no_map = false;
    let mut irq_candidate = false;
    let mut irq_is_l2 = false;
    let mut irq_is_gic = false;
    let mut irq_compatible = DtbPath::empty();
    let mut irq_path = DtbPath::empty();
    let mut irq_reg = [DiagnosticRange::default(); 2];
    let mut irq_reg_count = 0usize;
    let mut pcie_controller_index = None;
    let mut pcie_path = DtbPath::empty();
    let mut pcie_base = None;
    while let Some(event) = walker.next() {
        match event {
            Event::Begin { path, name } => {
                if path.starts_with(b"/reserved-memory/") {
                    reserved_range = None;
                    reserved_no_map = false;
                }
                irq_candidate =
                    name.starts_with(b"intc") || name.starts_with(b"interrupt-controller");
                irq_is_l2 = false;
                irq_is_gic = false;
                irq_compatible = DtbPath::empty();
                irq_path.set(path);
                irq_reg_count = 0;
                pcie_controller_index = None;
                pcie_path.set(path);
                pcie_base = None;
                if is_pcie_node_name(name) {
                    pcie_controller_index = ensure_pcie_controller(&mut out, pcie_path);
                }
                if path.starts_with(b"/cpus/")
                    && let Some(cpu) = parse_cpu_id_from_node_name(name)
                {
                    out.cpu_bitmap |= 1u64 << cpu;
                }
                if name == b"rp1" && out.rp1_node_path.is_empty() {
                    if let Some(parent) = direct_parent_path(path)
                        && let Some(index) = find_pcie_controller(&out, parent)
                    {
                        out.rp1_controller_index = Some(index);
                        out.rp1_node_path.set(path);
                    }
                }
            }
            Event::Property {
                path,
                name,
                value,
                parent_address_cells,
                parent_size_cells,
            } => {
                if is_memory_path(path) && name == b"reg" {
                    append_reg_ranges(
                        &mut out.memory_ranges,
                        &mut out.memory_range_count,
                        &mut out.memory_ranges_truncated,
                        value,
                        parent_address_cells,
                        parent_size_cells,
                        false,
                    )?;
                }
                if path.starts_with(b"/reserved-memory/") && name == b"reg" {
                    let (start, size) = first_reg(value, parent_address_cells, parent_size_cells);
                    reserved_range = Some(DiagnosticRange {
                        start: start?,
                        size: size?,
                        no_map: reserved_no_map,
                    });
                }
                if path.starts_with(b"/reserved-memory/") && name == b"no-map" {
                    reserved_no_map = true;
                    if let Some(range) = reserved_range.as_mut() {
                        range.no_map = true;
                    }
                }
                if path == b"/chosen" && name == b"linux,initrd-start" {
                    out.initrd_start = scalar(value);
                }
                if path == b"/chosen" && name == b"linux,initrd-end" {
                    out.initrd_end = scalar(value);
                }
                if path == b"/chosen" && name == b"bootargs" {
                    out.bootargs_len = first_string(value).len();
                    out.bootargs_truncated = out.bootargs_len > MAX_DIAGNOSTIC_BOOTARGS;
                }
                if name == b"interrupt-controller" {
                    irq_candidate = true;
                }
                if irq_candidate && name == b"compatible" {
                    irq_compatible.set(first_string(value));
                    irq_is_l2 = value.split(|byte| *byte == 0).any(is_bcm7271_l2_compatible);
                    irq_is_gic = value.split(|byte| *byte == 0).any(is_arm_gic_compatible);
                }
                if irq_candidate && name == b"reg" {
                    let mut truncated = false;
                    append_reg_ranges(
                        &mut irq_reg,
                        &mut irq_reg_count,
                        &mut truncated,
                        value,
                        parent_address_cells,
                        parent_size_cells,
                        false,
                    )?;
                    irq_path.set(path);
                }
                if path.starts_with(b"/psci") && name == b"method" {
                    out.psci_conduit = match first_string(value) {
                        b"hvc" => DiagnosticPsciConduit::Hvc,
                        b"smc" => DiagnosticPsciConduit::Smc,
                        _ => DiagnosticPsciConduit::None,
                    };
                }
                if name == b"device_type" && first_string(value) == b"pci" {
                    pcie_path.set(path);
                    if !node_name(path).is_some_and(is_excluded_pcie_node_name) {
                        pcie_controller_index = ensure_pcie_controller(&mut out, pcie_path);
                    }
                }
                if name == b"compatible"
                    && value.split(|byte| *byte == 0).any(is_known_pcie_compatible)
                    && !node_name(path).is_some_and(is_excluded_pcie_node_name)
                {
                    pcie_path.set(path);
                    pcie_controller_index = ensure_pcie_controller(&mut out, pcie_path);
                }
                if name == b"reg" {
                    let (base, _) = first_reg(value, parent_address_cells, parent_size_cells);
                    pcie_base = base;
                }
                if let (Some(index), Some(base)) = (pcie_controller_index, pcie_base) {
                    out.pcie_controllers[index].base =
                        translate_to_root(blocks, pcie_path.as_bytes(), base);
                }
            }
            Event::EndNode => {
                if let Some(range) = reserved_range.take() {
                    append_diagnostic_range(
                        &mut out.reserved_ranges,
                        &mut out.reserved_range_count,
                        &mut out.reserved_ranges_truncated,
                        range,
                    );
                }
                if irq_candidate && irq_reg_count != 0 {
                    let path = irq_path.as_bytes();
                    let first_base = translate_to_root(blocks, path, irq_reg[0].start);
                    if out.interrupt_controller_path.is_empty() {
                        out.interrupt_controller_path = irq_path;
                        out.interrupt_controller_base = first_base;
                        out.interrupt_controller_compatible = irq_compatible;
                    }
                    if irq_is_l2 && out.l2_interrupt_controller_path.is_empty() {
                        out.l2_interrupt_controller_path = irq_path;
                        out.l2_interrupt_controller_base = first_base;
                        out.l2_interrupt_controller_compatible = irq_compatible;
                    } else if irq_is_gic && out.gic_path.is_empty() {
                        out.gic_path = irq_path;
                        out.gic_compatible = irq_compatible;
                        out.gic_dist_base = first_base;
                        if irq_reg_count > 1 {
                            out.gic_redist_base = translate_to_root(blocks, path, irq_reg[1].start);
                        }
                    }
                }
                irq_candidate = false;
                pcie_controller_index = None;
            }
            Event::End => break,
        }
    }
    Some(out)
}

#[derive(Clone, Copy)]
struct Blocks<'a> {
    structure: &'a [u8],
    strings: &'a [u8],
}

#[derive(Clone, Copy)]
struct Walker<'a> {
    blocks: Blocks<'a>,
    cursor: usize,
    path: [u8; MAX_PATH],
    path_len: usize,
    parent_lens: [usize; MAX_DEPTH],
    depth: usize,
    address_cells: [u32; MAX_DEPTH],
    size_cells: [u32; MAX_DEPTH],
}

#[derive(Clone, Copy)]
enum Event<'a> {
    Begin {
        path: &'a [u8],
        name: &'a [u8],
    },
    Property {
        path: &'a [u8],
        name: &'a [u8],
        value: &'a [u8],
        parent_address_cells: u32,
        parent_size_cells: u32,
    },
    EndNode,
    End,
}

impl<'a> Walker<'a> {
    fn new(blocks: Blocks<'a>) -> Self {
        let mut address_cells = [2; MAX_DEPTH];
        let mut size_cells = [1; MAX_DEPTH];
        address_cells[0] = 2;
        size_cells[0] = 1;
        Self {
            blocks,
            cursor: 0,
            path: [0; MAX_PATH],
            path_len: 0,
            parent_lens: [0; MAX_DEPTH],
            depth: 0,
            address_cells,
            size_cells,
        }
    }

    fn next(&mut self) -> Option<Event<'_>> {
        loop {
            let token = be32(self.blocks.structure, self.cursor)?;
            self.cursor += 4;
            match token {
                FDT_BEGIN_NODE => {
                    let (name, next) = cstr(self.blocks.structure, self.cursor)?;
                    self.cursor = align4(next)?;
                    if self.depth >= MAX_DEPTH {
                        return None;
                    }
                    self.parent_lens[self.depth] = self.path_len;
                    if self.depth == 0 {
                        self.path[0] = b'/';
                        self.path_len = 1;
                    } else {
                        if self.path_len > 1 {
                            self.push(b'/')?;
                        }
                        self.extend(name)?;
                    }
                    let parent_depth = self.depth.saturating_sub(1);
                    let parent_address_cells = self.address_cells[parent_depth];
                    let parent_size_cells = self.size_cells[parent_depth];
                    self.address_cells[self.depth] = parent_address_cells;
                    self.size_cells[self.depth] = parent_size_cells;
                    self.depth += 1;
                    return Some(Event::Begin {
                        path: &self.path[..self.path_len],
                        name,
                    });
                }
                FDT_END_NODE => {
                    if self.depth == 0 {
                        return None;
                    }
                    self.depth -= 1;
                    self.path_len = self.parent_lens[self.depth];
                    return Some(Event::EndNode);
                }
                FDT_PROP => {
                    let len = be32(self.blocks.structure, self.cursor)? as usize;
                    let name_off = be32(self.blocks.structure, self.cursor + 4)? as usize;
                    self.cursor += 8;
                    let end = self.cursor.checked_add(len)?;
                    let value = self.blocks.structure.get(self.cursor..end)?;
                    self.cursor = align4(end)?;
                    let name = cstr(self.blocks.strings, name_off)?.0;
                    let node_depth = self.depth.checked_sub(1)?;
                    if name == b"#address-cells" {
                        self.address_cells[node_depth] = be32(value, 0)?;
                    }
                    if name == b"#size-cells" {
                        self.size_cells[node_depth] = be32(value, 0)?;
                    }
                    let parent_depth = node_depth.saturating_sub(1);
                    return Some(Event::Property {
                        path: &self.path[..self.path_len],
                        name,
                        value,
                        parent_address_cells: self.address_cells[parent_depth],
                        parent_size_cells: self.size_cells[parent_depth],
                    });
                }
                FDT_NOP => continue,
                FDT_END => return Some(Event::End),
                _ => return None,
            }
        }
    }

    fn push(&mut self, byte: u8) -> Option<()> {
        if self.path_len == MAX_PATH {
            return None;
        }
        self.path[self.path_len] = byte;
        self.path_len += 1;
        Some(())
    }
    fn extend(&mut self, bytes: &[u8]) -> Option<()> {
        if self.path_len.checked_add(bytes.len())? > MAX_PATH {
            return None;
        }
        self.path[self.path_len..self.path_len + bytes.len()].copy_from_slice(bytes);
        self.path_len += bytes.len();
        Some(())
    }
}

pub fn parse_platform_dtb(bytes: &[u8]) -> Option<PlatformDtbInfo> {
    let blocks = blocks(bytes)?;
    let mut info = PlatformDtbInfo::default();
    let mut stdout_ref = DtbPath::empty();
    let mut stdout_alias = DtbPath::empty();
    let mut root_address_cells = 2;
    let mut root_size_cells = 1;
    let mut walker = Walker::new(blocks);
    while let Some(event) = walker.next() {
        match event {
            Event::Property {
                path,
                name,
                value,
                parent_address_cells,
                parent_size_cells,
            } => {
                if path == b"/" && name == b"compatible" {
                    info.platform = classify_compatible(value);
                }
                if path == b"/" && name == b"#address-cells" {
                    root_address_cells = be32(value, 0)?;
                }
                if path == b"/" && name == b"#size-cells" {
                    root_size_cells = be32(value, 0)?;
                }
                if path == b"/chosen"
                    && (name == b"stdout-path"
                        || (name == b"linux,stdout-path" && stdout_ref.is_empty()))
                {
                    let raw = first_string(value);
                    let reference = raw.split(|byte| *byte == b':').next().unwrap_or(&[]);
                    stdout_ref.set(reference);
                }
                if path == b"/chosen" && name == b"linux,initrd-start" {
                    info.initrd_start = scalar(value);
                }
                if path == b"/chosen" && name == b"linux,initrd-end" {
                    info.initrd_end = scalar(value);
                }
                if is_memory_path(path) && name == b"reg" && info.memory_start.is_none() {
                    (info.memory_start, info.memory_len) =
                        first_reg(value, parent_address_cells, parent_size_cells);
                }
                if path.starts_with(b"/reserved-memory/") && name == b"reg" {
                    if let (Some(start), Some(len)) =
                        first_reg(value, parent_address_cells, parent_size_cells)
                    {
                        info.reserved_count = info.reserved_count.saturating_add(1);
                        if info.first_reserved_start.is_none() {
                            info.first_reserved_start = Some(start);
                            info.first_reserved_len = Some(len);
                        }
                    }
                }
            }
            Event::End => break,
            _ => {}
        }
    }
    if stdout_ref.as_bytes().starts_with(b"/") {
        info.stdout_path = stdout_ref;
    } else if !stdout_ref.is_empty() {
        let mut aliases = Walker::new(blocks);
        while let Some(event) = aliases.next() {
            if let Event::Property {
                path, name, value, ..
            } = event
            {
                if path == b"/aliases" && name == stdout_ref.as_bytes() {
                    stdout_alias.set(first_string(value));
                    break;
                }
            }
        }
        info.stdout_path = if stdout_alias.is_empty() {
            stdout_ref
        } else {
            stdout_alias
        };
    }

    let preferred = find_serial(blocks, RPI5_PREFERRED_UART);
    let resolved = if info.stdout_path.is_empty() {
        None
    } else {
        find_serial(blocks, info.stdout_path.as_bytes())
    };
    let first = find_first_pl011(blocks);
    info.serial = if info.platform == DetectedPlatform::Rpi5Bcm2712 {
        preferred.or(resolved).or(first)
    } else {
        resolved.or(first)
    };
    if let Some(mut serial) = info.serial {
        info.serial = translate_to_root(blocks, serial.path.as_bytes(), serial.base).map(|base| {
            serial.base = base;
            serial
        });
    }

    let mut gic = Walker::new(blocks);
    let mut candidate = DtbPath::empty();
    let mut candidate_base = None;
    let mut node_is_interrupt = false;
    let mut current_path = DtbPath::empty();
    while let Some(event) = gic.next() {
        match event {
            Event::Begin { path, name, .. } => {
                current_path.set(path);
                node_is_interrupt =
                    name.starts_with(b"intc") || name.starts_with(b"interrupt-controller");
            }
            Event::Property {
                path,
                name,
                value,
                parent_address_cells,
                parent_size_cells,
            } => {
                if name == b"interrupt-controller" {
                    node_is_interrupt = true;
                }
                if node_is_interrupt && name == b"reg" && candidate_base.is_none() {
                    let (base, _) = first_reg(value, parent_address_cells, parent_size_cells);
                    candidate.set(path);
                    candidate_base = base;
                }
            }
            Event::EndNode => {
                node_is_interrupt = false;
                current_path = DtbPath::empty();
            }
            Event::End => break,
        }
    }
    if let Some(base) = candidate_base {
        info.interrupt_controller_base =
            translate_to_root(blocks, candidate.as_bytes(), base).or(Some(base));
        info.interrupt_controller_path = candidate;
    }
    let _ = (root_address_cells, root_size_cells, current_path);
    Some(info)
}

fn find_serial(blocks: Blocks<'_>, wanted: &[u8]) -> Option<SerialSelection> {
    find_serial_matching(blocks, Some(wanted))
}
fn find_first_pl011(blocks: Blocks<'_>) -> Option<SerialSelection> {
    find_serial_matching(blocks, None)
}

fn find_serial_matching(blocks: Blocks<'_>, wanted: Option<&[u8]>) -> Option<SerialSelection> {
    let mut walker = Walker::new(blocks);
    let mut current = DtbPath::empty();
    let mut match_node = false;
    let mut usable = true;
    let mut pl011 = false;
    let mut base = None;
    let mut size = 0;
    let mut clock = None;
    while let Some(event) = walker.next() {
        match event {
            Event::Begin { path, name, .. } => {
                current.set(path);
                match_node = wanted.map_or(
                    name.starts_with(b"serial@") || name.starts_with(b"uart@"),
                    |value| value == path,
                );
                usable = true;
                pl011 = false;
                base = None;
                size = 0;
                clock = None;
            }
            Event::Property {
                path,
                name,
                value,
                parent_address_cells,
                parent_size_cells,
            } if match_node && path == current.as_bytes() => {
                if name == b"compatible" {
                    pl011 = string_list_contains(value, b"arm,pl011")
                        || string_list_contains(value, b"arm,primecell");
                }
                if name == b"status" {
                    let status = first_string(value);
                    usable = status.is_empty() || status == b"okay" || status == b"ok";
                }
                if name == b"reg" {
                    let (reg_base, reg_size) =
                        first_reg(value, parent_address_cells, parent_size_cells);
                    base = reg_base;
                    size = reg_size.unwrap_or(0);
                }
                if name == b"clock-frequency" {
                    clock = be32(value, 0);
                }
            }
            Event::EndNode => {
                if match_node
                    && usable
                    && pl011
                    && let Some(base) = base
                {
                    return Some(SerialSelection {
                        path: current,
                        base,
                        size,
                        clock_hz: clock,
                    });
                }
                match_node = false;
            }
            Event::End => break,
            _ => {}
        }
    }
    None
}

fn translate_to_root(blocks: Blocks<'_>, node_path: &[u8], mut address: u64) -> Option<u64> {
    let mut path = DtbPath::empty();
    path.set(node_path);
    for _ in 0..MAX_DEPTH {
        let parent_len = path
            .as_bytes()
            .iter()
            .rposition(|b| *b == b'/')
            .unwrap_or(0);
        if parent_len == 0 {
            return Some(address);
        }
        path.len = parent_len;
        address = translate_parent_ranges(blocks, path.as_bytes(), address)?;
    }
    None
}

fn translate_parent_ranges(blocks: Blocks<'_>, wanted: &[u8], address: u64) -> Option<u64> {
    let mut walker = Walker::new(blocks);
    while let Some(event) = walker.next() {
        if let Event::Property {
            path,
            name,
            value,
            parent_address_cells,
            parent_size_cells,
        } = event
        {
            if path == wanted && name == b"ranges" {
                if value.is_empty() {
                    return Some(address);
                }
                let child_cells = node_cell_count(blocks, wanted, b"#address-cells")
                    .unwrap_or(parent_address_cells);
                let size_cells =
                    node_cell_count(blocks, wanted, b"#size-cells").unwrap_or(parent_size_cells);
                if !(1..=2).contains(&child_cells)
                    || !(1..=2).contains(&parent_address_cells)
                    || !(1..=2).contains(&size_cells)
                {
                    return None;
                }
                let entry_cells = child_cells
                    .checked_add(parent_address_cells)?
                    .checked_add(size_cells)? as usize;
                let entry_bytes = entry_cells.checked_mul(4)?;
                if entry_bytes == 0 || value.len() % entry_bytes != 0 {
                    return None;
                }
                for entry in value.chunks_exact(entry_bytes) {
                    let child = cells(entry, child_cells, 0)?;
                    let parent = cells(entry, parent_address_cells, child_cells as usize)?;
                    let size = cells(
                        entry,
                        size_cells,
                        (child_cells + parent_address_cells) as usize,
                    )?;
                    let end = child.checked_add(size)?;
                    if size != 0 && address >= child && address < end {
                        return parent.checked_add(address - child);
                    }
                }
                return None;
            }
        }
    }
    None
}

fn node_cell_count(blocks: Blocks<'_>, wanted: &[u8], property: &[u8]) -> Option<u32> {
    let mut walker = Walker::new(blocks);
    while let Some(event) = walker.next() {
        if let Event::Property {
            path, name, value, ..
        } = event
        {
            if path == wanted && name == property {
                return be32(value, 0);
            }
        }
    }
    None
}

fn classify_compatible(value: &[u8]) -> DetectedPlatform {
    if string_list_contains(value, b"raspberrypi,5-model-b")
        || string_list_contains(value, b"brcm,bcm2712")
    {
        DetectedPlatform::Rpi5Bcm2712
    } else if string_list_contains(value, b"linux,dummy-virt")
        || string_list_contains(value, b"qemu,virt")
    {
        DetectedPlatform::QemuVirt
    } else {
        DetectedPlatform::Unknown
    }
}

fn blocks(bytes: &[u8]) -> Option<Blocks<'_>> {
    if be32(bytes, 0)? != FDT_MAGIC {
        return None;
    }
    let total = be32(bytes, 4)? as usize;
    let struct_off = be32(bytes, 8)? as usize;
    let strings_off = be32(bytes, 12)? as usize;
    let strings_len = be32(bytes, 32)? as usize;
    let struct_len = be32(bytes, 36)? as usize;
    if total > bytes.len() {
        return None;
    }
    Some(Blocks {
        structure: bytes.get(struct_off..struct_off.checked_add(struct_len)?)?,
        strings: bytes.get(strings_off..strings_off.checked_add(strings_len)?)?,
    })
}
fn is_memory_path(path: &[u8]) -> bool {
    path.strip_prefix(b"/")
        .is_some_and(|rest| rest.starts_with(b"memory@") || rest == b"memory")
}
fn parse_cpu_id_from_node_name(name: &[u8]) -> Option<u8> {
    let suffix = name.strip_prefix(b"cpu@")?;
    let mut value = 0u64;
    let mut consumed = 0usize;
    for byte in suffix.iter().copied() {
        let digit = match byte {
            b'0'..=b'9' => (byte - b'0') as u64,
            b'a'..=b'f' => (byte - b'a' + 10) as u64,
            b'A'..=b'F' => (byte - b'A' + 10) as u64,
            _ => break,
        };
        value = value.checked_mul(16)?.checked_add(digit)?;
        consumed += 1;
    }
    (consumed != 0 && value < 64).then_some(value as u8)
}
fn is_bcm7271_l2_compatible(value: &[u8]) -> bool {
    value == b"brcm,bcm7271-l2-intc"
}
fn is_arm_gic_compatible(value: &[u8]) -> bool {
    matches!(
        value,
        b"arm,gic-v3" | b"arm,gic-400" | b"arm,cortex-a15-gic"
    )
}
fn is_pcie_node_name(name: &[u8]) -> bool {
    !is_excluded_pcie_node_name(name) && (name.starts_with(b"pcie@") || name.starts_with(b"pci@"))
}
fn is_excluded_pcie_node_name(name: &[u8]) -> bool {
    name.windows(b"reset-controller".len())
        .any(|part| part == b"reset-controller")
}
fn is_known_pcie_compatible(value: &[u8]) -> bool {
    matches!(
        value,
        b"brcm,bcm2712-pcie" | b"brcm,bcm2711-pcie" | b"pci-host-ecam-generic" | b"snps,dw-pcie"
    )
}
fn node_name(path: &[u8]) -> Option<&[u8]> {
    path.rsplit(|byte| *byte == b'/').next()
}
fn direct_parent_path(path: &[u8]) -> Option<&[u8]> {
    let split = path.iter().rposition(|byte| *byte == b'/')?;
    (split != 0).then_some(&path[..split])
}
fn find_pcie_controller(info: &PlatformDtbDiagnostics, path: &[u8]) -> Option<usize> {
    info.pcie_controllers[..info.pcie_controller_count]
        .iter()
        .position(|controller| controller.path.as_bytes() == path)
}
fn ensure_pcie_controller(info: &mut PlatformDtbDiagnostics, path: DtbPath) -> Option<usize> {
    if let Some(index) = find_pcie_controller(info, path.as_bytes()) {
        return Some(index);
    }
    if info.pcie_controller_count == info.pcie_controllers.len() {
        info.pcie_controllers_truncated = true;
        return None;
    }
    let index = info.pcie_controller_count;
    info.pcie_controllers[index].path = path;
    info.pcie_controller_count += 1;
    Some(index)
}
fn append_reg_ranges<const N: usize>(
    ranges: &mut [DiagnosticRange; N],
    count: &mut usize,
    truncated: &mut bool,
    value: &[u8],
    address_cells: u32,
    size_cells: u32,
    no_map: bool,
) -> Option<()> {
    if !(1..=2).contains(&address_cells) || !(1..=2).contains(&size_cells) {
        return None;
    }
    let cells_per_entry = address_cells.checked_add(size_cells)? as usize;
    let bytes_per_entry = cells_per_entry.checked_mul(4)?;
    if bytes_per_entry == 0 || value.len() % bytes_per_entry != 0 {
        return None;
    }
    for entry in value.chunks_exact(bytes_per_entry) {
        append_diagnostic_range(
            ranges,
            count,
            truncated,
            DiagnosticRange {
                start: cells(entry, address_cells, 0)?,
                size: cells(entry, size_cells, address_cells as usize)?,
                no_map,
            },
        );
    }
    Some(())
}
fn append_diagnostic_range<const N: usize>(
    ranges: &mut [DiagnosticRange; N],
    count: &mut usize,
    truncated: &mut bool,
    range: DiagnosticRange,
) {
    if *count < N {
        ranges[*count] = range;
        *count += 1;
    } else {
        *truncated = true;
    }
}
fn first_string(value: &[u8]) -> &[u8] {
    &value[..value.iter().position(|b| *b == 0).unwrap_or(value.len())]
}
fn string_list_contains(value: &[u8], wanted: &[u8]) -> bool {
    value.split(|b| *b == 0).any(|part| part == wanted)
}
fn scalar(value: &[u8]) -> Option<u64> {
    match value.len() {
        4 => Some(be32(value, 0)? as u64),
        8 => Some((be32(value, 0)? as u64) << 32 | be32(value, 4)? as u64),
        _ => None,
    }
}
fn first_reg(value: &[u8], address_cells: u32, size_cells: u32) -> (Option<u64>, Option<u64>) {
    (
        cells(value, address_cells, 0),
        cells(value, size_cells, address_cells as usize),
    )
}
fn cells(value: &[u8], count: u32, offset_cells: usize) -> Option<u64> {
    if count > 2 {
        return None;
    }
    let mut out = 0;
    for i in 0..count as usize {
        out = (out << 32) | be32(value, (offset_cells + i) * 4)? as u64;
    }
    Some(out)
}
fn be32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn cstr(bytes: &[u8], offset: usize) -> Option<(&[u8], usize)> {
    let tail = bytes.get(offset..)?;
    let len = tail.iter().position(|b| *b == 0)?;
    Some((&tail[..len], offset + len + 1))
}
fn align4(value: usize) -> Option<usize> {
    value.checked_add(3).map(|v| v & !3)
}

pub fn rpi5_phase_allows_boot(
    phase: crate::kernel::boot_command_line::BootPhase,
    has_initrd: bool,
) -> bool {
    use crate::kernel::boot_command_line::BootPhase;
    match phase {
        BootPhase::Entry | BootPhase::Uart | BootPhase::Dtb | BootPhase::Mmu => true,
        BootPhase::Kernel => has_initrd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot_command_line::BootPhase;
    use std::collections::BTreeMap;
    use std::vec::Vec;

    fn be(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_be_bytes());
    }
    fn begin(out: &mut Vec<u8>, name: &[u8]) {
        be(out, FDT_BEGIN_NODE);
        out.extend_from_slice(name);
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    fn end(out: &mut Vec<u8>) {
        be(out, FDT_END_NODE);
    }
    fn prop(
        out: &mut Vec<u8>,
        strings: &mut Vec<u8>,
        offsets: &mut BTreeMap<&'static str, u32>,
        name: &'static str,
        value: &[u8],
    ) {
        let offset = *offsets.entry(name).or_insert_with(|| {
            let offset = strings.len() as u32;
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            offset
        });
        be(out, FDT_PROP);
        be(out, value.len() as u32);
        be(out, offset);
        out.extend_from_slice(value);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    fn reg64(address: u64, size: u64) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(address >> 32).to_be_bytes()[4..]);
        out.extend_from_slice(&(address as u32).to_be_bytes());
        out.extend_from_slice(&(size >> 32).to_be_bytes()[4..]);
        out.extend_from_slice(&(size as u32).to_be_bytes());
        out
    }
    fn reg_2_1(address: u64, size: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(address >> 32).to_be_bytes()[4..]);
        out.extend_from_slice(&(address as u32).to_be_bytes());
        out.extend_from_slice(&size.to_be_bytes());
        out
    }
    fn finish(structure: Vec<u8>, strings: Vec<u8>) -> Vec<u8> {
        let header = 40usize;
        let struct_off = header;
        let strings_off = struct_off + structure.len();
        let total = strings_off + strings.len();
        let mut out = Vec::new();
        for value in [
            FDT_MAGIC,
            total as u32,
            struct_off as u32,
            strings_off as u32,
            header as u32,
            17,
            16,
            0,
            strings.len() as u32,
            structure.len() as u32,
        ] {
            be(&mut out, value);
        }
        out.extend_from_slice(&structure);
        out.extend_from_slice(&strings);
        out
    }
    fn test_dtb(
        compatible: &[u8],
        rpi: bool,
        with_initrd: bool,
        rpi_ranges_map_uart: bool,
        bootargs: &[u8],
        with_gic: bool,
        with_pcie: bool,
    ) -> Vec<u8> {
        let mut st = Vec::new();
        let mut strings = Vec::new();
        let mut offsets = BTreeMap::new();
        begin(&mut st, b"");
        prop(
            &mut st,
            &mut strings,
            &mut offsets,
            "#address-cells",
            &2u32.to_be_bytes(),
        );
        prop(
            &mut st,
            &mut strings,
            &mut offsets,
            "#size-cells",
            &2u32.to_be_bytes(),
        );
        prop(
            &mut st,
            &mut strings,
            &mut offsets,
            "compatible",
            compatible,
        );
        begin(&mut st, b"aliases");
        prop(
            &mut st,
            &mut strings,
            &mut offsets,
            "serial10",
            if rpi {
                b"/soc@107c000000/serial@7d001000\0"
            } else {
                b"/pl011@9000000\0"
            },
        );
        end(&mut st);
        begin(&mut st, b"chosen");
        prop(
            &mut st,
            &mut strings,
            &mut offsets,
            "stdout-path",
            b"serial10:115200n8\0",
        );
        prop(&mut st, &mut strings, &mut offsets, "bootargs", bootargs);
        if with_initrd {
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "linux,initrd-start",
                &0x0800_0000u64.to_be_bytes(),
            );
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "linux,initrd-end",
                &0x0810_0000u64.to_be_bytes(),
            );
        }
        end(&mut st);
        begin(&mut st, b"memory@0");
        prop(
            &mut st,
            &mut strings,
            &mut offsets,
            "reg",
            &reg64(if rpi { 0 } else { 0x4000_0000 }, 0x4000_0000),
        );
        end(&mut st);
        if rpi {
            begin(&mut st, b"reserved-memory");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "#address-cells",
                &2u32.to_be_bytes(),
            );
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "#size-cells",
                &2u32.to_be_bytes(),
            );
            begin(&mut st, b"area@1000");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "reg",
                &reg64(0x1000, 0x2000),
            );
            prop(&mut st, &mut strings, &mut offsets, "no-map", &[]);
            end(&mut st);
            end(&mut st);
            begin(&mut st, b"cpus");
            begin(&mut st, b"cpu@0");
            end(&mut st);
            begin(&mut st, b"cpu@3");
            end(&mut st);
            end(&mut st);
            begin(&mut st, b"psci");
            prop(&mut st, &mut strings, &mut offsets, "method", b"smc\0");
            end(&mut st);
            begin(&mut st, b"soc@107c000000");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "#address-cells",
                &2u32.to_be_bytes(),
            );
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "#size-cells",
                &1u32.to_be_bytes(),
            );
            let mut ranges = Vec::new();
            // Real BCM2712-style bus ranges use two child-address cells, two
            // parent-address cells, and one size cell. Keep a non-matching
            // entry first to prove translation scans the complete property.
            ranges.extend_from_slice(&0u32.to_be_bytes());
            ranges.extend_from_slice(&0x4000_0000u32.to_be_bytes());
            ranges.extend_from_slice(&0u32.to_be_bytes());
            ranges.extend_from_slice(&0x4000_0000u32.to_be_bytes());
            ranges.extend_from_slice(&0x0100_0000u32.to_be_bytes());
            if rpi_ranges_map_uart {
                ranges.extend_from_slice(&0u32.to_be_bytes());
                ranges.extend_from_slice(&0x7c00_0000u32.to_be_bytes());
                ranges.extend_from_slice(&0x0000_0010u32.to_be_bytes());
                ranges.extend_from_slice(&0x7c00_0000u32.to_be_bytes());
                ranges.extend_from_slice(&0x0400_0000u32.to_be_bytes());
            }
            prop(&mut st, &mut strings, &mut offsets, "ranges", &ranges);
            begin(&mut st, b"serial@7d001000");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "compatible",
                b"arm,pl011\0arm,primecell\0",
            );
            prop(&mut st, &mut strings, &mut offsets, "status", b"okay\0");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "reg",
                &reg_2_1(0x7d00_1000, 0x1000),
            );
            end(&mut st);
            begin(&mut st, b"intc@7d517ac0");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "interrupt-controller",
                &[],
            );
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "compatible",
                b"brcm,bcm7271-l2-intc\0",
            );
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "reg",
                &reg_2_1(0x7d51_7ac0, 0x100),
            );
            end(&mut st);
            if with_gic {
                begin(&mut st, b"interrupt-controller@7fff9000");
                prop(
                    &mut st,
                    &mut strings,
                    &mut offsets,
                    "interrupt-controller",
                    &[],
                );
                prop(
                    &mut st,
                    &mut strings,
                    &mut offsets,
                    "compatible",
                    b"arm,gic-v3\0",
                );
                let mut gic_regs = reg_2_1(0x7fff_9000, 0x1000);
                gic_regs.extend_from_slice(&reg_2_1(0x7fff_a000, 0x20_000));
                prop(&mut st, &mut strings, &mut offsets, "reg", &gic_regs);
                end(&mut st);
            }
            begin(&mut st, b"reset-controller@119500");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "compatible",
                b"raspberrypi,rp1-pcie-reset\0",
            );
            end(&mut st);
            begin(&mut st, b"rp1");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "compatible",
                b"raspberrypi,rp1\0",
            );
            end(&mut st);
            end(&mut st);
            if with_pcie {
                begin(&mut st, b"axi");
                prop(
                    &mut st,
                    &mut strings,
                    &mut offsets,
                    "#address-cells",
                    &2u32.to_be_bytes(),
                );
                prop(
                    &mut st,
                    &mut strings,
                    &mut offsets,
                    "#size-cells",
                    &2u32.to_be_bytes(),
                );
                prop(&mut st, &mut strings, &mut offsets, "ranges", &[]);
                begin(&mut st, b"pcie@1000100000");
                prop(&mut st, &mut strings, &mut offsets, "device_type", b"pci\0");
                prop(
                    &mut st,
                    &mut strings,
                    &mut offsets,
                    "compatible",
                    b"brcm,bcm2712-pcie\0",
                );
                prop(
                    &mut st,
                    &mut strings,
                    &mut offsets,
                    "reg",
                    &reg64(0x10_0010_0000, 0x10_000),
                );
                end(&mut st);
                begin(&mut st, b"pcie@1000120000");
                prop(&mut st, &mut strings, &mut offsets, "device_type", b"pci\0");
                prop(
                    &mut st,
                    &mut strings,
                    &mut offsets,
                    "compatible",
                    b"brcm,bcm2712-pcie\0",
                );
                prop(
                    &mut st,
                    &mut strings,
                    &mut offsets,
                    "reg",
                    &reg64(0x10_0012_0000, 0x10_000),
                );
                begin(&mut st, b"rp1");
                prop(
                    &mut st,
                    &mut strings,
                    &mut offsets,
                    "compatible",
                    b"raspberrypi,rp1\0",
                );
                end(&mut st);
                end(&mut st);
                end(&mut st);
            }
        } else {
            begin(&mut st, b"pl011@9000000");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "compatible",
                b"arm,pl011\0",
            );
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "reg",
                &reg64(0x0900_0000, 0x1000),
            );
            end(&mut st);
        }
        end(&mut st);
        be(&mut st, FDT_END);
        finish(st, strings)
    }

    #[test]
    fn detects_qemu_virt_and_resolves_absolute_uart_alias() {
        let info = parse_platform_dtb(&test_dtb(
            b"linux,dummy-virt\0",
            false,
            true,
            false,
            b"console=ttyAMA0\0",
            false,
            false,
        ))
        .unwrap();
        assert_eq!(info.platform, DetectedPlatform::QemuVirt);
        assert_eq!(info.serial.unwrap().base, 0x0900_0000);
        assert!(info.has_initrd());
    }

    #[test]
    fn detects_rpi5_prefers_soc_pl011_and_translates_ranges() {
        let info = parse_platform_dtb(&test_dtb(
            b"raspberrypi,5-model-b\0brcm,bcm2712\0",
            true,
            false,
            true,
            b"console=ttyAMA10 yarm.boot_phase=dtb\0",
            true,
            true,
        ))
        .unwrap();
        assert_eq!(info.platform, DetectedPlatform::Rpi5Bcm2712);
        assert_eq!(info.stdout_path.as_str(), "/soc@107c000000/serial@7d001000");
        assert_eq!(info.serial.unwrap().base, 0x10_7d00_1000);
        assert_eq!(info.memory_start, Some(0));
        assert_eq!(info.reserved_count, 1);
        assert_eq!(
            info.interrupt_controller_path.as_str(),
            "/soc@107c000000/intc@7d517ac0"
        );
    }

    #[test]
    fn rpi5_uart_translation_fails_closed_when_ranges_do_not_map_reg() {
        let info = parse_platform_dtb(&test_dtb(
            b"raspberrypi,5-model-b\0brcm,bcm2712\0",
            true,
            false,
            false,
            b"console=ttyAMA10\0",
            true,
            true,
        ))
        .unwrap();
        assert_eq!(info.stdout_path.as_str(), "/soc@107c000000/serial@7d001000");
        assert_eq!(info.serial, None);
    }

    #[test]
    fn rpi5_stage1_diagnostics_report_bounded_firmware_state() {
        let dtb = test_dtb(
            b"raspberrypi,5-model-b\0brcm,bcm2712\0",
            true,
            true,
            true,
            b"console=ttyAMA10 yarm.boot_phase=dtb\0",
            true,
            true,
        );
        let info = parse_platform_dtb_diagnostics(&dtb).unwrap();
        assert_eq!(
            &info.memory_ranges[..info.memory_range_count],
            &[DiagnosticRange {
                start: 0,
                size: 0x4000_0000,
                no_map: false,
            }]
        );
        assert_eq!(
            &info.reserved_ranges[..info.reserved_range_count],
            &[DiagnosticRange {
                start: 0x1000,
                size: 0x2000,
                no_map: true,
            }]
        );
        assert_eq!(info.initrd_start, Some(0x0800_0000));
        assert_eq!(info.initrd_end, Some(0x0810_0000));
        assert_eq!(
            info.bootargs_len,
            b"console=ttyAMA10 yarm.boot_phase=dtb".len()
        );
        assert!(!info.bootargs_truncated);
        assert_eq!(
            info.l2_interrupt_controller_path.as_str(),
            "/soc@107c000000/intc@7d517ac0"
        );
        assert_eq!(
            info.l2_interrupt_controller_compatible.as_str(),
            "brcm,bcm7271-l2-intc"
        );
        assert_eq!(info.l2_interrupt_controller_base, Some(0x10_7d51_7ac0));
        assert_eq!(
            info.gic_path.as_str(),
            "/soc@107c000000/interrupt-controller@7fff9000"
        );
        assert_eq!(info.gic_compatible.as_str(), "arm,gic-v3");
        assert_eq!(info.gic_dist_base, Some(0x10_7fff_9000));
        assert_eq!(info.gic_redist_base, Some(0x10_7fff_a000));
        assert_eq!(info.psci_conduit, DiagnosticPsciConduit::Smc);
        assert_eq!(info.cpu_bitmap, 0b1001);
        assert_eq!(info.pcie_controller_count, 2);
        assert_eq!(
            info.pcie_controllers[0].path.as_str(),
            "/axi/pcie@1000100000"
        );
        assert_eq!(info.pcie_controllers[0].base, Some(0x10_0010_0000));
        assert_eq!(
            info.pcie_controllers[1].path.as_str(),
            "/axi/pcie@1000120000"
        );
        assert_eq!(info.pcie_controllers[1].base, Some(0x10_0012_0000));
        assert_eq!(info.rp1_controller_index, Some(1));
        assert_eq!(info.rp1_node_path.as_str(), "/axi/pcie@1000120000/rp1");
        assert_ne!(
            info.rp1_node_path.as_str(),
            "/soc@107c000000/reset-controller@119500"
        );
    }

    #[test]
    fn rpi5_stage1_diagnostics_report_missing_initrd_and_bootargs_truncation() {
        let mut bootargs = [b'x'; MAX_DIAGNOSTIC_BOOTARGS + 2];
        bootargs[MAX_DIAGNOSTIC_BOOTARGS + 1] = 0;
        let info = parse_platform_dtb_diagnostics(&test_dtb(
            b"raspberrypi,5-model-b\0brcm,bcm2712\0",
            true,
            false,
            true,
            &bootargs,
            false,
            false,
        ))
        .unwrap();
        assert_eq!(info.initrd_start, None);
        assert_eq!(info.initrd_end, None);
        assert_eq!(info.bootargs_len, MAX_DIAGNOSTIC_BOOTARGS + 1);
        assert!(info.bootargs_truncated);
        assert_eq!(info.gic_dist_base, None);
        assert_eq!(info.gic_redist_base, None);
        assert!(!info.l2_interrupt_controller_path.is_empty());
        assert_eq!(info.pcie_controller_count, 0);
        assert_eq!(info.rp1_controller_index, None);
        assert!(
            info.rp1_node_path.is_empty(),
            "an rp1 node outside the classified PCIe controller must not count"
        );
    }

    #[test]
    fn rpi5_stage1_kernel_plan_reserves_firmware_kernel_dtb_and_allocates_safely() {
        let mut info = PlatformDtbDiagnostics::default();
        info.memory_ranges[0] = DiagnosticRange {
            start: 0,
            size: 0x3fc0_0000,
            no_map: false,
        };
        info.memory_ranges[1] = DiagnosticRange {
            start: 0x4000_0000,
            size: 0x4000_0000,
            no_map: false,
        };
        info.memory_range_count = 2;
        info.reserved_ranges[0] = DiagnosticRange {
            start: 0,
            size: 0x80000,
            no_map: true,
        };
        info.reserved_ranges[1] = DiagnosticRange {
            start: 0x3fd2_3180,
            size: 0x3e,
            no_map: true,
        };
        info.reserved_ranges[2] = DiagnosticRange {
            start: 0x5000_0000,
            size: 0,
            no_map: false,
        };
        info.reserved_range_count = 3;

        let kernel = Stage1KernelRange::new(0x80000, 0x280000);
        let dtb = Stage1KernelRange::new(0x2efe_c600, 0x2eff_c600);
        let plan = plan_rpi5_stage1_kernel_memory(&info, kernel, dtb).unwrap();

        assert_eq!(plan.reserved_ranges[0], Stage1KernelRange::new(0, 0x80000));
        assert_eq!(plan.reserved_ranges[1], kernel);
        assert_eq!(plan.reserved_ranges[2], dtb);
        assert_eq!(
            plan.reserved_ranges[3],
            Stage1KernelRange::new(0x3fd2_3180, 0x3fd2_31be)
        );
        assert_eq!(&plan.zero_reserved_skipped[..1], &[2]);
        assert_eq!(
            plan.page_table_pool.end - plan.page_table_pool.start,
            STAGE1_PT_POOL_SIZE
        );
        assert_eq!(
            plan.early_heap.end - plan.early_heap.start,
            STAGE1_EARLY_HEAP_SIZE
        );
        for allocation in [plan.page_table_pool, plan.early_heap] {
            for reserved in &plan.reserved_ranges[..plan.reserved_range_count] {
                assert!(!allocation.overlaps(*reserved));
            }
        }
        assert!(!plan.page_table_pool.overlaps(plan.early_heap));
    }

    #[test]
    fn rpi5_stage1_kernel_plan_does_not_require_an_initrd() {
        let mut info = PlatformDtbDiagnostics::default();
        info.memory_ranges[0] = DiagnosticRange {
            start: 0,
            size: 0x4000_0000,
            no_map: false,
        };
        info.memory_range_count = 1;
        assert_eq!(info.initrd_start, None);
        assert_eq!(info.initrd_end, None);
        assert!(
            plan_rpi5_stage1_kernel_memory(
                &info,
                Stage1KernelRange::new(0x80000, 0x180000),
                Stage1KernelRange::new(0x2efe_c600, 0x2eff_c600),
            )
            .is_ok()
        );
    }

    #[test]
    fn rpi5_stage1_identity_map_covers_core_ranges_and_uart_attributes() {
        let mut info = PlatformDtbDiagnostics::default();
        info.memory_ranges[0] = DiagnosticRange {
            start: 0,
            size: 0x3fc0_0000,
            no_map: false,
        };
        info.memory_ranges[1] = DiagnosticRange {
            start: 0x4000_0000,
            size: 0x4000_0000,
            no_map: false,
        };
        info.memory_range_count = 2;
        info.gic_dist_base = Some(0x10_7fff_9000);
        info.gic_redist_base = Some(0x10_7fff_a000);
        let kernel = Stage1KernelRange::new(0x80000, 0x5b50_000);
        let dtb = Stage1KernelRange::new(0x2efe_c600, 0x2eff_ff4e);
        let kernel_plan = Stage1KernelPlan {
            page_table_pool: Stage1KernelRange::new(0x5b50_000, 0x5b90_000),
            early_heap: Stage1KernelRange::new(0x5b90_000, 0x5d90_000),
            ..Stage1KernelPlan::default()
        };

        let stack = Stage1KernelRange::new(0x5b4f_000, 0x5b50_000);
        let plan =
            plan_rpi5_stage1_identity_map(&info, &kernel_plan, kernel, stack, dtb, 0x10_7d00_1000)
                .unwrap();
        assert_eq!(plan.pt_pool, kernel_plan.page_table_pool);
        assert_eq!(plan.mapping_count, 5);
        assert_eq!(
            plan.mappings[0],
            Stage1MmuMapping {
                range: Stage1KernelRange::new(0, 0x3fc0_0000),
                memory_type: Stage1MmuMemoryType::Normal,
            }
        );
        assert_eq!(
            plan.mappings[1],
            Stage1MmuMapping {
                range: Stage1KernelRange::new(0x4000_0000, 0x8000_0000),
                memory_type: Stage1MmuMemoryType::Normal,
            }
        );
        assert_eq!(
            plan.mappings[2],
            Stage1MmuMapping {
                range: Stage1KernelRange::new(0x10_7d00_1000, 0x10_7d00_2000),
                memory_type: Stage1MmuMemoryType::DeviceNgnre,
            }
        );
        assert_eq!(
            plan.mappings[3],
            Stage1MmuMapping {
                range: Stage1KernelRange::new(0x10_7fff_9000, 0x10_7fff_a000),
                memory_type: Stage1MmuMemoryType::DeviceNgnre,
            }
        );
        assert_eq!(
            plan.mappings[4],
            Stage1MmuMapping {
                range: Stage1KernelRange::new(0x10_7fff_a000, 0x10_7fff_b000),
                memory_type: Stage1MmuMemoryType::DeviceNgnre,
            }
        );
        for normal in &plan.mappings[..2] {
            assert!(!normal.range.overlaps(plan.mappings[2].range));
        }
        for required in [
            kernel,
            stack,
            dtb,
            kernel_plan.page_table_pool,
            kernel_plan.early_heap,
        ] {
            assert!(stage1_mmu_normal_mapping_contains(&plan, required));
        }
    }

    #[test]
    fn rpi5_stage1_identity_map_rejects_pt_pool_outside_normal_ram() {
        let mut info = PlatformDtbDiagnostics::default();
        info.memory_ranges[0] = DiagnosticRange {
            start: 0,
            size: 0x4000_0000,
            no_map: false,
        };
        info.memory_range_count = 1;
        let kernel_plan = Stage1KernelPlan {
            page_table_pool: Stage1KernelRange::new(0x5000_0000, 0x5004_0000),
            early_heap: Stage1KernelRange::new(0x1000_0000, 0x1020_0000),
            ..Stage1KernelPlan::default()
        };
        assert_eq!(
            plan_rpi5_stage1_identity_map(
                &info,
                &kernel_plan,
                Stage1KernelRange::new(0x80000, 0x180000),
                Stage1KernelRange::new(0x170000, 0x171000),
                Stage1KernelRange::new(0x2efe_c600, 0x2eff_c600),
                0x10_7d00_1000,
            ),
            Err(Stage1MmuPlanFailure::PageTablePoolNotMapped)
        );
    }

    #[test]
    fn rpi5_stage1_allocator_handoff_is_sorted_aligned_and_excludes_reservations() {
        let mut info = PlatformDtbDiagnostics::default();
        info.memory_ranges[0] = DiagnosticRange {
            start: 0,
            size: 0x3fc0_0000,
            no_map: false,
        };
        info.memory_ranges[1] = DiagnosticRange {
            start: 0x4000_0000,
            size: 0x4000_0000,
            no_map: false,
        };
        info.memory_range_count = 2;
        let mut kernel_plan = Stage1KernelPlan::default();
        kernel_plan.reserved_ranges[0] = Stage1KernelRange::new(0, 0x80000);
        kernel_plan.reserved_ranges[1] = Stage1KernelRange::new(0x80000, 0x5b50_000);
        kernel_plan.reserved_ranges[2] = Stage1KernelRange::new(0x2efe_c600, 0x2eff_ff4e);
        kernel_plan.reserved_ranges[3] = Stage1KernelRange::new(0x3fd2_3180, 0x3fd2_31be);
        kernel_plan.reserved_range_count = 4;
        kernel_plan.page_table_pool = Stage1KernelRange::new(0x5b50_000, 0x5b90_000);
        kernel_plan.early_heap = Stage1KernelRange::new(0x5b90_000, 0x5d90_000);

        let plan = plan_rpi5_stage1_allocator_handoff(&info, &kernel_plan).unwrap();
        assert_eq!(plan.reserved_count, 6);
        assert_eq!(
            plan.reserved[4],
            Stage1AllocatorReservation {
                range: kernel_plan.page_table_pool,
                reason: Stage1AllocatorReservationReason::PageTablePool,
            }
        );
        assert_eq!(
            plan.reserved[5],
            Stage1AllocatorReservation {
                range: kernel_plan.early_heap,
                reason: Stage1AllocatorReservationReason::EarlyHeap,
            }
        );
        assert_eq!(
            &plan.usable[..plan.usable_count],
            &[
                Stage1KernelRange::new(0x5d90_000, 0x2efe_c000),
                Stage1KernelRange::new(0x2f00_0000, 0x3fc0_0000),
                Stage1KernelRange::new(0x4000_0000, 0x8000_0000),
            ]
        );
        for (index, usable) in plan.usable[..plan.usable_count].iter().enumerate() {
            assert_eq!(usable.start % STAGE1_PAGE_SIZE, 0);
            assert_eq!(usable.end % STAGE1_PAGE_SIZE, 0);
            if index != 0 {
                assert!(plan.usable[index - 1].start < usable.start);
            }
            for reserved in &plan.reserved[..plan.reserved_count] {
                assert!(!usable.overlaps(reserved.range));
            }
        }
        let test_frame = plan.usable[0].start;
        assert_eq!(test_frame, 0x5d90_000);
        assert_eq!(test_frame % STAGE1_PAGE_SIZE, 0);
        let test_range = Stage1KernelRange::new(test_frame, test_frame + STAGE1_PAGE_SIZE);
        assert!(
            plan.reserved[..plan.reserved_count]
                .iter()
                .all(|reserved| !test_range.overlaps(reserved.range))
        );
        assert_eq!(
            plan.total_pages,
            plan.usable[..plan.usable_count]
                .iter()
                .map(|range| (range.end - range.start) / STAGE1_PAGE_SIZE)
                .sum()
        );
        assert_eq!(plan.total_pages, 499_292);
    }

    #[test]
    fn rpi5_stage1_allocator_handoff_does_not_require_initrd() {
        let mut info = PlatformDtbDiagnostics::default();
        info.memory_ranges[0] = DiagnosticRange {
            start: 0,
            size: 0x4000_0000,
            no_map: false,
        };
        info.memory_range_count = 1;
        let mut kernel_plan = Stage1KernelPlan::default();
        kernel_plan.reserved_ranges[0] = Stage1KernelRange::new(0, 0x80000);
        kernel_plan.reserved_ranges[1] = Stage1KernelRange::new(0x80000, 0x180000);
        kernel_plan.reserved_ranges[2] = Stage1KernelRange::new(0x2efe_c600, 0x2eff_c600);
        kernel_plan.reserved_range_count = 3;
        kernel_plan.page_table_pool = Stage1KernelRange::new(0x180000, 0x1c0000);
        kernel_plan.early_heap = Stage1KernelRange::new(0x1c0000, 0x3c0000);
        assert_eq!(info.initrd_start, None);
        assert_eq!(info.initrd_end, None);
        assert!(plan_rpi5_stage1_allocator_handoff(&info, &kernel_plan).is_ok());
    }

    #[test]
    fn rpi5_stage1_timer_diagnostic_rejects_non_incrementing_counter() {
        assert_eq!(rpi5_stage1_timer_delta(100, 101), Some(1));
        assert_eq!(rpi5_stage1_timer_delta(100, 100), None);
        assert_eq!(rpi5_stage1_timer_delta(101, 100), None);
    }

    #[test]
    fn rpi5_stage1_irqtimer_diagnostic_does_not_require_initrd() {
        let info = PlatformDtbDiagnostics::default();
        assert_eq!(info.initrd_start, None);
        assert_eq!(info.initrd_end, None);
        assert_eq!(rpi5_stage1_timer_delta(7, 9), Some(2));
    }

    #[test]
    fn missing_initrd_is_allowed_before_kernel_phase_only() {
        for phase in [
            BootPhase::Entry,
            BootPhase::Uart,
            BootPhase::Dtb,
            BootPhase::Mmu,
        ] {
            assert!(rpi5_phase_allows_boot(phase, false));
        }
        assert!(!rpi5_phase_allows_boot(BootPhase::Kernel, false));
        assert!(rpi5_phase_allows_boot(BootPhase::Kernel, true));
    }
}
