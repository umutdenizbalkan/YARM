//! Generic VFS primitives and shared abstractions.
//! Concrete filesystem services must live under `src/services/*`.

use super::ipc::Message;
use super::vfs_abi::{
    OpenAtArgs, ReadWriteArgs, StatxArgs, VFS_OP_CLOSE, VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_STATX,
    VFS_OP_WRITE, VfsV1Args,
};

const MAX_FDS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsError {
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
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsError>;
    fn close(&mut self, fd: u64) -> Result<u64, VfsError>;
    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsError>;
    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsError>;
    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsError>;
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
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        self.route_by_path(path_ptr).openat(path_ptr)
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

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        self.route_by_path(path_ptr).statx(path_ptr)
    }
}

impl VfsBackend for InMemoryBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        self.alloc_fd(path_ptr)
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

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        Ok(path_ptr)
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
    pub path_ptr: u64,
    pub flags: u64,
    pub mask_or_buf: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountNamespacePolicy {
    pub allow_all: bool,
}

impl MountNamespacePolicy {
    pub const fn baseline() -> Self {
        Self { allow_all: true }
    }

    pub const fn deny_all() -> Self {
        Self { allow_all: false }
    }

    pub const fn allows_path(self, _path_ptr: u64) -> bool {
        self.allow_all
    }
}

#[derive(Debug)]
pub struct VfsService<B: VfsBackend = InMemoryBackend> {
    backend: B,
    policy: MountNamespacePolicy,
    op_sequence: u64,
}

impl Default for VfsService<InMemoryBackend> {
    fn default() -> Self {
        Self::new()
    }
}

impl VfsService<InMemoryBackend> {
    pub const fn new() -> Self {
        Self {
            backend: InMemoryBackend::new(),
            policy: MountNamespacePolicy::baseline(),
            op_sequence: 0,
        }
    }
}

impl<B: VfsBackend> VfsService<B> {
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

    fn u64_reply(opcode: u16, value: u64) -> Result<Message, VfsError> {
        Message::with_header(0, opcode, 0, None, &value.to_le_bytes())
            .map_err(|_| VfsError::Malformed)
    }

    pub fn parse_request(request: Message) -> Result<VfsRequest, VfsError> {
        match request.opcode {
            VFS_OP_OPENAT => {
                let args = OpenAtArgs::decode(request.as_slice())
                    .map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::OpenAt {
                    _dirfd: args.dirfd,
                    path_ptr: args.path_ptr,
                    _flags: args.flags,
                    _mode: args.mode,
                })
            }
            VFS_OP_CLOSE => {
                let args =
                    VfsV1Args::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::Close { fd: args.arg0 })
            }
            VFS_OP_READ => {
                let args =
                    ReadWriteArgs::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::Read {
                    fd: args.fd,
                    _buf_ptr: args.buf_ptr,
                    len: args.len,
                })
            }
            VFS_OP_WRITE => {
                let args =
                    ReadWriteArgs::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::Write {
                    fd: args.fd,
                    _buf_ptr: args.buf_ptr,
                    len: args.len,
                })
            }
            VFS_OP_STATX => {
                let args =
                    StatxArgs::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::Statx {
                    _dirfd: args.dirfd,
                    path_ptr: args.path_ptr,
                    _flags: args.flags,
                    _mask_or_buf: args.mask_or_buf,
                })
            }
            _ => Err(VfsError::Unsupported),
        }
    }

    pub fn handle_request(&mut self, request: Message) -> Result<Message, VfsError> {
        let parsed = Self::parse_request(request)?;
        let reply = match parsed {
            VfsRequest::OpenAt { path_ptr, .. } => {
                if !self.policy.allows_path(path_ptr) {
                    return Err(VfsError::PermissionDenied);
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
                    return Err(VfsError::PermissionDenied);
                }
                Self::u64_reply(VFS_OP_STATX, self.backend.statx(path_ptr)?)
            }
        }?;
        self.op_sequence = self.op_sequence.saturating_add(1);
        Ok(reply)
    }
}

pub fn openat_message(req: OpenAtRequest) -> Result<Message, VfsError> {
    Message::with_header(
        0,
        VFS_OP_OPENAT,
        0,
        None,
        &OpenAtArgs::new(req.dirfd, req.path_ptr, req.flags, req.mode).encode(),
    )
    .map_err(|_| VfsError::Malformed)
}

