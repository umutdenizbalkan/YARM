// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-IRQ2 — the feature-gated PL011 setup/drain FIXTURE for the AArch64 external-device
//! interrupt witness (`aarch64-pl011-irq-witness`).
//!
//! # What this module is, and what it is not
//!
//! It is the smallest amount of device knowledge the witness needs: it reads the PL011's base and
//! GIC specifier from the executed machine's DTB, configures ONE shared peripheral interrupt on
//! the existing GICv2 in a fixed order, drains the PL011 receive FIFO for a claim the production
//! vector entry already took, and turns the source off again after a bounded number of items. It
//! is not a UART driver and has no framework.
//!
//! It owns NO part of the interrupt path. The claim is `GICC_IAR`, read once by
//! `yarm_aarch64_vector_entry` and carried as a `GicAck`; delivery is the production route
//! (`dispatch_trap_entry_with_shared_kernel` → `settle_external_interrupt_at_bridge` →
//! `deliver_external_irq_split` → `irq_routes` → `NotificationObject::send_irq`); the one
//! completion is the vector tail's `complete_interrupt(ack)`. This module is only CALLED from
//! three points on that path — before delivery (record the origin, drain the device), after the
//! completion write (count it, observe the deactivation) and for a special INTID (count it) — and
//! none of them can change what the path does with the claim.
//!
//! # Ordering, and why
//!
//! Enable, run from `start_bsp_periodic_timer` immediately BEFORE its `DAIF.I` unmask: mappings
//! and handler readiness (read from the live translation with `AT`, never by touching the device)
//! → the notification route is already bound → controller configuration (group, priority,
//! target, level trigger), each read back → UART configuration and stale-state clearing → source
//! enablement (`UARTIMSC.RXIM`) → CPU admission (`GICD_ISENABLER`), and the PE-level unmask stays
//! last, where the timer bring-up already had it.
//!
//! Drain before completion: the PL011's receive interrupt is a LEVEL that stays asserted while
//! the FIFO holds data. Writing `GICC_EOIR` first would deactivate an interrupt whose line is
//! still high, and the distributor would re-present it at once. Draining first makes one byte one
//! claim one completion, and the post-EOI distributor state is read back to prove it.
//!
//! Bytes are not interrupt counts. Drained bytes go to the IRQ1 ring page layout the receiver
//! maps read-only; the receiver checks the DATA sequence there and the claim/completion
//! accounting separately.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use super::irq::GicAck;

/// How many data-bearing items the witness exchanges before the source is turned off.
pub const WITNESS_ITEMS: u32 = 8;

/// Upper bound on bytes drained for one claim. The exchange keeps one byte outstanding, so a
/// drain that reaches this bound is itself a finding.
const MAX_DRAIN_PER_CLAIM: usize = 16;

// PL011 (ARM PrimeCell UART) register offsets and bits.
const UART_DR: usize = 0x000;
const UART_FR: usize = 0x018;
const UART_CR: usize = 0x030;
const UART_IMSC: usize = 0x038;
const UART_RIS: usize = 0x03c;
const UART_MIS: usize = 0x040;
const UART_ICR: usize = 0x044;
const UART_FR_RXFE: u32 = 1 << 4;
const UART_CR_UARTEN: u32 = 1 << 0;
const UART_CR_RXE: u32 = 1 << 9;
const UART_IMSC_RXIM: u32 = 1 << 4;
const UART_ICR_ALL: u32 = 0x7ff;

// GICv2 distributor / CPU interface offsets.
const GICD_TYPER: usize = 0x004;
const GICD_IGROUPR: usize = 0x080;
const GICD_ISENABLER: usize = 0x100;
const GICD_ICENABLER: usize = 0x180;
const GICD_ISPENDR: usize = 0x200;
const GICD_ICPENDR: usize = 0x280;
const GICD_ISACTIVER: usize = 0x300;
const GICD_IPRIORITYR: usize = 0x400;
const GICD_ITARGETSR: usize = 0x800;
const GICD_ICFGR: usize = 0xc00;
const GICC_RPR: usize = 0x014;
/// Below the timer PPI's 0x00 and well inside `GICC_PMR = 0xff`.
const SOURCE_PRIORITY: u32 = 0x80;
const CPU0_TARGET: u32 = 0x01;
/// `GICC_RPR` with nothing active: the idle running priority.
const GICC_RPR_IDLE: u32 = 0xff;

