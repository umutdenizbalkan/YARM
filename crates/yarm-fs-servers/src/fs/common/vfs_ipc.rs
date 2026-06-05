// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::vfs_abi::{
    OpenAtInlinePath, ReadWriteArgs, StatxInlinePath, VfsReadSharedRequest, VfsV1Args,
    VfsWriteSharedRequest, VFS_OP_CLOSE, VFS_OP_DUP, VFS_OP_EPOLL_CREATE1, VFS_OP_EPOLL_CTL,
    VFS_OP_EPOLL_PWAIT, VFS_OP_FCNTL, VFS_OP_IOCTL, VFS_OP_OPENAT, VFS_OP_POLL, VFS_OP_READ,
    VFS_OP_READ_SHARED_REPLY, VFS_OP_SENDFILE, VFS_OP_STATX, VFS_OP_WRITE,
    VFS_OP_WRITE_SHARED_REQUEST,
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
        VfsSharedBufferDescriptor, VFS_SHARED_BUFFER_FS_READ, VFS_SHARED_BUFFER_FS_WRITE,
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
}
