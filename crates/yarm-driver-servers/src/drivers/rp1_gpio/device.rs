// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Safe, backend-abstracted RP1 GPIO device operations.
//!
//! The register model is protocol-ready, but production Raspberry Pi 5 / CM5
//! access still requires a platform path that discovers RP1 over PCIe and
//! grants this server an MMIO mapping. Hosted development uses only mock
//! backends and cannot construct the real volatile-MMIO backend.

use super::regs::{
    GPIO_BANK0_OFFSET, GPIO_BANK1_OFFSET, GPIO_BANK2_OFFSET, PADS_BANK0_OFFSET, PADS_BANK1_OFFSET,
    PADS_BANK2_OFFSET, RIO_BANK0_OFFSET, RIO_BANK1_OFFSET, RIO_BANK2_OFFSET, fsel, gpio_ctrl,
    pad_ctrl,
};

const BANK0_PINS: usize = 28;
const BANK1_PINS: usize = 20;
const BANK2_PINS: usize = 6;

/// Total number of GPIO pins across the three documented RP1 GPIO banks.
pub const TOTAL_GPIOS: usize = BANK0_PINS + BANK1_PINS + BANK2_PINS;

const GPIO_CTRL_OFFSET: usize = 4;
const GPIO_PIN_STRIDE: usize = 8;
const RIO_OUT_OFFSET: usize = 0;
const RIO_OE_OFFSET: usize = 4;
const RIO_IN_OFFSET: usize = 8;
const RIO_SET_ALIAS: usize = 0x2000;
const RIO_CLR_ALIAS: usize = 0x3000;
const PAD_GPIO0_OFFSET: usize = 4;
const PAD_PIN_STRIDE: usize = 4;

