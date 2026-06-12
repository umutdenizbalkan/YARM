// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Mock-safe userspace PL011 UART scaffold.

pub mod audit;
pub mod device;
pub mod regs;
pub mod service;

#[cfg(any(test, feature = "hosted-dev"))]
pub use device::MockUartRegisters;
pub use device::{Pl011Config, Pl011UartDevice, UartError, UartRegisterIo};
pub use service::run;
