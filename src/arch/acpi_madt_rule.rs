// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-IRQ3 §1 — the MADT facts one ISA interrupt needs, read from the firmware's own table.
//!
//! Arch-neutral and pure so the hosted suite executes the rule the x86_64 witness fixture applies
//! to the live table. It answers exactly two questions and nothing more general:
//!
//! * which Global System Interrupt an ISA IRQ arrives on, with which polarity and trigger — an
//!   Interrupt Source Override (MADT entry type 2) for the IRQ wins; without one the ISA defaults
//!   apply (GSI = IRQ, active-high, edge);
//! * which I/O APIC owns that GSI (entry type 1: `gsi_base` and the register window address).
//!
//! It is not an ACPI subsystem: no AML, no other tables, no enumeration beyond these two entry
//! types.

/// Size of the standard ACPI System Description Table header.
pub const SDT_HEADER_LEN: usize = 36;
/// MADT: header, then the local-APIC address (u32) and flags (u32), then the entries.
const MADT_ENTRIES_OFFSET: usize = SDT_HEADER_LEN + 8;
const MADT_TYPE_IOAPIC: u8 = 1;
const MADT_TYPE_ISO: u8 = 2;

/// Where one ISA IRQ is delivered, as the MADT describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IsaRoute {
    pub gsi: u32,
    pub active_low: bool,
    pub level_triggered: bool,
    /// `true` when an Interrupt Source Override named this IRQ.
    pub overridden: bool,
}

/// One I/O APIC entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoApicEntry {
    pub id: u8,
    pub address: u32,
    pub gsi_base: u32,
}

fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

/// ACPI tables sum to zero modulo 256.
pub fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)) == 0
}

/// The table's own length field, if the bytes carry a plausible SDT header with `signature`.
pub fn sdt_len(bytes: &[u8], signature: &[u8; 4]) -> Option<usize> {
    if bytes.get(0..4)? != signature {
        return None;
    }
    let len = u32_at(bytes, 4)? as usize;
    (len >= SDT_HEADER_LEN).then_some(len)
}

/// Every well-formed MADT entry, as `(type, body)`, stopping at the first malformed one.
fn entries(madt: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let end = sdt_len(madt, b"APIC").unwrap_or(0).min(madt.len());
    let mut off = MADT_ENTRIES_OFFSET;
    core::iter::from_fn(move || {
        if off + 2 > end {
            return None;
        }
        let kind = madt[off];
        let len = madt[off + 1] as usize;
        if len < 2 || off + len > end {
            return None;
        }
        let body = &madt[off..off + len];
        off += len;
        Some((kind, body))
    })
}

/// Where ISA IRQ `irq` arrives. `None` for a table that is not a well-formed MADT.
pub fn isa_route(madt: &[u8], irq: u8) -> Option<IsaRoute> {
    sdt_len(madt, b"APIC")?;
    for (kind, body) in entries(madt) {
        // ISO: type, len(10), bus, source, gsi (u32), flags (u16). Bus 0 is ISA.
        if kind == MADT_TYPE_ISO && body.len() >= 10 && body[2] == 0 && body[3] == irq {
            let gsi = u32_at(body, 4)?;
            let flags = u16_at(body, 8)?;
            // Polarity bits [1:0]: 00 conforms to the bus (ISA: high), 01 high, 11 low.
            // Trigger bits [3:2]: 00 conforms (ISA: edge), 01 edge, 11 level.
            return Some(IsaRoute {
                gsi,
                active_low: flags & 0b11 == 0b11,
                level_triggered: (flags >> 2) & 0b11 == 0b11,
                overridden: true,
            });
        }
    }
    Some(IsaRoute {
        gsi: irq as u32,
        active_low: false,
        level_triggered: false,
        overridden: false,
    })
}

