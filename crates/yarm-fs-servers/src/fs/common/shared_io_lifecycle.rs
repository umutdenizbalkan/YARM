// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Helper-only lifecycle model for future VFS shared I/O.
//!
//! This module models handle generations, request state transitions, and exactly-once cleanup. It
//! does not transfer capabilities, map memory, observe process exits, or participate in live VFS
//! dispatch.

use yarm_ipc_abi::vfs_abi::{
    VFS_SHARED_BUFFER_FS_READ, VFS_SHARED_BUFFER_FS_WRITE, VFS_SHARED_IO_F_ALLOW_INLINE_FALLBACK,
    VfsSharedBufferDescriptor,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsSharedIoDirection {
    ReadReply,
    WriteRequest,
}

impl VfsSharedIoDirection {
    const fn required_access(self) -> u32 {
        match self {
            Self::ReadReply => VFS_SHARED_BUFFER_FS_WRITE,
            Self::WriteRequest => VFS_SHARED_BUFFER_FS_READ,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsSharedIoState {
    Reserved,
    MappedForReadReply,
    MappedForWriteRequest,
    InFlight,
    Completed,
    Cleaned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsSharedIoTerminalReason {
    Success,
    BackendError,
    Unsupported,
    Cancelled,
    Timeout,
    RequesterExit,
    ServerExit,
    StaleHandle,
    BadDescriptor,
    DuplicateReply,
    FallbackInline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsSharedIoLifecycleError {
    Capacity,
    StaleHandle,
    BadDescriptor,
    InvalidState,
    DuplicateReply,
    CompletionTooLarge,
    AccessAfterCleanup,
    FallbackBeforeCleanup,
    FallbackNotAllowed,
    FallbackAlreadyStarted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VfsSharedIoHandle {
    pub object_handle: u64,
    pub object_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HandleSlot {
    generation: u64,
    active: bool,
}

impl HandleSlot {
    const EMPTY: Self = Self {
        generation: 1,
        active: false,
    };
}

/// Fixed-capacity helper handle table. Handles are one-based slot indexes and generations advance
/// whenever cleanup releases a slot.
pub struct VfsSharedIoHandleTable<const N: usize> {
    slots: [HandleSlot; N],
}

impl<const N: usize> VfsSharedIoHandleTable<N> {
    pub const fn new() -> Self {
        Self {
            slots: [HandleSlot::EMPTY; N],
        }
    }

    pub fn allocate(&mut self) -> Result<VfsSharedIoHandle, VfsSharedIoLifecycleError> {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if !slot.active {
                slot.active = true;
                return Ok(VfsSharedIoHandle {
                    object_handle: (index as u64) + 1,
                    object_generation: slot.generation,
                });
            }
        }
        Err(VfsSharedIoLifecycleError::Capacity)
    }

    pub fn validate(
        &self,
        descriptor: VfsSharedBufferDescriptor,
    ) -> Result<(), VfsSharedIoLifecycleError> {
        if descriptor.object_handle == 0 || descriptor.object_generation == 0 {
            return Err(VfsSharedIoLifecycleError::BadDescriptor);
        }
        let index = usize::try_from(descriptor.object_handle - 1)
            .map_err(|_| VfsSharedIoLifecycleError::StaleHandle)?;
        let slot = self
            .slots
            .get(index)
            .ok_or(VfsSharedIoLifecycleError::StaleHandle)?;
        if !slot.active || slot.generation != descriptor.object_generation {
            return Err(VfsSharedIoLifecycleError::StaleHandle);
        }
        Ok(())
    }

    fn release(
        &mut self,
        descriptor: VfsSharedBufferDescriptor,
    ) -> Result<(), VfsSharedIoLifecycleError> {
        self.validate(descriptor)?;
        let index = usize::try_from(descriptor.object_handle - 1)
            .map_err(|_| VfsSharedIoLifecycleError::StaleHandle)?;
        let slot = &mut self.slots[index];
        slot.active = false;
        slot.generation = slot.generation.wrapping_add(1);
        if slot.generation == 0 {
            slot.generation = 1;
        }
        Ok(())
    }
}

impl<const N: usize> Default for VfsSharedIoHandleTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsSharedIoCleanupResult {
    Won(VfsSharedIoTerminalReason),
    AlreadyCleaned(VfsSharedIoTerminalReason),
}

/// One helper request and its single cleanup token.
pub struct VfsSharedIoLifecycle {
    request_id: u64,
    descriptor: VfsSharedBufferDescriptor,
    requested_len: u64,
    flags: u32,
    direction: VfsSharedIoDirection,
    state: VfsSharedIoState,
    terminal_reason: Option<VfsSharedIoTerminalReason>,
    bytes_completed: u64,
    cleanup_consumed: bool,
    fallback_started: bool,
}

impl VfsSharedIoLifecycle {
    pub fn reserve(
        request_id: u64,
        descriptor: VfsSharedBufferDescriptor,
        requested_len: u64,
        flags: u32,
        direction: VfsSharedIoDirection,
    ) -> Result<Self, VfsSharedIoLifecycleError> {
        if request_id == 0 {
            return Err(VfsSharedIoLifecycleError::BadDescriptor);
        }
        descriptor
            .validate(direction.required_access(), requested_len)
            .map_err(|_| VfsSharedIoLifecycleError::BadDescriptor)?;
        Ok(Self {
            request_id,
            descriptor,
            requested_len,
            flags,
            direction,
            state: VfsSharedIoState::Reserved,
            terminal_reason: None,
            bytes_completed: 0,
            cleanup_consumed: false,
            fallback_started: false,
        })
    }

    pub const fn request_id(&self) -> u64 {
        self.request_id
    }

    pub const fn state(&self) -> VfsSharedIoState {
        self.state
    }

    pub const fn terminal_reason(&self) -> Option<VfsSharedIoTerminalReason> {
        self.terminal_reason
    }

    pub const fn bytes_completed(&self) -> u64 {
        self.bytes_completed
    }

    pub fn map<const N: usize>(
        &mut self,
        handles: &VfsSharedIoHandleTable<N>,
    ) -> Result<(), VfsSharedIoLifecycleError> {
        if self.state != VfsSharedIoState::Reserved {
            return Err(VfsSharedIoLifecycleError::InvalidState);
        }
        handles.validate(self.descriptor)?;
        self.state = match self.direction {
            VfsSharedIoDirection::ReadReply => VfsSharedIoState::MappedForReadReply,
            VfsSharedIoDirection::WriteRequest => VfsSharedIoState::MappedForWriteRequest,
        };
        Ok(())
    }

    pub fn begin(&mut self) -> Result<(), VfsSharedIoLifecycleError> {
        match self.state {
            VfsSharedIoState::MappedForReadReply | VfsSharedIoState::MappedForWriteRequest => {
                self.state = VfsSharedIoState::InFlight;
                Ok(())
            }
            VfsSharedIoState::Cleaned => Err(VfsSharedIoLifecycleError::AccessAfterCleanup),
            _ => Err(VfsSharedIoLifecycleError::InvalidState),
        }
    }

    pub fn authorize_access<const N: usize>(
        &self,
        handles: &VfsSharedIoHandleTable<N>,
        direction: VfsSharedIoDirection,
    ) -> Result<(), VfsSharedIoLifecycleError> {
        if self.state == VfsSharedIoState::Cleaned {
            return Err(VfsSharedIoLifecycleError::AccessAfterCleanup);
        }
        if direction != self.direction || self.state != VfsSharedIoState::InFlight {
            return Err(VfsSharedIoLifecycleError::InvalidState);
        }
        handles.validate(self.descriptor)
    }

    pub fn complete(&mut self, bytes_completed: u64) -> Result<(), VfsSharedIoLifecycleError> {
        if matches!(
            self.state,
            VfsSharedIoState::Completed | VfsSharedIoState::Cleaned
        ) {
            return Err(VfsSharedIoLifecycleError::DuplicateReply);
        }
        if self.state != VfsSharedIoState::InFlight {
            return Err(VfsSharedIoLifecycleError::InvalidState);
        }
        if bytes_completed > self.requested_len {
            return Err(VfsSharedIoLifecycleError::CompletionTooLarge);
        }
        self.bytes_completed = bytes_completed;
        self.state = VfsSharedIoState::Completed;
        Ok(())
    }

    pub fn cleanup<const N: usize>(
        &mut self,
        handles: &mut VfsSharedIoHandleTable<N>,
        reason: VfsSharedIoTerminalReason,
    ) -> Result<VfsSharedIoCleanupResult, VfsSharedIoLifecycleError> {
        if self.cleanup_consumed {
            let reason = self
                .terminal_reason
                .ok_or(VfsSharedIoLifecycleError::InvalidState)?;
            return Ok(VfsSharedIoCleanupResult::AlreadyCleaned(reason));
        }
        if reason == VfsSharedIoTerminalReason::Success && self.state != VfsSharedIoState::Completed
        {
            return Err(VfsSharedIoLifecycleError::InvalidState);
        }
        handles.release(self.descriptor)?;
        self.cleanup_consumed = true;
        self.terminal_reason = Some(reason);
        self.state = VfsSharedIoState::Cleaned;
        Ok(VfsSharedIoCleanupResult::Won(reason))
    }

    pub fn begin_inline_fallback(&mut self) -> Result<(), VfsSharedIoLifecycleError> {
        if self.state != VfsSharedIoState::Cleaned {
            return Err(VfsSharedIoLifecycleError::FallbackBeforeCleanup);
        }
        if self.flags & VFS_SHARED_IO_F_ALLOW_INLINE_FALLBACK == 0 {
            return Err(VfsSharedIoLifecycleError::FallbackNotAllowed);
        }
        if matches!(
            self.terminal_reason,
            Some(
                VfsSharedIoTerminalReason::Success
                    | VfsSharedIoTerminalReason::Cancelled
                    | VfsSharedIoTerminalReason::RequesterExit
                    | VfsSharedIoTerminalReason::ServerExit
            )
        ) {
            return Err(VfsSharedIoLifecycleError::FallbackNotAllowed);
        }
        if self.fallback_started {
            return Err(VfsSharedIoLifecycleError::FallbackAlreadyStarted);
        }
        self.fallback_started = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(handle: VfsSharedIoHandle, access: u32, len: u64) -> VfsSharedBufferDescriptor {
        VfsSharedBufferDescriptor::new(
            handle.object_handle,
            handle.object_generation,
            0,
            len,
            access,
        )
    }

    fn lifecycle<const N: usize>(
        handles: &mut VfsSharedIoHandleTable<N>,
        direction: VfsSharedIoDirection,
        flags: u32,
        len: u64,
    ) -> (VfsSharedIoHandle, VfsSharedIoLifecycle) {
        let handle = handles.allocate().expect("allocate handle");
        let access = direction.required_access();
        let request = VfsSharedIoLifecycle::reserve(
            1,
            descriptor(handle, access, len),
            len,
            flags,
            direction,
        )
        .expect("reserve");
        (handle, request)
    }

    #[test]
    fn handle_generation_invalidates_stale_descriptors_on_reuse() {
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (first, mut request) = lifecycle(&mut handles, VfsSharedIoDirection::ReadReply, 0, 8);
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        request.complete(8).expect("complete");
        assert_eq!(
            request.cleanup(&mut handles, VfsSharedIoTerminalReason::Success),
            Ok(VfsSharedIoCleanupResult::Won(
                VfsSharedIoTerminalReason::Success
            ))
        );
        assert_eq!(
            handles.validate(descriptor(first, VFS_SHARED_BUFFER_FS_WRITE, 8)),
            Err(VfsSharedIoLifecycleError::StaleHandle)
        );

        let second = handles.allocate().expect("reuse handle");
        assert_eq!(second.object_handle, first.object_handle);
        assert_ne!(second.object_generation, first.object_generation);
        assert_eq!(
            handles.validate(descriptor(second, VFS_SHARED_BUFFER_FS_WRITE, 8)),
            Ok(())
        );
        let mut stale_request = VfsSharedIoLifecycle::reserve(
            2,
            descriptor(first, VFS_SHARED_BUFFER_FS_WRITE, 8),
            8,
            0,
            VfsSharedIoDirection::ReadReply,
        )
        .expect("wire-valid stale request");
        assert_eq!(
            stale_request.map(&handles),
            Err(VfsSharedIoLifecycleError::StaleHandle)
        );
    }

    #[test]
    fn direction_and_duplicate_request_validation_are_strict() {
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let handle = handles.allocate().expect("allocate");
        assert_eq!(handles.allocate(), Err(VfsSharedIoLifecycleError::Capacity));
        assert_eq!(
            VfsSharedIoLifecycle::reserve(
                1,
                descriptor(handle, VFS_SHARED_BUFFER_FS_READ, 8),
                8,
                0,
                VfsSharedIoDirection::ReadReply,
            )
            .err(),
            Some(VfsSharedIoLifecycleError::BadDescriptor)
        );
    }

    #[test]
    fn read_success_short_completion_cleanup_and_access_revocation() {
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) = lifecycle(&mut handles, VfsSharedIoDirection::ReadReply, 0, 16);
        assert_eq!(request.state(), VfsSharedIoState::Reserved);
        request.map(&handles).expect("map");
        assert_eq!(request.state(), VfsSharedIoState::MappedForReadReply);
        request.begin().expect("begin");
        request
            .authorize_access(&handles, VfsSharedIoDirection::ReadReply)
            .expect("write access");
        request.complete(5).expect("short EOF");
        assert_eq!(request.bytes_completed(), 5);
        assert_eq!(request.state(), VfsSharedIoState::Completed);
        request
            .cleanup(&mut handles, VfsSharedIoTerminalReason::Success)
            .expect("cleanup");
        assert_eq!(request.state(), VfsSharedIoState::Cleaned);
        assert_eq!(
            request.authorize_access(&handles, VfsSharedIoDirection::ReadReply),
            Err(VfsSharedIoLifecycleError::AccessAfterCleanup)
        );
        assert_eq!(
            request.complete(5),
            Err(VfsSharedIoLifecycleError::DuplicateReply)
        );
    }

    #[test]
    fn write_partial_completion_is_read_only_and_cleanup_is_idempotent() {
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) = lifecycle(&mut handles, VfsSharedIoDirection::WriteRequest, 0, 16);
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        request
            .authorize_access(&handles, VfsSharedIoDirection::WriteRequest)
            .expect("read-only access");
        assert_eq!(
            request.authorize_access(&handles, VfsSharedIoDirection::ReadReply),
            Err(VfsSharedIoLifecycleError::InvalidState)
        );
        request.complete(7).expect("partial write");
        assert_eq!(request.bytes_completed(), 7);
        let first = request
            .cleanup(&mut handles, VfsSharedIoTerminalReason::Success)
            .expect("cleanup");
        let duplicate = request
            .cleanup(&mut handles, VfsSharedIoTerminalReason::BackendError)
            .expect("idempotent cleanup");
        assert_eq!(
            first,
            VfsSharedIoCleanupResult::Won(VfsSharedIoTerminalReason::Success)
        );
        assert_eq!(
            duplicate,
            VfsSharedIoCleanupResult::AlreadyCleaned(VfsSharedIoTerminalReason::Success)
        );
    }

    #[test]
    fn fallback_requires_cleanup_permission_and_is_one_shot() {
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) = lifecycle(
            &mut handles,
            VfsSharedIoDirection::ReadReply,
            VFS_SHARED_IO_F_ALLOW_INLINE_FALLBACK,
            8,
        );
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        assert_eq!(
            request.begin_inline_fallback(),
            Err(VfsSharedIoLifecycleError::FallbackBeforeCleanup)
        );
        request
            .cleanup(&mut handles, VfsSharedIoTerminalReason::Timeout)
            .expect("timeout cleanup before fallback");
        assert_eq!(request.begin_inline_fallback(), Ok(()));
        assert_eq!(
            request.begin_inline_fallback(),
            Err(VfsSharedIoLifecycleError::FallbackAlreadyStarted)
        );
    }

    #[test]
    fn cancel_timeout_and_exit_races_are_first_cleanup_wins() {
        let reasons = [
            VfsSharedIoTerminalReason::Cancelled,
            VfsSharedIoTerminalReason::Timeout,
            VfsSharedIoTerminalReason::RequesterExit,
            VfsSharedIoTerminalReason::ServerExit,
            VfsSharedIoTerminalReason::BackendError,
            VfsSharedIoTerminalReason::Unsupported,
        ];
        for reason in reasons {
            let mut handles = VfsSharedIoHandleTable::<1>::new();
            let (_, mut request) =
                lifecycle(&mut handles, VfsSharedIoDirection::WriteRequest, 0, 8);
            request.map(&handles).expect("map");
            request.begin().expect("begin");
            assert_eq!(
                request.cleanup(&mut handles, reason),
                Ok(VfsSharedIoCleanupResult::Won(reason))
            );
            assert_eq!(
                request.cleanup(&mut handles, VfsSharedIoTerminalReason::Success),
                Ok(VfsSharedIoCleanupResult::AlreadyCleaned(reason))
            );
            assert_eq!(
                request.complete(8),
                Err(VfsSharedIoLifecycleError::DuplicateReply)
            );
        }
    }

    #[test]
    fn requester_exit_after_completion_beats_unconsumed_success_reply() {
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) = lifecycle(&mut handles, VfsSharedIoDirection::ReadReply, 0, 8);
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        request.complete(8).expect("server completed");
        assert_eq!(
            request.cleanup(&mut handles, VfsSharedIoTerminalReason::RequesterExit),
            Ok(VfsSharedIoCleanupResult::Won(
                VfsSharedIoTerminalReason::RequesterExit
            ))
        );
        assert_eq!(
            request.terminal_reason(),
            Some(VfsSharedIoTerminalReason::RequesterExit)
        );
    }
}
