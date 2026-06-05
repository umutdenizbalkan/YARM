// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::vfs_abi::{
    OpenAtInlinePath, ReadWriteArgs, StatxInlinePath, VFS_OP_CLOSE, VFS_OP_DUP,
    VFS_OP_EPOLL_CREATE1, VFS_OP_EPOLL_CTL, VFS_OP_EPOLL_PWAIT, VFS_OP_FCNTL, VFS_OP_IOCTL,
    VFS_OP_OPENAT, VFS_OP_POLL, VFS_OP_READ, VFS_OP_READ_SHARED_REPLY, VFS_OP_SENDFILE,
    VFS_OP_STATX, VFS_OP_WRITE, VFS_OP_WRITE_INLINE, VFS_OP_WRITE_SHARED_REQUEST,
    VFS_SHARED_IO_STATUS_OK, VfsReadSharedReply, VfsReadSharedRequest, VfsV1Args,
    VfsWriteInlineReply, VfsWriteInlineRequest, VfsWriteSharedReply, VfsWriteSharedRequest,
};
use yarm_user_rt::ipc::Message;

#[cfg(test)]
use yarm_ipc_abi::vfs_abi::{
    VFS_SHARED_BUFFER_FS_READ, VFS_SHARED_BUFFER_FS_WRITE, VfsSharedBufferDescriptor,
};

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

/// Helper-only decoded READ_SHARED_REPLY plan. It describes future mapping work but does not map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VfsReadSharedPlan {
    request: VfsReadSharedRequest,
}

impl VfsReadSharedPlan {
    pub fn decode(opcode: u16, payload: &[u8]) -> Result<Self, VfsError> {
        if opcode != VFS_OP_READ_SHARED_REPLY {
            return Err(VfsError::Unsupported);
        }
        let request = VfsReadSharedRequest::decode(payload).map_err(|_| VfsError::Malformed)?;
        Ok(Self { request })
    }

    pub const fn request(self) -> VfsReadSharedRequest {
        self.request
    }

    pub fn completion(self, bytes_read: usize) -> Result<VfsReadSharedReply, VfsError> {
        let bytes_read = u64::try_from(bytes_read).map_err(|_| VfsError::Malformed)?;
        if bytes_read > self.request.requested_len {
            return Err(VfsError::Malformed);
        }
        Ok(VfsReadSharedReply {
            request_id: self.request.request_id,
            bytes_completed: bytes_read,
            status: VFS_SHARED_IO_STATUS_OK,
            flags: 0,
        })
    }
}

/// Borrowed test double for a future shared-object mapping.
///
/// This type is test-only: it validates descriptor identity, direction, and actual backing bounds,
/// but it is not a capability, mapping, revocation, or cleanup implementation.
#[cfg(test)]
pub struct VfsSharedIoTestBuffer<'a> {
    object_handle: u64,
    object_generation: u64,
    bytes: &'a mut [u8],
}

#[cfg(test)]
impl<'a> VfsSharedIoTestBuffer<'a> {
    pub fn new(object_handle: u64, object_generation: u64, bytes: &'a mut [u8]) -> Self {
        Self {
            object_handle,
            object_generation,
            bytes,
        }
    }

    fn range(
        &self,
        descriptor: VfsSharedBufferDescriptor,
        requested_len: u64,
        required_access: u32,
    ) -> Result<core::ops::Range<usize>, VfsError> {
        descriptor
            .validate(required_access, requested_len)
            .map_err(|_| VfsError::Malformed)?;
        if descriptor.object_handle != self.object_handle
            || descriptor.object_generation != self.object_generation
        {
            return Err(VfsError::Malformed);
        }
        let start = usize::try_from(descriptor.buffer_offset).map_err(|_| VfsError::Malformed)?;
        let length = usize::try_from(requested_len).map_err(|_| VfsError::Malformed)?;
        let end = start.checked_add(length).ok_or(VfsError::Malformed)?;
        if end > self.bytes.len() {
            return Err(VfsError::Malformed);
        }
        Ok(start..end)
    }

