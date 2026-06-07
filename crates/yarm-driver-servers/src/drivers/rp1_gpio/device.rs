// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! `Rp1GpioDriver` — MMIO driver for the RP1 GPIO/Pinctrl peripheral.
//!
//! The driver-manager maps the RP1 PCIe BAR into this process's address space
//! with Device-nGnRE memory attributes before calling `Rp1GpioDriver::new`.
//! Every register field is accessed through named `repr(C)` struct fields
//! rather than raw pointer arithmetic; unsafe is confined to the three
//! pointer-cast helpers (`gpio_bank`, `rio_bank`, `pads_bank`).
//!
//! Hardware sequencing follows `pinctrl-rp1.c` (raspberrypi/linux).

use super::regs::{
    fsel, gpio_ctrl, pad_ctrl, GpioBankRegs, PadsBankRegs, SysRioBankFull,
    GPIO_BANK0_OFFSET, GPIO_BANK1_OFFSET, GPIO_BANK2_OFFSET,
    PADS_BANK0_OFFSET, PADS_BANK1_OFFSET, PADS_BANK2_OFFSET,
    RIO_BANK0_OFFSET, RIO_BANK1_OFFSET, RIO_BANK2_OFFSET,
};

// ---------------------------------------------------------------------------
// Bank geometry (from pinctrl-rp1.c `rp1_gpio_pin_banks[]`)
// ---------------------------------------------------------------------------

const BANK0_PINS: usize = 28; // GPIO  0–27  (40-pin header)
const BANK1_PINS: usize = 20; // GPIO 28–47  (internal / extended)
const BANK2_PINS: usize = 6;  // GPIO 48–53  (internal)

/// Total number of GPIO pins across all three RP1 IO-banks.
pub const TOTAL_GPIOS: usize = BANK0_PINS + BANK1_PINS + BANK2_PINS; // 54

// ---------------------------------------------------------------------------
// Public API types
// ---------------------------------------------------------------------------

/// Operating mode for a GPIO pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinMode {
    /// High-impedance digital input.
    ///
    /// Clears SYS_RIO OE, selects FUNCSEL=GPIO, enables the pad input buffer.
    Input,

    /// Push-pull digital output.
    ///
    /// Pre-drives the latch to `initial_level` before asserting OE to prevent
    /// a glitch on the pin at the moment the output is enabled.
    Output { initial_level: bool },

    /// Peripheral alternate function (`fsel` values 1–4, 6–8).
    ///
    /// Clears SYS_RIO OE first so the peripheral's own OE takes control,
    /// then writes FUNCSEL.  Pad configuration is left as-is.
    AltFunction(u32),
}

// ---------------------------------------------------------------------------
// Driver trait
// ---------------------------------------------------------------------------

/// Core GPIO operations exposed to the IPC service layer.
///
/// All methods take `&self`; interior mutability is achieved through the
/// `Reg` volatile wrapper.
pub trait GpioDriver {
    /// Write the raw FUNCSEL field of GPIO_CTRL for `pin` (global pin number).
    fn set_function(&self, pin: usize, function: u32);

    /// Configure `pin` direction, OE, and pad input-buffer.
    fn set_pin_mode(&self, pin: usize, mode: PinMode);

    /// Drive `pin` to `level` using the SYS_RIO atomic SET/CLR alias.
    fn write_pin(&self, pin: usize, level: bool);

    /// Sample the current digital level of `pin` from SYS_RIO IN.
    fn read_pin(&self, pin: usize) -> bool;
}

// ---------------------------------------------------------------------------
// Driver struct
// ---------------------------------------------------------------------------

/// MMIO driver for the RP1 GPIO/Pinctrl peripheral block.
///
/// Holds three raw pointer arrays — one per register window (IO-bank,
/// SYS_RIO, PADS) × three banks — initialised from the BAR virtual address
/// supplied by the driver-manager.
///
/// No heap allocation is used; the struct lives on the server task's stack.
pub struct Rp1GpioDriver {
    gpio:  [*mut GpioBankRegs;   3],
    rio:   [*mut SysRioBankFull; 3],
    pads:  [*mut PadsBankRegs;   3],
}

// SAFETY: The RP1 PCIe BAR is mapped exclusively into this server process's
// address space by the driver-manager.  No other Rust task or thread holds a
// pointer into the BAR.
unsafe impl Send for Rp1GpioDriver {}
unsafe impl Sync for Rp1GpioDriver {}

