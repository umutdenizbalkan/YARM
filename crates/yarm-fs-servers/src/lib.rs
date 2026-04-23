// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

extern crate alloc;

pub mod fs;
pub use fs::{blkcache, common, devfs, ext4, fat, initramfs, ramfs};

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

pub fn run_blkcache() {
    fs::blkcache::run();
}

#[cfg(test)]
mod tests {
    #[test]
    fn scoped_fs_impl_does_not_delegate_back_to_legacy_fs_namespace() {
        let blkcache_src = include_str!("fs/blkcache/service.rs");
        let devfs_src = include_str!("fs/devfs/service.rs");
        let initramfs_src = include_str!("fs/initramfs/service.rs");
        let ramfs_src = include_str!("fs/ramfs/service.rs");
        let ext4_src = include_str!("fs/ext4/service.rs");
        let fat_src = include_str!("fs/fat/service.rs");
        let legacy_fs = ["yarm", "::services::", "fs::"].concat();
        for src in [
            blkcache_src,
            devfs_src,
            initramfs_src,
            ramfs_src,
            ext4_src,
            fat_src,
        ] {
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
            (
                "blkcache_srv",
                "name = \"blkcache_srv\"",
                "path = \"src/bin/blkcache_srv.rs\"",
                "bin/blkcache_srv.rs",
                "run_blkcache",
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
                "bin/blkcache_srv.rs" => include_str!("bin/blkcache_srv.rs"),
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
