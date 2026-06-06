// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod archive;
pub mod manifest;
pub mod service;
pub mod service_manifest;
use core::sync::atomic::{AtomicUsize, Ordering};

pub use archive::{
    INITRAMFS_BOOT_MARKER_PATH_PTR, INITRAMFS_ETC_HOSTS_PATH_PTR, INITRAMFS_INIT_PATH_PTR,
    INITRAMFS_POSIX_COMPAT_PATH_PTR, INITRAMFS_PROC_MGR_PATH_PTR, INITRAMFS_SRV_PATH,
    INITRAMFS_SRV_PATH_PTR, INITRAMFS_SUPERVISOR_PATH_PTR, INITRAMFS_VFS_PATH_PTR,
    InitramfsBackend, InitramfsMetrics,
};
pub use manifest::{
    CoreServiceElfLaunchPlan, CoreServiceImageManifest, ElfLoadSegmentPlan, InitramfsManifestError,
    ManifestEntryWire, ServiceElfLaunchPlan, build_core_service_elf_launch_plan,
    parse_core_service_manifest,
};
pub use service::{InitramfsService, run};
pub use service_manifest::{
    ELF_IDENT_BYTES, SERVICE_MANIFEST_MAX_BYTES, SERVICE_MANIFEST_MAX_ENTRIES,
    SERVICE_MANIFEST_MAX_LINE_BYTES, SERVICE_MANIFEST_MAX_PATH_BYTES, ServiceManifest,
    ServiceManifestArchiveError, ServiceManifestEntry, ServiceManifestError,
    parse_service_manifest, validate_service_manifest_archive,
};

static BOOT_INITRD_PTR: AtomicUsize = AtomicUsize::new(0);
static BOOT_INITRD_LEN: AtomicUsize = AtomicUsize::new(0);

pub fn install_boot_initrd_bytes(bytes: &'static [u8]) {
    BOOT_INITRD_LEN.store(bytes.len(), Ordering::Release);
    BOOT_INITRD_PTR.store(bytes.as_ptr() as usize, Ordering::Release);
}

pub fn boot_initrd_bytes() -> Option<&'static [u8]> {
    let ptr = BOOT_INITRD_PTR.load(Ordering::Acquire);
    let len = BOOT_INITRD_LEN.load(Ordering::Acquire);
    if ptr == 0 || len == 0 {
        return None;
    }
    // SAFETY: pointer/len pair is installed from an immutable boot memory window.
    Some(unsafe { core::slice::from_raw_parts(ptr as *const u8, len) })
}
