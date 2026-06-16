// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! x86_64 AP per-CPU record/slot scaffold.
//!
//! Provides one fixed `PerCpuRecord` per possible CPU, indexed by logical
//! CPU id. The records live in `.bss` (aligned to a cache line) so they
//! are addressable before the page-table-managed direct map is up.
//!
//! Safety contract:
//! - The per-CPU record is BSP-initialized for each AP before the AP is
//!   started. The AP itself never mutates the record in this pass.
//! - GS-base write to the record pointer is **not** performed in this
//!   pass (`yarm_x86_64_ap_entry` is asm-only, no per-CPU MSR write
//!   yet). The smoke gate accepts `X86_AP_GS_DEFERRED reason=...` and
//!   the explicit deferral reason is recorded.
//! - Records reserve slots for future TSS / IDT / scheduler pointers so
//!   the layout is forward-compatible when those bring-ups land.

use crate::kernel::scheduler::CpuId;

/// Number of per-CPU records; matches the platform MAX_CPUS.
pub const MAX_PERCPU_RECORDS: usize = crate::arch::platform_constants::MAX_CPUS;

/// AP-side state flags carried inside `PerCpuRecord.flags`.
pub mod flag {
    pub const RECORD_INITIALIZED: u32 = 1 << 0;
    pub const GS_BASE_WRITTEN: u32 = 1 << 1;
}

/// Fixed per-CPU record layout, owned by the BSP and indexed by logical
/// CPU id. Field offsets are stable and tested.
///
/// Layout (offsets in bytes):
/// - `0`  : cpu_id          u8
/// - `1`  : apic_id         u8
/// - `2`  : _pad_align      [u8; 6]
/// - `8`  : stack_top       u64
/// - `16` : flags           u32
/// - `20` : _pad_flags      u32
/// - `24` : tss_ptr         u64  (reserved for future per-CPU TSS)
/// - `32` : idt_ptr         u64  (reserved for future per-CPU IDT)
/// - `40` : scheduler_ptr   u64  (reserved for future per-CPU scheduler bridge)
/// - `48` : _reserved_tail  [u64; 8]
///
/// Explicit-field bytes = 112; struct stride = 128 (padded to the 64-byte
/// alignment so the slot table strides cleanly).
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct PerCpuRecord {
    pub cpu_id: u8,
    pub apic_id: u8,
    _pad_align: [u8; 6],
    pub stack_top: u64,
    pub flags: u32,
    _pad_flags: u32,
    pub tss_ptr: u64,
    pub idt_ptr: u64,
    pub scheduler_ptr: u64,
    _reserved_tail: [u64; 8],
}

impl PerCpuRecord {
    pub const SIZE: usize = core::mem::size_of::<Self>();

    pub const fn empty() -> Self {
        Self {
            cpu_id: 0,
            apic_id: 0,
            _pad_align: [0; 6],
            stack_top: 0,
            flags: 0,
            _pad_flags: 0,
            tss_ptr: 0,
            idt_ptr: 0,
            scheduler_ptr: 0,
            _reserved_tail: [0; 8],
        }
    }
}

/// Per-CPU slot table. Lives in `.bss` (aligned to 4 KiB so the table is
/// page-aligned for future LDGS / direct-map use). Index by logical CPU
/// id (0..MAX_PERCPU_RECORDS).
#[repr(C, align(4096))]
struct PerCpuSlots([PerCpuRecord; MAX_PERCPU_RECORDS]);

static mut PER_CPU_SLOTS: PerCpuSlots =
    PerCpuSlots([const { PerCpuRecord::empty() }; MAX_PERCPU_RECORDS]);

/// Returns the base virtual address of the per-CPU record for `cpu`.
///
/// The address is stable for the life of the kernel because the table
/// lives in `.bss`. The caller is responsible for not reading/writing
/// past `PerCpuRecord::SIZE` bytes from the returned base.
pub fn record_base(cpu: CpuId) -> usize {
    let idx = (cpu.0 as usize).min(MAX_PERCPU_RECORDS - 1);
    let slots = core::ptr::addr_of!(PER_CPU_SLOTS) as *const PerCpuRecord;
    unsafe { slots.add(idx) as usize }
}

