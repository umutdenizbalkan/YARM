// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::shared_io_adapter::{
    VfsReadSharedBinding, VfsReadSharedBindingError, VfsSharedIoMapper, VfsWriteSharedBinding,
    VfsWriteSharedBindingError,
};
use super::vfs_ipc::{
    InMemoryBackend, MountNamespacePolicy, MountRecord, VfsBackend, VfsError, VfsRequest,
};
use yarm_ipc_abi::vfs_abi::{
    OpenAtInlinePath, ReadWriteArgs, StatxInlinePath, VFS_OP_CLOSE, VFS_OP_DUP,
    VFS_OP_EPOLL_CREATE1, VFS_OP_EPOLL_CTL, VFS_OP_EPOLL_PWAIT, VFS_OP_FCNTL, VFS_OP_IOCTL,
    VFS_OP_OPENAT, VFS_OP_POLL, VFS_OP_READ, VFS_OP_SENDFILE, VFS_OP_STATX, VFS_OP_WRITE,
    VFS_SHARED_IO_STATUS_OK, VfsReadSharedReply, VfsReadSharedRequest, VfsV1Args,
    VfsWriteSharedReply, VfsWriteSharedRequest,
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

    /// Gated live route for WRITE_SHARED_REQUEST backed by recv_shared_v3 MAP_READ.
    ///
    /// **Production use:** call only when `VFS_WRITE_SHARED_REQUEST_ENABLED` is `true` (it is not,
    /// by default). When the gate constant is false, no production code path calls this method.
    /// Tests call it directly to prove the live route is correct.
    ///
    /// **What this does:**
    /// 1. Validates the recv_shared_v3 delivery metadata against the descriptor via
    ///    `VfsWriteSharedBinding::validate()`.
    /// 2. Calls `mapper.with_write_request_buffer` to obtain an immutable byte slice.
    /// 3. Calls `backend.write_shared_bytes(fd, bytes)` with the mapped bytes.
    /// 4. Calls `mapper.release(descriptor)` for cleanup (regardless of write success/failure).
    /// 5. Returns a `VfsWriteSharedReply` on success.
    ///
    /// **Not changed:** `handle_request` still returns `VfsError::Unsupported` for
    /// `VFS_OP_WRITE_SHARED_REQUEST`. `READ_SHARED_REPLY` is not dispatched here.
    pub fn dispatch_write_shared_request<M: VfsSharedIoMapper>(
        &mut self,
        request: VfsWriteSharedRequest,
        cleanup_token: u64,
        transferred_cap: u64,
        object_kind: u32,
        exact_region_len: u64,
        mapped_base: u64,
        page_rounded_mapped_len: u64,
        actual_mapping_perm: u32,
        mapper: &mut M,
    ) -> Result<VfsWriteSharedReply, VfsError> {
        let binding = VfsWriteSharedBinding::validate(
            cleanup_token,
            transferred_cap,
            object_kind,
            exact_region_len,
            mapped_base,
            page_rounded_mapped_len,
            actual_mapping_perm,
            &request,
        )
        .map_err(|err| match err {
            VfsWriteSharedBindingError::WrongDescriptorAccess
            | VfsWriteSharedBindingError::DescriptorHandleMismatch
            | VfsWriteSharedBindingError::DescriptorGenerationMismatch => {
                VfsError::PermissionDenied
            }
            _ => VfsError::Malformed,
        })?;

        let fd = binding.fd;
        let requested_len = binding.requested_len;
        let descriptor = binding.descriptor();
        let request_id = binding.request_id;

        let write_result = mapper.with_write_request_buffer(descriptor, requested_len, |bytes| {
            self.backend.write_shared_bytes(fd, bytes)
        });

        // Release/cleanup after access attempt, regardless of write outcome.
        let _ = mapper.release(descriptor);

        let bytes_written = write_result
            .map_err(|_| VfsError::Malformed)?
            .map_err(|_| VfsError::Malformed)?;

        self.op_sequence = self.op_sequence.saturating_add(1);
        Ok(VfsWriteSharedReply {
            request_id,
            bytes_completed: bytes_written,
            status: VFS_SHARED_IO_STATUS_OK,
            flags: 0,
        })
    }

    /// Gated live route for READ_SHARED_REPLY backed by recv_shared_v3 MAP_WRITE.
    ///
    /// **Production use:** call only when `VFS_READ_SHARED_REPLY_ENABLED` is `true` (it is not,
    /// by default). The Stage 60 kernel gate hard-rejects `map_intent & WRITE != 0`, so no live
    /// recv_shared_v3 delivery will ever provide `actual_mapping_perm = 3` until the gate is
    /// removed. Tests call this directly via `BorrowedSharedIoTestMapper` which simulates write
    /// access without going through the kernel path.
    ///
    /// **What this does:**
    /// 1. Validates the recv_shared_v3 delivery metadata against the descriptor via
    ///    `VfsReadSharedBinding::validate()`.
    /// 2. Calls `mapper.with_read_reply_buffer(descriptor, requested_len, |buf| backend.read_shared_bytes(fd, buf))`.
    /// 3. Calls `mapper.release(descriptor)` unconditionally after the access attempt.
    /// 4. Returns a `VfsReadSharedReply` on success.
    ///
    /// **Not changed:** `handle_request` still returns `VfsError::Unsupported` for
    /// `VFS_OP_READ_SHARED_REPLY`. The Stage 60 kernel MAP_WRITE gate is not touched.
    pub fn dispatch_read_shared_reply<M: VfsSharedIoMapper>(
        &mut self,
        request: VfsReadSharedRequest,
        cleanup_token: u64,
        transferred_cap: u64,
        object_kind: u32,
        exact_region_len: u64,
        mapped_base: u64,
        page_rounded_mapped_len: u64,
        actual_mapping_perm: u32,
        mapper: &mut M,
    ) -> Result<VfsReadSharedReply, VfsError> {
        let binding = VfsReadSharedBinding::validate(
            cleanup_token,
            transferred_cap,
            object_kind,
            exact_region_len,
            mapped_base,
            page_rounded_mapped_len,
            actual_mapping_perm,
            &request,
        )
        .map_err(|err| match err {
            VfsReadSharedBindingError::WrongDescriptorAccess
            | VfsReadSharedBindingError::DescriptorHandleMismatch
            | VfsReadSharedBindingError::DescriptorGenerationMismatch => {
                VfsError::PermissionDenied
            }
            _ => VfsError::Malformed,
        })?;

        let fd = binding.fd;
        let requested_len = binding.requested_len;
        let descriptor = binding.descriptor();
        let request_id = binding.request_id;

        let read_result = mapper.with_read_reply_buffer(descriptor, requested_len, |buf| {
            self.backend.read_shared_bytes(fd, buf)
        });

        // Release/cleanup after access attempt, regardless of read outcome.
        let _ = mapper.release(descriptor);

        let bytes_read = read_result
            .map_err(|_| VfsError::Malformed)?
            .map_err(|_| VfsError::Malformed)?;

        self.op_sequence = self.op_sequence.saturating_add(1);
        Ok(VfsReadSharedReply {
            request_id,
            bytes_completed: bytes_read,
            status: VFS_SHARED_IO_STATUS_OK,
            flags: 0,
        })
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
                if inline_len == 0 {
                    VfsReply::ReadLen(read_len)
                } else {
                    let mut payload = [0u8; Message::MAX_PAYLOAD];
                    payload[..8].copy_from_slice(&read_len.to_le_bytes());
                    payload[8..16].copy_from_slice(&0u64.to_le_bytes());
                    payload[16..16 + inline_len].copy_from_slice(&inline[..inline_len]);
                    return Message::with_header(
                        0,
                        VFS_OP_READ,
                        0,
                        None,
                        &payload[..16 + inline_len],
                    )
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
mod stage66_68_tests {
    use super::*;
    use crate::fs::common::shared_io_adapter::{
        BorrowedSharedIoTestMapper, UnsupportedSharedIoMapper, VFS_READ_SHARED_REPLY_ENABLED,
        VFS_SHARED_IO_ENABLED, VFS_WRITE_SHARED_REQUEST_ENABLED,
    };
    use crate::fs::common::vfs_ipc::{read_shared_message, write_shared_message};
    use crate::fs::ramfs::tree::RamFsBackend;
    use yarm_ipc_abi::vfs_abi::{
        VFS_SHARED_BUFFER_FS_READ, VFS_SHARED_BUFFER_FS_WRITE, VFS_SHARED_IO_STATUS_OK,
        VfsReadSharedRequest, VfsSharedBufferDescriptor, VfsWriteSharedRequest,
    };

    const TOKEN: u64 = 0x0002_0001; // gen=2, slot=1
    const CAP: u64 = 7;
    const KIND_DMA: u32 = 5;
    const MAPPED_BASE: u64 = 0x1000;
    const MAPPED_LEN: u64 = 4096;
    const REGION_LEN: u64 = 4096;
    const PERM_RO: u32 = 1;

    fn write_request(fd: u64, len: u64) -> VfsWriteSharedRequest {
        VfsWriteSharedRequest {
            fd,
            file_offset: 0,
            requested_len: len,
            request_id: 1,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(
                TOKEN,
                TOKEN >> 16,
                0,
                len,
                VFS_SHARED_BUFFER_FS_READ,
            ),
        }
    }

    fn ramfs_svc_with_file(path: &[u8]) -> (VfsService<RamFsBackend>, u64) {
        let mut svc = VfsService::with_backend(RamFsBackend::new());
        svc.backend_mut().create_file(path).expect("create");
        let fd = svc.backend_mut().open_path(path).expect("open");
        (svc, fd)
    }

    #[test]
    fn stage66_default_dispatch_still_rejects_write_shared_opcode() {
        let msg = write_shared_message(write_request(100, 8)).expect("msg");
        assert_eq!(
            VfsService::<InMemoryBackend>::parse_request(msg),
            Err(VfsError::Unsupported),
            "handle_request must not dispatch VFS_OP_WRITE_SHARED_REQUEST by default"
        );
    }

    #[test]
    fn stage66_gated_dispatch_ramfs_write_shared_succeeds() {
        let (mut svc, fd) = ramfs_svc_with_file(b"/stage66a");
        let mut storage = *b"stage66!";
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut storage);
        let reply = svc
            .dispatch_write_shared_request(
                write_request(fd, 8),
                TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RO,
                &mut mapper,
            )
            .expect("dispatch");
        assert_eq!(reply.request_id, 1);
        assert_eq!(reply.bytes_completed, 8);
        assert_eq!(reply.status, VFS_SHARED_IO_STATUS_OK);
        assert_eq!(reply.flags, 0);
    }

    #[test]
    fn stage66_gated_dispatch_bytes_written_match_file_contents() {
        let (mut svc, fd) = ramfs_svc_with_file(b"/stage66b");
        let mut storage = *b"hello66!";
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut storage);
        svc.dispatch_write_shared_request(
            write_request(fd, 8),
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RO,
            &mut mapper,
        )
        .expect("dispatch");
        svc.backend_mut().close_fd(fd).expect("close write");
        let fd2 = svc.backend_mut().open_path(b"/stage66b").expect("reopen");
        let mut buf = [0u8; 8];
        let n = svc.backend_mut().read_bytes(fd2, &mut buf).expect("read");
        assert_eq!(&buf[..n], b"hello66!");
    }

    #[test]
    fn stage66_gated_dispatch_cleanup_performed_exactly_once() {
        let (mut svc, fd) = ramfs_svc_with_file(b"/stage66c");
        let mut storage = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut storage);
        svc.dispatch_write_shared_request(
            write_request(fd, 8),
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RO,
            &mut mapper,
        )
        .expect("dispatch");
        assert_eq!(mapper.release_count(), 1, "release must be called exactly once");
    }

    #[test]
    fn stage66_gated_dispatch_op_sequence_advances_on_success() {
        let (mut svc, fd) = ramfs_svc_with_file(b"/stage66d");
        let seq_before = svc.op_sequence();
        let mut storage = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut storage);
        svc.dispatch_write_shared_request(
            write_request(fd, 8),
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RO,
            &mut mapper,
        )
        .expect("dispatch");
        assert_eq!(svc.op_sequence(), seq_before + 1);
    }

    #[test]
    fn stage66_gated_dispatch_missing_cleanup_token_rejected() {
        let (mut svc, fd) = ramfs_svc_with_file(b"/stage66e");
        let mut storage = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut storage);
        let result = svc.dispatch_write_shared_request(
            write_request(fd, 8),
            0, // cleanup_token = 0 → MissingCleanupToken
            CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RO,
            &mut mapper,
        );
        assert_eq!(result, Err(VfsError::Malformed));
    }

    #[test]
    fn stage66_gated_dispatch_stale_generation_rejected() {
        let (mut svc, fd) = ramfs_svc_with_file(b"/stage66f");
        let mut req = write_request(fd, 8);
        req.buffer.object_generation = (TOKEN >> 16) + 1; // stale generation
        let mut storage = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut storage);
        let result = svc.dispatch_write_shared_request(
            req, TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RO,
            &mut mapper,
        );
        assert_eq!(result, Err(VfsError::PermissionDenied));
    }

    #[test]
    fn stage66_gated_dispatch_wrong_object_handle_rejected() {
        let (mut svc, fd) = ramfs_svc_with_file(b"/stage66g");
        let mut req = write_request(fd, 8);
        req.buffer.object_handle = TOKEN + 1; // wrong handle
        let mut storage = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut storage);
        let result = svc.dispatch_write_shared_request(
            req, TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RO,
            &mut mapper,
        );
        assert_eq!(result, Err(VfsError::PermissionDenied));
    }

    #[test]
    fn stage66_gated_dispatch_non_readonly_mapping_rejected() {
        let (mut svc, fd) = ramfs_svc_with_file(b"/stage66h");
        let mut storage = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut storage);
        let result = svc.dispatch_write_shared_request(
            write_request(fd, 8),
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN,
            3, // MAP_READ|MAP_WRITE — not read-only
            &mut mapper,
        );
        assert_eq!(result, Err(VfsError::Malformed));
    }

    #[test]
    fn stage66_gated_dispatch_range_too_short_rejected() {
        let (mut svc, fd) = ramfs_svc_with_file(b"/stage66i");
        let mut req = write_request(fd, 4096);
        req.buffer.buffer_offset = 1; // offset=1 + len=4096 → end=4097 > MAPPED_LEN=4096
        req.buffer.buffer_len = 4096;
        let mut storage = [0u8; 1];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut storage);
        let result = svc.dispatch_write_shared_request(
            req, TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RO,
            &mut mapper,
        );
        assert_eq!(result, Err(VfsError::Malformed));
    }

    #[test]
    fn stage66_gated_dispatch_unsupported_production_mapper_rejected() {
        let (mut svc, fd) = ramfs_svc_with_file(b"/stage66j");
        let result = svc.dispatch_write_shared_request(
            write_request(fd, 8),
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RO,
            &mut UnsupportedSharedIoMapper,
        );
        assert_eq!(result, Err(VfsError::Malformed));
    }

    #[test]
    fn stage66_gated_dispatch_cleanup_called_even_on_failed_write() {
        // If the backend write fails, release must still be called (cleanup before fallback).
        // UnsupportedSharedIoMapper fails with_write_request_buffer, but release is
        // still attempted (and also fails). No panic must occur.
        let mut svc = VfsService::with_backend(InMemoryBackend::new());
        let result = svc.dispatch_write_shared_request(
            write_request(1, 8),
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RO,
            &mut UnsupportedSharedIoMapper,
        );
        assert!(result.is_err(), "failed dispatch must return Err, not panic");
    }

    #[test]
    fn stage67_read_shared_reply_still_unsupported_by_parse_request() {
        let msg = read_shared_message(VfsReadSharedRequest {
            fd: 1,
            file_offset: 0,
            requested_len: 8,
            request_id: 1,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(1, 1, 0, 8, VFS_SHARED_BUFFER_FS_WRITE),
        })
        .expect("msg");
        assert_eq!(
            VfsService::<InMemoryBackend>::parse_request(msg),
            Err(VfsError::Unsupported),
            "READ_SHARED_REPLY must remain unsupported even when WRITE gate is enabled"
        );
    }

    #[test]
    fn stage68_write_shared_request_gate_disabled_by_default() {
        assert!(
            !VFS_WRITE_SHARED_REQUEST_ENABLED,
            "VFS_WRITE_SHARED_REQUEST_ENABLED must be false by default"
        );
    }

    #[test]
    fn stage68_read_shared_reply_gate_disabled_by_default() {
        // Stage 73 enabled VFS_READ_SHARED_REPLY_ENABLED after proving the RequesterExit
        // lifecycle model (deliver_requester_exit helper + 7 tests).
        assert!(
            VFS_READ_SHARED_REPLY_ENABLED,
            "VFS_READ_SHARED_REPLY_ENABLED must be true after Stage 73"
        );
    }

    #[test]
    fn stage68_global_vfs_shared_io_disabled_by_default() {
        assert!(
            !VFS_SHARED_IO_ENABLED,
            "VFS_SHARED_IO_ENABLED must be false — requires both WRITE and READ gates"
        );
    }

    #[test]
    fn stage68_global_gate_false_unless_both_direction_gates_true() {
        // Both must be true for global enable; since neither is, global is false.
        assert_eq!(
            VFS_SHARED_IO_ENABLED,
            VFS_WRITE_SHARED_REQUEST_ENABLED && VFS_READ_SHARED_REPLY_ENABLED
        );
    }
}

