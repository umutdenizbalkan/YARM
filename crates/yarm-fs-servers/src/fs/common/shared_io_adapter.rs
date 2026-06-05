// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Adapter boundary between validated VFS shared-I/O lifecycle requests and a future userspace
//! object transfer/mapping implementation.
//!
//! YARM currently has no general userspace primitive that resolves `VfsSharedBufferDescriptor` into
//! a mapped range with direction-specific rights. Implementations must not interpret the opaque
//! handle as a kernel capability slot. The default adapter therefore reports unsupported mapping.

use super::shared_io_lifecycle::{
    VfsSharedIoDirection, VfsSharedIoHandleTable, VfsSharedIoLifecycle, VfsSharedIoLifecycleError,
};
use yarm_ipc_abi::vfs_abi::VfsSharedBufferDescriptor;

#[cfg(test)]
use yarm_ipc_abi::vfs_abi::{VFS_SHARED_BUFFER_FS_READ, VFS_SHARED_BUFFER_FS_WRITE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsSharedIoAdapterError {
    Lifecycle(VfsSharedIoLifecycleError),
    UnsupportedMapping,
    StaleHandle,
    BadRange,
    WrongDirection,
    AccessAfterCleanup,
}

impl From<VfsSharedIoLifecycleError> for VfsSharedIoAdapterError {
    fn from(value: VfsSharedIoLifecycleError) -> Self {
        Self::Lifecycle(value)
    }
}

/// Direction-safe mapping boundary. A write request can only expose an immutable slice.
pub trait VfsSharedIoMapper {
    fn with_read_reply_buffer<R>(
        &mut self,
        descriptor: VfsSharedBufferDescriptor,
        requested_len: u64,
        operation: impl FnOnce(&mut [u8]) -> R,
    ) -> Result<R, VfsSharedIoAdapterError>;

    fn with_write_request_buffer<R>(
        &mut self,
        descriptor: VfsSharedBufferDescriptor,
        requested_len: u64,
        operation: impl FnOnce(&[u8]) -> R,
    ) -> Result<R, VfsSharedIoAdapterError>;

    fn release(
        &mut self,
        descriptor: VfsSharedBufferDescriptor,
    ) -> Result<(), VfsSharedIoAdapterError>;
}

/// Production-safe placeholder until a real userspace transfer/mapping primitive exists.
pub struct UnsupportedSharedIoMapper;

impl VfsSharedIoMapper for UnsupportedSharedIoMapper {
    fn with_read_reply_buffer<R>(
        &mut self,
        _descriptor: VfsSharedBufferDescriptor,
        _requested_len: u64,
        _operation: impl FnOnce(&mut [u8]) -> R,
    ) -> Result<R, VfsSharedIoAdapterError> {
        Err(VfsSharedIoAdapterError::UnsupportedMapping)
    }

    fn with_write_request_buffer<R>(
        &mut self,
        _descriptor: VfsSharedBufferDescriptor,
        _requested_len: u64,
        _operation: impl FnOnce(&[u8]) -> R,
    ) -> Result<R, VfsSharedIoAdapterError> {
        Err(VfsSharedIoAdapterError::UnsupportedMapping)
    }

    fn release(
        &mut self,
        _descriptor: VfsSharedBufferDescriptor,
    ) -> Result<(), VfsSharedIoAdapterError> {
        Err(VfsSharedIoAdapterError::UnsupportedMapping)
    }
}

pub fn with_read_reply_buffer<const N: usize, M: VfsSharedIoMapper, R>(
    lifecycle: &VfsSharedIoLifecycle,
    handles: &VfsSharedIoHandleTable<N>,
    mapper: &mut M,
    operation: impl FnOnce(&mut [u8]) -> R,
) -> Result<R, VfsSharedIoAdapterError> {
    lifecycle.authorize_access(handles, VfsSharedIoDirection::ReadReply)?;
    mapper.with_read_reply_buffer(lifecycle.descriptor(), lifecycle.requested_len(), operation)
}

pub fn with_write_request_buffer<const N: usize, M: VfsSharedIoMapper, R>(
    lifecycle: &VfsSharedIoLifecycle,
    handles: &VfsSharedIoHandleTable<N>,
    mapper: &mut M,
    operation: impl FnOnce(&[u8]) -> R,
) -> Result<R, VfsSharedIoAdapterError> {
    lifecycle.authorize_access(handles, VfsSharedIoDirection::WriteRequest)?;
    mapper.with_write_request_buffer(lifecycle.descriptor(), lifecycle.requested_len(), operation)
}

pub fn cleanup_shared_io<const N: usize, M: VfsSharedIoMapper>(
    lifecycle: &mut VfsSharedIoLifecycle,
    handles: &mut VfsSharedIoHandleTable<N>,
    mapper: &mut M,
    reason: super::shared_io_lifecycle::VfsSharedIoTerminalReason,
) -> Result<super::shared_io_lifecycle::VfsSharedIoCleanupResult, VfsSharedIoAdapterError> {
    if lifecycle.state() != super::shared_io_lifecycle::VfsSharedIoState::Cleaned {
        mapper.release(lifecycle.descriptor())?;
    }
    lifecycle.cleanup(handles, reason).map_err(Into::into)
}

#[cfg(test)]
pub struct BorrowedSharedIoTestMapper<'a> {
    object_handle: u64,
    object_generation: u64,
    bytes: &'a mut [u8],
    released: bool,
    release_count: usize,
}

