// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! RP1 GPIO/Pinctrl driver — module root.
//!
//! Sub-modules:
//!   `regs`   — structural register-map definitions (volatile `Reg` type,
//!              `repr(C)` block structs, bit-field constants, BAR offsets).
//!   `device` — `Rp1GpioDriver` struct and `GpioDriver` trait implementation.
//!   `service`— YARM microkernel IPC service loop.

pub mod device;
pub mod regs;
pub mod service;

pub use device::{GpioDriver, PinMode, Rp1GpioDriver, TOTAL_GPIOS};
pub use service::run;
