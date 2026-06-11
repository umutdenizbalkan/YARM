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
    VfsSharedIoRequesterExitAction,
};
use yarm_ipc_abi::process_abi::{
    KERNEL_OP_PM_TASK_EXITED, PROC_OP_TASK_EXITED, KernelPmTaskExitedPayload, PmTaskExitedEvent,
};
use yarm_ipc_abi::vfs_abi::{
    VFS_SHARED_BUFFER_FS_READ, VFS_SHARED_BUFFER_FS_WRITE, VfsReadSharedRequest,
    VfsSharedBufferDescriptor, VfsWriteSharedRequest,
};
use yarm_user_rt::syscall::recv_v3::RecvSharedV3Delivery;
use yarm_user_rt::syscall::shared_transfer::release_v3_cleanup_token;

/// Gating constant for the WRITE_SHARED_REQUEST live route.
///
/// Stage 78: enabled (`true`). Prerequisites met:
/// - Kernel MAP_READ delivery always supported.
/// - `VfsWriteSharedBinding` validates all 11 constraints (Stage 65).
/// - RAMFS roundtrip proof complete (Stage 65).
/// - `dispatch_write_shared_request` available for direct calls (Stage 66+68).
/// - `VfsSharedIoLifecycle::RequesterExit` model proven for both directions (Stage 73+75).
/// - PM task-exit notification wired (Stage 77+78): kernel→PM→VFS death path complete.
///
/// Remaining production constraint: no real `VfsSharedIoMapper` implementation exists.
/// `UnsupportedSharedIoMapper` remains the production default. `handle_request` still
/// rejects WRITE_SHARED_REQUEST with `VfsError::Unsupported` until a real mapper is
/// available (see FS-17 through FS-19 for mapper ABI design).
pub const VFS_WRITE_SHARED_REQUEST_ENABLED: bool = true;

/// Gating constant for the READ_SHARED_REPLY live route.
///
/// Stage 73: enabled (`true`). Prerequisites met:
/// - Kernel MAP_WRITE delivery enabled (Stage 72).
/// - `VfsSharedIoTerminalReason::RequesterExit` lifecycle model proven (7 tests, mod stage73).
/// - `deliver_requester_exit` helper models VFS-side notification entry point.
///
/// Remaining production blocker: live kernel→VFS `RequesterExit` notification via supervisor
/// `SUPERVISOR_OP_TASK_EXITED` is not yet wired.  `dispatch_read_shared_reply` is available
/// for direct calls; `handle_request` still returns `VfsError::Unsupported` for the opcode.
pub const VFS_READ_SHARED_REPLY_ENABLED: bool = true;

/// Global shared-I/O umbrella gate.
///
/// Stage 78: `true`. All three prerequisites independently verified:
/// - `VFS_WRITE_SHARED_REQUEST_ENABLED = true` (Stage 78): WRITE helper fully proven.
/// - `VFS_READ_SHARED_REPLY_ENABLED = true` (Stage 73): READ helper fully proven.
/// - `VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED = true` (Stage 77+78): death path wired.
///
/// `true` here means both direction helpers and the PM notification path are proven correct,
/// NOT that `handle_request` routes shared opcodes in production. Both
/// `VFS_OP_WRITE_SHARED_REQUEST` and `VFS_OP_READ_SHARED_REPLY` remain `VfsError::Unsupported`
/// in live dispatch until a real `VfsSharedIoMapper` is available. See FS-17/FS-19.
pub const VFS_SHARED_IO_ENABLED: bool =
    VFS_WRITE_SHARED_REQUEST_ENABLED
    && VFS_READ_SHARED_REPLY_ENABLED
    && VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED;

/// Stage 75: supervisor→VFS task-exit notification channel.
///
/// `false` (Stage 75): not yet wired. Two missing pieces before this can become `true`:
///
/// 1. **VFS notification endpoint**: a supervisor capability pointing at VFS's endpoint
///    must be added to the startup handoff and the supervisor forwarding path wired.
///    Specifically: `InitFaultHandoff` needs a `vfs_task_exit_send_cap: Option<CapId>`,
///    the supervisor's `handle_task_exit` must send `SUPERVISOR_OP_TASK_EXITED` to that
///    cap when a non-supervisor task exits, and VFS's service loop must receive and decode it.
///
/// 2. **VFS-side lifecycle store**: `VfsService` currently has no persistent
///    `VfsSharedIoLifecycle` store.  A bounded table keyed by `requester_tid`
///    (now present in `VfsSharedIoLifecycle`) is needed so that on
///    `SUPERVISOR_OP_TASK_EXITED(tid)`, VFS can look up affected lifecycles and call
///    `deliver_requester_exit_if_tid_matches`.
///
/// Identity model is proven (Stage 75): `VfsSharedIoLifecycle::requester_tid` stores the TID,
/// and `deliver_requester_exit_if_tid_matches` dispatches by TID with safe no-op on mismatch.
pub const VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED: bool = false;

/// Stage 77+78: PM → VFS task-exit notification channel (PM-owned lifecycle authority).
///
/// `true` (Stage 77+78): both blockers from Stage 76 are now resolved:
///
/// 1. **PM→VFS send cap RESOLVED**: PM already has `vfs_send_cap` via
///    `lifecycle_table.get_by_image_id(6).pm_service_send_cap` (image_id=6 = VFS).
///    PM can send `PROC_OP_TASK_EXITED` to VFS on this existing cap.
///
/// 2. **Kernel→PM task-exit delivery RESOLVED**: `FaultSubsystem::pm_task_exit_endpoint`
///    added in Stage 77+78. `exit_task()` calls `report_task_exit_to_pm()` after
///    `report_task_exit_to_supervisor()`. Kernel sends `KERNEL_OP_PM_TASK_EXITED = 0xDC`
///    (16-byte LE: tid+exit_code) to PM's registered endpoint. Tests prove end-to-end delivery.
///
/// VFS dispatch entry point: `dispatch_pm_task_exited_push()` decodes `PROC_OP_TASK_EXITED`
/// and calls `handle_pm_task_exited(tid, lifecycle, handles)`.
pub const VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED: bool = true;

/// Stage 84: RAMFS-only shared-I/O service-loop bridge.
///
/// `true`: the gated methods `handle_write_shared_request_gated` and
/// `handle_read_shared_reply_gated` on `VfsService` are enabled.  These methods
/// use the per-request lifecycle/handle store and `RecvV3SharedIoMapper` for byte access.
///
/// **RAMFS-only** (Stage 84): production FAT/ext4/blkcache paths remain unchanged.
/// Does **not** globally enable `handle_request` shared-op routing.
/// Does **not** change SYSCALL_COUNT, startup slots, SpawnV5 ABI, or image IDs.
pub const VFS_STAGE84_RAMFS_BRIDGE_ENABLED: bool = true;

/// Stage 85: RAMFS live shared-I/O route/profile gate.
///
/// `true`: `VfsService::dispatch_shared_delivery` is active, wiring the RAMFS-only
/// Stage 84 bridge into the normal VFS service-loop message path.  A
/// `RecvSharedV3Delivery` + encoded `Message` is decoded by opcode and routed to
/// `handle_write_shared_request_gated` or `handle_read_shared_reply_gated`.
///
/// This is a **profile proof**: it proves the Stage 84 bridge can be called from
/// a live VFS route without changing the default `handle_request` path.
///
/// **RAMFS-only** (Stage 85): FAT/ext4/blkcache paths remain unchanged.
/// `UnsupportedSharedIoMapper` remains the default outside this explicit gate.
/// `handle_request` shared opcodes still return `VfsError::Unsupported`.
/// Does **not** change SYSCALL_COUNT, startup slots, SpawnV5 ABI, or image IDs.
pub const VFS_STAGE85_RAMFS_LIVE_ROUTE_ENABLED: bool = true;

/// Stage 86: RAMFS live-mount path enabled.
///
/// `true`: RAMFS server enters a resident `ipc_recv_v2` loop after bootstrap and
/// registers /ram with the VFS mount table.  Gate kept separate so FAT/ext4
/// live-mount can be enabled independently.
pub const VFS_RAMFS_LIVE_MOUNT_ENABLED: bool = true;

