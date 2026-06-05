// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use alloc::vec::Vec;

use super::super::common::fs::ServiceFsBackend;
use super::super::common::vfs_ipc::{VfsBackend, VfsError, normalize_path};

/// Compatibility path-id constant used by mount/policy/interop tests.
pub const RAMFS_BOOT_PATH_PTR: u64 = 0xA000;
pub const RAMFS_BOOT_PATH: &[u8] = b"/ram/boot";

pub const RAMFS_DEFAULT_MAX_BYTES: usize = 512 * 1024;
pub const RAMFS_DEFAULT_MAX_NODES: usize = 128;
pub const RAMFS_PATH_MAX: usize = 96;

pub const RAMFS_STATX_TYPE_REGULAR: u64 = 0x1000_0000_0000_0000;
pub const RAMFS_STATX_TYPE_DIRECTORY: u64 = 0x2000_0000_0000_0000;
pub const RAMFS_MODE_OWNER_READ: u64 = 0o400;
pub const RAMFS_MODE_OWNER_WRITE: u64 = 0o200;
pub const RAMFS_MODE_OWNER_EXEC: u64 = 0o100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RamFsNodeKind {
    Directory,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RamFsError {
    InvalidPath,
    NameTooLong,
    NotFound,
    AlreadyExists,
    NotDirectory,
    IsDirectory,
    Capacity,
    BadFd,
    Unsupported,
}

impl From<RamFsError> for VfsError {
    fn from(value: RamFsError) -> Self {
        match value {
            RamFsError::InvalidPath => VfsError::InvalidPath,
            RamFsError::NameTooLong => VfsError::NameTooLong,
            RamFsError::NotFound | RamFsError::NotDirectory | RamFsError::IsDirectory => {
                VfsError::InvalidPath
            }
            RamFsError::AlreadyExists => VfsError::PermissionDenied,
            RamFsError::Capacity => VfsError::NoFd,
            RamFsError::BadFd => VfsError::BadFd,
            RamFsError::Unsupported => VfsError::Unsupported,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RamFsNode {
    path: Vec<u8>,
    kind: RamFsNodeKind,
    data: Vec<u8>,
}

impl RamFsNode {
    pub fn path(&self) -> &[u8] {
        &self.path
    }

    pub const fn kind(&self) -> RamFsNodeKind {
        self.kind
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenFile {
    fd: u64,
    node: usize,
    offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RamFsMetrics {
    pub open_count: u64,
    pub close_count: u64,
    pub read_count: u64,
    pub write_count: u64,
    pub statx_count: u64,
    pub mkdir_count: u64,
    pub unlink_count: u64,
    pub bytes_written: u64,
    pub bytes_read: u64,
    pub error_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamFsLimits {
    pub max_bytes: usize,
    pub max_nodes: usize,
}

impl Default for RamFsLimits {
    fn default() -> Self {
        Self {
            max_bytes: RAMFS_DEFAULT_MAX_BYTES,
            max_nodes: RAMFS_DEFAULT_MAX_NODES,
        }
    }
}

#[derive(Debug)]
pub struct RamFsBackend {
    next_fd: u64,
    fds: Vec<OpenFile>,
    nodes: Vec<RamFsNode>,
    limits: RamFsLimits,
    metrics: RamFsMetrics,
}

impl Default for RamFsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RamFsBackend {
    pub fn new() -> Self {
        Self::with_limits(RamFsLimits::default())
    }

    pub fn with_limits(limits: RamFsLimits) -> Self {
        let mut backend = Self {
            next_fd: 100,
            fds: Vec::new(),
            nodes: Vec::new(),
            limits,
            metrics: RamFsMetrics::default(),
        };
        // Root and the boot smoke file are seeded without panicking; if a custom limit is
        // too small the backend still contains at least root and reports capacity on more work.
        let _ = backend.create_root();
        let _ = backend.mkdir_path(b"/ram");
        let _ = backend.create_file(RAMFS_BOOT_PATH);
        backend.metrics = RamFsMetrics::default();
        backend
    }

    pub const fn metrics(&self) -> RamFsMetrics {
        self.metrics
    }

    pub fn limits(&self) -> RamFsLimits {
        self.limits
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn used_bytes(&self) -> usize {
        self.nodes.iter().map(|node| node.data.len()).sum()
    }

    pub fn node_kind(&self, path: &[u8]) -> Result<RamFsNodeKind, RamFsError> {
        let path = normalized_vec(path)?;
        let idx = self
            .lookup_index(path.as_slice())
            .ok_or(RamFsError::NotFound)?;
        Ok(self.nodes[idx].kind)
    }

    fn create_root(&mut self) -> Result<(), RamFsError> {
        if self.nodes.iter().any(|node| node.path == b"/") {
            return Ok(());
        }
        self.push_node(b"/".to_vec(), RamFsNodeKind::Directory)
    }

    fn push_node(&mut self, path: Vec<u8>, kind: RamFsNodeKind) -> Result<(), RamFsError> {
        if self.nodes.len() >= self.limits.max_nodes {
            return Err(RamFsError::Capacity);
        }
        self.nodes.push(RamFsNode {
            path,
            kind,
            data: Vec::new(),
        });
        Ok(())
    }

    fn lookup_index(&self, path: &[u8]) -> Option<usize> {
        self.nodes.iter().position(|node| node.path == path)
    }

    fn parent_path(path: &[u8]) -> Result<&[u8], RamFsError> {
        if path == b"/" {
            return Err(RamFsError::InvalidPath);
        }
        let slash = path
            .iter()
            .rposition(|byte| *byte == b'/')
            .ok_or(RamFsError::InvalidPath)?;
        if slash == 0 {
            Ok(b"/")
        } else {
            Ok(&path[..slash])
        }
    }

    fn ensure_parent_dir(&self, path: &[u8]) -> Result<(), RamFsError> {
        let parent = Self::parent_path(path)?;
        let idx = self.lookup_index(parent).ok_or(RamFsError::NotFound)?;
        if self.nodes[idx].kind == RamFsNodeKind::Directory {
            Ok(())
        } else {
            Err(RamFsError::NotDirectory)
        }
    }

    pub fn mkdir_path(&mut self, path: &[u8]) -> Result<(), RamFsError> {
        let path = normalized_vec(path)?;
        if path.as_slice() == b"/" {
            return Err(RamFsError::AlreadyExists);
        }
        if self.lookup_index(path.as_slice()).is_some() {
            return Err(RamFsError::AlreadyExists);
        }
        self.ensure_parent_dir(path.as_slice())?;
        self.push_node(path, RamFsNodeKind::Directory)?;
        self.metrics.mkdir_count = self.metrics.mkdir_count.saturating_add(1);
        Ok(())
    }

    pub fn create_file(&mut self, path: &[u8]) -> Result<(), RamFsError> {
        let path = normalized_vec(path)?;
        if path.as_slice() == b"/" {
            return Err(RamFsError::IsDirectory);
        }
        if self.lookup_index(path.as_slice()).is_some() {
            return Err(RamFsError::AlreadyExists);
        }
        self.ensure_parent_dir(path.as_slice())?;
        self.push_node(path, RamFsNodeKind::File)
    }

    pub fn unlink_path(&mut self, path: &[u8]) -> Result<(), RamFsError> {
        let path = normalized_vec(path)?;
        let idx = self
            .lookup_index(path.as_slice())
            .ok_or(RamFsError::NotFound)?;
        if self.nodes[idx].kind == RamFsNodeKind::Directory {
            return Err(RamFsError::IsDirectory);
        }
        self.nodes.remove(idx);
        self.fds.retain(|fd| fd.node != idx);
        for fd in &mut self.fds {
            if fd.node > idx {
                fd.node -= 1;
            }
        }
        self.metrics.unlink_count = self.metrics.unlink_count.saturating_add(1);
        Ok(())
    }

    pub fn open_path(&mut self, path: &[u8]) -> Result<u64, RamFsError> {
        let path = normalized_vec(path)?;
        let node = self
            .lookup_index(path.as_slice())
            .ok_or(RamFsError::NotFound)?;
        let fd = self.next_fd;
        self.next_fd = self.next_fd.saturating_add(1);
        self.fds.push(OpenFile {
            fd,
            node,
            offset: 0,
        });
        Ok(fd)
    }

    pub fn close_fd(&mut self, fd: u64) -> Result<(), RamFsError> {
        let idx = self
            .fds
            .iter()
            .position(|entry| entry.fd == fd)
            .ok_or(RamFsError::BadFd)?;
        self.fds.remove(idx);
        Ok(())
    }

    fn fd_index(&self, fd: u64) -> Result<usize, RamFsError> {
        self.fds
            .iter()
            .position(|entry| entry.fd == fd)
            .ok_or(RamFsError::BadFd)
    }

    pub fn read_bytes(&mut self, fd: u64, out: &mut [u8]) -> Result<usize, RamFsError> {
        let fd_idx = self.fd_index(fd)?;
        let node_idx = self.fds[fd_idx].node;
        if self.nodes[node_idx].kind == RamFsNodeKind::Directory {
            return Err(RamFsError::IsDirectory);
        }
        let offset = self.fds[fd_idx].offset;
        let data = self.nodes[node_idx].data.as_slice();
        let available = data.len().saturating_sub(offset);
        let len = core::cmp::min(out.len(), available);
        out[..len].copy_from_slice(&data[offset..offset + len]);
        self.fds[fd_idx].offset = self.fds[fd_idx].offset.saturating_add(len);
        Ok(len)
    }

    pub fn read_len(&mut self, fd: u64, len: usize) -> Result<usize, RamFsError> {
        let fd_idx = self.fd_index(fd)?;
        let node_idx = self.fds[fd_idx].node;
        if self.nodes[node_idx].kind == RamFsNodeKind::Directory {
            return Err(RamFsError::IsDirectory);
        }
        let available = self.nodes[node_idx]
            .data
            .len()
            .saturating_sub(self.fds[fd_idx].offset);
        let read_len = core::cmp::min(len, available);
        self.fds[fd_idx].offset = self.fds[fd_idx].offset.saturating_add(read_len);
        Ok(read_len)
    }

    pub fn write_bytes(&mut self, fd: u64, bytes: &[u8]) -> Result<usize, RamFsError> {
        let fd_idx = self.fd_index(fd)?;
        let node_idx = self.fds[fd_idx].node;
        if self.nodes[node_idx].kind == RamFsNodeKind::Directory {
            return Err(RamFsError::IsDirectory);
        }
        let offset = self.fds[fd_idx].offset;
        let end = offset
            .checked_add(bytes.len())
            .ok_or(RamFsError::Capacity)?;
        let current_len = self.nodes[node_idx].data.len();
        let growth = end.saturating_sub(current_len);
        if self.used_bytes().saturating_add(growth) > self.limits.max_bytes {
            return Err(RamFsError::Capacity);
        }
        if end > current_len {
            self.nodes[node_idx].data.resize(end, 0);
        }
        self.nodes[node_idx].data[offset..end].copy_from_slice(bytes);
        self.fds[fd_idx].offset = end;
        Ok(bytes.len())
    }

    pub fn write_zeroes(&mut self, fd: u64, len: usize) -> Result<usize, RamFsError> {
        let bytes = alloc::vec![0u8; len];
        self.write_bytes(fd, bytes.as_slice())
    }

    pub fn statx_path_value(&self, path: &[u8]) -> Result<u64, RamFsError> {
        let path = normalized_vec(path)?;
        let idx = self
            .lookup_index(path.as_slice())
            .ok_or(RamFsError::NotFound)?;
        let node = &self.nodes[idx];
        let kind = match node.kind {
            RamFsNodeKind::File => RAMFS_STATX_TYPE_REGULAR,
            RamFsNodeKind::Directory => RAMFS_STATX_TYPE_DIRECTORY,
        };
        let mode = match node.kind {
            RamFsNodeKind::File => RAMFS_MODE_OWNER_READ | RAMFS_MODE_OWNER_WRITE,
            RamFsNodeKind::Directory => {
                RAMFS_MODE_OWNER_READ | RAMFS_MODE_OWNER_WRITE | RAMFS_MODE_OWNER_EXEC
            }
        };
        Ok(kind | mode | ((node.data.len() as u64) << 16))
    }
}

fn normalized_vec(path: &[u8]) -> Result<Vec<u8>, RamFsError> {
    let normalized = normalize_path(path).map_err(|err| match err {
        VfsError::NameTooLong => RamFsError::NameTooLong,
        VfsError::Malformed | VfsError::InvalidPath => RamFsError::InvalidPath,
        _ => RamFsError::InvalidPath,
    })?;
    Ok(normalized.as_slice().to_vec())
}

impl ServiceFsBackend for RamFsBackend {
    fn name(&self) -> &'static str {
        "ramfs"
    }

    fn validate(&self) -> Result<(), VfsError> {
        if self.lookup_index(b"/").is_some() {
            Ok(())
        } else {
            Err(VfsError::InvalidPath)
        }
    }
}

impl VfsBackend for RamFsBackend {
    fn openat_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        match self.open_path(path) {
            Ok(fd) => {
                self.metrics.open_count = self.metrics.open_count.saturating_add(1);
                yarm_user_rt::user_log!(
                    "RAMFS_OPEN_OK path={}",
                    alloc::string::String::from_utf8_lossy(path)
                );
                Ok(fd)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err.into())
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
                Err(err.into())
            }
        }
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        match self.read_len(fd, usize::try_from(len).unwrap_or(usize::MAX)) {
            Ok(read_len) => {
                self.metrics.read_count = self.metrics.read_count.saturating_add(1);
                self.metrics.bytes_read = self.metrics.bytes_read.saturating_add(read_len as u64);
                yarm_user_rt::user_log!("RAMFS_READ_OK fd={} len={}", fd, read_len as u64);
                Ok(read_len as u64)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err.into())
            }
        }
    }

    fn read_into(&mut self, fd: u64, len: u64, out: &mut [u8]) -> Result<(u64, usize), VfsError> {
        let capped = core::cmp::min(usize::try_from(len).unwrap_or(usize::MAX), out.len());
        match self.read_bytes(fd, &mut out[..capped]) {
            Ok(read_len) => {
                self.metrics.read_count = self.metrics.read_count.saturating_add(1);
                self.metrics.bytes_read = self.metrics.bytes_read.saturating_add(read_len as u64);
                yarm_user_rt::user_log!("RAMFS_READ_OK fd={} len={}", fd, read_len as u64);
                Ok((read_len as u64, read_len))
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err.into())
            }
        }
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        match self.write_zeroes(fd, usize::try_from(len).unwrap_or(usize::MAX)) {
            Ok(written) => {
                self.metrics.write_count = self.metrics.write_count.saturating_add(1);
                self.metrics.bytes_written =
                    self.metrics.bytes_written.saturating_add(written as u64);
                yarm_user_rt::user_log!("RAMFS_WRITE_OK fd={} len={}", fd, written as u64);
                Ok(written as u64)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err.into())
            }
        }
    }

    fn statx_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        match self.statx_path_value(path) {
            Ok(stat) => {
                self.metrics.statx_count = self.metrics.statx_count.saturating_add(1);
                Ok(stat)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err.into())
            }
        }
    }

    fn ioctl(&mut self, _fd: u64, _request: u64, _arg: u64) -> Result<u64, VfsError> {
        yarm_user_rt::user_log!("RAMFS_UNSUPPORTED_OP op=ioctl");
        Err(VfsError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::common::vfs_ipc::{VfsWritePayload, write_inline_message, write_shared_message};
    use yarm_ipc_abi::vfs_abi::{
        VFS_SHARED_BUFFER_FS_READ, VFS_WRITE_INLINE_MAX_BYTES, VfsSharedBufferDescriptor,
        VfsWriteInlineRequest, VfsWriteSharedRequest,
    };

    #[test]
    fn empty_root_exists() {
        let fs = RamFsBackend::new();
        assert_eq!(fs.node_kind(b"/"), Ok(RamFsNodeKind::Directory));
    }

    #[test]
    fn create_write_read_file() {
        let mut fs = RamFsBackend::new();
        fs.create_file(b"/ram/hello").expect("create");
        let fd = fs.open_path(b"/ram/hello").expect("open");
        assert_eq!(fs.write_bytes(fd, b"hello"), Ok(5));
        fs.close_fd(fd).expect("close");
        let fd = fs.open_path(b"/ram/hello").expect("reopen");
        let mut out = [0u8; 8];
        assert_eq!(fs.read_bytes(fd, &mut out), Ok(5));
        assert_eq!(&out[..5], b"hello");
    }

    #[test]
    fn read_eof_clamps() {
        let mut fs = RamFsBackend::new();
        fs.create_file(b"/ram/eof").expect("create");
        let fd = fs.open_path(b"/ram/eof").expect("open");
        fs.write_bytes(fd, b"abc").expect("write");
        fs.close_fd(fd).expect("close");
        let fd = fs.open_path(b"/ram/eof").expect("reopen");
        let mut out = [0u8; 8];
        assert_eq!(fs.read_bytes(fd, &mut out), Ok(3));
        assert_eq!(fs.read_bytes(fd, &mut out), Ok(0));
    }

    #[test]
    fn overwrite_and_append_follow_file_offset() {
        let mut fs = RamFsBackend::new();
        fs.create_file(b"/ram/data").expect("create");
        let fd = fs.open_path(b"/ram/data").expect("open");
        fs.write_bytes(fd, b"abc").expect("write");
        fs.write_bytes(fd, b"def").expect("append");
        fs.close_fd(fd).expect("close");
        let fd = fs.open_path(b"/ram/data").expect("reopen");
        let mut out = [0u8; 6];
        assert_eq!(fs.read_bytes(fd, &mut out), Ok(6));
        assert_eq!(&out, b"abcdef");
    }

    #[test]
    fn mkdir_nested_and_repeated_slash_normalization() {
        let mut fs = RamFsBackend::new();
        fs.mkdir_path(b"/ram//tmp").expect("mkdir tmp");
        fs.mkdir_path(b"/ram/tmp/nested").expect("mkdir nested");
        fs.create_file(b"/ram/tmp//nested/file").expect("create");
        assert_eq!(
            fs.node_kind(b"/ram/tmp/nested/file"),
            Ok(RamFsNodeKind::File)
        );
    }

    #[test]
    fn stat_file_vs_directory() {
        let mut fs = RamFsBackend::new();
        fs.mkdir_path(b"/ram/statdir").expect("mkdir");
        fs.create_file(b"/ram/statfile").expect("create");
        let fd = fs.open_path(b"/ram/statfile").expect("open");
        fs.write_bytes(fd, b"abcd").expect("write");
        assert_eq!(
            fs.statx_path_value(b"/ram/statfile").expect("stat file"),
            RAMFS_STATX_TYPE_REGULAR | RAMFS_MODE_OWNER_READ | RAMFS_MODE_OWNER_WRITE | (4 << 16)
        );
        assert_eq!(
            fs.statx_path_value(b"/ram/statdir").expect("stat dir"),
            RAMFS_STATX_TYPE_DIRECTORY
                | RAMFS_MODE_OWNER_READ
                | RAMFS_MODE_OWNER_WRITE
                | RAMFS_MODE_OWNER_EXEC
        );
    }

    #[test]
    fn unlink_file_and_missing_cases() {
        let mut fs = RamFsBackend::new();
        fs.create_file(b"/ram/remove").expect("create");
        fs.unlink_path(b"/ram/remove").expect("unlink");
        assert_eq!(fs.open_path(b"/ram/remove"), Err(RamFsError::NotFound));
        assert_eq!(fs.unlink_path(b"/ram/missing"), Err(RamFsError::NotFound));
        assert_eq!(fs.unlink_path(b"/ram"), Err(RamFsError::IsDirectory));
    }

    #[test]
    fn create_existing_and_directory_io_rejected() {
        let mut fs = RamFsBackend::new();
        assert_eq!(
            fs.create_file(RAMFS_BOOT_PATH),
            Err(RamFsError::AlreadyExists)
        );
        let fd = fs.open_path(b"/ram").expect("open dir");
        assert_eq!(fs.write_bytes(fd, b"x"), Err(RamFsError::IsDirectory));
        let mut out = [0u8; 1];
        assert_eq!(fs.read_bytes(fd, &mut out), Err(RamFsError::IsDirectory));
    }

    #[test]
    fn capacity_limits_are_enforced() {
        let mut fs = RamFsBackend::with_limits(RamFsLimits {
            max_bytes: 4,
            max_nodes: 4,
        });
        fs.create_file(b"/ram/a").expect("create");
        let fd = fs.open_path(b"/ram/a").expect("open");
        assert_eq!(fs.write_bytes(fd, b"1234"), Ok(4));
        assert_eq!(fs.write_bytes(fd, b"5"), Err(RamFsError::Capacity));
        assert_eq!(fs.create_file(b"/ram/b"), Err(RamFsError::Capacity));
    }

    #[test]
    fn ramfs_inline_write_payload_helper_writes_exact_bytes() {
        let mut fs = RamFsBackend::new();
        fs.create_file(b"/ram/payload").expect("create");
        let fd = fs.open_path(b"/ram/payload").expect("open");
        let request = VfsWriteInlineRequest {
            fd,
            file_offset: 0,
            request_id: 41,
            flags: 0,
            bytes: b"real write payload",
        };
        let message = write_inline_message(request).expect("encode inline payload");
        let payload = VfsWritePayload::decode(message.opcode, message.as_slice()).expect("decode");
        let (decoded_fd, _, _, bytes) = payload.inline_parts().expect("inline bytes");
        assert_eq!(fs.write_bytes(decoded_fd, bytes), Ok(bytes.len()));

        fs.close_fd(fd).expect("close");
        let fd = fs.open_path(b"/ram/payload").expect("reopen");
        let mut out = [0u8; 32];
        let read = fs.read_bytes(fd, &mut out).expect("read back");
        assert_eq!(&out[..read], b"real write payload");
    }

    #[test]
    fn ramfs_oversized_inline_requires_shared_plan_but_mapping_stays_unavailable() {
        let oversized = [0u8; VFS_WRITE_INLINE_MAX_BYTES + 1];
        let inline = VfsWriteInlineRequest {
            fd: 1,
            file_offset: 0,
            request_id: 42,
            flags: 0,
            bytes: &oversized,
        };
        assert_eq!(write_inline_message(inline), Err(VfsError::Malformed));

        let shared = VfsWriteSharedRequest {
            fd: 1,
            file_offset: 0,
            requested_len: oversized.len() as u64,
            request_id: 43,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(
                9,
                2,
                0,
                oversized.len() as u64,
                VFS_SHARED_BUFFER_FS_READ,
            ),
        };
        let message = write_shared_message(shared).expect("shared helper");
        let payload = VfsWritePayload::decode(message.opcode, message.as_slice()).expect("decode");
        assert_eq!(payload.inline_parts(), Err(VfsError::Unsupported));
    }
}