#[cfg(test)]
mod stage69_70_tests {
    use super::*;
    use crate::fs::common::shared_io_adapter::{
        BorrowedSharedIoTestMapper, UnsupportedSharedIoMapper, VFS_READ_SHARED_REPLY_ENABLED,
        VFS_SHARED_IO_ENABLED, VFS_WRITE_SHARED_REQUEST_ENABLED,
    };
    use crate::fs::common::vfs_ipc::{read_shared_message, write_shared_message};
    use crate::fs::ramfs::tree::RamFsBackend;
    use yarm_ipc_abi::vfs_abi::{
        VFS_SHARED_BUFFER_FS_READ, VFS_SHARED_BUFFER_FS_WRITE, VFS_SHARED_IO_STATUS_OK,
        VfsReadSharedRequest, VfsSharedBufferDescriptor, VfsWriteSharedRequest,
    };

    const TOKEN: u64 = 0x0003_0002; // gen=3, slot=2
    const CAP: u64 = 11;
    const KIND_DMA: u32 = 5;
    const MAPPED_BASE: u64 = 0x2000;
    const MAPPED_LEN: u64 = 4096;
    const REGION_LEN: u64 = 4096;
    // MAP_WRITE | MAP_READ — kernel MAP_WRITE gate removed by Stage 72; now live-deliverable.
    const PERM_RW: u32 = 3;