/// Stage 86: FAT shared-I/O live route.
///
/// `false`: FAT server requires a virtio_blk block device not available in the
/// default hosted-dev environment.  `FatBackend::read_shared_bytes` is wired but
/// the spawn gate (`INIT_SPAWN_FAT_SRV`) is disabled.  Enable once a block-device
/// stub is available for integration tests.
pub const VFS_FAT_SHARED_IO_ENABLED: bool = false;

/// Stage 87: FAT live-mount path.
///
/// `false`: FAT server requires a virtio_blk block device not present in the
/// default hosted-dev environment.  The resident recv loop is implemented in
/// `fat/service.rs` (Stage 87) but `INIT_SPAWN_FAT_SRV` remains disabled until:
///   (1) a block-device stub is available for integration tests, and
///   (2) `VFS_FAT_SHARED_IO_ENABLED` is lifted to `true`.
/// When `true` this gate allows `register_fat_mount_with_vfs` to be called
/// after a successful FAT spawn and `/fat` is registered in the VFS mount table.
pub const VFS_FAT_LIVE_MOUNT_ENABLED: bool = false;

/// Stage 86: ext4 resident IPC recv loop.
///
/// `true`: ext4/service.rs::run() enters a resident `ipc_recv_v2` loop after
/// the smoke demo.  This lifted the Stage-80 "no-ipc-loop" blocker.
/// Stage 88 builds on this: `VFS_EXT4_LIVE_MOUNT_ENABLED = true` wires
/// VFS mount registration, making `/ext4` available read-only after ext4_srv spawns.
/// ext4 write operations remain `VfsError::Unsupported`.
pub const VFS_EXT4_RECV_LOOP_ENABLED: bool = true;

/// Stage 88: ext4 live read-only VFS mount.
///
/// `true` (Stage 88): ext4 backend satisfies the read-only VFS contract:
/// - `openat_path()` resolves `/ext4/*` paths to inodes.
/// - `statx_path()` returns file metadata (file_len; currently 0 for demo inodes).
/// - `read()` returns at most `file_len` bytes.
/// - `write()` unconditionally returns `VfsError::Unsupported`.
///
/// `VFS_EXT4_RECV_LOOP_ENABLED` (Stage 86) provides the resident service loop.
/// `register_ext4_mount_with_vfs` is called from init after a successful ext4 spawn,
/// registering `/ext4` read-only in the VFS mount table (flags=1).
///
/// FAT (`VFS_FAT_LIVE_MOUNT_ENABLED`) remains disabled: FAT requires a real
/// virtio_blk block device not present in the default hosted-dev environment.
/// FAT cap handoff design is proven — init passes `init_blkcache_send_cap` to
/// fat_srv at spawn time via `service_extra_cap_0` — but `INIT_SPAWN_FAT_SRV`
/// remains `false` until a block-device stub is available for integration tests.
pub const VFS_EXT4_LIVE_MOUNT_ENABLED: bool = true;

/// Stage 76: VFS entry point for a PM-pushed `PROC_OP_TASK_EXITED` event.
///
/// Gated by `VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED` (enabled in Stage 77+78).
/// `dispatch_pm_task_exited_push()` decodes the wire message and calls this function.
/// Tests may also call directly to exercise the per-lifecycle match logic.
///
/// Returns `NotMatched` if `tid` does not match `lifecycle.requester_tid()`.
/// Returns `Matched(result)` on a TID match, where `result` is the cleanup outcome.
pub fn handle_pm_task_exited<const N: usize>(
    tid: u64,
    lifecycle: &mut VfsSharedIoLifecycle,
    handles: &mut VfsSharedIoHandleTable<N>,
) -> Result<VfsSharedIoRequesterExitAction, VfsSharedIoLifecycleError> {
    lifecycle.deliver_requester_exit_if_tid_matches(tid, handles)
}

/// Errors returned by the VFS-side PM push dispatch functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsPmPushDispatchError {
    /// Message opcode was not the expected push opcode.
    WrongOpcode,
    /// Payload too short to decode.
    Malformed,
}

/// Stage 77+78: VFS-side dispatch for an incoming `PROC_OP_TASK_EXITED` push message.
///
/// Decodes the 16-byte `PmTaskExitedEvent` payload from `opcode` + `payload`, then calls
/// `handle_pm_task_exited(tid, lifecycle, handles)`.  Returns `WrongOpcode` if `opcode !=
/// PROC_OP_TASK_EXITED` and `Malformed` if the payload is too short.
///
/// Gated by `VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED` (now `true`).
pub fn dispatch_pm_task_exited_push<const N: usize>(
    opcode: u16,
    payload: &[u8],
    lifecycle: &mut VfsSharedIoLifecycle,
    handles: &mut VfsSharedIoHandleTable<N>,
) -> Result<VfsSharedIoRequesterExitAction, VfsPmPushDispatchError> {
    if opcode != PROC_OP_TASK_EXITED {
        return Err(VfsPmPushDispatchError::WrongOpcode);
    }
    let event =
        PmTaskExitedEvent::decode(payload).map_err(|_| VfsPmPushDispatchError::Malformed)?;
    handle_pm_task_exited(event.tid, lifecycle, handles)
        .map_err(|_| VfsPmPushDispatchError::Malformed)
}

