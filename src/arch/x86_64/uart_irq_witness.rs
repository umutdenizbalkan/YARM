// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-IRQ3 — the feature-gated COM1 setup/drain FIXTURE for the x86_64 external-device interrupt
//! witness (`x86_64-uart-irq-witness`).
//!
//! # What this module is, and what it is not
//!
//! It is the smallest amount of device and controller knowledge the witness needs: it reads the
//! MADT through the PVH RSDP, derives the GSI and I/O APIC for COM1's ISA IRQ, programs that ONE
//! I/O APIC redirection entry to the vector the production decoder maps to the route's line,
//! drains COM1's receive buffer for a claim the production entry already took, and turns the source
//! off again after a bounded number of items. It is not a UART driver and not an interrupt
//! controller framework.
//!
//! It owns NO part of the interrupt path. The CPU delivers the vector through the existing IDT
//! gate into `yarm_x86_common_trap_entry`; `decode_trap_context` maps it to
//! `TrapEvent::ExternalInterrupt(vector - 0x20)`; `settle_external_interrupt_at_bridge` delivers it
//! (`deliver_external_irq_split` → `irq_routes` → `NotificationObject::send_irq`) and writes the
//! ONE LAPIC EOI (`acknowledge_interrupt`, the x86 single step). This module is only CALLED from
//! two points on that path — before delivery (record the origin, drain the device) and after the
//! bridge returns (observe the LAPIC in-service bit and the redirection entry, count the
//! completion) — and neither can change what the path does with the interrupt.
//!
//! # Two obligations, two owners
//!
//! Device acknowledgement: reading RBR until LSR.DR clears withdraws the UART's receive cause —
//! this fixture, before delivery. Controller completion: the LAPIC EOI — the production bridge,
//! once. The source is edge-triggered on an ISA pin, so the I/O APIC owes no EOI of its own; its
//! Remote-IRR bit is read back only to show it never latched.
//!
//! # Ordering
//!
//! Enable, run from `start_bsp_periodic_timer` after the tick is armed: handler and access
//! readiness (LAPIC configured, shared trap state installed, the I/O APIC page resolved in the live
//! CR3 as a present, uncached, supervisor-only mapping, the 8259 input for this IRQ masked) → route
//! bound (by `bootstrap_first_user_task`) → controller configuration (redirection entry written
//! MASKED, read back) → device configuration (IER off, stale state cleared, OUT2) → source
//! enablement (IER.ERBFI) → controller admission (redirection entry unmasked, read back).

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// How many data-bearing items the witness exchanges before the source is turned off.
pub const WITNESS_ITEMS: u32 = 8;
const MAX_DRAIN_PER_CLAIM: usize = 16;

/// COM1's ISA IRQ, as the executed machine configures it: QEMU 8.2.2 `info qtree` on
/// `-machine q35` shows `isa-serial` index 0 at `iobase = 0x3f8`, `irq = 4`. The MADT then decides
/// the GSI, polarity and trigger; nothing below assumes the GSI equals 4.
const COM1_ISA_IRQ: u8 = 4;
/// COM1's port base — the same port the console already owns, confirmed by the `info qtree` above.
const COM1: u16 = 0x3F8;
const UART_RBR: u16 = 0;
const UART_IER: u16 = 1;
const UART_IIR: u16 = 2;
const UART_MCR: u16 = 4;
const UART_LSR: u16 = 5;
const UART_MSR: u16 = 6;
const UART_LSR_DR: u8 = 1 << 0;
const UART_IER_ERBFI: u8 = 1 << 0;
const UART_MCR_OUT2: u8 = 1 << 3;

/// The production decoder: vector 0x20 is the timer and vectors 0x21..0x20+MAX_IRQ_LINES are
/// `ExternalInterrupt(vector - 0x20)`. So line L can only arrive at vector 0x20 + L, L >= 1.
const VEC_EXTERNAL_BASE: u32 = 0x20;
const LAPIC_TIMER_VECTOR: u32 = 0x20;

// I/O APIC indirect registers.
const IOAPIC_IOREGSEL: usize = 0x00;
const IOAPIC_IOWIN: usize = 0x10;
const IOAPIC_REG_VER: u32 = 0x01;
const IOAPIC_REDTBL_BASE: u32 = 0x10;
const REDIR_MASKED: u32 = 1 << 16;
const REDIR_LEVEL: u32 = 1 << 15;
const REDIR_REMOTE_IRR: u32 = 1 << 14;
const REDIR_ACTIVE_LOW: u32 = 1 << 13;
const REDIR_DELIVERY_PENDING: u32 = 1 << 12;

