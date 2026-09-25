// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-IRQ1 — the feature-gated UART0 setup/drain FIXTURE for the first real external-interrupt
//! witness (`riscv-uart-irq-witness`).
//!
//! # What this module is, and what it is not
//!
//! It is the smallest amount of device knowledge the witness needs and nothing more: it builds
//! the kernel-only device window, enables ONE DTB-identified source in a fixed order, drains the
//! UART receive FIFO for a claim the production entry owner already took, and turns the source
//! off again after a bounded number of items. It is not a UART driver and has no framework.
//!
//! It owns NO part of the interrupt path. The claim is read once by the trap entry owner
//! (`yarm_riscv64_trap_bridge` / the idle-origin entry), carried in `Riscv64TrapContext`, settled
//! by `settle_riscv_external_claim` through the production route
//! (`settle_external_interrupt_at_bridge` → `deliver_external_irq_split` → `irq_routes` →
//! `NotificationObject::send_irq` → waiter wake), and completed by the one completion owner.
//! This module is only CALLED from two points on that path — before delivery, to drain the device
//! so its level drops before the completion is written, and after the completion write, to count
//! it — and neither call can change what the path does with the claim.
//!
//! # Ordering, and why
//!
//! Enable: mappings → claim readiness → route bound → controller configuration (threshold,
//! priority) → source enablement (PLIC enable bit, UART `IER.ERBFI`) → CPU admission (`sie.SEIE`).
//! Each step is re-derived from the hardware tables or kernel state immediately before the next.
//!
//! Drain before completion: the ns16550a keeps its interrupt line asserted while `LSR.DR` is set.
//! The PLIC gateway re-latches a still-asserted level source as soon as the completion is written,
//! so completing before the FIFO is empty is a reassertion storm. Draining first makes one byte
//! one claim one completion.
//!
//! Bytes are not interrupt counts. Drained bytes go to a ring page the receiver maps read-only;
//! the receiver checks the DATA sequence there and the claim/completion accounting separately.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use super::irq;
use super::page_table;

/// How many data-bearing items the witness exchanges before the source is turned off.
pub const WITNESS_ITEMS: u32 = 8;

/// Upper bound on bytes drained for one claim. The exchange keeps one byte outstanding, so a
/// drain that reaches this bound is itself a finding.
const MAX_DRAIN_PER_CLAIM: usize = 16;

// ns16550a register offsets (byte-wide registers, `reg-shift = 0` on QEMU virt).
const UART_RBR: usize = 0;
const UART_IER: usize = 1;
const UART_LSR: usize = 5;
const UART_LSR_DR: u8 = 1 << 0;
const UART_IER_ERBFI: u8 = 1 << 0;

// PLIC register layout (sifive,plic-1.0.0).
const PLIC_PRIORITY_STRIDE: usize = 4;
const PLIC_ENABLE_BASE: usize = 0x2000;
const PLIC_ENABLE_STRIDE: usize = 0x80;
const PLIC_CONTEXT_BASE: usize = 0x0020_0000;
const PLIC_CONTEXT_STRIDE: usize = 0x1000;
const PLIC_CLAIM_OFFSET: usize = 4;

const SIE_SEIE: usize = 1 << 9;

// Ring page layout, in u32 words. The page is mapped read-only into the receiver.
pub const RING_MAGIC: u32 = 0x4952_5131; // "IRQ1"
pub const RING_WORD_MAGIC: usize = 0;
pub const RING_WORD_ARMED: usize = 1;
pub const RING_WORD_DISABLED: usize = 2;
pub const RING_WORD_BYTES: usize = 3;
pub const RING_WORD_CLAIMS: usize = 4;
pub const RING_WORD_COMPLETIONS: usize = 5;
pub const RING_WORD_IDLE_ORIGIN: usize = 6;
pub const RING_WORD_USER_ORIGIN: usize = 7;
pub const RING_WORD_EMPTY_DRAINS: usize = 8;
/// The interrupt line the provisioning bound to the receiver's notification.
pub const RING_WORD_BOUND_LINE: usize = 9;
/// Byte offset of the drained-byte log inside the ring page.
pub const RING_BYTES_OFFSET: usize = 256;
pub const RING_BYTES_CAPACITY: usize = 1024;

static UART_PA: AtomicUsize = AtomicUsize::new(0);
static UART_SOURCE: AtomicU32 = AtomicU32::new(0);
static SOURCE_ENABLED: AtomicBool = AtomicBool::new(false);
static SOURCE_DISABLED: AtomicBool = AtomicBool::new(false);
static RING_PA: AtomicU64 = AtomicU64::new(0);

