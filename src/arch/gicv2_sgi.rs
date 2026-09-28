// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP2 — GICv2 software-generated interrupts (SGIs): the pure half of the protocol.
//!
//! The AArch64 port (`arch::aarch64::smp`) owns the registers; this module owns every decision
//! that does not need them, so the hosted suite can exercise it on any host.
//!
//! # Identity: a scheduler CPU is not a controller target bit
//!
//! `GICD_SGIR.CPUTargetList` addresses GIC **CPU interfaces**, and nothing in GICv2 makes CPU
//! interface *n* the PE whose `MPIDR_EL1.Aff0` is *n*. The architecture does provide the mapping
//! from the other side: `GICD_ITARGETSR0..7` are banked per CPU interface and read back, in every
//! byte lane, the bit of the interface performing the read. So each CPU reads its own lane and
//! PUBLISHES it ([`TargetTable::publish`]); a sender looks the target up and refuses a CPU that
//! has not published ([`TargetTable::sgir_for`]) rather than guessing `1 << cpu`.
//!
//! # The SGI identity
//!
//! INTIDs in use on this port: 30 (the EL1 physical timer PPI, priority `0x00`), 33 (the PL011
//! SPI, witness builds only, priority `0x80`), and the special 1020..=1023. SGIs 0..=15 are
//! otherwise unused. The reschedule wake is SGI **1**; SGI 0 is deliberately left unused, so a
//! zeroed or truncated `GICD_SGIR` write can never be mistaken for a wake.
//!
//! # The acknowledgement token
//!
//! For an SGI, `GICC_IAR[12:10]` names the CPU interface that REQUESTED it. The claim carries the
//! whole token to the one `GICC_EOIR` write (QEMU-IRQ2); [`sgi_source_interface`] only reads it.

use core::sync::atomic::{AtomicU8, Ordering};

/// The reschedule-wake SGI.
pub const RESCHEDULE_SGI_INTID: u16 = 1;
/// Priority of the reschedule SGI: below the timer PPI (`0x00`), above the PL011 witness SPI
/// (`0x80`). `GICC_PMR` is `0xff`, so every one of them is signalled.
pub const RESCHEDULE_SGI_PRIORITY: u8 = 0x40;

/// Number of GICv2 CPU interfaces the architecture can address (`CPUTargetList` is 8 bits).
pub const GICV2_MAX_INTERFACES: usize = 8;

/// `true` for INTIDs 0..=15.
pub const fn is_sgi(intid: u16) -> bool {
    intid < 16
}

/// The requesting CPU interface an SGI acknowledgement names (`GICC_IAR[12:10]`).
pub const fn sgi_source_interface(raw_iar: u32) -> u8 {
    ((raw_iar >> 10) & 0x7) as u8
}

/// The interface bit a CPU reads in its own byte lane of `GICD_ITARGETSR0`. `None` unless exactly
/// one bit is set: an implementation that answers 0 (a uniprocessor GIC) or several bits gives no
/// usable identity, and nothing may be sent on its behalf.
pub const fn own_interface_mask(itargetsr0: u32) -> Option<u8> {
    let lane = (itargetsr0 & 0xff) as u8;
    if lane.count_ones() == 1 {
        Some(lane)
    } else {
        None
    }
}

/// `GICD_SGIR` for one SGI to exactly the interfaces in `target_mask` (`TargetListFilter` 0).
/// `None` for an empty mask or a non-SGI INTID; the NSATT bit is 0 (group 0, no security).
pub const fn sgir_value(target_mask: u8, intid: u16) -> Option<u32> {
    if target_mask == 0 || !is_sgi(intid) {
        return None;
    }
    Some(((target_mask as u32) << 16) | intid as u32)
}

/// Why an SGI was not sent. Nothing was written to the controller in any of these cases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SgiRefusal {
    /// The CPU index is outside the table.
    InvalidCpu,
    /// The target has not published its interface bit: it has not brought its interface up.
    TargetNotPublished,
    /// The target is the sender. A wake the sender owes itself is its own dispatcher's work.
    SelfTarget,
    /// The controller's distributor base is unknown.
    ControllerUnconfigured,
}

impl SgiRefusal {
    pub const fn reason(self) -> &'static str {
        match self {
            SgiRefusal::InvalidCpu => "invalid_cpu",
            SgiRefusal::TargetNotPublished => "target_not_published",
            SgiRefusal::SelfTarget => "self_target",
            SgiRefusal::ControllerUnconfigured => "controller_unconfigured",
        }
    }
}

/// The per-CPU interface bits, each written once by the CPU it describes.
pub struct TargetTable<const N: usize> {
    masks: [AtomicU8; N],
}