/// `MAIR_EL1` attribute byte for AttrIdx 3 on this port (Device-nGnRE), as `PAR_EL1.ATTR`
/// reports it after a successful `AT`.
const PAR_ATTR_DEVICE_NGNRE: u64 = 0x04;

// Ring page layout, in u32 words — IRQ1's layout, unchanged, so one receiver reads both ports.
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
/// The interrupt line (here: the GIC INTID) the provisioning bound to the receiver's notification.
pub const RING_WORD_BOUND_LINE: usize = 9;
/// Byte offset of the drained-byte log inside the ring page.
pub const RING_BYTES_OFFSET: usize = 256;
pub const RING_BYTES_CAPACITY: usize = 1024;

static UART_PA: AtomicUsize = AtomicUsize::new(0);
static INTID: AtomicU32 = AtomicU32::new(0);
static LEVEL_TRIGGERED: AtomicBool = AtomicBool::new(false);
static SOURCE_ENABLED: AtomicBool = AtomicBool::new(false);
static SOURCE_DISABLED: AtomicBool = AtomicBool::new(false);
static RING_PA: AtomicU64 = AtomicU64::new(0);

static CLAIMS: AtomicU32 = AtomicU32::new(0);
static DATA_CLAIMS: AtomicU32 = AtomicU32::new(0);
static EMPTY_DRAINS: AtomicU32 = AtomicU32::new(0);
static COMPLETIONS: AtomicU32 = AtomicU32::new(0);
static DEACTIVATED: AtomicU32 = AtomicU32::new(0);
static BYTES: AtomicU32 = AtomicU32::new(0);
static IDLE_ORIGIN: AtomicU32 = AtomicU32::new(0);
static USER_ORIGIN: AtomicU32 = AtomicU32::new(0);
static OTHER_ORIGIN: AtomicU32 = AtomicU32::new(0);
static FOREIGN_CLAIMS: AtomicU32 = AtomicU32::new(0);
static SPECIAL_CLAIMS: AtomicU32 = AtomicU32::new(0);
/// Set between the drain of a data-bearing claim and its completion, so the completion that
/// answers the WITNESS_ITEMS-th data item is the one that turns the source off.
static COMPLETION_OWED_FOR_DATA: AtomicBool = AtomicBool::new(false);

// ── Translation-state readiness, from the live `TTBR0_EL1` ─────────────────────────────────────

fn at_s1e1r(va: usize) -> u64 {
    let par: u64;
    // SAFETY: an address-translation query. It reads the page tables only; the device is never
    // touched, whatever the answer.
    unsafe {
        core::arch::asm!(
            "at s1e1r, {va}",
            "isb",
            "mrs {par}, par_el1",
            va = in(reg) va,
            par = out(reg) par,
            options(nostack, preserves_flags)
        );
    }
    par
}

fn at_s1e0r(va: usize) -> u64 {
    let par: u64;
    // SAFETY: as `at_s1e1r`, asked for an EL0 read.
    unsafe {
        core::arch::asm!(
            "at s1e0r, {va}",
            "isb",
            "mrs {par}, par_el1",
            va = in(reg) va,
            par = out(reg) par,
            options(nostack, preserves_flags)
        );
    }
    par
}

/// The live translation answer for one device page: EL1 reaches it as Device-nGnRE at the same
/// physical address, and EL0 does not reach it at all.
#[derive(Clone, Copy)]
struct PageAccess {
    kernel_device: bool,
    user_denied: bool,
    par_el1: u64,
}