#[cfg(test)]
impl<'a> BorrowedSharedIoTestMapper<'a> {
    pub fn new(object_handle: u64, object_generation: u64, bytes: &'a mut [u8]) -> Self {
        Self {
            object_handle,
            object_generation,
            bytes,
            released: false,
            release_count: 0,
        }
    }

    pub const fn release_count(&self) -> usize {
        self.release_count
    }

    fn range(
        &self,
        descriptor: VfsSharedBufferDescriptor,
        requested_len: u64,
        expected_access: u32,
    ) -> Result<core::ops::Range<usize>, VfsSharedIoAdapterError> {
        if self.released {
            return Err(VfsSharedIoAdapterError::AccessAfterCleanup);
        }
        if descriptor.object_handle != self.object_handle
            || descriptor.object_generation != self.object_generation
        {
            return Err(VfsSharedIoAdapterError::StaleHandle);
        }
        if descriptor.access != expected_access {
            return Err(VfsSharedIoAdapterError::WrongDirection);
        }
        descriptor
            .validate(expected_access, requested_len)
            .map_err(|_| VfsSharedIoAdapterError::BadRange)?;
        let start = usize::try_from(descriptor.buffer_offset)
            .map_err(|_| VfsSharedIoAdapterError::BadRange)?;
        let len = usize::try_from(requested_len).map_err(|_| VfsSharedIoAdapterError::BadRange)?;
        let end = start
            .checked_add(len)
            .ok_or(VfsSharedIoAdapterError::BadRange)?;
        if end > self.bytes.len() {
            return Err(VfsSharedIoAdapterError::BadRange);
        }
        Ok(start..end)
    }
}