// LAPIC registers (byte offsets).
const LAPIC_ID: usize = 0x20;
const LAPIC_ISR_BASE: usize = 0x100;
const LAPIC_IRR_BASE: usize = 0x200;

// 8259 interrupt mask registers.
const PIC_MASTER_IMR: u16 = 0x21;
const PIC_SLAVE_IMR: u16 = 0xA1;

// Ring page layout, in u32 words — IRQ1's layout, unchanged, so one receiver reads every port.
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
/// The interrupt line (here: the GSI, = vector - 0x20) the provisioning bound to the notification.
pub const RING_WORD_BOUND_LINE: usize = 9;
pub const RING_BYTES_OFFSET: usize = 256;
pub const RING_BYTES_CAPACITY: usize = 1024;

/// The data-bearing claims on which the fixture holds the interrupt (IF clear, EOI not yet
/// written) until the LAPIC timer is ALSO pending, so a device interrupt in service and a timer
/// interrupt pending are exercised together — one from the idle boundary (item 3) and one from
/// user code (item 4). The hold only waits; it changes nothing the path does.
const TIMER_TOGETHER_CLAIMS: [u32; 2] = [3, 4];
/// Bound on LAPIC IRR polls while holding; about three ticks at the QEMU q35 tick rate.
const TOGETHER_POLL_LIMIT: u32 = 40_000_000;

static RSDP_PADDR: AtomicU64 = AtomicU64::new(0);
static GSI: AtomicU32 = AtomicU32::new(0);
static VECTOR: AtomicU32 = AtomicU32::new(0);
static PIN: AtomicU32 = AtomicU32::new(0);
static REDIR_LOW_ENABLED: AtomicU32 = AtomicU32::new(0);
static ROUTE_READY: AtomicBool = AtomicBool::new(false);
static SOURCE_ENABLED: AtomicBool = AtomicBool::new(false);
static SOURCE_DISABLED: AtomicBool = AtomicBool::new(false);
static RING_PA: AtomicU64 = AtomicU64::new(0);

static CLAIMS: AtomicU32 = AtomicU32::new(0);
static DATA_CLAIMS: AtomicU32 = AtomicU32::new(0);
static EMPTY_DRAINS: AtomicU32 = AtomicU32::new(0);
static COMPLETIONS: AtomicU32 = AtomicU32::new(0);
static COMPLETED_CLEAN: AtomicU32 = AtomicU32::new(0);
static BYTES: AtomicU32 = AtomicU32::new(0);
static IDLE_ORIGIN: AtomicU32 = AtomicU32::new(0);
static USER_ORIGIN: AtomicU32 = AtomicU32::new(0);
static OTHER_ORIGIN: AtomicU32 = AtomicU32::new(0);
static TOGETHER_HELD: AtomicU32 = AtomicU32::new(0);
static COMPLETION_OWED_FOR_DATA: AtomicBool = AtomicBool::new(false);
/// The production EOI owner's write count when the current claim entered (see `note_after_dispatch`).
static EOI_AT_ENTRY: AtomicU32 = AtomicU32::new(0);

// ── Port and MMIO access (kernel only) ─────────────────────────────────────────────────────────

fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: ring-0 port read of a device this fixture was built for.
    unsafe {
        core::arch::asm!("in al, dx", in("dx") port, out("al") v, options(nomem, nostack, preserves_flags))
    };
    v
}

fn outb(port: u16, v: u8) {
    // SAFETY: ring-0 port write, as above.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags))
    };
}

fn ioapic_va() -> usize {
    crate::arch::x86_64::platform_layout::IOAPIC_MMIO_BASE
}

fn lapic_va() -> usize {
    crate::arch::x86_64::platform_layout::LAPIC_MMIO_BASE
}

fn mmio_read(va: usize) -> u32 {
    // SAFETY: an uncached kernel mapping of the controller page, checked by `page_facts`.
    unsafe { core::ptr::read_volatile(va as *const u32) }
}

fn mmio_write(va: usize, v: u32) {
    // SAFETY: as `mmio_read`.
    unsafe { core::ptr::write_volatile(va as *mut u32, v) }
}

fn ioapic_read(reg: u32) -> u32 {
    mmio_write(ioapic_va() + IOAPIC_IOREGSEL, reg);
    mmio_read(ioapic_va() + IOAPIC_IOWIN)
}