impl Rp1GpioDriver {
    /// Construct the driver from the BAR virtual address.
    ///
    /// # Safety
    ///
    /// `bar_base_vaddr` must point to a Device-nGnRE mapped region that
    /// covers at least the first 1 MiB (0x10_0000 bytes) of the RP1
    /// peripheral space.  The driver-manager validates this before spawning
    /// the server and passing the pointer.
    pub unsafe fn new(bar_base_vaddr: usize) -> Self {
        let b = bar_base_vaddr;
        Self {
            gpio: [
                (b + GPIO_BANK0_OFFSET) as *mut GpioBankRegs,
                (b + GPIO_BANK1_OFFSET) as *mut GpioBankRegs,
                (b + GPIO_BANK2_OFFSET) as *mut GpioBankRegs,
            ],
            rio: [
                (b + RIO_BANK0_OFFSET) as *mut SysRioBankFull,
                (b + RIO_BANK1_OFFSET) as *mut SysRioBankFull,
                (b + RIO_BANK2_OFFSET) as *mut SysRioBankFull,
            ],
            pads: [
                (b + PADS_BANK0_OFFSET) as *mut PadsBankRegs,
                (b + PADS_BANK1_OFFSET) as *mut PadsBankRegs,
                (b + PADS_BANK2_OFFSET) as *mut PadsBankRegs,
            ],
        }
    }

    // -----------------------------------------------------------------------
    // Private register-window accessors
    // -----------------------------------------------------------------------

    /// Decompose a global GPIO number into `(bank_index, pin_within_bank)`.
    ///
    /// Panics in debug builds when `pin >= TOTAL_GPIOS`; in release the
    /// caller must guarantee valid pin numbers (enforced by the ABI decoder).
    #[inline]
    fn pin_location(pin: usize) -> (usize, usize) {
        debug_assert!(pin < TOTAL_GPIOS, "RP1: GPIO pin index out of range");
        if pin < BANK0_PINS {
            (0, pin)
        } else if pin < BANK0_PINS + BANK1_PINS {
            (1, pin - BANK0_PINS)
        } else {
            (2, pin - BANK0_PINS - BANK1_PINS)
        }
    }

    /// Shared reference to one IO-bank's register block.
    ///
    /// # Safety (encapsulated)
    /// Pointer was set from a valid BAR mapping in `new`; no `&mut` aliases exist.
    #[inline]
    fn gpio_bank(&self, bank: usize) -> &GpioBankRegs {
        unsafe { &*self.gpio[bank] }
    }

    /// Shared reference to one SYS_RIO bank (all alias pages).
    #[inline]
    fn rio_bank(&self, bank: usize) -> &SysRioBankFull {
        unsafe { &*self.rio[bank] }
    }

    /// Shared reference to one PADS bank.
    #[inline]
    fn pads_bank(&self, bank: usize) -> &PadsBankRegs {
        unsafe { &*self.pads[bank] }
    }
}

// ---------------------------------------------------------------------------
// GpioDriver implementation
// ---------------------------------------------------------------------------

impl GpioDriver for Rp1GpioDriver {
    /// Select a peripheral function for `pin`.
    ///
    /// Hardware sequence (`pinctrl-rp1.c: rp1_gpio_set_function`):
    ///   1. Read GPIO_CTRL.
    ///   2. Clear FUNCSEL[4:0].
    ///   3. Insert the new function code.
    ///   4. Write back in a single volatile store (avoids partial-update windows).
    fn set_function(&self, pin: usize, function: u32) {
        let (bank, idx) = Self::pin_location(pin);
        self.gpio_bank(bank).gpio[idx].ctrl.modify(|v| {
            (v & !gpio_ctrl::FUNCSEL_MASK)
                | ((function << gpio_ctrl::FUNCSEL_SHIFT) & gpio_ctrl::FUNCSEL_MASK)
        });
    }

