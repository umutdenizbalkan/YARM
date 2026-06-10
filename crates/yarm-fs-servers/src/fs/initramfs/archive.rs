// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::super::common::vfs_ipc::{VfsBackend, VfsError};
use yarm_srv_common::cpio::CpioArchive;

/// Compatibility-only legacy path identifier; prefer `INITRAMFS_BOOT_MARKER_PATH`.
pub const INITRAMFS_BOOT_MARKER_PATH_PTR: u64 = 0x494E_4954_424F_4F54;
pub const INITRAMFS_BOOT_MARKER_PATH: &[u8] = b"/initramfs/boot-marker";
/// Compatibility-only legacy path identifier; prefer `INITRAMFS_INIT_PATH`.
pub const INITRAMFS_INIT_PATH_PTR: u64 = 0x494E_4954_524F_4F54;
pub const INITRAMFS_INIT_PATH: &[u8] = b"/initramfs/init";
/// Compatibility-only legacy path identifier; prefer `INITRAMFS_ETC_HOSTS_PATH`.
pub const INITRAMFS_ETC_HOSTS_PATH_PTR: u64 = 0x494E_4954_484F_5354;
pub const INITRAMFS_ETC_HOSTS_PATH: &[u8] = b"/initramfs/etc/hosts";
/// Compatibility-only legacy path identifier; prefer `INITRAMFS_PROC_MGR_PATH`.
pub const INITRAMFS_PROC_MGR_PATH_PTR: u64 = 0x494E_4954_5052_4F43;
pub const INITRAMFS_PROC_MGR_PATH: &[u8] = b"/initramfs/process_manager";
/// Compatibility-only legacy path identifier; prefer `INITRAMFS_VFS_PATH`.
pub const INITRAMFS_VFS_PATH_PTR: u64 = 0x494E_4954_5F56_4653;
pub const INITRAMFS_VFS_PATH: &[u8] = b"/initramfs/vfs";
/// Compatibility-only legacy path identifier; prefer `INITRAMFS_SUPERVISOR_PATH`.
pub const INITRAMFS_SUPERVISOR_PATH_PTR: u64 = 0x494E_4954_5355_5056;
pub const INITRAMFS_SUPERVISOR_PATH: &[u8] = b"/initramfs/supervisor";
/// Compatibility-only legacy path identifier; prefer `INITRAMFS_POSIX_COMPAT_PATH`.
pub const INITRAMFS_POSIX_COMPAT_PATH_PTR: u64 = 0x494E_4954_5058_434D;
pub const INITRAMFS_POSIX_COMPAT_PATH: &[u8] = b"/initramfs/posix_compat";
/// Compatibility-only legacy path identifier; prefer `INITRAMFS_SRV_PATH`.
pub const INITRAMFS_SRV_PATH_PTR: u64 = 0x494E_4954_5352_5653;
pub const INITRAMFS_SRV_PATH: &[u8] = b"/initramfs/sbin/initramfs_srv";
pub const INITRAMFS_DRIVER_MANAGER_PATH: &[u8] = b"/initramfs/sbin/driver_manager";
pub const INITRAMFS_BLKCACHE_PATH: &[u8] = b"/initramfs/sbin/blkcache_srv";
pub const INITRAMFS_VIRTIO_BLK_PATH: &[u8] = b"/initramfs/sbin/virtio_blk_srv";
pub const INITRAMFS_FAT_SRV_PATH: &[u8] = b"/initramfs/sbin/fat_srv";
pub const INITRAMFS_RAMFS_SRV_PATH: &[u8] = b"/initramfs/sbin/ramfs_srv";
pub const INITRAMFS_EXT4_SRV_PATH: &[u8] = b"/initramfs/sbin/ext4_srv";

