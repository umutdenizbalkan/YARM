// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! RP1 GPIO/Pinctrl register map — structural definitions for bare-metal MMIO.
//!
//! ## Memory layout (byte offsets from the PCIe BAR virtual base)
//!
//! ```text
//! IO-bank (GPIO_CTRL / GPIO_STATUS per pin):
//!   Bank 0 — BAR + 0x000D_0000  (28 pins, GPIO  0–27)
//!   Bank 1 — BAR + 0x000D_4000  (20 pins, GPIO 28–47)
//!   Bank 2 — BAR + 0x000D_8000  ( 6 pins, GPIO 48–53)
//!
//! SYS_RIO (atomic data-I/O and output-enable registers):
//!   Bank 0 — BAR + 0x000E_0000
//!   Bank 1 — BAR + 0x000E_4000
//!   Bank 2 — BAR + 0x000E_8000
//!
//!   Within each RIO bank, three alias pages provide atomic bit-manipulation
//!   without software read-modify-write:
//!     direct view  (+0x0000): plain read/write
//!     XOR alias    (+0x1000): writing 1 toggles the bit
//!     SET alias    (+0x2000): writing 1 sets   the bit   ← used for output-high / OE-enable
//!     CLR alias    (+0x3000): writing 1 clears the bit   ← used for output-low  / OE-disable
//!
//! PADS (per-pin IO-cell configuration):
//!   Bank 0 — BAR + 0x000F_0000
//!   Bank 1 — BAR + 0x000F_4000
//!   Bank 2 — BAR + 0x000F_8000
//! ```
//!
//! Sources: `pinctrl-rp1.c` (raspberrypi/linux), `rp1lib` (scottalford75),
//!          `rpi5-rp1-spi` (praktronics).

use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};

// ---------------------------------------------------------------------------
// Volatile register cell — the primitive for all MMIO access
// ---------------------------------------------------------------------------

/// A single 32-bit device register that must always be accessed through
/// `read_volatile` / `write_volatile`.
///
/// `UnsafeCell` signals interior mutability to the compiler; without it,
/// the optimizer would be free to eliminate or reorder writes that it
/// considers dead, corrupting hardware state.
///
/// `repr(transparent)` guarantees the same in-memory layout as a plain `u32`
/// so that `repr(C)` structs composed of `Reg` fields have the exact hardware
/// layout.
#[repr(transparent)]
pub struct Reg(UnsafeCell<u32>);

// SAFETY: The RP1 BAR is mapped with Device-nGnRE attributes; hardware
// serialises all accesses.  No Rust aliasing invariants are broken because
// every read/write goes through volatile intrinsics, and the BAR is exclusive
// to this driver process.
unsafe impl Sync for Reg {}

impl Reg {
    /// Volatile read — always loads from the device register.
    #[inline(always)]
    pub fn read(&self) -> u32 {
        // SAFETY: non-null, 4-byte aligned pointer into Device-nGnRE mapped MMIO.
        unsafe { read_volatile(self.0.get()) }
    }

    /// Volatile write — always stores to the device register.
    #[inline(always)]
    pub fn write(&self, val: u32) {
        // SAFETY: same as read.
        unsafe { write_volatile(self.0.get(), val) }
    }

    /// Read-modify-write with a closure.
    ///
    /// Non-atomic — caller must use the SYS_RIO SET/CLR aliases wherever
    /// atomicity matters (output data, output-enable).  This method is safe
    /// only for registers where no other agent modifies bits concurrently,
    /// e.g. GPIO_CTRL (only this driver writes FUNCSEL).
    #[inline(always)]
    pub fn modify<F: FnOnce(u32) -> u32>(&self, f: F) {
        self.write(f(self.read()));
    }
}

// ---------------------------------------------------------------------------
// IO-bank register layout
// ---------------------------------------------------------------------------