static CLAIMS: AtomicU32 = AtomicU32::new(0);
static DATA_CLAIMS: AtomicU32 = AtomicU32::new(0);
static EMPTY_DRAINS: AtomicU32 = AtomicU32::new(0);
static COMPLETIONS: AtomicU32 = AtomicU32::new(0);
static BYTES: AtomicU32 = AtomicU32::new(0);
static IDLE_ORIGIN: AtomicU32 = AtomicU32::new(0);
static USER_ORIGIN: AtomicU32 = AtomicU32::new(0);
static OTHER_SOURCE_CLAIMS: AtomicU32 = AtomicU32::new(0);
/// Set between the drain of a data-bearing claim and its completion, so the completion that
/// answers the WITNESS_ITEMS-th data item is the one that turns the source off.
static COMPLETION_OWED_FOR_DATA: AtomicBool = AtomicBool::new(false);

fn marker(args: core::fmt::Arguments<'_>) {
    super::boot::early_sbi_marker(args);
}

/// The source the witness enabled, or `0` before it has been enabled.
pub fn witness_source() -> u32 {
    if SOURCE_ENABLED.load(Ordering::Acquire) {
        UART_SOURCE.load(Ordering::Acquire)
    } else {
        0
    }
}

/// `true` once the witness owner has enabled its source. The idle-origin external-interrupt
/// admission requires it: before any source is enabled, a supervisor external interrupt at the
/// idle boundary has no legitimate producer and stays fail-closed. It stays `true` after the
/// source is turned off, so an interrupt already latched before the disable is still claimed and
/// completed by the production path rather than halting the hart.
pub fn external_admission_armed() -> bool {
    SOURCE_ENABLED.load(Ordering::Acquire)
}

/// Record the ring page the provisioning allocated, and the line it bound. Called once, before
/// the idle safe point.
pub fn set_ring_page(pa: u64, bound_line: u16) {
    RING_PA.store(pa, Ordering::Release);
    ring_store(RING_WORD_BOUND_LINE, bound_line as u32);
    ring_store(RING_WORD_MAGIC, RING_MAGIC);
}

fn ring_store(word: usize, value: u32) {
    let pa = RING_PA.load(Ordering::Acquire);
    if pa == 0 {
        return;
    }
    // The ring page is RAM, inside the identity gigapage every root maps for the kernel.
    unsafe {
        core::ptr::write_volatile((pa as *mut u32).add(word), value);
    }
    core::sync::atomic::fence(Ordering::Release);
}

fn ring_store_byte(index: usize, value: u8) {
    let pa = RING_PA.load(Ordering::Acquire);
    if pa == 0 || index >= RING_BYTES_CAPACITY {
        return;
    }
    unsafe {
        core::ptr::write_volatile((pa as *mut u8).add(RING_BYTES_OFFSET + index), value);
    }
}

fn dtb_uart() -> Option<(usize, u32)> {
    let dtb = super::boot::captured_dtb()?;
    let (base, _size) = crate::arch::fdt::find_node_reg_by_name_prefix(dtb, b"serial@")?;
    let irq = crate::arch::fdt::find_node_interrupt_by_name_prefix(dtb, b"serial@")?;
    Some((base as usize, irq))
}

fn priority_pa(base: usize, source: u32) -> usize {
    base + (source as usize) * PLIC_PRIORITY_STRIDE
}

fn enable_word_pa(base: usize, context: usize, source: u32) -> usize {
    base + PLIC_ENABLE_BASE + context * PLIC_ENABLE_STRIDE + ((source as usize) / 32) * 4
}

fn threshold_pa(base: usize, context: usize) -> usize {
    base + PLIC_CONTEXT_BASE + context * PLIC_CONTEXT_STRIDE
}

const PAGE_MASK: usize = !0xfff;

