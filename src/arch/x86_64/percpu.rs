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

/// Flags carried inside `PerCpuRecord.idle_flags` (Stage 183 inc.3).
pub mod idle_flag {
    /// BSP populated `idle_entry`/`idle_stack_top`/`idle_cr3` before SIPI. This is
    /// idle-task METADATA (a reserved, validated description of the AP idle context) —
    /// NOT a scheduler task; nothing is enqueued.
    pub const IDLE_TASK_META_SET: u32 = 1 << 0;
}

/// Value the AP stores into `PerCpuRecord.env_canary` (via a `gs:`-relative write)
/// after loading the kernel CR3 — proves higher-half `.bss` is writable on the AP
/// under the kernel address space AND that GS-relative addressing works ('183' pun
/// intended). Checked BSP-side for the `X86_AP_KERNEL_CR3_OK` verdict.
pub const AP_ENV_CANARY: u32 = 0x0183_C0DE;

/// Byte offsets of the AP-written per-CPU fields, hardcoded in the AP entry asm's
/// `gs:[..]` stores (GS base == this record). Locked by tests below.
pub const ENV_CANARY_OFFSET: usize = 48;
pub const SAVED_RSP_OFFSET: usize = 56;
/// Stage 183 inc.4 (interrupt-safe idle): offsets the AP IDT stubs write via `gs:`.
/// `irq_hit_count`/`irq_hit_vector` are written by the smoke-vector handler;
/// `irq_unexpected_vec` (vector+1; 0 = none) by the catch-all park stub.
pub const IRQ_HIT_COUNT_OFFSET: usize = 96;
pub const IRQ_HIT_VECTOR_OFFSET: usize = 100;
pub const IRQ_UNEXPECTED_VEC_OFFSET: usize = 104;
/// Stage 183.5: incremented by the remote-wake stub (vector 0xF1) via gs:.
pub const REMOTE_WAKE_COUNT_OFFSET: usize = 108;
/// Stage 183.5 fix (no_resume_after_handler): the smoke handler's sub-stage
/// (IRQ_HANDLER_ENTER/EOI/IRET) written via gs:[112], and the PERSISTENT
/// post-resume ACK the AP writes via gs:[116] after the hlt-return path
/// confirms the handler ran. The BSP polls the persistent ACK — never a
/// transient stage — so serial-logging latency can no longer lose the race.
pub const IRQ_STAGE_OFFSET: usize = 112;
pub const IRQ_ACK_OFFSET: usize = 116;
/// Stage 183.5 fix #2 (#PF at 0x7170): the admission phase runs on the current
/// TASK address space where the low identity trampoline VAs are unmapped — the
/// AP therefore MIRRORS its sched-idle stage and wake-reenter count into the
/// per-CPU record (kernel .bss, always mapped) via gs:, and the BSP polls ONLY
/// these mirrors post-boot.
pub const SCHED_STAGE_OFFSET: usize = 120;
pub const WAKE_REENTER_MIRROR_OFFSET: usize = 124;
/// Stage 183.6 real cross-CPU TLB shootdown mailbox (BSP writes req_gen/req_va,
/// the AP sched-idle wake path executes invlpg + sets ack_gen == req_gen). Single
/// writer per field per direction, so it needs no lock and no KernelState access
/// from the AP — a real remote ACK entirely through the (always-mapped) record.
pub const TLB_REQ_GEN_OFFSET: usize = 128;
pub const TLB_ACK_GEN_OFFSET: usize = 132;
pub const TLB_REQ_VA_OFFSET: usize = 136;

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
/// - `24` : tss_ptr         u64  (Stage 183 inc.3: VA of this AP's X86TaskStateSegment)
/// - `32` : idt_ptr         u64  (reserved for future per-CPU IDT)
/// - `40` : scheduler_ptr   u64  (reserved for future per-CPU scheduler bridge)
/// - `48` : env_canary      u32  (AP-written via gs:[48] after kernel-CR3 load)
/// - `52` : _pad_canary     u32
/// - `56` : saved_rsp       u64  (AP-written via gs:[56]; live idle-loop rsp)
/// - `64` : idle_entry      u64  (BSP: idle "task" metadata — entry VA, not enqueued)
/// - `72` : idle_stack_top  u64  (BSP: idle metadata — the AP's stack top)
/// - `80` : idle_cr3        u64  (BSP: idle metadata — kernel CR3 the AP idles on)
/// - `88` : idle_flags      u32  (see `idle_flag`)
/// - `92` : _pad_idle       u32
/// - `96` : irq_hit_count     u32 (AP IDT smoke handler: gs:[96] += 1)
/// - `100`: irq_hit_vector    u32 (AP IDT smoke handler: vector delivered)
/// - `104`: irq_unexpected_vec u32 (catch-all park stub: vector+1; 0 = none)
/// - `108`: remote_wake_count u32 (AP wake stub, vector 0xF1: gs:[108] += 1)
/// - `112`: irq_stage        u32 (smoke handler sub-stage: enter/EOI/iret)
/// - `116`: irq_ack          u32 (persistent post-resume ACK; 1 = resumed+acked)
/// - `120`: sched_stage      u32 (AP mirror of the sched-idle stage 30/31 via gs:)
/// - `124`: wake_reenter_mirror u32 (AP mirror of wake_reenter_out via gs:)
/// - `128`: tlb_req_gen      u32 (BSP: shootdown request generation)
/// - `132`: tlb_ack_gen      u32 (AP:  shootdown ack generation, via gs:)
/// - `136`: tlb_req_va       u64 (BSP: shootdown VA; 0 = full flush)
///
/// Explicit-field bytes = 144; struct stride = 192 (64-byte aligned).
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
    pub env_canary: u32,
    _pad_canary: u32,
    pub saved_rsp: u64,
    pub idle_entry: u64,
    pub idle_stack_top: u64,
    pub idle_cr3: u64,
    pub idle_flags: u32,
    _pad_idle: u32,
    pub irq_hit_count: u32,
    pub irq_hit_vector: u32,
    pub irq_unexpected_vec: u32,
    pub remote_wake_count: u32,
    pub irq_stage: u32,
    pub irq_ack: u32,
    pub sched_stage: u32,
    pub wake_reenter_mirror: u32,
    pub tlb_req_gen: u32,
    pub tlb_ack_gen: u32,
    pub tlb_req_va: u64,
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
            env_canary: 0,
            _pad_canary: 0,
            saved_rsp: 0,
            idle_entry: 0,
            idle_stack_top: 0,
            idle_cr3: 0,
            idle_flags: 0,
            _pad_idle: 0,
            irq_hit_count: 0,
            irq_hit_vector: 0,
            irq_unexpected_vec: 0,
            remote_wake_count: 0,
            irq_stage: 0,
            irq_ack: 0,
            sched_stage: 0,
            wake_reenter_mirror: 0,
            tlb_req_gen: 0,
            tlb_ack_gen: 0,
            tlb_req_va: 0,
        }
    }
}