pub fn close_message(req: CloseRequest) -> Result<Message, VfsError> {
    Message::with_header(
        0,
        VFS_OP_CLOSE,
        0,
        None,
        &VfsV1Args::new(req.fd, 0, 0, 0).encode(),
    )
    .map_err(|_| VfsError::Malformed)
}

pub fn read_message(req: ReadWriteRequest) -> Result<Message, VfsError> {
    Message::with_header(
        0,
        VFS_OP_READ,
        0,
        None,
        &ReadWriteArgs::new(req.fd, req.buf_ptr, req.len).encode(),
    )
    .map_err(|_| VfsError::Malformed)
}

pub fn write_message(req: ReadWriteRequest) -> Result<Message, VfsError> {
    Message::with_header(
        0,
        VFS_OP_WRITE,
        0,
        None,
        &ReadWriteArgs::new(req.fd, req.buf_ptr, req.len).encode(),
    )
    .map_err(|_| VfsError::Malformed)
}

pub fn statx_message(req: StatxRequest) -> Result<Message, VfsError> {
    Message::with_header(
        0,
        VFS_OP_STATX,
        0,
        None,
        &StatxArgs::new(req.dirfd, req.path_ptr, req.flags, req.mask_or_buf).encode(),
    )
    .map_err(|_| VfsError::Malformed)
}

pub trait FilesystemService {
    fn service_name(&self) -> &'static str;
    fn dispatch(&mut self, request: Message) -> Result<Message, VfsError>;
}

pub fn dispatch_once<S: FilesystemService>(
    service: &mut S,
    request: Message,
) -> Result<Message, VfsError> {
    service.dispatch(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dummy;
    impl FilesystemService for Dummy {
        fn service_name(&self) -> &'static str {
            "dummy"
        }

        fn dispatch(&mut self, request: Message) -> Result<Message, VfsError> {
            Ok(request)
        }
    }

    fn pack(a0: u64, a1: u64, a2: u64, a3: u64) -> [u8; 32] {
        OpenAtArgs::new(a0, a1, a2, a3).encode()
    }

    #[test]
    fn dispatch_helper_roundtrips_message() {
        let mut dummy = Dummy;
        let msg = Message::with_header(0, 7, 0, None, &[1]).expect("msg");
        let rep = dispatch_once(&mut dummy, msg).expect("dispatch");
        assert_eq!(rep.opcode, 7);
    }

    #[test]
    fn typed_openat_message_encodes_vfs_abi() {
        let req = OpenAtRequest {
            dirfd: 0,
            path_ptr: 0x1000,
            flags: 1,
            mode: 0,
        };
        let msg = openat_message(req).expect("open");
        assert_eq!(msg.opcode, VFS_OP_OPENAT);
    }

    #[test]
    fn parser_extracts_openat_fields() {
        let open_req = Message::with_header(0, VFS_OP_OPENAT, 0, None, &pack(0, 0x1000, 0x10, 0))
            .expect("open");
        let parsed = VfsService::<InMemoryBackend>::parse_request(open_req).expect("parse");
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
        let mut svc = VfsService::new();

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
    fn deny_all_policy_blocks_open() {
        let mut svc = VfsService::new();
        svc.set_policy(MountNamespacePolicy::deny_all());
        let open =
            Message::with_header(0, VFS_OP_OPENAT, 0, None, &pack(0, 0x1000, 0, 0)).expect("open");
        assert_eq!(svc.handle_request(open), Err(VfsError::PermissionDenied));
    }

    #[test]
    fn op_sequence_increments_per_successful_request() {
        let mut svc = VfsService::new();
        let open =
            Message::with_header(0, VFS_OP_OPENAT, 0, None, &pack(0, 0x1000, 0, 0)).expect("open");
        let _ = svc.handle_request(open).expect("open rep");
        assert_eq!(svc.op_sequence(), 1);
    }

    #[test]
    fn mount_router_routes_by_path_split() {
        let router = MountRouter::new(0x8000, InMemoryBackend::new(), InMemoryBackend::new());
        let mut svc = VfsService::with_backend(router);

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
        let mut svc = VfsService::new();
        let read_req =
            Message::with_header(0, VFS_OP_READ, 0, None, &pack(99, 0, 1, 0)).expect("read");
        assert_eq!(svc.handle_request(read_req), Err(VfsError::BadFd));
    }

    #[test]
    fn rejects_unsupported_opcode() {
        let mut svc = VfsService::new();
        let req = Message::with_header(0, 0xFFFF, 0, None, &pack(0, 0, 0, 0)).expect("msg");
        assert_eq!(svc.handle_request(req), Err(VfsError::Unsupported));
    }
}