fn ioapic_write(reg: u32, v: u32) {
    mmio_write(ioapic_va() + IOAPIC_IOREGSEL, reg);
    mmio_write(ioapic_va() + IOAPIC_IOWIN, v);
}

fn lapic_bit(base: usize, vector: u32) -> bool {
    let reg = mmio_read(lapic_va() + base + 0x10 * (vector as usize / 32));
    reg & (1 << (vector % 32)) != 0
}

/// The live translation of one kernel VA, from the active CR3: present, uncached (PCD at the leaf
/// level), and supervisor-only (the U/S bit clear at some level of the walk — a user access needs
/// it set at every level).
#[derive(Clone, Copy)]
struct PageFacts {
    present: bool,
    uncached: bool,
    user_reachable: bool,
    leaf: u64,
    level: u8,
}

fn page_facts(va: usize) -> PageFacts {
    let cr3: u64;
    // SAFETY: reading CR3.
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags))
    };
    let (l4, l3, l2, l1) = crate::arch::x86_64::page_table::hw_pte_walk_verbose(cr3, va as u64);
    const P: u64 = 1;
    const US: u64 = 1 << 2;
    const PCD: u64 = 1 << 4;
    const PS: u64 = 1 << 7;
    let (leaf, level, path): (u64, u8, &[u64]) = if l3 & PS != 0 {
        (l3, 3, &[l4, l3])
    } else if l2 & PS != 0 {
        (l2, 2, &[l4, l3, l2])
    } else {
        (l1, 1, &[l4, l3, l2, l1])
    };
    let present = path.iter().all(|e| e & P != 0);
    PageFacts {
        present,
        uncached: leaf & PCD != 0,
        user_reachable: present && path.iter().all(|e| e & US != 0),
        leaf,
        level,
    }
}

// ── Ring page ─────────────────────────────────────────────────────────────────────────────────

pub fn set_ring_page(pa: u64, bound_line: u16) {
    RING_PA.store(pa, Ordering::Release);
    ring_store(RING_WORD_BOUND_LINE, bound_line as u32);
    ring_store(RING_WORD_MAGIC, RING_MAGIC);
}

fn ring_ptr(offset: usize) -> Option<*mut u8> {
    let pa = RING_PA.load(Ordering::Acquire);
    if pa == 0 {
        return None;
    }
    crate::kernel::boot::KernelState::phys_to_direct_map_ptr(pa + offset as u64)
}

fn ring_store(word: usize, value: u32) {
    if let Some(p) = ring_ptr(word * 4) {
        // SAFETY: the ring page is RAM the provisioning allocated; the direct map covers it.
        unsafe { core::ptr::write_volatile(p as *mut u32, value) };
        core::sync::atomic::fence(Ordering::Release);
    }
}

fn ring_store_byte(index: usize, value: u8) {
    if index >= RING_BYTES_CAPACITY {
        return;
    }
    if let Some(p) = ring_ptr(RING_BYTES_OFFSET + index) {
        // SAFETY: as `ring_store`.
        unsafe { core::ptr::write_volatile(p, value) };
    }
}

// ── Firmware tables ────────────────────────────────────────────────────────────────────────────

/// Record the PVH start_info's RSDP address. Called once from the PVH boot-data capture.
pub fn capture_rsdp(paddr: u64) {
    RSDP_PADDR.store(paddr, Ordering::Release);
}

fn phys_slice(pa: u64, len: usize) -> Option<&'static [u8]> {
    if pa == 0 || len == 0 || len > 64 * 1024 {
        return None;
    }
    let p = crate::kernel::boot::KernelState::phys_to_direct_map_ptr(pa)?;
    crate::kernel::boot::KernelState::phys_to_direct_map_ptr(pa + len as u64 - 1)?;
    // SAFETY: firmware tables live in RAM the direct map covers; they are read-only to us.
    Some(unsafe { core::slice::from_raw_parts(p as *const u8, len) })
}

fn sdt_at(pa: u64, signature: &[u8; 4]) -> Option<&'static [u8]> {
    let head = phys_slice(pa, crate::arch::acpi_madt_rule::SDT_HEADER_LEN)?;
    let len = crate::arch::acpi_madt_rule::sdt_len(head, signature)?;
    let table = phys_slice(pa, len)?;
    crate::arch::acpi_madt_rule::checksum_ok(table).then_some(table)
}

