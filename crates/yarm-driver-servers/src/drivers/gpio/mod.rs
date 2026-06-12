// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Generic GPIO device contract used by ABI dispatch.
//!
//! This module contains no register layout, physical address, platform
//! discovery, MMIO construction, or live service-spawn policy.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioDeviceError {
    InvalidPin,
    InvalidFunction,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioDirection {
    Input,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioPull {
    Off,
    Down,
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioPinMode {
    Input,
    Output { initial_level: bool },
    AltFunction(u32),
}

/// Backend-neutral operations already supported by the existing GPIO device
/// and service layers. GPIO ABI v1 exposes only its existing subset.
pub trait GpioDeviceOps {
    fn set_function(&self, pin: usize, function: u32) -> Result<(), GpioDeviceError>;
    fn set_pin_mode(&self, pin: usize, mode: GpioPinMode) -> Result<(), GpioDeviceError>;
    fn direction(&self, pin: usize) -> Result<GpioDirection, GpioDeviceError>;
    fn write_pin(&self, pin: usize, level: bool) -> Result<(), GpioDeviceError>;
    fn read_pin(&self, pin: usize) -> Result<bool, GpioDeviceError>;
    fn set_pull(&self, pin: usize, pull: GpioPull) -> Result<(), GpioDeviceError>;
    fn pull(&self, pin: usize) -> Result<GpioPull, GpioDeviceError>;
}
