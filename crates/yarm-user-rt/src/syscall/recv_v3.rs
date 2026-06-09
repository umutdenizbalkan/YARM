// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Non-blocking user-rt wrapper for the `recv_shared_v3` syscall (NR 30).
//!
//! This module implements the userspace half of the Stage 42+43 and Stage 61+62 ABI.
//! Only the non-blocking (timeout_ticks == 0) path is exposed; blocking requires
//! kernel work not yet complete.
//!
//! Two public functions are provided:
//! - [`ipc_recv_shared_v3_nonblocking`]: plain receive (no mapping, map_intent = 0).
//! - [`ipc_recv_shared_v3_mapped_readonly_nonblocking`]: mapped read-only receive
//!   (map_intent = READ, Stage 61+62). Returns mapping metadata including
//!   `cleanup_token` for explicit release via `release_v3_cleanup_token`.

use core::mem::size_of;
use yarm_ipc_abi::recv_shared_v3_abi::{
    RECV_V3_CLEANUP_TOKEN_NONE, RECV_V3_MAP_READ, RECV_V3_MIN_REQUEST_LEN, RECV_V3_STATUS_OK,
    RECV_V3_STATUS_WOULD_BLOCK, RECV_V3_VERSION, RecvSharedV3ObjectKind, RecvSharedV3Output,
};

use super::{SyscallError, decode_syscall_error};

/// NR 30: non-blocking `recv_shared_v3` added in Stage 42+43.
pub const SYSCALL_RECV_SHARED_V3_NR: usize = 30;

/// Pre-write sentinel placed into `output.result_status` before the syscall.
///
/// On aarch64/riscv64, the kernel writes the error code in x0/a0 rather than a
/// separate error register. Writing this sentinel first lets the wrapper detect
/// "kernel never wrote to output buffer" (no valid kernel status equals
/// `0xFF_FF_FF_FF`) and treat `ret.ret0` as the error code in that case.
const STATUS_SENTINEL_UNWRITTEN: u32 = 0xFF_FF_FF_FF;

/// Decoded delivery result from a successful `recv_shared_v3` call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecvSharedV3Delivery {
    /// Thread ID of the message sender.
    pub sender_tid: u64,
    /// Number of payload bytes written to the caller's payload buffer.
    pub message_len: u32,
    /// Raw message flags from the sender.
    pub message_flags: u32,
    /// Materialized local capability ID when the sender transferred one;
    /// `None` when no capability was transferred.
    pub transferred_cap: Option<u64>,
    /// Kind of the transferred capability (Stage 47+48); 0 when none.
    pub object_kind: u32,
    /// Object generation of the transferred cap (Stage 47+48); 0 when unavailable.
    pub object_generation: u64,
    /// Effective rights on the receiver-local transferred cap (Stage 47+48); 0 when none.
    pub effective_rights: u32,
    /// Exact byte size of the transferred MemoryObject (Stage 49); 0 for non-MemoryObject
    /// caps and when no cap was transferred. Always a non-zero page-aligned multiple
    /// when a MemoryObject was successfully transferred.
    pub exact_object_size: u64,
    /// Exact byte length of the transferred DmaRegion sub-region (Stage 50); 0 for
    /// non-DmaRegion caps and when no cap was transferred. Only populated when the
    /// caller provided at least 88 bytes for the output buffer.
    pub exact_region_len: u64,
    /// VA where the shared region was mapped (Stage 61+62).  Non-zero only after
    /// [`ipc_recv_shared_v3_mapped_readonly_nonblocking`]; always 0 after a plain
    /// receive.
    pub mapped_base: u64,
    /// Page-rounded byte length of the mapping (Stage 61+62); 0 for plain receive.
    pub page_rounded_mapped_len: u64,
    /// Actual mapping permissions granted (Stage 61+62); 0 for plain receive.
    /// `0x1` = read-only.  Bitmask of RECV_V3_MAP_READ (0x1) / RECV_V3_MAP_WRITE (0x2).
    pub actual_mapping_perm: u32,
    /// Opaque cleanup token (Stage 61+62).  Non-zero after a successful mapped receive;
    /// 0 ([`RECV_V3_CLEANUP_TOKEN_NONE`]) for plain receives.  Pass to
    /// `release_v3_cleanup_token` when the mapping is no longer needed.
    pub cleanup_token: u64,
}

impl RecvSharedV3Delivery {
    /// Decode a delivery result from a kernel-written [`RecvSharedV3Output`].
    ///
    /// Returns `Some(delivery)` when `output.result_status == RECV_V3_STATUS_OK`.
    /// Returns `None` for any other status (including WouldBlock and error codes).
    #[inline]
    pub fn from_output(output: &RecvSharedV3Output) -> Option<Self> {
        if output.result_status != RECV_V3_STATUS_OK {
            return None;
        }
        let transferred_cap = if output.has_no_transfer_cap() {
            None
        } else {
            Some(output.transferred_cap)
        };
        Some(Self {
            sender_tid: output.sender_tid,
            message_len: output.message_len,
            message_flags: output.message_flags,
            transferred_cap,
            object_kind: output.object_kind,
            object_generation: output.object_generation,
            effective_rights: output.effective_rights,
            exact_object_size: output.exact_object_size,
            exact_region_len: output.exact_region_len,
            mapped_base: output.mapped_base,
            page_rounded_mapped_len: output.page_rounded_mapped_len,
            actual_mapping_perm: output.actual_mapping_perm,
            cleanup_token: output.cleanup_token,
        })
    }

    /// Returns `true` if a capability was transferred.
    #[inline]
    pub fn has_transfer_cap(&self) -> bool {
        self.transferred_cap.is_some()
    }

    /// Returns the result status for a delivered message (`RECV_V3_STATUS_OK`).
    #[inline]
    pub const fn status(&self) -> u32 {
        RECV_V3_STATUS_OK
    }

    /// Returns the kind discriminant of the transferred capability (Stage 47+48).
    #[inline]
    pub const fn object_kind(&self) -> u32 {
        self.object_kind
    }

    /// Returns the object generation of the transferred capability (Stage 47+48).
    #[inline]
    pub const fn object_generation(&self) -> u64 {
        self.object_generation
    }

    /// Returns the effective rights on the receiver-local cap (Stage 47+48).
    #[inline]
    pub const fn effective_rights(&self) -> u32 {
        self.effective_rights
    }

    /// Returns the exact byte size of the transferred MemoryObject (Stage 49).
    /// 0 when no MemoryObject was transferred or when the size is unavailable.
    #[inline]
    pub const fn exact_object_size(&self) -> u64 {
        self.exact_object_size
    }

    /// Returns `true` when `exact_object_size` is authoritative (> 0).
    #[inline]
    pub const fn has_exact_object_size(&self) -> bool {
        self.exact_object_size > 0
    }

