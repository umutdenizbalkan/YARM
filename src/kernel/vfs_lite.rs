use super::ipc::Message;
use super::vfs_proto::{
    VFS_OP_CLOSE, VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_STATX, VFS_OP_WRITE, VfsV1Args,
};

const MAX_FDS: usize = 16;
const MAX_RAMFS_INODES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsLiteError {
    Malformed,
    NoFd,
    BadFd,
    Unsupported,
    PermissionDenied,
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
        path_ptr: u64,
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
        path_ptr: u64,
        _flags: u64,
        _mask_or_buf: u64,
    },
}

pub trait VfsBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError>;
    fn close(&mut self, fd: u64) -> Result<u64, VfsLiteError>;
    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError>;
    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError>;
    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError>;
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

    fn alloc_fd(&mut self, inode: u64) -> Result<u64, VfsLiteError> {
        let fd = self.next_fd;
        self.next_fd = self.next_fd.saturating_add(1);
        if let Some(slot) = self.fds.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(FdEntry { fd, inode });
            Ok(fd)
        } else {
            Err(VfsLiteError::NoFd)
        }
    }

    fn has_fd(&self, fd: u64) -> bool {
        self.fds.iter().flatten().any(|entry| entry.fd == fd)
    }

    fn close_fd(&mut self, fd: u64) -> Result<(), VfsLiteError> {
        if let Some(slot) = self
            .fds
            .iter_mut()
            .find(|slot| slot.map(|entry| entry.fd == fd).unwrap_or(false))
        {
            *slot = None;
            Ok(())
        } else {
            Err(VfsLiteError::BadFd)
        }
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

    fn route_by_path(&mut self, path_ptr: u64) -> &mut dyn VfsBackend {
        if path_ptr < self.split_at {
            &mut self.low
        } else {
            &mut self.high
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
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        self.route_by_path(path_ptr).openat(path_ptr)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsLiteError> {
        self.route_by_fd(fd).close(fd)
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        self.route_by_fd(fd).read(fd, len)
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        self.route_by_fd(fd).write(fd, len)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        self.route_by_path(path_ptr).statx(path_ptr)
    }
}

impl VfsBackend for InMemoryBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        self.alloc_fd(path_ptr)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsLiteError> {
        self.close_fd(fd)?;
        Ok(0)
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        if !self.has_fd(fd) {
            return Err(VfsLiteError::BadFd);
        }
        Ok(len)
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        if !self.has_fd(fd) {
            return Err(VfsLiteError::BadFd);
        }
        Ok(len)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        Ok(path_ptr)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RamFsInode {
    path_ptr: u64,
    file_len: u64,
}

#[derive(Debug)]
pub struct RamFsBackend {
    next_fd: u64,
    fds: [Option<FdEntry>; MAX_FDS],
    inodes: [Option<RamFsInode>; MAX_RAMFS_INODES],
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
            fds: [None; MAX_FDS],
            inodes: [None; MAX_RAMFS_INODES],
        }
    }

    fn alloc_fd(&mut self, inode: u64) -> Result<u64, VfsLiteError> {
        let fd = self.next_fd;
        self.next_fd = self.next_fd.saturating_add(1);
        if let Some(slot) = self.fds.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(FdEntry { fd, inode });
            Ok(fd)
        } else {
            Err(VfsLiteError::NoFd)
        }
    }

    fn open_inode(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
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
            *slot = Some(RamFsInode {
                path_ptr,
                file_len: 0,
            });
            return self.alloc_fd(path_ptr);
        }
        Err(VfsLiteError::NoFd)
    }

    fn close_fd(&mut self, fd: u64) -> Result<(), VfsLiteError> {
        if let Some(slot) = self
            .fds
            .iter_mut()
            .find(|slot| slot.map(|entry| entry.fd == fd).unwrap_or(false))
        {
            *slot = None;
            Ok(())
        } else {
            Err(VfsLiteError::BadFd)
        }
    }

    fn inode_for_fd(&self, fd: u64) -> Option<u64> {
        self.fds
            .iter()
            .flatten()
            .find(|entry| entry.fd == fd)
            .map(|entry| entry.inode)
    }

    fn inode_index_for_path(&self, path_ptr: u64) -> Option<usize> {
        self.inodes.iter().position(|slot| {
            slot.map(|inode| inode.path_ptr == path_ptr)
                .unwrap_or(false)
        })
    }
}