// Stage 183 inc.3 layout guard: the AP entry asm writes `gs:[48]` (env_canary) and
// `gs:[56]` (saved_rsp) with GS base == the record base; lock the offsets so a field
// reorder can never silently move the asm's targets.
const _: () = {
    assert!(core::mem::offset_of!(PerCpuRecord, env_canary) == ENV_CANARY_OFFSET);
    assert!(core::mem::offset_of!(PerCpuRecord, saved_rsp) == SAVED_RSP_OFFSET);
    assert!(core::mem::offset_of!(PerCpuRecord, irq_hit_count) == IRQ_HIT_COUNT_OFFSET);
    assert!(core::mem::offset_of!(PerCpuRecord, irq_hit_vector) == IRQ_HIT_VECTOR_OFFSET);
    assert!(core::mem::offset_of!(PerCpuRecord, irq_unexpected_vec) == IRQ_UNEXPECTED_VEC_OFFSET);
    assert!(core::mem::offset_of!(PerCpuRecord, remote_wake_count) == REMOTE_WAKE_COUNT_OFFSET);
    assert!(core::mem::offset_of!(PerCpuRecord, irq_stage) == IRQ_STAGE_OFFSET);
    assert!(core::mem::offset_of!(PerCpuRecord, irq_ack) == IRQ_ACK_OFFSET);
    assert!(core::mem::offset_of!(PerCpuRecord, sched_stage) == SCHED_STAGE_OFFSET);
    assert!(core::mem::offset_of!(PerCpuRecord, wake_reenter_mirror) == WAKE_REENTER_MIRROR_OFFSET);
    assert!(core::mem::offset_of!(PerCpuRecord, tlb_req_gen) == TLB_REQ_GEN_OFFSET);
    assert!(core::mem::offset_of!(PerCpuRecord, tlb_ack_gen) == TLB_ACK_GEN_OFFSET);
    assert!(core::mem::offset_of!(PerCpuRecord, tlb_req_va) == TLB_REQ_VA_OFFSET);
    assert!(core::mem::size_of::<PerCpuRecord>() == 192);
};

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
            env_canary: 0,
            _pad_canary: 0,
            saved_rsp: 0,
            idle_entry: 0,
            idle_stack_top: 0,
            idle_cr3: 0,
            idle_flags: 0,
            _pad_idle: 0,
            irq_hit_count: 0,
            irq_hit_vector: 0,
            irq_unexpected_vec: 0,
            remote_wake_count: 0,
            irq_stage: 0,
            irq_ack: 0,
            sched_stage: 0,
            wake_reenter_mirror: 0,
            tlb_req_gen: 0,
            tlb_ack_gen: 0,
            tlb_req_va: 0,
        };
        core::ptr::write_volatile(base, record);
    }
}