/// RSDP → XSDT (revision ≥ 2) or RSDT → the table signed `APIC`.
fn find_madt() -> Option<&'static [u8]> {
    let rsdp = phys_slice(RSDP_PADDR.load(Ordering::Acquire), 36)?;
    if &rsdp[0..8] != b"RSD PTR " {
        return None;
    }
    let revision = rsdp[15];
    if revision >= 2 {
        let xsdt_pa = u64::from_le_bytes(rsdp[24..32].try_into().ok()?);
        if let Some(xsdt) = sdt_at(xsdt_pa, b"XSDT") {
            for chunk in xsdt[crate::arch::acpi_madt_rule::SDT_HEADER_LEN..].chunks_exact(8) {
                let pa = u64::from_le_bytes(chunk.try_into().ok()?);
                if let Some(madt) = sdt_at(pa, b"APIC") {
                    return Some(madt);
                }
            }
        }
    }
    let rsdt_pa = u32::from_le_bytes(rsdp[16..20].try_into().ok()?) as u64;
    let rsdt = sdt_at(rsdt_pa, b"RSDT")?;
    for chunk in rsdt[crate::arch::acpi_madt_rule::SDT_HEADER_LEN..].chunks_exact(4) {
        let pa = u32::from_le_bytes(chunk.try_into().ok()?) as u64;
        if let Some(madt) = sdt_at(pa, b"APIC") {
            return Some(madt);
        }
    }
    None
}

/// Derive COM1's route from the executed machine's MADT and publish the line the provisioning
/// binds: the GSI, which the production decoder receives at vector 0x20 + GSI. Returns the line,
/// or `None` (logged) when the tables do not support a route this fixture may program.
pub fn derive_route() -> Option<u16> {
    let Some(madt) = find_madt() else {
        crate::yarm_log!("IRQ3_UART_ROUTE_REFUSED reason=no_madt_via_pvh_rsdp");
        return None;
    };
    let route = crate::arch::acpi_madt_rule::isa_route(madt, COM1_ISA_IRQ)?;
    let Some(ioapic) = crate::arch::acpi_madt_rule::ioapic_for_gsi(madt, route.gsi) else {
        crate::yarm_log!(
            "IRQ3_UART_ROUTE_REFUSED reason=no_ioapic_for_gsi gsi={}",
            route.gsi
        );
        return None;
    };
    let mut isos = [0u32; 8];
    let mut n = 0;
    for (irq, gsi, flags) in crate::arch::acpi_madt_rule::overrides(madt) {
        if n < isos.len() {
            isos[n] = ((irq as u32) << 24) | ((gsi & 0xff) << 16) | flags as u32;
            n += 1;
        }
    }
    crate::yarm_log!(
        "IRQ3_UART_MADT len={} ioapic_id={} ioapic_pa=0x{:x} gsi_base={} overrides={} iso0=0x{:08x} iso1=0x{:08x} iso2=0x{:08x} iso3=0x{:08x} iso4=0x{:08x}",
        madt.len(),
        ioapic.id,
        ioapic.address,
        ioapic.gsi_base,
        n,
        isos[0],
        isos[1],
        isos[2],
        isos[3],
        isos[4]
    );
    let vector = VEC_EXTERNAL_BASE + route.gsi;
    let lines = crate::arch::platform_constants::MAX_IRQ_LINES as u32;
    if route.gsi == 0 || route.gsi >= lines {
        crate::yarm_log!(
            "IRQ3_UART_ROUTE_REFUSED reason=gsi_outside_decoder_lines gsi={}",
            route.gsi
        );
        return None;
    }
    if ioapic.address as usize != crate::arch::x86_64::platform_layout::IOAPIC_MMIO_PHYS {
        crate::yarm_log!(
            "IRQ3_UART_ROUTE_REFUSED reason=ioapic_not_at_mapped_window pa=0x{:x}",
            ioapic.address
        );
        return None;
    }
    GSI.store(route.gsi, Ordering::Release);
    VECTOR.store(vector, Ordering::Release);
    PIN.store(route.gsi - ioapic.gsi_base, Ordering::Release);
    let low = vector
        | if route.active_low {
            REDIR_ACTIVE_LOW
        } else {
            0
        }
        | if route.level_triggered {
            REDIR_LEVEL
        } else {
            0
        };
    REDIR_LOW_ENABLED.store(low, Ordering::Release);
    ROUTE_READY.store(true, Ordering::Release);
    crate::yarm_log!(
        "IRQ3_UART_ROUTE isa_irq={} gsi={} overridden={} active_low={} level={} ioapic_pin={} vector=0x{:x} line={}",
        COM1_ISA_IRQ,
        route.gsi,
        route.overridden as u8,
        route.active_low as u8,
        route.level_triggered as u8,
        route.gsi - ioapic.gsi_base,
        vector,
        route.gsi
    );
    Some(route.gsi as u16)
}

