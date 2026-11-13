// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-TIME-FOUNDATION1 B (TIMEKEEP1) — each port's free-running counter and the evidence for
//! its frequency. The arch-neutral clock is `crate::kernel::timekeeping`.
//!
//! | port | counter | frequency evidence | cross-CPU |
//! |---|---|---|---|
//! | x86_64 | `rdtsc` | measured against the 8254 PIT (1 193 182 Hz, the device's own crystal) over three 10 ms windows, median | not proven: `qemu64` advertises no invariant TSC, so reads are clamped nondecreasing |
//! | AArch64 | `CNTPCT_EL0` (after `isb`) | `CNTFRQ_EL0`, programmed by firmware for the system counter | one system counter for every PE (Arm ARM D11.1) |
//! | RISC-V | `time` CSR | the device tree's `/cpus/timebase-frequency` | one platform count (`mtime`) for every hart |
//!
//! No frequency is assumed: a port whose evidence is missing installs no source, and the
//! monotonic clock then reports `Unavailable` instead of a guessed rate.

use crate::kernel::timekeeping::SourceKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveredSource {
    pub freq_hz: u64,
    pub origin: u64,
    pub kind: SourceKind,
}

// ── hosted (tests, hosted-dev): a settable counter ─────────────────────────────────────────
#[cfg(any(test, feature = "hosted-dev"))]
pub(crate) static HOSTED_COUNTER: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// The hosted counter runs at 1 GHz (one unit per nanosecond) unless a test installs otherwise.
#[cfg(any(test, feature = "hosted-dev"))]
pub const HOSTED_FREQ_HZ: u64 = 1_000_000_000;

#[cfg(any(test, feature = "hosted-dev"))]
pub fn counter() -> u64 {
    HOSTED_COUNTER.load(core::sync::atomic::Ordering::Acquire)
}

#[cfg(any(test, feature = "hosted-dev"))]
pub fn discover_source() -> Option<DiscoveredSource> {
    Some(DiscoveredSource {
        freq_hz: HOSTED_FREQ_HZ,
        origin: counter(),
        kind: SourceKind::Hosted,
    })
}

// ── x86_64: TSC, PIT-calibrated ────────────────────────────────────────────────────────────
#[cfg(all(not(any(test, feature = "hosted-dev")), target_arch = "x86_64"))]
mod imp {
    use super::{DiscoveredSource, SourceKind};

    /// The 8254 PIT input clock, a property of the device (1.193182 MHz crystal / 12 · 14.31818).
    const PIT_HZ: u64 = 1_193_182;
    /// One calibration window: 10 ms of PIT counts.
    const WINDOW_MS: u64 = 10;
    const WINDOW_COUNTS: u64 = PIT_HZ * WINDOW_MS / 1000;
    /// Bound on polls of the PIT output per window (a missing PIT ends calibration).
    const POLL_BOUND: u64 = 200_000_000;

    fn outb(port: u16, v: u8) {
        // SAFETY: PIT / NMI-status port writes at boot on the BSP, interrupts masked.
        unsafe {
            core::arch::asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack))
        }
    }
    fn inb(port: u16) -> u8 {
        let v: u8;
        // SAFETY: PIT / NMI-status port read; no memory effect.
        unsafe {
            core::arch::asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack))
        }
        v
    }

    pub fn counter() -> u64 {
        // SAFETY: `rdtsc` is unprivileged and has no memory or state effects. `lfence` orders it
        // after earlier loads so a read cannot be hoisted above the code that requested it.
        unsafe {
            core::arch::x86_64::_mm_lfence();
            core::arch::x86_64::_rdtsc()
        }
    }

    /// TSC counts across one PIT window (channel 2, mode 0, gate on, speaker off), or `None` if
    /// the PIT output never rises.
    fn one_window() -> Option<u64> {
        let gate = inb(0x61);
        outb(0x61, (gate & !0x02) | 0x01);
        outb(0x43, 0b1011_0000); // channel 2, lobyte/hibyte, mode 0, binary
        outb(0x42, (WINDOW_COUNTS & 0xff) as u8);
        outb(0x42, (WINDOW_COUNTS >> 8) as u8);
        let t0 = counter();
        let mut polls = 0u64;
        while inb(0x61) & 0x20 == 0 {
            polls += 1;
            if polls >= POLL_BOUND {
                outb(0x61, gate);
                return None;
            }
        }
        let t1 = counter();
        outb(0x61, gate);
        Some(t1.wrapping_sub(t0))
    }

    pub fn discover_source() -> Option<DiscoveredSource> {
        let mut w = [0u64; 3];
        for slot in w.iter_mut() {
            *slot = one_window()?;
        }
        w.sort_unstable();
        let freq_hz = w[1].checked_mul(1000)? / WINDOW_MS;
        crate::yarm_log!(
            "TIMEKEEP_X86_TSC_CALIBRATION pit_hz={} window_ms={} counts={},{},{} median_hz={}",
            PIT_HZ,
            WINDOW_MS,
            w[0],
            w[1],
            w[2],
            freq_hz
        );
        (freq_hz != 0).then(|| DiscoveredSource {
            freq_hz,
            origin: counter(),
            kind: SourceKind::X86TscPitCalibrated,
        })
    }
}

// ── AArch64: the generic timer's physical counter ─────────────────────────────────────────
#[cfg(all(not(any(test, feature = "hosted-dev")), target_arch = "aarch64"))]
mod imp {
    use super::{DiscoveredSource, SourceKind};

    pub fn counter() -> u64 {
        let v: u64;
        // SAFETY: `CNTPCT_EL0` is readable at EL1; `isb` keeps the read from being speculated
        // ahead of program order.
        unsafe {
            core::arch::asm!("isb", "mrs {0}, CNTPCT_EL0", out(reg) v, options(nomem, nostack))
        };
        v
    }

    pub fn discover_source() -> Option<DiscoveredSource> {
        let freq: u64;
        // SAFETY: `CNTFRQ_EL0` is a read-only frequency register at EL1.
        unsafe { core::arch::asm!("mrs {0}, CNTFRQ_EL0", out(reg) freq, options(nomem, nostack)) };
        let freq_hz = freq & 0xFFFF_FFFF;
        (freq_hz != 0).then(|| DiscoveredSource {
            freq_hz,
            origin: counter(),
            kind: SourceKind::Aarch64Cntpct,
        })
    }
}

// ── RISC-V: the `time` CSR ─────────────────────────────────────────────────────────────────
#[cfg(all(not(any(test, feature = "hosted-dev")), target_arch = "riscv64"))]
mod imp {
    use super::{DiscoveredSource, SourceKind};

    pub fn counter() -> u64 {
        let v: u64;
        // SAFETY: `time` is a read-only CSR readable from S-mode.
        unsafe { core::arch::asm!("csrr {0}, time", out(reg) v, options(nomem, nostack)) };
        v
    }

    pub fn discover_source() -> Option<DiscoveredSource> {
        let dtb = crate::arch::riscv64::boot::captured_dtb()?;
        let freq_hz = crate::arch::fdt::cpus_timebase_frequency(dtb)?;
        (freq_hz != 0).then(|| DiscoveredSource {
            freq_hz,
            origin: counter(),
            kind: SourceKind::RiscvTime,
        })
    }
}

#[cfg(not(any(test, feature = "hosted-dev")))]
pub use imp::{counter, discover_source};