const MAX_INITRAMFS_HANDLES: usize = 16;
const MAX_INITRAMFS_INODES: usize = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InitramfsInode {
    path: &'static [u8],
    file_len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenHandle {
    fd: u64,
    inode_idx: usize,
    cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InitramfsMetrics {
    pub open_count: u64,
    pub close_count: u64,
    pub read_count: u64,
    pub write_count: u64,
    pub statx_count: u64,
    pub bytes_read: u64,
    pub error_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitramfsBackend {
    next_fd: u64,
    handles: [Option<OpenHandle>; MAX_INITRAMFS_HANDLES],
    inodes: [Option<InitramfsInode>; MAX_INITRAMFS_INODES],
    metrics: InitramfsMetrics,
    cpio: Option<&'static [u8]>,
}

impl Default for InitramfsBackend {
    fn default() -> Self {
        Self::new(4096)
    }
}

impl InitramfsBackend {
    pub const fn new(boot_file_len: u64) -> Self {
        Self {
            next_fd: 10,
            handles: [None; MAX_INITRAMFS_HANDLES],
            inodes: [
                Some(InitramfsInode {
                    path: INITRAMFS_BOOT_MARKER_PATH,
                    file_len: boot_file_len,
                }),
                Some(InitramfsInode {
                    path: INITRAMFS_INIT_PATH,
                    file_len: 1024,
                }),
                Some(InitramfsInode {
                    path: INITRAMFS_ETC_HOSTS_PATH,
                    file_len: 256,
                }),
                Some(InitramfsInode {
                    path: INITRAMFS_PROC_MGR_PATH,
                    file_len: 1536,
                }),
                Some(InitramfsInode {
                    path: INITRAMFS_VFS_PATH,
                    file_len: 1536,
                }),
                Some(InitramfsInode {
                    path: INITRAMFS_SUPERVISOR_PATH,
                    file_len: 1536,
                }),
                Some(InitramfsInode {
                    path: INITRAMFS_POSIX_COMPAT_PATH,
                    file_len: 1536,
                }),
                Some(InitramfsInode {
                    path: INITRAMFS_SRV_PATH,
                    file_len: 1536,
                }),
                Some(InitramfsInode {
                    path: INITRAMFS_DRIVER_MANAGER_PATH,
                    file_len: 1536,
                }),
                Some(InitramfsInode {
                    path: INITRAMFS_BLKCACHE_PATH,
                    file_len: 1536,
                }),
                Some(InitramfsInode {
                    path: INITRAMFS_VIRTIO_BLK_PATH,
                    file_len: 1536,
                }),
                Some(InitramfsInode {
                    path: INITRAMFS_FAT_SRV_PATH,
                    file_len: 1536,
                }),
                Some(InitramfsInode {
                    path: INITRAMFS_RAMFS_SRV_PATH,
                    file_len: 1536,
                }),
                Some(InitramfsInode {
                    path: INITRAMFS_EXT4_SRV_PATH,
                    file_len: 1536,
                }),
            ],
            metrics: InitramfsMetrics {
                open_count: 0,
                close_count: 0,
                read_count: 0,
                write_count: 0,
                statx_count: 0,
                bytes_read: 0,
                error_count: 0,
            },
            cpio: None,
        }
    }

    pub const fn metrics(&self) -> InitramfsMetrics {
        self.metrics
    }

    pub fn from_cpio_newc(cpio: &[u8]) -> Self {
        let mut backend = Self::new(0);
        for entry in CpioArchive::new(cpio).entries().flatten() {
            if !entry.is_regular_file() {
                continue;
            }
            let path = match entry.name {
                b"init" => INITRAMFS_INIT_PATH,
                b"etc/hosts" => INITRAMFS_ETC_HOSTS_PATH,
                b"sbin/process_manager" => INITRAMFS_PROC_MGR_PATH,
                b"vfs" => INITRAMFS_VFS_PATH,
                b"sbin/supervisor" => INITRAMFS_SUPERVISOR_PATH,
                b"posix_compat" => INITRAMFS_POSIX_COMPAT_PATH,
                b"sbin/initramfs_srv" => INITRAMFS_SRV_PATH,
                b"sbin/driver_manager" => INITRAMFS_DRIVER_MANAGER_PATH,
                b"sbin/blkcache_srv" => INITRAMFS_BLKCACHE_PATH,
                b"sbin/virtio_blk_srv" => INITRAMFS_VIRTIO_BLK_PATH,
                b"sbin/fat_srv" => INITRAMFS_FAT_SRV_PATH,
                b"sbin/ramfs_srv" => INITRAMFS_RAMFS_SRV_PATH,
                b"sbin/ext4_srv" => INITRAMFS_EXT4_SRV_PATH,
                _ => continue,
            };
            if let Some(idx) = backend.lookup_slot(path) {
                backend.inodes[idx] = Some(InitramfsInode {
                    path,
                    file_len: entry.file_data().len() as u64,
                });
            }
        }
        backend
    }

    pub fn from_cpio_newc_static(cpio: &'static [u8]) -> Self {
        let mut backend = Self::from_cpio_newc(cpio);
        backend.cpio = Some(cpio);
        backend
    }

    fn lookup_slot(&self, path: &[u8]) -> Option<usize> {
        self.inodes
            .iter()
            .position(|entry| entry.map(|inode| inode.path == path).unwrap_or(false))
    }

    fn lookup_by_path(&self, path: &[u8]) -> Result<usize, VfsError> {
        self.lookup_slot(path).ok_or(VfsError::InvalidPath)
    }

    fn inode_for_fd(&self, fd: u64) -> Result<InitramfsInode, VfsError> {
        let handle = self
            .handles
            .iter()
            .flatten()
            .find(|handle| handle.fd == fd)
            .ok_or(VfsError::BadFd)?;
        self.inodes[handle.inode_idx].ok_or(VfsError::BadFd)
    }

    /// Returns the bare CPIO entry name for an open fd (strips `/initramfs/` prefix).
    /// Used by the Phase 2B `VFS_OP_READ_BULK` handler to pass to the kernel bulk copy
    /// primitive (`initramfs_write_to_pm_buf` / syscall nr=27 with arg5=PM_TID).
    pub fn cpio_name_for_fd(&self, fd: u64) -> Option<&'static [u8]> {
        let inode = self.inode_for_fd(fd).ok()?;
        Some(
            inode
                .path
                .strip_prefix(b"/initramfs/")
                .unwrap_or(inode.path),
        )
    }

    /// Returns the total file length for an open fd.
    /// Used by the Phase 2B handler to determine whether a bulk read reached EOF.
    pub fn file_len_for_fd(&self, fd: u64) -> Option<u64> {
        let inode = self.inode_for_fd(fd).ok()?;
        Some(inode.file_len)
    }

    fn alloc_handle(&mut self, inode_idx: usize) -> Result<u64, VfsError> {
        let fd = self.next_fd;
        self.next_fd = self.next_fd.saturating_add(1);
        if let Some(slot) = self.handles.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(OpenHandle {
                fd,
                inode_idx,
                cursor: 0,
            });
            return Ok(fd);
        }
        Err(VfsError::NoFd)
    }

    fn close_handle(&mut self, fd: u64) -> Result<(), VfsError> {
        if let Some(slot) = self
            .handles
            .iter_mut()
            .find(|slot| slot.map(|handle| handle.fd == fd).unwrap_or(false))
        {
            *slot = None;
            return Ok(());
        }
        Err(VfsError::BadFd)
    }

    fn is_placeholder_mode(&self) -> bool {
        self.cpio.is_none()
    }

    fn is_late_exec_path(path: &[u8]) -> bool {
        matches!(
            path,
            INITRAMFS_DRIVER_MANAGER_PATH | INITRAMFS_BLKCACHE_PATH | INITRAMFS_VIRTIO_BLK_PATH
        )
    }

    fn reject_placeholder_exec_path(&self, path: &[u8]) -> Result<(), VfsError> {
        if self.is_placeholder_mode() && Self::is_late_exec_path(path) {
            return Err(VfsError::Unsupported);
        }
        Ok(())
    }

    fn statx_value(file_len: u64) -> u64 {
        file_len
    }

    fn metadata_by_path(&self, path: &[u8]) -> Result<u64, VfsError> {
        let inode_idx = self.lookup_by_path(path)?;
        let inode = self.inodes[inode_idx].ok_or(VfsError::BadFd)?;
        Ok(Self::statx_value(inode.file_len))
    }
}

impl VfsBackend for InitramfsBackend {
    fn openat_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        if let Err(err) = self.reject_placeholder_exec_path(path) {
            self.metrics.error_count = self.metrics.error_count.saturating_add(1);
            return Err(err);
        }
        match self
            .lookup_by_path(path)
            .and_then(|inode_idx| self.alloc_handle(inode_idx))
        {
            Ok(fd) => {
                self.metrics.open_count = self.metrics.open_count.saturating_add(1);
                Ok(fd)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err)
            }
        }
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsError> {
        match self.close_handle(fd) {
            Ok(()) => {
                self.metrics.close_count = self.metrics.close_count.saturating_add(1);
                Ok(0)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err)
            }
        }
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        match self.inode_for_fd(fd) {
            Ok(inode) => {
                self.metrics.read_count = self.metrics.read_count.saturating_add(1);
                let read_len = core::cmp::min(len, inode.file_len);
                self.metrics.bytes_read = self.metrics.bytes_read.saturating_add(read_len);
                Ok(read_len)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err)
            }
        }
    }

    fn read_into(&mut self, fd: u64, len: u64, out: &mut [u8]) -> Result<(u64, usize), VfsError> {
        let inode = self.inode_for_fd(fd)?;
        let cpio = match self.cpio {
            Some(c) => c,
            None => return Ok((self.read(fd, len)?, 0)),
        };
        let name = inode
            .path
            .strip_prefix(b"/initramfs/")
            .unwrap_or(inode.path);
        let Some(entry) = yarm_srv_common::cpio::CpioArchive::new(cpio)
            .find(core::str::from_utf8(name).unwrap_or(""))
            .ok()
            .flatten()
        else {
            return Ok((0, 0));
        };
        let handle = self
            .handles
            .iter_mut()
            .flatten()
            .find(|h| h.fd == fd)
            .ok_or(VfsError::BadFd)?;
        let data = entry.file_data();
        if handle.cursor >= data.len() {
            return Ok((0, 0));
        }
        let want = core::cmp::min(len as usize, out.len());
        let n = core::cmp::min(want, data.len() - handle.cursor);
        out[..n].copy_from_slice(&data[handle.cursor..handle.cursor + n]);
        handle.cursor += n;
        self.metrics.read_count = self.metrics.read_count.saturating_add(1);
        self.metrics.bytes_read = self.metrics.bytes_read.saturating_add(n as u64);
        Ok((n as u64, n))
    }

    fn write(&mut self, fd: u64, _len: u64) -> Result<u64, VfsError> {
        if self.inode_for_fd(fd).is_err() {
            self.metrics.error_count = self.metrics.error_count.saturating_add(1);
            return Err(VfsError::BadFd);
        }
        self.metrics.write_count = self.metrics.write_count.saturating_add(1);
        self.metrics.error_count = self.metrics.error_count.saturating_add(1);
        Err(VfsError::Unsupported)
    }

    fn statx_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        if let Err(err) = self.reject_placeholder_exec_path(path) {
            self.metrics.error_count = self.metrics.error_count.saturating_add(1);
            return Err(err);
        }
        match self.metadata_by_path(path) {
            Ok(stat) => {
                self.metrics.statx_count = self.metrics.statx_count.saturating_add(1);
                Ok(stat)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::vec::Vec;

    fn push_entry(out: &mut Vec<u8>, name: &str, mode: u32, data: &[u8]) {
        let namesz = name.len() + 1;
        let mut h = [0u8; 110];
        h[0..6].copy_from_slice(b"070701");
        h[14..22].copy_from_slice(format!("{mode:08x}").as_bytes());
        h[54..62].copy_from_slice(format!("{:08x}", data.len()).as_bytes());
        h[94..102].copy_from_slice(format!("{namesz:08x}").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    #[test]
    fn initramfs_multi_open_allocates_unique_fds() {
        let mut fs = InitramfsBackend::new(4096);
        let fd0 = fs.openat_path(INITRAMFS_BOOT_MARKER_PATH).expect("open");
        let fd1 = fs.openat_path(INITRAMFS_BOOT_MARKER_PATH).expect("open");
        let fd2 = fs.openat_path(INITRAMFS_INIT_PATH).expect("open");
        assert_eq!(fd0, 10);
        assert_eq!(fd1, 11);
        assert_eq!(fd2, 12);
    }

    #[test]
    fn initramfs_openat_path_accepts_real_bytes() {
        let mut fs = InitramfsBackend::new(4096);
        let fd = fs
            .openat_path(INITRAMFS_BOOT_MARKER_PATH)
            .expect("open path");
        assert_eq!(fd, 10);
    }

    #[test]
    fn initramfs_paths_have_stable_read_only_semantics() {
        let mut fs = InitramfsBackend::new(4096);
        let boot_fd = fs.openat_path(INITRAMFS_BOOT_MARKER_PATH).expect("open");
        let init_fd = fs.openat_path(INITRAMFS_INIT_PATH).expect("open");
        assert_eq!(fs.read(boot_fd, 8192), Ok(4096));
        assert_eq!(fs.read(init_fd, 8192), Ok(1024));
        assert_eq!(fs.write(boot_fd, 1), Err(VfsError::Unsupported));
    }

    #[test]
    fn initramfs_statx_contract_returns_file_size() {
        let mut fs = InitramfsBackend::new(4096);
        let boot_stat = fs.statx_path(INITRAMFS_BOOT_MARKER_PATH).expect("stat");
        let hosts_stat = fs.statx_path(INITRAMFS_ETC_HOSTS_PATH).expect("stat");
        assert_eq!(boot_stat, 4096);
        assert_eq!(hosts_stat, 256);
    }

    #[test]
    fn initramfs_statx_path_accepts_real_bytes() {
        let mut fs = InitramfsBackend::new(4096);
        let stat = fs.statx_path(INITRAMFS_VFS_PATH).expect("statx path");
        assert_eq!(stat, 1536);
    }

    #[test]
    fn initramfs_metrics_account_reads_and_errors() {
        let mut fs = InitramfsBackend::new(4096);
        let boot_fd = fs.openat_path(INITRAMFS_BOOT_MARKER_PATH).expect("open");
        let _ = fs.read(boot_fd, 64).expect("read");
        let _ = fs.write(boot_fd, 64).expect_err("write unsupported");
        let _ = fs.close(boot_fd).expect("close");
        let _ = fs.read(boot_fd, 1).expect_err("read closed fd");

        let metrics = fs.metrics();
        assert_eq!(metrics.open_count, 1);
        assert_eq!(metrics.read_count, 1);
        assert_eq!(metrics.bytes_read, 64);
        assert_eq!(metrics.write_count, 1);
        assert_eq!(metrics.close_count, 1);
        assert_eq!(metrics.error_count, 2);
    }

    #[test]
    fn initramfs_core_service_paths_exist_with_stable_statx_sizes() {
        let mut fs = InitramfsBackend::new(4096);
        let proc_stat = fs.statx_path(INITRAMFS_PROC_MGR_PATH).expect("proc stat");
        let vfs_stat = fs.statx_path(INITRAMFS_VFS_PATH).expect("vfs stat");
        let supervisor_stat = fs
            .statx_path(INITRAMFS_SUPERVISOR_PATH)
            .expect("supervisor stat");
        let expected = 1536;
        assert_eq!(proc_stat, expected);
        assert_eq!(vfs_stat, expected);
        assert_eq!(supervisor_stat, expected);
    }

    #[test]
    fn initramfs_cpio_updates_known_file_sizes() {
        let mut cpio = Vec::new();
        push_entry(&mut cpio, "init", 0o100755, &[0u8; 77]);
        push_entry(&mut cpio, "sbin/process_manager", 0o100755, &[0u8; 111]);
        push_entry(&mut cpio, "sbin/supervisor", 0o100755, &[0u8; 135]);
        push_entry(&mut cpio, "vfs", 0o100755, &[0u8; 222]);
        push_entry(&mut cpio, "TRAILER!!!", 0, &[]);
        let leaked: &'static [u8] = Box::leak(cpio.into_boxed_slice());
        let mut fs = InitramfsBackend::from_cpio_newc_static(leaked);
        let init_stat = fs.statx_path(INITRAMFS_INIT_PATH).expect("init stat");
        let proc_stat = fs.statx_path(INITRAMFS_PROC_MGR_PATH).expect("proc stat");
        let sup_stat = fs
            .statx_path(INITRAMFS_SUPERVISOR_PATH)
            .expect("supervisor stat");
        let vfs_stat = fs.statx_path(INITRAMFS_VFS_PATH).expect("vfs stat");
        assert_eq!(init_stat, 77);
        assert_eq!(proc_stat, 111);
        assert_eq!(sup_stat, 135);
        assert_eq!(vfs_stat, 222);
    }

    #[test]
    fn initramfs_placeholder_rejects_late_exec_paths() {
        let mut fs = InitramfsBackend::new(4096);
        assert_eq!(
            fs.statx_path(INITRAMFS_DRIVER_MANAGER_PATH),
            Err(VfsError::Unsupported)
        );
        assert_eq!(
            fs.openat_path(INITRAMFS_DRIVER_MANAGER_PATH),
            Err(VfsError::Unsupported)
        );
    }

    #[test]
    fn initramfs_cpio_allows_late_exec_paths_and_returns_real_size() {
        let mut cpio = Vec::new();
        push_entry(&mut cpio, "sbin/driver_manager", 0o100755, b"ELFdriver");
        push_entry(&mut cpio, "TRAILER!!!", 0, &[]);
        let leaked: &'static [u8] = Box::leak(cpio.into_boxed_slice());
        let mut fs = InitramfsBackend::from_cpio_newc_static(leaked);
        assert_eq!(
            fs.statx_path(INITRAMFS_DRIVER_MANAGER_PATH).expect("stat"),
            10
        );
        let fd = fs.openat_path(INITRAMFS_DRIVER_MANAGER_PATH).expect("open");
        assert!(fd >= 10);
    }

    // ── Phase 2B bulk-read helper method tests ─────────────────────────────

    /// cpio_name_for_fd strips "/initramfs/" prefix and returns the bare CPIO entry name.
    #[test]
    fn cpio_name_for_fd_strips_initramfs_prefix() {
        let mut cpio_data = Vec::new();
        push_entry(&mut cpio_data, "sbin/driver_manager", 0o100755, b"ELFdm");
        push_entry(&mut cpio_data, "TRAILER!!!", 0, &[]);
        let leaked: &'static [u8] = Box::leak(cpio_data.into_boxed_slice());
        let mut fs = InitramfsBackend::from_cpio_newc_static(leaked);
        let fd = fs.openat_path(INITRAMFS_DRIVER_MANAGER_PATH).expect("open");
        let cpio_name = fs.cpio_name_for_fd(fd).expect("cpio name");
        assert_eq!(cpio_name, b"sbin/driver_manager");
    }

    /// file_len_for_fd returns the file length for an open fd.
    #[test]
    fn file_len_for_fd_returns_correct_length() {
        let mut cpio_data = Vec::new();
        push_entry(&mut cpio_data, "sbin/blkcache_srv", 0o100755, &[0u8; 333]);
        push_entry(&mut cpio_data, "TRAILER!!!", 0, &[]);
        let leaked: &'static [u8] = Box::leak(cpio_data.into_boxed_slice());
        let mut fs = InitramfsBackend::from_cpio_newc_static(leaked);
        let fd = fs.openat_path(INITRAMFS_BLKCACHE_PATH).expect("open");
        let file_len = fs.file_len_for_fd(fd).expect("file len");
        assert_eq!(file_len, 333);
    }

    /// cpio_name_for_fd returns None for a nonexistent fd.
    #[test]
    fn cpio_name_for_fd_returns_none_for_invalid_fd() {
        let fs = InitramfsBackend::new(4096);
        assert!(fs.cpio_name_for_fd(999).is_none());
    }

    /// file_len_for_fd returns None for a nonexistent fd.
    #[test]
    fn file_len_for_fd_returns_none_for_invalid_fd() {
        let fs = InitramfsBackend::new(4096);
        assert!(fs.file_len_for_fd(999).is_none());
    }

    /// After close, cpio_name_for_fd returns None (fd ownership enforced).
    #[test]
    fn cpio_name_for_fd_unavailable_after_close() {
        let mut cpio_data = Vec::new();
        push_entry(&mut cpio_data, "sbin/virtio_blk_srv", 0o100755, &[0u8; 77]);
        push_entry(&mut cpio_data, "TRAILER!!!", 0, &[]);
        let leaked: &'static [u8] = Box::leak(cpio_data.into_boxed_slice());
        let mut fs = InitramfsBackend::from_cpio_newc_static(leaked);
        let fd = fs.openat_path(INITRAMFS_VIRTIO_BLK_PATH).expect("open");
        assert!(fs.cpio_name_for_fd(fd).is_some());
        fs.close(fd).expect("close");
        assert!(fs.cpio_name_for_fd(fd).is_none());
    }
}