    /// Returns the exact byte length of the transferred DmaRegion sub-region (Stage 50).
    /// 0 when no DmaRegion was transferred or when the buffer was < 88 bytes.
    #[inline]
    pub const fn exact_region_len(&self) -> u64 {
        self.exact_region_len
    }

    /// Returns `true` when `exact_region_len` is authoritative (> 0).
    #[inline]
    pub const fn has_exact_region_len(&self) -> bool {
        self.exact_region_len > 0
    }

    /// Returns `true` if the transferred capability is a DmaRegion (Stage 52+53).
    #[inline]
    pub fn is_dma_region(&self) -> bool {
        self.object_kind == RecvSharedV3ObjectKind::DmaRegion as u32
    }

    /// Returns the raw cleanup token from the kernel output (Stage 61+62).
    ///
    /// Non-zero after [`ipc_recv_shared_v3_mapped_readonly_nonblocking`].
    /// Zero ([`RECV_V3_CLEANUP_TOKEN_NONE`]) after a plain receive.
    /// Pass to `release_v3_cleanup_token` to unmap the region.
    #[inline]
    pub const fn cleanup_token(&self) -> u64 {
        self.cleanup_token
    }

    /// Returns `true` when a non-zero cleanup token is present (Stage 61+62).
    #[inline]
    pub fn has_cleanup_token(&self) -> bool {
        self.cleanup_token != RECV_V3_CLEANUP_TOKEN_NONE
    }

    /// VA where the shared region was mapped (Stage 61+62).
    ///
    /// Non-zero only after [`ipc_recv_shared_v3_mapped_readonly_nonblocking`];
    /// 0 after a plain receive.
    #[inline]
    pub const fn mapped_base(&self) -> u64 {
        self.mapped_base
    }

    /// Page-rounded byte length of the mapping (Stage 61+62); 0 for plain receive.
    #[inline]
    pub const fn page_rounded_mapped_len(&self) -> u64 {
        self.page_rounded_mapped_len
    }

    /// Actual permissions granted for the mapping (Stage 61+62); 0 for plain receive.
    /// `0x1` = read-only.
    #[inline]
    pub const fn actual_mapping_perm(&self) -> u32 {
        self.actual_mapping_perm
    }

    /// Returns `true` when a live shared-memory mapping was established
    /// (i.e. `mapped_base != 0`).  `true` after a successful mapped receive.
    #[inline]
    pub fn has_mapping(&self) -> bool {
        self.mapped_base != 0
    }
}

/// Encode a non-blocking, no-map-intent `recv_shared_v3` request as 80 bytes.
///
/// All fields are written as little-endian. The buffer can be placed on the
/// user stack and its address passed to the kernel as syscall arg 0.
#[inline]
fn encode_nonblocking_request(
    endpoint_cap: u64,
    payload_ptr: u64,
    payload_len: u64,
    metadata_ptr: u64,
    metadata_len: u64,
) -> [u8; 80] {
    let mut buf = [0u8; 80];
    buf[0..4].copy_from_slice(&RECV_V3_VERSION.to_le_bytes());
    buf[4..8].copy_from_slice(&RECV_V3_MIN_REQUEST_LEN.to_le_bytes());
    buf[8..16].copy_from_slice(&endpoint_cap.to_le_bytes());
    buf[16..24].copy_from_slice(&payload_ptr.to_le_bytes());
    buf[24..32].copy_from_slice(&payload_len.to_le_bytes());
    buf[32..40].copy_from_slice(&metadata_ptr.to_le_bytes());
    buf[40..48].copy_from_slice(&metadata_len.to_le_bytes());
    // map_intent @ 48: 0 (no mapping)
    // flags @ 52:      0 (reserved)
    // timeout_ticks @ 56: 0 (non-blocking)
    // reserved @ 64:   0, 0
    buf
}

/// Encode a non-blocking, READ-ONLY map-intent `recv_shared_v3` request as 80 bytes.
///
/// Sets `map_intent = RECV_V3_MAP_READ` (0x1) and `metadata_len` = the full
/// `RecvSharedV3Output` struct size (≥ `RECV_V3_TOKEN_OUTPUT_LEN` = 120) so the
/// kernel writes all live-mapping fields including `cleanup_token`.
#[inline]
fn encode_mapped_readonly_request(
    endpoint_cap: u64,
    payload_ptr: u64,
    payload_len: u64,
    metadata_ptr: u64,
) -> [u8; 80] {
    let metadata_len = size_of::<RecvSharedV3Output>() as u64;
    let mut buf = [0u8; 80];
    buf[0..4].copy_from_slice(&RECV_V3_VERSION.to_le_bytes());
    buf[4..8].copy_from_slice(&RECV_V3_MIN_REQUEST_LEN.to_le_bytes());
    buf[8..16].copy_from_slice(&endpoint_cap.to_le_bytes());
    buf[16..24].copy_from_slice(&payload_ptr.to_le_bytes());
    buf[24..32].copy_from_slice(&payload_len.to_le_bytes());
    buf[32..40].copy_from_slice(&metadata_ptr.to_le_bytes());
    buf[40..48].copy_from_slice(&metadata_len.to_le_bytes());
    buf[48..52].copy_from_slice(&(RECV_V3_MAP_READ as u32).to_le_bytes()); // map_intent = READ
    // flags @ 52: 0, timeout_ticks @ 56: 0 (non-blocking)
    buf
}