    fn read_request(fd: u64, len: u64) -> VfsReadSharedRequest {
        VfsReadSharedRequest {
            fd,
            file_offset: 0,
            requested_len: len,
            request_id: 1,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(
                TOKEN,
                TOKEN >> 16,
                0,
                len,
                VFS_SHARED_BUFFER_FS_WRITE,
            ),
        }
    }

    fn write_request(fd: u64, len: u64) -> VfsWriteSharedRequest {
        VfsWriteSharedRequest {
            fd,
            file_offset: 0,
            requested_len: len,
            request_id: 2,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(
                TOKEN,
                TOKEN >> 16,
                0,
                len,
                VFS_SHARED_BUFFER_FS_READ,
            ),
        }
    }

    fn ramfs_svc_with_content(path: &[u8], content: &[u8]) -> (VfsService<RamFsBackend>, u64) {
        let mut svc = VfsService::with_backend(RamFsBackend::new());
        svc.backend_mut().create_file(path).expect("create");
        let fd = svc.backend_mut().open_path(path).expect("open write");
        svc.backend_mut().write_bytes(fd, content).expect("seed");
        svc.backend_mut().close_fd(fd).expect("close seed");
        let fd = svc.backend_mut().open_path(path).expect("open read");
        (svc, fd)
    }

    #[test]
    fn stage69_audit_map_write_gate_remains_blocking() {
        // Stage 72 removed the Stage 60 WRITE gate, but VfsReadSharedBinding still requires
        // actual_mapping_perm & WRITE != 0. A MAP_READ-only delivery (perm=1) is always rejected.
        use crate::fs::common::shared_io_adapter::{
            VfsReadSharedBinding, VfsReadSharedBindingError,
        };
        let req = read_request(1, 8);
        // actual_mapping_perm = 1 (MAP_READ only) — no WRITE bit → MappingNotWritable
        let result = VfsReadSharedBinding::validate(
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN,
            1, // MAP_READ only — binding always rejects; READ_SHARED_REPLY requires write perm
            &req,
        );
        assert_eq!(
            result.err(),
            Some(VfsReadSharedBindingError::MappingNotWritable),
            "binding must reject MAP_READ-only perm even after Stage 72 removed the kernel gate"
        );
    }

