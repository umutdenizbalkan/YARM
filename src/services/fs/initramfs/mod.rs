// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod archive;
pub mod service;

pub use archive::{
    INITRAMFS_BOOT_MARKER_PATH_PTR, INITRAMFS_ETC_HOSTS_PATH_PTR, INITRAMFS_INIT_PATH_PTR,
    InitramfsBackend, InitramfsMetrics,
};
pub use service::{InitramfsService, run};