    /// Configure `pin` direction and pad parameters.
    ///
    /// ### Input sequence (`pinctrl-rp1.c: rp1_gpio_direction_input`)
    ///   1. Atomically clear OE via the SYS_RIO CLR alias — output is
    ///      tri-stated *before* any function-select change to prevent driving
    ///      an unintended level during the transition.
    ///   2. Set FUNCSEL = GPIO (5) in GPIO_CTRL so SYS_RIO owns the pin.
    ///   3. Enable the pad input buffer (IE bit) so the IN register tracks
    ///      the external pad voltage.  Clear OD (output-disable in the pad)
    ///      since direction is now managed through SYS_RIO OE instead.
    ///
    /// ### Output sequence (`pinctrl-rp1.c: rp1_gpio_direction_output`)
    ///   1. Pre-drive the output latch to `initial_level` via the atomic
    ///      SYS_RIO SET or CLR alias — the pin level is committed before
    ///      OE is asserted, eliminating the output-enable glitch.
    ///   2. Set FUNCSEL = GPIO (5) in GPIO_CTRL.
    ///   3. Atomically assert OE via the SYS_RIO SET alias — the output
    ///      driver turns on at the already-correct level.
    ///
    /// ### Alternate-function sequence
    ///   1. Atomically clear OE (SYS_RIO CLR) — releases the pin from
    ///      GPIO data-path control so the peripheral's own OE takes over.
    ///   2. Write FUNCSEL to the requested alternate function code.
    ///      Pad bias / drive settings are intentionally left unchanged;
    ///      the pinmux policy layer is responsible for those.
    fn set_pin_mode(&self, pin: usize, mode: PinMode) {
        let (bank, idx) = Self::pin_location(pin);
        let mask = 1u32 << idx;

        match mode {
            PinMode::Input => {
                // Step 1: tri-state before function change.
                self.rio_bank(bank).clr.oe.write(mask);

                // Step 2: switch GPIO_CTRL to GPIO function.
                self.gpio_bank(bank).gpio[idx].ctrl.modify(|v| {
                    (v & !gpio_ctrl::FUNCSEL_MASK) | fsel::GPIO
                });

                // Step 3: enable pad input buffer; clear OD (output-disable).
                self.pads_bank(bank).gpio[idx].modify(|v| {
                    (v & !pad_ctrl::OUTPUT_DISABLE) | pad_ctrl::INPUT_ENABLE
                });
            }

            PinMode::Output { initial_level } => {
                // Step 1: commit output level before enabling the driver.
                if initial_level {
                    self.rio_bank(bank).set.out.write(mask);
                } else {
                    self.rio_bank(bank).clr.out.write(mask);
                }

                // Step 2: switch GPIO_CTRL to GPIO function.
                self.gpio_bank(bank).gpio[idx].ctrl.modify(|v| {
                    (v & !gpio_ctrl::FUNCSEL_MASK) | fsel::GPIO
                });

                // Step 3: enable the output driver atomically.
                self.rio_bank(bank).set.oe.write(mask);
            }

            PinMode::AltFunction(func) => {
                // Step 1: release SYS_RIO ownership of OE.
                self.rio_bank(bank).clr.oe.write(mask);

                // Step 2: select the peripheral.
                self.gpio_bank(bank).gpio[idx].ctrl.modify(|v| {
                    (v & !gpio_ctrl::FUNCSEL_MASK)
                        | ((func << gpio_ctrl::FUNCSEL_SHIFT) & gpio_ctrl::FUNCSEL_MASK)
                });
            }
        }
    }

    /// Drive `pin` high or low using the SYS_RIO atomic SET/CLR alias.
    ///
    /// Writing `mask` to the SET alias sets exactly those bits in the OUT
    /// register without disturbing any other pin in the bank (RP1 RIO
    /// atomicity guarantee).  No read-modify-write is performed.
    fn write_pin(&self, pin: usize, level: bool) {
        let (bank, idx) = Self::pin_location(pin);
        let mask = 1u32 << idx;
        if level {
            self.rio_bank(bank).set.out.write(mask);
        } else {
            self.rio_bank(bank).clr.out.write(mask);
        }
    }

    /// Return `true` if `pin` is currently high.
    ///
    /// Reads from SYS_RIO IN, which presents a 2-cycle synchronised sample
    /// of all pad inputs.  For output pins this reflects the driven value
    /// (or an overdriven external voltage if something fights the output).
    fn read_pin(&self, pin: usize) -> bool {
        let (bank, idx) = Self::pin_location(pin);
        (self.rio_bank(bank).direct.r#in.read() >> idx) & 1 != 0
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_location_bank0_boundary() {
        assert_eq!(Rp1GpioDriver::pin_location(0), (0, 0));
        assert_eq!(Rp1GpioDriver::pin_location(27), (0, 27));
    }

    #[test]
    fn pin_location_bank1_boundary() {
        assert_eq!(Rp1GpioDriver::pin_location(28), (1, 0));
        assert_eq!(Rp1GpioDriver::pin_location(47), (1, 19));
    }

    #[test]
    fn pin_location_bank2_boundary() {
        assert_eq!(Rp1GpioDriver::pin_location(48), (2, 0));
        assert_eq!(Rp1GpioDriver::pin_location(53), (2, 5));
    }

    #[test]
    fn total_gpio_count_matches_bank_sum() {
        assert_eq!(TOTAL_GPIOS, 54);
        assert_eq!(BANK0_PINS + BANK1_PINS + BANK2_PINS, TOTAL_GPIOS);
    }
}