fn deferred(reason: &'static str) -> bool {
    crate::yarm_log!("IRQ3_UART_ENABLE_DEFERRED reason={}", reason);
    false
}

/// The whole enable order. Returns `true` once the source is live.
pub fn enable_source() -> bool {
    // x86_64 boot opens interrupts (`enable_interrupts_for_boot`) before the timer is armed, so IF
    // is normally already 1 here. The whole ordered sequence runs with IF cleared, and the CPU's
    // admission is restored only after the redirection entry is unmasked: the CPU can take this
    // vector no earlier than the last step of the source's own enablement.
    let rflags: u64;
    // SAFETY: reads RFLAGS through the stack, then clears IF; restored below.
    unsafe {
        core::arch::asm!("pushfq", "pop {}", "cli", out(reg) rflags);
    }
    let was_enabled = (rflags >> 9) & 1 == 1;
    let ok = enable_source_interrupts_off(was_enabled);
    if was_enabled {
        // SAFETY: restores the interrupt flag this function found.
        unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
    }
    if ok {
        crate::yarm_log!(
            "IRQ3_UART_CPU_ADMISSION vector=0x{:x} if_restored={} after=source_enabled",
            VECTOR.load(Ordering::Acquire),
            was_enabled as u8
        );
    }
    ok
}

