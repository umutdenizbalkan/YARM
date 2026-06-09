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
        assert!(
            !VFS_READ_SHARED_REPLY_ENABLED,
            "VFS_READ_SHARED_REPLY_ENABLED must be false by default"
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
    // MAP_WRITE | MAP_READ — simulated; kernel gate still rejects actual map_intent=3.
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
        // Stage 60 kernel MAP_WRITE gate: hard-rejects map_intent & 0x2 != 0.
        // We prove the binding requires perm & 0x2 != 0, confirming no live kernel delivery
        // can reach this path while the gate is intact.
        use crate::fs::common::shared_io_adapter::{
            VfsReadSharedBinding, VfsReadSharedBindingError,
        };
        let req = read_request(1, 8);
        // actual_mapping_perm = 1 (MAP_READ only) — no WRITE bit → MappingNotWritable
        let result = VfsReadSharedBinding::validate(
            TOKEN, CAP, KIND_DMA, REGION_LEN, MAPPED_BASE, MAPPED_LEN,
            1, // MAP_READ only; kernel gate blocks perm=3 from ever arriving
            &req,
        );
        assert_eq!(
            result.err(),
            Some(VfsReadSharedBindingError::MappingNotWritable),
            "binding must reject MAP_READ-only perm — kernel gate means perm=3 never arrives"
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
        assert!(!VFS_WRITE_SHARED_REQUEST_ENABLED);
        assert!(!VFS_READ_SHARED_REPLY_ENABLED);
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
