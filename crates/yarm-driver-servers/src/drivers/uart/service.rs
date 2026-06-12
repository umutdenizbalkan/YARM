// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Internal userspace UART service state and mock-testable PL011 data helpers.
//!
//! No UART wire ABI exists yet, so this module does not invent request opcodes.
//! The kernel early console is a separate architecture facility and is not used
//! by this server.

use super::device::{Pl011UartDevice, UartError, UartRegisterIo};

const UART_TX_QUEUE_LIMIT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartStats {
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub dropped_tx_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartService {
    stats: UartStats,
    tx_inflight: usize,
}

impl UartService {
    pub const fn new() -> Self {
        Self {
            stats: UartStats {
                tx_bytes: 0,
                rx_bytes: 0,
                dropped_tx_bytes: 0,
            },
            tx_inflight: 0,
        }
    }

    pub fn write(&mut self, bytes: usize) {
        let available = UART_TX_QUEUE_LIMIT.saturating_sub(self.tx_inflight);
        let accepted = available.min(bytes);
        let dropped = bytes.saturating_sub(accepted);

        self.tx_inflight = self.tx_inflight.saturating_add(accepted);
        self.stats.tx_bytes = self.stats.tx_bytes.saturating_add(accepted as u64);
        self.stats.dropped_tx_bytes = self.stats.dropped_tx_bytes.saturating_add(dropped as u64);
    }

    pub fn complete_tx(&mut self, bytes: usize) {
        self.tx_inflight = self.tx_inflight.saturating_sub(bytes);
    }

    pub fn ingest(&mut self, bytes: usize) {
        self.stats.rx_bytes = self.stats.rx_bytes.saturating_add(bytes as u64);
    }

    pub const fn stats(&self) -> UartStats {
        self.stats
    }

    /// Attempt a nonblocking write to a configured PL011 backend.
    ///
    /// Accepted and unaccepted bytes are reflected in the same counters used
    /// by the existing synthetic queue model. No retry loop is performed.
    pub fn write_device<B: UartRegisterIo>(
        &mut self,
        device: &Pl011UartDevice<B>,
        bytes: &[u8],
    ) -> Result<usize, UartError> {
        match device.write_bytes(bytes) {
            Ok(written) => {
                self.stats.tx_bytes = self.stats.tx_bytes.saturating_add(written as u64);
                self.stats.dropped_tx_bytes = self
                    .stats
                    .dropped_tx_bytes
                    .saturating_add(bytes.len().saturating_sub(written) as u64);
                Ok(written)
            }
            Err(error) => {
                self.stats.dropped_tx_bytes = self
                    .stats
                    .dropped_tx_bytes
                    .saturating_add(bytes.len() as u64);
                Err(error)
            }
        }
    }

    /// Poll one receive byte without waiting and account for successful input.
    pub fn read_device_nonblocking<B: UartRegisterIo>(
        &mut self,
        device: &Pl011UartDevice<B>,
    ) -> Result<u8, UartError> {
        let byte = device.read_byte_nonblocking()?;
        self.stats.rx_bytes = self.stats.rx_bytes.saturating_add(1);
        Ok(byte)
    }
}

pub fn run() {
    // The binary exists for build parity but is not live-spawned. A future
    // platform service must discover the UART from DTB/platform data and pass
    // a validated capability-granted MMIO mapping before a device is created.
    yarm_user_rt::user_log!("UART_SRV_DEFERRED_NO_MMIO_GRANT");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uart_backpressure_is_deterministic() {
        let mut s = UartService::new();
        s.write(80);
        s.ingest(3);
        s.complete_tx(32);
        s.write(16);
        assert_eq!(
            s.stats(),
            UartStats {
                tx_bytes: 80,
                rx_bytes: 3,
                dropped_tx_bytes: 16,
            }
        );
    }

    #[test]
    fn internal_data_helpers_translate_nonblocking_device_results() {
        use crate::drivers::uart::{MockUartRegisters, Pl011UartDevice, regs};

        let device = Pl011UartDevice::new(MockUartRegisters::default());
        let mut service = UartService::new();

        device.backend().set(regs::FR, 0);
        assert_eq!(service.write_device(&device, b"OK"), Ok(2));
        device.backend().set(regs::DR, b'R' as u32);
        assert_eq!(service.read_device_nonblocking(&device), Ok(b'R'));
        assert_eq!(
            service.stats(),
            UartStats {
                tx_bytes: 2,
                rx_bytes: 1,
                dropped_tx_bytes: 0,
            }
        );

        device
            .backend()
            .set(regs::FR, regs::fr::TXFF | regs::fr::RXFE);
        assert_eq!(
            service.write_device(&device, b"NO"),
            Err(UartError::TxWouldBlock)
        );
        assert_eq!(
            service.read_device_nonblocking(&device),
            Err(UartError::RxWouldBlock)
        );
        assert_eq!(service.stats().dropped_tx_bytes, 2);
    }
}