/// Stage 77+78: VFS-side decode for a kernel→PM `KERNEL_OP_PM_TASK_EXITED` message
/// (arriving at PM's `pm_task_exit_endpoint`).
///
/// Returns the extracted `(tid, exit_code)` pair, or `Malformed` on payload error.
/// PM calls this to decode the kernel push before forwarding to VFS via `PROC_OP_TASK_EXITED`.
pub fn decode_kernel_pm_task_exited(
    opcode: u16,
    payload: &[u8],
) -> Result<(u64, u64), VfsPmPushDispatchError> {
    if opcode != KERNEL_OP_PM_TASK_EXITED {
        return Err(VfsPmPushDispatchError::WrongOpcode);
    }
    let ev = KernelPmTaskExitedPayload::decode(payload)
        .map_err(|_| VfsPmPushDispatchError::Malformed)?;
    Ok((ev.tid, ev.exit_code))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsSharedIoAdapterError {
    Lifecycle(VfsSharedIoLifecycleError),
    UnsupportedMapping,
    StaleHandle,
    WrongObject,
    MissingRights,
    BadRange,
    WrongDirection,
    MapFailure,
    ReleaseFailure,
    AccessAfterCleanup,
}

impl From<VfsSharedIoLifecycleError> for VfsSharedIoAdapterError {
    fn from(value: VfsSharedIoLifecycleError) -> Self {
        Self::Lifecycle(value)
    }
}

/// Direction-safe mapping boundary. A write request can only expose an immutable slice.
///
/// A production implementation must resolve an adapter-owned opaque handle/generation registry,
/// validate the transferred object type, rights, size, and descriptor range, and release the
/// receive-time mapping exactly once. The descriptor handle is never implicitly a raw VA or cap slot.
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

// ── Stage 79: Production VfsSharedIoMapper bridge ──────────────────────────────

/// Stage 79+83: production `VfsSharedIoMapper` backed by a `recv_shared_v3` delivery.
///
/// Maps delivery metadata to direction-safe byte slice access.  Exposes the mapped
/// region as `&[u8]` (write request) or `&mut [u8]` (read reply) via
/// `core::slice::from_raw_parts[_mut]`.
///
/// ## Release contract
///
/// `release` calls `release_v3_cleanup_token(cleanup_token)` exactly once; the
/// `released` flag is set on the first call regardless of syscall outcome (at-most-once
/// semantics).  Subsequent accesses return `AccessAfterCleanup`.  No Drop cleanup —
/// callers must call `release` explicitly.
///
/// ## Byte-access contract
///
/// `with_write_request_buffer` and `with_read_reply_buffer` call
/// `core::slice::from_raw_parts[_mut]` on `mapped_base`.  In production this is a
/// kernel-mapped VA with appropriate permissions.  Stage 83 unit tests supply a real
/// heap-allocated backing buffer as `mapped_base`; the slice construction is defined
/// and byte content is directly observable.  Stage 79 error-path tests use a fake VA
/// constant and never reach the slice creation.
pub struct RecvV3SharedIoMapper {
    cleanup_token: u64,
    mapped_base: u64,
    page_rounded_mapped_len: u64,
    actual_mapping_perm: u32,
    released: bool,
}

impl RecvV3SharedIoMapper {
    const MAP_PERM_READ_ONLY: u32 = 1;
    const MAP_PERM_WRITE_BIT: u32 = 0x2;

    /// Construct from the natural production entry point.
    pub fn from_delivery(delivery: &RecvSharedV3Delivery) -> Self {
        Self {
            cleanup_token: delivery.cleanup_token,
            mapped_base: delivery.mapped_base,
            page_rounded_mapped_len: delivery.page_rounded_mapped_len,
            actual_mapping_perm: delivery.actual_mapping_perm,
            released: false,
        }
    }

    /// Construct from raw delivery metadata fields.
    pub fn from_fields(
        cleanup_token: u64,
        mapped_base: u64,
        page_rounded_mapped_len: u64,
        actual_mapping_perm: u32,
    ) -> Self {
        Self {
            cleanup_token,
            mapped_base,
            page_rounded_mapped_len,
            actual_mapping_perm,
            released: false,
        }
    }

    pub const fn is_released(&self) -> bool {
        self.released
    }

    fn write_slice_ptr(
        &self,
        descriptor: VfsSharedBufferDescriptor,
        requested_len: u64,
    ) -> Result<(*const u8, usize), VfsSharedIoAdapterError> {
        if self.released {
            return Err(VfsSharedIoAdapterError::AccessAfterCleanup);
        }
        if descriptor.access != VFS_SHARED_BUFFER_FS_READ {
            return Err(VfsSharedIoAdapterError::WrongDirection);
        }
        if descriptor.object_handle != self.cleanup_token
            || descriptor.object_generation != self.cleanup_token >> 16
        {
            return Err(VfsSharedIoAdapterError::StaleHandle);
        }
        if self.actual_mapping_perm != Self::MAP_PERM_READ_ONLY {
            return Err(VfsSharedIoAdapterError::MissingRights);
        }
        self.byte_slice_ptr(descriptor.buffer_offset, requested_len)
    }

    fn read_slice_ptr(
        &self,
        descriptor: VfsSharedBufferDescriptor,
        requested_len: u64,
    ) -> Result<(*const u8, usize), VfsSharedIoAdapterError> {
        if self.released {
            return Err(VfsSharedIoAdapterError::AccessAfterCleanup);
        }
        if descriptor.access != VFS_SHARED_BUFFER_FS_WRITE {
            return Err(VfsSharedIoAdapterError::WrongDirection);
        }
        if descriptor.object_handle != self.cleanup_token
            || descriptor.object_generation != self.cleanup_token >> 16
        {
            return Err(VfsSharedIoAdapterError::StaleHandle);
        }
        if self.actual_mapping_perm & Self::MAP_PERM_WRITE_BIT == 0 {
            return Err(VfsSharedIoAdapterError::MissingRights);
        }
        self.byte_slice_ptr(descriptor.buffer_offset, requested_len)
    }

    fn byte_slice_ptr(
        &self,
        buffer_offset: u64,
        requested_len: u64,
    ) -> Result<(*const u8, usize), VfsSharedIoAdapterError> {
        let range_end = buffer_offset
            .checked_add(requested_len)
            .ok_or(VfsSharedIoAdapterError::BadRange)?;
        if self.page_rounded_mapped_len < range_end {
            return Err(VfsSharedIoAdapterError::BadRange);
        }
        let offset = usize::try_from(buffer_offset)
            .map_err(|_| VfsSharedIoAdapterError::BadRange)?;
        let base = usize::try_from(self.mapped_base)
            .map_err(|_| VfsSharedIoAdapterError::BadRange)?;
        let ptr = base
            .checked_add(offset)
            .ok_or(VfsSharedIoAdapterError::BadRange)?;
        let len = usize::try_from(requested_len)
            .map_err(|_| VfsSharedIoAdapterError::BadRange)?;
        Ok((ptr as *const u8, len))
    }
}

impl VfsSharedIoMapper for RecvV3SharedIoMapper {
    fn with_read_reply_buffer<R>(
        &mut self,
        descriptor: VfsSharedBufferDescriptor,
        requested_len: u64,
        operation: impl FnOnce(&mut [u8]) -> R,
    ) -> Result<R, VfsSharedIoAdapterError> {
        let (ptr, len) = self.read_slice_ptr(descriptor, requested_len)?;
        // SAFETY: ptr is within a kernel-mapped region with WRITE permission;
        // mapping is live (not released); len bytes are within page_rounded_mapped_len.
        // Not safe in hosted-dev tests — see struct doc blocker note.
        let buf = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len) };
        Ok(operation(buf))
    }

    fn with_write_request_buffer<R>(
        &mut self,
        descriptor: VfsSharedBufferDescriptor,
        requested_len: u64,
        operation: impl FnOnce(&[u8]) -> R,
    ) -> Result<R, VfsSharedIoAdapterError> {
        let (ptr, len) = self.write_slice_ptr(descriptor, requested_len)?;
        // SAFETY: ptr is within a kernel-mapped region with READ permission;
        // mapping is live (not released); len bytes are within page_rounded_mapped_len.
        // Not safe in hosted-dev tests — see struct doc blocker note.
        let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
        Ok(operation(bytes))
    }

    fn release(
        &mut self,
        descriptor: VfsSharedBufferDescriptor,
    ) -> Result<(), VfsSharedIoAdapterError> {
        if descriptor.object_handle != self.cleanup_token
            || descriptor.object_generation != self.cleanup_token >> 16
        {
            return Err(VfsSharedIoAdapterError::StaleHandle);
        }
        if self.released {
            return Ok(());
        }
        self.released = true;
        // SAFETY: cleanup_token is from a recv_shared_v3 delivery;
        // released flag ensures at-most-once semantics.
        unsafe { release_v3_cleanup_token(self.cleanup_token) }
            .map_err(|_| VfsSharedIoAdapterError::ReleaseFailure)
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

// ── Stage 65: WRITE_SHARED_REQUEST ↔ recv_shared_v3 MAP_READ binding ─────────

/// Validation errors for the recv_shared_v3 → WRITE_SHARED_REQUEST binding.
///
/// Each variant maps to a distinct rejected field or constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsWriteSharedBindingError {
    /// cleanup_token is 0 (RECV_V3_CLEANUP_TOKEN_NONE): no live mapping.
    MissingCleanupToken,
    /// transferred_cap is u64::MAX (RECV_V3_NO_TRANSFER_CAP): no cap transferred.
    NoTransferCap,
    /// actual_mapping_perm is not MAP_PERM_READ_ONLY (1).
    MappingNotReadOnly,
    /// mapped_base is 0: mapping was not established.
    MappingNotEstablished,
    /// object_kind is not DmaRegion (5) or MemoryObject (1).
    UnsupportedObjectKind,
    /// descriptor.access is not VFS_SHARED_BUFFER_FS_READ.
    WrongDescriptorAccess,
    /// descriptor.object_handle does not equal cleanup_token.
    DescriptorHandleMismatch,
    /// descriptor.object_generation does not equal cleanup_token >> 16.
    DescriptorGenerationMismatch,
    /// page_rounded_mapped_len < buffer_offset + buffer_len.
    MappingRangeTooShort,
    /// exact_region_len is authoritative (> 0) and < buffer_offset + buffer_len.
    ExactRegionLenInsufficient,
    /// request_id is 0.
    ZeroRequestId,
}

