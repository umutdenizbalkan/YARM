// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Mockable userspace PL011 UART device.
//!
//! The device is nonblocking: it never spins waiting for FIFO state. Hosted
//! development can only use trait-backed mocks; volatile MMIO is compiled out
//! whenever `hosted-dev` is enabled.

use super::regs::{self, cr, fr, lcrh};
use crate::drivers::uart::service::{UartDeviceError, UartDeviceOps};

pub type UartError = UartDeviceError;

/// Data-only PL011 configuration. Clock/baud policy is expected to calculate
/// and validate these divisors before granting the device to this server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pl011Config {
    pub integer_divisor: u16,
    pub fractional_divisor: u8,
    pub fifo_enabled: bool,
    pub tx_enabled: bool,
    pub rx_enabled: bool,
}

impl Pl011Config {
    pub const fn data_8n1(integer_divisor: u16, fractional_divisor: u8) -> Self {
        Self {
            integer_divisor,
            fractional_divisor,
            fifo_enabled: true,
            tx_enabled: true,
            rx_enabled: true,
        }
    }

    fn validate(self) -> Result<(), UartError> {
        if self.integer_divisor == 0
            || self.fractional_divisor > 63
            || (!self.tx_enabled && !self.rx_enabled)
        {
            Err(UartError::InvalidConfig)
        } else {
            Ok(())
        }
    }
}

/// Register transport for PL011 offsets relative to an already-granted MMIO
/// mapping. Implementations must not add an assumed physical base address.
pub trait UartRegisterIo {
    fn read32(&self, offset: usize) -> u32;
    fn write32(&self, offset: usize, value: u32);
}

pub struct Pl011UartDevice<B> {
    registers: B,
}

impl<B> Pl011UartDevice<B> {
    pub const fn new(registers: B) -> Self {
        Self { registers }
    }

    pub fn backend(&self) -> &B {
        &self.registers
    }
}

impl<B: UartRegisterIo> Pl011UartDevice<B> {
    /// Apply a polling/data-only configuration without enabling interrupts.
    ///
    /// Sequence: disable, mask interrupts, clear pending interrupts, program
    /// divisors, program 8N1/FIFO format, then enable requested data paths.
    pub fn configure(&self, config: Pl011Config) -> Result<(), UartError> {
        config.validate()?;

        self.registers.write32(regs::CR, 0);
        self.registers.write32(regs::IMSC, 0);
        self.clear_interrupts();
        self.registers
            .write32(regs::IBRD, config.integer_divisor as u32);
        self.registers
            .write32(regs::FBRD, config.fractional_divisor as u32);

        let line_control = lcrh::WLEN_8 | if config.fifo_enabled { lcrh::FEN } else { 0 };
        self.registers.write32(regs::LCRH, line_control);

        let mut control = cr::UARTEN;
        if config.tx_enabled {
            control |= cr::TXE;
        }
        if config.rx_enabled {
            control |= cr::RXE;
        }
        self.registers.write32(regs::CR, control);
        Ok(())
    }

    pub fn tx_ready(&self) -> bool {
        self.registers.read32(regs::FR) & fr::TXFF == 0
    }

    pub fn write_byte(&self, byte: u8) -> Result<(), UartError> {
        if !self.tx_ready() {
            return Err(UartError::TxWouldBlock);
        }
        self.registers.write32(regs::DR, byte as u32);
        Ok(())
    }

    /// Write until all bytes are accepted or the FIFO reports full.
    pub fn write_bytes(&self, bytes: &[u8]) -> Result<usize, UartError> {
        let mut written = 0;
        for &byte in bytes {
            match self.write_byte(byte) {
                Ok(()) => written += 1,
                Err(UartError::TxWouldBlock) if written != 0 => return Ok(written),
                Err(error) => return Err(error),
            }
        }
        Ok(written)
    }

    pub fn rx_ready(&self) -> bool {
        self.registers.read32(regs::FR) & fr::RXFE == 0
    }

    pub fn read_byte_nonblocking(&self) -> Result<u8, UartError> {
        if !self.rx_ready() {
            return Err(UartError::RxWouldBlock);
        }
        Ok((self.registers.read32(regs::DR) & 0xff) as u8)
    }

    pub fn clear_interrupts(&self) {
        self.registers.write32(regs::ICR, regs::ICR_ALL);
    }

    pub fn set_tx_enabled(&self, enabled: bool) {
        self.update_control(cr::TXE, enabled);
    }

    pub fn set_rx_enabled(&self, enabled: bool) {
        self.update_control(cr::RXE, enabled);
    }