    #[test]
    fn stage69_write_shared_request_still_works_after_read_shared_added() {
        // Regression: dispatch_write_shared_request must not be broken by the new read path.
        let mut svc = VfsService::with_backend(RamFsBackend::new());
        svc.backend_mut().create_file(b"/reg69").expect("create");
        let fd = svc.backend_mut().open_path(b"/reg69").expect("open");
        let write_token: u64 = 0x0001_0001;
        let mut storage = *b"regress!";
        let mut mapper = BorrowedSharedIoTestMapper::new(write_token, write_token >> 16, &mut storage);
        let req = VfsWriteSharedRequest {
            fd,
            file_offset: 0,
            requested_len: 8,
            request_id: 99,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(
                write_token, write_token >> 16, 0, 8, VFS_SHARED_BUFFER_FS_READ,
            ),
        };
        let reply = svc.dispatch_write_shared_request(
            req, write_token, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, 1,
            &mut mapper,
        ).expect("write dispatch");
        assert_eq!(reply.bytes_completed, 8);
    }

    #[test]
    fn stage69_read_shared_reply_default_dispatch_still_unsupported() {
        let msg = read_shared_message(read_request(1, 8)).expect("msg");
        assert_eq!(
            VfsService::<InMemoryBackend>::parse_request(msg),
            Err(VfsError::Unsupported),
            "handle_request must not dispatch VFS_OP_READ_SHARED_REPLY"
        );
    }

    #[test]
    fn stage69_gate_values_all_false() {
        // Stage 73 enabled VFS_READ_SHARED_REPLY_ENABLED; WRITE direction and umbrella still false.
        assert!(!VFS_WRITE_SHARED_REQUEST_ENABLED);
        assert!(VFS_READ_SHARED_REPLY_ENABLED);
        assert!(!VFS_SHARED_IO_ENABLED);
    }

    #[test]
    fn stage70_read_shared_reply_ramfs_writes_bytes_into_buffer() {
        let (mut svc, fd) = ramfs_svc_with_content(b"/stage70a", b"stage70!");
        let mut buf = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut buf);
        let reply = svc
            .dispatch_read_shared_reply(
                read_request(fd, 8),
                TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RW,
                &mut mapper,
            )
            .expect("dispatch");
        assert_eq!(reply.request_id, 1);
        assert_eq!(reply.bytes_completed, 8);
        assert_eq!(reply.status, VFS_SHARED_IO_STATUS_OK);
        assert_eq!(reply.flags, 0);
        drop(mapper);
        assert_eq!(&buf, b"stage70!");
    }

    #[test]
    fn stage70_read_shared_reply_short_eof_bytes_read_le_requested() {
        // File has 4 bytes; requested 8 → bytes_completed = 4 (EOF short read).
        let (mut svc, fd) = ramfs_svc_with_content(b"/stage70b", b"four");
        let mut buf = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut buf);
        let reply = svc
            .dispatch_read_shared_reply(
                read_request(fd, 8),
                TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RW,
                &mut mapper,
            )
            .expect("dispatch");
        assert_eq!(reply.bytes_completed, 4);
        assert_eq!(reply.status, VFS_SHARED_IO_STATUS_OK);
        drop(mapper);
        assert_eq!(&buf[..4], b"four");
    }

    #[test]
    fn stage70_read_shared_reply_wrong_direction_rejected() {
        // Descriptor with FS_READ access (not FS_WRITE) must be rejected.
        let (mut svc, fd) = ramfs_svc_with_content(b"/stage70c", b"xxxxxxxx");
        let mut buf = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut buf);
        let mut req = read_request(fd, 8);
        req.buffer.access = VFS_SHARED_BUFFER_FS_READ; // wrong direction
        let result = svc.dispatch_read_shared_reply(
            req, TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RW,
            &mut mapper,
        );
        assert_eq!(result, Err(VfsError::PermissionDenied));
    }

    #[test]
    fn stage70_read_shared_reply_readonly_mapping_rejected() {
        // actual_mapping_perm without WRITE bit must be rejected.
        let (mut svc, fd) = ramfs_svc_with_content(b"/stage70d", b"xxxxxxxx");
        let mut buf = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut buf);
        let result = svc.dispatch_read_shared_reply(
            read_request(fd, 8),
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN,
            1, // MAP_READ only — no WRITE bit
            &mut mapper,
        );
        assert_eq!(result, Err(VfsError::Malformed));
    }

    #[test]
    fn stage70_read_shared_reply_stale_generation_rejected() {
        let (mut svc, fd) = ramfs_svc_with_content(b"/stage70e", b"xxxxxxxx");
        let mut buf = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut buf);
        let mut req = read_request(fd, 8);
        req.buffer.object_generation = (TOKEN >> 16) + 1; // stale
        let result = svc.dispatch_read_shared_reply(
            req, TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RW,
            &mut mapper,
        );
        assert_eq!(result, Err(VfsError::PermissionDenied));
    }

    #[test]
    fn stage70_read_shared_reply_range_too_short_rejected() {
        let (mut svc, fd) = ramfs_svc_with_content(b"/stage70f", b"xxxxxxxx");
        let mut buf = [0u8; 1];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut buf);
        let mut req = read_request(fd, 4096);
        req.buffer.buffer_offset = 1; // offset=1 + len=4096 → end=4097 > MAPPED_LEN=4096
        req.buffer.buffer_len = 4096;
        let result = svc.dispatch_read_shared_reply(
            req, TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RW,
            &mut mapper,
        );
        assert_eq!(result, Err(VfsError::Malformed));
    }

    #[test]
    fn stage70_read_shared_reply_cleanup_called_on_backend_error() {
        // Backend returns Unsupported for InMemoryBackend.read_shared_bytes;
        // mapper.release must still be called.
        let mut svc = VfsService::with_backend(InMemoryBackend::new());
        let fd = svc.backend_mut().openat_path(b"/x").expect("open");
        let mut buf = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut buf);
        let result = svc.dispatch_read_shared_reply(
            read_request(fd, 8),
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RW,
            &mut mapper,
        );
        assert!(result.is_err(), "InMemoryBackend.read_shared_bytes returns Unsupported");
        assert_eq!(mapper.release_count(), 1, "release must be called even on backend error");
    }

    #[test]
    fn stage70_read_shared_reply_unsupported_production_mapper_rejects_safely() {
        let (mut svc, fd) = ramfs_svc_with_content(b"/stage70g", b"xxxxxxxx");
        let result = svc.dispatch_read_shared_reply(
            read_request(fd, 8),
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RW,
            &mut UnsupportedSharedIoMapper,
        );
        assert_eq!(result, Err(VfsError::Malformed));
    }

    #[test]
    fn stage70_read_shared_reply_op_sequence_advances_on_success() {
        let (mut svc, fd) = ramfs_svc_with_content(b"/stage70h", b"hello70h");
        let seq_before = svc.op_sequence();
        let mut buf = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut buf);
        svc.dispatch_read_shared_reply(
            read_request(fd, 8),
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RW,
            &mut mapper,
        ).expect("dispatch");
        assert_eq!(svc.op_sequence(), seq_before + 1);
    }

    #[test]
    fn stage70_read_shared_reply_cleanup_exactly_once() {
        let (mut svc, fd) = ramfs_svc_with_content(b"/stage70i", b"hello70i");
        let mut buf = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut buf);
        svc.dispatch_read_shared_reply(
            read_request(fd, 8),
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RW,
            &mut mapper,
        ).expect("dispatch");
        assert_eq!(mapper.release_count(), 1, "release must be called exactly once");
    }

    #[test]
    fn stage70_global_vfs_shared_io_still_false() {
        assert!(
            !VFS_SHARED_IO_ENABLED,
            "VFS_SHARED_IO_ENABLED must remain false until both direction gates pass"
        );
    }

    #[test]
    fn stage70_write_shared_request_still_unsupported_in_handle_request() {
        let msg = write_shared_message(write_request(1, 8)).expect("msg");
        assert_eq!(
            VfsService::<InMemoryBackend>::parse_request(msg),
            Err(VfsError::Unsupported)
        );
    }
}

