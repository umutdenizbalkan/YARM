// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! AP-SCHED4 §B — the firmware tables production reads: the RSDP the PVH boot protocol hands over,
//! and through it the MADT (the processors the firmware lists, and the interrupt-controller facts
//! the COM1 witness reads). Read-only; no AML, no table but these.

use core::sync::atomic::{AtomicU64, Ordering};

static RSDP_PADDR: AtomicU64 = AtomicU64::new(0);

/// Record the PVH start_info's RSDP address. Called once from the PVH boot-data capture.
pub fn capture_rsdp(paddr: u64) {
    RSDP_PADDR.store(paddr, Ordering::Release);
}

/// The RSDP address the boot protocol handed over (0 when none).
pub fn rsdp_paddr() -> u64 {
    RSDP_PADDR.load(Ordering::Acquire)
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

/// RSDP → XSDT (revision ≥ 2) or RSDT → the table signed `APIC`, checksums verified.
pub fn find_madt() -> Option<&'static [u8]> {
    let rsdp = phys_slice(rsdp_paddr(), 36)?;
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