impl VfsBackend for RamFsBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        self.open_inode(path_ptr)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsLiteError> {
        self.close_fd(fd)?;
        Ok(0)
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        let inode = self.inode_for_fd(fd).ok_or(VfsLiteError::BadFd)?;
        let idx = self
            .inode_index_for_path(inode)
            .ok_or(VfsLiteError::BadFd)?;
        let file_len = self.inodes[idx].ok_or(VfsLiteError::BadFd)?.file_len;
        Ok(core::cmp::min(len, file_len))
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        let inode = self.inode_for_fd(fd).ok_or(VfsLiteError::BadFd)?;
        let idx = self
            .inode_index_for_path(inode)
            .ok_or(VfsLiteError::BadFd)?;
        let Some(mut inode_slot) = self.inodes[idx] else {
            return Err(VfsLiteError::BadFd);
        };
        inode_slot.file_len = inode_slot.file_len.saturating_add(len);
        self.inodes[idx] = Some(inode_slot);
        Ok(len)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        let idx = self
            .inode_index_for_path(path_ptr)
            .ok_or(VfsLiteError::BadFd)?;
        Ok(self.inodes[idx].ok_or(VfsLiteError::BadFd)?.file_len)
    }
}

#[derive(Debug)]
pub struct Ext4Backend {
    next_fd: u64,
    fds: [Option<FdEntry>; MAX_FDS],
    inodes: [Option<RamFsInode>; MAX_RAMFS_INODES],
    max_file_len: u64,
    journal_seq: u64,
}

impl Default for Ext4Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl Ext4Backend {
    pub const fn new() -> Self {
        Self {
            next_fd: 200,
            fds: [None; MAX_FDS],
            inodes: [None; MAX_RAMFS_INODES],
            max_file_len: 16 * 1024 * 1024,
            journal_seq: 0,
        }
    }

    pub const fn journal_seq(&self) -> u64 {
        self.journal_seq
    }

    fn alloc_fd(&mut self, inode: u64) -> Result<u64, VfsLiteError> {
        let fd = self.next_fd;
        self.next_fd = self.next_fd.saturating_add(1);
        if let Some(slot) = self.fds.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(FdEntry { fd, inode });
            Ok(fd)
        } else {
            Err(VfsLiteError::NoFd)
        }
    }

    fn open_inode(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
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
            *slot = Some(RamFsInode {
                path_ptr,
                file_len: 0,
            });
            return self.alloc_fd(path_ptr);
        }
        Err(VfsLiteError::NoFd)
    }

    fn close_fd(&mut self, fd: u64) -> Result<(), VfsLiteError> {
        if let Some(slot) = self
            .fds
            .iter_mut()
            .find(|slot| slot.map(|entry| entry.fd == fd).unwrap_or(false))
        {
            *slot = None;
            Ok(())
        } else {
            Err(VfsLiteError::BadFd)
        }
    }

    fn inode_for_fd(&self, fd: u64) -> Option<u64> {
        self.fds
            .iter()
            .flatten()
            .find(|entry| entry.fd == fd)
            .map(|entry| entry.inode)
    }

    fn inode_index_for_path(&self, path_ptr: u64) -> Option<usize> {
        self.inodes.iter().position(|slot| {
            slot.map(|inode| inode.path_ptr == path_ptr)
                .unwrap_or(false)
        })
    }
}