/// Helper-only validated binding between a `recv_shared_v3` MAP_READ delivery and a
/// VFS `WRITE_SHARED_REQUEST` descriptor.
///
/// ## Binding contract
///
/// The requester encodes the kernel cleanup_token into the descriptor as follows:
/// - `descriptor.object_handle = cleanup_token` (the full 64-bit CapId)
/// - `descriptor.object_generation = cleanup_token >> 16` (the generation part)
///
/// The FS server validates this cross-reference on construction.  The binding carries
/// the validated descriptor so a mapper (`BorrowedSharedIoTestMapper` in tests) can
/// produce an immutable byte slice without granting write access.
///
/// ## Constraints enforced
///
/// - `actual_mapping_perm == 1` (MAP_READ): no write permission is ever granted.
/// - `descriptor.access == VFS_SHARED_BUFFER_FS_READ`: FS receives read-only direction.
/// - `descriptor.object_handle == cleanup_token`: binding cross-reference.
/// - `page_rounded_mapped_len` covers the full descriptor range.
/// - `exact_region_len` (if authoritative) covers the full descriptor range.
///
/// ## Not provided
///
/// - No mapping authority (bytes accessible only through a mapper).
/// - No process-exit or timeout signals.
/// - Not connected to live VFS dispatch.
pub struct VfsWriteSharedBinding {
    /// From recv_shared_v3 delivery.
    pub cleanup_token: u64,
    /// From recv_shared_v3 delivery.
    pub transferred_cap: u64,
    /// From recv_shared_v3 delivery (5 = DmaRegion, 1 = MemoryObject).
    pub object_kind: u32,
    /// From recv_shared_v3 delivery (0 = not authoritative).
    pub exact_region_len: u64,
    /// From recv_shared_v3 delivery.
    pub mapped_base: u64,
    /// From recv_shared_v3 delivery (page-rounded).
    pub page_rounded_mapped_len: u64,
    /// From VFS request.
    pub request_id: u64,
    /// From VFS request.
    pub fd: u64,
    /// From VFS request.
    pub file_offset: u64,
    /// From VFS request.
    pub requested_len: u64,
    /// Validated descriptor (object_handle = cleanup_token, access = FS_READ).
    descriptor: VfsSharedBufferDescriptor,
}

impl VfsWriteSharedBinding {
    const MAP_PERM_READ_ONLY: u32 = 1;
    const OBJECT_KIND_MEMORY_OBJECT: u32 = 1;
    const OBJECT_KIND_DMA_REGION: u32 = 5;
    const RECV_V3_CLEANUP_TOKEN_NONE: u64 = 0;
    const RECV_V3_NO_TRANSFER_CAP: u64 = u64::MAX;

    /// Validate a binding from raw recv_shared_v3 output fields and a VFS write request.
    ///
    /// All constraints must pass or the binding is rejected.  On success, the returned
    /// `VfsWriteSharedBinding` carries the validated descriptor for use with a mapper.
    pub fn validate(
        cleanup_token: u64,
        transferred_cap: u64,
        object_kind: u32,
        exact_region_len: u64,
        mapped_base: u64,
        page_rounded_mapped_len: u64,
        actual_mapping_perm: u32,
        request: &VfsWriteSharedRequest,
    ) -> Result<Self, VfsWriteSharedBindingError> {
        use VfsWriteSharedBindingError::*;

        if cleanup_token == Self::RECV_V3_CLEANUP_TOKEN_NONE {
            return Err(MissingCleanupToken);
        }
        if transferred_cap == Self::RECV_V3_NO_TRANSFER_CAP {
            return Err(NoTransferCap);
        }
        if actual_mapping_perm != Self::MAP_PERM_READ_ONLY {
            return Err(MappingNotReadOnly);
        }
        if mapped_base == 0 {
            return Err(MappingNotEstablished);
        }
        if object_kind != Self::OBJECT_KIND_MEMORY_OBJECT
            && object_kind != Self::OBJECT_KIND_DMA_REGION
        {
            return Err(UnsupportedObjectKind);
        }
        let d = request.buffer;
        if d.access != VFS_SHARED_BUFFER_FS_READ {
            return Err(WrongDescriptorAccess);
        }
        if d.object_handle != cleanup_token {
            return Err(DescriptorHandleMismatch);
        }
        if d.object_generation != cleanup_token >> 16 {
            return Err(DescriptorGenerationMismatch);
        }
        let range_end = d
            .buffer_offset
            .checked_add(d.buffer_len)
            .ok_or(MappingRangeTooShort)?;
        if page_rounded_mapped_len < range_end {
            return Err(MappingRangeTooShort);
        }
        if exact_region_len > 0 && exact_region_len < range_end {
            return Err(ExactRegionLenInsufficient);
        }
        if request.request_id == 0 {
            return Err(ZeroRequestId);
        }
        Ok(Self {
            cleanup_token,
            transferred_cap,
            object_kind,
            exact_region_len,
            mapped_base,
            page_rounded_mapped_len,
            request_id: request.request_id,
            fd: request.fd,
            file_offset: request.file_offset,
            requested_len: request.requested_len,
            descriptor: d,
        })
    }

    /// Returns the validated descriptor (object_handle == cleanup_token, access == FS_READ).
    pub const fn descriptor(&self) -> VfsSharedBufferDescriptor {
        self.descriptor
    }

    /// Returns `(generation, slot)` from the cleanup_token.
    ///
    /// `cleanup_token = (generation << 16) | slot_index` per the CapId encoding.
    pub const fn cleanup_token_parts(&self) -> (u64, u64) {
        (self.cleanup_token >> 16, self.cleanup_token & 0xFFFF)
    }
}

// ── Stage 69+70+72: READ_SHARED_REPLY ↔ recv_shared_v3 MAP_WRITE binding ─────
//
// Stage 72 removed the Stage 60 blanket WRITE gate. recv_shared_v3 with
// map_intent=0x3 now maps memory writably when the cap has CAP_RIGHT_WRITE.
// The production VFS route (VFS_READ_SHARED_REPLY_ENABLED) remains false
// pending RequesterExit signal delivery from kernel to VFS server.

/// Validation errors for the recv_shared_v3 → READ_SHARED_REPLY binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsReadSharedBindingError {
    /// cleanup_token is 0 (RECV_V3_CLEANUP_TOKEN_NONE): no live mapping.
    MissingCleanupToken,
    /// transferred_cap is u64::MAX (RECV_V3_NO_TRANSFER_CAP): no cap transferred.
    NoTransferCap,
    /// actual_mapping_perm does not have the write bit set (perm & 0x2 == 0).
    MappingNotWritable,
    /// actual_mapping_perm has the execute bit set (perm & 0x4 != 0).
    ExecutableMapping,
    /// mapped_base is 0: mapping was not established.
    MappingNotEstablished,
    /// object_kind is not DmaRegion (5) or MemoryObject (1).
    UnsupportedObjectKind,
    /// descriptor.access is not VFS_SHARED_BUFFER_FS_WRITE.
    WrongDescriptorAccess,
    /// descriptor.object_handle does not equal cleanup_token.
    DescriptorHandleMismatch,
    /// descriptor.object_generation does not equal cleanup_token >> 16.
    DescriptorGenerationMismatch,
    /// page_rounded_mapped_len < buffer_offset + buffer_len.
    MappingRangeTooShort,
    /// exact_region_len is authoritative (> 0) and < buffer_offset + buffer_len.
    ExactRegionLenInsufficient,
    /// request_id is 0.
    ZeroRequestId,
}

/// Helper-only validated binding between a `recv_shared_v3` MAP_WRITE delivery and a
/// VFS `READ_SHARED_REPLY` descriptor.
///
/// ## Binding contract
///
/// The requester encodes the kernel cleanup_token into the descriptor as follows:
/// - `descriptor.object_handle = cleanup_token` (the full 64-bit CapId)
/// - `descriptor.object_generation = cleanup_token >> 16` (the generation part)
///
/// ## Constraints enforced
///
/// - `actual_mapping_perm & 0x2 != 0` (MAP_WRITE bit present).
/// - `actual_mapping_perm & 0x4 == 0` (no execute bit).
/// - `descriptor.access == VFS_SHARED_BUFFER_FS_WRITE`: FS writes into the buffer.
/// - `descriptor.object_handle == cleanup_token`: binding cross-reference.
/// - `page_rounded_mapped_len` covers the full descriptor range.
/// - `exact_region_len` (if authoritative) covers the full descriptor range.
///
/// ## Kernel gate status
///
/// Stage 72 removed the Stage 60 WRITE gate.  `actual_mapping_perm = 3` is now
/// delivered by a live recv_shared_v3 call when the transferred cap carries write
/// rights.  The production VFS route remains gated behind `VFS_READ_SHARED_REPLY_ENABLED`.
pub struct VfsReadSharedBinding {
    pub cleanup_token: u64,
    pub transferred_cap: u64,
    pub object_kind: u32,
    pub exact_region_len: u64,
    pub mapped_base: u64,
    pub page_rounded_mapped_len: u64,
    pub request_id: u64,
    pub fd: u64,
    pub file_offset: u64,
    pub requested_len: u64,
    descriptor: VfsSharedBufferDescriptor,
}