fn enable_source_interrupts_off(if_at_call: bool) -> bool {
    if !ROUTE_READY.load(Ordering::Acquire) {
        return deferred("route_not_derived");
    }
    let gsi = GSI.load(Ordering::Acquire);
    let vector = VECTOR.load(Ordering::Acquire);
    let pin = PIN.load(Ordering::Acquire);

    // ── 1. Handler and access readiness ───────────────────────────────────────────────────────
    if !crate::arch::x86_64::irq::lapic_configured()
        || crate::arch::x86_64::descriptor_tables::trap_shared_kernel().is_none()
    {
        return deferred("handler_not_ready");
    }
    for (name, va) in [("ioapic", ioapic_va()), ("lapic", lapic_va())] {
        let f = page_facts(va);
        crate::yarm_log!(
            "IRQ3_UART_MMIO_ACCESS page={} va=0x{:x} present={} uncached={} user_reachable={} level={} leaf=0x{:x}",
            name,
            va,
            f.present as u8,
            f.uncached as u8,
            f.user_reachable as u8,
            f.level,
            f.leaf
        );
        if !f.present || !f.uncached {
            return deferred("controller_page_not_uncached_kernel_mapping");
        }
        if f.user_reachable {
            return deferred("controller_page_user_reachable");
        }
    }
    let (io_map_base, tss_size) = crate::arch::x86_64::descriptor_tables::boot_tss_io_map_facts();
    let pic = (inb(PIC_MASTER_IMR), inb(PIC_SLAVE_IMR));
    let isa_bit = 1u8 << (COM1_ISA_IRQ % 8);
    crate::yarm_log!(
        "IRQ3_UART_PORT_ACCESS port=0x{:x} tss_io_map_base={} tss_size={} io_bitmap={} pic_master_imr=0x{:02x} pic_slave_imr=0x{:02x} pic_irq_masked={}",
        COM1,
        io_map_base,
        tss_size,
        (io_map_base as usize) < tss_size,
        pic.0,
        pic.1,
        (pic.0 & isa_bit != 0) as u8
    );
    if pic.0 & isa_bit == 0 {
        // A competing 8259 route for the same source must stay closed.
        return deferred("pic_route_for_irq_open");
    }
    let version = ioapic_read(IOAPIC_REG_VER);
    let pins = ((version >> 16) & 0xff) + 1;
    if pin >= pins {
        return deferred("gsi_beyond_ioapic_pins");
    }

    // ── 2. The route must already be bound through the production owner ───────────────────────
    let route = crate::arch::x86_64::descriptor_tables::trap_shared_kernel().and_then(|shared| {
        shared.with_ipc_split_mut(|ipc| ipc.irq_routes.get(gsi as usize).copied().flatten())
    });
    let Some(notification_idx) = route else {
        return deferred("route_unbound");
    };

    // ── 3. Controller configuration, written MASKED and read back ────────────────────────────
    let apic_id = mmio_read(lapic_va() + LAPIC_ID) >> 24;
    let redtbl = IOAPIC_REDTBL_BASE + 2 * pin;
    let low = REDIR_LOW_ENABLED.load(Ordering::Acquire);
    ioapic_write(redtbl, low | REDIR_MASKED);
    ioapic_write(redtbl + 1, apic_id << 24);
    let (rb_low, rb_high) = (ioapic_read(redtbl), ioapic_read(redtbl + 1));
    if rb_low & 0x1_ffff != (low | REDIR_MASKED) & 0x1_ffff || rb_high >> 24 != apic_id {
        return deferred("redirection_readback_disagrees");
    }

    // ── 4. Device: interrupts off, stale state cleared, OUT2 on ───────────────────────────────
    outb(COM1 + UART_IER, 0);
    let mut stale = 0u32;
    while inb(COM1 + UART_LSR) & UART_LSR_DR != 0 && stale < 64 {
        let _ = inb(COM1 + UART_RBR);
        stale += 1;
    }
    let _ = (
        inb(COM1 + UART_IIR),
        inb(COM1 + UART_LSR),
        inb(COM1 + UART_MSR),
    );
    let mcr_before = inb(COM1 + UART_MCR);
    outb(COM1 + UART_MCR, mcr_before | UART_MCR_OUT2);

    // ── 5. Source enablement ──────────────────────────────────────────────────────────────────
    outb(COM1 + UART_IER, UART_IER_ERBFI);
    if inb(COM1 + UART_IER) & 0x0f != UART_IER_ERBFI {
        outb(COM1 + UART_IER, 0);
        return deferred("uart_ier_readback_disagrees");
    }

    // ── 6. Controller admission: unmask the one redirection entry ─────────────────────────────
    ioapic_write(redtbl, low);
    let enabled_low = ioapic_read(redtbl);
    if enabled_low & REDIR_MASKED != 0 {
        outb(COM1 + UART_IER, 0);
        return deferred("redirection_unmask_readback_disagrees");
    }
    SOURCE_ENABLED.store(true, Ordering::Release);
    ring_store(RING_WORD_ARMED, 1);
    // CPU admission is NOT this function's: `enable_source` restores IF after it returns.
    // `cpu_if` records that it is off for the whole sequence.
    let rflags: u64;
    // SAFETY: reads RFLAGS through the stack; no other state changes.
    unsafe { core::arch::asm!("pushfq", "pop {}", out(reg) rflags, options(preserves_flags)) };
    // Two lines: the kernel log line limit is 192 bytes.
    crate::yarm_log!(
        "IRQ3_UART_DEVICE_ENABLED port=0x{:x} if_at_call={} cpu_if={} ier=0x{:x} mcr=0x{:x} mcr_before=0x{:x} stale_drained={}",
        COM1,
        if_at_call as u8,
        (rflags >> 9) & 1,
        inb(COM1 + UART_IER),
        inb(COM1 + UART_MCR),
        mcr_before,
        stale
    );
    crate::yarm_log!(
        "IRQ3_UART_SOURCE_ENABLED gsi={} ioapic_pin={} vector=0x{:x} line={} dest_apic={} redir_low=0x{:x} redir_high=0x{:x} ioapic_pins={} route_notification={} items={}",
        gsi,
        pin,
        vector,
        gsi,
        apic_id,
        enabled_low,
        ioapic_read(redtbl + 1),
        pins,
        notification_idx,
        WITNESS_ITEMS
    );
    true
}

/// The witness vector once the route is derived, else 0.
pub fn witness_vector() -> u32 {
    if ROUTE_READY.load(Ordering::Acquire) {
        VECTOR.load(Ordering::Acquire)
    } else {
        0
    }
}

// ── The two hooks on the production path ──────────────────────────────────────────────────────

