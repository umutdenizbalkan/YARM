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

/// Outcome of a TID-matched requester-exit delivery attempt.
///
/// `Matched` means the requester TID matched this lifecycle's stored TID.
/// `NotMatched` means the TID did not match; the lifecycle state is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsSharedIoRequesterExitAction {
    Matched(VfsSharedIoCleanupResult),
    NotMatched,
}

/// One helper request and its single cleanup token.
pub struct VfsSharedIoLifecycle {
    request_id: u64,
    /// TID of the task that submitted this request. Used to correlate
    /// `SUPERVISOR_OP_TASK_EXITED(tid)` notifications to active lifecycles.
    requester_tid: u64,
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
        requester_tid: u64,
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
            requester_tid,
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

    pub const fn requester_tid(&self) -> u64 {
        self.requester_tid
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

    pub const fn descriptor(&self) -> VfsSharedBufferDescriptor {
        self.descriptor
    }

    pub const fn requested_len(&self) -> u64 {
        self.requested_len
    }

    pub const fn direction(&self) -> VfsSharedIoDirection {
        self.direction
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

    /// Delivers RequesterExit only if `tid` matches this lifecycle's `requester_tid`.
    ///
    /// Returns `RequesterExitAction::NotMatched` (safe no-op) when `tid != self.requester_tid`.
    /// Returns `RequesterExitAction::Matched(result)` when TID matches — equivalent to calling
    /// `deliver_requester_exit` directly.
    ///
    /// This is the Stage 75 helper that models what VFS would do when it receives a
    /// `SUPERVISOR_OP_TASK_EXITED(tid)` notification and scans its active lifecycle store.
    /// Production wiring requires a VFS-side lifecycle store and supervisor→VFS notification
    /// channel (both absent; see `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED`).
    pub fn deliver_requester_exit_if_tid_matches<const N: usize>(
        &mut self,
        tid: u64,
        handles: &mut VfsSharedIoHandleTable<N>,
    ) -> Result<VfsSharedIoRequesterExitAction, VfsSharedIoLifecycleError> {
        if tid != self.requester_tid {
            return Ok(VfsSharedIoRequesterExitAction::NotMatched);
        }
        self.deliver_requester_exit(handles)
            .map(VfsSharedIoRequesterExitAction::Matched)
    }

    /// Delivers a helper-only requester-exit notification to this lifecycle state machine.
    ///
    /// This is the VFS-side entry point for `VfsSharedIoTerminalReason::RequesterExit`.
    /// In production it would be triggered by a supervisor `SUPERVISOR_OP_TASK_EXITED`
    /// notification correlated to an active `cleanup_token`; in Stage 73 tests it is
    /// called directly.  The caller is responsible for releasing any mapper resources
    /// via `cleanup_shared_io` if a live mapper was acquired.
    ///
    /// Idempotent: returns `AlreadyCleaned` if cleanup already ran.
    pub fn deliver_requester_exit<const N: usize>(
        &mut self,
        handles: &mut VfsSharedIoHandleTable<N>,
    ) -> Result<VfsSharedIoCleanupResult, VfsSharedIoLifecycleError> {
        self.cleanup(handles, VfsSharedIoTerminalReason::RequesterExit)
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
            0, // requester_tid: 0 in non-Stage-75 tests
            descriptor(handle, access, len),
            len,
            flags,
            direction,
        )
        .expect("reserve");
        (handle, request)
    }

    fn lifecycle_with_tid<const N: usize>(
        handles: &mut VfsSharedIoHandleTable<N>,
        requester_tid: u64,
        direction: VfsSharedIoDirection,
        flags: u32,
        len: u64,
    ) -> (VfsSharedIoHandle, VfsSharedIoLifecycle) {
        let handle = handles.allocate().expect("allocate handle");
        let access = direction.required_access();
        let request = VfsSharedIoLifecycle::reserve(
            1,
            requester_tid,
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
            0, // requester_tid: 0 in stale-descriptor test
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
                0, // requester_tid: 0 in direction-validation test
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

    // ── Stage 73: RequesterExit helper-only notification model ───────────────

    #[test]
    fn stage73_requester_exit_before_completion_wins() {
        // Process exits while I/O is in-flight (server has not yet replied).
        // deliver_requester_exit must win cleanup and terminate with RequesterExit.
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) = lifecycle(&mut handles, VfsSharedIoDirection::ReadReply, 0, 16);
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        assert_eq!(request.state(), VfsSharedIoState::InFlight);
        let result = request.deliver_requester_exit(&mut handles).expect("deliver");
        assert_eq!(result, VfsSharedIoCleanupResult::Won(VfsSharedIoTerminalReason::RequesterExit));
        assert_eq!(request.state(), VfsSharedIoState::Cleaned);
        assert_eq!(request.terminal_reason(), Some(VfsSharedIoTerminalReason::RequesterExit));
    }

    #[test]
    fn stage73_duplicate_requester_exit_is_idempotent() {
        // Calling deliver_requester_exit twice must not panic and must return AlreadyCleaned.
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) = lifecycle(&mut handles, VfsSharedIoDirection::ReadReply, 0, 8);
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        let first = request.deliver_requester_exit(&mut handles).expect("first");
        let second = request.deliver_requester_exit(&mut handles).expect("second idempotent");
        assert_eq!(first, VfsSharedIoCleanupResult::Won(VfsSharedIoTerminalReason::RequesterExit));
        assert_eq!(second, VfsSharedIoCleanupResult::AlreadyCleaned(VfsSharedIoTerminalReason::RequesterExit));
    }

    #[test]
    fn stage73_success_cleanup_beats_requester_exit() {
        // Server completes and explicit Success cleanup runs before RequesterExit arrives.
        // RequesterExit must return AlreadyCleaned(Success).
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) = lifecycle(&mut handles, VfsSharedIoDirection::ReadReply, 0, 8);
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        request.complete(8).expect("complete");
        let first = request
            .cleanup(&mut handles, VfsSharedIoTerminalReason::Success)
            .expect("success cleanup");
        let exit = request.deliver_requester_exit(&mut handles).expect("exit after success");
        assert_eq!(first, VfsSharedIoCleanupResult::Won(VfsSharedIoTerminalReason::Success));
        assert_eq!(exit, VfsSharedIoCleanupResult::AlreadyCleaned(VfsSharedIoTerminalReason::Success));
    }