/// Status and control registers for one GPIO pin (8 bytes per pin).
///
/// Hardware layout:
/// ```text
///   +0: GPIO_STATUS  — read-only; sampled signal levels, IRQ flags
///   +4: GPIO_CTRL    — function select, output/input override fields
/// ```
#[repr(C)]
pub struct GpioPinRegs {
    /// GPIO_STATUS: read-only view of the current pin state.
    pub status: Reg,
    /// GPIO_CTRL: FUNCSEL[4:0], OUTOVER[13:12], OEOVER[15:14], INOVER[17:16],
    ///            IRQOVER[29:28].
    pub ctrl: Reg,
}

/// One complete IO-bank — up to 32 `GpioPinRegs` entries.
///
/// Only the first N entries are connected to real silicon per bank
/// (28 / 20 / 6).  Accesses beyond the wired count target reserved space
/// and must be avoided; the driver enforces this in `pin_location()`.
#[repr(C)]
pub struct GpioBankRegs {
    pub gpio: [GpioPinRegs; 32],
}

// ---------------------------------------------------------------------------
// SYS_RIO register layout — direct view and four alias pages
// ---------------------------------------------------------------------------

/// The three fundamental SYS_RIO data registers occupying 12 bytes.
///
/// This same triple appears at four different page offsets within each RIO
/// bank, providing direct access and three atomic-alias views (XOR / SET /
/// CLR).  `SysRioBankFull` exposes all four views as named fields.
#[repr(C)]
pub struct SysRioRegs {
    /// OUT: output data latch (the value driven to the pin when OE = 1).
    pub out: Reg,
    /// OE: output-enable latch (1 = pin is an output, 0 = input / hi-Z).
    pub oe: Reg,
    /// IN: synchronised (2-cycle) sample of all pad inputs (read-only).
    pub r#in: Reg,
}

/// The full 16 KiB SYS_RIO window for one bank, including all four alias
/// pages, laid out as a single `repr(C)` struct so field access replaces
/// manual pointer arithmetic.
///
/// ```text
/// Offset    Content
/// ──────────────────────────────────────────────
/// 0x0000    SysRioRegs::direct  (plain r/w)
/// 0x0001    …padding…
/// 0x1000    SysRioRegs::xor     (bit-toggle alias)
/// 0x2000    SysRioRegs::set     (bit-set   alias)
/// 0x3000    SysRioRegs::clr     (bit-clear alias)
/// ```
///
/// Total struct size: 4 × 0x1000 bytes = 0x4000 (one bank stride).
#[repr(C)]
pub struct SysRioBankFull {
    /// Direct read/write — use only for full-register writes.
    pub direct: SysRioRegs,
    _pad0: [u32; (0x1000 - 12) / 4], // pads direct → 0x1000
    /// XOR alias: writing a `1` bit atomically toggles that bit.
    pub xor: SysRioRegs,
    _pad1: [u32; (0x1000 - 12) / 4], // pads xor → 0x2000
    /// SET alias: writing a `1` bit atomically sets that bit.
    /// Use for: drive output high (`set.out`), enable output (`set.oe`).
    pub set: SysRioRegs,
    _pad2: [u32; (0x1000 - 12) / 4], // pads set → 0x3000
    /// CLR alias: writing a `1` bit atomically clears that bit.
    /// Use for: drive output low (`clr.out`), disable output (`clr.oe`).
    pub clr: SysRioRegs,
    _pad3: [u32; (0x1000 - 12) / 4], // pads clr → end of bank (0x4000)
}

// ---------------------------------------------------------------------------
// PADS bank register layout
// ---------------------------------------------------------------------------