/// Step 1 — MAPPINGS. Build the device window over exactly the pages the witness touches, from
/// the configured controller context and the DTB's UART node, and point every root at it.
///
/// Refuses, with a marker, when the DTB does not name the UART: the address and the interrupt are
/// read from the executed machine or not used at all.
pub fn install_device_window_at_safe_point() {
    let Some(ctx) = irq::configured_context() else {
        marker(format_args!(
            "IRQ1_DEVICE_WINDOW_REFUSED reason=plic_not_configured"
        ));
        return;
    };
    let Some((uart_pa, uart_irq)) = dtb_uart() else {
        marker(format_args!(
            "IRQ1_DEVICE_WINDOW_REFUSED reason=dtb_has_no_uart_node"
        ));
        return;
    };
    UART_PA.store(uart_pa, Ordering::Release);
    UART_SOURCE.store(uart_irq, Ordering::Release);
    let pages = [
        (priority_pa(ctx.base, uart_irq) & PAGE_MASK) as u64,
        (enable_word_pa(ctx.base, ctx.context_index, uart_irq) & PAGE_MASK) as u64,
        (threshold_pa(ctx.base, ctx.context_index) & PAGE_MASK) as u64,
        (uart_pa & PAGE_MASK) as u64,
    ];
    match page_table::install_device_window(&pages) {
        Ok(n) => {
            marker(format_args!(
                "IRQ1_DEVICE_WINDOW_INSTALLED va=0x{:x} slot={} pages={} flags=0x{:x}",
                page_table::DEVICE_WINDOW_BASE,
                page_table::DEVICE_WINDOW_ROOT_SLOT,
                n,
                page_table::DEVICE_WINDOW_LEAF_FLAGS
            ));
            marker(format_args!(
                "IRQ1_DEVICE_WINDOW_PAGES prio=0x{:x} enable=0x{:x} context=0x{:x} uart=0x{:x}",
                pages[0], pages[1], pages[2], pages[3]
            ));
            // Isolation, stated from the same facts the kernel enforces: the window lies above
            // every address a user mapping may name, and its leaves carry no USER bit.
            let user_va_admissible =
                crate::kernel::vm::VirtAddr(page_table::DEVICE_WINDOW_BASE).is_user();
            marker(format_args!(
                "IRQ1_DEVICE_WINDOW_ISOLATION user_va_admissible={} leaf_user={} leaf_exec={} dtb_uart=0x{:x} dtb_uart_irq={}",
                user_va_admissible as u8,
                (page_table::DEVICE_WINDOW_LEAF_FLAGS & page_table::PageTableEntry::USER != 0)
                    as u8,
                (page_table::DEVICE_WINDOW_LEAF_FLAGS & page_table::PageTableEntry::EXECUTE != 0)
                    as u8,
                uart_pa,
                uart_irq
            ));
        }
        Err(e) => marker(format_args!("IRQ1_DEVICE_WINDOW_REFUSED reason={:?}", e)),
    }
}

fn mmio_va(pa: usize, len: usize) -> Option<usize> {
    super::plic::mmio_va_under_active_satp(pa, len)
}

fn read8(va: usize) -> u8 {
    unsafe { core::ptr::read_volatile(va as *const u8) }
}

fn write8(va: usize, value: u8) {
    unsafe { core::ptr::write_volatile(va as *mut u8, value) }
}

fn read32(va: usize) -> u32 {
    unsafe { core::ptr::read_volatile(va as *const u32) }
}

fn write32(va: usize, value: u32) {
    unsafe { core::ptr::write_volatile(va as *mut u32, value) }
}

