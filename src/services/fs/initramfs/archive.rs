// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::vfs::{VfsBackend, VfsError};

pub const INITRAMFS_BOOT_MARKER_PATH_PTR: u64 = 0x494E_4954_424F_4F54;
pub const INITRAMFS_INIT_PATH_PTR: u64 = 0x494E_4954_524F_4F54;
pub const INITRAMFS_ETC_HOSTS_PATH_PTR: u64 = 0x494E_4954_484F_5354;
pub const INITRAMFS_PROC_MGR_PATH_PTR: u64 = 0x494E_4954_5052_4F43;
pub const INITRAMFS_VFS_PATH_PTR: u64 = 0x494E_4954_5F56_4653;
pub const INITRAMFS_SUPERVISOR_PATH_PTR: u64 = 0x494E_4954_5355_5056;

const MAX_INITRAMFS_HANDLES: usize = 16;
const MAX_INITRAMFS_INODES: usize = 8;
const INITRAMFS_STATX_TYPE_REGULAR: u64 = 0x1000_0000_0000_0000;
const INITRAMFS_MODE_OWNER_READ: u64 = 0o400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InitramfsInode {
    path_ptr: u64,
    file_len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenHandle {
    fd: u64,
    inode_idx: usize,
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
                    path_ptr: INITRAMFS_BOOT_MARKER_PATH_PTR,
                    file_len: boot_file_len,
                }),
                Some(InitramfsInode {
                    path_ptr: INITRAMFS_INIT_PATH_PTR,
                    file_len: 1024,
                }),
                Some(InitramfsInode {
                    path_ptr: INITRAMFS_ETC_HOSTS_PATH_PTR,
                    file_len: 256,
                }),
                Some(InitramfsInode {
                    path_ptr: INITRAMFS_PROC_MGR_PATH_PTR,
                    file_len: 1536,
                }),
                Some(InitramfsInode {
                    path_ptr: INITRAMFS_VFS_PATH_PTR,
                    file_len: 1536,
                }),
                Some(InitramfsInode {
                    path_ptr: INITRAMFS_SUPERVISOR_PATH_PTR,
                    file_len: 1536,
                }),
                None,
                None,
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
        }
    }

    pub const fn metrics(&self) -> InitramfsMetrics {
        self.metrics
    }

    fn inode_idx_for_path(&self, path_ptr: u64) -> Result<usize, VfsError> {
        self.inodes
            .iter()
            .position(|entry| {
                entry
                    .map(|inode| inode.path_ptr == path_ptr)
                    .unwrap_or(false)
            })
            .ok_or(VfsError::BadFd)
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

    fn alloc_handle(&mut self, inode_idx: usize) -> Result<u64, VfsError> {
        let fd = self.next_fd;
        self.next_fd = self.next_fd.saturating_add(1);
        if let Some(slot) = self.handles.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(OpenHandle { fd, inode_idx });
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

    fn statx_value(file_len: u64) -> u64 {
        INITRAMFS_STATX_TYPE_REGULAR | INITRAMFS_MODE_OWNER_READ | (file_len << 16)
    }
}

impl VfsBackend for InitramfsBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        match self
            .inode_idx_for_path(path_ptr)
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

    fn write(&mut self, fd: u64, _len: u64) -> Result<u64, VfsError> {
        if self.inode_for_fd(fd).is_err() {
            self.metrics.error_count = self.metrics.error_count.saturating_add(1);
            return Err(VfsError::BadFd);
        }
        self.metrics.write_count = self.metrics.write_count.saturating_add(1);
        self.metrics.error_count = self.metrics.error_count.saturating_add(1);
        Err(VfsError::Unsupported)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        match self
            .inode_idx_for_path(path_ptr)
            .and_then(|inode_idx| self.inodes[inode_idx].ok_or(VfsError::BadFd))
        {
            Ok(inode) => {
                self.metrics.statx_count = self.metrics.statx_count.saturating_add(1);
                Ok(Self::statx_value(inode.file_len))
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

    #[test]
    fn initramfs_multi_open_allocates_unique_fds() {
        let mut fs = InitramfsBackend::new(4096);
        let fd0 = fs.openat(INITRAMFS_BOOT_MARKER_PATH_PTR).expect("open");
        let fd1 = fs.openat(INITRAMFS_BOOT_MARKER_PATH_PTR).expect("open");
        let fd2 = fs.openat(INITRAMFS_INIT_PATH_PTR).expect("open");
        assert_eq!(fd0, 10);
        assert_eq!(fd1, 11);
        assert_eq!(fd2, 12);
    }

    #[test]
    fn initramfs_paths_have_stable_read_only_semantics() {
        let mut fs = InitramfsBackend::new(4096);
        let boot_fd = fs.openat(INITRAMFS_BOOT_MARKER_PATH_PTR).expect("open");
        let init_fd = fs.openat(INITRAMFS_INIT_PATH_PTR).expect("open");
        assert_eq!(fs.read(boot_fd, 8192), Ok(4096));
        assert_eq!(fs.read(init_fd, 8192), Ok(1024));
        assert_eq!(fs.write(boot_fd, 1), Err(VfsError::Unsupported));
    }

    #[test]
    fn initramfs_statx_contract_encodes_type_mode_and_size() {
        let mut fs = InitramfsBackend::new(4096);
        let boot_stat = fs.statx(INITRAMFS_BOOT_MARKER_PATH_PTR).expect("stat");
        let hosts_stat = fs.statx(INITRAMFS_ETC_HOSTS_PATH_PTR).expect("stat");
        assert_eq!(
            boot_stat,
            INITRAMFS_STATX_TYPE_REGULAR | INITRAMFS_MODE_OWNER_READ | (4096 << 16)
        );
        assert_eq!(
            hosts_stat,
            INITRAMFS_STATX_TYPE_REGULAR | INITRAMFS_MODE_OWNER_READ | (256 << 16)
        );
    }

    #[test]
    fn initramfs_metrics_account_reads_and_errors() {
        let mut fs = InitramfsBackend::new(4096);
        let boot_fd = fs.openat(INITRAMFS_BOOT_MARKER_PATH_PTR).expect("open");
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
        let proc_stat = fs.statx(INITRAMFS_PROC_MGR_PATH_PTR).expect("proc stat");
        let vfs_stat = fs.statx(INITRAMFS_VFS_PATH_PTR).expect("vfs stat");
        let supervisor_stat = fs
            .statx(INITRAMFS_SUPERVISOR_PATH_PTR)
            .expect("supervisor stat");
        let expected = INITRAMFS_STATX_TYPE_REGULAR | INITRAMFS_MODE_OWNER_READ | (1536 << 16);
        assert_eq!(proc_stat, expected);
        assert_eq!(vfs_stat, expected);
        assert_eq!(supervisor_stat, expected);
    }
}
