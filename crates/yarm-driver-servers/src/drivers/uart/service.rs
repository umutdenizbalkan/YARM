// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Internal userspace UART service state and pure, mock-testable ABI dispatch.
//!
//! The kernel early console is a separate architecture facility and is not used
//! by this server. Dispatch performs no syscall, startup-slot access, MMIO
//! construction, or retry loop.

use super::device::{Pl011Config, Pl011UartDevice, UartError, UartRegisterIo};
use yarm_ipc_abi::uart_abi::{
    UART_ABI_VERSION, UART_FEATURE_ALL, UART_MAX_INLINE_WRITE, UartCodecError, UartReply,
    UartRequest, UartStatus,
};

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

/// Decode one fixed-size UART ABI v1 request and execute it synchronously over
/// an already-constructed device. Inline writes are nonblocking: a short write
/// returns `OK` with the exact `bytes_written`; zero progress returns
/// `TX_WOULD_BLOCK`.
pub fn dispatch_uart_request<B: UartRegisterIo>(
    service: &mut UartService,
    device: &Pl011UartDevice<B>,
    request_bytes: &[u8],
) -> [u8; UartReply::ENCODED_LEN] {
    let request = match UartRequest::decode(request_bytes) {
        Ok(request) => request,
        Err(UartCodecError::UnsupportedOpcode) => {
            return UartReply::status(UartStatus::Unsupported).encode();
        }
        Err(UartCodecError::InvalidArg | UartCodecError::MessageTooLarge) => {
            return UartReply::status(UartStatus::InvalidArg).encode();
        }
        Err(UartCodecError::Malformed) => {
            return UartReply::status(UartStatus::Malformed).encode();
        }
    };

    let reply = match request {
        UartRequest::GetInfo => UartReply {
            status: UartStatus::Ok,
            abi_version: UART_ABI_VERSION,
            max_inline_write: UART_MAX_INLINE_WRITE as u16,
            features: UART_FEATURE_ALL,
            ..UartReply::status(UartStatus::Ok)
        },
        UartRequest::Configure8N1(config) => {
            let config = Pl011Config {
                integer_divisor: config.integer_divisor,
                fractional_divisor: config.fractional_divisor,
                fifo_enabled: config.fifo_enabled,
                tx_enabled: true,
                rx_enabled: true,
            };
            match device.configure(config) {
                Ok(()) => UartReply::status(UartStatus::Ok),
                Err(error) => UartReply::status(map_device_error(error)),
            }
        }
        UartRequest::WriteByte(byte) => match service.write_device(device, &[byte]) {
            Ok(written) => UartReply {
                bytes_written: written as u16,
                ..UartReply::status(UartStatus::Ok)
            },
            Err(error) => UartReply::status(map_device_error(error)),
        },
        UartRequest::Write(write) => match service.write_device(device, write.bytes()) {
            Ok(written) => UartReply {
                bytes_written: written as u16,
                ..UartReply::status(UartStatus::Ok)
            },
            Err(error) => UartReply::status(map_device_error(error)),
        },
        UartRequest::ReadByte => match service.read_device_nonblocking(device) {
            Ok(byte) => UartReply {
                byte_read: Some(byte),
                ..UartReply::status(UartStatus::Ok)
            },
            Err(error) => UartReply::status(map_device_error(error)),
        },
        UartRequest::GetStats => {
            let stats = service.stats();
            UartReply {
                status: UartStatus::Ok,
                tx_bytes: stats.tx_bytes,
                rx_bytes: stats.rx_bytes,
                dropped_tx_bytes: stats.dropped_tx_bytes,
                ..UartReply::status(UartStatus::Ok)
            }
        }
        UartRequest::ClearInterrupts => {
            device.clear_interrupts();
            UartReply::status(UartStatus::Ok)
        }
    };
    reply.encode()
}

