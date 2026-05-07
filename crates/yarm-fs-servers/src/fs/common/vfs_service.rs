// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::vfs_ipc::{
    InMemoryBackend, MountNamespacePolicy, MountRecord, VfsBackend, VfsError, VfsRequest,
};
use yarm_ipc_abi::vfs_abi::{
    OpenAtInlinePath, ReadWriteArgs, StatxInlinePath, VFS_OP_CLOSE,
    VFS_OP_DUP, VFS_OP_EPOLL_CREATE1, VFS_OP_EPOLL_CTL, VFS_OP_EPOLL_PWAIT, VFS_OP_FCNTL,
    VFS_OP_IOCTL, VFS_OP_OPENAT, VFS_OP_POLL, VFS_OP_READ, VFS_OP_SENDFILE, VFS_OP_STATX,
    VFS_OP_WRITE, VfsV1Args,
};
use yarm_user_rt::ipc::Message;
use yarm_ipc_abi::ipc_v2::{
    IpcV2SharedReplyMeta, IPC_V2_SHARED_REPLY_FLAG_READ_ONLY, IPC_V2_SHARED_REPLY_META_VERSION,
    encode_shared_reply_meta,
};
use yarm_user_rt::syscall::{AnonMapResult, vm_anon_map, vm_unmap};

