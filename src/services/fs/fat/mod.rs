// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod fs;
pub mod service;

pub use fs::FatBackend;
pub use service::{FatService, run};
