// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

extern crate alloc;

pub mod fs;
pub use fs::{common, devfs, ext4, fat, initramfs, ramfs};

pub fn run_devfs() {
    fs::devfs::run();
}

pub fn run_initramfs() {
    fs::initramfs::run();
}

pub fn run_ramfs() {
    fs::ramfs::run();
}

pub fn run_ext4() {
    fs::ext4::run();
}

pub fn run_fat() {
    fs::fat::run();
}

#[cfg(test)]
mod tests {
    #[test]
    fn scoped_fs_impl_does_not_delegate_back_to_legacy_fs_namespace() {
        let devfs_src = include_str!("fs/devfs/service.rs");
        let initramfs_src = include_str!("fs/initramfs/service.rs");
        let ramfs_src = include_str!("fs/ramfs/service.rs");
        let ext4_src = include_str!("fs/ext4/service.rs");
        let fat_src = include_str!("fs/fat/service.rs");
        let legacy_fs = ["yarm", "::services::", "fs::"].concat();
        for src in [devfs_src, initramfs_src, ramfs_src, ext4_src, fat_src] {
            assert!(
                !src.contains(legacy_fs.as_str()),
                "workspace scoped fs impl must not delegate to legacy fs namespace"
            );
        }
    }

    #[test]
    fn fs_server_bin_parity_guard_covers_expected_entrypoints() {
        let cargo_toml = include_str!("../Cargo.toml");
        let expected_bins = [
            (
                "devfs_srv",
                "name = \"devfs_srv\"",
                "path = \"src/bin/devfs_srv.rs\"",
                "bin/devfs_srv.rs",
                "run_devfs",
            ),
            (
                "ramfs_srv",
                "name = \"ramfs_srv\"",
                "path = \"src/bin/ramfs_srv.rs\"",
                "bin/ramfs_srv.rs",
                "run_ramfs",
            ),
            (
                "initramfs_srv",
                "name = \"initramfs_srv\"",
                "path = \"src/bin/initramfs_srv.rs\"",
                "bin/initramfs_srv.rs",
                "run_initramfs",
            ),
            (
                "ext4_srv",
                "name = \"ext4_srv\"",
                "path = \"src/bin/ext4_srv.rs\"",
                "bin/ext4_srv.rs",
                "run_ext4",
            ),
            (
                "fat_srv",
                "name = \"fat_srv\"",
                "path = \"src/bin/fat_srv.rs\"",
                "bin/fat_srv.rs",
                "run_fat",
            ),
        ];

        for (bin_name, name_entry, path_entry, bin_path, run_fn) in expected_bins {
            assert!(
                cargo_toml.contains(name_entry),
                "Cargo.toml missing expected bin entry: {bin_name}"
            );
            assert!(
                cargo_toml.contains(path_entry),
                "Cargo.toml missing expected bin path for: {bin_name}"
            );

            let src = match bin_path {
                "bin/devfs_srv.rs" => include_str!("bin/devfs_srv.rs"),
                "bin/ramfs_srv.rs" => include_str!("bin/ramfs_srv.rs"),
                "bin/initramfs_srv.rs" => include_str!("bin/initramfs_srv.rs"),
                "bin/ext4_srv.rs" => include_str!("bin/ext4_srv.rs"),
                "bin/fat_srv.rs" => include_str!("bin/fat_srv.rs"),
                _ => panic!("unexpected bin path in parity table: {bin_path}"),
            };

            assert!(
                src.contains("yarm_fs_servers::"),
                "{bin_name} should dispatch via yarm_fs_servers crate entrypoint"
            );
            assert!(
                src.contains(run_fn),
                "{bin_name} should call {run_fn} for parity with FS service mapping"
            );
        }
    }
}

