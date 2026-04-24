// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod archive;
pub mod manifest;
pub mod service;

pub use archive::{
    INITRAMFS_BOOT_MARKER_PATH_PTR, INITRAMFS_ETC_HOSTS_PATH_PTR, INITRAMFS_INIT_PATH_PTR,
    INITRAMFS_POSIX_COMPAT_PATH_PTR, INITRAMFS_PROC_MGR_PATH_PTR, INITRAMFS_SUPERVISOR_PATH_PTR,
    INITRAMFS_VFS_PATH_PTR,
    InitramfsBackend, InitramfsMetrics,
};
pub use manifest::{
    CoreServiceElfLaunchPlan, CoreServiceImageManifest, ElfLoadSegmentPlan, InitramfsManifestError,
    ManifestEntryWire, ServiceElfLaunchPlan, build_core_service_elf_launch_plan,
    parse_core_service_manifest,
};
pub use service::{InitramfsService, run};