fn page_access(pa: usize) -> PageAccess {
    let page = pa & !0xfff;
    let par = at_s1e1r(page);
    let kernel_device = par & 1 == 0
        && (par >> 56) == PAR_ATTR_DEVICE_NGNRE
        && (par & 0x0000_ffff_ffff_f000) as usize == page;
    let user_denied = at_s1e0r(page) & 1 == 1;
    PageAccess {
        kernel_device,
        user_denied,
        par_el1: par,
    }
}

/// `true` iff the kernel may touch this device register now — the page resolves, under the
/// translation this code is running on, to a Device-nGnRE kernel mapping of that very address.
fn reachable(pa: usize) -> bool {
    page_access(pa).kernel_device
}

// ── Raw register access (only ever after `reachable`) ─────────────────────────────────────────

fn read32(pa: usize) -> u32 {
    // SAFETY: identity-mapped Device-nGnRE kernel leaf, checked by the caller.
    unsafe { core::ptr::read_volatile(pa as *const u32) }
}

fn write32(pa: usize, value: u32) {
    // SAFETY: as `read32`.
    unsafe { core::ptr::write_volatile(pa as *mut u32, value) }
}

fn byte_lane_rmw(word_pa: usize, lane: usize, value: u32) -> u32 {
    let shift = (lane * 8) as u32;
    let old = read32(word_pa);
    write32(
        word_pa,
        (old & !(0xff << shift)) | ((value & 0xff) << shift),
    );
    (read32(word_pa) >> shift) & 0xff
}

fn bit_word(base: usize, bank: usize, intid: u32) -> (usize, u32) {
    (
        base + bank + (intid as usize / 32) * 4,
        1u32 << (intid % 32),
    )
}

// ── Ring page ─────────────────────────────────────────────────────────────────────────────────

/// Record the ring page the provisioning allocated, and the line it bound. Called once, during
/// `bootstrap_first_user_task`, before the source can be enabled.
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
    // The ring page is RAM, identity-mapped for the kernel in every root.
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

// ── Configuration from the executed machine ──────────────────────────────────────────────────

/// Read the PL011's base and interrupt from the boot DTB, while `prepare_arch_boot` still holds
/// it. The specifier is translated to the GIC INTID here, once; everything after works in INTIDs.
pub fn capture_from_dtb(dtb: &[u8]) {
    let base = crate::arch::fdt::find_node_reg_by_name_prefix(dtb, b"pl011@");
    let irq = crate::arch::fdt::find_node_gic_interrupt_by_name_prefix(dtb, b"pl011@");
    let (Some((base, _size)), Some(irq)) = (base, irq) else {
        crate::yarm_log!("IRQ2_PL011_DTB result=missing");
        return;
    };
    UART_PA.store(base as usize, Ordering::Release);
    INTID.store(irq.intid as u32, Ordering::Release);
    LEVEL_TRIGGERED.store(irq.level_triggered, Ordering::Release);
    crate::yarm_log!(
        "IRQ2_PL011_DTB base=0x{:x} spi={} intid={} trigger={} level={}",
        base,
        irq.intid.saturating_sub(32),
        irq.intid,
        irq.trigger,
        irq.level_triggered as u8
    );
}

/// The GIC INTID the DTB names for the PL011 — the identity `GICC_IAR` will report, and so the
/// line the route must be bound to. `None` when the DTB named none.
pub fn witness_intid() -> Option<u16> {
    match INTID.load(Ordering::Acquire) {
        0 => None,
        v => u16::try_from(v).ok(),
    }
}

fn deferred(reason: &'static str) -> bool {
    crate::yarm_log!("IRQ2_PL011_ENABLE_DEFERRED reason={}", reason);
    false
}

