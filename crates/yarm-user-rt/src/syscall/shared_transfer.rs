// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Typed userspace wrappers for the frozen receive-map and transfer-release ABI.
//!
//! This module does not validate transferred object kind, rights, or exact region size and does not
//! provide slices. It is a low-level wrapper layer, not a production shared-I/O mapper.

use super::{SYSCALL_IPC_RECV_NR, SYSCALL_NO_TRANSFER_CAP, SyscallError, decode_syscall_error};
use crate::arch::SyscallReturn;

/// Existing frozen syscall number for receiver-side transfer release.
pub const SYSCALL_TRANSFER_RELEASE_NR: usize = 4;
/// Existing `IpcRecv` argument-4 bit requesting a readable shared mapping.
pub const SYSCALL_RECV_MAP_INTENT_READ: usize = 0x1;
/// Existing `IpcRecv` argument-4 bit adding write access to a shared mapping.
pub const SYSCALL_RECV_MAP_INTENT_WRITE: usize = 0x2;

/// Frozen receive-time mapping modes accepted by `IpcRecv` argument 4.
///
/// `DefaultReadWrite` preserves the historical zero-valued behavior. New code should prefer an
/// explicit direction so the receiver does not accidentally obtain write access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum IpcRecvMapIntent {
    DefaultReadWrite = 0,
    ReadOnly = SYSCALL_RECV_MAP_INTENT_READ,
    ReadWrite = SYSCALL_RECV_MAP_INTENT_READ | SYSCALL_RECV_MAP_INTENT_WRITE,
}

impl IpcRecvMapIntent {
    #[inline]
    pub const fn bits(self) -> usize {
        self as usize
    }

    #[inline]
    pub const fn permits_write(self) -> bool {
        matches!(self, Self::DefaultReadWrite | Self::ReadWrite)
    }
}

/// Metadata returned by the frozen receive-time shared-transfer register ABI.
///
/// The mapping base is the target supplied by the caller. `mapped_len` is the page-rounded length
/// returned in result lane 1. The frozen cross-architecture ABI does not return the exact unrounded
/// `SharedMemoryRegion::len`, so this type deliberately makes no such claim.
#[derive(Debug, PartialEq, Eq)]
pub struct MappedTransferRecv {
    sender_tid: u64,
    local_transfer_cap: u32,
    mapped_base: usize,
    mapped_len: usize,
    intent: IpcRecvMapIntent,
}

impl MappedTransferRecv {
    #[inline]
    pub const fn sender_tid(&self) -> u64 {
        self.sender_tid
    }

    #[inline]
    pub const fn local_transfer_cap(&self) -> u32 {
        self.local_transfer_cap
    }

    #[inline]
    pub const fn mapped_base(&self) -> usize {
        self.mapped_base
    }

    #[inline]
    pub const fn mapped_len(&self) -> usize {
        self.mapped_len
    }

    #[inline]
    pub const fn intent(&self) -> IpcRecvMapIntent {
        self.intent
    }

    /// Consume the receive result into an explicit, metadata-only release token.
    #[inline]
    pub const fn release_token(self) -> TransferReleaseToken {
        TransferReleaseToken::new(TransferReleaseRequest::explicit(
            self.local_transfer_cap,
            self.mapped_base,
            self.mapped_len,
        ))
    }
}

/// Exact frozen register arguments for syscall 4 (`TransferRelease`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferReleaseRequest {
    pub local_transfer_cap: u32,
    pub mapped_base: usize,
    pub mapped_len: usize,
}

impl TransferReleaseRequest {
    #[inline]
    pub const fn explicit(local_transfer_cap: u32, mapped_base: usize, mapped_len: usize) -> Self {
        Self {
            local_transfer_cap,
            mapped_base,
            mapped_len,
        }
    }

    /// Ask the kernel to use the active mapping record associated with the local cap.
    #[inline]
    pub const fn active(local_transfer_cap: u32) -> Self {
        Self::explicit(local_transfer_cap, 0, 0)
    }