    fn update_control(&self, bit: u32, enabled: bool) {
        let current = self.registers.read32(regs::CR);
        let updated = if enabled {
            current | bit
        } else {
            current & !bit
        };
        self.registers.write32(regs::CR, updated);
    }
}

impl<B: UartRegisterIo> UartDeviceOps for Pl011UartDevice<B> {
    fn configure_8n1(
        &self,
        integer_divisor: u16,
        fractional_divisor: u8,
        fifo_enabled: bool,
    ) -> Result<(), UartDeviceError> {
        self.configure(Pl011Config {
            integer_divisor,
            fractional_divisor,
            fifo_enabled,
            tx_enabled: true,
            rx_enabled: true,
        })
    }

    fn write_byte(&self, byte: u8) -> Result<(), UartDeviceError> {
        Pl011UartDevice::write_byte(self, byte)
    }

    fn write_bytes(&self, bytes: &[u8]) -> Result<usize, UartDeviceError> {
        Pl011UartDevice::write_bytes(self, bytes)
    }

    fn read_byte_nonblocking(&self) -> Result<u8, UartDeviceError> {
        Pl011UartDevice::read_byte_nonblocking(self)
    }

    fn clear_interrupts(&self) {
        Pl011UartDevice::clear_interrupts(self);
    }
}

/// Volatile backend for a platform-granted UART mapping. No physical address
/// or discovery policy is embedded here.
#[cfg(not(feature = "hosted-dev"))]
pub struct VolatileUartMmio {
    base: usize,
}

#[cfg(not(feature = "hosted-dev"))]
impl VolatileUartMmio {
    /// # Safety
    /// `base` must be a validated, capability-granted PL011 MMIO mapping.
    pub unsafe fn from_granted_mapping(base: usize) -> Self {
        Self { base }
    }
}

#[cfg(not(feature = "hosted-dev"))]
impl UartRegisterIo for VolatileUartMmio {
    fn read32(&self, offset: usize) -> u32 {
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u32) }
    }

    fn write32(&self, offset: usize, value: u32) {
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u32, value) }
    }
}

/// Fixed-capacity hosted register model with an ordered write log.
#[cfg(any(test, feature = "hosted-dev"))]
pub struct MockUartRegisters {
    values: core::cell::RefCell<[u32; 18]>,
    writes: core::cell::RefCell<[(usize, u32); 32]>,
    write_count: core::cell::Cell<usize>,
}

#[cfg(any(test, feature = "hosted-dev"))]
impl Default for MockUartRegisters {
    fn default() -> Self {
        Self {
            values: core::cell::RefCell::new([0; 18]),
            writes: core::cell::RefCell::new([(0, 0); 32]),
            write_count: core::cell::Cell::new(0),
        }
    }
}

#[cfg(any(test, feature = "hosted-dev"))]
impl MockUartRegisters {
    pub fn set(&self, offset: usize, value: u32) {
        self.values.borrow_mut()[offset / 4] = value;
    }

    pub fn get(&self, offset: usize) -> u32 {
        self.values.borrow()[offset / 4]
    }

    pub fn write_count(&self) -> usize {
        self.write_count.get()
    }

    pub fn write_at(&self, index: usize) -> Option<(usize, u32)> {
        (index < self.write_count.get()).then(|| self.writes.borrow()[index])
    }
}

#[cfg(any(test, feature = "hosted-dev"))]
impl UartRegisterIo for MockUartRegisters {
    fn read32(&self, offset: usize) -> u32 {
        self.get(offset)
    }