#[cfg(test)]
impl VfsSharedIoMapper for BorrowedSharedIoTestMapper<'_> {
    fn with_read_reply_buffer<R>(
        &mut self,
        descriptor: VfsSharedBufferDescriptor,
        requested_len: u64,
        operation: impl FnOnce(&mut [u8]) -> R,
    ) -> Result<R, VfsSharedIoAdapterError> {
        let range = self.range(descriptor, requested_len, VFS_SHARED_BUFFER_FS_WRITE)?;
        Ok(operation(&mut self.bytes[range]))
    }

    fn with_write_request_buffer<R>(
        &mut self,
        descriptor: VfsSharedBufferDescriptor,
        requested_len: u64,
        operation: impl FnOnce(&[u8]) -> R,
    ) -> Result<R, VfsSharedIoAdapterError> {
        let range = self.range(descriptor, requested_len, VFS_SHARED_BUFFER_FS_READ)?;
        Ok(operation(&self.bytes[range]))
    }

    fn release(
        &mut self,
        descriptor: VfsSharedBufferDescriptor,
    ) -> Result<(), VfsSharedIoAdapterError> {
        if descriptor.object_handle != self.object_handle
            || descriptor.object_generation != self.object_generation
        {
            return Err(VfsSharedIoAdapterError::StaleHandle);
        }
        if !self.released {
            self.released = true;
            self.release_count += 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::common::shared_io_lifecycle::{
        VfsSharedIoCleanupResult, VfsSharedIoTerminalReason,
    };
    use crate::fs::ramfs::tree::RamFsBackend;
    use yarm_ipc_abi::vfs_abi::VFS_SHARED_IO_F_ALLOW_INLINE_FALLBACK;

    fn lifecycle<const N: usize>(
        handles: &mut VfsSharedIoHandleTable<N>,
        direction: VfsSharedIoDirection,
        len: u64,
        offset: u64,
        flags: u32,
    ) -> VfsSharedIoLifecycle {
        let handle = handles.allocate().expect("allocate");
        let access = match direction {
            VfsSharedIoDirection::ReadReply => VFS_SHARED_BUFFER_FS_WRITE,
            VfsSharedIoDirection::WriteRequest => VFS_SHARED_BUFFER_FS_READ,
        };
        VfsSharedIoLifecycle::reserve(
            1,
            VfsSharedBufferDescriptor::new(
                handle.object_handle,
                handle.object_generation,
                offset,
                len,
                access,
            ),
            len,
            flags,
            direction,
        )
        .expect("reserve")
    }

    #[test]
    fn adapter_read_reply_mutable_access_then_cleanup_revokes_access() {
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let mut lifecycle = lifecycle(&mut handles, VfsSharedIoDirection::ReadReply, 4, 2, 0);
        lifecycle.map(&handles).expect("map lifecycle");
        lifecycle.begin().expect("begin");
        let descriptor = lifecycle.descriptor();
        let mut bytes = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(
            descriptor.object_handle,
            descriptor.object_generation,
            &mut bytes,
        );
        let mut ramfs = RamFsBackend::new();
        ramfs.create_file(b"/ram/read-adapter").expect("create");
        let seed_fd = ramfs.open_path(b"/ram/read-adapter").expect("open seed");
        ramfs.write_bytes(seed_fd, b"read").expect("seed");
        ramfs.close_fd(seed_fd).expect("close seed");
        let read_fd = ramfs.open_path(b"/ram/read-adapter").expect("open read");
        let read = with_read_reply_buffer(&lifecycle, &handles, &mut mapper, |out| {
            ramfs.read_bytes(read_fd, out)
        })
        .expect("mapped write")
        .expect("RAMFS read");
        assert_eq!(read, 4);
        lifecycle.complete(read as u64).expect("complete");
        cleanup_shared_io(
            &mut lifecycle,
            &mut handles,
            &mut mapper,
            VfsSharedIoTerminalReason::Success,
        )
        .expect("cleanup");
        assert_eq!(
            with_read_reply_buffer(&lifecycle, &handles, &mut mapper, |_| ()),
            Err(VfsSharedIoAdapterError::Lifecycle(
                VfsSharedIoLifecycleError::AccessAfterCleanup
            ))
        );
        assert_eq!(mapper.release_count(), 1);
        assert_eq!(
            cleanup_shared_io(
                &mut lifecycle,
                &mut handles,
                &mut mapper,
                VfsSharedIoTerminalReason::BackendError,
            ),
            Ok(VfsSharedIoCleanupResult::AlreadyCleaned(
                VfsSharedIoTerminalReason::Success
            ))
        );
        assert_eq!(mapper.release_count(), 1);
        drop(mapper);
        assert_eq!(&bytes[2..6], b"read");
    }

    #[test]
    fn adapter_write_request_is_immutable_and_ramfs_consumes_exact_bytes() {
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let mut lifecycle = lifecycle(&mut handles, VfsSharedIoDirection::WriteRequest, 12, 2, 0);
        lifecycle.map(&handles).expect("map lifecycle");
        lifecycle.begin().expect("begin");
        let descriptor = lifecycle.descriptor();
        let mut bytes = *b"xxshared bytesyy";
        let mut mapper = BorrowedSharedIoTestMapper::new(
            descriptor.object_handle,
            descriptor.object_generation,
            &mut bytes,
        );
        let mut ramfs = RamFsBackend::new();
        ramfs.create_file(b"/ram/adapter").expect("create");
        let fd = ramfs.open_path(b"/ram/adapter").expect("open");
        let written = with_write_request_buffer(&lifecycle, &handles, &mut mapper, |input| {
            ramfs.write_bytes(fd, input)
        })
        .expect("mapped read")
        .expect("RAMFS write");
        assert_eq!(written, 12);
        lifecycle.complete(written as u64).expect("complete");
        cleanup_shared_io(
            &mut lifecycle,
            &mut handles,
            &mut mapper,
            VfsSharedIoTerminalReason::Success,
        )
        .expect("cleanup");
        ramfs.close_fd(fd).expect("close");
        let fd = ramfs.open_path(b"/ram/adapter").expect("reopen");
        let mut out = [0u8; 16];
        let read = ramfs.read_bytes(fd, &mut out).expect("read");
        assert_eq!(&out[..read], b"shared bytes");
    }

    #[test]
    fn adapter_rejects_wrong_direction_stale_generation_and_bad_range() {
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let mut lifecycle = lifecycle(&mut handles, VfsSharedIoDirection::ReadReply, 4, 6, 0);
        lifecycle.map(&handles).expect("map lifecycle");
        lifecycle.begin().expect("begin");
        let descriptor = lifecycle.descriptor();
        let mut bytes = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(
            descriptor.object_handle,
            descriptor.object_generation,
            &mut bytes,
        );
        assert_eq!(
            with_write_request_buffer(&lifecycle, &handles, &mut mapper, |_| ()),
            Err(VfsSharedIoAdapterError::Lifecycle(
                VfsSharedIoLifecycleError::InvalidState
            ))
        );
        assert_eq!(
            with_read_reply_buffer(&lifecycle, &handles, &mut mapper, |_| ()),
            Err(VfsSharedIoAdapterError::BadRange)
        );

        let mut stale_bytes = [0u8; 8];
        let mut stale = BorrowedSharedIoTestMapper::new(
            descriptor.object_handle,
            descriptor.object_generation + 1,
            &mut stale_bytes,
        );
        assert_eq!(
            with_read_reply_buffer(&lifecycle, &handles, &mut stale, |_| ()),
            Err(VfsSharedIoAdapterError::StaleHandle)
        );
    }

    #[test]
    fn unsupported_mapper_and_timeout_fallback_are_explicit() {
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let mut lifecycle = lifecycle(
            &mut handles,
            VfsSharedIoDirection::ReadReply,
            4,
            0,
            VFS_SHARED_IO_F_ALLOW_INLINE_FALLBACK,
        );
        lifecycle.map(&handles).expect("map lifecycle");
        lifecycle.begin().expect("begin");
        assert_eq!(
            with_read_reply_buffer(&lifecycle, &handles, &mut UnsupportedSharedIoMapper, |_| ()),
            Err(VfsSharedIoAdapterError::UnsupportedMapping)
        );
        assert_eq!(
            lifecycle.cleanup(&mut handles, VfsSharedIoTerminalReason::Timeout),
            Ok(VfsSharedIoCleanupResult::Won(
                VfsSharedIoTerminalReason::Timeout
            ))
        );
        assert_eq!(lifecycle.begin_inline_fallback(), Ok(()));
        assert_eq!(
            lifecycle.cleanup(&mut handles, VfsSharedIoTerminalReason::BackendError),
            Ok(VfsSharedIoCleanupResult::AlreadyCleaned(
                VfsSharedIoTerminalReason::Timeout
            ))
        );
    }
}