/// Steps 1–6 of the enable order, run immediately before the PE-level unmask. Returns `true` once
/// the source is live; any failed step leaves it off and says which.
pub fn enable_source_before_unmask() -> bool {
    let intid = INTID.load(Ordering::Acquire);
    let uart = UART_PA.load(Ordering::Acquire);
    if intid < 32 || uart == 0 {
        return deferred("dtb_pl011_missing_or_not_spi");
    }
    if !LEVEL_TRIGGERED.load(Ordering::Acquire) {
        return deferred("dtb_trigger_not_level");
    }
    let (dist_pa, cpu_if_pa) = super::page_table::gic_mmio_bases();
    let (dist, cpu_if) = (dist_pa as usize, cpu_if_pa as usize);
    if dist == 0 || cpu_if == 0 {
        return deferred("gic_bases_unpublished");
    }

    // ── 1. Mappings and handler readiness, from the live translation ──────────────────────────
    let pages = [("uart", uart), ("gicd", dist), ("gicc", cpu_if)];
    for (name, pa) in pages {
        let a = page_access(pa);
        crate::yarm_log!(
            "IRQ2_PL011_MMIO_ACCESS page={} pa=0x{:x} el1_device={} el0_denied={} par_el1=0x{:x}",
            name,
            pa,
            a.kernel_device as u8,
            a.user_denied as u8,
            a.par_el1
        );
        if !a.kernel_device {
            return deferred("mmio_not_kernel_device_under_active_ttbr0");
        }
        if !a.user_denied {
            return deferred("mmio_reachable_from_el0");
        }
    }
    if !super::boot::vector_table_is_installed()
        || !super::boot::trap_shared_kernel_is_installed()
        || !super::irq::controller_configured()
    {
        return deferred("handler_not_ready");
    }
    let lines = ((read32(dist + GICD_TYPER) & 0x1f) + 1) * 32;
    if intid >= lines {
        return deferred("intid_beyond_gicd_typer");
    }

    // ── 2. The route must already be bound through the production owner ───────────────────────
    let route = super::boot::trap_shared_kernel().and_then(|shared| {
        shared.with_ipc_split_mut(|ipc| ipc.irq_routes.get(intid as usize).copied().flatten())
    });
    let Some(notification_idx) = route else {
        return deferred("route_unbound");
    };

    // ── 3. Controller configuration, each write read back ──────────────────────────────────────
    let (group_word, bit) = bit_word(dist, GICD_IGROUPR, intid);
    if read32(group_word) & bit != 0 {
        // The CPU interface enables group 0 only; a group-1 source would never be signalled.
        return deferred("source_not_group0");
    }
    let lane = (intid % 4) as usize;
    let word = (intid as usize / 4) * 4;
    let priority = byte_lane_rmw(dist + GICD_IPRIORITYR + word, lane, SOURCE_PRIORITY);
    let target = byte_lane_rmw(dist + GICD_ITARGETSR + word, lane, CPU0_TARGET);
    let cfg_word = dist + GICD_ICFGR + (intid as usize / 16) * 4;
    let edge_bit = 1u32 << (((intid % 16) * 2) + 1);
    write32(cfg_word, read32(cfg_word) & !edge_bit);
    let level = read32(cfg_word) & edge_bit == 0;
    if priority != SOURCE_PRIORITY || !level {
        return deferred("controller_readback_disagrees");
    }
    // `ITARGETSR` is RAZ/WI on a uniprocessor GICv2; either answer delivers to CPU0.
    if target != CPU0_TARGET && target != 0 {
        return deferred("target_not_cpu0");
    }

    // ── 4. UART: source masked, receiver on, no stale data, no stale cause ─────────────────────
    //
    // The QEMU boot path never programs the PL011 — the console writes `UARTDR` on the reset
    // state, `UARTCR = 0x300` (TXE|RXE, UARTEN clear) — and a PL011 receives only with UARTEN and
    // RXE both set. So the receiver is switched on here, preserving TXE and every other bit. The
    // divisors and `UARTLCR_H` are deliberately left as the console has them: this is the live
    // console, and the QEMU model's receive path does not depend on them (the DTB clock is a
    // fixed 24 MHz `apb_pclk`).
    write32(uart + UART_IMSC, 0);
    let cr_before = read32(uart + UART_CR);
    let rx_on = UART_CR_UARTEN | UART_CR_RXE;
    if cr_before & rx_on != rx_on {
        write32(uart + UART_CR, cr_before | rx_on);
    }
    let cr = read32(uart + UART_CR);
    if cr & rx_on != rx_on {
        return deferred("uart_receiver_readback_disagrees");
    }
    let mut stale = 0u32;
    while read32(uart + UART_FR) & UART_FR_RXFE == 0 && stale < 64 {
        let _ = read32(uart + UART_DR);
        stale += 1;
    }
    write32(uart + UART_ICR, UART_ICR_ALL);
    let (icpend_word, bit) = bit_word(dist, GICD_ICPENDR, intid);
    write32(icpend_word, bit);

    // ── 5. Source enablement ───────────────────────────────────────────────────────────────────
    write32(uart + UART_IMSC, UART_IMSC_RXIM);
    let imsc = read32(uart + UART_IMSC);
    if imsc != UART_IMSC_RXIM {
        write32(uart + UART_IMSC, 0);
        return deferred("uart_imsc_readback_disagrees");
    }

    // ── 6. CPU admission: the distributor forwards the SPI to the CPU interface ───────────────
    let (enable_word, bit) = bit_word(dist, GICD_ISENABLER, intid);
    write32(enable_word, bit);
    let enabled = read32(enable_word) & bit != 0;
    if !enabled {
        write32(uart + UART_IMSC, 0);
        return deferred("gicd_isenabler_readback_disagrees");
    }
    SOURCE_ENABLED.store(true, Ordering::Release);
    ring_store(RING_WORD_ARMED, 1);
    crate::yarm_log!(
        "IRQ2_PL011_SOURCE_ENABLED intid={} spi={} group=0 priority=0x{:x} target=0x{:x} trigger=level uart_cr_before=0x{:x} uart_cr=0x{:x} imsc=0x{:x} isenabler=1 stale_drained={} route_notification={} items={}",
        intid,
        intid - 32,
        priority,
        target,
        cr_before,
        cr,
        imsc,
        stale,
        notification_idx,
        WITNESS_ITEMS
    );
    true
}