/// Stage 183 inc.3: BSP records the AP idle-task METADATA (entry / stack / CR3) and the
/// per-CPU TSS pointer before SIPI. This is a reserved, validated description of the AP
/// idle context — NOT a scheduler task; nothing is enqueued and the scheduler never
/// selects an AP. MUST be called before the AP runs (the AP later writes `env_canary` /
/// `saved_rsp` into the same record via gs:, so BSP writes may not race it afterwards).
pub fn set_idle_task_meta(cpu: CpuId, idle_entry: u64, idle_stack_top: u64, idle_cr3: u64) {
    let base = record_base(cpu) as *mut PerCpuRecord;
    unsafe {
        let mut record = core::ptr::read_volatile(base);
        record.idle_entry = idle_entry;
        record.idle_stack_top = idle_stack_top;
        record.idle_cr3 = idle_cr3;
        record.idle_flags |= idle_flag::IDLE_TASK_META_SET;
        core::ptr::write_volatile(base, record);
    }
}

/// Stage 183 inc.3: BSP records the AP-local TSS VA in the per-CPU record.
/// Same before-SIPI-only contract as `set_idle_task_meta`.
pub fn set_tss_ptr(cpu: CpuId, tss_ptr: u64) {
    let base = record_base(cpu) as *mut PerCpuRecord;
    unsafe {
        let mut record = core::ptr::read_volatile(base);
        record.tss_ptr = tss_ptr;
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

/// Stage 183.6: BSP posts a TLB shootdown request for `cpu` (VA `va`; 0 = full
/// flush) and returns the new request generation. Writes `tlb_req_va` BEFORE
/// bumping `tlb_req_gen` so the AP — which reads gen first, then va — always sees
/// the matching VA. Field-granular `write_volatile` (not a full-record write) so
/// it never clobbers the AP-owned `tlb_ack_gen`.
pub fn tlb_request_shootdown(cpu: CpuId, va: u64) -> u32 {
    let base = record_base(cpu) as *mut PerCpuRecord;
    unsafe {
        let cur_gen = core::ptr::read_volatile(core::ptr::addr_of!((*base).tlb_req_gen));
        let next = cur_gen.wrapping_add(1);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*base).tlb_req_va), va);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*base).tlb_req_gen), next);
        next
    }
}

/// Stage 183.6: BSP reads the AP's TLB shootdown ACK generation for `cpu`.
pub fn tlb_ack_gen(cpu: CpuId) -> u32 {
    let base = record_base(cpu) as *const PerCpuRecord;
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*base).tlb_ack_gen)) }
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
        // Stage 183 inc.3: offsets the AP entry asm writes via gs:[..] + idle metadata.
        assert_eq!(core::ptr::addr_of!(record.env_canary) as usize - base, 48);
        assert_eq!(core::ptr::addr_of!(record.saved_rsp) as usize - base, 56);
        assert_eq!(core::ptr::addr_of!(record.idle_entry) as usize - base, 64);
        assert_eq!(
            core::ptr::addr_of!(record.idle_stack_top) as usize - base,
            72
        );
        assert_eq!(core::ptr::addr_of!(record.idle_cr3) as usize - base, 80);
        assert_eq!(core::ptr::addr_of!(record.idle_flags) as usize - base, 88);
    }

    #[test]
    fn set_idle_task_meta_records_metadata_without_enqueue() {
        init_record_for_ap(CpuId(5), 5, 0x1234_0000);
        set_idle_task_meta(CpuId(5), 0xFFFF_FFFF_8000_0000, 0x1234_0000, 0x0010_0000);
        set_tss_ptr(CpuId(5), 0xFFFF_FF80_0042_0000);
        let record = read_record(CpuId(5));
        assert_eq!(record.idle_entry, 0xFFFF_FFFF_8000_0000);
        assert_eq!(record.idle_stack_top, 0x1234_0000);
        assert_eq!(record.idle_cr3, 0x0010_0000);
        assert_eq!(record.tss_ptr, 0xFFFF_FF80_0042_0000);
        assert_eq!(
            record.idle_flags & idle_flag::IDLE_TASK_META_SET,
            idle_flag::IDLE_TASK_META_SET
        );
        // Metadata only: the record init flag is preserved and the AP-written
        // fields stay zero until the AP itself publishes them.
        assert_eq!(
            record.flags & flag::RECORD_INITIALIZED,
            flag::RECORD_INITIALIZED
        );
        assert_eq!(record.env_canary, 0);
        assert_eq!(record.saved_rsp, 0);
    }

    #[test]
    fn percpu_record_size_and_alignment() {
        // Stage 183.6: 144 bytes of explicit fields (through the TLB shootdown
        // mailbox at 128/132/136) round up to a 192-byte stride at 64-byte align.
        assert_eq!(PerCpuRecord::SIZE, 192);
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
