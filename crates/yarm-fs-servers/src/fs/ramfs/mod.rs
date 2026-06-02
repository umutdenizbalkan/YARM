// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod service;
pub mod tree;

pub use service::{
    RAMFS_DEFAULT_MOUNT_PREFIX, RAMFS_MOUNT_CONFIG_FLAG_READONLY, RamFsMountConfig, RamFsService,
    RamFsServiceStartup, RamFsStartupConfig, run, run_with_config,
};
pub use tree::{
    RAMFS_BOOT_PATH, RAMFS_BOOT_PATH_PTR, RAMFS_DEFAULT_MAX_BYTES, RAMFS_DEFAULT_MAX_NODES,
    RamFsBackend, RamFsError, RamFsLimits, RamFsMetrics, RamFsNodeKind,
};