impl VfsBackend for Ext4Backend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        self.open_inode(path_ptr)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsLiteError> {
        self.close_fd(fd)?;
        Ok(0)
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        let inode = self.inode_for_fd(fd).ok_or(VfsLiteError::BadFd)?;
        let idx = self
            .inode_index_for_path(inode)
            .ok_or(VfsLiteError::BadFd)?;
        let file_len = self.inodes[idx].ok_or(VfsLiteError::BadFd)?.file_len;
        Ok(core::cmp::min(len, file_len))
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        let inode = self.inode_for_fd(fd).ok_or(VfsLiteError::BadFd)?;
        let idx = self
            .inode_index_for_path(inode)
            .ok_or(VfsLiteError::BadFd)?;
        let Some(mut inode_slot) = self.inodes[idx] else {
            return Err(VfsLiteError::BadFd);
        };
        let Some(new_len) = inode_slot.file_len.checked_add(len) else {
            return Err(VfsLiteError::Unsupported);
        };
        if new_len > self.max_file_len {
            return Err(VfsLiteError::Unsupported);
        }
        inode_slot.file_len = new_len;
        self.inodes[idx] = Some(inode_slot);
        self.journal_seq = self.journal_seq.saturating_add(1);
        Ok(len)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        let idx = self
            .inode_index_for_path(path_ptr)
            .ok_or(VfsLiteError::BadFd)?;
        Ok(self.inodes[idx].ok_or(VfsLiteError::BadFd)?.file_len)
    }
}

pub const DEV_CONSOLE_PATH_PTR: u64 = 0x434F_4E53_4F4C_4500;
pub const INITRAMFS_BUSYBOX_PATH_PTR: u64 = 0x494E_4954_4255_5359;
pub const DEV_NULL_PATH_PTR: u64 = 0x4445_564E_554C_4C00;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootPath {
    DevConsole,
    Busybox,
    DevNull,
}

pub const fn boot_path_ptr(path: BootPath) -> u64 {
    match path {
        BootPath::DevConsole => DEV_CONSOLE_PATH_PTR,
        BootPath::Busybox => INITRAMFS_BUSYBOX_PATH_PTR,
        BootPath::DevNull => DEV_NULL_PATH_PTR,
    }
}

