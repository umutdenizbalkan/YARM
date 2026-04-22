// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::super::common::vfs_ipc::{VfsBackend, VfsError};

pub const DEV_CONSOLE_PATH_PTR: u64 = 0x434F_4E53_4F4C_4500;
pub const DEV_NULL_PATH_PTR: u64 = 0x4445_564E_554C_4C00;

const MAX_OPEN_HANDLES: usize = 16;
const DEVFS_STATX_TYPE_CHAR_DEVICE: u64 = 0x2000_0000_0000_0000;
const DEVFS_MODE_OWNER_READ: u64 = 0o400;
const DEVFS_MODE_OWNER_WRITE: u64 = 0o200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DevNode {
    Console,
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenHandle {
    fd: u64,
    node: DevNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DevFsMetrics {
    pub open_count: u64,
    pub close_count: u64,
    pub read_count: u64,
    pub write_count: u64,
    pub statx_count: u64,
    pub console_bytes_written: u64,
    pub null_bytes_written: u64,
    pub error_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevFsBackend {
    next_fd: u64,
    handles: [Option<OpenHandle>; MAX_OPEN_HANDLES],
    metrics: DevFsMetrics,
}

impl Default for DevFsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DevFsBackend {
    pub const fn new() -> Self {
        Self {
            next_fd: 3,
            handles: [None; MAX_OPEN_HANDLES],
            metrics: DevFsMetrics {
                open_count: 0,
                close_count: 0,
                read_count: 0,
                write_count: 0,
                statx_count: 0,
                console_bytes_written: 0,
                null_bytes_written: 0,
                error_count: 0,
            },
        }
    }

    pub const fn metrics(&self) -> DevFsMetrics {
        self.metrics
    }

    fn resolve_node(path_ptr: u64) -> Result<DevNode, VfsError> {
        if path_ptr == DEV_CONSOLE_PATH_PTR {
            return Ok(DevNode::Console);
        }
        if path_ptr == DEV_NULL_PATH_PTR {
            return Ok(DevNode::Null);
        }
        Err(VfsError::BadFd)
    }

    fn alloc_handle(&mut self, node: DevNode) -> Result<u64, VfsError> {
        let fd = self.next_fd;
        self.next_fd = self.next_fd.saturating_add(1);
        if let Some(slot) = self.handles.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(OpenHandle { fd, node });
            return Ok(fd);
        }
        Err(VfsError::NoFd)
    }

    fn node_for_fd(&self, fd: u64) -> Result<DevNode, VfsError> {
        self.handles
            .iter()
            .flatten()
            .find(|handle| handle.fd == fd)
            .map(|handle| handle.node)
            .ok_or(VfsError::BadFd)
    }

    fn close_fd(&mut self, fd: u64) -> Result<(), VfsError> {
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

    fn statx_for_node(node: DevNode) -> u64 {
        match node {
            DevNode::Console => DEVFS_STATX_TYPE_CHAR_DEVICE | DEVFS_MODE_OWNER_WRITE,
            DevNode::Null => {
                DEVFS_STATX_TYPE_CHAR_DEVICE | DEVFS_MODE_OWNER_READ | DEVFS_MODE_OWNER_WRITE
            }
        }
    }
}

impl VfsBackend for DevFsBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        match Self::resolve_node(path_ptr).and_then(|node| self.alloc_handle(node)) {
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
        match self.node_for_fd(fd) {
            Ok(DevNode::Null) => {
                self.metrics.read_count = self.metrics.read_count.saturating_add(1);
                let _ = len;
                Ok(0)
            }
            Ok(DevNode::Console) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(VfsError::Unsupported)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err)
            }
        }
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        match self.node_for_fd(fd) {
            Ok(DevNode::Console) => {
                self.metrics.write_count = self.metrics.write_count.saturating_add(1);
                self.metrics.console_bytes_written =
                    self.metrics.console_bytes_written.saturating_add(len);
                Ok(len)
            }
            Ok(DevNode::Null) => {
                self.metrics.write_count = self.metrics.write_count.saturating_add(1);
                self.metrics.null_bytes_written =
                    self.metrics.null_bytes_written.saturating_add(len);
                Ok(len)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err)
            }
        }
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        match Self::resolve_node(path_ptr) {
            Ok(node) => {
                self.metrics.statx_count = self.metrics.statx_count.saturating_add(1);
                Ok(Self::statx_for_node(node))
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
    fn multi_open_allocates_unique_handles() {
        let mut backend = DevFsBackend::new();
        let fd0 = backend.openat(DEV_CONSOLE_PATH_PTR).expect("console open");
        let fd1 = backend.openat(DEV_CONSOLE_PATH_PTR).expect("console open");
        let fd2 = backend.openat(DEV_NULL_PATH_PTR).expect("null open");
        assert_ne!(fd0, fd1);
        assert_ne!(fd1, fd2);
        assert_eq!(fd0, 3);
        assert_eq!(fd1, 4);
        assert_eq!(fd2, 5);
    }

    #[test]
    fn node_specific_read_write_semantics_are_enforced() {
        let mut backend = DevFsBackend::new();
        let console_fd = backend.openat(DEV_CONSOLE_PATH_PTR).expect("console");
        let null_fd = backend.openat(DEV_NULL_PATH_PTR).expect("null");
        assert_eq!(backend.write(console_fd, 11), Ok(11));
        assert_eq!(backend.write(null_fd, 7), Ok(7));
        assert_eq!(backend.read(null_fd, 64), Ok(0));
        assert_eq!(backend.read(console_fd, 64), Err(VfsError::Unsupported));
    }

    #[test]
    fn statx_contract_returns_node_specific_metadata() {
        let mut backend = DevFsBackend::new();
        let console = backend.statx(DEV_CONSOLE_PATH_PTR).expect("console stat");
        let null = backend.statx(DEV_NULL_PATH_PTR).expect("null stat");
        assert_eq!(
            console,
            DEVFS_STATX_TYPE_CHAR_DEVICE | DEVFS_MODE_OWNER_WRITE
        );
        assert_eq!(
            null,
            DEVFS_STATX_TYPE_CHAR_DEVICE | DEVFS_MODE_OWNER_READ | DEVFS_MODE_OWNER_WRITE
        );
    }

    #[test]
    fn metrics_track_success_and_error_paths() {
        let mut backend = DevFsBackend::new();
        let console_fd = backend.openat(DEV_CONSOLE_PATH_PTR).expect("console");
        let null_fd = backend.openat(DEV_NULL_PATH_PTR).expect("null");
        let _ = backend.write(console_fd, 8).expect("console write");
        let _ = backend.write(null_fd, 5).expect("null write");
        let _ = backend
            .read(console_fd, 1)
            .expect_err("console read unsupported");
        let _ = backend.read(null_fd, 2).expect("null read");
        let _ = backend.close(console_fd).expect("close console");
        let _ = backend.close(null_fd).expect("close null");
        let _ = backend.close(999).expect_err("close bad fd");

        let metrics = backend.metrics();
        assert_eq!(metrics.open_count, 2);
        assert_eq!(metrics.close_count, 2);
        assert_eq!(metrics.read_count, 1);
        assert_eq!(metrics.write_count, 2);
        assert_eq!(metrics.console_bytes_written, 8);
        assert_eq!(metrics.null_bytes_written, 5);
        assert_eq!(metrics.error_count, 2);
    }
}