/// Device-level errors translated into deterministic ABI status replies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioError {
    InvalidPin,
    InvalidFunction,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pull {
    Off,
    Down,
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinMode {
    Input,
    Output { initial_level: bool },
    AltFunction(u32),
}

/// Minimal register transport used by the RP1 GPIO logic.
///
/// Implementations may perform volatile MMIO or provide deterministic hosted
/// mocks. Offsets are relative to the granted RP1 peripheral BAR mapping; the
/// device never embeds a BCM2712 physical address.
pub trait RegisterIo {
    fn read32(&self, offset: usize) -> u32;
    fn write32(&self, offset: usize, value: u32);
}

/// Operations consumed by the IPC dispatcher.
pub trait GpioDriver {
    fn set_function(&self, pin: usize, function: u32) -> Result<(), GpioError>;
    fn set_pin_mode(&self, pin: usize, mode: PinMode) -> Result<(), GpioError>;
    fn direction(&self, pin: usize) -> Result<Direction, GpioError>;
    fn write_pin(&self, pin: usize, level: bool) -> Result<(), GpioError>;
    fn read_pin(&self, pin: usize) -> Result<bool, GpioError>;
    fn set_pull(&self, pin: usize, pull: Pull) -> Result<(), GpioError>;
    fn pull(&self, pin: usize) -> Result<Pull, GpioError>;
}

/// Backend-independent RP1 GPIO device.
pub struct Rp1GpioDevice<B> {
    io: B,
}

impl<B> Rp1GpioDevice<B> {
    pub const fn new(io: B) -> Self {
        Self { io }
    }

    pub fn backend(&self) -> &B {
        &self.io
    }

    fn pin_location(pin: usize) -> Result<(usize, usize), GpioError> {
        if pin < BANK0_PINS {
            Ok((0, pin))
        } else if pin < BANK0_PINS + BANK1_PINS {
            Ok((1, pin - BANK0_PINS))
        } else if pin < TOTAL_GPIOS {
            Ok((2, pin - BANK0_PINS - BANK1_PINS))
        } else {
            Err(GpioError::InvalidPin)
        }
    }
}

impl<B: RegisterIo> Rp1GpioDevice<B> {
    fn locations(pin: usize) -> Result<(usize, usize, usize), GpioError> {
        let (bank, index) = Self::pin_location(pin)?;
        let gpio_base = [GPIO_BANK0_OFFSET, GPIO_BANK1_OFFSET, GPIO_BANK2_OFFSET][bank];
        let rio_base = [RIO_BANK0_OFFSET, RIO_BANK1_OFFSET, RIO_BANK2_OFFSET][bank];
        let pads_base = [PADS_BANK0_OFFSET, PADS_BANK1_OFFSET, PADS_BANK2_OFFSET][bank];
        Ok((
            gpio_base + index * GPIO_PIN_STRIDE + GPIO_CTRL_OFFSET,
            rio_base,
            pads_base + PAD_GPIO0_OFFSET + index * PAD_PIN_STRIDE,
        ))
    }

    fn update(&self, offset: usize, clear: u32, set: u32) {
        self.io
            .write32(offset, (self.io.read32(offset) & !clear) | set);
    }
}

impl<B: RegisterIo> GpioDriver for Rp1GpioDevice<B> {
    fn set_function(&self, pin: usize, function: u32) -> Result<(), GpioError> {
        if function > 0x1f {
            return Err(GpioError::InvalidFunction);
        }
        let (ctrl, _, _) = Self::locations(pin)?;
        self.update(
            ctrl,
            gpio_ctrl::FUNCSEL_MASK,
            function << gpio_ctrl::FUNCSEL_SHIFT,
        );
        Ok(())
    }

    fn set_pin_mode(&self, pin: usize, mode: PinMode) -> Result<(), GpioError> {
        let (ctrl, rio, pad) = Self::locations(pin)?;
        let (_, index) = Self::pin_location(pin)?;
        let mask = 1u32 << index;

        match mode {
            PinMode::Input => {
                self.io.write32(rio + RIO_CLR_ALIAS + RIO_OE_OFFSET, mask);
                self.update(ctrl, gpio_ctrl::FUNCSEL_MASK, fsel::GPIO);
                self.update(pad, pad_ctrl::OUTPUT_DISABLE, pad_ctrl::INPUT_ENABLE);
            }
            PinMode::Output { initial_level } => {
                let alias = if initial_level {
                    RIO_SET_ALIAS
                } else {
                    RIO_CLR_ALIAS
                };
                self.io.write32(rio + alias + RIO_OUT_OFFSET, mask);
                self.update(ctrl, gpio_ctrl::FUNCSEL_MASK, fsel::GPIO);
                self.io.write32(rio + RIO_SET_ALIAS + RIO_OE_OFFSET, mask);
            }
            PinMode::AltFunction(function) => {
                if !matches!(function, 1..=4 | 6..=8) {
                    return Err(GpioError::InvalidFunction);
                }
                self.io.write32(rio + RIO_CLR_ALIAS + RIO_OE_OFFSET, mask);
                self.update(
                    ctrl,
                    gpio_ctrl::FUNCSEL_MASK,
                    function << gpio_ctrl::FUNCSEL_SHIFT,
                );
            }
        }
        Ok(())
    }

    fn direction(&self, pin: usize) -> Result<Direction, GpioError> {
        let (_, rio, _) = Self::locations(pin)?;
        let (_, index) = Self::pin_location(pin)?;
        Ok(
            if self.io.read32(rio + RIO_OE_OFFSET) & (1u32 << index) != 0 {
                Direction::Output
            } else {
                Direction::Input
            },
        )
    }

    fn write_pin(&self, pin: usize, level: bool) -> Result<(), GpioError> {
        let (_, rio, _) = Self::locations(pin)?;
        let (_, index) = Self::pin_location(pin)?;
        let alias = if level { RIO_SET_ALIAS } else { RIO_CLR_ALIAS };
        self.io.write32(rio + alias + RIO_OUT_OFFSET, 1u32 << index);
        Ok(())
    }

    fn read_pin(&self, pin: usize) -> Result<bool, GpioError> {
        let (_, rio, _) = Self::locations(pin)?;
        let (_, index) = Self::pin_location(pin)?;
        Ok(self.io.read32(rio + RIO_IN_OFFSET) & (1u32 << index) != 0)
    }

    fn set_pull(&self, pin: usize, pull: Pull) -> Result<(), GpioError> {
        let (_, _, pad) = Self::locations(pin)?;
        let set = match pull {
            Pull::Off => 0,
            Pull::Down => pad_ctrl::PULL_DOWN,
            Pull::Up => pad_ctrl::PULL_UP,
        };
        self.update(pad, pad_ctrl::PULL_DOWN | pad_ctrl::PULL_UP, set);
        Ok(())
    }

    fn pull(&self, pin: usize) -> Result<Pull, GpioError> {
        let (_, _, pad) = Self::locations(pin)?;
        match self.io.read32(pad) & (pad_ctrl::PULL_DOWN | pad_ctrl::PULL_UP) {
            0 => Ok(Pull::Off),
            pad_ctrl::PULL_DOWN => Ok(Pull::Down),
            pad_ctrl::PULL_UP => Ok(Pull::Up),
            _ => Err(GpioError::Unsupported),
        }
    }
}

/// Real volatile BAR backend. It is deliberately unavailable in hosted-dev,
/// ensuring hosted tests cannot dereference production MMIO.
#[cfg(not(feature = "hosted-dev"))]
pub struct VolatileMmio {
    base: usize,
}

#[cfg(not(feature = "hosted-dev"))]
impl VolatileMmio {
    /// # Safety
    /// `base` must be a platform-discovered, capability-granted RP1 PCIe BAR
    /// mapping covering every offset used by this module.
    pub unsafe fn from_granted_bar(base: usize) -> Self {
        Self { base }
    }
}

#[cfg(not(feature = "hosted-dev"))]
impl RegisterIo for VolatileMmio {
    fn read32(&self, offset: usize) -> u32 {
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u32) }
    }

    fn write32(&self, offset: usize, value: u32) {
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u32, value) }
    }
}