pub const fn resolve_boot_path(path_ptr: u64) -> Option<BootPath> {
    if path_ptr == DEV_CONSOLE_PATH_PTR {
        Some(BootPath::DevConsole)
    } else if path_ptr == INITRAMFS_BUSYBOX_PATH_PTR {
        Some(BootPath::Busybox)
    } else if path_ptr == DEV_NULL_PATH_PTR {
        Some(BootPath::DevNull)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOnlyInitramfsBackend {
    opened_fd: Option<u64>,
    file_len: u64,
}

impl Default for ReadOnlyInitramfsBackend {
    fn default() -> Self {
        Self::new(4096)
    }
}

impl ReadOnlyInitramfsBackend {
    pub const fn new(file_len: u64) -> Self {
        Self {
            opened_fd: None,
            file_len,
        }
    }
}

impl VfsBackend for ReadOnlyInitramfsBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        if resolve_boot_path(path_ptr) != Some(BootPath::Busybox) {
            return Err(VfsLiteError::BadFd);
        }
        self.opened_fd = Some(10);
        Ok(10)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsLiteError> {
        if self.opened_fd == Some(fd) {
            self.opened_fd = None;
            Ok(0)
        } else {
            Err(VfsLiteError::BadFd)
        }
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        if self.opened_fd != Some(fd) {
            return Err(VfsLiteError::BadFd);
        }
        Ok(core::cmp::min(len, self.file_len))
    }

    fn write(&mut self, fd: u64, _len: u64) -> Result<u64, VfsLiteError> {
        if self.opened_fd != Some(fd) {
            return Err(VfsLiteError::BadFd);
        }
        Err(VfsLiteError::Unsupported)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        if resolve_boot_path(path_ptr) == Some(BootPath::Busybox) {
            Ok(self.file_len)
        } else {
            Err(VfsLiteError::BadFd)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConsoleBackend {
    open_console_fd: Option<u64>,
}

impl VfsBackend for ConsoleBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        if resolve_boot_path(path_ptr) != Some(BootPath::DevConsole) {
            return Err(VfsLiteError::BadFd);
        }
        self.open_console_fd = Some(3);
        Ok(3)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsLiteError> {
        if self.open_console_fd == Some(fd) {
            self.open_console_fd = None;
            Ok(0)
        } else {
            Err(VfsLiteError::BadFd)
        }
    }

    fn read(&mut self, fd: u64, _len: u64) -> Result<u64, VfsLiteError> {
        if self.open_console_fd != Some(fd) {
            return Err(VfsLiteError::BadFd);
        }
        Ok(0)
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        if self.open_console_fd != Some(fd) {
            return Err(VfsLiteError::BadFd);
        }
        Ok(len)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        if resolve_boot_path(path_ptr) == Some(BootPath::DevConsole) {
            Ok(0)
        } else {
            Err(VfsLiteError::BadFd)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DevFsBackend {
    open_console_fd: Option<u64>,
    open_null_fd: Option<u64>,
}

impl VfsBackend for DevFsBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        match resolve_boot_path(path_ptr) {
            Some(BootPath::DevConsole) => {
                self.open_console_fd = Some(3);
                Ok(3)
            }
            Some(BootPath::DevNull) => {
                self.open_null_fd = Some(4);
                Ok(4)
            }
            _ => Err(VfsLiteError::BadFd),
        }
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsLiteError> {
        if self.open_console_fd == Some(fd) {
            self.open_console_fd = None;
            return Ok(0);
        }
        if self.open_null_fd == Some(fd) {
            self.open_null_fd = None;
            return Ok(0);
        }
        Err(VfsLiteError::BadFd)
    }

    fn read(&mut self, fd: u64, _len: u64) -> Result<u64, VfsLiteError> {
        if self.open_console_fd == Some(fd) || self.open_null_fd == Some(fd) {
            return Ok(0);
        }
        Err(VfsLiteError::BadFd)
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        if self.open_console_fd == Some(fd) || self.open_null_fd == Some(fd) {
            return Ok(len);
        }
        Err(VfsLiteError::BadFd)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        match resolve_boot_path(path_ptr) {
            Some(BootPath::DevConsole) | Some(BootPath::DevNull) => Ok(0),
            _ => Err(VfsLiteError::BadFd),
        }
    }
}

#[derive(Debug)]
pub struct RamFsService {
    inner: VfsLiteService<RamFsBackend>,
    handled: usize,
}

impl Default for RamFsService {
    fn default() -> Self {
        Self::new()
    }
}

impl RamFsService {
    pub const fn new() -> Self {
        Self {
            inner: VfsLiteService::with_backend(RamFsBackend::new()),
            handled: 0,
        }
    }

    pub const fn handled_count(&self) -> usize {
        self.handled
    }

    pub fn handle(&mut self, request: Message) -> Result<Message, VfsLiteError> {
        let reply = self.inner.handle_request(request)?;
        self.handled = self.handled.saturating_add(1);
        Ok(reply)
    }
}

#[derive(Debug)]
pub struct InitramfsService {
    inner: VfsLiteService<ReadOnlyInitramfsBackend>,
    handled: usize,
}

impl InitramfsService {
    pub const fn new(file_len: u64) -> Self {
        Self {
            inner: VfsLiteService::with_backend(ReadOnlyInitramfsBackend::new(file_len)),
            handled: 0,
        }
    }

    pub const fn handled_count(&self) -> usize {
        self.handled
    }

    pub fn handle(&mut self, request: Message) -> Result<Message, VfsLiteError> {
        let reply = self.inner.handle_request(request)?;
        self.handled = self.handled.saturating_add(1);
        Ok(reply)
    }
}

#[derive(Debug)]
pub struct Ext4Service {
    inner: VfsLiteService<Ext4Backend>,
    handled: usize,
}

impl Default for Ext4Service {
    fn default() -> Self {
        Self::new()
    }
}

impl Ext4Service {
    pub const fn new() -> Self {
        Self {
            inner: VfsLiteService::with_backend(Ext4Backend::new()),
            handled: 0,
        }
    }

    pub const fn handled_count(&self) -> usize {
        self.handled
    }

    pub fn handle(&mut self, request: Message) -> Result<Message, VfsLiteError> {
        let reply = self.inner.handle_request(request)?;
        self.handled = self.handled.saturating_add(1);
        Ok(reply)
    }
}

#[derive(Debug)]
pub struct DevFsService {
    inner: VfsLiteService<DevFsBackend>,
    handled: usize,
}

impl Default for DevFsService {
    fn default() -> Self {
        Self::new()
    }
}

impl DevFsService {
    pub const fn new() -> Self {
        Self {
            inner: VfsLiteService::with_backend(DevFsBackend {
                open_console_fd: None,
                open_null_fd: None,
            }),
            handled: 0,
        }
    }

    pub const fn handled_count(&self) -> usize {
        self.handled
    }

    pub fn handle(&mut self, request: Message) -> Result<Message, VfsLiteError> {
        let reply = self.inner.handle_request(request)?;
        self.handled = self.handled.saturating_add(1);
        Ok(reply)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountNamespacePolicy {
    pub allow_console: bool,
    pub allow_initramfs: bool,
}

impl MountNamespacePolicy {
    pub const fn baseline() -> Self {
        Self {
            allow_console: true,
            allow_initramfs: true,
        }
    }

    pub const fn allows_path(self, path_ptr: u64) -> bool {
        match resolve_boot_path(path_ptr) {
            Some(BootPath::DevConsole) => self.allow_console,
            Some(BootPath::DevNull) => self.allow_console,
            Some(BootPath::Busybox) => self.allow_initramfs,
            None => true,
        }
    }
}

#[derive(Debug)]
pub struct VfsLiteService<B: VfsBackend = InMemoryBackend> {
    backend: B,
    policy: MountNamespacePolicy,
    op_sequence: u64,
}

impl Default for VfsLiteService<InMemoryBackend> {
    fn default() -> Self {
        Self::new()
    }
}

impl VfsLiteService<InMemoryBackend> {
    pub const fn new() -> Self {
        Self {
            backend: InMemoryBackend::new(),
            policy: MountNamespacePolicy::baseline(),
            op_sequence: 0,
        }
    }
}

impl<B: VfsBackend> VfsLiteService<B> {
    pub const fn with_backend(backend: B) -> Self {
        Self {
            backend,
            policy: MountNamespacePolicy::baseline(),
            op_sequence: 0,
        }
    }

    pub fn set_policy(&mut self, policy: MountNamespacePolicy) {
        self.policy = policy;
    }

    pub const fn op_sequence(&self) -> u64 {
        self.op_sequence
    }

    fn u64_reply(opcode: u16, value: u64) -> Result<Message, VfsLiteError> {
        Message::with_header(0, opcode, 0, None, &value.to_le_bytes())
            .map_err(|_| VfsLiteError::Malformed)
    }

    pub fn parse_request(request: Message) -> Result<VfsRequest, VfsLiteError> {
        let args = VfsV1Args::decode(request.as_slice()).map_err(|_| VfsLiteError::Malformed)?;
        match request.opcode {
            VFS_OP_OPENAT => Ok(VfsRequest::OpenAt {
                _dirfd: args.arg0,
                path_ptr: args.arg1,
                _flags: args.arg2,
                _mode: args.arg3,
            }),
            VFS_OP_CLOSE => Ok(VfsRequest::Close { fd: args.arg0 }),
            VFS_OP_READ => Ok(VfsRequest::Read {
                fd: args.arg0,
                _buf_ptr: args.arg1,
                len: args.arg2,
            }),
            VFS_OP_WRITE => Ok(VfsRequest::Write {
                fd: args.arg0,
                _buf_ptr: args.arg1,
                len: args.arg2,
            }),
            VFS_OP_STATX => Ok(VfsRequest::Statx {
                _dirfd: args.arg0,
                path_ptr: args.arg1,
                _flags: args.arg2,
                _mask_or_buf: args.arg3,
            }),
            _ => Err(VfsLiteError::Unsupported),
        }
    }

    pub fn handle_request(&mut self, request: Message) -> Result<Message, VfsLiteError> {
        let parsed = Self::parse_request(request)?;
        let reply = match parsed {
            VfsRequest::OpenAt { path_ptr, .. } => {
                if !self.policy.allows_path(path_ptr) {
                    return Err(VfsLiteError::PermissionDenied);
                }
                Self::u64_reply(VFS_OP_OPENAT, self.backend.openat(path_ptr)?)
            }
            VfsRequest::Close { fd } => Self::u64_reply(VFS_OP_CLOSE, self.backend.close(fd)?),
            VfsRequest::Read { fd, len, .. } => {
                Self::u64_reply(VFS_OP_READ, self.backend.read(fd, len)?)
            }
            VfsRequest::Write { fd, len, .. } => {
                Self::u64_reply(VFS_OP_WRITE, self.backend.write(fd, len)?)
            }
            VfsRequest::Statx { path_ptr, .. } => {
                if !self.policy.allows_path(path_ptr) {
                    return Err(VfsLiteError::PermissionDenied);
                }
                Self::u64_reply(VFS_OP_STATX, self.backend.statx(path_ptr)?)
            }
        }?;
        self.op_sequence = self.op_sequence.saturating_add(1);
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::vfs_proto::{
        VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_STATX, VFS_OP_WRITE, VfsV1Args,
    };

    fn pack(a0: u64, a1: u64, a2: u64, a3: u64) -> [u8; 32] {
        VfsV1Args::new(a0, a1, a2, a3).encode()
    }

    #[test]
    fn boot_path_resolution_is_stable() {
        assert_eq!(
            resolve_boot_path(boot_path_ptr(BootPath::DevConsole)),
            Some(BootPath::DevConsole)
        );
        assert_eq!(
            resolve_boot_path(boot_path_ptr(BootPath::Busybox)),
            Some(BootPath::Busybox)
        );
        assert_eq!(
            resolve_boot_path(boot_path_ptr(BootPath::DevNull)),
            Some(BootPath::DevNull)
        );
        assert_eq!(resolve_boot_path(0xDEAD), None);
    }

    #[test]
    fn ramfs_service_supports_write_then_stat() {
        let mut svc = RamFsService::new();
        let open =
            Message::with_header(0, VFS_OP_OPENAT, 0, None, &pack(0, 0x1010, 0, 0)).expect("open");
        let open_rep = svc.handle(open).expect("open rep");
        let mut fd_bytes = [0u8; 8];
        fd_bytes.copy_from_slice(open_rep.as_slice());
        let fd = u64::from_le_bytes(fd_bytes);

        let write =
            Message::with_header(0, VFS_OP_WRITE, 0, None, &pack(fd, 0, 128, 0)).expect("write");
        let _ = svc.handle(write).expect("write rep");

        let stat =
            Message::with_header(0, VFS_OP_STATX, 0, None, &pack(0, 0x1010, 0, 0)).expect("stat");
        let stat_rep = svc.handle(stat).expect("stat rep");
        let mut len_bytes = [0u8; 8];
        len_bytes.copy_from_slice(stat_rep.as_slice());
        assert_eq!(u64::from_le_bytes(len_bytes), 128);
        assert_eq!(svc.handled_count(), 3);
    }

    #[test]
    fn initramfs_service_is_read_only() {
        let mut svc = InitramfsService::new(4096);
        let open = Message::with_header(
            0,
            VFS_OP_OPENAT,
            0,
            None,
            &pack(0, INITRAMFS_BUSYBOX_PATH_PTR, 0, 0),
        )
        .expect("open");
        let open_rep = svc.handle(open).expect("open rep");
        let mut fd_bytes = [0u8; 8];
        fd_bytes.copy_from_slice(open_rep.as_slice());
        let fd = u64::from_le_bytes(fd_bytes);

        let write =
            Message::with_header(0, VFS_OP_WRITE, 0, None, &pack(fd, 0, 1, 0)).expect("write");
        assert_eq!(svc.handle(write), Err(VfsLiteError::Unsupported));
    }

    #[test]
    fn devfs_service_supports_console_and_null() {
        let mut svc = DevFsService::new();
        let open_console = Message::with_header(
            0,
            VFS_OP_OPENAT,
            0,
            None,
            &pack(0, DEV_CONSOLE_PATH_PTR, 0, 0),
        )
        .expect("open console");
        let open_null =
            Message::with_header(0, VFS_OP_OPENAT, 0, None, &pack(0, DEV_NULL_PATH_PTR, 0, 0))
                .expect("open null");

        let console_rep = svc.handle(open_console).expect("console rep");
        let null_rep = svc.handle(open_null).expect("null rep");

        let mut fd_bytes = [0u8; 8];
        fd_bytes.copy_from_slice(console_rep.as_slice());
        let console_fd = u64::from_le_bytes(fd_bytes);

        fd_bytes.copy_from_slice(null_rep.as_slice());
        let null_fd = u64::from_le_bytes(fd_bytes);

        let write_console =
            Message::with_header(0, VFS_OP_WRITE, 0, None, &pack(console_fd, 0, 7, 0))
                .expect("write console");
        let write_null = Message::with_header(0, VFS_OP_WRITE, 0, None, &pack(null_fd, 0, 11, 0))
            .expect("write null");

        let _ = svc.handle(write_console).expect("write console rep");
        let _ = svc.handle(write_null).expect("write null rep");
        assert_eq!(svc.handled_count(), 4);
    }

    #[test]
    fn ext4_service_supports_write_stat_and_journal() {
        let mut svc = Ext4Service::new();
        let open =
            Message::with_header(0, VFS_OP_OPENAT, 0, None, &pack(0, 0x2020, 0, 0)).expect("open");
        let open_rep = svc.handle(open).expect("open rep");
        let mut fd_bytes = [0u8; 8];
        fd_bytes.copy_from_slice(open_rep.as_slice());
        let fd = u64::from_le_bytes(fd_bytes);

        let write =
            Message::with_header(0, VFS_OP_WRITE, 0, None, &pack(fd, 0, 4096, 0)).expect("write");
        let _ = svc.handle(write).expect("write rep");

        let stat =
            Message::with_header(0, VFS_OP_STATX, 0, None, &pack(0, 0x2020, 0, 0)).expect("stat");
        let stat_rep = svc.handle(stat).expect("stat rep");
        let mut len_bytes = [0u8; 8];
        len_bytes.copy_from_slice(stat_rep.as_slice());
        assert_eq!(u64::from_le_bytes(len_bytes), 4096);
        assert_eq!(svc.handled_count(), 3);
    }

    #[test]
    fn ext4_backend_rejects_oversized_write() {
        let mut backend = Ext4Backend::new();
        let fd = backend.openat(0x3030).expect("open");
        assert_eq!(
            backend.write(fd, (16 * 1024 * 1024) + 1),
            Err(VfsLiteError::Unsupported)
        );
    }

    #[test]
    fn initramfs_backend_is_read_only_and_resolves_busybox() {
        let mut svc = VfsLiteService::with_backend(ReadOnlyInitramfsBackend::new(2048));
        let open = Message::with_header(
            0,
            VFS_OP_OPENAT,
            0,
            None,
            &pack(0, INITRAMFS_BUSYBOX_PATH_PTR, 0, 0),
        )
        .expect("open");
        let open_rep = svc.handle_request(open).expect("open rep");
        let mut fd_bytes = [0u8; 8];
        fd_bytes.copy_from_slice(open_rep.as_slice());
        let fd = u64::from_le_bytes(fd_bytes);

        let read =
            Message::with_header(0, VFS_OP_READ, 0, None, &pack(fd, 0, 512, 0)).expect("read");
        let read_rep = svc.handle_request(read).expect("read rep");
        assert_eq!(read_rep.opcode, VFS_OP_READ);

        let write =
            Message::with_header(0, VFS_OP_WRITE, 0, None, &pack(fd, 0, 1, 0)).expect("write");
        assert_eq!(svc.handle_request(write), Err(VfsLiteError::Unsupported));
    }

    #[test]
    fn console_backend_exposes_dev_console_write() {
        let mut svc = VfsLiteService::with_backend(ConsoleBackend::default());
        let open = Message::with_header(
            0,
            VFS_OP_OPENAT,
            0,
            None,
            &pack(0, DEV_CONSOLE_PATH_PTR, 0, 0),
        )
        .expect("open");
        let open_rep = svc.handle_request(open).expect("open rep");
        let mut fd_bytes = [0u8; 8];
        fd_bytes.copy_from_slice(open_rep.as_slice());
        let fd = u64::from_le_bytes(fd_bytes);

        let write =
            Message::with_header(0, VFS_OP_WRITE, 0, None, &pack(fd, 0, 32, 0)).expect("write");
        let write_rep = svc.handle_request(write).expect("write rep");
        assert_eq!(write_rep.opcode, VFS_OP_WRITE);
    }

    #[test]
    fn parser_extracts_openat_fields() {
        let open_req = Message::with_header(0, VFS_OP_OPENAT, 0, None, &pack(0, 0x1000, 0x10, 0))
            .expect("open");
        let parsed = VfsLiteService::<InMemoryBackend>::parse_request(open_req).expect("parse");
        assert_eq!(
            parsed,
            VfsRequest::OpenAt {
                _dirfd: 0,
                path_ptr: 0x1000,
                _flags: 0x10,
                _mode: 0,
            }
        );
    }

    #[test]
    fn open_read_close_lifecycle_is_stable() {
        let mut svc = VfsLiteService::new();

        let open_req =
            Message::with_header(0, VFS_OP_OPENAT, 0, None, &pack(0, 0x1000, 0, 0)).expect("open");
        let open_rep = svc.handle_request(open_req).expect("open rep");
        let mut fd_bytes = [0u8; 8];
        fd_bytes.copy_from_slice(open_rep.as_slice());
        let fd = u64::from_le_bytes(fd_bytes);

        let read_req =
            Message::with_header(0, VFS_OP_READ, 0, None, &pack(fd, 0x2000, 64, 0)).expect("read");
        let read_rep = svc.handle_request(read_req).expect("read rep");
        assert_eq!(read_rep.opcode, VFS_OP_READ);

        let close_req =
            Message::with_header(0, VFS_OP_CLOSE, 0, None, &pack(fd, 0, 0, 0)).expect("close");
        let close_rep = svc.handle_request(close_req).expect("close rep");
        assert_eq!(close_rep.opcode, VFS_OP_CLOSE);
    }

    #[test]
    fn mount_policy_can_deny_console_path() {
        let mut svc = VfsLiteService::with_backend(ConsoleBackend::default());
        svc.set_policy(MountNamespacePolicy {
            allow_console: false,
            allow_initramfs: true,
        });
        let open = Message::with_header(
            0,
            VFS_OP_OPENAT,
            0,
            None,
            &pack(0, DEV_CONSOLE_PATH_PTR, 0, 0),
        )
        .expect("open");
        assert_eq!(
            svc.handle_request(open),
            Err(VfsLiteError::PermissionDenied)
        );
    }

    #[test]
    fn op_sequence_increments_per_successful_request() {
        let mut svc = VfsLiteService::new();
        let open =
            Message::with_header(0, VFS_OP_OPENAT, 0, None, &pack(0, 0x1000, 0, 0)).expect("open");
        let _ = svc.handle_request(open).expect("open rep");
        assert_eq!(svc.op_sequence(), 1);
    }

    #[test]
    fn mount_router_routes_by_path_split() {
        let router = MountRouter::new(0x8000, InMemoryBackend::new(), InMemoryBackend::new());
        let mut svc = VfsLiteService::with_backend(router);

        let open_low =
            Message::with_header(0, VFS_OP_OPENAT, 0, None, &pack(0, 0x1000, 0, 0)).expect("open");
        let low_rep = svc.handle_request(open_low).expect("rep");
        assert_eq!(low_rep.opcode, VFS_OP_OPENAT);

        let open_high =
            Message::with_header(0, VFS_OP_OPENAT, 0, None, &pack(0, 0x9000, 0, 0)).expect("open");
        let high_rep = svc.handle_request(open_high).expect("rep");
        assert_eq!(high_rep.opcode, VFS_OP_OPENAT);
    }

    #[test]
    fn read_rejects_unknown_fd() {
        let mut svc = VfsLiteService::new();
        let read_req =
            Message::with_header(0, VFS_OP_READ, 0, None, &pack(99, 0, 1, 0)).expect("read");
        assert_eq!(svc.handle_request(read_req), Err(VfsLiteError::BadFd));
    }

    #[test]
    fn rejects_unsupported_opcode() {
        let mut svc = VfsLiteService::new();
        let req = Message::with_header(0, 0xFFFF, 0, None, &pack(0, 0, 0, 0)).expect("msg");
        assert_eq!(svc.handle_request(req), Err(VfsLiteError::Unsupported));
    }
}