impl<const N: usize> Default for TargetTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> TargetTable<N> {
    pub const fn new() -> Self {
        Self {
            masks: [const { AtomicU8::new(0) }; N],
        }
    }

    /// Record `cpu`'s own interface bit, as it read it from its banked `GICD_ITARGETSR0`. Release:
    /// a sender that sees the bit also sees the interface configuration the CPU finished before
    /// publishing it.
    pub fn publish(&self, cpu: usize, itargetsr0: u32) -> Option<u8> {
        let mask = own_interface_mask(itargetsr0)?;
        self.masks.get(cpu)?.store(mask, Ordering::Release);
        Some(mask)
    }

    /// The published bit of `cpu`, or 0.
    pub fn mask_of(&self, cpu: usize) -> u8 {
        self.masks
            .get(cpu)
            .map(|m| m.load(Ordering::Acquire))
            .unwrap_or(0)
    }

    /// The `GICD_SGIR` value that wakes `target` from `sender` with `intid`, or why none may be
    /// written.
    pub fn sgir_for(&self, sender: usize, target: usize, intid: u16) -> Result<u32, SgiRefusal> {
        if target >= N || sender >= N {
            return Err(SgiRefusal::InvalidCpu);
        }
        if target == sender {
            return Err(SgiRefusal::SelfTarget);
        }
        sgir_value(self.mask_of(target), intid).ok_or(SgiRefusal::TargetNotPublished)
    }

    /// Which published CPU owns interface bit `interface` (the inverse lookup an arrival uses to
    /// name its sender as a scheduler CPU).
    pub fn cpu_of_interface(&self, interface: u8) -> Option<usize> {
        let bit = 1u8.checked_shl(u32::from(interface))?;
        (0..N).find(|&cpu| self.mask_of(cpu) == bit)
    }

    #[cfg(test)]
    pub fn clear_for_test(&self) {
        for m in &self.masks {
            m.store(0, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reschedule_sgi_is_an_sgi_other_than_zero() {
        assert!(is_sgi(RESCHEDULE_SGI_INTID));
        assert_ne!(RESCHEDULE_SGI_INTID, 0);
        assert!(!is_sgi(30) && !is_sgi(33) && !is_sgi(1023));
    }

    #[test]
    fn sgir_carries_the_target_list_and_the_intid_only() {
        assert_eq!(sgir_value(0b10, 1), Some(0x0002_0001));
        assert_eq!(sgir_value(0b01, 1), Some(0x0001_0001));
        assert_eq!(sgir_value(0, 1), None, "no target, no write");
        assert_eq!(sgir_value(0b10, 16), None, "a PPI is not an SGI");
    }

    #[test]
    fn the_source_is_read_from_bits_12_to_10_of_the_token() {
        assert_eq!(sgi_source_interface(0x0000_0401), 1);
        assert_eq!(sgi_source_interface(0x0000_0001), 0);
        assert_eq!(sgi_source_interface(0x0000_1c01), 7);
    }

    #[test]
    fn identity_is_what_each_cpu_reads_not_its_index() {
        let t: TargetTable<4> = TargetTable::new();
        // A controller whose interface numbering does not follow Aff0: CPU 1 is interface 2.
        assert_eq!(t.publish(0, 0x0101_0101), Some(0b0001));
        assert_eq!(t.publish(1, 0x0404_0404), Some(0b0100));
        assert_eq!(t.sgir_for(0, 1, RESCHEDULE_SGI_INTID), Ok(0x0004_0001));
        assert_eq!(t.cpu_of_interface(2), Some(1));
        assert_eq!(t.cpu_of_interface(1), None, "no CPU owns interface 1");
    }

    #[test]
    fn a_target_that_never_published_is_refused_not_guessed() {
        let t: TargetTable<4> = TargetTable::new();
        assert_eq!(t.publish(0, 0x01), Some(1));
        assert_eq!(
            t.sgir_for(0, 1, RESCHEDULE_SGI_INTID),
            Err(SgiRefusal::TargetNotPublished)
        );
        assert_eq!(
            t.sgir_for(0, 0, RESCHEDULE_SGI_INTID),
            Err(SgiRefusal::SelfTarget)
        );
        assert_eq!(
            t.sgir_for(0, 9, RESCHEDULE_SGI_INTID),
            Err(SgiRefusal::InvalidCpu)
        );
    }

    #[test]
    fn an_ambiguous_lane_publishes_nothing() {
        let t: TargetTable<2> = TargetTable::new();
        assert_eq!(t.publish(1, 0x00), None, "uniprocessor GIC answers 0");
        assert_eq!(
            t.publish(1, 0x03),
            None,
            "two bits name no single interface"
        );
        assert_eq!(t.mask_of(1), 0);
    }
}