/// Non-blocking receive on a `recv_shared_v3` endpoint (NR 30).
///
/// Writes `STATUS_SENTINEL_UNWRITTEN` to `output.result_status` before the
/// syscall so that aarch64/riscv64 callers can detect whether the kernel wrote
/// output (no valid kernel status equals the sentinel).
///
/// Returns:
/// - `Ok(Some(delivery))` — a message was received and `output` is populated.
/// - `Ok(None)` — the endpoint was empty (non-blocking WouldBlock).
/// - `Err(e)` — a kernel error other than WouldBlock.
///
/// # Safety
///
/// `payload_ptr..payload_ptr+payload_len` must be a valid, writable user
/// virtual address range in the current task's address space. `output` must
/// be valid for the duration of this call.
#[inline]
pub unsafe fn ipc_recv_shared_v3_nonblocking(
    endpoint_cap: u64,
    payload_ptr: u64,
    payload_len: u64,
    output: &mut RecvSharedV3Output,
) -> Result<Option<RecvSharedV3Delivery>, SyscallError> {
    output.result_status = STATUS_SENTINEL_UNWRITTEN;

    let req_bytes = encode_nonblocking_request(
        endpoint_cap,
        payload_ptr,
        payload_len,
        output as *mut RecvSharedV3Output as u64,
        size_of::<RecvSharedV3Output>() as u64,
    );

    // SAFETY: req_bytes is on the caller's stack (a valid user VA).
    //         output is valid for the duration of this call.
    let ret = unsafe {
        crate::arch::raw_syscall(
            SYSCALL_RECV_SHARED_V3_NR,
            [
                req_bytes.as_ptr() as usize,
                req_bytes.len(), // 80 >= RECV_V3_MIN_REQUEST_LEN (64)
                0,
                0,
                0,
                0,
            ],
        )
    };

    #[cfg(target_arch = "x86_64")]
    let error: Option<SyscallError> = if ret.error != 0 {
        Some(decode_syscall_error(ret.error))
    } else {
        None
    };

    // On aarch64/riscv64 the error code lands in ret.ret0 (x0/a0).  The
    // sentinel distinguishes "kernel returned an error without writing output"
    // from "kernel wrote output and status happens to look like an error code".
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    let error: Option<SyscallError> = if output.result_status == STATUS_SENTINEL_UNWRITTEN {
        Some(decode_syscall_error(ret.ret0))
    } else {
        None
    };

    if let Some(err) = error {
        return if matches!(err, SyscallError::WouldBlock) {
            Ok(None)
        } else {
            Err(err)
        };
    }

    match output.result_status {
        RECV_V3_STATUS_OK => {
            let transferred_cap = if output.has_no_transfer_cap() {
                None
            } else {
                Some(output.transferred_cap)
            };
            Ok(Some(RecvSharedV3Delivery {
                sender_tid: output.sender_tid,
                message_len: output.message_len,
                message_flags: output.message_flags,
                transferred_cap,
                object_kind: output.object_kind,
                object_generation: output.object_generation,
                effective_rights: output.effective_rights,
                exact_object_size: output.exact_object_size,
                exact_region_len: output.exact_region_len,
                mapped_base: output.mapped_base,
                page_rounded_mapped_len: output.page_rounded_mapped_len,
                actual_mapping_perm: output.actual_mapping_perm,
                cleanup_token: output.cleanup_token,
            }))
        }
        RECV_V3_STATUS_WOULD_BLOCK => Ok(None),
        _ => Err(SyscallError::Internal),
    }
}