    fn write32(&self, offset: usize, value: u32) {
        self.values.borrow_mut()[offset / 4] = value;
        let index = self.write_count.get();
        assert!(index < 32, "mock PL011 write log capacity exceeded");
        self.writes.borrow_mut()[index] = (offset, value);
        self.write_count.set(index + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> Pl011UartDevice<MockUartRegisters> {
        Pl011UartDevice::new(MockUartRegisters::default())
    }

    #[test]
    fn tx_ready_tracks_tx_fifo_full_flag() {
        let uart = device();
        uart.backend().set(regs::FR, 0);
        assert!(uart.tx_ready());
        uart.backend().set(regs::FR, fr::TXFF);
        assert!(!uart.tx_ready());
    }

    #[test]
    fn write_byte_writes_dr_only_when_ready() {
        let uart = device();
        uart.backend().set(regs::FR, 0);
        assert_eq!(uart.write_byte(b'A'), Ok(()));
        assert_eq!(uart.backend().write_at(0), Some((regs::DR, b'A' as u32)));

        uart.backend().set(regs::FR, fr::TXFF);
        assert_eq!(uart.write_byte(b'B'), Err(UartError::TxWouldBlock));
        assert_eq!(uart.backend().write_count(), 1);
    }

    #[test]
    fn rx_ready_tracks_rx_fifo_empty_flag() {
        let uart = device();
        uart.backend().set(regs::FR, 0);
        assert!(uart.rx_ready());
        uart.backend().set(regs::FR, fr::RXFE);
        assert!(!uart.rx_ready());
    }

    #[test]
    fn read_byte_nonblocking_tracks_rx_fifo_empty_flag() {
        let uart = device();
        uart.backend().set(regs::FR, 0);
        uart.backend().set(regs::DR, 0x1ab);
        assert_eq!(uart.read_byte_nonblocking(), Ok(0xab));

        uart.backend().set(regs::FR, fr::RXFE);
        assert_eq!(uart.read_byte_nonblocking(), Err(UartError::RxWouldBlock));
    }

    #[test]
    fn configure_writes_expected_data_only_sequence() {
        let uart = device();
        let config = Pl011Config::data_8n1(26, 3);
        assert_eq!(uart.configure(config), Ok(()));
        let expected = [
            (regs::CR, 0),
            (regs::IMSC, 0),
            (regs::ICR, regs::ICR_ALL),
            (regs::IBRD, 26),
            (regs::FBRD, 3),
            (regs::LCRH, lcrh::WLEN_8 | lcrh::FEN),
            (regs::CR, cr::UARTEN | cr::TXE | cr::RXE),
        ];
        assert_eq!(uart.backend().write_count(), expected.len());
        for (index, expected_write) in expected.into_iter().enumerate() {
            assert_eq!(uart.backend().write_at(index), Some(expected_write));
        }
    }

    #[test]
    fn invalid_config_writes_nothing() {
        let uart = device();
        for config in [
            Pl011Config::data_8n1(0, 1),
            Pl011Config::data_8n1(1, 64),
            Pl011Config {
                integer_divisor: 1,
                fractional_divisor: 0,
                fifo_enabled: true,
                tx_enabled: false,
                rx_enabled: false,
            },
        ] {
            assert_eq!(uart.configure(config), Err(UartError::InvalidConfig));
        }
        assert_eq!(uart.backend().write_count(), 0);
    }

    #[test]
    fn clear_interrupts_and_data_path_controls_use_exact_bits() {
        let uart = device();
        uart.clear_interrupts();
        assert_eq!(uart.backend().write_at(0), Some((regs::ICR, regs::ICR_ALL)));
        uart.backend().set(regs::CR, cr::UARTEN | cr::RXE);
        uart.set_tx_enabled(true);
        assert_eq!(uart.backend().get(regs::CR), cr::UARTEN | cr::RXE | cr::TXE);
        uart.set_rx_enabled(false);
        assert_eq!(uart.backend().get(regs::CR), cr::UARTEN | cr::TXE);
    }

    #[test]
    fn write_bytes_stops_without_spinning_when_fifo_becomes_full() {
        let uart = device();
        uart.backend().set(regs::FR, fr::TXFF);
        assert_eq!(uart.write_bytes(b"abc"), Err(UartError::TxWouldBlock));
        assert_eq!(uart.backend().write_count(), 0);
    }

    #[test]
    fn generic_uart_dispatch_operates_through_pl011_trait_impl() {
        use crate::drivers::uart::service::{UartService, dispatch_uart_request};
        use yarm_ipc_abi::uart_abi::{UartConfig8N1, UartReply, UartRequest, UartStatus};

        let uart = device();
        let mut service = UartService::new();
        let config = UartRequest::Configure8N1(UartConfig8N1 {
            integer_divisor: 26,
            fractional_divisor: 3,
            fifo_enabled: true,
        });
        let reply = UartReply::decode(&dispatch_uart_request(
            &mut service,
            &uart,
            &config.encode(),
        ))
        .unwrap();
        assert_eq!(reply.status, UartStatus::Ok);
        assert_eq!(uart.backend().get(regs::IBRD), 26);

        uart.backend().set(regs::FR, 0);
        let reply = UartReply::decode(&dispatch_uart_request(
            &mut service,
            &uart,
            &UartRequest::WriteByte(b'P').encode(),
        ))
        .unwrap();
        assert_eq!(reply.bytes_written, 1);
        assert_eq!(uart.backend().get(regs::DR), b'P' as u32);
    }

    #[test]
    fn hosted_build_excludes_real_mmio_backend() {
        assert!(cfg!(feature = "hosted-dev"));
        let source = include_str!("device.rs");
        assert!(
            source.contains("#[cfg(not(feature = \"hosted-dev\"))]\npub struct VolatileUartMmio")
        );
        let forbidden_qemu_base = ["0x0900", "_0000"].concat();
        assert!(!source.contains(&forbidden_qemu_base));
    }
}