const fn map_device_error(error: UartError) -> UartStatus {
    match error {
        UartError::TxWouldBlock => UartStatus::TxWouldBlock,
        UartError::RxWouldBlock => UartStatus::RxWouldBlock,
        UartError::InvalidConfig => UartStatus::InvalidConfig,
        UartError::Unsupported => UartStatus::Unsupported,
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
    use crate::drivers::uart::{MockUartRegisters, regs};
    use yarm_ipc_abi::uart_abi::{UART_OP_GET_INFO, UartConfig8N1, UartRequest, UartWrite};

    fn device() -> Pl011UartDevice<MockUartRegisters> {
        Pl011UartDevice::new(MockUartRegisters::default())
    }

    fn dispatch(
        service: &mut UartService,
        device: &Pl011UartDevice<MockUartRegisters>,
        request: UartRequest,
    ) -> UartReply {
        UartReply::decode(&dispatch_uart_request(service, device, &request.encode())).unwrap()
    }

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
    fn get_info_returns_v1_capabilities() {
        let device = device();
        let mut service = UartService::new();
        let reply = dispatch(&mut service, &device, UartRequest::GetInfo);
        assert_eq!(reply.status, UartStatus::Ok);
        assert_eq!(reply.abi_version, UART_ABI_VERSION);
        assert_eq!(reply.max_inline_write, UART_MAX_INLINE_WRITE as u16);
        assert_eq!(reply.features, UART_FEATURE_ALL);
    }

    #[test]
    fn configure_8n1_programs_existing_device_path_and_rejects_bad_config() {
        let device = device();
        let mut service = UartService::new();
        let reply = dispatch(
            &mut service,
            &device,
            UartRequest::Configure8N1(UartConfig8N1 {
                integer_divisor: 26,
                fractional_divisor: 3,
                fifo_enabled: true,
            }),
        );
        assert_eq!(reply.status, UartStatus::Ok);
        assert_eq!(device.backend().get(regs::IBRD), 26);
        assert_eq!(device.backend().get(regs::FBRD), 3);
        assert_eq!(
            device.backend().get(regs::LCRH),
            regs::lcrh::WLEN_8 | regs::lcrh::FEN
        );

        let bad = dispatch(
            &mut service,
            &device,
            UartRequest::Configure8N1(UartConfig8N1 {
                integer_divisor: 0,
                fractional_divisor: 0,
                fifo_enabled: true,
            }),
        );
        assert_eq!(bad.status, UartStatus::InvalidConfig);
    }

    #[test]
    fn write_byte_and_inline_write_report_exact_progress() {
        let device = device();
        let mut service = UartService::new();
        device.backend().set(regs::FR, 0);
        let one = dispatch(&mut service, &device, UartRequest::WriteByte(b'X'));
        assert_eq!(one.status, UartStatus::Ok);
        assert_eq!(one.bytes_written, 1);
        assert_eq!(device.backend().get(regs::DR), b'X' as u32);

        let write = UartWrite::new(b"OK").unwrap();
        let multiple = dispatch(&mut service, &device, UartRequest::Write(write));
        assert_eq!(multiple.status, UartStatus::Ok);
        assert_eq!(multiple.bytes_written, 2);
        assert_eq!(device.backend().get(regs::DR), b'K' as u32);
    }

    #[test]
    fn tx_full_and_rx_empty_map_to_would_block() {
        let device = device();
        let mut service = UartService::new();
        device
            .backend()
            .set(regs::FR, regs::fr::TXFF | regs::fr::RXFE);
        assert_eq!(
            dispatch(&mut service, &device, UartRequest::WriteByte(b'X')).status,
            UartStatus::TxWouldBlock
        );
        assert_eq!(
            dispatch(&mut service, &device, UartRequest::ReadByte).status,
            UartStatus::RxWouldBlock
        );
    }

    #[test]
    fn read_clear_interrupts_and_stats_use_existing_device_helpers() {
        let device = device();
        let mut service = UartService::new();
        device.backend().set(regs::FR, 0);
        device.backend().set(regs::DR, b'R' as u32);
        let read = dispatch(&mut service, &device, UartRequest::ReadByte);
        assert_eq!(read.status, UartStatus::Ok);
        assert_eq!(read.byte_read, Some(b'R'));

        let clear = dispatch(&mut service, &device, UartRequest::ClearInterrupts);
        assert_eq!(clear.status, UartStatus::Ok);
        assert_eq!(device.backend().get(regs::ICR), regs::ICR_ALL);

        device.backend().set(regs::FR, 0);
        let _ = dispatch(&mut service, &device, UartRequest::WriteByte(b'T'));
        let stats = dispatch(&mut service, &device, UartRequest::GetStats);
        assert_eq!(stats.status, UartStatus::Ok);
        assert_eq!(stats.tx_bytes, 1);
        assert_eq!(stats.rx_bytes, 1);
        assert_eq!(stats.dropped_tx_bytes, 0);
    }

    #[test]
    fn malformed_and_unknown_requests_map_to_wire_statuses() {
        let device = device();
        let mut service = UartService::new();
        let malformed = dispatch_uart_request(&mut service, &device, &[0; 3]);
        assert_eq!(
            UartReply::decode(&malformed).unwrap().status,
            UartStatus::Malformed
        );

        let mut unknown = UartRequest::GetInfo.encode();
        unknown[..2].copy_from_slice(&0xffffu16.to_le_bytes());
        let reply = dispatch_uart_request(&mut service, &device, &unknown);
        assert_eq!(
            UartReply::decode(&reply).unwrap().status,
            UartStatus::Unsupported
        );
        assert_ne!(UART_OP_GET_INFO, 0xffff);
    }

    #[test]
    fn run_remains_deferred_and_dispatch_has_no_live_ipc_or_mmio_construction() {
        let source = include_str!("service.rs");
        let startup_receive = ["recv", "_startup"].concat();
        let ipc_receive = ["ipc", "_recv"].concat();
        let volatile_constructor = ["Volatile", "UartMmio::from"].concat();
        assert!(!source.contains(&startup_receive));
        assert!(!source.contains(&ipc_receive));
        assert!(!source.contains(&volatile_constructor));
        assert!(source.contains("UART_SRV_DEFERRED_NO_MMIO_GRANT"));
    }
}
