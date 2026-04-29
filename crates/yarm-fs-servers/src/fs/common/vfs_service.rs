// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::vfs_ipc::{
    InMemoryBackend, MountNamespacePolicy, MountRecord, VfsBackend, VfsError, VfsRequest,
};
use yarm_ipc_abi::vfs_abi::{
    OpenAtArgs, OpenAtInlinePath, ReadWriteArgs, StatxArgs, StatxInlinePath, VFS_OP_CLOSE,
    VFS_OP_DUP, VFS_OP_EPOLL_CREATE1, VFS_OP_EPOLL_CTL, VFS_OP_EPOLL_PWAIT, VFS_OP_FCNTL,
    VFS_OP_IOCTL, VFS_OP_OPENAT, VFS_OP_POLL, VFS_OP_READ, VFS_OP_SENDFILE, VFS_OP_STATX,
    VFS_OP_WRITE, VfsV1Args,
};
use yarm_user_rt::ipc::Message;

const MAX_MOUNTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsReply {
    OpenAtFd(u64),
    CloseResult(u64),
    ReadLen(u64),
    WriteLen(u64),
    StatxValue(u64),
    IoctlResult(u64),
    DupFd(u64),
    FcntlResult(u64),
    PollEvents(u64),
    EpollFd(u64),
    EpollCtlResult(u64),
    EpollWaitEvents(u64),
    SendfileLen(u64),
}

impl VfsReply {
    const fn opcode(self) -> u16 {
        match self {
            Self::OpenAtFd(_) => VFS_OP_OPENAT,
            Self::CloseResult(_) => VFS_OP_CLOSE,
            Self::ReadLen(_) => VFS_OP_READ,
            Self::WriteLen(_) => VFS_OP_WRITE,
            Self::StatxValue(_) => VFS_OP_STATX,
            Self::IoctlResult(_) => VFS_OP_IOCTL,
            Self::DupFd(_) => VFS_OP_DUP,
            Self::FcntlResult(_) => VFS_OP_FCNTL,
            Self::PollEvents(_) => VFS_OP_POLL,
            Self::EpollFd(_) => VFS_OP_EPOLL_CREATE1,
            Self::EpollCtlResult(_) => VFS_OP_EPOLL_CTL,
            Self::EpollWaitEvents(_) => VFS_OP_EPOLL_PWAIT,
            Self::SendfileLen(_) => VFS_OP_SENDFILE,
        }
    }

    pub const fn as_u64(self) -> u64 {
        match self {
            Self::OpenAtFd(value)
            | Self::CloseResult(value)
            | Self::ReadLen(value)
            | Self::WriteLen(value)
            | Self::StatxValue(value)
            | Self::IoctlResult(value)
            | Self::DupFd(value)
            | Self::FcntlResult(value)
            | Self::PollEvents(value)
            | Self::EpollFd(value)
            | Self::EpollCtlResult(value)
            | Self::EpollWaitEvents(value)
            | Self::SendfileLen(value) => value,
        }
    }

    pub fn to_message(self) -> Result<Message, VfsError> {
        Message::with_header(0, self.opcode(), 0, None, &self.as_u64().to_le_bytes())
            .map_err(|_| VfsError::Malformed)
    }

