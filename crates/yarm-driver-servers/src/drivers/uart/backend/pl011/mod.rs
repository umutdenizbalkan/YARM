// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! ARM PL011 backend for the generic UART service contract.

pub mod device;
pub mod regs;

#[cfg(any(test, feature = "hosted-dev"))]
pub use device::MockUartRegisters;
pub use device::{Pl011Config, Pl011UartDevice, UartRegisterIo};