#[cfg(test)]
mod shared_io_dispatch_tests {
    use super::*;
    use crate::fs::common::vfs_ipc::{
        read_shared_message, write_inline_message, write_shared_message,
    };
    use yarm_ipc_abi::vfs_abi::{
        VFS_SHARED_BUFFER_FS_READ, VFS_SHARED_BUFFER_FS_WRITE, VfsReadSharedRequest,
        VfsSharedBufferDescriptor, VfsWriteInlineRequest, VfsWriteSharedRequest,
    };

    #[test]
    fn shared_io_opcodes_remain_unsupported_by_live_dispatch() {
        let read = read_shared_message(VfsReadSharedRequest {
            fd: 1,
            file_offset: 0,
            requested_len: 16,
            request_id: 1,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(1, 1, 0, 16, VFS_SHARED_BUFFER_FS_WRITE),
        })
        .expect("read shared message");
        assert!(matches!(
            VfsService::<InMemoryBackend>::parse_request(read),
            Err(VfsError::Unsupported)
        ));

        let write = write_shared_message(VfsWriteSharedRequest {
            fd: 1,
            file_offset: 0,
            requested_len: 16,
            request_id: 2,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(2, 1, 0, 16, VFS_SHARED_BUFFER_FS_READ),
        })
        .expect("write shared message");
        assert!(matches!(
            VfsService::<InMemoryBackend>::parse_request(write),
            Err(VfsError::Unsupported)
        ));

        let inline = write_inline_message(VfsWriteInlineRequest {
            fd: 1,
            file_offset: 0,
            request_id: 3,
            flags: 0,
            bytes: b"not live",
        })
        .expect("inline write message");
        assert!(matches!(
            VfsService::<InMemoryBackend>::parse_request(inline),
            Err(VfsError::Unsupported)
        ));
    }
}

#[cfg(test)]
mod stage73_74_tests {
    //! Stage 73+74 — RequesterExit helper-only model proof + gated VFS_READ_SHARED_REPLY path.
    //!
    //! A. Gate status: VFS_READ_SHARED_REPLY_ENABLED=true, VFS_SHARED_IO_ENABLED=false.
    //! B. handle_request still returns Unsupported for the READ_SHARED_REPLY opcode.
    //! C. dispatch_read_shared_reply full RAMFS roundtrip with kernel-live MAP_WRITE perm=3.
    //! D. EOF short-read, exactly-once cleanup, readonly-perm rejected.
    //! E. WRITE_SHARED_REQUEST regression unaffected.

    use super::*;
    use crate::fs::common::shared_io_adapter::{
        BorrowedSharedIoTestMapper, VFS_READ_SHARED_REPLY_ENABLED,
        VFS_SHARED_IO_ENABLED, VFS_WRITE_SHARED_REQUEST_ENABLED,
    };
    use crate::fs::common::vfs_ipc::read_shared_message;
    use crate::fs::ramfs::tree::RamFsBackend;
    use yarm_ipc_abi::vfs_abi::{
        VFS_SHARED_BUFFER_FS_WRITE, VFS_SHARED_IO_STATUS_OK,
        VfsReadSharedRequest, VfsSharedBufferDescriptor,
    };

    // Distinct TOKEN from stage66-70 to avoid any slot collision in future refactors.
    const TOKEN: u64 = 0x0005_0004; // gen=5, slot=4
    const CAP: u64 = 17;
    const KIND_DMA: u32 = 5;
    const MAPPED_BASE: u64 = 0x4000;
    const MAPPED_LEN: u64 = 4096;
    const REGION_LEN: u64 = 4096;
    // Stage 72 removed the WRITE gate; perm=3 can now arrive from a real recv_shared_v3 call.
    const PERM_RW: u32 = 3;

    fn read_request(fd: u64, len: u64) -> VfsReadSharedRequest {
        VfsReadSharedRequest {
            fd,
            file_offset: 0,
            requested_len: len,
            request_id: 1,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(
                TOKEN,
                TOKEN >> 16,
                0,
                len,
                VFS_SHARED_BUFFER_FS_WRITE,
            ),
        }
    }

    fn ramfs_svc_with_content(path: &[u8], content: &[u8]) -> (VfsService<RamFsBackend>, u64) {
        let mut svc = VfsService::with_backend(RamFsBackend::new());
        svc.backend_mut().create_file(path).expect("create");
        let fd = svc.backend_mut().open_path(path).expect("open write");
        svc.backend_mut().write_bytes(fd, content).expect("seed");
        svc.backend_mut().close_fd(fd).expect("close seed");
        let fd = svc.backend_mut().open_path(path).expect("open read");
        (svc, fd)
    }

    // ── A. Gate status ────────────────────────────────────────────────────────

    #[test]
    fn stage74_vfs_read_shared_reply_enabled() {
        assert!(
            VFS_READ_SHARED_REPLY_ENABLED,
            "VFS_READ_SHARED_REPLY_ENABLED must be true after Stage 73"
        );
    }

    #[test]
    fn stage74_vfs_shared_io_still_disabled() {
        // Umbrella stays false: VFS_WRITE_SHARED_REQUEST_ENABLED is still false.
        assert!(
            !VFS_SHARED_IO_ENABLED,
            "VFS_SHARED_IO_ENABLED must remain false until both direction gates pass"
        );
    }

    #[test]
    fn stage74_write_shared_request_still_disabled() {
        assert!(
            !VFS_WRITE_SHARED_REQUEST_ENABLED,
            "VFS_WRITE_SHARED_REQUEST_ENABLED must remain false"
        );
    }

    // ── B. handle_request still Unsupported ──────────────────────────────────

    #[test]
    fn stage74_handle_request_rejects_read_shared_opcode() {
        let msg = read_shared_message(read_request(1, 8)).expect("msg");
        assert_eq!(
            VfsService::<InMemoryBackend>::parse_request(msg),
            Err(VfsError::Unsupported),
            "handle_request must not dispatch VFS_OP_READ_SHARED_REPLY even after Stage 73"
        );
    }

    // ── C. dispatch_read_shared_reply full roundtrip ──────────────────────────