// ── The three hooks on the production path ─────────────────────────────────────────────────────

/// Before delivery: record where the claim was taken from, then drain the device.
///
/// Identity comes from the claim, never from any other state. A claim for another INTID, or one
/// taken before the source was enabled, is counted and left to the production path unchanged.
pub fn note_claim_and_drain(
    intid: u16,
    kind: u64,
    spsr_el1: u64,
    elr_el1: u64,
    cpu: crate::kernel::scheduler::CpuId,
) {
    if intid as u32 != INTID.load(Ordering::Acquire) || !SOURCE_ENABLED.load(Ordering::Acquire) {
        FOREIGN_CLAIMS.fetch_add(1, Ordering::AcqRel);
        crate::yarm_log!("IRQ2_PL011_FOREIGN_CLAIM intid={} kind={}", intid, kind);
        return;
    }
    let claim = CLAIMS.fetch_add(1, Ordering::AcqRel) + 1;
    ring_store(RING_WORD_CLAIMS, claim);

    // ── Origin: the vector that took it, and for EL1 whether this CPU was PARKED ──────────────
    // `irq_lower_a64` (10) is EL0. `irq_current_spx` (6) is EL1h, and it is the idle boundary only
    // when the halt loop's publication is still standing (the bridge consumes it after this).
    let parked = crate::kernel::idle_boundary::is_parked(cpu.0 as usize);
    let mode = spsr_el1 & 0xf;
    let (origin, n) = match (kind, mode, parked) {
        (10, 0b0000, _) => {
            let n = USER_ORIGIN.fetch_add(1, Ordering::AcqRel) + 1;
            ring_store(RING_WORD_USER_ORIGIN, n);
            ("user", n)
        }
        (6, 0b0101, true) => {
            let n = IDLE_ORIGIN.fetch_add(1, Ordering::AcqRel) + 1;
            ring_store(RING_WORD_IDLE_ORIGIN, n);
            ("idle", n)
        }
        _ => ("other", OTHER_ORIGIN.fetch_add(1, Ordering::AcqRel) + 1),
    };
    let tid = super::boot::trap_shared_kernel()
        .and_then(|s| s.current_tid_split_read(cpu))
        .unwrap_or(0);
    crate::yarm_log!(
        "IRQ2_PL011_IRQ_ENTRY origin={} n={} claim={} intid={} kind={} spsr=0x{:x} elr=0x{:x} parked={} tid={}",
        origin,
        n,
        claim,
        intid,
        kind,
        spsr_el1,
        elr_el1,
        parked as u8,
        tid
    );

    // ── Drain: the level drops only when the FIFO is empty ────────────────────────────────────
    let uart = UART_PA.load(Ordering::Acquire);
    if !reachable(uart) {
        crate::yarm_log!(
            "IRQ2_PL011_DRAIN claim={} bytes=0 reason=uart_unreachable",
            claim
        );
        return;
    }
    let mis_before = read32(uart + UART_MIS);
    let mut drained = 0usize;
    let mut first = 0u8;
    while drained < MAX_DRAIN_PER_CLAIM && read32(uart + UART_FR) & UART_FR_RXFE == 0 {
        let byte = (read32(uart + UART_DR) & 0xff) as u8;
        if drained == 0 {
            first = byte;
        }
        let index = BYTES.load(Ordering::Acquire) as usize;
        ring_store_byte(index, byte);
        let total = BYTES.fetch_add(1, Ordering::AcqRel) + 1;
        ring_store(RING_WORD_BYTES, total);
        drained += 1;
    }
    let rxfe_after = (read32(uart + UART_FR) & UART_FR_RXFE != 0) as u8;
    let ris_after = read32(uart + UART_RIS);
    let mis_after = read32(uart + UART_MIS);
    if drained == 0 {
        let n = EMPTY_DRAINS.fetch_add(1, Ordering::AcqRel) + 1;
        ring_store(RING_WORD_EMPTY_DRAINS, n);
    } else {
        DATA_CLAIMS.fetch_add(1, Ordering::AcqRel);
        COMPLETION_OWED_FOR_DATA.store(true, Ordering::Release);
    }
    crate::yarm_log!(
        "IRQ2_PL011_DRAIN claim={} intid={} bytes={} first=0x{:02x} mis_before=0x{:x} rxfe_after={} ris_after=0x{:x} mis_after=0x{:x} total_bytes={}",
        claim,
        intid,
        drained,
        first,
        mis_before,
        rxfe_after,
        ris_after,
        mis_after,
        BYTES.load(Ordering::Acquire)
    );
}