fn deferred(reason: &'static str) -> Option<&'static str> {
    marker(format_args!("RISCV_EXTIRQ_DEFERRED reason={}", reason));
    Some(reason)
}

/// Steps 2–5 — readiness, controller configuration, source enablement, CPU admission, in that
/// order. Returns the deferral reason, or `None` once the source is live.
pub fn enable_source_after_plic_ready(
    claim_reachable: bool,
    candidate_source: u16,
) -> Option<&'static str> {
    let Some(ctx) = irq::configured_context() else {
        return deferred("witness_plic_not_configured");
    };
    let source = UART_SOURCE.load(Ordering::Acquire);
    let uart_pa = UART_PA.load(Ordering::Acquire);
    if source == 0 || uart_pa == 0 {
        return deferred("witness_dtb_uart_missing");
    }
    // The DTB is the authority on the source; the candidate named by the platform table must agree
    // or nothing is enabled.
    if source != candidate_source as u32 {
        marker(format_args!(
            "IRQ1_UART_SOURCE_MISMATCH dtb={} candidate={}",
            source, candidate_source
        ));
        return deferred("witness_dtb_source_mismatch");
    }
    // ── Readiness, re-derived for EVERY register this fixture and the claim path touch ──────────
    let claim_pa = threshold_pa(ctx.base, ctx.context_index) + PLIC_CLAIM_OFFSET;
    let regs = (
        mmio_va(priority_pa(ctx.base, source), 4),
        mmio_va(enable_word_pa(ctx.base, ctx.context_index, source), 4),
        mmio_va(threshold_pa(ctx.base, ctx.context_index), 4),
        mmio_va(claim_pa, 4),
        mmio_va(uart_pa + UART_RBR, 1),
        mmio_va(uart_pa + UART_IER, 1),
        mmio_va(uart_pa + UART_LSR, 1),
    );
    let (
        Some(prio_va),
        Some(enable_va),
        Some(thresh_va),
        Some(claim_va),
        Some(rbr_va),
        Some(ier_va),
        Some(lsr_va),
    ) = regs
    else {
        return deferred("witness_mmio_unreachable_under_active_satp");
    };
    if !claim_reachable {
        return deferred("witness_claim_unreachable");
    }
    // ── The route must already be bound through the production owner ───────────────────────────
    let route = super::boot::trap_shared_kernel_riscv().and_then(|shared| {
        shared.with_ipc_split_mut(|ipc| ipc.irq_routes.get(source as usize).copied().flatten())
    });
    let Some(notification_idx) = route else {
        return deferred("witness_route_unbound");
    };
    marker(format_args!(
        "IRQ1_UART_READINESS source={} context={} claim_va=0x{:x} uart_va=0x{:x} route_notification={} satp_walk=ok",
        source, ctx.context_index, claim_va, rbr_va, notification_idx
    ));

    // ── Controller configuration ───────────────────────────────────────────────────────────────
    write32(thresh_va, 0);
    write32(prio_va, 1);
    // ── Source enablement: first empty the device so nothing stale is claimed as item 1 ───────
    let mut stale = 0u32;
    while read8(lsr_va) & UART_LSR_DR != 0 && stale < 64 {
        let _ = read8(rbr_va);
        stale += 1;
    }
    let bit = 1u32 << (source % 32);
    write32(enable_va, read32(enable_va) | bit);
    write8(ier_va, UART_IER_ERBFI);
    SOURCE_ENABLED.store(true, Ordering::Release);
    // ── CPU admission, last ────────────────────────────────────────────────────────────────────
    unsafe {
        core::arch::asm!("csrs sie, {0}", in(reg) SIE_SEIE, options(nomem, nostack));
    }
    ring_store(RING_WORD_ARMED, 1);
    marker(format_args!(
        "RISCV_EXTIRQ_SMOKE_OK source={} context={} priority={} threshold={} enable_word=0x{:x} ier=0x{:x} seie=1 stale_drained={} mode=witness",
        source,
        ctx.context_index,
        read32(prio_va),
        read32(thresh_va),
        read32(enable_va),
        read8(ier_va),
        stale
    ));
    marker(format_args!(
        "IRQ1_UART_SOURCE_ENABLED source={} context={} items={}",
        source, ctx.context_index, WITNESS_ITEMS
    ));
    None
}

/// Record where a claimed witness interrupt was taken from. Called by the entry owner, which
/// alone knows the origin; the claim itself is not touched.
pub fn note_claim_origin(idle: bool, sepc: usize, tid: u64) {
    let n = if idle {
        let n = IDLE_ORIGIN.fetch_add(1, Ordering::AcqRel) + 1;
        ring_store(RING_WORD_IDLE_ORIGIN, n);
        n
    } else {
        let n = USER_ORIGIN.fetch_add(1, Ordering::AcqRel) + 1;
        ring_store(RING_WORD_USER_ORIGIN, n);
        n
    };
    marker(format_args!(
        "IRQ1_UART_IRQ_ENTRY origin={} n={} sepc=0x{:x} tid={}",
        if idle { "idle" } else { "user" },
        n,
        sepc,
        tid
    ));
}