    pub fn from_message(message: Message) -> Result<Self, VfsError> {
        let bytes = message.as_slice();
        if bytes.len() != 8 {
            return Err(VfsError::Malformed);
        }
        let mut arr = [0u8; 8];
        arr.copy_from_slice(bytes);
        let value = u64::from_le_bytes(arr);
        match message.opcode {
            VFS_OP_OPENAT => Ok(Self::OpenAtFd(value)),
            VFS_OP_CLOSE => Ok(Self::CloseResult(value)),
            VFS_OP_READ => Ok(Self::ReadLen(value)),
            VFS_OP_WRITE => Ok(Self::WriteLen(value)),
            VFS_OP_STATX => Ok(Self::StatxValue(value)),
            VFS_OP_IOCTL => Ok(Self::IoctlResult(value)),
            VFS_OP_DUP => Ok(Self::DupFd(value)),
            VFS_OP_FCNTL => Ok(Self::FcntlResult(value)),
            VFS_OP_POLL => Ok(Self::PollEvents(value)),
            VFS_OP_EPOLL_CREATE1 => Ok(Self::EpollFd(value)),
            VFS_OP_EPOLL_CTL => Ok(Self::EpollCtlResult(value)),
            VFS_OP_EPOLL_PWAIT => Ok(Self::EpollWaitEvents(value)),
            VFS_OP_SENDFILE => Ok(Self::SendfileLen(value)),
            _ => Err(VfsError::Unsupported),
        }
    }
}

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

    pub fn parse_request(request: Message) -> Result<VfsRequest, VfsError> {
        match request.opcode {
            VFS_OP_OPENAT => {
                if let Some(inline) = OpenAtInlinePath::decode(request.as_slice()) {
                    return Ok(VfsRequest::OpenAt {
                        _dirfd: inline.dirfd,
                        path_ptr: 0,
                        path_inline: Some(
                            super::vfs_ipc::PathBytes::from_slice(inline.path)
                                .map_err(|_| VfsError::Malformed)?,
                        ),
                        _flags: inline.flags,
                        _mode: inline.mode,
                    });
                }
                let args =
                    OpenAtArgs::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::OpenAt {
                    _dirfd: args.dirfd,
                    path_ptr: args.path_ptr,
                    path_inline: None,
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
                if let Some(inline) = StatxInlinePath::decode(request.as_slice()) {
                    return Ok(VfsRequest::Statx {
                        _dirfd: inline.dirfd,
                        path_ptr: 0,
                        path_inline: Some(
                            super::vfs_ipc::PathBytes::from_slice(inline.path)
                                .map_err(|_| VfsError::Malformed)?,
                        ),
                        _flags: inline.flags,
                        _mask_or_buf: inline.mask_or_buf,
                    });
                }
                let args =
                    StatxArgs::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
                Ok(VfsRequest::Statx {
                    _dirfd: args.dirfd,
                    path_ptr: args.path_ptr,
                    path_inline: None,
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
            VfsRequest::OpenAt {
                path_ptr,
                path_inline,
                ..
            } => {
                let _ = path_ptr;
                if let Some(path) = path_inline {
                    if !self.policy.allows_path_bytes(path.as_slice()) {
                        return Err(VfsError::PermissionDenied);
                    }
                    VfsReply::OpenAtFd(self.backend.openat_path(path.as_slice())?)
                } else {
                    return Err(VfsError::Malformed);
                }
            }
            VfsRequest::Close { fd } => VfsReply::CloseResult(self.backend.close(fd)?),
            VfsRequest::Read { fd, len, .. } => VfsReply::ReadLen(self.backend.read(fd, len)?),
            VfsRequest::Write { fd, len, .. } => VfsReply::WriteLen(self.backend.write(fd, len)?),
            VfsRequest::Statx {
                path_ptr,
                path_inline,
                ..
            } => {
                let _ = path_ptr;
                if let Some(path) = path_inline {
                    if !self.policy.allows_path_bytes(path.as_slice()) {
                        return Err(VfsError::PermissionDenied);
                    }
                    VfsReply::StatxValue(self.backend.statx_path(path.as_slice())?)
                } else {
                    return Err(VfsError::Malformed);
                }
            }
            VfsRequest::Ioctl { fd, request, arg } => {
                VfsReply::IoctlResult(self.backend.ioctl(fd, request, arg)?)
            }
            VfsRequest::Dup { fd } => VfsReply::DupFd(self.backend.dup(fd)?),
            VfsRequest::Fcntl { fd, cmd, arg } => {
                VfsReply::FcntlResult(self.backend.fcntl(fd, cmd, arg)?)
            }
            VfsRequest::Poll {
                fds_ptr,
                nfds,
                timeout,
            } => VfsReply::PollEvents(self.backend.poll(fds_ptr, nfds, timeout)?),
            VfsRequest::EpollCreate1 { flags } => {
                VfsReply::EpollFd(self.backend.epoll_create1(flags)?)
            }
            VfsRequest::EpollCtl {
                epfd,
                op,
                fd,
                event_ptr,
            } => VfsReply::EpollCtlResult(self.backend.epoll_ctl(epfd, op, fd, event_ptr)?),
            VfsRequest::EpollPwait {
                epfd,
                events_ptr,
                maxevents,
                timeout,
            } => VfsReply::EpollWaitEvents(
                self.backend
                    .epoll_pwait(epfd, events_ptr, maxevents, timeout)?,
            ),
            VfsRequest::Sendfile {
                out_fd,
                in_fd,
                offset_ptr,
                count,
            } => VfsReply::SendfileLen(self.backend.sendfile(out_fd, in_fd, offset_ptr, count)?),
        };
        self.op_sequence = self.op_sequence.saturating_add(1);
        reply.to_message()
    }
}
