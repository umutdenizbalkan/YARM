// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(any(test, feature = "hosted-dev"))]
pub mod fake_fdt;
pub mod service;

pub use service::{
    DriverClass, DriverLiveness, DriverRecord, DriverRegistry, DriverService, handle_request, run,
};