/// Non-blocking READ-ONLY mapped receive on a `recv_shared_v3` endpoint (NR 30).
///
/// Sets `map_intent = RECV_V3_MAP_READ` (0x1) so the kernel maps the transferred
/// `DmaRegion` or `MemoryObject` into the receiver's address space at `payload_ptr`
/// (page-aligned, read-only).  The kernel must receive an `OPCODE_SHARED_MEM` message
/// for the mapping to succeed; a plain inline message returns `Err(InvalidArgs)`.
///
/// On success, `delivery.has_mapping()` is `true` and `delivery.cleanup_token()` is
/// non-zero.  Pass the token to `release_v3_cleanup_token` when the mapping is no
/// longer needed.  No `Drop`-based cleanup is performed — callers must release
/// explicitly.
///
/// Returns:
/// - `Ok(Some(delivery))` — mapping established; `output` contains all metadata.
/// - `Ok(None)` — no message ready (non-blocking WouldBlock).
/// - `Err(e)` — kernel error (includes `InvalidArgs` if received message is not
///   `OPCODE_SHARED_MEM` or map_intent is unsupported for this cap type).
///
/// # Safety
///
/// `payload_ptr` must be a page-aligned VA in the current task's address space
/// with at least `payload_len` bytes of capacity; the kernel will install page
/// table entries there.  `output` must be valid for the duration of this call.
#[inline]
pub unsafe fn ipc_recv_shared_v3_mapped_readonly_nonblocking(
    endpoint_cap: u64,
    payload_ptr: u64,
    payload_len: u64,
    output: &mut RecvSharedV3Output,
) -> Result<Option<RecvSharedV3Delivery>, SyscallError> {
    output.result_status = STATUS_SENTINEL_UNWRITTEN;

    let req_bytes = encode_mapped_readonly_request(
        endpoint_cap,
        payload_ptr,
        payload_len,
        output as *mut RecvSharedV3Output as u64,
    );

    // SAFETY: req_bytes is on the caller's stack (a valid user VA).
    //         output is valid for the duration of this call.
    let ret = unsafe {
        crate::arch::raw_syscall(
            SYSCALL_RECV_SHARED_V3_NR,
            [
                req_bytes.as_ptr() as usize,
                req_bytes.len(), // 80 >= RECV_V3_MIN_REQUEST_LEN (64)
                0,
                0,
                0,
                0,
            ],
        )
    };

    #[cfg(target_arch = "x86_64")]
    let error: Option<SyscallError> = if ret.error != 0 {
        Some(decode_syscall_error(ret.error))
    } else {
        None
    };

    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    let error: Option<SyscallError> = if output.result_status == STATUS_SENTINEL_UNWRITTEN {
        Some(decode_syscall_error(ret.ret0))
    } else {
        None
    };

    if let Some(err) = error {
        return if matches!(err, SyscallError::WouldBlock) {
            Ok(None)
        } else {
            Err(err)
        };
    }

    match output.result_status {
        RECV_V3_STATUS_OK => {
            let transferred_cap = if output.has_no_transfer_cap() {
                None
            } else {
                Some(output.transferred_cap)
            };
            Ok(Some(RecvSharedV3Delivery {
                sender_tid: output.sender_tid,
                message_len: output.message_len,
                message_flags: output.message_flags,
                transferred_cap,
                object_kind: output.object_kind,
                object_generation: output.object_generation,
                effective_rights: output.effective_rights,
                exact_object_size: output.exact_object_size,
                exact_region_len: output.exact_region_len,
                mapped_base: output.mapped_base,
                page_rounded_mapped_len: output.page_rounded_mapped_len,
                actual_mapping_perm: output.actual_mapping_perm,
                cleanup_token: output.cleanup_token,
            }))
        }
        RECV_V3_STATUS_WOULD_BLOCK => Ok(None),
        _ => Err(SyscallError::Internal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;
    use yarm_ipc_abi::recv_shared_v3_abi::{
        RecvSharedV3Output, RECV_V3_MIN_OUTPUT_LEN, RECV_V3_MIN_REQUEST_LEN,
        RECV_V3_NO_TRANSFER_CAP, RECV_V3_STATUS_BAD_REQUEST, RECV_V3_STATUS_INVALID_CAP,
        RECV_V3_STATUS_OK, RECV_V3_STATUS_TIMED_OUT, RECV_V3_STATUS_WOULD_BLOCK, RECV_V3_VERSION,
    };

    #[test]
    fn syscall_nr_is_30() {
        assert_eq!(SYSCALL_RECV_SHARED_V3_NR, 30);
    }

    #[test]
    fn delivery_has_transfer_cap_true_when_some() {
        let d = RecvSharedV3Delivery {
            sender_tid: 1,
            transferred_cap: Some(42),
            ..Default::default()
        };
        assert!(d.has_transfer_cap());
    }

    #[test]
    fn delivery_has_transfer_cap_false_when_none() {
        let d = RecvSharedV3Delivery {
            sender_tid: 1,
            ..Default::default()
        };
        assert!(!d.has_transfer_cap());
    }

    #[test]
    fn output_accessor_sender_tid_and_message_len() {
        let d = RecvSharedV3Delivery {
            sender_tid: 99,
            message_len: 7,
            ..Default::default()
        };
        assert_eq!(d.sender_tid, 99);
        assert_eq!(d.message_len, 7);
    }

    #[test]
    fn output_accessor_message_flags() {
        let d = RecvSharedV3Delivery {
            message_flags: 0xDEAD,
            ..Default::default()
        };
        assert_eq!(d.message_flags, 0xDEAD);
    }

    #[test]
    fn delivery_status_is_always_ok() {
        let d = RecvSharedV3Delivery::default();
        assert_eq!(d.status(), RECV_V3_STATUS_OK);
    }

    #[test]
    fn status_sentinel_is_distinct_from_all_valid_statuses() {
        assert_ne!(STATUS_SENTINEL_UNWRITTEN, RECV_V3_STATUS_OK);
        assert_ne!(STATUS_SENTINEL_UNWRITTEN, RECV_V3_STATUS_WOULD_BLOCK);
        assert_ne!(STATUS_SENTINEL_UNWRITTEN, RECV_V3_STATUS_TIMED_OUT);
        assert_ne!(STATUS_SENTINEL_UNWRITTEN, RECV_V3_STATUS_INVALID_CAP);
        assert_ne!(STATUS_SENTINEL_UNWRITTEN, RECV_V3_STATUS_BAD_REQUEST);
    }

    #[test]
    fn request_encodes_nonblocking_no_map() {
        let buf = encode_nonblocking_request(7, 0x1000, 128, 0x2000, 80);
        assert_eq!(
            u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            RECV_V3_VERSION
        );
        assert_eq!(
            u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            RECV_V3_MIN_REQUEST_LEN
        );
        assert_eq!(u64::from_le_bytes(buf[8..16].try_into().unwrap()), 7);
        assert_eq!(u64::from_le_bytes(buf[16..24].try_into().unwrap()), 0x1000);
        assert_eq!(u64::from_le_bytes(buf[24..32].try_into().unwrap()), 128);
        assert_eq!(u64::from_le_bytes(buf[32..40].try_into().unwrap()), 0x2000);
        assert_eq!(u64::from_le_bytes(buf[40..48].try_into().unwrap()), 80);
        assert_eq!(
            u32::from_le_bytes(buf[48..52].try_into().unwrap()),
            0,
            "map_intent"
        );
        assert_eq!(
            u32::from_le_bytes(buf[52..56].try_into().unwrap()),
            0,
            "flags"
        );
        assert_eq!(
            u64::from_le_bytes(buf[56..64].try_into().unwrap()),
            0,
            "timeout_ticks"
        );
    }

    #[test]
    fn output_struct_size_meets_minimum() {
        assert!(
            size_of::<RecvSharedV3Output>() >= RECV_V3_MIN_OUTPUT_LEN as usize,
            "RecvSharedV3Output must be at least {} bytes",
            RECV_V3_MIN_OUTPUT_LEN
        );
    }

    #[test]
    fn wrapper_uses_correct_request_size() {
        let buf = encode_nonblocking_request(0, 0, 0, 0, 0);
        assert_eq!(buf.len(), 80);
        assert!(buf.len() >= RECV_V3_MIN_REQUEST_LEN as usize);
    }

    #[test]
    fn no_transfer_cap_sentinel_matches_abi() {
        assert_eq!(RECV_V3_NO_TRANSFER_CAP, u64::MAX);
    }

    #[test]
    fn delivery_transferred_cap_none_from_no_transfer_sentinel() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.sender_tid = 5;
        output.message_len = 3;
        output.transferred_cap = RECV_V3_NO_TRANSFER_CAP;
        let transferred_cap = if output.has_no_transfer_cap() {
            None
        } else {
            Some(output.transferred_cap)
        };
        let delivery = RecvSharedV3Delivery {
            sender_tid: output.sender_tid,
            message_len: output.message_len,
            message_flags: output.message_flags,
            transferred_cap,
            ..Default::default()
        };
        assert!(!delivery.has_transfer_cap());
        assert_eq!(delivery.transferred_cap, None);
    }

    #[test]
    fn delivery_transferred_cap_some_when_cap_id_present() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = 17;
        let transferred_cap = if output.has_no_transfer_cap() {
            None
        } else {
            Some(output.transferred_cap)
        };
        let delivery = RecvSharedV3Delivery {
            transferred_cap,
            ..Default::default()
        };
        assert!(delivery.has_transfer_cap());
        assert_eq!(delivery.transferred_cap, Some(17));
    }

    // ── Stage 45: first userspace proof — from_output() decoder ──────────────

    #[test]
    fn from_output_plain_ok_decodes_all_fields() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.sender_tid = 42;
        output.message_len = 7;
        output.message_flags = 0;
        output.transferred_cap = RECV_V3_NO_TRANSFER_CAP;
        let delivery = RecvSharedV3Delivery::from_output(&output)
            .expect("STATUS_OK must produce Some(delivery)");
        assert_eq!(delivery.sender_tid, 42);
        assert_eq!(delivery.message_len, 7);
        assert_eq!(delivery.message_flags, 0);
        assert!(!delivery.has_transfer_cap());
        assert_eq!(delivery.status(), RECV_V3_STATUS_OK);
    }

    #[test]
    fn from_output_with_cap_decodes_transferred_cap() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.sender_tid = 3;
        output.message_len = 4;
        output.transferred_cap = 99; // a valid materialized cap ID
        let delivery = RecvSharedV3Delivery::from_output(&output)
            .expect("STATUS_OK must produce Some(delivery)");
        assert!(delivery.has_transfer_cap());
        assert_eq!(delivery.transferred_cap, Some(99));
    }

    #[test]
    fn from_output_would_block_returns_none() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_WOULD_BLOCK;
        assert_eq!(RecvSharedV3Delivery::from_output(&output), None);
    }

    #[test]
    fn from_output_non_ok_status_returns_none() {
        for &status in &[
            RECV_V3_STATUS_TIMED_OUT,
            RECV_V3_STATUS_INVALID_CAP,
            RECV_V3_STATUS_BAD_REQUEST,
        ] {
            let mut output = RecvSharedV3Output::new_zeroed();
            output.result_status = status;
            assert_eq!(
                RecvSharedV3Delivery::from_output(&output),
                None,
                "status {status} must return None"
            );
        }
    }

    #[test]
    fn output_wire_bytes_80_parse_correctly() {
        // Simulate the 80-byte wire format that the kernel writes via write_v3_output_to_user.
        // Layout: @0 version(u32), @4 record_len(u32), @8 abi_version(u32),
        //         @12 result_status(u32), @16 sender_tid(u64), @24 message_len(u32),
        //         @28 message_flags(u32), @32 transferred_cap(u64), @40..80 zeros (FUTURE).
        let mut wire = [0u8; 80];
        wire[0..4].copy_from_slice(&3u32.to_le_bytes()); // RECV_V3_VERSION
        wire[4..8].copy_from_slice(&80u32.to_le_bytes()); // RECV_V3_MIN_OUTPUT_LEN
        wire[8..12].copy_from_slice(&10u32.to_le_bytes()); // RECV_V3_ABI_VERSION
        wire[12..16].copy_from_slice(&0u32.to_le_bytes()); // RECV_V3_STATUS_OK
        wire[16..24].copy_from_slice(&7u64.to_le_bytes()); // sender_tid = 7
        wire[24..28].copy_from_slice(&5u32.to_le_bytes()); // message_len = 5
        wire[28..32].copy_from_slice(&0u32.to_le_bytes()); // message_flags = 0
        wire[32..40].copy_from_slice(&u64::MAX.to_le_bytes()); // no cap

        // Parse the wire bytes back into RecvSharedV3Output fields manually.
        let result_status = u32::from_le_bytes(wire[12..16].try_into().unwrap());
        let sender_tid = u64::from_le_bytes(wire[16..24].try_into().unwrap());
        let message_len = u32::from_le_bytes(wire[24..28].try_into().unwrap());
        let message_flags = u32::from_le_bytes(wire[28..32].try_into().unwrap());
        let transferred_cap_raw = u64::from_le_bytes(wire[32..40].try_into().unwrap());

        assert_eq!(result_status, RECV_V3_STATUS_OK);
        assert_eq!(sender_tid, 7);
        assert_eq!(message_len, 5);
        assert_eq!(message_flags, 0);
        assert_eq!(transferred_cap_raw, RECV_V3_NO_TRANSFER_CAP);

        // Construct a delivery from parsed fields.
        let delivery = RecvSharedV3Delivery {
            sender_tid,
            message_len,
            message_flags,
            transferred_cap: if transferred_cap_raw == RECV_V3_NO_TRANSFER_CAP {
                None
            } else {
                Some(transferred_cap_raw)
            },
            ..Default::default()
        };
        assert!(!delivery.has_transfer_cap());
        assert_eq!(delivery.sender_tid, 7);
        assert_eq!(delivery.message_len, 5);
    }

    // ── Stage 46: from_output roundtrip with cap-transfer output ────────────

    #[test]
    fn from_output_stage46_cap_transfer_output_roundtrip() {
        // Simulate what the kernel writes to the output buffer after materializing
        // a transferred capability (Stage 46 proof: transferred_cap = some_cap_id != u64::MAX).
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.sender_tid = 0;
        output.message_len = 4;
        output.message_flags = 1; // FLAG_CAP_TRANSFER bit (1 << 0)
        output.transferred_cap = 42; // materialized cap ID (non-sentinel)
        let delivery = RecvSharedV3Delivery::from_output(&output)
            .expect("STATUS_OK with non-sentinel cap must produce Some(delivery)");
        assert!(delivery.has_transfer_cap(), "has_transfer_cap must be true");
        assert_eq!(delivery.transferred_cap, Some(42));
        assert_eq!(delivery.message_flags, 1);
        assert_eq!(delivery.message_len, 4);
    }

    #[test]
    fn from_output_and_manual_decode_agree_on_plain_message() {
        // Stage 45: prove that from_output() agrees with the manual field decode path
        // used by ipc_recv_shared_v3_nonblocking().
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.sender_tid = 11;
        output.message_len = 3;
        output.message_flags = 0;
        output.transferred_cap = RECV_V3_NO_TRANSFER_CAP;

        // from_output helper path.
        let via_helper =
            RecvSharedV3Delivery::from_output(&output).expect("helper must decode OK status");

        // Manual decode path (mirrors ipc_recv_shared_v3_nonblocking internals).
        let manual = RecvSharedV3Delivery {
            sender_tid: output.sender_tid,
            message_len: output.message_len,
            message_flags: output.message_flags,
            transferred_cap: if output.has_no_transfer_cap() {
                None
            } else {
                Some(output.transferred_cap)
            },
            object_kind: output.object_kind,
            object_generation: output.object_generation,
            effective_rights: output.effective_rights,
            exact_object_size: output.exact_object_size,
            exact_region_len: output.exact_region_len,
            mapped_base: output.mapped_base,
            page_rounded_mapped_len: output.page_rounded_mapped_len,
            actual_mapping_perm: output.actual_mapping_perm,
            cleanup_token: output.cleanup_token,
        };

        assert_eq!(
            via_helper, manual,
            "from_output and manual decode must agree"
        );
    }

    // ── Stage 47+48: object metadata accessors ───────────────────────────────

    #[test]
    fn object_kind_accessor_returns_field() {
        let d = RecvSharedV3Delivery {
            object_kind: 1,
            ..Default::default()
        };
        assert_eq!(d.object_kind(), 1);
    }

    #[test]
    fn object_generation_accessor_returns_field() {
        let d = RecvSharedV3Delivery {
            object_generation: 0xDEAD_BEEF_CAFE_0001,
            ..Default::default()
        };
        assert_eq!(d.object_generation(), 0xDEAD_BEEF_CAFE_0001);
    }

    #[test]
    fn effective_rights_accessor_returns_field() {
        let d = RecvSharedV3Delivery {
            effective_rights: 0x07,
            ..Default::default()
        };
        assert_eq!(d.effective_rights(), 0x07);
    }

    #[test]
    fn from_output_decodes_object_kind_memory_object() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = 5;
        output.object_kind = 1; // RecvSharedV3ObjectKind::MemoryObject
        output.object_generation = 0;
        output.effective_rights = 0x07;
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert_eq!(d.object_kind(), 1, "MemoryObject kind");
        assert_eq!(d.object_generation(), 0, "MemoryObject has no generation");
        assert_eq!(d.effective_rights(), 0x07, "READ|WRITE|MAP");
    }

    #[test]
    fn from_output_object_metadata_zero_when_no_cap() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = RECV_V3_NO_TRANSFER_CAP;
        // kernel writes zeros for all object fields when there is no transfer
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert!(!d.has_transfer_cap());
        assert_eq!(d.object_kind(), 0);
        assert_eq!(d.object_generation(), 0);
        assert_eq!(d.effective_rights(), 0);
    }

    #[test]
    fn from_output_endpoint_kind_and_generation() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = 7;
        output.object_kind = 2; // RecvSharedV3ObjectKind::Endpoint
        output.object_generation = 0x0000_0000_0000_0042;
        output.effective_rights = 0x08; // SEND
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert_eq!(d.object_kind(), 2);
        assert_eq!(d.object_generation(), 0x42);
        assert_eq!(d.effective_rights(), 0x08);
    }

    // ── Stage 49: exact_object_size accessors ────────────────────────────────

    #[test]
    fn exact_object_size_accessor_returns_field() {
        let d = RecvSharedV3Delivery {
            exact_object_size: 4096,
            ..Default::default()
        };
        assert_eq!(d.exact_object_size(), 4096);
    }

    #[test]
    fn has_exact_object_size_true_when_nonzero() {
        let d = RecvSharedV3Delivery {
            exact_object_size: 4096,
            ..Default::default()
        };
        assert!(d.has_exact_object_size());
    }

    #[test]
    fn has_exact_object_size_false_when_zero() {
        let d = RecvSharedV3Delivery::default();
        assert!(!d.has_exact_object_size());
    }

    #[test]
    fn from_output_decodes_exact_object_size_for_memory_object() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = 3;
        output.object_kind = 1; // MemoryObject
        output.effective_rights = 0x07;
        output.exact_object_size = 4096; // 1 page
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert_eq!(d.exact_object_size(), 4096);
        assert!(d.has_exact_object_size());
    }

    #[test]
    fn from_output_exact_object_size_zero_for_no_cap() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = u64::MAX; // no cap
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert_eq!(d.exact_object_size(), 0);
        assert!(!d.has_exact_object_size());
    }

    #[test]
    fn from_output_exact_object_size_zero_for_endpoint() {
        // Endpoint caps do not have a byte size — kernel writes 0 for non-MemoryObject kinds.
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = 9;
        output.object_kind = 2; // Endpoint
        output.exact_object_size = 0;
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert_eq!(d.exact_object_size(), 0);
        assert!(!d.has_exact_object_size());
    }

    // ── Stage 50: exact_region_len accessors ────────────────────────────────

    #[test]
    fn exact_region_len_accessor_returns_field() {
        let d = RecvSharedV3Delivery {
            exact_region_len: 4096,
            ..Default::default()
        };
        assert_eq!(d.exact_region_len(), 4096);
    }

    #[test]
    fn has_exact_region_len_true_when_nonzero() {
        let d = RecvSharedV3Delivery {
            exact_region_len: 4096,
            ..Default::default()
        };
        assert!(d.has_exact_region_len());
    }

    #[test]
    fn has_exact_region_len_false_when_zero() {
        let d = RecvSharedV3Delivery::default();
        assert!(!d.has_exact_region_len());
    }

    #[test]
    fn from_output_decodes_exact_region_len_for_dma_region() {
        // Simulate kernel writing exact_region_len into an 88-byte buffer.
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = 11;
        output.object_kind = 0xFF; // DmaRegion maps to Other (0xFF)
        output.effective_rights = 0x07; // READ|WRITE|MAP
        output.exact_region_len = 4096;
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert_eq!(d.exact_region_len(), 4096);
        assert!(d.has_exact_region_len());
    }

    #[test]
    fn from_output_exact_region_len_zero_for_no_cap() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = u64::MAX; // no cap
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert_eq!(d.exact_region_len(), 0);
        assert!(!d.has_exact_region_len());
    }

    #[test]
    fn from_output_exact_region_len_zero_for_memory_object() {
        // MemoryObject transfers have exact_object_size, not exact_region_len.
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = 4;
        output.object_kind = 1; // MemoryObject
        output.exact_object_size = 4096;
        output.exact_region_len = 0;
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert_eq!(d.exact_region_len(), 0);
        assert!(!d.has_exact_region_len());
        assert_eq!(d.exact_object_size(), 4096);
    }

    // ── Stage 52+53: DmaRegion first-class kind + cleanup token ─────────────

    #[test]
    fn is_dma_region_true_for_kind_five() {
        let d = RecvSharedV3Delivery {
            object_kind: 5,
            ..Default::default()
        };
        assert!(d.is_dma_region());
    }

    #[test]
    fn is_dma_region_false_for_memory_object() {
        let d = RecvSharedV3Delivery {
            object_kind: 1,
            ..Default::default()
        };
        assert!(!d.is_dma_region());
    }

    #[test]
    fn is_dma_region_false_when_no_cap() {
        let d = RecvSharedV3Delivery::default();
        assert!(!d.is_dma_region());
    }

    #[test]
    fn from_output_dma_region_kind_five_decoded() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = 7;
        output.object_kind = 5; // DmaRegion
        output.effective_rights = 0x07;
        output.exact_region_len = 4096;
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert!(d.is_dma_region(), "kind=5 must identify as DmaRegion");
        assert_eq!(d.exact_region_len(), 4096);
        assert_eq!(d.exact_object_size(), 0, "DmaRegion has no object size");
    }

    #[test]
    fn cleanup_token_accessor_returns_zero_always() {
        let d = RecvSharedV3Delivery::default();
        assert_eq!(d.cleanup_token(), 0);
        assert!(
            !d.has_cleanup_token(),
            "has_cleanup_token must be false (no live mapping)"
        );
    }

    #[test]
    fn from_output_cleanup_token_zero_for_dma_region() {
        // cleanup_token field in RecvSharedV3Output is at @112, beyond the 88-byte write
        // window; it is never written by the kernel and stays 0 from new_zeroed().
        use yarm_ipc_abi::recv_shared_v3_abi::RECV_V3_CLEANUP_TOKEN_NONE;
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = 8;
        output.object_kind = 5; // DmaRegion
        output.exact_region_len = 4096;
        // cleanup_token stays at 0 (new_zeroed initialises all fields to 0)
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert_eq!(d.cleanup_token(), RECV_V3_CLEANUP_TOKEN_NONE);
        assert!(!d.has_cleanup_token());
    }

    #[test]
    fn from_output_and_manual_decode_agree_on_dma_region() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = 9;
        output.object_kind = 5;
        output.effective_rights = 0x07;
        output.exact_region_len = 4096;

        let via_helper = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        let manual = RecvSharedV3Delivery {
            sender_tid: 0,
            message_len: 0,
            message_flags: 0,
            transferred_cap: Some(9),
            object_kind: 5,
            object_generation: 0,
            effective_rights: 0x07,
            exact_object_size: 0,
            exact_region_len: 4096,
            mapped_base: 0,
            page_rounded_mapped_len: 0,
            actual_mapping_perm: 0,
            cleanup_token: 0,
        };
        assert_eq!(
            via_helper, manual,
            "from_output and manual decode must agree"
        );
    }

    // ── Stage 54+55: mapped_base / mapped_len / actual_mapping_perm ──────────

    #[test]
    fn mapped_base_accessor_returns_field() {
        let d = RecvSharedV3Delivery {
            mapped_base: 0x4000,
            ..Default::default()
        };
        assert_eq!(d.mapped_base(), 0x4000);
    }

    #[test]
    fn has_mapping_true_when_mapped_base_nonzero() {
        let d = RecvSharedV3Delivery {
            mapped_base: 0x8000,
            ..Default::default()
        };
        assert!(d.has_mapping(), "mapped_base != 0 must report has_mapping");
    }

    #[test]
    fn has_mapping_false_when_mapped_base_zero() {
        let d = RecvSharedV3Delivery::default();
        assert!(!d.has_mapping(), "default delivery must not have mapping");
    }

    #[test]
    fn page_rounded_mapped_len_accessor_returns_field() {
        let d = RecvSharedV3Delivery {
            page_rounded_mapped_len: 8192,
            ..Default::default()
        };
        assert_eq!(d.page_rounded_mapped_len(), 8192);
    }

    #[test]
    fn actual_mapping_perm_accessor_returns_field() {
        let d = RecvSharedV3Delivery {
            actual_mapping_perm: 0x1,
            ..Default::default()
        };
        assert_eq!(d.actual_mapping_perm(), 0x1);
    }

    #[test]
    fn from_output_decodes_mapping_fields() {
        use yarm_ipc_abi::recv_shared_v3_abi::RECV_V3_STATUS_OK;
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.mapped_base = 0x4000;
        output.page_rounded_mapped_len = 4096;
        output.actual_mapping_perm = 0x1;
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert_eq!(d.mapped_base(), 0x4000);
        assert_eq!(d.page_rounded_mapped_len(), 4096);
        assert_eq!(d.actual_mapping_perm(), 0x1);
        assert!(d.has_mapping());
    }

    #[test]
    fn from_output_mapping_fields_zero_when_output_zeroed() {
        use yarm_ipc_abi::recv_shared_v3_abi::RECV_V3_STATUS_OK;
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert_eq!(
            d.mapped_base(),
            0,
            "mapped_base must be 0 when kernel writes none"
        );
        assert_eq!(d.page_rounded_mapped_len(), 0);
        assert_eq!(d.actual_mapping_perm(), 0);
        assert!(!d.has_mapping());
    }

    // ── Stage 61+62: mapped read-only receive wrapper ────────────────────────

    #[test]
    fn stage61_encode_mapped_readonly_has_map_intent_one() {
        let buf = encode_mapped_readonly_request(7, 0x2000, 4096, 0x3000);
        let map_intent = u32::from_le_bytes(buf[48..52].try_into().unwrap());
        assert_eq!(map_intent, 1, "map_intent must be RECV_V3_MAP_READ = 1");
    }

    #[test]
    fn stage61_encode_mapped_readonly_timeout_is_zero() {
        let buf = encode_mapped_readonly_request(0, 0, 0, 0);
        let timeout = u64::from_le_bytes(buf[56..64].try_into().unwrap());
        assert_eq!(timeout, 0, "timeout_ticks must be 0 (non-blocking)");
    }

    #[test]
    fn stage61_encode_mapped_readonly_metadata_len_at_least_120() {
        let buf = encode_mapped_readonly_request(1, 0x2000, 4096, 0x3000);
        let metadata_len = u64::from_le_bytes(buf[40..48].try_into().unwrap());
        use yarm_ipc_abi::recv_shared_v3_abi::RECV_V3_TOKEN_OUTPUT_LEN;
        assert!(
            metadata_len >= RECV_V3_TOKEN_OUTPUT_LEN as u64,
            "metadata_len {metadata_len} must be >= RECV_V3_TOKEN_OUTPUT_LEN ({RECV_V3_TOKEN_OUTPUT_LEN})"
        );
    }

    #[test]
    fn stage61_encode_mapped_readonly_endpoint_cap_encoded() {
        let buf = encode_mapped_readonly_request(42, 0, 0, 0);
        assert_eq!(u64::from_le_bytes(buf[8..16].try_into().unwrap()), 42);
    }

    #[test]
    fn stage61_encode_mapped_readonly_payload_ptr_encoded() {
        let buf = encode_mapped_readonly_request(0, 0x4000, 0, 0);
        assert_eq!(
            u64::from_le_bytes(buf[16..24].try_into().unwrap()),
            0x4000
        );
    }

    #[test]
    fn stage61_encode_mapped_readonly_payload_len_encoded() {
        let buf = encode_mapped_readonly_request(0, 0, 8192, 0);
        assert_eq!(u64::from_le_bytes(buf[24..32].try_into().unwrap()), 8192);
    }

    #[test]
    fn stage61_encode_mapped_readonly_flags_zero() {
        let buf = encode_mapped_readonly_request(0, 0, 0, 0);
        let flags = u32::from_le_bytes(buf[52..56].try_into().unwrap());
        assert_eq!(flags, 0, "flags must be 0 (reserved)");
    }

    #[test]
    fn stage61_from_output_with_live_mapping_has_mapping_true() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = 7;
        output.object_kind = 5; // DmaRegion
        output.exact_region_len = 4096;
        output.mapped_base = 0x2_0000;
        output.page_rounded_mapped_len = 4096;
        output.actual_mapping_perm = 1; // read-only
        output.cleanup_token = 0x0001_0003; // generation=1, slot=3
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert!(d.has_mapping(), "has_mapping must be true when mapped_base != 0");
        assert_eq!(d.mapped_base(), 0x2_0000);
        assert_eq!(d.page_rounded_mapped_len(), 4096);
        assert_eq!(d.actual_mapping_perm(), 1);
    }

    #[test]
    fn stage61_from_output_cleanup_token_nonzero_has_token_true() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.transferred_cap = 5;
        output.cleanup_token = 0x0001_0002; // nonzero token
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert!(d.has_cleanup_token(), "nonzero token must report has_cleanup_token");
        assert_eq!(d.cleanup_token(), 0x0001_0002);
    }

    #[test]
    fn stage61_from_output_full_mapping_roundtrip() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.sender_tid = 2;
        output.transferred_cap = 9;
        output.object_kind = 5; // DmaRegion
        output.effective_rights = 0x07;
        output.exact_region_len = 4096;
        output.mapped_base = 0x2_0000;
        output.page_rounded_mapped_len = 4096;
        output.actual_mapping_perm = 1;
        output.cleanup_token = 0x0001_0009;
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert_eq!(d.sender_tid, 2);
        assert_eq!(d.transferred_cap, Some(9));
        assert!(d.is_dma_region());
        assert_eq!(d.exact_region_len(), 4096);
        assert!(d.has_mapping());
        assert_eq!(d.mapped_base(), 0x2_0000);
        assert_eq!(d.page_rounded_mapped_len(), 4096);
        assert_eq!(d.actual_mapping_perm(), 1);
        assert!(d.has_cleanup_token());
        assert_eq!(d.cleanup_token(), 0x0001_0009);
    }

    #[test]
    fn stage61_from_output_actual_perm_read_only_is_one() {
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.mapped_base = 0x2_0000;
        output.actual_mapping_perm = 1; // MAP_PERM_READ_ONLY sentinel
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert_eq!(d.actual_mapping_perm(), 1, "read-only sentinel must be 1");
    }

    #[test]
    fn stage61_from_output_plain_receive_has_no_mapping() {
        // Plain receive (no map_intent) leaves mapping fields at 0.
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.sender_tid = 1;
        output.transferred_cap = RECV_V3_CLEANUP_TOKEN_NONE; // no cap
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert!(!d.has_mapping(), "plain receive must not report mapping");
        assert!(!d.has_cleanup_token(), "plain receive must not have cleanup token");
        assert_eq!(d.mapped_base(), 0);
        assert_eq!(d.page_rounded_mapped_len(), 0);
        assert_eq!(d.cleanup_token(), 0);
    }

    #[test]
    fn stage62_cleanup_token_none_is_zero() {
        assert_eq!(
            RECV_V3_CLEANUP_TOKEN_NONE, 0,
            "RECV_V3_CLEANUP_TOKEN_NONE sentinel must be 0"
        );
    }

    #[test]
    fn stage62_cleanup_token_generation_in_high_bits() {
        // CapId.0 = (generation << 16) | slot.  A freshly minted cap with generation=1
        // and slot=2 yields CapId.0 = 0x0001_0002.  Bits[63:16] encode generation.
        let token: u64 = 0x0001_0002;
        let generation = token >> 16;
        let slot = token & 0xFFFF;
        assert_eq!(generation, 1, "generation in bits[63:16]");
        assert_eq!(slot, 2, "slot in bits[15:0]");
        // has_cleanup_token must be true for any nonzero token.
        let d = RecvSharedV3Delivery {
            cleanup_token: token,
            ..Default::default()
        };
        assert!(d.has_cleanup_token());
    }

    // ── Stage 63: plain-receive adoption proof ────────────────────────────────
    // These tests prove the user-rt wrapper contracts for the plain-receive path
    // (map_intent=0).  No syscall is issued; they are pure encode/decode proofs.

    #[test]
    fn stage63_adoption_encode_plain_recv_map_intent_zero() {
        // The encode_nonblocking_request helper must produce map_intent=0
        // (no mapping requested).  This is the adoption proof that the plain-receive
        // encoding path sets the correct intent discriminant.
        let buf = encode_nonblocking_request(3, 0x1000, 128, 0x2000, 128);
        let map_intent = u32::from_le_bytes(buf[48..52].try_into().unwrap());
        assert_eq!(
            map_intent, 0,
            "plain receive must encode map_intent=0 (no mapping)"
        );
    }

    #[test]
    fn stage63_adoption_from_output_plain_recv_no_mapping() {
        // Prove that from_output() on a kernel-written plain-receive output
        // (mapping fields all zero) produces has_mapping()==false and
        // has_cleanup_token()==false.
        let mut output = RecvSharedV3Output::new_zeroed();
        output.result_status = RECV_V3_STATUS_OK;
        output.sender_tid = 1;
        output.message_len = 0;
        output.transferred_cap = 5; // materialized cap
        output.object_kind = 5; // DmaRegion
        output.exact_region_len = 4096;
        // mapping fields stay at 0 (plain receive, map_intent=0)
        let d = RecvSharedV3Delivery::from_output(&output).expect("must decode");
        assert!(!d.has_mapping(), "plain receive must not report a mapping");
        assert!(
            !d.has_cleanup_token(),
            "plain receive must not have a cleanup token"
        );
        assert_eq!(d.mapped_base(), 0, "mapped_base must be 0");
        assert_eq!(d.page_rounded_mapped_len(), 0, "mapped_len must be 0");
        assert_eq!(d.cleanup_token(), 0, "cleanup_token must be 0");
    }

    #[test]
    fn stage63_adoption_mapped_recv_delivery_differs_from_plain() {
        // Prove that a mapped-receive delivery has_mapping()==true and
        // has_cleanup_token()==true, while a plain-receive delivery does not.
        let mut plain_output = RecvSharedV3Output::new_zeroed();
        plain_output.result_status = RECV_V3_STATUS_OK;
        plain_output.transferred_cap = 5;
        plain_output.object_kind = 5;
        let plain = RecvSharedV3Delivery::from_output(&plain_output).expect("plain");

        let mut mapped_output = RecvSharedV3Output::new_zeroed();
        mapped_output.result_status = RECV_V3_STATUS_OK;
        mapped_output.transferred_cap = 5;
        mapped_output.object_kind = 5;
        mapped_output.mapped_base = 0x2_0000;
        mapped_output.page_rounded_mapped_len = 4096;
        mapped_output.actual_mapping_perm = 1;
        mapped_output.cleanup_token = 0x0001_0005;
        let mapped = RecvSharedV3Delivery::from_output(&mapped_output).expect("mapped");

        assert!(!plain.has_mapping(), "plain must not have mapping");
        assert!(mapped.has_mapping(), "mapped must have mapping");
        assert!(!plain.has_cleanup_token(), "plain must not have token");
        assert!(mapped.has_cleanup_token(), "mapped must have token");
        assert_ne!(
            plain.mapped_base(),
            mapped.mapped_base(),
            "mapped_base must differ"
        );
    }

    #[test]
    fn stage63_adoption_encode_mapped_vs_plain_map_intent_differ() {
        // The two encoders must produce different map_intent values — this proves
        // they are not accidentally interchangeable.
        let plain_buf = encode_nonblocking_request(1, 0, 0, 0, 0);
        let mapped_buf = encode_mapped_readonly_request(1, 0, 0, 0);
        let plain_intent = u32::from_le_bytes(plain_buf[48..52].try_into().unwrap());
        let mapped_intent = u32::from_le_bytes(mapped_buf[48..52].try_into().unwrap());
        assert_eq!(plain_intent, 0, "plain encode must set map_intent=0");
        assert_eq!(mapped_intent, 1, "mapped-read encode must set map_intent=1");
        assert_ne!(plain_intent, mapped_intent, "intents must differ");
    }
}