    #[test]
    fn stage73_backend_error_beats_requester_exit() {
        // Backend error cleanup runs before RequesterExit notification arrives.
        // RequesterExit must return AlreadyCleaned(BackendError).
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) = lifecycle(&mut handles, VfsSharedIoDirection::WriteRequest, 0, 8);
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        let first = request
            .cleanup(&mut handles, VfsSharedIoTerminalReason::BackendError)
            .expect("backend error cleanup");
        let exit = request.deliver_requester_exit(&mut handles).expect("exit after error");
        assert_eq!(first, VfsSharedIoCleanupResult::Won(VfsSharedIoTerminalReason::BackendError));
        assert_eq!(exit, VfsSharedIoCleanupResult::AlreadyCleaned(VfsSharedIoTerminalReason::BackendError));
    }

    #[test]
    fn stage73_requester_exit_blocks_inline_fallback() {
        // RequesterExit terminal reason must prevent inline fallback, even with the
        // F_ALLOW_INLINE_FALLBACK flag set.
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) = lifecycle(
            &mut handles,
            VfsSharedIoDirection::ReadReply,
            VFS_SHARED_IO_F_ALLOW_INLINE_FALLBACK,
            8,
        );
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        request.deliver_requester_exit(&mut handles).expect("exit");
        assert_eq!(request.state(), VfsSharedIoState::Cleaned);
        assert_eq!(
            request.begin_inline_fallback(),
            Err(VfsSharedIoLifecycleError::FallbackNotAllowed),
            "RequesterExit must block inline fallback"
        );
    }

    #[test]
    fn stage73_requester_exit_from_reserved_state() {
        // deliver_requester_exit must work even before map()/begin() are called.
        // This covers the case where the process exits immediately after initiating I/O.
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) = lifecycle(&mut handles, VfsSharedIoDirection::ReadReply, 0, 8);
        assert_eq!(request.state(), VfsSharedIoState::Reserved);
        let result = request.deliver_requester_exit(&mut handles).expect("exit from reserved");
        assert_eq!(result, VfsSharedIoCleanupResult::Won(VfsSharedIoTerminalReason::RequesterExit));
        assert_eq!(request.state(), VfsSharedIoState::Cleaned);
    }

    #[test]
    fn stage73_handle_generation_advances_after_requester_exit() {
        // After deliver_requester_exit the old descriptor's handle slot is released
        // and the generation advances.  A new allocation reuses the slot with a
        // higher generation.
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (old_handle, mut request) =
            lifecycle(&mut handles, VfsSharedIoDirection::ReadReply, 0, 8);
        let old_descriptor = descriptor(old_handle, VFS_SHARED_BUFFER_FS_WRITE, 8);
        request.deliver_requester_exit(&mut handles).expect("exit");
        // Old descriptor must no longer validate.
        assert_eq!(
            handles.validate(old_descriptor),
            Err(VfsSharedIoLifecycleError::StaleHandle),
            "old descriptor must be stale after RequesterExit"
        );
        // New allocation reuses slot with bumped generation.
        let new_handle = handles.allocate().expect("reuse slot");
        assert_eq!(new_handle.object_handle, old_handle.object_handle, "same slot reused");
        assert_ne!(
            new_handle.object_generation, old_handle.object_generation,
            "generation must advance"
        );
    }

    // ── Stage 75: TID-matched RequesterExit delivery ─────────────────────────
    //
    // These tests prove the VFS-side identity model for RequesterExit:
    // `VfsSharedIoLifecycle::requester_tid` stores the requesting TID, and
    // `deliver_requester_exit_if_tid_matches` dispatches by TID with a safe
    // no-op on mismatch.  Production wiring (supervisor→VFS notification
    // channel + VFS lifecycle store) is documented but not implemented here.

    #[test]
    fn stage75_lifecycle_requester_tid_stored() {
        // requester_tid stored at reserve() time and readable via accessor.
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, request) =
            lifecycle_with_tid(&mut handles, 42, VfsSharedIoDirection::ReadReply, 0, 8);
        assert_eq!(request.requester_tid(), 42);
    }

    #[test]
    fn stage75_matched_tid_delivers_requester_exit() {
        // TID matches → Matched(Won(RequesterExit)).
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) =
            lifecycle_with_tid(&mut handles, 99, VfsSharedIoDirection::ReadReply, 0, 16);
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        assert_eq!(request.state(), VfsSharedIoState::InFlight);
        let action = request
            .deliver_requester_exit_if_tid_matches(99, &mut handles)
            .expect("deliver");
        assert_eq!(
            action,
            VfsSharedIoRequesterExitAction::Matched(VfsSharedIoCleanupResult::Won(
                VfsSharedIoTerminalReason::RequesterExit
            ))
        );
        assert_eq!(request.state(), VfsSharedIoState::Cleaned);
    }

    #[test]
    fn stage75_unmatched_tid_is_safe_noop() {
        // TID does not match → NotMatched; lifecycle state unchanged.
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) =
            lifecycle_with_tid(&mut handles, 7, VfsSharedIoDirection::ReadReply, 0, 8);
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        let action = request
            .deliver_requester_exit_if_tid_matches(8, &mut handles)
            .expect("no-op");
        assert_eq!(action, VfsSharedIoRequesterExitAction::NotMatched);
        assert_eq!(
            request.state(),
            VfsSharedIoState::InFlight,
            "state must be unchanged after NotMatched"
        );
    }

    #[test]
    fn stage75_duplicate_matched_tid_is_idempotent() {
        // Calling deliver_requester_exit_if_tid_matches twice with a matching TID
        // must return Matched(AlreadyCleaned) on the second call.
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) =
            lifecycle_with_tid(&mut handles, 5, VfsSharedIoDirection::ReadReply, 0, 8);
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        let first = request
            .deliver_requester_exit_if_tid_matches(5, &mut handles)
            .expect("first");
        let second = request
            .deliver_requester_exit_if_tid_matches(5, &mut handles)
            .expect("second");
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

    #[test]
    fn stage75_explicit_cleanup_before_matched_tid_is_noop() {
        // Success cleanup runs before TID-matched exit notification arrives.
        // Exit must return Matched(AlreadyCleaned(Success)).
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) =
            lifecycle_with_tid(&mut handles, 11, VfsSharedIoDirection::ReadReply, 0, 8);
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        request.complete(8).expect("complete");
        request
            .cleanup(&mut handles, VfsSharedIoTerminalReason::Success)
            .expect("success cleanup");
        let action = request
            .deliver_requester_exit_if_tid_matches(11, &mut handles)
            .expect("exit after success");
        assert_eq!(
            action,
            VfsSharedIoRequesterExitAction::Matched(VfsSharedIoCleanupResult::AlreadyCleaned(
                VfsSharedIoTerminalReason::Success
            ))
        );
    }

    #[test]
    fn stage75_zero_tid_lifecycle_matches_only_zero_tid() {
        // A lifecycle created with requester_tid=0 only matches TID 0.
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) =
            lifecycle_with_tid(&mut handles, 0, VfsSharedIoDirection::ReadReply, 0, 8);
        let action_nonzero = request
            .deliver_requester_exit_if_tid_matches(1, &mut handles)
            .expect("nonzero no-op");
        assert_eq!(action_nonzero, VfsSharedIoRequesterExitAction::NotMatched);
        let action_zero = request
            .deliver_requester_exit_if_tid_matches(0, &mut handles)
            .expect("zero matches");
        assert!(matches!(
            action_zero,
            VfsSharedIoRequesterExitAction::Matched(VfsSharedIoCleanupResult::Won(_))
        ));
    }

    #[test]
    fn stage75_generation_advances_after_tid_matched_exit() {
        // After TID-matched exit, the slot generation advances and old descriptor is stale.
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (old_handle, mut request) =
            lifecycle_with_tid(&mut handles, 3, VfsSharedIoDirection::ReadReply, 0, 8);
        let old_descriptor = descriptor(old_handle, VFS_SHARED_BUFFER_FS_WRITE, 8);
        request
            .deliver_requester_exit_if_tid_matches(3, &mut handles)
            .expect("exit");
        assert_eq!(
            handles.validate(old_descriptor),
            Err(VfsSharedIoLifecycleError::StaleHandle),
            "slot must be released and descriptor stale"
        );
        let new_handle = handles.allocate().expect("reuse slot");
        assert_eq!(new_handle.object_handle, old_handle.object_handle);
        assert_ne!(new_handle.object_generation, old_handle.object_generation);
    }

    #[test]
    fn stage75_read_reply_lifecycle_observes_tid_matched_exit() {
        // ReadReply direction lifecycle is cleaned up by TID-matched exit.
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) = lifecycle_with_tid(
            &mut handles,
            77,
            VfsSharedIoDirection::ReadReply,
            0,
            32,
        );
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        assert_eq!(request.direction(), VfsSharedIoDirection::ReadReply);
        let action = request
            .deliver_requester_exit_if_tid_matches(77, &mut handles)
            .expect("exit");
        assert_eq!(
            action,
            VfsSharedIoRequesterExitAction::Matched(VfsSharedIoCleanupResult::Won(
                VfsSharedIoTerminalReason::RequesterExit
            ))
        );
    }

    #[test]
    fn stage75_write_request_lifecycle_unaffected_by_unmatched_tid() {
        // WriteRequest lifecycle for TID=20 is not affected by exit notification for TID=21.
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let (_, mut request) = lifecycle_with_tid(
            &mut handles,
            20,
            VfsSharedIoDirection::WriteRequest,
            0,
            8,
        );
        request.map(&handles).expect("map");
        request.begin().expect("begin");
        let action = request
            .deliver_requester_exit_if_tid_matches(21, &mut handles)
            .expect("no-op");
        assert_eq!(action, VfsSharedIoRequesterExitAction::NotMatched);
        assert_eq!(request.state(), VfsSharedIoState::InFlight);
        assert_eq!(request.requester_tid(), 20);
    }

    #[test]
    fn stage75_multiple_lifecycles_only_matched_tid_cleaned() {
        // Two lifecycles with different TIDs: exit for TID A must not affect TID B's lifecycle.
        let mut handles = VfsSharedIoHandleTable::<2>::new();
        let (_, mut req_a) = lifecycle_with_tid(
            &mut handles,
            100,
            VfsSharedIoDirection::ReadReply,
            0,
            8,
        );
        let (_, mut req_b) = lifecycle_with_tid(
            &mut handles,
            200,
            VfsSharedIoDirection::ReadReply,
            0,
            8,
        );
        req_a.map(&handles).expect("map a");
        req_a.begin().expect("begin a");
        req_b.map(&handles).expect("map b");
        req_b.begin().expect("begin b");

        // Exit for TID 100 only cleans req_a.
        let action_a = req_a
            .deliver_requester_exit_if_tid_matches(100, &mut handles)
            .expect("exit a");
        let action_b = req_b
            .deliver_requester_exit_if_tid_matches(100, &mut handles)
            .expect("exit b no-op");

        assert_eq!(
            action_a,
            VfsSharedIoRequesterExitAction::Matched(VfsSharedIoCleanupResult::Won(
                VfsSharedIoTerminalReason::RequesterExit
            ))
        );
        assert_eq!(action_b, VfsSharedIoRequesterExitAction::NotMatched);
        assert_eq!(req_a.state(), VfsSharedIoState::Cleaned);
        assert_eq!(req_b.state(), VfsSharedIoState::InFlight);
    }
}