    pub fn write_read_reply(
        &mut self,
        request: VfsReadSharedRequest,
        source: &[u8],
    ) -> Result<usize, VfsError> {
        let range = self.range(
            request.buffer,
            request.requested_len,
            VFS_SHARED_BUFFER_FS_WRITE,
        )?;
        if source.len() > range.len() {
            return Err(VfsError::Malformed);
        }
        let end = range.start + source.len();
        self.bytes[range.start..end].copy_from_slice(source);
        Ok(source.len())
    }

    pub fn read_write_request(&self, request: VfsWriteSharedRequest) -> Result<&[u8], VfsError> {
        let range = self.range(
            request.buffer,
            request.requested_len,
            VFS_SHARED_BUFFER_FS_READ,
        )?;
        Ok(&self.bytes[range])
    }

    pub fn read_for_assert(&self, offset: usize, len: usize) -> Result<&[u8], VfsError> {
        let end = offset.checked_add(len).ok_or(VfsError::Malformed)?;
        self.bytes.get(offset..end).ok_or(VfsError::Malformed)
    }
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

pub fn read_shared_reply_message(reply: VfsReadSharedReply) -> Result<Message, VfsError> {
    let payload = reply.encode().map_err(|_| VfsError::Malformed)?;
    Message::with_header(0, VFS_OP_READ_SHARED_REPLY, 0, None, &payload)
        .map_err(|_| VfsError::Malformed)
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
    fn read_shared_plan_completes_and_test_buffer_enforces_direction() {
        let request = VfsReadSharedRequest {
            fd: 3,
            file_offset: 0,
            requested_len: 8,
            request_id: 91,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(21, 4, 2, 8, VFS_SHARED_BUFFER_FS_WRITE),
        };
        let message = read_shared_message(request).expect("shared read message");
        let plan = VfsReadSharedPlan::decode(message.opcode, message.as_slice()).expect("plan");
        assert_eq!(plan.request(), request);

        let mut storage = [0u8; 16];
        let mut buffer = VfsSharedIoTestBuffer::new(21, 4, &mut storage);
        assert_eq!(buffer.write_read_reply(request, b"read"), Ok(4));
        assert_eq!(buffer.read_for_assert(2, 4), Ok(b"read".as_slice()));

        let reply = plan.completion(4).expect("completion");
        let message = read_shared_reply_message(reply).expect("reply message");
        assert_eq!(VfsReadSharedReply::decode(message.as_slice()), Ok(reply));
        assert_eq!(plan.completion(9), Err(VfsError::Malformed));

        let mut wrong = request;
        wrong.buffer.access = VFS_SHARED_BUFFER_FS_READ;
        assert_eq!(
            buffer.write_read_reply(wrong, b"read"),
            Err(VfsError::Malformed)
        );
    }

    #[test]
    fn test_buffer_checks_identity_actual_bounds_and_write_read_only_access() {
        let mut storage = *b"0123456789abcdef";
        let buffer = VfsSharedIoTestBuffer::new(30, 7, &mut storage);
        let request = VfsWriteSharedRequest {
            fd: 4,
            file_offset: 0,
            requested_len: 5,
            request_id: 92,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(30, 7, 3, 5, VFS_SHARED_BUFFER_FS_READ),
        };
        assert_eq!(buffer.read_write_request(request), Ok(b"34567".as_slice()));

        let mut stale = request;
        stale.buffer.object_generation = 8;
        assert_eq!(buffer.read_write_request(stale), Err(VfsError::Malformed));

        let mut outside = request;
        outside.buffer.buffer_offset = 14;
        outside.buffer.buffer_len = 5;
        assert_eq!(buffer.read_write_request(outside), Err(VfsError::Malformed));

        let mut writable = request;
        writable.buffer.access = VFS_SHARED_BUFFER_FS_WRITE;
        assert_eq!(
            buffer.read_write_request(writable),
            Err(VfsError::Malformed)
        );
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