    #[inline]
    const fn syscall_args(self) -> [usize; 6] {
        [
            self.local_transfer_cap as usize,
            self.mapped_base,
            self.mapped_len,
            0,
            0,
            0,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferReleaseResult {
    pub released_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferReleaseError {
    Syscall(SyscallError),
    AlreadyReleased,
}

/// Metadata-only at-most-once successful release guard.
///
/// This guard intentionally exposes no slice: the current userspace ABI cannot prove object kind,
/// rights, or exact transferred region length. Drop performs no syscall; callers must explicitly
/// release and handle failure. A failed syscall leaves the token retryable.
#[must_use = "a mapped transfer must be explicitly released"]
#[derive(Debug, PartialEq, Eq)]
pub struct TransferReleaseToken {
    request: TransferReleaseRequest,
    released: bool,
}

impl TransferReleaseToken {
    #[inline]
    const fn new(request: TransferReleaseRequest) -> Self {
        Self {
            request,
            released: false,
        }
    }

    /// Build a token for the kernel's active-mapping-record release path.
    #[inline]
    pub const fn active(local_transfer_cap: u32) -> Self {
        Self::new(TransferReleaseRequest::active(local_transfer_cap))
    }

    #[inline]
    pub const fn request(&self) -> TransferReleaseRequest {
        self.request
    }

    #[inline]
    pub const fn is_released(&self) -> bool {
        self.released
    }

    #[inline]
    pub fn release(&mut self) -> Result<TransferReleaseResult, TransferReleaseError> {
        self.release_with(|request| {
            // SAFETY: the token was constructed from caller-provided release metadata; the kernel
            // validates the cap and active mapping record before unmapping.
            unsafe { transfer_release(request) }.map_err(TransferReleaseError::Syscall)
        })
    }

    #[inline]
    fn release_with(
        &mut self,
        release: impl FnOnce(
            TransferReleaseRequest,
        ) -> Result<TransferReleaseResult, TransferReleaseError>,
    ) -> Result<TransferReleaseResult, TransferReleaseError> {
        if self.released {
            return Err(TransferReleaseError::AlreadyReleased);
        }
        let result = release(self.request)?;
        self.released = true;
        Ok(result)
    }
}

#[inline]
fn recv_with_map_intent_args(
    ep_cap: u32,
    mapping_target: *mut u8,
    mapping_capacity: usize,
    intent: IpcRecvMapIntent,
) -> [usize; 6] {
    [
        ep_cap as usize,
        mapping_target as usize,
        mapping_capacity,
        0,
        intent.bits(),
        0,
    ]
}

#[inline]
fn validate_mapping_target(
    mapping_target: *mut u8,
    mapping_capacity: usize,
) -> Result<(), SyscallError> {
    if mapping_target.is_null()
        || !(mapping_target as usize).is_multiple_of(crate::vm::PAGE_SIZE)
        || mapping_capacity == 0
    {
        Err(SyscallError::InvalidArgs)
    } else {
        Ok(())
    }
}

#[inline]
fn validate_release_request(request: TransferReleaseRequest) -> Result<(), SyscallError> {
    let active_record = request.mapped_base == 0 && request.mapped_len == 0;
    let explicit = request.mapped_base != 0
        && request.mapped_base.is_multiple_of(crate::vm::PAGE_SIZE)
        && request.mapped_len != 0;
    if active_record || explicit {
        Ok(())
    } else {
        Err(SyscallError::InvalidArgs)
    }
}

#[inline]
fn decode_mapped_recv(
    ret: SyscallReturn,
    mapping_target: *mut u8,
    intent: IpcRecvMapIntent,
) -> Result<Option<MappedTransferRecv>, SyscallError> {
    #[cfg(target_arch = "x86_64")]
    if ret.error != 0 {
        let err = decode_syscall_error(ret.error);
        return if matches!(err, SyscallError::WouldBlock) {
            Ok(None)
        } else {
            Err(err)
        };
    }
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    if ret.ret1 == 0 {
        let err = decode_syscall_error(ret.ret0);
        return if matches!(err, SyscallError::WouldBlock) {
            Ok(None)
        } else {
            Err(err)
        };
    }
    if ret.ret1 == 0 || ret.ret2 as u64 == SYSCALL_NO_TRANSFER_CAP {
        return Err(SyscallError::Internal);
    }
    let local_transfer_cap = u32::try_from(ret.ret2).map_err(|_| SyscallError::Internal)?;
    Ok(Some(MappedTransferRecv {
        sender_tid: ret.ret0 as u64,
        local_transfer_cap,
        mapped_base: mapping_target as usize,
        mapped_len: ret.ret1,
        intent,
    }))
}

#[inline]
fn decode_release(ret: SyscallReturn) -> Result<TransferReleaseResult, SyscallError> {
    #[cfg(target_arch = "x86_64")]
    if ret.error != 0 {
        return Err(decode_syscall_error(ret.error));
    }
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    if ret.ret0 < crate::vm::PAGE_SIZE || !ret.ret0.is_multiple_of(crate::vm::PAGE_SIZE) {
        return Err(decode_syscall_error(ret.ret0));
    }
    if ret.ret0 == 0 {
        return Err(SyscallError::Internal);
    }
    Ok(TransferReleaseResult {
        released_len: ret.ret0,
    })
}

/// Receive one shared-memory transfer using the frozen legacy `IpcRecv` map-intent layout.
///
/// # Safety
///
/// The endpoint protocol must guarantee that the next delivered message is `OPCODE_SHARED_MEM`
/// with a transferred MemoryObject capability, and `mapping_target` must designate a page-aligned,
/// writable, currently-unmapped userspace range of at least `mapping_capacity` bytes. The frozen
/// legacy result does not return an opcode or exact unrounded region length, so this wrapper cannot
/// verify that protocol itself.
///
/// This wrapper does not use or modify recv-v2. The frozen recv-v2 metadata-length lane overlaps the
/// shared-memory map-intent lane, so combining those layouts would be ambiguous.
#[inline]
pub unsafe fn ipc_recv_transfer_with_map_intent(
    ep_cap: u32,
    mapping_target: *mut u8,
    mapping_capacity: usize,
    intent: IpcRecvMapIntent,
) -> Result<Option<MappedTransferRecv>, SyscallError> {
    validate_mapping_target(mapping_target, mapping_capacity)?;
    let args = recv_with_map_intent_args(ep_cap, mapping_target, mapping_capacity, intent);
    // SAFETY: Uses the already-frozen legacy IpcRecv register ABI.
    let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_RECV_NR, args) };
    decode_mapped_recv(ret, mapping_target, intent)
}

/// Release an auto-mapped transferred object using syscall 4's frozen register layout.
///
/// `TransferReleaseRequest::active` selects the active-record fast path. Explicit requests must
/// provide the same local cap/base/length tuple returned or established by receive-time mapping.
#[inline]
pub unsafe fn transfer_release(
    request: TransferReleaseRequest,
) -> Result<TransferReleaseResult, SyscallError> {
    validate_release_request(request)?;
    let ret =
        unsafe { crate::arch::raw_syscall(SYSCALL_TRANSFER_RELEASE_NR, request.syscall_args()) };
    decode_release(ret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_intent_bits_match_frozen_abi() {
        assert_eq!(IpcRecvMapIntent::DefaultReadWrite.bits(), 0);
        assert_eq!(IpcRecvMapIntent::ReadOnly.bits(), 0x1);
        assert_eq!(IpcRecvMapIntent::ReadWrite.bits(), 0x3);
        assert!(!IpcRecvMapIntent::ReadOnly.permits_write());
        assert!(IpcRecvMapIntent::ReadWrite.permits_write());
    }

    #[test]
    fn mapped_receive_register_layout_matches_frozen_abi() {
        let args = recv_with_map_intent_args(
            17,
            0x4000_0000usize as *mut u8,
            0x20_000,
            IpcRecvMapIntent::ReadOnly,
        );
        assert_eq!(args, [17, 0x4000_0000, 0x20_000, 0, 0x1, 0]);
    }

    #[test]
    fn frozen_return_lanes_decode_to_partial_mapping_and_release_metadata() {
        let target = 0x4000_0000usize as *mut u8;
        let received = decode_mapped_recv(
            SyscallReturn {
                ret0: 9,
                ret1: 0x2000,
                ret2: 27,
                ret3: 0,
                ret4: 0,
                ret5: 0,
                error: 0,
            },
            target,
            IpcRecvMapIntent::ReadOnly,
        )
        .expect("decode receive")
        .expect("mapped receive");
        assert_eq!(received.sender_tid(), 9);
        assert_eq!(received.local_transfer_cap(), 27);
        assert_eq!(received.mapped_base(), target as usize);
        assert_eq!(received.mapped_len(), 0x2000);
        assert_eq!(received.intent(), IpcRecvMapIntent::ReadOnly);

        assert_eq!(
            decode_release(SyscallReturn {
                ret0: 0x2000,
                ret1: 0,
                ret2: 0,
                ret3: 0,
                ret4: 0,
                ret5: 0,
                error: 0,
            }),
            Ok(TransferReleaseResult {
                released_len: 0x2000
            })
        );
    }

    #[test]
    fn obvious_bad_mapping_and_release_arguments_are_rejected_before_syscall() {
        assert_eq!(
            validate_mapping_target(core::ptr::null_mut(), 4096),
            Err(SyscallError::InvalidArgs)
        );
        assert_eq!(
            validate_mapping_target(0x4000_0001usize as *mut u8, 4096),
            Err(SyscallError::InvalidArgs)
        );
        assert_eq!(
            validate_release_request(TransferReleaseRequest::explicit(1, 0, 4096)),
            Err(SyscallError::InvalidArgs)
        );
        assert_eq!(
            validate_release_request(TransferReleaseRequest::explicit(1, 0x4000_0001, 4096)),
            Err(SyscallError::InvalidArgs)
        );
        assert_eq!(
            validate_release_request(TransferReleaseRequest::active(1)),
            Ok(())
        );
    }

    #[test]
    fn release_register_layout_matches_frozen_abi() {
        let explicit = TransferReleaseRequest::explicit(23, 0x5000_0000, 0x3000);
        assert_eq!(explicit.syscall_args(), [23, 0x5000_0000, 0x3000, 0, 0, 0]);
        assert_eq!(
            TransferReleaseRequest::active(23).syscall_args(),
            [23, 0, 0, 0, 0, 0]
        );
        assert_eq!(SYSCALL_TRANSFER_RELEASE_NR, 4);
    }

    #[test]
    fn release_token_is_at_most_once_and_failure_is_retryable() {
        let mut token = TransferReleaseToken::active(41);
        let failed =
            token.release_with(|_| Err(TransferReleaseError::Syscall(SyscallError::Internal)));
        assert_eq!(
            failed,
            Err(TransferReleaseError::Syscall(SyscallError::Internal))
        );
        assert!(!token.is_released());

        let mut calls = 0;
        let released = token.release_with(|request| {
            calls += 1;
            assert_eq!(request, TransferReleaseRequest::active(41));
            Ok(TransferReleaseResult { released_len: 4096 })
        });
        assert_eq!(released, Ok(TransferReleaseResult { released_len: 4096 }));
        assert_eq!(calls, 1);
        assert!(token.is_released());
        assert_eq!(
            token.release_with(|_| panic!("duplicate release must not call syscall")),
            Err(TransferReleaseError::AlreadyReleased)
        );
    }

    #[test]
    fn mapped_result_builds_explicit_release_token_without_exposing_bytes() {
        let received = MappedTransferRecv {
            sender_tid: 9,
            local_transfer_cap: 27,
            mapped_base: 0x6000_0000,
            mapped_len: 0x2000,
            intent: IpcRecvMapIntent::ReadWrite,
        };
        let token = received.release_token();
        assert_eq!(
            token.request(),
            TransferReleaseRequest::explicit(27, 0x6000_0000, 0x2000)
        );
        assert!(!token.is_released());
    }
}