const MAX_MOUNTS: usize = 8;
const VFS_READ_SHARED_REPLY_ENABLED: bool = false;
const VFS_READ_SHARED_REPLY_THRESHOLD: u64 = 64;
const VFS_SHARED_STAGE_BASE: usize = 0x4000_0000;
const VM_MAP_PROT_READ: u64 = 0x1;
const VM_MAP_PROT_WRITE: u64 = 0x2;

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
    fn try_build_shared_read_reply(
        read_len: u64,
        inline_len: usize,
        inline_bytes: &[u8],
    ) -> Result<Option<Message>, VfsError> {
        Self::try_build_shared_read_reply_with_policy_and_allocator(
            VFS_READ_SHARED_REPLY_ENABLED,
            read_len,
            inline_len,
            inline_bytes,
            || {
                // SAFETY: VM_ANON_MAP is explicit userspace syscall surface.
                unsafe {
                    vm_anon_map(
                        VFS_SHARED_STAGE_BASE,
                        read_len as usize,
                        VM_MAP_PROT_READ | VM_MAP_PROT_WRITE,
                    )
                }
            },
            |map| {
                // SAFETY: producer-local cleanup syscall wrappers.
                unsafe {
                    vm_unmap(map.base, map.len)?;
                }
                Ok(())
            },
        )
    }

    fn try_build_shared_read_reply_with_policy_and_allocator<F, C>(
        enabled: bool,
        read_len: u64,
        inline_len: usize,
        inline_bytes: &[u8],
        mut alloc: F,
        mut cleanup: C,
    ) -> Result<Option<Message>, VfsError>
    where
        F: FnMut() -> Result<AnonMapResult, yarm_user_rt::syscall::SyscallError>,
        C: FnMut(&AnonMapResult) -> Result<(), yarm_user_rt::syscall::SyscallError>,
    {
        if !enabled {
            return Ok(None);
        }
        if read_len <= VFS_READ_SHARED_REPLY_THRESHOLD || inline_len == 0 || inline_len as u64 != read_len {
            return Ok(None);
        }
        let map = match alloc() {
            Ok(map) => map,
            Err(_) => return Ok(None),
        };
        let write_len = core::cmp::min(inline_len, map.len);
        if write_len == 0 {
            return Ok(None);
        }
        // SAFETY: VM_ANON_MAP returned a mapped writable userspace region.
        let dst = unsafe { core::slice::from_raw_parts_mut(map.base as *mut u8, write_len) };
        dst.copy_from_slice(&inline_bytes[..write_len]);
        let meta = IpcV2SharedReplyMeta {
            version: IPC_V2_SHARED_REPLY_META_VERSION,
            flags: IPC_V2_SHARED_REPLY_FLAG_READ_ONLY,
            reserved: 0,
            offset: 0,
            len: write_len as u64,
        };
        let payload = encode_shared_reply_meta(meta).map_err(|_| VfsError::Malformed)?;
        let msg = Message::with_header(
            0,
            VFS_OP_READ,
            Message::FLAG_CAP_TRANSFER,
            Some(map.mem_cap),
            &payload,
        )
        .map_err(|_| VfsError::Malformed)?;
        // NOTE(stage2): intentionally retain local mem_cap here. Releasing
        // before IPC handoff/materialization can invalidate transfer lifetime.
        // Deferred until post-handoff policy or kernel-side transfer pinning.
        if cleanup(&map).is_err() {
            return Ok(None);
        }
        Ok(Some(msg))
    }

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
                let inline =
                    OpenAtInlinePath::decode(request.as_slice()).ok_or(VfsError::Malformed)?;
                Ok(VfsRequest::OpenAt {
                    _dirfd: inline.dirfd,
                    path_inline: Some(
                        super::vfs_ipc::PathBytes::from_slice(inline.path)
                            .map_err(|_| VfsError::Malformed)?,
                    ),
                    _flags: inline.flags,
                    _mode: inline.mode,
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
                let inline =
                    StatxInlinePath::decode(request.as_slice()).ok_or(VfsError::Malformed)?;
                Ok(VfsRequest::Statx {
                    _dirfd: inline.dirfd,
                    path_inline: Some(
                        super::vfs_ipc::PathBytes::from_slice(inline.path)
                            .map_err(|_| VfsError::Malformed)?,
                    ),
                    _flags: inline.flags,
                    _mask_or_buf: inline.mask_or_buf,
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
            VfsRequest::OpenAt { path_inline, .. } => {
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
            VfsRequest::Read { fd, len, .. } => {
                let mut inline = [0u8; Message::MAX_PAYLOAD - 16];
                let (read_len, inline_len) = self.backend.read_into(fd, len, &mut inline)?;
                if let Some(shared_reply) =
                    Self::try_build_shared_read_reply(read_len, inline_len, &inline[..inline_len])?
                {
                    return Ok(shared_reply);
                }
                if inline_len == 0 {
                    VfsReply::ReadLen(read_len)
                } else {
                    let mut payload = [0u8; Message::MAX_PAYLOAD];
                    payload[..8].copy_from_slice(&read_len.to_le_bytes());
                    payload[8..16].copy_from_slice(&0u64.to_le_bytes());
                    payload[16..16 + inline_len].copy_from_slice(&inline[..inline_len]);
                    return Message::with_header(0, VFS_OP_READ, 0, None, &payload[..16 + inline_len])
                        .map_err(|_| VfsError::Malformed);
                }
            }
            VfsRequest::Write { fd, len, .. } => VfsReply::WriteLen(self.backend.write(fd, len)?),
            VfsRequest::Statx { path_inline, .. } => {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct InlineReadBackend;

    impl VfsBackend for InlineReadBackend {
        fn openat_path(&mut self, _path: &[u8]) -> Result<u64, VfsError> { Ok(7) }
        fn close(&mut self, _fd: u64) -> Result<u64, VfsError> { Ok(0) }
        fn read(&mut self, _fd: u64, len: u64) -> Result<u64, VfsError> { Ok(len) }
        fn read_into(&mut self, _fd: u64, _len: u64, out: &mut [u8]) -> Result<(u64, usize), VfsError> {
            let bytes = b"hello";
            out[..bytes.len()].copy_from_slice(bytes);
            Ok((bytes.len() as u64, bytes.len()))
        }
        fn write(&mut self, _fd: u64, len: u64) -> Result<u64, VfsError> { Ok(len) }
    }

    #[test]
    fn read_reply_default_path_stays_inline_extended() {
        let mut svc = VfsService::with_backend(InlineReadBackend);
        let req = Message::with_header(
            0,
            VFS_OP_READ,
            0,
            None,
            &ReadWriteArgs::new(7, 0x1000, 64).encode(),
        )
        .expect("request");
        let reply = svc.handle_request(req).expect("reply");
        assert_eq!(reply.opcode, VFS_OP_READ);
        assert_eq!(reply.transferred_cap(), None);
        assert_eq!(reply.as_slice().len(), 21);
        assert_eq!(&reply.as_slice()[16..21], b"hello");
    }

    #[test]
    fn read_shared_reply_branch_not_taken_when_disabled() {
        assert!(!VFS_READ_SHARED_REPLY_ENABLED, "pass1 default must stay disabled");
        let out = VfsService::<InlineReadBackend>::try_build_shared_read_reply(5, 5, b"hello")
            .expect("decision");
        assert!(out.is_none(), "shared-reply branch must stay off by default");
    }

    #[test]
    fn read_shared_reply_helper_falls_back_when_allocator_fails() {
        let out = VfsService::<InlineReadBackend>::try_build_shared_read_reply_with_policy_and_allocator(
            true,
            4096,
            5,
            b"hello",
            || Err(yarm_user_rt::syscall::SyscallError::InvalidArgs),
            |_| Ok(()),
        )
        .expect("fallback");
        assert!(out.is_none());
    }

    #[test]
    fn read_shared_reply_helper_emits_shared_meta_when_forced_enabled() {
        let data = [0xABu8; 256];
        let mut mapped = [0u8; 4096];
        let base = mapped.as_mut_ptr() as usize;
        let mut cleaned = false;
        let out = VfsService::<InlineReadBackend>::try_build_shared_read_reply_with_policy_and_allocator(
            true,
            256,
            data.len(),
            &data,
            || {
                Ok(AnonMapResult {
                    base,
                    len: 4096,
                    mem_cap: 77,
                })
            },
            |_| {
                cleaned = true;
                Ok(())
            },
        )
        .expect("shared")
        .expect("some");
        assert_eq!(out.opcode, VFS_OP_READ);
        assert_eq!(out.transferred_cap().map(|cap| cap.0), Some(77));
        assert!(cleaned, "successful shared path must cleanup local producer mapping");
    }

    #[test]
    fn read_shared_reply_helper_cleanup_failure_falls_back() {
        let data = [0xABu8; 256];
        let mut mapped = [0u8; 4096];
        let base = mapped.as_mut_ptr() as usize;
        let out = VfsService::<InlineReadBackend>::try_build_shared_read_reply_with_policy_and_allocator(
            true,
            256,
            data.len(),
            &data,
            || {
                Ok(AnonMapResult {
                    base,
                    len: 4096,
                    mem_cap: 77,
                })
            },
            |_| Err(yarm_user_rt::syscall::SyscallError::InvalidArgs),
        )
        .expect("fallback");
        assert!(out.is_none(), "cleanup failure must fallback to inline path");
    }

}
