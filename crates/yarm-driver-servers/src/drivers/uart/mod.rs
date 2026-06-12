// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Generic UART service plus explicitly namespaced hardware backends.

pub mod audit;
pub mod backend;
pub mod service;

pub use backend::pl011;

/// Compatibility module for the former `uart::device` path.
pub mod device {
    pub use super::backend::pl011::device::*;
}

/// Compatibility module for the former `uart::regs` path.
pub mod regs {
    pub use super::backend::pl011::regs::*;
}

#[cfg(any(test, feature = "hosted-dev"))]
pub use backend::pl011::MockUartRegisters;
// Compatibility re-exports for existing users; new code should use `uart::pl011`.
pub use backend::pl011::device::UartError;
pub use backend::pl011::{Pl011Config, Pl011UartDevice, UartRegisterIo};
pub use service::{
    UartDeviceError, UartDeviceOps, UartService, UartStats, dispatch_uart_request, run,
};