#[cfg(test)]
mod stage80_tests {
    use crate::fs::common::shared_io_adapter::{
        VFS_READ_SHARED_REPLY_ENABLED, VFS_SHARED_IO_ENABLED, VFS_WRITE_SHARED_REQUEST_ENABLED,
    };
    use crate::fs::common::vfs_ipc::VfsError;
    use crate::fs::ext4::{EXT4_SERVICE_PATH, Ext4Backend};
    use crate::fs::fat::service::{FatServiceStartup, FatStartupConfig, service_from_startup_config};
    use crate::fs::ramfs::service::{RamFsServiceStartup, RamFsStartupConfig, run_with_config};
    use yarm_srv_common::vfs_core::VfsBackend;

    #[test]
    fn stage80_ext4_write_path_remains_unsupported() {
        let mut backend = Ext4Backend::new();
        let fd = backend.openat_path(EXT4_SERVICE_PATH).expect("open");
        assert_eq!(backend.write(fd, 512), Err(VfsError::Unsupported));
        assert_eq!(backend.write(fd, 4096), Err(VfsError::Unsupported));
    }

    #[test]
    fn stage80_ext4_backend_rejects_writes_of_all_sizes() {
        let mut backend = Ext4Backend::new();
        let fd = backend.openat_path(EXT4_SERVICE_PATH).expect("open");
        for &len in &[1u64, 512, 4096, 65536, 16 * 1024 * 1024 + 1] {
            assert_eq!(
                backend.write(fd, len),
                Err(VfsError::Unsupported),
                "expected Unsupported for write len={len}"
            );
        }
    }

    #[test]
    fn stage80_fat_write_mode_guard_requires_block_backend() {
        let result = service_from_startup_config(FatStartupConfig::production(None, Some(1), 1));
        assert!(
            matches!(result, Err(FatServiceStartup::NoBlockBackend)),
            "fat production config with no block backend must return NoBlockBackend"
        );
    }

    #[test]
    fn stage80_vfs_shared_io_enabled_consistent_with_stage78() {
        assert!(VFS_WRITE_SHARED_REQUEST_ENABLED);
        assert!(VFS_READ_SHARED_REPLY_ENABLED);
        assert!(VFS_SHARED_IO_ENABLED);
    }

    #[test]
    fn stage80_ramfs_run_with_config_smoke_unchanged() {
        let result = run_with_config(RamFsStartupConfig::default_compat());
        assert!(
            matches!(result, RamFsServiceStartup::Mounted { .. }),
            "ramfs run_with_config must mount successfully with default_compat config"
        );
    }

    #[test]
    fn stage80_ext4_srv_bin_has_entry_and_ready_markers() {
        let ext4_bin_src = include_str!("bin/ext4_srv.rs");
        assert!(ext4_bin_src.contains("EXT4_SRV_ENTRY"));
        assert!(ext4_bin_src.contains("EXT4_MOUNT_READY"));
    }

    #[test]
    fn stage80_all_three_fs_server_bins_have_entry_markers() {
        let ramfs_src = include_str!("bin/ramfs_srv.rs");
        let fat_src = include_str!("bin/fat_srv.rs");
        let ext4_src = include_str!("bin/ext4_srv.rs");
        assert!(ramfs_src.contains("RAMFS_BIN_ENTRY_START"));
        assert!(fat_src.contains("FAT_BIN_ENTRY_START"));
        assert!(ext4_src.contains("EXT4_BIN_ENTRY_START"));
    }

    #[test]
    fn stage80_ext4_vfs_registration_deferred_blocker_no_ipc_loop() {
        // ext4/service.rs::run() performs a smoke and returns — it does not
        // enter a kernel ipc_recv loop. VFS mount registration is deferred.
        // Blocker: add ipc_recv loop to ext4/service.rs before enabling.
        let ext4_service_src = include_str!("fs/ext4/service.rs");
        assert!(
            !ext4_service_src.contains("ipc_recv("),
            "ext4 service must NOT have ipc_recv yet — registration blocker still active"
        );
    }
}
