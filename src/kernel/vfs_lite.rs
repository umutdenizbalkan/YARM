use super::ipc::Message;
use super::linux_compat::{
    VFS_OP_CLOSE, VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_STATX, VFS_OP_WRITE, VfsV1Args,
};

const MAX_FDS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsLiteError {
    Malformed,
    NoFd,
    BadFd,
    Unsupported,
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

#[derive(Debug)]
pub struct VfsLiteService<B: VfsBackend = InMemoryBackend> {
    backend: B,
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
        }
    }
}

impl<B: VfsBackend> VfsLiteService<B> {
    pub const fn with_backend(backend: B) -> Self {
        Self { backend }
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
        match Self::parse_request(request)? {
            VfsRequest::OpenAt { path_ptr, .. } => {
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
                Self::u64_reply(VFS_OP_STATX, self.backend.statx(path_ptr)?)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::linux_compat::{VFS_OP_OPENAT, VFS_OP_READ, VfsV1Args};

    fn pack(a0: u64, a1: u64, a2: u64, a3: u64) -> [u8; 32] {
        VfsV1Args::new(a0, a1, a2, a3).encode()
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