/// Per-GPIO pad / IO-cell control registers.
///
/// Register 0 is a bank-wide voltage-select; registers 1…N are per-GPIO.
///
/// Pad register bit fields (per-GPIO entry):
/// ```text
///   [0]   SLEWFAST — fast slew rate (reduces edge time, increases EMI)
///   [1]   SCHMITT  — Schmitt-trigger hysteresis on the input stage
///   [2]   PD       — pull-down enable
///   [3]   PU       — pull-up enable
///   [5:4] DRIVE    — drive strength: 00=2 mA, 01=4 mA, 10=8 mA, 11=12 mA
///   [6]   IE       — input-buffer enable (must be 1 to read pin level)
///   [7]   OD       — output-disable (tri-state the pad, independent of OE)
///   [8]   ISO      — isolation (forces pad low during power-domain shutdown)
/// ```
#[repr(C)]
pub struct PadsBankRegs {
    /// VOLTAGE_SELECT: 0 = 3.3 V, 1 = 1.8 V (applies to entire bank).
    pub voltage: Reg,
    /// Per-GPIO pad control, indexed by pin-within-bank (0..N-1).
    pub gpio: [Reg; 32],
}

// ---------------------------------------------------------------------------
// BAR-relative byte offsets for each peripheral block
// ---------------------------------------------------------------------------

/// IO-bank offsets from the PCIe BAR virtual base.
pub const GPIO_BANK0_OFFSET: usize = 0x000D_0000;
pub const GPIO_BANK1_OFFSET: usize = 0x000D_4000;
pub const GPIO_BANK2_OFFSET: usize = 0x000D_8000;

/// SYS_RIO offsets from the PCIe BAR virtual base.
pub const RIO_BANK0_OFFSET: usize = 0x000E_0000;
pub const RIO_BANK1_OFFSET: usize = 0x000E_4000;
pub const RIO_BANK2_OFFSET: usize = 0x000E_8000;

/// PADS offsets from the PCIe BAR virtual base.
pub const PADS_BANK0_OFFSET: usize = 0x000F_0000;
pub const PADS_BANK1_OFFSET: usize = 0x000F_4000;
pub const PADS_BANK2_OFFSET: usize = 0x000F_8000;

// ---------------------------------------------------------------------------
// GPIO_CTRL field definitions
// ---------------------------------------------------------------------------

/// Bit-field masks and shift counts for the GPIO_CTRL register.
pub mod gpio_ctrl {
    /// FUNCSEL[4:0] — peripheral function select.
    /// 0 = NULL/disconnected, 5 = GPIO (SYS_RIO), 0x1f = tri-state.
    pub const FUNCSEL_SHIFT: u32 = 0;
    pub const FUNCSEL_MASK: u32 = 0x1f << FUNCSEL_SHIFT;

    /// OUTOVER[13:12] — override applied to the output data path.
    pub const OUTOVER_SHIFT: u32 = 12;
    pub const OUTOVER_MASK: u32 = 0x3 << OUTOVER_SHIFT;
    /// Peripheral drives the output.
    pub const OUTOVER_PERIPH: u32 = 0 << OUTOVER_SHIFT;
    /// Inverted peripheral drives the output.
    pub const OUTOVER_INVERT: u32 = 1 << OUTOVER_SHIFT;
    /// Force output low regardless of peripheral.
    pub const OUTOVER_LOW: u32 = 2 << OUTOVER_SHIFT;
    /// Force output high regardless of peripheral.
    pub const OUTOVER_HIGH: u32 = 3 << OUTOVER_SHIFT;

    /// OEOVER[15:14] — output-enable override.
    pub const OEOVER_SHIFT: u32 = 14;
    pub const OEOVER_MASK: u32 = 0x3 << OEOVER_SHIFT;
    /// OE controlled by peripheral (SYS_RIO in GPIO mode).
    pub const OEOVER_PERIPH: u32 = 0 << OEOVER_SHIFT;
    pub const OEOVER_INVERT: u32 = 1 << OEOVER_SHIFT;
    /// Force output disabled (input / hi-Z).
    pub const OEOVER_DISABLE: u32 = 2 << OEOVER_SHIFT;
    /// Force output always enabled.
    pub const OEOVER_ENABLE: u32 = 3 << OEOVER_SHIFT;