/// Before delivery: record where the interrupt was taken from, prove the regime it runs under,
/// then drain the device. Identity is the vector the CPU delivered.
pub fn note_entry_and_drain(vector: u64, cs: u64, rip: u64, cpu: crate::kernel::scheduler::CpuId) {
    let witness = VECTOR.load(Ordering::Acquire) as u64;
    if witness == 0 || vector != witness || !SOURCE_ENABLED.load(Ordering::Acquire) {
        return;
    }
    EOI_AT_ENTRY.store(
        crate::arch::x86_64::irq::lapic_eoi_writes(),
        Ordering::Release,
    );
    let claim = CLAIMS.fetch_add(1, Ordering::AcqRel) + 1;
    ring_store(RING_WORD_CLAIMS, claim);
    let parked = crate::kernel::idle_boundary::is_parked(cpu.0 as usize);
    let (origin, n) = match (cs & 3, parked) {
        (3, _) => {
            let n = USER_ORIGIN.fetch_add(1, Ordering::AcqRel) + 1;
            ring_store(RING_WORD_USER_ORIGIN, n);
            ("user", n)
        }
        (0, true) => {
            let n = IDLE_ORIGIN.fetch_add(1, Ordering::AcqRel) + 1;
            ring_store(RING_WORD_IDLE_ORIGIN, n);
            ("idle", n)
        }
        _ => ("other", OTHER_ORIGIN.fetch_add(1, Ordering::AcqRel) + 1),
    };
    let tid = crate::arch::x86_64::descriptor_tables::trap_shared_kernel()
        .and_then(|s| s.current_tid_split_read(cpu))
        .unwrap_or(0);
    let (io, la) = (page_facts(ioapic_va()), page_facts(lapic_va()));
    let regime_ok = io.present
        && io.uncached
        && !io.user_reachable
        && la.present
        && la.uncached
        && !la.user_reachable;
    let in_service = lapic_bit(LAPIC_ISR_BASE, vector as u32);

    // ── Device acknowledgement FIRST: RBR until LSR.DR clears, before any console output ─────
    //
    // The console is this same UART. QEMU's I/O APIC models an edge-triggered pin as "deliver on
    // every assertion", and its 16550 re-asserts the line on every status update — a transmit
    // included — while a receive cause is pending. A log line written before the cause is
    // serviced would therefore queue a SECOND delivery of this vector behind the one in service
    // (a real ISA line that stays high makes no new edge). Servicing the cause before printing
    // anything is the order the device requires here; `self_irr` records that no re-delivery
    // was queued.
    let iir_before = inb(COM1 + UART_IIR);
    let mut drained = 0usize;
    let mut first = 0u8;
    while drained < MAX_DRAIN_PER_CLAIM && inb(COM1 + UART_LSR) & UART_LSR_DR != 0 {
        let byte = inb(COM1 + UART_RBR);
        if drained == 0 {
            first = byte;
        }
        let index = BYTES.load(Ordering::Acquire) as usize;
        ring_store_byte(index, byte);
        let total = BYTES.fetch_add(1, Ordering::AcqRel) + 1;
        ring_store(RING_WORD_BYTES, total);
        drained += 1;
    }
    let iir_after = inb(COM1 + UART_IIR);
    let lsr_after = inb(COM1 + UART_LSR);
    let self_irr_after_drain = lapic_bit(LAPIC_IRR_BASE, vector as u32);
    crate::yarm_log!(
        "IRQ3_UART_IRQ_ENTRY origin={} n={} claim={} vector=0x{:x} line={} cs=0x{:x} rip=0x{:x} parked={} tid={} lapic_isr={} regime_ok={}",
        origin,
        n,
        claim,
        vector,
        vector - VEC_EXTERNAL_BASE as u64,
        cs,
        rip,
        parked as u8,
        tid,
        in_service as u8,
        regime_ok as u8
    );
    let data_claim = if drained == 0 {
        let n = EMPTY_DRAINS.fetch_add(1, Ordering::AcqRel) + 1;
        ring_store(RING_WORD_EMPTY_DRAINS, n);
        0
    } else {
        COMPLETION_OWED_FOR_DATA.store(true, Ordering::Release);
        DATA_CLAIMS.fetch_add(1, Ordering::AcqRel) + 1
    };
    crate::yarm_log!(
        "IRQ3_UART_DRAIN claim={} vector=0x{:x} bytes={} first=0x{:02x} iir_before=0x{:02x} iir_after=0x{:02x} lsr_dr_after={} self_irr={} total_bytes={}",
        claim,
        vector,
        drained,
        first,
        iir_before,
        iir_after,
        lsr_after & UART_LSR_DR,
        self_irr_after_drain as u8,
        BYTES.load(Ordering::Acquire)
    );

    // ── Timer and device together: hold until the tick is pending too ─────────────────────────
    if TIMER_TOGETHER_CLAIMS.contains(&data_claim) {
        let mut polls = 0u32;
        while !lapic_bit(LAPIC_IRR_BASE, LAPIC_TIMER_VECTOR) && polls < TOGETHER_POLL_LIMIT {
            core::hint::spin_loop();
            polls += 1;
        }
        let pending = lapic_bit(LAPIC_IRR_BASE, LAPIC_TIMER_VECTOR);
        if pending {
            TOGETHER_HELD.fetch_add(1, Ordering::AcqRel);
        }
        crate::yarm_log!(
            "IRQ3_UART_TOGETHER claim={} data_item={} origin={} timer_irr={} device_isr={} polls={}",
            claim,
            data_claim,
            origin,
            pending as u8,
            lapic_bit(LAPIC_ISR_BASE, vector as u32) as u8,
            polls
        );
    }
}

