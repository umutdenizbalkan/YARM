// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod service;
pub mod tree;

pub use service::{RamFsService, run};
pub use tree::{RAMFS_BOOT_PATH, RAMFS_BOOT_PATH_PTR, RamFsBackend, RamFsMetrics};