/// The I/O APIC whose input range starts at the greatest `gsi_base` not above `gsi`. The caller
/// still checks the pin against that I/O APIC's own redirection-entry count.
pub fn ioapic_for_gsi(madt: &[u8], gsi: u32) -> Option<IoApicEntry> {
    sdt_len(madt, b"APIC")?;
    let mut best: Option<IoApicEntry> = None;
    for (kind, body) in entries(madt) {
        // IOAPIC: type, len(12), id, reserved, address (u32), gsi_base (u32).
        if kind == MADT_TYPE_IOAPIC && body.len() >= 12 {
            let e = IoApicEntry {
                id: body[2],
                address: u32_at(body, 4)?,
                gsi_base: u32_at(body, 8)?,
            };
            if e.gsi_base <= gsi && best.is_none_or(|b| e.gsi_base > b.gsi_base) {
                best = Some(e);
            }
        }
    }
    best
}

/// Every Interrupt Source Override, as `(isa_irq, gsi, flags)`, for the record.
pub fn overrides(madt: &[u8]) -> impl Iterator<Item = (u8, u32, u16)> + '_ {
    entries(madt).filter_map(|(kind, body)| {
        (kind == MADT_TYPE_ISO && body.len() >= 10)
            .then(|| Some((body[3], u32_at(body, 4)?, u16_at(body, 8)?)))
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    /// A MADT shaped like QEMU q35's: one LAPIC, one I/O APIC at 0xFEC0_0000 base 0, and the
    /// overrides QEMU installs (IRQ0 -> GSI2; IRQ5, 9, 10, 11 active-high level).
    fn q35_like() -> Vec<u8> {
        let mut e = Vec::new();
        e.extend_from_slice(&[0, 8, 0, 0, 1, 0, 0, 0]); // LAPIC
        e.extend_from_slice(&[1, 12, 0, 0]);
        e.extend_from_slice(&0xFEC0_0000u32.to_le_bytes());
        e.extend_from_slice(&0u32.to_le_bytes());
        let iso = |e: &mut Vec<u8>, irq: u8, gsi: u32, flags: u16| {
            e.extend_from_slice(&[2, 10, 0, irq]);
            e.extend_from_slice(&gsi.to_le_bytes());
            e.extend_from_slice(&flags.to_le_bytes());
        };
        iso(&mut e, 0, 2, 0);
        for irq in [5u8, 9, 10, 11] {
            iso(&mut e, irq, irq as u32, 0b1101);
        }
        let len = (MADT_ENTRIES_OFFSET + e.len()) as u32;
        let mut t = Vec::new();
        t.extend_from_slice(b"APIC");
        t.extend_from_slice(&len.to_le_bytes());
        t.resize(SDT_HEADER_LEN, 0);
        t.extend_from_slice(&0xFEE0_0000u32.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&e);
        let sum = t.iter().fold(0u8, |a, b| a.wrapping_add(*b));
        t[9] = 0u8.wrapping_sub(sum);
        t
    }

    #[test]
    fn irq4_has_no_override_and_takes_the_isa_defaults() {
        let m = q35_like();
        assert!(checksum_ok(&m));
        assert_eq!(
            isa_route(&m, 4),
            Some(IsaRoute {
                gsi: 4,
                active_low: false,
                level_triggered: false,
                overridden: false
            })
        );
    }

    #[test]
    fn an_override_wins_and_its_flags_are_decoded() {
        let m = q35_like();
        assert_eq!(
            isa_route(&m, 0).map(|r| (r.gsi, r.overridden)),
            Some((2, true))
        );
        let r9 = isa_route(&m, 9).expect("irq 9");
        assert!(r9.overridden && r9.level_triggered && !r9.active_low);
        assert_eq!(overrides(&m).count(), 5);
    }

    #[test]
    fn the_ioapic_is_chosen_by_gsi_base() {
        let m = q35_like();
        assert_eq!(
            ioapic_for_gsi(&m, 4),
            Some(IoApicEntry {
                id: 0,
                address: 0xFEC0_0000,
                gsi_base: 0
            })
        );
    }

    #[test]
    fn a_table_that_is_not_a_madt_answers_nothing() {
        let mut m = q35_like();
        m[0] = b'X';
        assert_eq!(isa_route(&m, 4), None);
        assert_eq!(ioapic_for_gsi(&m, 4), None);
        // A truncated entry ends the walk rather than reading past the table.
        let mut t = q35_like();
        let len = t.len() as u32 + 1;
        t[4..8].copy_from_slice(&len.to_le_bytes());
        assert_eq!(isa_route(&t, 4).map(|r| r.gsi), Some(4));
    }
}
