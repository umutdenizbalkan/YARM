// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::vfs_abi::{
    OpenAtInlinePath, ReadWriteArgs, StatxInlinePath, VFS_OP_CLOSE, VFS_OP_DUP,
    VFS_OP_EPOLL_CREATE1, VFS_OP_EPOLL_CTL, VFS_OP_EPOLL_PWAIT, VFS_OP_FCNTL, VFS_OP_IOCTL,
    VFS_OP_OPENAT, VFS_OP_POLL, VFS_OP_READ, VFS_OP_READ_SHARED_REPLY, VFS_OP_SENDFILE,
    VFS_OP_STATX, VFS_OP_WRITE, VFS_OP_WRITE_INLINE, VFS_OP_WRITE_SHARED_REQUEST,
    VfsReadSharedRequest, VfsV1Args, VfsWriteInlineReply, VfsWriteInlineRequest,
    VfsWriteSharedReply, VfsWriteSharedRequest,
};
use yarm_user_rt::ipc::Message;

pub use yarm_srv_common::vfs_core::*;

pub fn openat_inline_message(
    dirfd: u64,
    path: &[u8],
    flags: u64,
    mode: u64,
) -> Result<Message, VfsError> {
    let (payload, len) = OpenAtInlinePath {
        dirfd,
        flags,
        mode,
        path,
    }
    .encode()
    .ok_or(VfsError::NameTooLong)?;
    Message::with_header(0, VFS_OP_OPENAT, 0, None, &payload[..len])
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsWritePayload<'a> {
    Inline(VfsWriteInlineRequest<'a>),
    Shared(VfsWriteSharedRequest),
}

impl<'a> VfsWritePayload<'a> {
    pub fn decode(opcode: u16, payload: &'a [u8]) -> Result<Self, VfsError> {
        match opcode {
            VFS_OP_WRITE_INLINE => VfsWriteInlineRequest::decode(payload)
                .map(Self::Inline)
                .map_err(|_| VfsError::Malformed),
            VFS_OP_WRITE_SHARED_REQUEST => VfsWriteSharedRequest::decode(payload)
                .map(Self::Shared)
                .map_err(|_| VfsError::Malformed),
            _ => Err(VfsError::Unsupported),
        }
    }

    /// Returns exact inline bytes. Shared payloads remain unavailable until mapping exists.
    pub fn inline_parts(self) -> Result<(u64, u64, u32, &'a [u8]), VfsError> {
        match self {
            Self::Inline(request) => Ok((
                request.fd,
                request.file_offset,
                request.flags,
                request.bytes,
            )),
            Self::Shared(_) => Err(VfsError::Unsupported),
        }
    }
}

/// Helper-only exact-byte inline write message. Live VFS dispatch does not handle opcode 28.
pub fn write_inline_message(req: VfsWriteInlineRequest<'_>) -> Result<Message, VfsError> {
    let (payload, len) = req.encode().map_err(|_| VfsError::Malformed)?;
    Message::with_header(0, VFS_OP_WRITE_INLINE, 0, None, &payload[..len])
        .map_err(|_| VfsError::Malformed)
}

pub fn write_inline_reply_message(reply: VfsWriteInlineReply) -> Result<Message, VfsError> {
    let payload = reply.encode().map_err(|_| VfsError::Malformed)?;
    Message::with_header(0, VFS_OP_WRITE_INLINE, 0, None, &payload).map_err(|_| VfsError::Malformed)
}

pub fn write_shared_reply_message(reply: VfsWriteSharedReply) -> Result<Message, VfsError> {
    let payload = reply.encode().map_err(|_| VfsError::Malformed)?;
    Message::with_header(0, VFS_OP_WRITE_SHARED_REQUEST, 0, None, &payload)
        .map_err(|_| VfsError::Malformed)
}

/// Helper-only encoder for the reserved READ_SHARED_REPLY service request.
/// The live VFS service intentionally does not dispatch this opcode in FS-11.
pub fn read_shared_message(req: VfsReadSharedRequest) -> Result<Message, VfsError> {
    let payload = req.encode().map_err(|_| VfsError::Malformed)?;
    Message::with_header(0, VFS_OP_READ_SHARED_REPLY, 0, None, &payload)
        .map_err(|_| VfsError::Malformed)
}

/// Helper-only encoder for the reserved WRITE_SHARED_REQUEST service request.
/// The live VFS service intentionally does not dispatch this opcode in FS-11.
pub fn write_shared_message(req: VfsWriteSharedRequest) -> Result<Message, VfsError> {
    let payload = req.encode().map_err(|_| VfsError::Malformed)?;
    Message::with_header(0, VFS_OP_WRITE_SHARED_REQUEST, 0, None, &payload)
        .map_err(|_| VfsError::Malformed)
}

pub fn statx_inline_message(
    dirfd: u64,
    path: &[u8],
    flags: u64,
    mask_or_buf: u64,
) -> Result<Message, VfsError> {
    let (payload, len) = StatxInlinePath {
        dirfd,
        flags,
        mask_or_buf,
        path,
    }
    .encode()
    .ok_or(VfsError::NameTooLong)?;
    Message::with_header(0, VFS_OP_STATX, 0, None, &payload[..len]).map_err(|_| VfsError::Malformed)
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
mod shared_io_tests {
    use super::*;
    use yarm_ipc_abi::vfs_abi::{
        VFS_SHARED_BUFFER_FS_READ, VFS_SHARED_BUFFER_FS_WRITE, VfsSharedBufferDescriptor,
    };

    #[test]
    fn shared_message_helpers_encode_without_enabling_dispatch() {
        let read = VfsReadSharedRequest {
            fd: 3,
            file_offset: 0,
            requested_len: 64,
            request_id: 11,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(5, 1, 0, 64, VFS_SHARED_BUFFER_FS_WRITE),
        };
        let message = read_shared_message(read).expect("read shared message");
        assert_eq!(message.opcode, VFS_OP_READ_SHARED_REPLY);
        assert_eq!(VfsReadSharedRequest::decode(message.as_slice()), Ok(read));

        let write = VfsWriteSharedRequest {
            fd: 4,
            file_offset: 128,
            requested_len: 32,
            request_id: 12,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(6, 2, 8, 32, VFS_SHARED_BUFFER_FS_READ),
        };
        let message = write_shared_message(write).expect("write shared message");
        assert_eq!(message.opcode, VFS_OP_WRITE_SHARED_REQUEST);
        assert_eq!(VfsWriteSharedRequest::decode(message.as_slice()), Ok(write));
    }

    #[test]
    fn legacy_inline_read_write_messages_are_unchanged() {
        let request = ReadWriteRequest {
            fd: 7,
            buf_ptr: 8,
            len: 9,
        };
        let read = read_message(request).expect("legacy read");
        let write = write_message(request).expect("legacy write");
        assert_eq!(read.opcode, VFS_OP_READ);
        assert_eq!(write.opcode, VFS_OP_WRITE);
        assert_eq!(
            ReadWriteArgs::decode(read.as_slice()),
            Ok(ReadWriteArgs::new(7, 8, 9))
        );
        assert_eq!(
            ReadWriteArgs::decode(write.as_slice()),
            Ok(ReadWriteArgs::new(7, 8, 9))
        );
    }

    #[test]
    fn inline_write_payload_helper_preserves_exact_bytes_and_replies() {
        let request = VfsWriteInlineRequest {
            fd: 9,
            file_offset: 0,
            request_id: 77,
            flags: 0,
            bytes: b"payload bytes",
        };
        let message = write_inline_message(request).expect("inline message");
        assert_eq!(message.opcode, VFS_OP_WRITE_INLINE);
        let decoded = VfsWritePayload::decode(message.opcode, message.as_slice()).expect("decode");
        assert_eq!(
            decoded.inline_parts(),
            Ok((9, 0, 0, b"payload bytes".as_slice()))
        );

        let reply = VfsWriteInlineReply {
            request_id: 77,
            bytes_completed: 13,
            status: 0,
            flags: 0,
        };
        let message = write_inline_reply_message(reply).expect("inline reply");
        assert_eq!(VfsWriteInlineReply::decode(message.as_slice()), Ok(reply));
    }

    #[test]
    fn shared_write_payload_decodes_but_cannot_expose_bytes_without_mapping() {
        let request = VfsWriteSharedRequest {
            fd: 4,
            file_offset: 0,
            requested_len: 32,
            request_id: 12,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(6, 2, 8, 32, VFS_SHARED_BUFFER_FS_READ),
        };
        let message = write_shared_message(request).expect("shared write message");
        let decoded = VfsWritePayload::decode(message.opcode, message.as_slice()).expect("decode");
        assert_eq!(decoded, VfsWritePayload::Shared(request));
        assert_eq!(decoded.inline_parts(), Err(VfsError::Unsupported));

        let reply = VfsWriteSharedReply {
            request_id: request.request_id,
            bytes_completed: 0,
            status: 5,
            flags: 0,
        };
        let message = write_shared_reply_message(reply).expect("shared reply");
        assert_eq!(VfsWriteSharedReply::decode(message.as_slice()), Ok(reply));
    }
}