impl VfsReadSharedBinding {
    const MAP_PERM_WRITE_BIT: u32 = 0x2;
    const MAP_PERM_EXEC_BIT: u32 = 0x4;
    const OBJECT_KIND_MEMORY_OBJECT: u32 = 1;
    const OBJECT_KIND_DMA_REGION: u32 = 5;
    const RECV_V3_CLEANUP_TOKEN_NONE: u64 = 0;
    const RECV_V3_NO_TRANSFER_CAP: u64 = u64::MAX;

    pub fn validate(
        cleanup_token: u64,
        transferred_cap: u64,
        object_kind: u32,
        exact_region_len: u64,
        mapped_base: u64,
        page_rounded_mapped_len: u64,
        actual_mapping_perm: u32,
        request: &VfsReadSharedRequest,
    ) -> Result<Self, VfsReadSharedBindingError> {
        use VfsReadSharedBindingError::*;

        if cleanup_token == Self::RECV_V3_CLEANUP_TOKEN_NONE {
            return Err(MissingCleanupToken);
        }
        if transferred_cap == Self::RECV_V3_NO_TRANSFER_CAP {
            return Err(NoTransferCap);
        }
        if actual_mapping_perm & Self::MAP_PERM_WRITE_BIT == 0 {
            return Err(MappingNotWritable);
        }
        if actual_mapping_perm & Self::MAP_PERM_EXEC_BIT != 0 {
            return Err(ExecutableMapping);
        }
        if mapped_base == 0 {
            return Err(MappingNotEstablished);
        }
        if object_kind != Self::OBJECT_KIND_MEMORY_OBJECT
            && object_kind != Self::OBJECT_KIND_DMA_REGION
        {
            return Err(UnsupportedObjectKind);
        }
        let d = request.buffer;
        if d.access != VFS_SHARED_BUFFER_FS_WRITE {
            return Err(WrongDescriptorAccess);
        }
        if d.object_handle != cleanup_token {
            return Err(DescriptorHandleMismatch);
        }
        if d.object_generation != cleanup_token >> 16 {
            return Err(DescriptorGenerationMismatch);
        }
        let range_end = d
            .buffer_offset
            .checked_add(d.buffer_len)
            .ok_or(MappingRangeTooShort)?;
        if page_rounded_mapped_len < range_end {
            return Err(MappingRangeTooShort);
        }
        if exact_region_len > 0 && exact_region_len < range_end {
            return Err(ExactRegionLenInsufficient);
        }
        if request.request_id == 0 {
            return Err(ZeroRequestId);
        }
        Ok(Self {
            cleanup_token,
            transferred_cap,
            object_kind,
            exact_region_len,
            mapped_base,
            page_rounded_mapped_len,
            request_id: request.request_id,
            fd: request.fd,
            file_offset: request.file_offset,
            requested_len: request.requested_len,
            descriptor: d,
        })
    }

    pub const fn descriptor(&self) -> VfsSharedBufferDescriptor {
        self.descriptor
    }

