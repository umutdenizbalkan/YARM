// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Userspace RP1 GPIO/Pinctrl protocol and device scaffold.
//!
//! See [`audit`] for implemented, deferred, and hardware-blocked work.

pub mod audit;
pub mod device;
pub mod regs;
pub mod service;

pub use device::{
    Direction, GpioDriver, GpioError, PinMode, Pull, RegisterIo, Rp1GpioDevice, TOTAL_GPIOS,
};
pub use service::{dispatch, run};