/// After the bridge returns: the one EOI has been written by the production owner. Read back the
/// LAPIC in-service bit and the redirection entry, count the completion, and turn the source off
/// after the last data item. Never writes an EOI.
pub fn note_after_dispatch(vector: u64) {
    let witness = VECTOR.load(Ordering::Acquire) as u64;
    if witness == 0 || vector != witness || !SOURCE_ENABLED.load(Ordering::Acquire) {
        return;
    }
    let n = COMPLETIONS.fetch_add(1, Ordering::AcqRel) + 1;
    ring_store(RING_WORD_COMPLETIONS, n);
    let in_service = lapic_bit(LAPIC_ISR_BASE, vector as u32);
    let redir = ioapic_read(IOAPIC_REDTBL_BASE + 2 * PIN.load(Ordering::Acquire));
    let timer_pending = lapic_bit(LAPIC_IRR_BASE, LAPIC_TIMER_VECTOR);
    // Interrupts stay off from entry to here (interrupt gate), so the delta is this dispatch's own
    // EOIs: exactly one, written by the production owner, is the contract.
    let eoi_writes = crate::arch::x86_64::irq::lapic_eoi_writes()
        .wrapping_sub(EOI_AT_ENTRY.load(Ordering::Acquire));
    if !in_service && eoi_writes == 1 && redir & (REDIR_REMOTE_IRR | REDIR_DELIVERY_PENDING) == 0 {
        COMPLETED_CLEAN.fetch_add(1, Ordering::AcqRel);
    }
    crate::yarm_log!(
        "IRQ3_UART_COMPLETE n={} vector=0x{:x} claims={} eoi_writes={} lapic_isr_after={} remote_irr={} delivery_pending={} timer_irr={}",
        n,
        vector,
        CLAIMS.load(Ordering::Acquire),
        eoi_writes,
        in_service as u8,
        (redir & REDIR_REMOTE_IRR != 0) as u8,
        (redir & REDIR_DELIVERY_PENDING != 0) as u8,
        timer_pending as u8
    );
    if COMPLETION_OWED_FOR_DATA.swap(false, Ordering::AcqRel)
        && DATA_CLAIMS.load(Ordering::Acquire) >= WITNESS_ITEMS
    {
        disable_source();
    }
}

/// Device first (IER = 0, the cause can no longer assert), then the redirection entry masked and
/// read back. The LAPIC and IF are left alone — the timer still needs them.
fn disable_source() {
    if SOURCE_DISABLED.swap(true, Ordering::AcqRel) {
        return;
    }
    outb(COM1 + UART_IER, 0);
    let redtbl = IOAPIC_REDTBL_BASE + 2 * PIN.load(Ordering::Acquire);
    ioapic_write(redtbl, ioapic_read(redtbl) | REDIR_MASKED);
    let redir_after = ioapic_read(redtbl);
    ring_store(RING_WORD_DISABLED, 1);
    crate::yarm_log!(
        "IRQ3_UART_SOURCE_DISABLED vector=0x{:x} claims={} data_claims={} completions={} completed_clean={} bytes={} ier_after=0x{:x} redir_masked_after={}",
        VECTOR.load(Ordering::Acquire),
        CLAIMS.load(Ordering::Acquire),
        DATA_CLAIMS.load(Ordering::Acquire),
        COMPLETIONS.load(Ordering::Acquire),
        COMPLETED_CLEAN.load(Ordering::Acquire),
        BYTES.load(Ordering::Acquire),
        inb(COM1 + UART_IER),
        (redir_after & REDIR_MASKED != 0) as u8
    );
    crate::yarm_log!(
        "IRQ3_UART_TOTALS empty_drains={} idle_origin={} user_origin={} other_origin={} together_held={}",
        EMPTY_DRAINS.load(Ordering::Acquire),
        IDLE_ORIGIN.load(Ordering::Acquire),
        USER_ORIGIN.load(Ordering::Acquire),
        OTHER_ORIGIN.load(Ordering::Acquire),
        TOGETHER_HELD.load(Ordering::Acquire)
    );
}