    pub const fn cleanup_token_parts(&self) -> (u64, u64) {
        (self.cleanup_token >> 16, self.cleanup_token & 0xFFFF)
    }
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
            0, // requester_tid: 0 in adapter tests
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
    fn production_mapper_rejects_both_directions_and_release() {
        let descriptor = VfsSharedBufferDescriptor::new(1, 1, 0, 4, VFS_SHARED_BUFFER_FS_WRITE);
        let mut mapper = UnsupportedSharedIoMapper;
        assert_eq!(
            mapper.with_read_reply_buffer(descriptor, 4, |_| ()),
            Err(VfsSharedIoAdapterError::UnsupportedMapping)
        );
        let write_descriptor = VfsSharedBufferDescriptor {
            access: VFS_SHARED_BUFFER_FS_READ,
            ..descriptor
        };
        assert_eq!(
            mapper.with_write_request_buffer(write_descriptor, 4, |_| ()),
            Err(VfsSharedIoAdapterError::UnsupportedMapping)
        );
        assert_eq!(
            mapper.release(descriptor),
            Err(VfsSharedIoAdapterError::UnsupportedMapping)
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

    // ── Stage 65: WRITE_SHARED_REQUEST ↔ recv_shared_v3 MAP_READ binding ────

    /// Build a minimal valid VfsWriteSharedRequest for Stage 65 binding tests.
    /// cleanup_token = (gen=1, slot=1) = 0x0001_0001.
    fn make_valid_request(cleanup_token: u64, requested_len: u64) -> VfsWriteSharedRequest {
        VfsWriteSharedRequest {
            fd: 3,
            file_offset: 0,
            requested_len,
            request_id: 42,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(
                cleanup_token,         // object_handle = cleanup_token (binding contract)
                cleanup_token >> 16,   // object_generation = generation part
                0,
                requested_len,
                VFS_SHARED_BUFFER_FS_READ,
            ),
        }
    }

    const TEST_TOKEN: u64 = 0x0001_0001; // gen=1, slot=1
    const TEST_MAPPED_BASE: u64 = 0x10_0000;
    const TEST_MAPPED_LEN: u64 = 4096;
    const TEST_REGION_LEN: u64 = 4096;
    const TEST_OBJECT_KIND: u32 = 5; // DmaRegion
    const TEST_TRANSFERRED_CAP: u64 = 7;
    const TEST_MAP_PERM: u32 = 1; // MAP_READ

    fn valid_binding(requested_len: u64) -> VfsWriteSharedBinding {
        VfsWriteSharedBinding::validate(
            TEST_TOKEN,
            TEST_TRANSFERRED_CAP,
            TEST_OBJECT_KIND,
            TEST_REGION_LEN,
            TEST_MAPPED_BASE,
            TEST_MAPPED_LEN,
            TEST_MAP_PERM,
            &make_valid_request(TEST_TOKEN, requested_len),
        )
        .expect("valid binding")
    }

    #[test]
    fn stage65_valid_write_shared_binding_accepted() {
        let b = valid_binding(8);
        assert_eq!(b.cleanup_token, TEST_TOKEN);
        assert_eq!(b.transferred_cap, TEST_TRANSFERRED_CAP);
        assert_eq!(b.object_kind, TEST_OBJECT_KIND);
        assert_eq!(b.mapped_base, TEST_MAPPED_BASE);
        assert_eq!(b.request_id, 42);
        assert_eq!(b.requested_len, 8);
        let (generation, slot) = b.cleanup_token_parts();
        assert_eq!(generation, 1);
        assert_eq!(slot, 1);
        assert_eq!(b.descriptor().object_handle, TEST_TOKEN);
        assert_eq!(b.descriptor().object_generation, 1);
        assert_eq!(b.descriptor().access, VFS_SHARED_BUFFER_FS_READ);
    }

    #[test]
    fn stage65_binding_rejects_zero_cleanup_token() {
        let req = make_valid_request(0, 8);
        // cleanup_token = 0 → MissingCleanupToken even though descriptor handle = 0
        let result = VfsWriteSharedBinding::validate(
            0, TEST_TRANSFERRED_CAP, TEST_OBJECT_KIND, TEST_REGION_LEN,
            TEST_MAPPED_BASE, TEST_MAPPED_LEN, TEST_MAP_PERM, &req,
        );
        assert_eq!(result.err(), Some(VfsWriteSharedBindingError::MissingCleanupToken));
    }

    #[test]
    fn stage65_binding_rejects_no_transfer_cap() {
        let req = make_valid_request(TEST_TOKEN, 8);
        let result = VfsWriteSharedBinding::validate(
            TEST_TOKEN, u64::MAX, TEST_OBJECT_KIND, TEST_REGION_LEN,
            TEST_MAPPED_BASE, TEST_MAPPED_LEN, TEST_MAP_PERM, &req,
        );
        assert_eq!(result.err(), Some(VfsWriteSharedBindingError::NoTransferCap));
    }

    #[test]
    fn stage65_binding_rejects_map_write_permission() {
        let req = make_valid_request(TEST_TOKEN, 8);
        let result = VfsWriteSharedBinding::validate(
            TEST_TOKEN, TEST_TRANSFERRED_CAP, TEST_OBJECT_KIND, TEST_REGION_LEN,
            TEST_MAPPED_BASE, TEST_MAPPED_LEN, 3, // MAP_READ|MAP_WRITE
            &req,
        );
        assert_eq!(result.err(), Some(VfsWriteSharedBindingError::MappingNotReadOnly));
    }

    #[test]
    fn stage65_binding_rejects_unmapped_zero_base() {
        let req = make_valid_request(TEST_TOKEN, 8);
        let result = VfsWriteSharedBinding::validate(
            TEST_TOKEN, TEST_TRANSFERRED_CAP, TEST_OBJECT_KIND, TEST_REGION_LEN,
            0, TEST_MAPPED_LEN, TEST_MAP_PERM, &req,
        );
        assert_eq!(result.err(), Some(VfsWriteSharedBindingError::MappingNotEstablished));
    }

    #[test]
    fn stage65_binding_rejects_unsupported_object_kind() {
        let req = make_valid_request(TEST_TOKEN, 8);
        let result = VfsWriteSharedBinding::validate(
            TEST_TOKEN, TEST_TRANSFERRED_CAP, 2, // Endpoint (unsupported)
            TEST_REGION_LEN, TEST_MAPPED_BASE, TEST_MAPPED_LEN, TEST_MAP_PERM, &req,
        );
        assert_eq!(result.err(), Some(VfsWriteSharedBindingError::UnsupportedObjectKind));
    }

    #[test]
    fn stage65_binding_accepts_memory_object_kind() {
        let req = make_valid_request(TEST_TOKEN, 8);
        let result = VfsWriteSharedBinding::validate(
            TEST_TOKEN, TEST_TRANSFERRED_CAP, 1, // MemoryObject
            TEST_REGION_LEN, TEST_MAPPED_BASE, TEST_MAPPED_LEN, TEST_MAP_PERM, &req,
        );
        assert!(result.is_ok(), "MemoryObject kind must be accepted");
    }

    #[test]
    fn stage65_binding_rejects_wrong_descriptor_access_fs_write() {
        let mut req = make_valid_request(TEST_TOKEN, 8);
        req.buffer.access = VFS_SHARED_BUFFER_FS_WRITE; // FS-WRITE not allowed for write request
        let result = VfsWriteSharedBinding::validate(
            TEST_TOKEN, TEST_TRANSFERRED_CAP, TEST_OBJECT_KIND, TEST_REGION_LEN,
            TEST_MAPPED_BASE, TEST_MAPPED_LEN, TEST_MAP_PERM, &req,
        );
        assert_eq!(result.err(), Some(VfsWriteSharedBindingError::WrongDescriptorAccess));
    }

    #[test]
    fn stage65_binding_rejects_descriptor_handle_mismatch() {
        let mut req = make_valid_request(TEST_TOKEN, 8);
        req.buffer.object_handle = TEST_TOKEN + 1; // wrong handle
        let result = VfsWriteSharedBinding::validate(
            TEST_TOKEN, TEST_TRANSFERRED_CAP, TEST_OBJECT_KIND, TEST_REGION_LEN,
            TEST_MAPPED_BASE, TEST_MAPPED_LEN, TEST_MAP_PERM, &req,
        );
        assert_eq!(result.err(), Some(VfsWriteSharedBindingError::DescriptorHandleMismatch));
    }

    #[test]
    fn stage65_binding_rejects_descriptor_generation_mismatch() {
        let mut req = make_valid_request(TEST_TOKEN, 8);
        req.buffer.object_generation = (TEST_TOKEN >> 16) + 1; // stale generation
        let result = VfsWriteSharedBinding::validate(
            TEST_TOKEN, TEST_TRANSFERRED_CAP, TEST_OBJECT_KIND, TEST_REGION_LEN,
            TEST_MAPPED_BASE, TEST_MAPPED_LEN, TEST_MAP_PERM, &req,
        );
        assert_eq!(result.err(), Some(VfsWriteSharedBindingError::DescriptorGenerationMismatch));
    }

    #[test]
    fn stage65_binding_rejects_range_exceeds_mapped_len() {
        // requested_len=4096, buffer_offset=1 → end=4097 > mapped_len=4096
        let mut req = make_valid_request(TEST_TOKEN, 4096);
        req.buffer.buffer_offset = 1;
        req.buffer.buffer_len = 4096;
        let result = VfsWriteSharedBinding::validate(
            TEST_TOKEN, TEST_TRANSFERRED_CAP, TEST_OBJECT_KIND, TEST_REGION_LEN,
            TEST_MAPPED_BASE, TEST_MAPPED_LEN, TEST_MAP_PERM, &req,
        );
        assert_eq!(result.err(), Some(VfsWriteSharedBindingError::MappingRangeTooShort));
    }

    #[test]
    fn stage65_binding_rejects_exact_region_len_insufficient() {
        // exact_region_len=4 < buffer_offset(0) + buffer_len(8) = 8
        let req = make_valid_request(TEST_TOKEN, 8);
        let result = VfsWriteSharedBinding::validate(
            TEST_TOKEN, TEST_TRANSFERRED_CAP, TEST_OBJECT_KIND,
            4, // exact_region_len authoritative and too small
            TEST_MAPPED_BASE, TEST_MAPPED_LEN, TEST_MAP_PERM, &req,
        );
        assert_eq!(result.err(), Some(VfsWriteSharedBindingError::ExactRegionLenInsufficient));
    }

    #[test]
    fn stage65_binding_nonauthoritative_exact_region_len_zero_accepted() {
        // exact_region_len = 0 means "not authoritative" — must be accepted even if < requested_len
        let req = make_valid_request(TEST_TOKEN, 8);
        let result = VfsWriteSharedBinding::validate(
            TEST_TOKEN, TEST_TRANSFERRED_CAP, TEST_OBJECT_KIND,
            0, // not authoritative
            TEST_MAPPED_BASE, TEST_MAPPED_LEN, TEST_MAP_PERM, &req,
        );
        assert!(result.is_ok(), "zero exact_region_len must be accepted (not authoritative)");
    }

    #[test]
    fn stage65_binding_rejects_zero_request_id() {
        let mut req = make_valid_request(TEST_TOKEN, 8);
        req.request_id = 0;
        let result = VfsWriteSharedBinding::validate(
            TEST_TOKEN, TEST_TRANSFERRED_CAP, TEST_OBJECT_KIND, TEST_REGION_LEN,
            TEST_MAPPED_BASE, TEST_MAPPED_LEN, TEST_MAP_PERM, &req,
        );
        assert_eq!(result.err(), Some(VfsWriteSharedBindingError::ZeroRequestId));
    }

    #[test]
    fn stage65_ramfs_consumes_immutable_bytes_via_binding_and_mapper() {
        // Stage 65 end-to-end: binding validates recv_shared_v3 → descriptor → mapper →
        // immutable bytes → RAMFS write → verified content.
        let b = valid_binding(8);
        // BorrowedSharedIoTestMapper is seeded with cleanup_token as object_handle and
        // (cleanup_token >> 16) as object_generation — exactly the binding contract.
        let mut storage = *b"hello65!"; // 8 bytes of "shared write data"
        let mut mapper = BorrowedSharedIoTestMapper::new(
            b.cleanup_token,
            b.cleanup_token >> 16,
            &mut storage,
        );

        let mut ramfs = RamFsBackend::new();
        ramfs.create_file(b"/stage65").expect("create");
        let fd = ramfs.open_path(b"/stage65").expect("open");

        let bytes_written = mapper
            .with_write_request_buffer(b.descriptor(), b.requested_len, |bytes| {
                ramfs.write_bytes(fd, bytes).map(|_| bytes.len())
            })
            .expect("mapper access")
            .expect("ramfs write");
        assert_eq!(bytes_written, 8);
        ramfs.close_fd(fd).expect("close write fd");

        let read_fd = ramfs.open_path(b"/stage65").expect("open for read");
        let mut buf = [0u8; 8];
        let n = ramfs.read_bytes(read_fd, &mut buf).expect("read");
        ramfs.close_fd(read_fd).expect("close read fd");
        assert_eq!(&buf[..n], b"hello65!");
    }

    #[test]
    fn stage65_mapper_rejects_write_access_to_write_request_buffer() {
        // Prove that BorrowedSharedIoTestMapper with_read_reply_buffer is rejected
        // when the mapper object_handle/generation matches — it requires FS_WRITE access
        // but the descriptor only carries FS_READ.  This proves direction safety.
        let b = valid_binding(8);
        let mut storage = [0u8; 8];
        let mut mapper = BorrowedSharedIoTestMapper::new(
            b.cleanup_token,
            b.cleanup_token >> 16,
            &mut storage,
        );
        // with_read_reply_buffer requires FS_WRITE; our descriptor has FS_READ → rejected.
        let result = mapper.with_read_reply_buffer(b.descriptor(), b.requested_len, |_| ());
        assert!(
            matches!(result, Err(VfsSharedIoAdapterError::WrongDirection)),
            "read-reply direction must be rejected for a write-request descriptor"
        );
    }

    #[test]
    fn stage65_cleanup_idempotent_after_success() {
        // Prove the lifecycle cleanup-idempotency contract holds for WRITE_SHARED_REQUEST.
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let lc = lifecycle(&mut handles, VfsSharedIoDirection::WriteRequest, 8, 0, 0);
        let mut lc = lc;
        lc.map(&handles).expect("map");
        lc.begin().expect("begin");
        lc.complete(8).expect("complete");
        let first = lc.cleanup(&mut handles, VfsSharedIoTerminalReason::Success).expect("cleanup");
        let dup = lc.cleanup(&mut handles, VfsSharedIoTerminalReason::BackendError).expect("dup");
        assert_eq!(first, VfsSharedIoCleanupResult::Won(VfsSharedIoTerminalReason::Success));
        assert_eq!(dup, VfsSharedIoCleanupResult::AlreadyCleaned(VfsSharedIoTerminalReason::Success));
    }

    #[test]
    fn stage65_cleanup_before_fallback_required_for_write_request() {
        // Inline fallback must be rejected before cleanup even if the flag is set.
        let mut handles = VfsSharedIoHandleTable::<1>::new();
        let lc = lifecycle(
            &mut handles,
            VfsSharedIoDirection::WriteRequest,
            8,
            0,
            VFS_SHARED_IO_F_ALLOW_INLINE_FALLBACK,
        );
        let mut lc = lc;
        lc.map(&handles).expect("map");
        lc.begin().expect("begin");
        assert_eq!(
            lc.begin_inline_fallback(),
            Err(VfsSharedIoLifecycleError::FallbackBeforeCleanup),
            "fallback before cleanup must be rejected"
        );
        lc.cleanup(&mut handles, VfsSharedIoTerminalReason::Timeout).expect("cleanup");
        assert_eq!(lc.begin_inline_fallback(), Ok(()), "fallback after timeout cleanup must succeed");
    }

    #[test]
    fn stage65_production_mapper_rejects_write_shared_request() {
        // The production UnsupportedSharedIoMapper must reject even valid descriptors —
        // live WRITE_SHARED_REQUEST mapping is not implemented.
        let b = valid_binding(8);
        let result = UnsupportedSharedIoMapper.with_write_request_buffer(
            b.descriptor(),
            b.requested_len,
            |_| (),
        );
        assert_eq!(result, Err(VfsSharedIoAdapterError::UnsupportedMapping));
    }

    #[test]
    fn stage65_read_shared_reply_still_unsupported_by_production_mapper() {
        // READ_SHARED_REPLY remains blocked; both directions are unsupported in production.
        let descriptor = VfsSharedBufferDescriptor::new(
            TEST_TOKEN, TEST_TOKEN >> 16, 0, 8, VFS_SHARED_BUFFER_FS_WRITE,
        );
        let result = UnsupportedSharedIoMapper.with_read_reply_buffer(descriptor, 8, |_| ());
        assert_eq!(result, Err(VfsSharedIoAdapterError::UnsupportedMapping));
    }

    #[test]
    fn stage65_vfs_shared_io_enabled_remains_disabled() {
        // UnsupportedSharedIoMapper is the production default; live shared-I/O opcodes
        // remain unsupported.  Confirmed by production_mapper_rejects_write_shared_request
        // and read_shared_reply_still_unsupported_by_production_mapper above.
        assert_eq!(
            UnsupportedSharedIoMapper.release(VfsSharedBufferDescriptor::new(1, 1, 0, 1, VFS_SHARED_BUFFER_FS_READ)),
            Err(VfsSharedIoAdapterError::UnsupportedMapping),
            "UnsupportedSharedIoMapper must reject release — live mapping is disabled"
        );
    }

    // ── Stage 79: RecvV3SharedIoMapper unit tests ──────────────────────────────
    //
    // These tests cover only validation error paths that fail before from_raw_parts.
    // Byte content access and successful release require a real YARM kernel mapping.
    // In hosted-dev, release_v3_cleanup_token invokes a Linux syscall (NR 4 = write)
    // that fails; release() returns ReleaseFailure but sets the released flag.

    const TOKEN_79A: u64 = 0x0009_0007; // gen=9, slot=7
    const BASE_79A: u64 = 0xB000;       // fake VA — not valid in hosted-dev
    const LEN_79A: u64 = 4096;
    const PERM_RO_79A: u32 = 1;
    const PERM_RW_79A: u32 = 3;

    fn wr_desc_79(token: u64, offset: u64, len: u64) -> VfsSharedBufferDescriptor {
        VfsSharedBufferDescriptor::new(token, token >> 16, offset, len, VFS_SHARED_BUFFER_FS_READ)
    }

    fn rd_desc_79(token: u64, offset: u64, len: u64) -> VfsSharedBufferDescriptor {
        VfsSharedBufferDescriptor::new(token, token >> 16, offset, len, VFS_SHARED_BUFFER_FS_WRITE)
    }

    #[test]
    fn stage79_recv_v3_mapper_from_delivery_constructs_with_all_fields() {
        use yarm_user_rt::syscall::recv_v3::RecvSharedV3Delivery;
        let d = RecvSharedV3Delivery {
            cleanup_token: TOKEN_79A,
            mapped_base: BASE_79A,
            page_rounded_mapped_len: LEN_79A,
            actual_mapping_perm: PERM_RO_79A,
            ..RecvSharedV3Delivery::default()
        };
        let m = RecvV3SharedIoMapper::from_delivery(&d);
        assert!(!m.is_released());
    }

    #[test]
    fn stage79_recv_v3_mapper_from_fields_is_not_released() {
        let m = RecvV3SharedIoMapper::from_fields(TOKEN_79A, BASE_79A, LEN_79A, PERM_RO_79A);
        assert!(!m.is_released());
    }

    #[test]
    fn stage79_write_request_wrong_direction_rejected() {
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN_79A, BASE_79A, LEN_79A, PERM_RO_79A);
        // FS_WRITE access on a write-request mapper (expects FS_READ) → WrongDirection
        let desc = VfsSharedBufferDescriptor::new(
            TOKEN_79A, TOKEN_79A >> 16, 0, 8, VFS_SHARED_BUFFER_FS_WRITE,
        );
        assert_eq!(
            m.with_write_request_buffer(desc, 8, |_| ()),
            Err(VfsSharedIoAdapterError::WrongDirection)
        );
    }

    #[test]
    fn stage79_write_request_stale_handle_rejected() {
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN_79A, BASE_79A, LEN_79A, PERM_RO_79A);
        let desc = VfsSharedBufferDescriptor::new(
            TOKEN_79A + 1, TOKEN_79A >> 16, 0, 8, VFS_SHARED_BUFFER_FS_READ,
        );
        assert_eq!(
            m.with_write_request_buffer(desc, 8, |_| ()),
            Err(VfsSharedIoAdapterError::StaleHandle)
        );
    }