/// Drain the device for a claim the entry owner already took, BEFORE delivery and completion.
///
/// Acts only on the claim's own source — identity comes from the claim, never from the trap's
/// `stval` — and only while the witness source is enabled. Any other claimed source is counted
/// and left to the production path unchanged.
pub fn drain_before_delivery(source: u32, context_index: usize) {
    if source != UART_SOURCE.load(Ordering::Acquire) || !SOURCE_ENABLED.load(Ordering::Acquire) {
        OTHER_SOURCE_CLAIMS.fetch_add(1, Ordering::AcqRel);
        marker(format_args!(
            "IRQ1_UART_FOREIGN_CLAIM source={} context={} drained=0",
            source, context_index
        ));
        return;
    }
    let claim = CLAIMS.fetch_add(1, Ordering::AcqRel) + 1;
    ring_store(RING_WORD_CLAIMS, claim);
    let uart_pa = UART_PA.load(Ordering::Acquire);
    let (Some(rbr_va), Some(lsr_va)) = (mmio_va(uart_pa + UART_RBR, 1), mmio_va(uart_pa + UART_LSR, 1))
    else {
        marker(format_args!(
            "IRQ1_UART_DRAIN claim={} source={} bytes=0 reason=uart_unreachable",
            claim, source
        ));
        return;
    };
    let mut drained = 0usize;
    let mut first = 0u8;
    while drained < MAX_DRAIN_PER_CLAIM && read8(lsr_va) & UART_LSR_DR != 0 {
        let byte = read8(rbr_va);
        if drained == 0 {
            first = byte;
        }
        let index = BYTES.load(Ordering::Acquire) as usize;
        ring_store_byte(index, byte);
        let total = BYTES.fetch_add(1, Ordering::AcqRel) + 1;
        ring_store(RING_WORD_BYTES, total);
        drained += 1;
    }
    let lsr_after = read8(lsr_va);
    if drained == 0 {
        let n = EMPTY_DRAINS.fetch_add(1, Ordering::AcqRel) + 1;
        ring_store(RING_WORD_EMPTY_DRAINS, n);
    } else {
        DATA_CLAIMS.fetch_add(1, Ordering::AcqRel);
        COMPLETION_OWED_FOR_DATA.store(true, Ordering::Release);
    }
    marker(format_args!(
        "IRQ1_UART_DRAIN claim={} source={} context={} bytes={} first=0x{:02x} lsr_dr_after={} total_bytes={}",
        claim,
        source,
        context_index,
        drained,
        first,
        lsr_after & UART_LSR_DR,
        BYTES.load(Ordering::Acquire)
    ));
}

/// Count a completion the completion owner has just WRITTEN, and turn the source off once the
/// last data item has been completed. Never writes a completion itself.
pub fn note_completion_written(source: u32, context_index: usize) {
    if source != UART_SOURCE.load(Ordering::Acquire) {
        return;
    }
    let n = COMPLETIONS.fetch_add(1, Ordering::AcqRel) + 1;
    ring_store(RING_WORD_COMPLETIONS, n);
    marker(format_args!(
        "IRQ1_UART_COMPLETE n={} source={} context={} claims={}",
        n,
        source,
        context_index,
        CLAIMS.load(Ordering::Acquire)
    ));
    if COMPLETION_OWED_FOR_DATA.swap(false, Ordering::AcqRel)
        && DATA_CLAIMS.load(Ordering::Acquire) >= WITNESS_ITEMS
    {
        disable_source();
    }
}

/// Turn the witness source off: device first (`IER = 0`, so the line cannot reassert), then the
/// controller's enable bit. `sie.SEIE` is left as it is — with no source enabled for this
/// context the controller raises nothing, and a late interrupt would still be claimed and
/// completed by the production path.
fn disable_source() {
    if SOURCE_DISABLED.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(ctx) = irq::configured_context() else {
        return;
    };
    let source = UART_SOURCE.load(Ordering::Acquire);
    let uart_pa = UART_PA.load(Ordering::Acquire);
    if let Some(ier_va) = mmio_va(uart_pa + UART_IER, 1) {
        write8(ier_va, 0);
    }
    let mut enable_after = u32::MAX;
    if let Some(enable_va) = mmio_va(enable_word_pa(ctx.base, ctx.context_index, source), 4) {
        write32(enable_va, read32(enable_va) & !(1u32 << (source % 32)));
        enable_after = read32(enable_va);
    }
    ring_store(RING_WORD_DISABLED, 1);
    marker(format_args!(
        "IRQ1_UART_SOURCE_DISABLED source={} claims={} data_claims={} completions={} bytes={} enable_word_after=0x{:x} ier=0",
        source,
        CLAIMS.load(Ordering::Acquire),
        DATA_CLAIMS.load(Ordering::Acquire),
        COMPLETIONS.load(Ordering::Acquire),
        BYTES.load(Ordering::Acquire),
        enable_after
    ));
    marker(format_args!(
        "IRQ1_UART_TOTALS empty_drains={} idle_origin={} user_origin={} foreign_claims={}",
        EMPTY_DRAINS.load(Ordering::Acquire),
        IDLE_ORIGIN.load(Ordering::Acquire),
        USER_ORIGIN.load(Ordering::Acquire),
        OTHER_SOURCE_CLAIMS.load(Ordering::Acquire)
    ));
}