/// BSP-initializes the per-CPU record for `cpu` with the supplied APIC
/// id and stack top. Sets `RECORD_INITIALIZED` flag. Idempotent: a
/// second call overwrites the prior contents (intentional — the
/// AP-bring-up path may retry).
pub fn init_record_for_ap(cpu: CpuId, apic_id: u8, stack_top: u64) {
    let base = record_base(cpu) as *mut PerCpuRecord;
    unsafe {
        let record = PerCpuRecord {
            cpu_id: cpu.0,
            apic_id,
            _pad_align: [0; 6],
            stack_top,
            flags: flag::RECORD_INITIALIZED,
            _pad_flags: 0,
            tss_ptr: 0,
            idt_ptr: 0,
            scheduler_ptr: 0,
            _reserved_tail: [0; 8],
        };
        core::ptr::write_volatile(base, record);
    }
}

/// Reads back the per-CPU record for `cpu`. Used by tests and by the
/// BSP-side env-scaffold to confirm the record was initialized.
pub fn read_record(cpu: CpuId) -> PerCpuRecord {
    let base = record_base(cpu) as *const PerCpuRecord;
    unsafe { core::ptr::read_volatile(base) }
}

/// Marks the per-CPU record for `cpu` as having had GS-base written.
/// Currently unused (GS-base write is deferred); future passes that
/// extend `yarm_x86_64_ap_entry` to write IA32_GS_BASE will call this
/// after the readback succeeds.
pub fn mark_gs_base_written(cpu: CpuId) {
    let base = record_base(cpu) as *mut PerCpuRecord;
    unsafe {
        let mut record = core::ptr::read_volatile(base);
        record.flags |= flag::GS_BASE_WRITTEN;
        core::ptr::write_volatile(base, record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percpu_record_layout_offsets_are_stable() {
        let record = PerCpuRecord::empty();
        let base = &record as *const _ as usize;
        assert_eq!(core::ptr::addr_of!(record.cpu_id) as usize - base, 0);
        assert_eq!(core::ptr::addr_of!(record.apic_id) as usize - base, 1);
        assert_eq!(core::ptr::addr_of!(record.stack_top) as usize - base, 8);
        assert_eq!(core::ptr::addr_of!(record.flags) as usize - base, 16);
        assert_eq!(core::ptr::addr_of!(record.tss_ptr) as usize - base, 24);
        assert_eq!(core::ptr::addr_of!(record.idt_ptr) as usize - base, 32);
        assert_eq!(
            core::ptr::addr_of!(record.scheduler_ptr) as usize - base,
            40
        );
    }

    #[test]
    fn percpu_record_size_and_alignment() {
        // 112 bytes of explicit fields + 16 bytes tail padding so the
        // struct stride is a multiple of the 64-byte alignment.
        assert_eq!(PerCpuRecord::SIZE, 128);
        assert_eq!(core::mem::align_of::<PerCpuRecord>(), 64);
    }

    #[test]
    fn record_base_is_distinct_per_cpu() {
        let a = record_base(CpuId(0));
        let b = record_base(CpuId(1));
        let c = record_base(CpuId(2));
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
        // Stride must equal PerCpuRecord::SIZE.
        assert_eq!(b - a, PerCpuRecord::SIZE);
        assert_eq!(c - b, PerCpuRecord::SIZE);
    }

    #[test]
    fn record_base_clamps_out_of_range_cpu_to_last_slot() {
        let last = record_base(CpuId((MAX_PERCPU_RECORDS - 1) as u8));
        let oob = record_base(CpuId(255));
        assert_eq!(last, oob, "out-of-range CpuId must clamp into the table");
    }

    #[test]
    fn init_record_for_ap_writes_and_reads_back() {
        init_record_for_ap(CpuId(3), 3, 0xCAFE_0000);
        let record = read_record(CpuId(3));
        assert_eq!(record.cpu_id, 3);
        assert_eq!(record.apic_id, 3);
        assert_eq!(record.stack_top, 0xCAFE_0000);
        assert_eq!(
            record.flags & flag::RECORD_INITIALIZED,
            flag::RECORD_INITIALIZED
        );
        assert_eq!(record.flags & flag::GS_BASE_WRITTEN, 0);
    }

    #[test]
    fn mark_gs_base_written_sets_only_the_gs_flag() {
        init_record_for_ap(CpuId(4), 4, 0xBEEF_0000);
        mark_gs_base_written(CpuId(4));
        let record = read_record(CpuId(4));
        assert_eq!(record.flags & flag::GS_BASE_WRITTEN, flag::GS_BASE_WRITTEN);
        assert_eq!(
            record.flags & flag::RECORD_INITIALIZED,
            flag::RECORD_INITIALIZED
        );
    }
}