    #[test]
    fn stage79_write_request_stale_generation_rejected() {
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN_79A, BASE_79A, LEN_79A, PERM_RO_79A);
        let desc = VfsSharedBufferDescriptor::new(
            TOKEN_79A, (TOKEN_79A >> 16) + 1, 0, 8, VFS_SHARED_BUFFER_FS_READ,
        );
        assert_eq!(
            m.with_write_request_buffer(desc, 8, |_| ()),
            Err(VfsSharedIoAdapterError::StaleHandle)
        );
    }

    #[test]
    fn stage79_write_request_rw_perm_rejected() {
        // Write requests require MAP_READ_ONLY (perm=1); RW perm → MissingRights.
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN_79A, BASE_79A, LEN_79A, PERM_RW_79A);
        assert_eq!(
            m.with_write_request_buffer(wr_desc_79(TOKEN_79A, 0, 8), 8, |_| ()),
            Err(VfsSharedIoAdapterError::MissingRights)
        );
    }

    #[test]
    fn stage79_write_request_bad_range_rejected() {
        // page_rounded_mapped_len=8; offset=1 + len=8 → end=9 > 8 → BadRange.
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN_79A, BASE_79A, 8, PERM_RO_79A);
        let desc = VfsSharedBufferDescriptor::new(
            TOKEN_79A, TOKEN_79A >> 16, 1, 8, VFS_SHARED_BUFFER_FS_READ,
        );
        assert_eq!(
            m.with_write_request_buffer(desc, 8, |_| ()),
            Err(VfsSharedIoAdapterError::BadRange)
        );
    }

    #[test]
    fn stage79_read_reply_wrong_direction_rejected() {
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN_79A, BASE_79A, LEN_79A, PERM_RW_79A);
        // FS_READ access on a read-reply mapper (expects FS_WRITE) → WrongDirection.
        let desc = VfsSharedBufferDescriptor::new(
            TOKEN_79A, TOKEN_79A >> 16, 0, 8, VFS_SHARED_BUFFER_FS_READ,
        );
        assert_eq!(
            m.with_read_reply_buffer(desc, 8, |_| ()),
            Err(VfsSharedIoAdapterError::WrongDirection)
        );
    }

    #[test]
    fn stage79_read_reply_readonly_perm_rejected() {
        // Read replies require MAP_WRITE bit; RO perm (no write bit) → MissingRights.
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN_79A, BASE_79A, LEN_79A, PERM_RO_79A);
        assert_eq!(
            m.with_read_reply_buffer(rd_desc_79(TOKEN_79A, 0, 8), 8, |_| ()),
            Err(VfsSharedIoAdapterError::MissingRights)
        );
    }

    #[test]
    fn stage79_release_stale_handle_rejected() {
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN_79A, BASE_79A, LEN_79A, PERM_RO_79A);
        let wrong = VfsSharedBufferDescriptor::new(
            TOKEN_79A + 1, TOKEN_79A >> 16, 0, 8, VFS_SHARED_BUFFER_FS_READ,
        );
        assert_eq!(m.release(wrong), Err(VfsSharedIoAdapterError::StaleHandle));
        assert!(!m.is_released(), "stale-handle release must not mark as released");
    }

    #[test]
    fn stage79_release_marks_released_and_blocks_subsequent_access() {
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN_79A, BASE_79A, LEN_79A, PERM_RO_79A);
        let desc = wr_desc_79(TOKEN_79A, 0, 8);
        let r = m.release(desc);
        // In hosted-dev: release_v3_cleanup_token invokes a Linux syscall that fails
        // (RCX = return address ≠ 0 → decode_release sees error → ReleaseFailure).
        // In production: Ok(()) when the kernel mapping is live.
        assert!(
            r == Ok(()) || r == Err(VfsSharedIoAdapterError::ReleaseFailure),
            "release must return Ok or ReleaseFailure; got {r:?}"
        );
        assert!(m.is_released(), "released flag must be set after first release attempt");
        assert_eq!(
            m.with_write_request_buffer(desc, 8, |_| ()),
            Err(VfsSharedIoAdapterError::AccessAfterCleanup),
            "access after release must return AccessAfterCleanup"
        );
    }

    #[test]
    fn stage79_release_idempotent_second_call_returns_ok() {
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN_79A, BASE_79A, LEN_79A, PERM_RO_79A);
        let desc = wr_desc_79(TOKEN_79A, 0, 8);
        let _ = m.release(desc); // first call (may fail in hosted-dev; sets released=true)
        assert!(m.is_released());
        assert_eq!(m.release(desc), Ok(()), "second release must return Ok without syscall");
    }
}

