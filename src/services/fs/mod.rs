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
        let blkcache_service = include_str!("blkcache/service.rs");
        let devfs_nodes = include_str!("devfs/nodes.rs");
        let devfs_service = include_str!("devfs/service.rs");
        let initramfs_archive = include_str!("initramfs/archive.rs");
        let initramfs_manifest = include_str!("initramfs/manifest.rs");
        let initramfs_service = include_str!("initramfs/service.rs");
        let ramfs_tree = include_str!("ramfs/tree.rs");
        let ramfs_service = include_str!("ramfs/service.rs");
        let ext4_dir = include_str!("ext4/dir.rs");
        let ext4_file = include_str!("ext4/file.rs");
        let ext4_fs = include_str!("ext4/fs.rs");
        let ext4_inode = include_str!("ext4/inode.rs");
        let ext4_service = include_str!("ext4/service.rs");
        let fat_fs = include_str!("fat/fs.rs");
        let fat_service = include_str!("fat/service.rs");

        assert!(blkcache_service.contains("/crates/yarm-fs-servers/src/fs/blkcache/service.rs"));
        assert!(devfs_nodes.contains("/crates/yarm-fs-servers/src/fs/devfs/nodes.rs"));
        assert!(devfs_service.contains("/crates/yarm-fs-servers/src/fs/devfs/service.rs"));
        assert!(initramfs_archive.contains("/crates/yarm-fs-servers/src/fs/initramfs/archive.rs"));
        assert!(
            initramfs_manifest.contains("/crates/yarm-fs-servers/src/fs/initramfs/manifest.rs")
        );
        assert!(initramfs_service.contains("/crates/yarm-fs-servers/src/fs/initramfs/service.rs"));
        assert!(ramfs_tree.contains("/crates/yarm-fs-servers/src/fs/ramfs/tree.rs"));
        assert!(ramfs_service.contains("/crates/yarm-fs-servers/src/fs/ramfs/service.rs"));
        assert!(ext4_dir.contains("/crates/yarm-fs-servers/src/fs/ext4/dir.rs"));
        assert!(ext4_file.contains("/crates/yarm-fs-servers/src/fs/ext4/file.rs"));
        assert!(ext4_fs.contains("/crates/yarm-fs-servers/src/fs/ext4/fs.rs"));
        assert!(ext4_inode.contains("/crates/yarm-fs-servers/src/fs/ext4/inode.rs"));
        assert!(ext4_service.contains("/crates/yarm-fs-servers/src/fs/ext4/service.rs"));
        assert!(fat_fs.contains("/crates/yarm-fs-servers/src/fs/fat/fs.rs"));
        assert!(fat_service.contains("/crates/yarm-fs-servers/src/fs/fat/service.rs"));
    }
}
