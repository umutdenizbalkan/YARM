// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::super::common::vfs_ipc::{VfsBackend, VfsError};
use super::super::common::fs::{
    FdRecord, InodeRecord, MAX_SERVICE_FDS, MAX_SERVICE_INODES, ServiceFsBackend, find_inode_index,
};

const RAMFS_STATX_TYPE_REGULAR: u64 = 0x1000_0000_0000_0000;
const RAMFS_MODE_OWNER_READ: u64 = 0o400;
const RAMFS_MODE_OWNER_WRITE: u64 = 0o200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RamFsMetrics {
    pub open_count: u64,
    pub close_count: u64,
    pub read_count: u64,
    pub write_count: u64,
    pub statx_count: u64,
    pub bytes_written: u64,
    pub bytes_read: u64,
    pub error_count: u64,
}

#[derive(Debug)]
pub struct RamFsBackend {
    next_fd: u64,
    fds: [Option<FdRecord>; MAX_SERVICE_FDS],
    inodes: [Option<InodeRecord>; MAX_SERVICE_INODES],
    metrics: RamFsMetrics,
}

impl Default for RamFsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RamFsBackend {
    pub const fn new() -> Self {
        Self {
            next_fd: 100,
            fds: [None; MAX_SERVICE_FDS],
            inodes: [None; MAX_SERVICE_INODES],
            metrics: RamFsMetrics {
                open_count: 0,
                close_count: 0,
                read_count: 0,
                write_count: 0,
                statx_count: 0,
                bytes_written: 0,
                bytes_read: 0,
                error_count: 0,
            },
        }
    }

    pub const fn metrics(&self) -> RamFsMetrics {
        self.metrics
    }

    fn alloc_fd(&mut self, inode: u64) -> Result<u64, VfsError> {
        let fd = self.next_fd;
        self.next_fd = self.next_fd.saturating_add(1);
        if let Some(slot) = self.fds.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(FdRecord { fd, inode });
            Ok(fd)
        } else {
            Err(VfsError::NoFd)
        }
    }

    fn open_inode(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        if let Some(inode) = self
            .inodes
            .iter()
            .flatten()
            .find(|inode| inode.path_ptr == path_ptr)
            .map(|inode| inode.path_ptr)
        {
            return self.alloc_fd(inode);
        }
        if let Some(slot) = self.inodes.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(InodeRecord {
                path_ptr,
                file_len: 0,
            });
            return self.alloc_fd(path_ptr);
        }
        Err(VfsError::NoFd)
    }

    fn close_fd(&mut self, fd: u64) -> Result<(), VfsError> {
        if let Some(slot) = self
            .fds
            .iter_mut()
            .find(|slot| slot.map(|entry| entry.fd == fd).unwrap_or(false))
        {
            *slot = None;
            Ok(())
        } else {
            Err(VfsError::BadFd)
        }
    }

    fn inode_for_fd(&self, fd: u64) -> Option<u64> {
        self.fds
            .iter()
            .flatten()
            .find(|entry| entry.fd == fd)
            .map(|entry| entry.inode)
    }

    fn statx_value(file_len: u64) -> u64 {
        RAMFS_STATX_TYPE_REGULAR | RAMFS_MODE_OWNER_READ | RAMFS_MODE_OWNER_WRITE | (file_len << 16)
    }
}

impl ServiceFsBackend for RamFsBackend {
    fn name(&self) -> &'static str {
        "ramfs"
    }

    fn validate(&self) -> Result<(), VfsError> {
        Ok(())
    }
}

impl VfsBackend for RamFsBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        match self.open_inode(path_ptr) {
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
        match self.close_fd(fd) {
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
        let inode = match self.inode_for_fd(fd) {
            Some(inode) => inode,
            None => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                return Err(VfsError::BadFd);
            }
        };
        let idx = find_inode_index(&self.inodes, inode).ok_or(VfsError::BadFd)?;
        let file_len = self.inodes[idx].ok_or(VfsError::BadFd)?.file_len;
        let read_len = core::cmp::min(len, file_len);
        self.metrics.read_count = self.metrics.read_count.saturating_add(1);
        self.metrics.bytes_read = self.metrics.bytes_read.saturating_add(read_len);
        Ok(read_len)
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        let inode = match self.inode_for_fd(fd) {
            Some(inode) => inode,
            None => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                return Err(VfsError::BadFd);
            }
        };
        let idx = find_inode_index(&self.inodes, inode).ok_or(VfsError::BadFd)?;
        let Some(mut inode_slot) = self.inodes[idx] else {
            self.metrics.error_count = self.metrics.error_count.saturating_add(1);
            return Err(VfsError::BadFd);
        };
        inode_slot.file_len = inode_slot.file_len.saturating_add(len);
        self.inodes[idx] = Some(inode_slot);
        self.metrics.write_count = self.metrics.write_count.saturating_add(1);
        self.metrics.bytes_written = self.metrics.bytes_written.saturating_add(len);
        Ok(len)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        let idx = match find_inode_index(&self.inodes, path_ptr) {
            Some(idx) => idx,
            None => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                return Err(VfsError::BadFd);
            }
        };
        let file_len = self.inodes[idx].ok_or(VfsError::BadFd)?.file_len;
        self.metrics.statx_count = self.metrics.statx_count.saturating_add(1);
        Ok(Self::statx_value(file_len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramfs_multi_open_allocates_unique_handles() {
        let mut fs = RamFsBackend::new();
        let fd0 = fs.openat(0x1010).expect("open");
        let fd1 = fs.openat(0x1010).expect("open");
        let fd2 = fs.openat(0x2020).expect("open");
        assert_eq!(fd0, 100);
        assert_eq!(fd1, 101);
        assert_eq!(fd2, 102);
    }

    #[test]
    fn ramfs_statx_contract_encodes_type_mode_and_size() {
        let mut fs = RamFsBackend::new();
        let fd = fs.openat(0x1010).expect("open");
        let _ = fs.write(fd, 128).expect("write");
        assert_eq!(
            fs.statx(0x1010).expect("stat"),
            RAMFS_STATX_TYPE_REGULAR | RAMFS_MODE_OWNER_READ | RAMFS_MODE_OWNER_WRITE | (128 << 16)
        );
    }

    #[test]
    fn ramfs_metrics_account_reads_writes_and_errors() {
        let mut fs = RamFsBackend::new();
        let fd = fs.openat(0x1010).expect("open");
        let _ = fs.write(fd, 64).expect("write");
        let _ = fs.read(fd, 32).expect("read");
        let _ = fs.close(fd).expect("close");
        let _ = fs.read(fd, 1).expect_err("read closed fd");

        let metrics = fs.metrics();
        assert_eq!(metrics.open_count, 1);
        assert_eq!(metrics.write_count, 1);
        assert_eq!(metrics.read_count, 1);
        assert_eq!(metrics.close_count, 1);
        assert_eq!(metrics.bytes_written, 64);
        assert_eq!(metrics.bytes_read, 32);
        assert_eq!(metrics.error_count, 1);
    }
}