    /// INOVER[17:16] — input override.
    pub const INOVER_SHIFT: u32 = 16;
    pub const INOVER_MASK: u32 = 0x3 << INOVER_SHIFT;
    /// Normal input from pad.
    pub const INOVER_PERIPH: u32 = 0 << INOVER_SHIFT;

    /// IRQOVER[29:28] — IRQ status override.
    pub const IRQOVER_SHIFT: u32 = 28;
    pub const IRQOVER_MASK: u32 = 0x3 << IRQOVER_SHIFT;
    pub const IRQOVER_PERIPH: u32 = 0 << IRQOVER_SHIFT;
}

// ---------------------------------------------------------------------------
// Function select values for GPIO_CTRL.FUNCSEL
// ---------------------------------------------------------------------------

/// Named FUNCSEL constants for the RP1 GPIO controller.
pub mod fsel {
    /// Function 0: NULL — pin disconnected from all peripherals.
    pub const NULL: u32 = 0;
    pub const ALT1: u32 = 1;
    pub const ALT2: u32 = 2;
    pub const ALT3: u32 = 3;
    pub const ALT4: u32 = 4;
    /// Function 5: GPIO — SYS_RIO owns output data, OE, and input sample.
    pub const GPIO: u32 = 5;
    pub const ALT6: u32 = 6;
    pub const ALT7: u32 = 7;
    pub const ALT8: u32 = 8;
    /// Function 31 (0x1f): tri-state — all overrides inactive, pin floats.
    pub const NONE: u32 = 0x1f;
}

// ---------------------------------------------------------------------------
// Pad control bit definitions
// ---------------------------------------------------------------------------

/// Bit-field constants for the per-GPIO PADS register.
pub mod pad_ctrl {
    pub const SLEWFAST: u32 = 1 << 0;
    pub const SCHMITT: u32 = 1 << 1;
    pub const PULL_DOWN: u32 = 1 << 2;
    pub const PULL_UP: u32 = 1 << 3;

    pub const DRIVE_SHIFT: u32 = 4;
    pub const DRIVE_MASK: u32 = 0x3 << DRIVE_SHIFT;
    pub const DRIVE_2MA: u32 = 0 << DRIVE_SHIFT;
    pub const DRIVE_4MA: u32 = 1 << DRIVE_SHIFT;
    pub const DRIVE_8MA: u32 = 2 << DRIVE_SHIFT;
    pub const DRIVE_12MA: u32 = 3 << DRIVE_SHIFT;

    /// IE: input-buffer enable — must be set for `SYS_RIO::IN` to reflect the
    /// external pad voltage.
    pub const INPUT_ENABLE: u32 = 1 << 6;
    /// OD: output-disable — tri-states the pad output driver independently of OE.
    pub const OUTPUT_DISABLE: u32 = 1 << 7;
    /// ISO: isolation — forces the pad low during power-domain power-down sequences.
    pub const ISO: u32 = 1 << 8;
}

// ---------------------------------------------------------------------------
// Compile-time layout assertions
// ---------------------------------------------------------------------------

// These fire at build time in both std and no_std configurations, catching
// any struct-layout regression before the binary reaches hardware.
const _: () = {
    use core::mem::{offset_of, size_of};

    assert!(size_of::<GpioPinRegs>() == 8);
    assert!(size_of::<GpioBankRegs>() == 256);
    assert!(size_of::<SysRioRegs>() == 12);

    // Verify alias pages are at the correct offsets within SysRioBankFull.
    assert!(offset_of!(SysRioBankFull, xor) == 0x1000);
    assert!(offset_of!(SysRioBankFull, set) == 0x2000);
    assert!(offset_of!(SysRioBankFull, clr) == 0x3000);

    // Full bank size: 4 × 0x1000 bytes.
    assert!(size_of::<SysRioBankFull>() == 0x4000);
};
