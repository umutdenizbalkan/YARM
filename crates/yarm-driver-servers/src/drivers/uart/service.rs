// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Internal userspace UART service state and pure, mock-testable ABI dispatch.
//!
//! The kernel early console is a separate architecture facility and is not used
//! by this server. Dispatch performs no syscall, startup-slot access, MMIO
//! construction, or retry loop.

use yarm_ipc_abi::uart_abi::{
    UART_ABI_VERSION, UART_FEATURE_ALL, UART_MAX_INLINE_WRITE, UartCodecError, UartReply,
    UartRequest, UartStatus,
};

const UART_TX_QUEUE_LIMIT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UartDeviceError {
    TxWouldBlock,
    RxWouldBlock,
    InvalidConfig,
    Unsupported,
}

/// Generic nonblocking UART device contract consumed by ABI dispatch.
/// Implementations may be PL011, another UART IP block, or a hosted mock.
pub trait UartDeviceOps {
    fn configure_8n1(
        &self,
        integer_divisor: u16,
        fractional_divisor: u8,
        fifo_enabled: bool,
    ) -> Result<(), UartDeviceError>;
    fn write_byte(&self, byte: u8) -> Result<(), UartDeviceError>;
    fn write_bytes(&self, bytes: &[u8]) -> Result<usize, UartDeviceError>;
    fn read_byte_nonblocking(&self) -> Result<u8, UartDeviceError>;
    fn clear_interrupts(&self);
}

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

    /// Attempt a nonblocking write to a configured UART backend.
    ///
    /// Accepted and unaccepted bytes are reflected in the same counters used
    /// by the existing synthetic queue model. No retry loop is performed.
    pub fn write_device<D: UartDeviceOps>(
        &mut self,
        device: &D,
        bytes: &[u8],
    ) -> Result<usize, UartDeviceError> {
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

    /// Attempt one nonblocking transmit and account for the result.
    pub fn write_byte_device<D: UartDeviceOps>(
        &mut self,
        device: &D,
        byte: u8,
    ) -> Result<(), UartDeviceError> {
        match device.write_byte(byte) {
            Ok(()) => {
                self.stats.tx_bytes = self.stats.tx_bytes.saturating_add(1);
                Ok(())
            }
            Err(error) => {
                self.stats.dropped_tx_bytes = self.stats.dropped_tx_bytes.saturating_add(1);
                Err(error)
            }
        }
    }

    /// Poll one receive byte without waiting and account for successful input.
    pub fn read_device_nonblocking<D: UartDeviceOps>(
        &mut self,
        device: &D,
    ) -> Result<u8, UartDeviceError> {
        let byte = device.read_byte_nonblocking()?;
        self.stats.rx_bytes = self.stats.rx_bytes.saturating_add(1);
        Ok(byte)
    }
}