// ── Stage 83: RecvV3SharedIoMapper byte-access proof via heap backing ─────────
//
// Stage 79 left byte-access tests deferred (fake VA = UB). Stage 83 supplies a
// real heap Vec<u8> as mapped_base so from_raw_parts[_mut] is valid.
// Release calls release_v3_cleanup_token (NR 4 = write on Linux) with an
// invalid fd; the syscall fails but the released flag is set beforehand.
#[cfg(test)]
mod stage83_adapter_tests {
    use super::*;
    use alloc::vec;

    const TOKEN83A: u64 = 0x0083_0010; // gen=0x83, slot=0x10

    fn wr_d(token: u64, offset: u64, len: u64) -> VfsSharedBufferDescriptor {
        VfsSharedBufferDescriptor::new(
            token, token >> 16, offset, len, VFS_SHARED_BUFFER_FS_READ,
        )
    }

    fn rr_d(token: u64, offset: u64, len: u64) -> VfsSharedBufferDescriptor {
        VfsSharedBufferDescriptor::new(
            token, token >> 16, offset, len, VFS_SHARED_BUFFER_FS_WRITE,
        )
    }

    #[test]
    fn stage83_write_request_heap_backing_byte_roundtrip() {
        // with_write_request_buffer constructs a valid &[u8] from a real heap pointer.
        // Proves the unsafe slice creation is defined when mapped_base is a heap VA.
        let mut backing = vec![0u8; 64];
        backing[..8].copy_from_slice(b"stage83a");
        let ptr = backing.as_ptr() as u64;
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN83A, ptr, 64, 1);
        let desc = wr_d(TOKEN83A, 0, 8);
        let mut observed = [0u8; 8];
        m.with_write_request_buffer(desc, 8, |bytes| {
            observed.copy_from_slice(bytes);
        })
        .expect("write request with heap backing must succeed");
        assert_eq!(&observed, b"stage83a");
    }

    #[test]
    fn stage83_read_reply_heap_backing_byte_roundtrip() {
        // with_read_reply_buffer constructs a valid &mut [u8] from a real heap pointer.
        // Proves the unsafe mutable slice creation is defined when mapped_base is a heap VA.
        let mut backing = vec![0u8; 64];
        let ptr = backing.as_mut_ptr() as u64;
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN83A + 1, ptr, 64, 3);
        let desc = rr_d(TOKEN83A + 1, 0, 8);
        m.with_read_reply_buffer(desc, 8, |buf| {
            buf.copy_from_slice(b"stage83b");
        })
        .expect("read reply with heap backing must succeed");
        assert_eq!(&backing[..8], b"stage83b");
    }

    #[test]
    fn stage83_write_request_offset_within_heap_backing() {
        // Slice with nonzero buffer_offset indexes into the heap backing correctly.
        let mut backing = vec![0u8; 64];
        backing[16..24].copy_from_slice(b"offset83");
        let ptr = backing.as_ptr() as u64;
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN83A + 2, ptr, 64, 1);
        let desc = wr_d(TOKEN83A + 2, 16, 8);
        let mut observed = [0u8; 8];
        m.with_write_request_buffer(desc, 8, |bytes| {
            observed.copy_from_slice(bytes);
        })
        .expect("offset slice must succeed");
        assert_eq!(&observed, b"offset83");
    }

    #[test]
    fn stage83_release_sets_flag_before_syscall() {
        // is_released() returns true after release(), even though the syscall fails in hosted-dev.
        let mut backing = vec![0u8; 64];
        let ptr = backing.as_ptr() as u64;
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN83A + 3, ptr, 64, 1);
        assert!(!m.is_released());
        let desc = wr_d(TOKEN83A + 3, 0, 8);
        let _ = m.release(desc);
        assert!(m.is_released());
    }

    #[test]
    fn stage83_access_after_release_rejected_with_heap_backing() {
        let mut backing = vec![0u8; 64];
        let ptr = backing.as_ptr() as u64;
        let mut m = RecvV3SharedIoMapper::from_fields(TOKEN83A + 4, ptr, 64, 1);
        let desc = wr_d(TOKEN83A + 4, 0, 8);
        let _ = m.release(desc);
        assert_eq!(
            m.with_write_request_buffer(desc, 8, |_| ()),
            Err(VfsSharedIoAdapterError::AccessAfterCleanup),
        );
    }
}