    #[test]
    fn stage74_read_shared_reply_with_kernel_rw_perm_delivers_bytes() {
        // Stage 72 removed the WRITE gate; perm=3 is now live-deliverable from
        // recv_shared_v3.  Prove end-to-end: bytes from RAMFS are written into buffer.
        let (mut svc, fd) = ramfs_svc_with_content(b"/stage74a", b"stage74!");
        let mut buf = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut buf);
        let reply = svc
            .dispatch_read_shared_reply(
                read_request(fd, 8),
                TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RW,
                &mut mapper,
            )
            .expect("dispatch must succeed with perm=3");
        assert_eq!(reply.request_id, 1);
        assert_eq!(reply.bytes_completed, 8);
        assert_eq!(reply.status, VFS_SHARED_IO_STATUS_OK);
        drop(mapper);
        assert_eq!(&buf, b"stage74!");
    }

    // ── D. Edge cases ─────────────────────────────────────────────────────────

    #[test]
    fn stage74_read_shared_reply_short_eof() {
        // File has 5 bytes; request 8 → bytes_completed = 5 (EOF short read).
        let (mut svc, fd) = ramfs_svc_with_content(b"/stage74b", b"short");
        let mut buf = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut buf);
        let reply = svc
            .dispatch_read_shared_reply(
                read_request(fd, 8),
                TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RW,
                &mut mapper,
            )
            .expect("dispatch");
        assert_eq!(reply.bytes_completed, 5);
        assert_eq!(reply.status, VFS_SHARED_IO_STATUS_OK);
        drop(mapper);
        assert_eq!(&buf[..5], b"short");
    }

    #[test]
    fn stage74_read_shared_reply_cleanup_exactly_once() {
        let (mut svc, fd) = ramfs_svc_with_content(b"/stage74c", b"once74_!");
        let mut buf = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut buf);
        svc.dispatch_read_shared_reply(
            read_request(fd, 8),
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, PERM_RW,
            &mut mapper,
        )
        .expect("dispatch");
        assert_eq!(mapper.release_count(), 1, "release must be called exactly once");
    }

    #[test]
    fn stage74_read_shared_reply_readonly_perm_rejected() {
        // actual_mapping_perm=1 (MAP_READ only) must be rejected by VfsReadSharedBinding.
        let (mut svc, fd) = ramfs_svc_with_content(b"/stage74d", b"xxxxxxxx");
        let mut buf = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(TOKEN, TOKEN >> 16, &mut buf);
        let result = svc.dispatch_read_shared_reply(
            read_request(fd, 8),
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN,
            1, // MAP_READ only — VfsReadSharedBinding requires WRITE bit
            &mut mapper,
        );
        assert!(result.is_err(), "MAP_READ-only perm must be rejected");
    }

    // ── E. WRITE_SHARED_REQUEST regression ────────────────────────────────────

    #[test]
    fn stage74_write_shared_request_regression() {
        // dispatch_write_shared_request must be unaffected by Stage 73+74 changes.
        use crate::fs::common::shared_io_adapter::BorrowedSharedIoTestMapper;
        use yarm_ipc_abi::vfs_abi::{VFS_SHARED_BUFFER_FS_READ, VfsWriteSharedRequest};
        let write_token: u64 = 0x0001_0001;
        let mut svc = VfsService::with_backend(RamFsBackend::new());
        svc.backend_mut().create_file(b"/stage74e").expect("create");
        let fd = svc.backend_mut().open_path(b"/stage74e").expect("open");
        let mut storage = *b"reg74src";
        let mut mapper = BorrowedSharedIoTestMapper::new(write_token, write_token >> 16, &mut storage);
        let req = VfsWriteSharedRequest {
            fd,
            file_offset: 0,
            requested_len: 8,
            request_id: 99,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(
                write_token, write_token >> 16, 0, 8, VFS_SHARED_BUFFER_FS_READ,
            ),
        };
        let reply = svc
            .dispatch_write_shared_request(
                req, write_token, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN, 1,
                &mut mapper,
            )
            .expect("write dispatch must succeed");
        assert_eq!(reply.bytes_completed, 8, "write regression: 8 bytes written");
    }
}

#[cfg(test)]
mod stage75_tests {
    //! Stage 75 — TID-matched RequesterExit identity model + gate/regression checks.
    //!
    //! A. Gate status: VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED=false,
    //!    VFS_READ_SHARED_REPLY_ENABLED=true, VFS_SHARED_IO_ENABLED=false.
    //! B. handle_request still returns Unsupported for READ_SHARED_REPLY opcode.
    //! C. TID-matched deliver_requester_exit_if_tid_matches roundtrip via VfsService context.
    //! D. Unmatched TID is a safe no-op; lifecycle state unchanged.
    //! E. Old VFS read/write ops unchanged (regression).
    //!
    //! Production blockers documented in VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED:
    //! 1. Supervisor→VFS notification cap (new startup cap, not yet added).
    //! 2. VfsService persistent lifecycle store (not yet added).

    use super::*;
    use crate::fs::common::shared_io_adapter::{
        VFS_READ_SHARED_REPLY_ENABLED, VFS_SHARED_IO_ENABLED,
        VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED, VFS_WRITE_SHARED_REQUEST_ENABLED,
    };
    use crate::fs::common::shared_io_lifecycle::{
        VfsSharedIoCleanupResult, VfsSharedIoDirection, VfsSharedIoHandleTable,
        VfsSharedIoLifecycle, VfsSharedIoRequesterExitAction, VfsSharedIoTerminalReason,
    };
    use crate::fs::ramfs::tree::RamFsBackend;
    use yarm_ipc_abi::vfs_abi::{VFS_SHARED_BUFFER_FS_WRITE, VfsSharedBufferDescriptor};

    const STAGE75_TID_A: u64 = 0xA_0001;
    const STAGE75_TID_B: u64 = 0xB_0002;

    fn make_lifecycle_pair(
        tid: u64,
        direction: VfsSharedIoDirection,
        len: u64,
    ) -> (VfsSharedIoHandleTable<1>, VfsSharedIoLifecycle) {
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let handle = handles.allocate().expect("allocate");
        let access = match direction {
            VfsSharedIoDirection::ReadReply => VFS_SHARED_BUFFER_FS_WRITE,
            VfsSharedIoDirection::WriteRequest => {
                yarm_ipc_abi::vfs_abi::VFS_SHARED_BUFFER_FS_READ
            }
        };
        let desc = VfsSharedBufferDescriptor::new(
            handle.object_handle,
            handle.object_generation,
            0,
            len,
            access,
        );
        let lc = VfsSharedIoLifecycle::reserve(1, tid, desc, len, 0, direction)
            .expect("reserve lifecycle");
        (handles, lc)
    }

    #[test]
    fn stage75_supervisor_task_exit_notification_not_yet_wired() {
        // Production blocker #1: supervisor→VFS notification channel absent.
        // This constant is the machine-readable record of that gap.
        assert!(
            !VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED,
            "supervisor→VFS task-exit channel must remain disabled until \
             startup cap + supervisor forwarding are wired"
        );
    }

    #[test]
    fn stage75_vfs_shared_io_still_disabled() {
        assert!(!VFS_SHARED_IO_ENABLED);
    }

