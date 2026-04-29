// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Canonical VFS core policy/backend types for server-side use.
//! Kernel-facing message wrappers should live outside this crate.

const MAX_FDS: usize = 16;
const MAX_POLICY_RANGES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsError {
    Malformed,
    InvalidPath,
    NameTooLong,
    NoFd,
    BadFd,
    Unsupported,
    PermissionDenied,
    MountConflict,
    MountNotFound,
    MountFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FdEntry {
    fd: u64,
    inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsRequest {
    OpenAt {
        _dirfd: u64,
        /// Legacy pointer-path argument; prefer `path_inline`.
        path_ptr: u64,
        path_inline: Option<PathBytes>,
        _flags: u64,
        _mode: u64,
    },
    Close {
        fd: u64,
    },
    Read {
        fd: u64,
        _buf_ptr: u64,
        len: u64,
    },
    Write {
        fd: u64,
        _buf_ptr: u64,
        len: u64,
    },
    Statx {
        _dirfd: u64,
        /// Legacy pointer-path argument; prefer `path_inline`.
        path_ptr: u64,
        path_inline: Option<PathBytes>,
        _flags: u64,
        _mask_or_buf: u64,
    },
    Ioctl {
        fd: u64,
        request: u64,
        arg: u64,
    },
    Dup {
        fd: u64,
    },
    Fcntl {
        fd: u64,
        cmd: u64,
        arg: u64,
    },
    Poll {
        fds_ptr: u64,
        nfds: u64,
        timeout: u64,
    },
    EpollCreate1 {
        flags: u64,
    },
    EpollCtl {
        epfd: u64,
        op: u64,
        fd: u64,
        event_ptr: u64,
    },
    EpollPwait {
        epfd: u64,
        events_ptr: u64,
        maxevents: u64,
        timeout: u64,
    },
    Sendfile {
        out_fd: u64,
        in_fd: u64,
        offset_ptr: u64,
        count: u64,
    },
}

pub trait VfsBackend {
    fn openat_path(&mut self, _path: &[u8]) -> Result<u64, VfsError> {
        Err(VfsError::InvalidPath)
    }
    fn close(&mut self, fd: u64) -> Result<u64, VfsError>;
    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsError>;
    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsError>;
    fn statx_path(&mut self, _path: &[u8]) -> Result<u64, VfsError> {
        Err(VfsError::InvalidPath)
    }
    fn ioctl(&mut self, _fd: u64, _request: u64, _arg: u64) -> Result<u64, VfsError> {
        Err(VfsError::Unsupported)
    }
    fn dup(&mut self, _fd: u64) -> Result<u64, VfsError> {
        Err(VfsError::Unsupported)
    }
    fn fcntl(&mut self, _fd: u64, _cmd: u64, _arg: u64) -> Result<u64, VfsError> {
        Err(VfsError::Unsupported)
    }
    fn poll(&mut self, _fds_ptr: u64, _nfds: u64, _timeout: u64) -> Result<u64, VfsError> {
        Err(VfsError::Unsupported)
    }
    fn epoll_create1(&mut self, _flags: u64) -> Result<u64, VfsError> {
        Err(VfsError::Unsupported)
    }
    fn epoll_ctl(
        &mut self,
        _epfd: u64,
        _op: u64,
        _fd: u64,
        _event_ptr: u64,
    ) -> Result<u64, VfsError> {
        Err(VfsError::Unsupported)
    }
    fn epoll_pwait(
        &mut self,
        _epfd: u64,
        _events_ptr: u64,
        _maxevents: u64,
        _timeout: u64,
    ) -> Result<u64, VfsError> {
        Err(VfsError::Unsupported)
    }
    fn sendfile(
        &mut self,
        _out_fd: u64,
        _in_fd: u64,
        _offset_ptr: u64,
        _count: u64,
    ) -> Result<u64, VfsError> {
        Err(VfsError::Unsupported)
    }
}

#[derive(Debug)]
pub struct InMemoryBackend {
    next_fd: u64,
    fds: [Option<FdEntry>; MAX_FDS],
}

impl Default for InMemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryBackend {
    pub const fn new() -> Self {
        Self {
            next_fd: 3,
            fds: [None; MAX_FDS],
        }
    }

    fn alloc_fd(&mut self, inode: u64) -> Result<u64, VfsError> {
        let fd = self.next_fd;
        self.next_fd = self.next_fd.saturating_add(1);
        if let Some(slot) = self.fds.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(FdEntry { fd, inode });
            Ok(fd)
        } else {
            Err(VfsError::NoFd)
        }
    }

    fn has_fd(&self, fd: u64) -> bool {
        self.fds.iter().flatten().any(|entry| entry.fd == fd)
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

    fn inode_for_fd(&self, fd: u64) -> Result<u64, VfsError> {
        self.fds
            .iter()
            .flatten()
            .find(|entry| entry.fd == fd)
            .map(|entry| entry.inode)
            .ok_or(VfsError::BadFd)
    }
}

#[derive(Debug)]
pub struct MountRouter<A: VfsBackend, B: VfsBackend> {
    split_at: u64,
    low: A,
    high: B,
}

impl<A: VfsBackend, B: VfsBackend> MountRouter<A, B> {
    pub const fn new(split_at: u64, low: A, high: B) -> Self {
        Self {
            split_at,
            low,
            high,
        }
    }

    /// Primary router for OPENAT/STATX runtime traffic.
    fn route_by_path_bytes(&mut self, path: &[u8]) -> &mut dyn VfsBackend {
        if path.starts_with(b"/initramfs/") {
            &mut self.high
        } else {
            &mut self.low
        }
    }

    fn route_by_fd(&mut self, fd: u64) -> &mut dyn VfsBackend {
        if fd < self.split_at {
            &mut self.low
        } else {
            &mut self.high
        }
    }
}

impl<A: VfsBackend, B: VfsBackend> VfsBackend for MountRouter<A, B> {

    fn openat_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        self.route_by_path_bytes(path).openat_path(path)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsError> {
        self.route_by_fd(fd).close(fd)
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        self.route_by_fd(fd).read(fd, len)
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        self.route_by_fd(fd).write(fd, len)
    }


    fn statx_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        self.route_by_path_bytes(path).statx_path(path)
    }

    fn ioctl(&mut self, fd: u64, request: u64, arg: u64) -> Result<u64, VfsError> {
        self.route_by_fd(fd).ioctl(fd, request, arg)
    }

    fn dup(&mut self, fd: u64) -> Result<u64, VfsError> {
        self.route_by_fd(fd).dup(fd)
    }

    fn fcntl(&mut self, fd: u64, cmd: u64, arg: u64) -> Result<u64, VfsError> {
        self.route_by_fd(fd).fcntl(fd, cmd, arg)
    }

    fn poll(&mut self, fds_ptr: u64, nfds: u64, timeout: u64) -> Result<u64, VfsError> {
        self.low.poll(fds_ptr, nfds, timeout)
    }

    fn epoll_create1(&mut self, flags: u64) -> Result<u64, VfsError> {
        self.low.epoll_create1(flags)
    }

    fn epoll_ctl(&mut self, epfd: u64, op: u64, fd: u64, event_ptr: u64) -> Result<u64, VfsError> {
        self.route_by_fd(epfd).epoll_ctl(epfd, op, fd, event_ptr)
    }

    fn epoll_pwait(
        &mut self,
        epfd: u64,
        events_ptr: u64,
        maxevents: u64,
        timeout: u64,
    ) -> Result<u64, VfsError> {
        self.route_by_fd(epfd)
            .epoll_pwait(epfd, events_ptr, maxevents, timeout)
    }

    fn sendfile(
        &mut self,
        out_fd: u64,
        in_fd: u64,
        offset_ptr: u64,
        count: u64,
    ) -> Result<u64, VfsError> {
        self.route_by_fd(out_fd)
            .sendfile(out_fd, in_fd, offset_ptr, count)
    }
}

impl VfsBackend for InMemoryBackend {

    fn openat_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        if path.is_empty() {
            return Err(VfsError::InvalidPath);
        }
        let mut inode = 0u64;
        for &byte in path {
            inode = inode.wrapping_mul(131).wrapping_add(byte as u64);
        }
        self.alloc_fd(inode)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsError> {
        self.close_fd(fd)?;
        Ok(0)
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        if !self.has_fd(fd) {
            return Err(VfsError::BadFd);
        }
        Ok(len)
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        if !self.has_fd(fd) {
            return Err(VfsError::BadFd);
        }
        Ok(len)
    }


    fn statx_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        if path.is_empty() {
            return Err(VfsError::InvalidPath);
        }
        let mut stat = 0u64;
        for &byte in path {
            stat = stat.wrapping_mul(167).wrapping_add(byte as u64);
        }
        Ok(stat)
    }

    fn ioctl(&mut self, fd: u64, request: u64, arg: u64) -> Result<u64, VfsError> {
        if !self.has_fd(fd) {
            return Err(VfsError::BadFd);
        }
        Ok(request ^ arg)
    }

    fn dup(&mut self, fd: u64) -> Result<u64, VfsError> {
        let inode = self.inode_for_fd(fd)?;
        self.alloc_fd(inode)
    }

    fn fcntl(&mut self, fd: u64, cmd: u64, arg: u64) -> Result<u64, VfsError> {
        if !self.has_fd(fd) {
            return Err(VfsError::BadFd);
        }
        Ok(cmd.saturating_add(arg))
    }

    fn poll(&mut self, _fds_ptr: u64, nfds: u64, _timeout: u64) -> Result<u64, VfsError> {
        Ok(u64::from(nfds > 0))
    }

    fn epoll_create1(&mut self, flags: u64) -> Result<u64, VfsError> {
        self.alloc_fd(0xE000 | flags)
    }

    fn epoll_ctl(
        &mut self,
        epfd: u64,
        _op: u64,
        fd: u64,
        _event_ptr: u64,
    ) -> Result<u64, VfsError> {
        if !self.has_fd(epfd) || !self.has_fd(fd) {
            return Err(VfsError::BadFd);
        }
        Ok(0)
    }

    fn epoll_pwait(
        &mut self,
        epfd: u64,
        _events_ptr: u64,
        maxevents: u64,
        _timeout: u64,
    ) -> Result<u64, VfsError> {
        if !self.has_fd(epfd) {
            return Err(VfsError::BadFd);
        }
        Ok(maxevents.min(1))
    }

    fn sendfile(
        &mut self,
        out_fd: u64,
        in_fd: u64,
        _offset_ptr: u64,
        count: u64,
    ) -> Result<u64, VfsError> {
        if !self.has_fd(out_fd) || !self.has_fd(in_fd) {
            return Err(VfsError::BadFd);
        }
        Ok(count)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    File,
    Directory,
    CharDevice,
    BlockDevice,
    Symlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VNodeId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFlags {
    pub read: bool,
    pub write: bool,
    pub create: bool,
}

impl OpenFlags {
    pub const fn rdonly() -> Self {
        Self {
            read: true,
            write: false,
            create: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LookupRequest {
    pub dir: VNodeId,
    pub path_ptr: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadRequest {
    pub fd: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub node: VNodeId,
    pub kind: FileType,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAtRequest {
    pub dirfd: u64,
    /// Legacy pointer-path argument; prefer inline byte-path requests.
    pub path_ptr: u64,
    pub flags: u64,
    pub mode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseRequest {
    pub fd: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadWriteRequest {
    pub fd: u64,
    pub buf_ptr: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatxRequest {
    pub dirfd: u64,
    /// Legacy pointer-path argument; prefer inline byte-path requests.
    pub path_ptr: u64,
    pub flags: u64,
    pub mask_or_buf: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathPolicyRange {
    pub start: u64,
    pub end: u64,
}

impl PathPolicyRange {
    pub const fn contains(self, path_ptr: u64) -> bool {
        path_ptr >= self.start && path_ptr <= self.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountNamespacePolicy {
    allow_all: bool,
    ranges: [Option<PathPolicyRange>; MAX_POLICY_RANGES],
}

impl MountNamespacePolicy {
    pub const fn baseline() -> Self {
        Self {
            allow_all: true,
            ranges: [None; MAX_POLICY_RANGES],
        }
    }

    pub const fn deny_all() -> Self {
        Self {
            allow_all: false,
            ranges: [None; MAX_POLICY_RANGES],
        }
    }

    pub const fn boot_profile() -> Self {
        Self {
            allow_all: false,
            ranges: [
                Some(PathPolicyRange {
                    start: 0x1000,
                    end: 0x1FFF,
                }),
                Some(PathPolicyRange {
                    start: 0x4000,
                    end: 0x4FFF,
                }),
                Some(PathPolicyRange {
                    start: 0xA000,
                    end: 0xAFFF,
                }),
                None,
            ],
        }
    }

    pub const fn with_range(mut self, start: u64, end: u64) -> Self {
        let mut idx = 0;
        while idx < MAX_POLICY_RANGES {
            if self.ranges[idx].is_none() {
                self.ranges[idx] = Some(PathPolicyRange { start, end });
                break;
            }
            idx += 1;
        }
        self
    }

    /// Legacy numeric-path policy check kept for compatibility-only pointer ABI callers.
    pub const fn allows_path(self, path_ptr: u64) -> bool {
        if self.allow_all {
            return true;
        }
        let mut idx = 0;
        while idx < MAX_POLICY_RANGES {
            if let Some(range) = self.ranges[idx] {
                if range.contains(path_ptr) {
                    return true;
                }
            }
            idx += 1;
        }
        false
    }

    /// Primary policy check for OPENAT/STATX runtime traffic.
    pub fn allows_path_bytes(self, path: &[u8]) -> bool {
        if self.allow_all {
            return true;
        }
        if path.is_empty() || !path.starts_with(b"/") {
            return false;
        }
        if path.starts_with(b"/initramfs/") || path.starts_with(b"/dev/") || path.starts_with(b"/ramfs/") {
            return true;
        }
        path == b"/etc/hosts"
    }
}

pub const INLINE_PATH_MAX: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathBytes {
    len: u8,
    bytes: [u8; INLINE_PATH_MAX],
}

impl PathBytes {
    pub fn from_slice(path: &[u8]) -> Result<Self, VfsError> {
        if path.is_empty() {
            return Err(VfsError::InvalidPath);
        }
        if path.len() > INLINE_PATH_MAX {
            return Err(VfsError::NameTooLong);
        }
        let mut bytes = [0u8; INLINE_PATH_MAX];
        bytes[..path.len()].copy_from_slice(path);
        Ok(Self {
            len: path.len() as u8,
            bytes,
        })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountRecord {
    pub mountpoint_ptr: u64,
    pub fs_tag: u64,
    pub active: bool,
    pub failed: bool,
}
