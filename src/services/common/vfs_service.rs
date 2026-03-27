use crate::kernel::ipc::Message;
use crate::kernel::vfs::{
    InMemoryBackend, MountNamespacePolicy, MountRecord, VfsBackend, VfsError, VfsRequest,
};
use crate::kernel::vfs_abi::{
    OpenAtArgs, ReadWriteArgs, StatxArgs, VFS_OP_CLOSE, VFS_OP_DUP, VFS_OP_EPOLL_CREATE1,
    VFS_OP_EPOLL_CTL, VFS_OP_EPOLL_PWAIT, VFS_OP_FCNTL, VFS_OP_IOCTL, VFS_OP_OPENAT, VFS_OP_POLL,
    VFS_OP_READ, VFS_OP_SENDFILE, VFS_OP_STATX, VFS_OP_WRITE, VfsV1Args,
};

const MAX_MOUNTS: usize = 8;

#[derive(Debug)]
pub struct VfsService<B: VfsBackend = InMemoryBackend> {
    backend: B,
    policy: MountNamespacePolicy,
    op_sequence: u64,
    mounts: [Option<MountRecord>; MAX_MOUNTS],
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
            mounts: [None; MAX_MOUNTS],
        }
    }
}

impl<B: VfsBackend> VfsService<B> {
    pub const fn with_backend(backend: B) -> Self {
        Self {
            backend,
            policy: MountNamespacePolicy::baseline(),
            op_sequence: 0,
            mounts: [None; MAX_MOUNTS],
        }
    }

    pub fn set_policy(&mut self, policy: MountNamespacePolicy) {
        self.policy = policy;
    }

    pub const fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub const fn op_sequence(&self) -> u64 {
        self.op_sequence
    }