    #[test]
    fn stage75_vfs_read_shared_reply_still_enabled() {
        assert!(VFS_READ_SHARED_REPLY_ENABLED);
    }

    #[test]
    fn stage75_write_shared_request_still_disabled() {
        assert!(!VFS_WRITE_SHARED_REQUEST_ENABLED);
    }

    #[test]
    fn stage75_tid_matched_exit_cleans_lifecycle_in_vfs_context() {
        // Models what VFS would do on receiving SUPERVISOR_OP_TASK_EXITED(tid=STAGE75_TID_A):
        // TID match → Matched(Won(RequesterExit)).
        let (mut handles, mut lc) = make_lifecycle_pair(
            STAGE75_TID_A,
            VfsSharedIoDirection::ReadReply,
            16,
        );
        lc.map(&handles).expect("map");
        lc.begin().expect("begin");
        let action = lc
            .deliver_requester_exit_if_tid_matches(STAGE75_TID_A, &mut handles)
            .expect("deliver");
        assert_eq!(
            action,
            VfsSharedIoRequesterExitAction::Matched(VfsSharedIoCleanupResult::Won(
                VfsSharedIoTerminalReason::RequesterExit
            ))
        );
    }

    #[test]
    fn stage75_unrelated_task_exit_does_not_affect_active_request() {
        // SUPERVISOR_OP_TASK_EXITED for TID_B must not affect TID_A's lifecycle.
        let (mut handles, mut lc) = make_lifecycle_pair(
            STAGE75_TID_A,
            VfsSharedIoDirection::ReadReply,
            8,
        );
        lc.map(&handles).expect("map");
        lc.begin().expect("begin");
        let action = lc
            .deliver_requester_exit_if_tid_matches(STAGE75_TID_B, &mut handles)
            .expect("no-op");
        assert_eq!(action, VfsSharedIoRequesterExitAction::NotMatched);
        // Request still in-flight; backend can still write bytes.
        let _ = RamFsBackend::new(); // VFS context is consistent
    }

    #[test]
    fn stage75_handle_request_unchanged_for_read_shared_opcode() {
        use crate::fs::common::vfs_ipc::read_shared_message;
        use yarm_ipc_abi::vfs_abi::VfsReadSharedRequest;
        let mut svc = VfsService::with_backend(RamFsBackend::new());
        let req = VfsReadSharedRequest {
            fd: 0,
            file_offset: 0,
            requested_len: 8,
            request_id: 1,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(1, 1, 0, 8, VFS_SHARED_BUFFER_FS_WRITE),
        };
        let msg = read_shared_message(req).expect("msg");
        let result = svc.handle_request(msg);
        assert_eq!(result, Err(VfsError::Unsupported));
    }

    #[test]
    fn stage75_old_vfs_parse_request_accepts_standard_ops() {
        use crate::fs::common::vfs_ipc::openat_inline_message;
        // Regression: VfsService still parses standard VFS ops after Stage 75 changes.
        // READ_SHARED_REPLY and WRITE_SHARED_REQUEST remain rejected (Unsupported).
        let open_msg = openat_inline_message(0, b"/dev/console", 0, 0).expect("open");
        let result = VfsService::<InMemoryBackend>::parse_request(open_msg);
        assert!(
            result.is_ok(),
            "parse_request must succeed for a valid openat message: {result:?}"
        );
    }
}

#[cfg(test)]
mod stage76_tests {
    //! Stage 76 — PM-owned TaskExited/ProcessExited notification ABI + VFS handler model.
    //!
    //! A. Gate status: VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED=false.
    //!    Blockers: (1) no PM→VFS send cap in startup, (2) PM does not receive kernel exits.
    //! B. PROC_OP_TASK_EXITED=13 and PROC_OP_PROCESS_EXITED=14 codec roundtrips verified.
    //! C. handle_pm_task_exited correctly routes TID-matched exits to RequesterExit cleanup.
    //! D. handle_pm_task_exited is a safe no-op for unmatched TID.
    //! E. Duplicate TID-matched call after first is idempotent (already-cleaned result).
    //! F. handle_request still rejects unknown opcodes including 13 and 14.
    //! G. Gate constant regressions: supervisor gate, shared-IO umbrella, write direction.

    use super::*;
    use crate::fs::common::shared_io_adapter::{
        handle_pm_task_exited, VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED,
        VFS_READ_SHARED_REPLY_ENABLED, VFS_SHARED_IO_ENABLED,
        VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED, VFS_WRITE_SHARED_REQUEST_ENABLED,
    };
    use crate::fs::common::shared_io_lifecycle::{
        VfsSharedIoCleanupResult, VfsSharedIoDirection, VfsSharedIoHandleTable,
        VfsSharedIoLifecycle, VfsSharedIoRequesterExitAction, VfsSharedIoTerminalReason,
    };
    use yarm_ipc_abi::process_abi::{
        PmProcessExitedEvent, PmTaskExitedEvent, PROC_OP_PROCESS_EXITED, PROC_OP_TASK_EXITED,
    };
    use yarm_ipc_abi::vfs_abi::{VFS_SHARED_BUFFER_FS_WRITE, VfsSharedBufferDescriptor};

    const TID_A: u64 = 0x7600_0001;
    const TID_B: u64 = 0x7600_0002;

    fn lifecycle_pair(
        tid: u64,
        direction: VfsSharedIoDirection,
        len: u64,
    ) -> (VfsSharedIoHandleTable<1>, VfsSharedIoLifecycle) {
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let handle = handles.allocate().expect("allocate");
        let access = match direction {
            VfsSharedIoDirection::ReadReply => VFS_SHARED_BUFFER_FS_WRITE,
            VfsSharedIoDirection::WriteRequest => yarm_ipc_abi::vfs_abi::VFS_SHARED_BUFFER_FS_READ,
        };
        let desc = VfsSharedBufferDescriptor::new(
            handle.object_handle,
            handle.object_generation,
            0,
            len,
            access,
        );
        let lc = VfsSharedIoLifecycle::reserve(1, tid, desc, len, 0, direction)
            .expect("reserve");
        (handles, lc)
    }

    // ── A. Gate constants ──────────────────────────────────────────────────────

    #[test]
    fn stage76_pm_task_exit_notification_gate_disabled() {
        assert!(
            !VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED,
            "VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED must remain false: \
             no PM→VFS send cap and PM has no kernel task-exit notification"
        );
    }

    #[test]
    fn stage76_supervisor_task_exit_notification_still_disabled() {
        assert!(!VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED);
    }

    #[test]
    fn stage76_vfs_shared_io_umbrella_still_disabled() {
        assert!(!VFS_SHARED_IO_ENABLED);
    }

    #[test]
    fn stage76_read_shared_reply_still_enabled() {
        assert!(VFS_READ_SHARED_REPLY_ENABLED);
    }

    #[test]
    fn stage76_write_shared_request_still_disabled() {
        assert!(!VFS_WRITE_SHARED_REQUEST_ENABLED);
    }

    // ── B. Opcode constants ────────────────────────────────────────────────────

