// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod device;
pub mod service;

pub use device::VirtioBlkDevice;
pub use service::{run, VirtioBlkService};