#[cfg(not(feature = "hosted-dev"))]
pub type Rp1GpioDriver = Rp1GpioDevice<VolatileMmio>;

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use core::cell::RefCell;
    use std::collections::BTreeMap;
    use std::vec::Vec;

    #[derive(Default)]
    struct MockIo {
        registers: RefCell<BTreeMap<usize, u32>>,
        writes: RefCell<Vec<(usize, u32)>>,
    }

    impl MockIo {
        fn put(&self, offset: usize, value: u32) {
            self.registers.borrow_mut().insert(offset, value);
        }

        fn writes(&self) -> Vec<(usize, u32)> {
            self.writes.borrow().clone()
        }
    }

    impl RegisterIo for MockIo {
        fn read32(&self, offset: usize) -> u32 {
            *self.registers.borrow().get(&offset).unwrap_or(&0)
        }

        fn write32(&self, offset: usize, value: u32) {
            self.writes.borrow_mut().push((offset, value));
            self.registers.borrow_mut().insert(offset, value);

            let page = offset & 0x3000;
            if page == RIO_SET_ALIAS || page == RIO_CLR_ALIAS {
                let direct = offset - page;
                let old = *self.registers.borrow().get(&direct).unwrap_or(&0);
                let new = if page == RIO_SET_ALIAS {
                    old | value
                } else {
                    old & !value
                };
                self.registers.borrow_mut().insert(direct, new);
            }
        }
    }

    #[test]
    fn validates_all_pin_boundaries() {
        for pin in [0, 27, 28, 47, 48, 53] {
            assert!(Rp1GpioDevice::<MockIo>::pin_location(pin).is_ok());
        }
        assert_eq!(
            Rp1GpioDevice::<MockIo>::pin_location(54),
            Err(GpioError::InvalidPin)
        );
        assert_eq!(
            Rp1GpioDevice::<MockIo>::pin_location(usize::MAX),
            Err(GpioError::InvalidPin)
        );
    }

    #[test]
    fn direction_set_get_uses_exact_rio_alias_bits() {
        let dev = Rp1GpioDevice::new(MockIo::default());
        dev.set_pin_mode(
            28,
            PinMode::Output {
                initial_level: true,
            },
        )
        .unwrap();
        assert_eq!(dev.direction(28), Ok(Direction::Output));
        assert!(
            dev.backend()
                .writes()
                .contains(&(RIO_BANK1_OFFSET + RIO_SET_ALIAS, 1))
        );
        assert!(
            dev.backend()
                .writes()
                .contains(&(RIO_BANK1_OFFSET + RIO_SET_ALIAS + RIO_OE_OFFSET, 1))
        );

        dev.set_pin_mode(28, PinMode::Input).unwrap();
        assert_eq!(dev.direction(28), Ok(Direction::Input));
        assert!(
            dev.backend()
                .writes()
                .contains(&(RIO_BANK1_OFFSET + RIO_CLR_ALIAS + RIO_OE_OFFSET, 1))
        );
    }

    #[test]
    fn level_write_and_read_use_exact_bank_bit() {
        let dev = Rp1GpioDevice::new(MockIo::default());
        dev.write_pin(53, true).unwrap();
        assert!(
            dev.backend()
                .writes()
                .contains(&(RIO_BANK2_OFFSET + RIO_SET_ALIAS, 1 << 5))
        );
        dev.backend().put(RIO_BANK2_OFFSET + RIO_IN_OFFSET, 1 << 5);
        assert_eq!(dev.read_pin(53), Ok(true));
        dev.write_pin(53, false).unwrap();
        assert!(
            dev.backend()
                .writes()
                .contains(&(RIO_BANK2_OFFSET + RIO_CLR_ALIAS, 1 << 5))
        );
    }

    #[test]
    fn pull_config_preserves_unrelated_pad_bits() {
        let dev = Rp1GpioDevice::new(MockIo::default());
        let pad = PADS_BANK0_OFFSET + PAD_GPIO0_OFFSET + 7 * PAD_PIN_STRIDE;
        dev.backend()
            .put(pad, pad_ctrl::SCHMITT | pad_ctrl::PULL_DOWN);
        dev.set_pull(7, Pull::Up).unwrap();
        assert_eq!(dev.pull(7), Ok(Pull::Up));
        assert_eq!(
            dev.backend().read32(pad),
            pad_ctrl::SCHMITT | pad_ctrl::PULL_UP
        );
        dev.set_pull(7, Pull::Off).unwrap();
        assert_eq!(dev.pull(7), Ok(Pull::Off));
    }

    #[test]
    fn invalid_functions_are_rejected_before_mmio() {
        let dev = Rp1GpioDevice::new(MockIo::default());
        assert_eq!(dev.set_function(0, 32), Err(GpioError::InvalidFunction));
        assert_eq!(
            dev.set_pin_mode(0, PinMode::AltFunction(5)),
            Err(GpioError::InvalidFunction)
        );
        assert!(dev.backend().writes().is_empty());
    }

    #[test]
    fn conflicting_pull_bits_are_explicitly_unsupported() {
        let dev = Rp1GpioDevice::new(MockIo::default());
        let pad = PADS_BANK0_OFFSET + PAD_GPIO0_OFFSET;
        dev.backend()
            .put(pad, pad_ctrl::PULL_DOWN | pad_ctrl::PULL_UP);
        assert_eq!(dev.pull(0), Err(GpioError::Unsupported));
    }

    #[test]
    fn hosted_build_has_no_real_mmio_backend() {
        assert!(cfg!(feature = "hosted-dev"));
        let source = include_str!("device.rs");
        assert!(source.contains("#[cfg(not(feature = \"hosted-dev\"))]\npub struct VolatileMmio"));
    }
}