    pub fn mount(&mut self, mountpoint_ptr: u64, fs_tag: u64) -> Result<(), VfsError> {
        if self
            .mounts
            .iter()
            .flatten()
            .any(|record| record.mountpoint_ptr == mountpoint_ptr && record.active)
        {
            return Err(VfsError::MountConflict);
        }
        if let Some(slot) = self.mounts.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(MountRecord {
                mountpoint_ptr,
                fs_tag,
                active: true,
                failed: false,
            });
            return Ok(());
        }
        Err(VfsError::NoFd)
    }

    pub fn unmount(&mut self, mountpoint_ptr: u64) -> Result<(), VfsError> {
        if let Some(record) = self
            .mounts
            .iter_mut()
            .flatten()
            .find(|record| record.mountpoint_ptr == mountpoint_ptr && record.active)
        {
            record.active = false;
            return Ok(());
        }
        Err(VfsError::MountNotFound)
    }

    pub fn mark_mount_failed(&mut self, mountpoint_ptr: u64) -> Result<(), VfsError> {
        if let Some(record) = self
            .mounts
            .iter_mut()
            .flatten()
            .find(|record| record.mountpoint_ptr == mountpoint_ptr)
        {
            record.failed = true;
            record.active = false;
            return Ok(());
        }
        Err(VfsError::MountNotFound)
    }

    pub fn recover_mount(&mut self, mountpoint_ptr: u64) -> Result<(), VfsError> {
        if let Some(record) = self
            .mounts
            .iter_mut()
            .flatten()
            .find(|record| record.mountpoint_ptr == mountpoint_ptr)
        {
            record.failed = false;
            record.active = true;
            return Ok(());
        }
        Err(VfsError::MountNotFound)
    }

    pub fn mount_record(&self, mountpoint_ptr: u64) -> Option<MountRecord> {
        self.mounts
            .iter()
            .flatten()
            .find(|record| record.mountpoint_ptr == mountpoint_ptr)
            .copied()
    }

    pub fn active_mounts(&self) -> usize {
        self.mounts
            .iter()
            .flatten()
            .filter(|record| record.active)
            .count()
    }

    fn u64_reply(opcode: u16, value: u64) -> Result<Message, VfsError> {
        Message::with_header(0, opcode, 0, None, &value.to_le_bytes())
            .map_err(|_| VfsError::Malformed)
    }

    pub fn parse_request(request: Message) -> Result<VfsRequest, VfsError> {
        match request.opcode {
            VFS_OP_OPENAT => {
                let args =
                    OpenAtArgs::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
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
            VFS_OP_IOCTL => {
                let args =
                    VfsV1Args::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::Ioctl {
                    fd: args.arg0,
                    request: args.arg1,
                    arg: args.arg2,
                })
            }
            VFS_OP_DUP => {
                let args =
                    VfsV1Args::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::Dup { fd: args.arg0 })
            }
            VFS_OP_FCNTL => {
                let args =
                    VfsV1Args::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::Fcntl {
                    fd: args.arg0,
                    cmd: args.arg1,
                    arg: args.arg2,
                })
            }
            VFS_OP_POLL => {
                let args =
                    VfsV1Args::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::Poll {
                    fds_ptr: args.arg0,
                    nfds: args.arg1,
                    timeout: args.arg2,
                })
            }
            VFS_OP_EPOLL_CREATE1 => {
                let args =
                    VfsV1Args::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::EpollCreate1 { flags: args.arg0 })
            }
            VFS_OP_EPOLL_CTL => {
                let args =
                    VfsV1Args::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::EpollCtl {
                    epfd: args.arg0,
                    op: args.arg1,
                    fd: args.arg2,
                    event_ptr: args.arg3,
                })
            }
            VFS_OP_EPOLL_PWAIT => {
                let args =
                    VfsV1Args::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::EpollPwait {
                    epfd: args.arg0,
                    events_ptr: args.arg1,
                    maxevents: args.arg2,
                    timeout: args.arg3,
                })
            }
            VFS_OP_SENDFILE => {
                let args =
                    VfsV1Args::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::Sendfile {
                    out_fd: args.arg0,
                    in_fd: args.arg1,
                    offset_ptr: args.arg2,
                    count: args.arg3,
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
            VfsRequest::Ioctl { fd, request, arg } => {
                Self::u64_reply(VFS_OP_IOCTL, self.backend.ioctl(fd, request, arg)?)
            }
            VfsRequest::Dup { fd } => Self::u64_reply(VFS_OP_DUP, self.backend.dup(fd)?),
            VfsRequest::Fcntl { fd, cmd, arg } => {
                Self::u64_reply(VFS_OP_FCNTL, self.backend.fcntl(fd, cmd, arg)?)
            }
            VfsRequest::Poll {
                fds_ptr,
                nfds,
                timeout,
            } => Self::u64_reply(VFS_OP_POLL, self.backend.poll(fds_ptr, nfds, timeout)?),
            VfsRequest::EpollCreate1 { flags } => {
                Self::u64_reply(VFS_OP_EPOLL_CREATE1, self.backend.epoll_create1(flags)?)
            }
            VfsRequest::EpollCtl {
                epfd,
                op,
                fd,
                event_ptr,
            } => Self::u64_reply(
                VFS_OP_EPOLL_CTL,
                self.backend.epoll_ctl(epfd, op, fd, event_ptr)?,
            ),
            VfsRequest::EpollPwait {
                epfd,
                events_ptr,
                maxevents,
                timeout,
            } => Self::u64_reply(
                VFS_OP_EPOLL_PWAIT,
                self.backend
                    .epoll_pwait(epfd, events_ptr, maxevents, timeout)?,
            ),
            VfsRequest::Sendfile {
                out_fd,
                in_fd,
                offset_ptr,
                count,
            } => Self::u64_reply(
                VFS_OP_SENDFILE,
                self.backend.sendfile(out_fd, in_fd, offset_ptr, count)?,
            ),
        }?;
        self.op_sequence = self.op_sequence.saturating_add(1);
        Ok(reply)
    }
}
