//! Generic VFS primitives and shared abstractions.
//! Concrete filesystem services must live under `src/services/*`.

use super::ipc::Message;
use super::vfs_abi::{
    OpenAtArgs, ReadWriteArgs, StatxArgs, VFS_OP_CLOSE, VFS_OP_DUP, VFS_OP_EPOLL_CREATE1,
    VFS_OP_EPOLL_CTL, VFS_OP_EPOLL_PWAIT, VFS_OP_FCNTL, VFS_OP_IOCTL, VFS_OP_OPENAT, VFS_OP_POLL,
    VFS_OP_READ, VFS_OP_SENDFILE, VFS_OP_STATX, VFS_OP_WRITE, VfsV1Args,
};

const MAX_FDS: usize = 16;
const MAX_POLICY_RANGES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsError {
    Malformed,
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
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsError>;
    fn close(&mut self, fd: u64) -> Result<u64, VfsError>;
    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsError>;
    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsError>;
    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsError>;
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountRecord {
    pub mountpoint_ptr: u64,
    pub fs_tag: u64,
    pub active: bool,
    pub failed: bool,
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

pub fn ioctl_message(fd: u64, request: u64, arg: u64) -> Result<Message, VfsError> {
    Message::with_header(
        0,
        VFS_OP_IOCTL,
        0,
        None,
        &VfsV1Args::new(fd, request, arg, 0).encode(),
    )
    .map_err(|_| VfsError::Malformed)
}

pub fn dup_message(fd: u64) -> Result<Message, VfsError> {
    Message::with_header(
        0,
        VFS_OP_DUP,
        0,
        None,
        &VfsV1Args::new(fd, 0, 0, 0).encode(),
    )
    .map_err(|_| VfsError::Malformed)
}

pub fn fcntl_message(fd: u64, cmd: u64, arg: u64) -> Result<Message, VfsError> {
    Message::with_header(
        0,
        VFS_OP_FCNTL,
        0,
        None,
        &VfsV1Args::new(fd, cmd, arg, 0).encode(),
    )
    .map_err(|_| VfsError::Malformed)
}

pub fn poll_message(fds_ptr: u64, nfds: u64, timeout: u64) -> Result<Message, VfsError> {
    Message::with_header(
        0,
        VFS_OP_POLL,
        0,
        None,
        &VfsV1Args::new(fds_ptr, nfds, timeout, 0).encode(),
    )
    .map_err(|_| VfsError::Malformed)
}

pub fn epoll_create1_message(flags: u64) -> Result<Message, VfsError> {
    Message::with_header(
        0,
        VFS_OP_EPOLL_CREATE1,
        0,
        None,
        &VfsV1Args::new(flags, 0, 0, 0).encode(),
    )
    .map_err(|_| VfsError::Malformed)
}

pub fn epoll_ctl_message(epfd: u64, op: u64, fd: u64, event_ptr: u64) -> Result<Message, VfsError> {
    Message::with_header(
        0,
        VFS_OP_EPOLL_CTL,
        0,
        None,
        &VfsV1Args::new(epfd, op, fd, event_ptr).encode(),
    )
    .map_err(|_| VfsError::Malformed)
}

pub fn epoll_pwait_message(
    epfd: u64,
    events_ptr: u64,
    maxevents: u64,
    timeout: u64,
) -> Result<Message, VfsError> {
    Message::with_header(
        0,
        VFS_OP_EPOLL_PWAIT,
        0,
        None,
        &VfsV1Args::new(epfd, events_ptr, maxevents, timeout).encode(),
    )
    .map_err(|_| VfsError::Malformed)
}

pub fn sendfile_message(
    out_fd: u64,
    in_fd: u64,
    offset_ptr: u64,
    count: u64,
) -> Result<Message, VfsError> {
    Message::with_header(
        0,
        VFS_OP_SENDFILE,
        0,
        None,
        &VfsV1Args::new(out_fd, in_fd, offset_ptr, count).encode(),
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
    use crate::services::common::vfs_service::VfsReply;
    use crate::services::common::vfs_service::VfsService;

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

    fn decode_reply_u64(reply: Message) -> u64 {
        match VfsReply::from_message(reply).expect("decode reply") {
            VfsReply::OpenAtFd(value)
            | VfsReply::CloseResult(value)
            | VfsReply::ReadLen(value)
            | VfsReply::WriteLen(value)
            | VfsReply::StatxValue(value)
            | VfsReply::IoctlResult(value)
            | VfsReply::DupFd(value)
            | VfsReply::FcntlResult(value)
            | VfsReply::PollEvents(value)
            | VfsReply::EpollFd(value)
            | VfsReply::EpollCtlResult(value)
            | VfsReply::EpollWaitEvents(value)
            | VfsReply::SendfileLen(value) => value,
        }
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
    fn path_policy_ranges_gate_boot_paths() {
        let mut svc = VfsService::new();
        svc.set_policy(MountNamespacePolicy::boot_profile());

        let allowed =
            Message::with_header(0, VFS_OP_OPENAT, 0, None, &pack(0, 0x1000, 0, 0)).expect("open");
        assert!(svc.handle_request(allowed).is_ok());

        let denied =
            Message::with_header(0, VFS_OP_OPENAT, 0, None, &pack(0, 0x3000, 0, 0)).expect("open");
        assert_eq!(svc.handle_request(denied), Err(VfsError::PermissionDenied));
    }

    #[test]
    fn mount_lifecycle_supports_failure_and_recovery() {
        let mut svc = VfsService::new();
        svc.mount(0x2000, 1).expect("mount");
        assert_eq!(svc.active_mounts(), 1);
        assert_eq!(svc.mount(0x2000, 2), Err(VfsError::MountConflict));

        svc.mark_mount_failed(0x2000).expect("mark failed");
        let record = svc.mount_record(0x2000).expect("record");
        assert!(!record.active);
        assert!(record.failed);

        svc.recover_mount(0x2000).expect("recover");
        let record = svc.mount_record(0x2000).expect("record");
        assert!(record.active);
        assert!(!record.failed);

        svc.unmount(0x2000).expect("unmount");
        assert_eq!(svc.active_mounts(), 0);
    }

    #[test]
    fn full_frozen_opcode_surface_roundtrips_through_service() {
        let mut svc = VfsService::new();
        let open_reply = svc
            .handle_request(
                openat_message(OpenAtRequest {
                    dirfd: 0,
                    path_ptr: 0x1000,
                    flags: 0,
                    mode: 0,
                })
                .expect("open"),
            )
            .expect("open reply");
        let fd = decode_reply_u64(open_reply);

        let dup_reply = svc
            .handle_request(dup_message(fd).expect("dup"))
            .expect("dup reply");
        let dup_fd = decode_reply_u64(dup_reply);
        assert!(dup_fd > fd);

        let ioctl = svc
            .handle_request(ioctl_message(fd, 0x1234, 0x55).expect("ioctl"))
            .expect("ioctl reply");
        assert_eq!(decode_reply_u64(ioctl), 0x1234 ^ 0x55);

        let fcntl = svc
            .handle_request(fcntl_message(fd, 3, 9).expect("fcntl"))
            .expect("fcntl reply");
        assert_eq!(decode_reply_u64(fcntl), 12);

        let poll = svc
            .handle_request(poll_message(0x9000, 2, 10).expect("poll"))
            .expect("poll reply");
        assert_eq!(decode_reply_u64(poll), 1);

        let epoll_create = svc
            .handle_request(epoll_create1_message(0).expect("epoll create"))
            .expect("epoll create reply");
        let epfd = decode_reply_u64(epoll_create);

        let epoll_ctl = svc
            .handle_request(epoll_ctl_message(epfd, 1, fd, 0xA000).expect("epoll ctl"))
            .expect("epoll ctl reply");
        assert_eq!(decode_reply_u64(epoll_ctl), 0);

        let epoll_wait = svc
            .handle_request(epoll_pwait_message(epfd, 0xB000, 4, 10).expect("epoll wait"))
            .expect("epoll wait reply");
        assert_eq!(decode_reply_u64(epoll_wait), 1);

        let sendfile = svc
            .handle_request(sendfile_message(fd, dup_fd, 0xC000, 99).expect("sendfile"))
            .expect("sendfile reply");
        assert_eq!(decode_reply_u64(sendfile), 99);

        let statx = svc
            .handle_request(
                statx_message(StatxRequest {
                    dirfd: 0,
                    path_ptr: 0x1000,
                    flags: 0,
                    mask_or_buf: 0,
                })
                .expect("statx"),
            )
            .expect("statx reply");
        assert_eq!(decode_reply_u64(statx), 0x1000);
        assert_eq!(svc.op_sequence(), 10);
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