/// A special INTID (1020..=1023) was claimed. The vector entry returns without completing it —
/// GICv2 forbids an EOI for one — and this only counts it, so a boot can show whether any
/// spurious acknowledgement happened while the witness source was live.
pub fn note_special_claim(claimed: Option<GicAck>, kind: u64) {
    let Some(ack) = claimed else {
        return;
    };
    let n = SPECIAL_CLAIMS.fetch_add(1, Ordering::AcqRel) + 1;
    crate::yarm_log!(
        "IRQ2_GIC_SPECIAL_CLAIM n={} token=0x{:x} intid={} kind={} completed=0 source_enabled={}",
        n,
        ack.raw(),
        ack.intid(),
        kind,
        SOURCE_ENABLED.load(Ordering::Acquire) as u8
    );
}

/// After the completion owner has WRITTEN `GICC_EOIR` for `ack`: count it, read back what the
/// distributor now says about the source, and turn the source off once the last data item has
/// been completed. Never writes a completion itself.
pub fn note_completion_written(ack: GicAck) {
    let intid = ack.intid() as u32;
    if intid != INTID.load(Ordering::Acquire) {
        return;
    }
    let n = COMPLETIONS.fetch_add(1, Ordering::AcqRel) + 1;
    ring_store(RING_WORD_COMPLETIONS, n);
    let (dist_pa, cpu_if_pa) = super::page_table::gic_mmio_bases();
    let (dist, cpu_if) = (dist_pa as usize, cpu_if_pa as usize);
    let (active, pending, rpr) = if reachable(dist) && reachable(cpu_if) {
        let (aw, bit) = bit_word(dist, GICD_ISACTIVER, intid);
        let (pw, _) = bit_word(dist, GICD_ISPENDR, intid);
        (
            (read32(aw) & bit != 0) as u8,
            (read32(pw) & bit != 0) as u8,
            read32(cpu_if + GICC_RPR) & 0xff,
        )
    } else {
        (0xff, 0xff, 0)
    };
    if active == 0 && pending == 0 && rpr == GICC_RPR_IDLE {
        DEACTIVATED.fetch_add(1, Ordering::AcqRel);
    }
    crate::yarm_log!(
        "IRQ2_PL011_COMPLETE n={} token=0x{:x} intid={} claims={} active_after={} pending_after={} rpr_after=0x{:x}",
        n,
        ack.raw(),
        intid,
        CLAIMS.load(Ordering::Acquire),
        active,
        pending,
        rpr
    );
    if COMPLETION_OWED_FOR_DATA.swap(false, Ordering::AcqRel)
        && DATA_CLAIMS.load(Ordering::Acquire) >= WITNESS_ITEMS
    {
        disable_source();
    }
}

