// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Deprecated legacy namespace.
//! Workspace crates under `crates/` are the runtime dispatch entrypoints.

pub mod blkcache;
pub mod devfs;
pub mod ext4;
pub mod fat;
pub mod initramfs;
pub mod ramfs;

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_scoped_fs_modules_are_include_only_shims() {
        let devfs_nodes = include_str!("devfs/nodes.rs");
        let devfs_service = include_str!("devfs/service.rs");
        let initramfs_archive = include_str!("initramfs/archive.rs");
        let initramfs_manifest = include_str!("initramfs/manifest.rs");
        let initramfs_service = include_str!("initramfs/service.rs");
        let ramfs_tree = include_str!("ramfs/tree.rs");
        let ramfs_service = include_str!("ramfs/service.rs");

        assert!(devfs_nodes.contains("/crates/yarm-fs-servers/src/fs/devfs/nodes.rs"));
        assert!(devfs_service.contains("/crates/yarm-fs-servers/src/fs/devfs/service.rs"));
        assert!(initramfs_archive.contains("/crates/yarm-fs-servers/src/fs/initramfs/archive.rs"));
        assert!(initramfs_manifest.contains("/crates/yarm-fs-servers/src/fs/initramfs/manifest.rs"));
        assert!(initramfs_service.contains("/crates/yarm-fs-servers/src/fs/initramfs/service.rs"));
        assert!(ramfs_tree.contains("/crates/yarm-fs-servers/src/fs/ramfs/tree.rs"));
        assert!(ramfs_service.contains("/crates/yarm-fs-servers/src/fs/ramfs/service.rs"));
    }
}
