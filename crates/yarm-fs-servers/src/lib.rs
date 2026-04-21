// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

pub mod fs;
pub use fs::{blkcache, devfs, ext4, fat, initramfs, ramfs};

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
}