/// Turn the witness source off: device first (`UARTIMSC = 0`, so the line cannot reassert), then
/// the distributor's enable bit, read back. The PE mask is left alone — the timer still needs it —
/// and a late interrupt would still be claimed and completed by the production path.
fn disable_source() {
    if SOURCE_DISABLED.swap(true, Ordering::AcqRel) {
        return;
    }
    let intid = INTID.load(Ordering::Acquire);
    let uart = UART_PA.load(Ordering::Acquire);
    let mut imsc_after = u32::MAX;
    if reachable(uart) {
        write32(uart + UART_IMSC, 0);
        imsc_after = read32(uart + UART_IMSC);
    }
    let (dist_pa, _) = super::page_table::gic_mmio_bases();
    let dist = dist_pa as usize;
    let mut enabled_after = u32::MAX;
    if reachable(dist) {
        let (clear_word, bit) = bit_word(dist, GICD_ICENABLER, intid);
        write32(clear_word, bit);
        let (set_word, _) = bit_word(dist, GICD_ISENABLER, intid);
        enabled_after = (read32(set_word) & bit != 0) as u32;
    }
    ring_store(RING_WORD_DISABLED, 1);
    crate::yarm_log!(
        "IRQ2_PL011_SOURCE_DISABLED intid={} claims={} data_claims={} completions={} deactivated={} bytes={} imsc_after=0x{:x} isenabler_after={}",
        intid,
        CLAIMS.load(Ordering::Acquire),
        DATA_CLAIMS.load(Ordering::Acquire),
        COMPLETIONS.load(Ordering::Acquire),
        DEACTIVATED.load(Ordering::Acquire),
        BYTES.load(Ordering::Acquire),
        imsc_after,
        enabled_after
    );
    crate::yarm_log!(
        "IRQ2_PL011_TOTALS empty_drains={} idle_origin={} user_origin={} other_origin={} foreign_claims={} special_claims={}",
        EMPTY_DRAINS.load(Ordering::Acquire),
        IDLE_ORIGIN.load(Ordering::Acquire),
        USER_ORIGIN.load(Ordering::Acquire),
        OTHER_ORIGIN.load(Ordering::Acquire),
        FOREIGN_CLAIMS.load(Ordering::Acquire),
        SPECIAL_CLAIMS.load(Ordering::Acquire)
    );
}