/// Decode one fixed-size UART ABI v1 request and execute it synchronously over
/// an already-constructed device. Inline writes are nonblocking: a short write
/// returns `OK` with the exact `bytes_written`; zero progress returns
/// `TX_WOULD_BLOCK`.
pub fn dispatch_uart_request<D: UartDeviceOps>(
    service: &mut UartService,
    device: &D,
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
            match device.configure_8n1(
                config.integer_divisor,
                config.fractional_divisor,
                config.fifo_enabled,
            ) {
                Ok(()) => UartReply::status(UartStatus::Ok),
                Err(error) => UartReply::status(map_device_error(error)),
            }
        }
        UartRequest::WriteByte(byte) => match service.write_byte_device(device, byte) {
            Ok(()) => UartReply {
                bytes_written: 1,
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

const fn map_device_error(error: UartDeviceError) -> UartStatus {
    match error {
        UartDeviceError::TxWouldBlock => UartStatus::TxWouldBlock,
        UartDeviceError::RxWouldBlock => UartStatus::RxWouldBlock,
        UartDeviceError::InvalidConfig => UartStatus::InvalidConfig,
        UartDeviceError::Unsupported => UartStatus::Unsupported,
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
    use core::cell::{Cell, RefCell};
    use yarm_ipc_abi::uart_abi::{UartConfig8N1, UartReply, UartRequest, UartWrite};

    struct MockUartDevice {
        config: Cell<Option<(u16, u8, bool)>>,
        tx_blocked: Cell<bool>,
        rx: Cell<Option<u8>>,
        writes: RefCell<[u8; 96]>,
        write_count: Cell<usize>,
        clears: Cell<usize>,
    }

    impl Default for MockUartDevice {
        fn default() -> Self {
            Self {
                config: Cell::new(None),
                tx_blocked: Cell::new(false),
                rx: Cell::new(None),
                writes: RefCell::new([0; 96]),
                write_count: Cell::new(0),
                clears: Cell::new(0),
            }
        }
    }

    impl UartDeviceOps for MockUartDevice {
        fn configure_8n1(
            &self,
            integer_divisor: u16,
            fractional_divisor: u8,
            fifo_enabled: bool,
        ) -> Result<(), UartDeviceError> {
            if integer_divisor == 0 || fractional_divisor > 63 {
                return Err(UartDeviceError::InvalidConfig);
            }
            self.config
                .set(Some((integer_divisor, fractional_divisor, fifo_enabled)));
            Ok(())
        }

        fn write_byte(&self, byte: u8) -> Result<(), UartDeviceError> {
            if self.tx_blocked.get() {
                return Err(UartDeviceError::TxWouldBlock);
            }
            let index = self.write_count.get();
            self.writes.borrow_mut()[index] = byte;
            self.write_count.set(index + 1);
            Ok(())
        }

        fn write_bytes(&self, bytes: &[u8]) -> Result<usize, UartDeviceError> {
            if self.tx_blocked.get() {
                return Err(UartDeviceError::TxWouldBlock);
            }
            for &byte in bytes {
                self.write_byte(byte)?;
            }
            Ok(bytes.len())
        }

        fn read_byte_nonblocking(&self) -> Result<u8, UartDeviceError> {
            self.rx.take().ok_or(UartDeviceError::RxWouldBlock)
        }

        fn clear_interrupts(&self) {
            self.clears.set(self.clears.get() + 1);
        }
    }

    fn dispatch(
        service: &mut UartService,
        device: &MockUartDevice,
        request: UartRequest,
    ) -> UartReply {
        UartReply::decode(&dispatch_uart_request(service, device, &request.encode())).unwrap()
    }

    #[test]
    fn generic_dispatch_covers_info_config_write_read_clear_and_stats() {
        let device = MockUartDevice::default();
        let mut service = UartService::new();
        let info = dispatch(&mut service, &device, UartRequest::GetInfo);
        assert_eq!(info.status, UartStatus::Ok);
        assert_eq!(info.abi_version, UART_ABI_VERSION);

        let config = UartConfig8N1 {
            integer_divisor: 26,
            fractional_divisor: 3,
            fifo_enabled: true,
        };
        assert_eq!(
            dispatch(&mut service, &device, UartRequest::Configure8N1(config)).status,
            UartStatus::Ok
        );
        assert_eq!(device.config.get(), Some((26, 3, true)));

        assert_eq!(
            dispatch(&mut service, &device, UartRequest::WriteByte(b'X')).bytes_written,
            1
        );
        assert_eq!(
            dispatch(
                &mut service,
                &device,
                UartRequest::Write(UartWrite::new(b"OK").unwrap()),
            )
            .bytes_written,
            2
        );
        device.rx.set(Some(b'R'));
        assert_eq!(
            dispatch(&mut service, &device, UartRequest::ReadByte).byte_read,
            Some(b'R')
        );
        assert_eq!(
            dispatch(&mut service, &device, UartRequest::ClearInterrupts).status,
            UartStatus::Ok
        );
        assert_eq!(device.clears.get(), 1);
        let stats = dispatch(&mut service, &device, UartRequest::GetStats);
        assert_eq!((stats.tx_bytes, stats.rx_bytes), (3, 1));
    }

    #[test]
    fn generic_dispatch_maps_device_and_decode_errors() {
        let device = MockUartDevice::default();
        let mut service = UartService::new();
        device.tx_blocked.set(true);
        assert_eq!(
            dispatch(&mut service, &device, UartRequest::WriteByte(b'X')).status,
            UartStatus::TxWouldBlock
        );
        assert_eq!(
            dispatch(&mut service, &device, UartRequest::ReadByte).status,
            UartStatus::RxWouldBlock
        );
        let malformed = dispatch_uart_request(&mut service, &device, &[0; 3]);
        assert_eq!(
            UartReply::decode(&malformed).unwrap().status,
            UartStatus::Malformed
        );
        let mut unknown = UartRequest::GetInfo.encode();
        unknown[..2].copy_from_slice(&0xffffu16.to_le_bytes());
        assert_eq!(
            UartReply::decode(&dispatch_uart_request(&mut service, &device, &unknown))
                .unwrap()
                .status,
            UartStatus::Unsupported
        );
    }

    #[test]
    fn uart_backpressure_is_deterministic() {
        let mut service = UartService::new();
        service.write(80);
        service.ingest(3);
        service.complete_tx(32);
        service.write(16);
        assert_eq!(
            service.stats(),
            UartStats {
                tx_bytes: 80,
                rx_bytes: 3,
                dropped_tx_bytes: 16,
            }
        );
    }

    #[test]
    fn generic_service_has_no_concrete_backend_or_live_ipc_construction() {
        let source = include_str!("service.rs");
        let concrete = ["Pl011", "UartDevice"].concat();
        let startup_receive = ["recv", "_startup"].concat();
        let ipc_receive = ["ipc", "_recv"].concat();
        assert!(!source.contains(&concrete));
        assert!(!source.contains(&startup_receive));
        assert!(!source.contains(&ipc_receive));
        assert!(source.contains("UART_SRV_DEFERRED_NO_MMIO_GRANT"));
    }
}