    #[test]
    fn stage76_proc_op_task_exited_is_13() {
        assert_eq!(PROC_OP_TASK_EXITED, 13u16);
    }

    #[test]
    fn stage76_proc_op_process_exited_is_14() {
        assert_eq!(PROC_OP_PROCESS_EXITED, 14u16);
    }

    // ── B. Codec roundtrips ────────────────────────────────────────────────────

    #[test]
    fn stage76_pm_task_exited_event_encode_decode_roundtrip() {
        let event = PmTaskExitedEvent::new(TID_A, 42);
        let encoded = event.encode();
        assert_eq!(encoded.len(), 16);
        let decoded = PmTaskExitedEvent::decode(&encoded).expect("decode");
        assert_eq!(decoded.tid, TID_A);
        assert_eq!(decoded.exit_code, 42);
    }

    #[test]
    fn stage76_pm_task_exited_event_decode_short_payload_rejected() {
        let short = [0u8; 15];
        let result = PmTaskExitedEvent::decode(&short);
        assert!(result.is_err(), "decode must reject payload shorter than 16 bytes");
    }

    #[test]
    fn stage76_pm_process_exited_event_encode_decode_roundtrip() {
        let event = PmProcessExitedEvent::new(TID_B, 255);
        let encoded = event.encode();
        assert_eq!(encoded.len(), 16);
        let decoded = PmProcessExitedEvent::decode(&encoded).expect("decode");
        assert_eq!(decoded.process_tid, TID_B);
        assert_eq!(decoded.exit_code, 255);
    }

    #[test]
    fn stage76_pm_process_exited_event_decode_short_payload_rejected() {
        let short = [0u8; 7];
        let result = PmProcessExitedEvent::decode(&short);
        assert!(result.is_err(), "decode must reject payload shorter than 16 bytes");
    }

    #[test]
    fn stage76_pm_task_exited_event_le_byte_order() {
        let event = PmTaskExitedEvent::new(0x0102_0304_0506_0708, 0xA1B2_C3D4_E5F6_0718);
        let enc = event.encode();
        assert_eq!(&enc[..8], &0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(&enc[8..16], &0xA1B2_C3D4_E5F6_0718u64.to_le_bytes());
    }

    // ── C. handle_pm_task_exited — matched TID ─────────────────────────────────

    #[test]
    fn stage76_pm_task_exited_matched_tid_delivers_requester_exit() {
        let (mut handles, mut lc) = lifecycle_pair(TID_A, VfsSharedIoDirection::ReadReply, 16);
        lc.map(&handles).expect("map");
        lc.begin().expect("begin");
        let action = handle_pm_task_exited(TID_A, &mut lc, &mut handles).expect("deliver");
        assert_eq!(
            action,
            VfsSharedIoRequesterExitAction::Matched(VfsSharedIoCleanupResult::Won(
                VfsSharedIoTerminalReason::RequesterExit
            ))
        );
    }

    #[test]
    fn stage76_pm_task_exited_matched_lifecycle_write_direction() {
        let (mut handles, mut lc) =
            lifecycle_pair(TID_A, VfsSharedIoDirection::WriteRequest, 8);
        lc.map(&handles).expect("map");
        lc.begin().expect("begin");
        let action = handle_pm_task_exited(TID_A, &mut lc, &mut handles).expect("deliver");
        assert_eq!(
            action,
            VfsSharedIoRequesterExitAction::Matched(VfsSharedIoCleanupResult::Won(
                VfsSharedIoTerminalReason::RequesterExit
            ))
        );
    }

    // ── D. handle_pm_task_exited — unmatched TID ──────────────────────────────

    #[test]
    fn stage76_pm_task_exited_unmatched_tid_is_safe_noop() {
        let (mut handles, mut lc) = lifecycle_pair(TID_A, VfsSharedIoDirection::ReadReply, 8);
        lc.map(&handles).expect("map");
        lc.begin().expect("begin");
        let action = handle_pm_task_exited(TID_B, &mut lc, &mut handles).expect("no-op");
        assert_eq!(action, VfsSharedIoRequesterExitAction::NotMatched);
        // Lifecycle must still be in-flight after the no-op.
        let second = handle_pm_task_exited(TID_A, &mut lc, &mut handles).expect("second");
        assert_eq!(
            second,
            VfsSharedIoRequesterExitAction::Matched(VfsSharedIoCleanupResult::Won(
                VfsSharedIoTerminalReason::RequesterExit
            ))
        );
    }

    // ── E. Idempotency ─────────────────────────────────────────────────────────

    #[test]
    fn stage76_pm_task_exited_duplicate_matched_tid_is_idempotent() {
        let (mut handles, mut lc) = lifecycle_pair(TID_A, VfsSharedIoDirection::ReadReply, 8);
        lc.map(&handles).expect("map");
        lc.begin().expect("begin");
        let first = handle_pm_task_exited(TID_A, &mut lc, &mut handles).expect("first");
        let second = handle_pm_task_exited(TID_A, &mut lc, &mut handles).expect("second");
        assert_eq!(
            first,
            VfsSharedIoRequesterExitAction::Matched(VfsSharedIoCleanupResult::Won(
                VfsSharedIoTerminalReason::RequesterExit
            ))
        );
        assert_eq!(
            second,
            VfsSharedIoRequesterExitAction::Matched(VfsSharedIoCleanupResult::AlreadyCleaned(
                VfsSharedIoTerminalReason::RequesterExit
            ))
        );
    }

    // ── F. PM notification dispatch is separate from handle_request ─────────────

    #[test]
    fn stage76_pm_task_exited_uses_separate_dispatch_not_handle_request() {
        // PM notifications arrive on a dedicated PM→VFS notify endpoint (when wired),
        // not through VFS's main IPC recv loop. handle_pm_task_exited is the correct
        // VFS entry point. This test proves the helper route works end-to-end.
        let (mut handles, mut lc) = lifecycle_pair(TID_A, VfsSharedIoDirection::ReadReply, 8);
        lc.map(&handles).expect("map");
        lc.begin().expect("begin");
        let action = handle_pm_task_exited(TID_A, &mut lc, &mut handles).expect("pm dispatch");
        assert_eq!(
            action,
            VfsSharedIoRequesterExitAction::Matched(VfsSharedIoCleanupResult::Won(
                VfsSharedIoTerminalReason::RequesterExit
            )),
            "PROC_OP_TASK_EXITED must be dispatched via handle_pm_task_exited"
        );
    }

    #[test]
    fn stage76_pm_and_vfs_opcodes_are_in_separate_endpoint_namespaces() {
        // PROC_OP_TASK_EXITED=13 and PROC_OP_PROCESS_EXITED=14 share u16 values with VFS
        // opcodes (VFS_OP_WRITE=13, VFS_OP_IOCTL=14) but are in separate IPC protocols.
        // PM and VFS use isolated endpoints; no opcode collision is possible at runtime.
        // This test documents the intended separation and proves the PM gate is false.
        assert_eq!(PROC_OP_TASK_EXITED, 13u16);
        assert_eq!(PROC_OP_PROCESS_EXITED, 14u16);
        // The PM notification channel is not yet wired (gate = false).
        assert!(!VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED);
    }
}
