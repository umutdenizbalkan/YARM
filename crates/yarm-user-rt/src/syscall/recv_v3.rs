// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Non-blocking user-rt wrapper for the `recv_shared_v3` syscall (NR 30).
//!
//! This module implements the userspace half of the frozen Stage 42+43 ABI.
//! Only the non-blocking (timeout_ticks == 0) path is exposed; blocking and
//! map_intent require kernel work not yet complete.

use core::mem::size_of;
use yarm_ipc_abi::recv_shared_v3_abi::{
    RecvSharedV3Output, RECV_V3_MIN_REQUEST_LEN, RECV_V3_STATUS_OK, RECV_V3_STATUS_WOULD_BLOCK,
    RECV_V3_VERSION,
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
        RecvSharedV3Output, RECV_V3_MIN_OUTPUT_LEN, RECV_V3_MIN_REQUEST_LEN, RECV_V3_NO_TRANSFER_CAP,
        RECV_V3_STATUS_BAD_REQUEST, RECV_V3_STATUS_INVALID_CAP, RECV_V3_STATUS_OK,
        RECV_V3_STATUS_TIMED_OUT, RECV_V3_STATUS_WOULD_BLOCK, RECV_V3_VERSION,
    };

    #[test]
    fn syscall_nr_is_30() {
        assert_eq!(SYSCALL_RECV_SHARED_V3_NR, 30);
    }

    #[test]
    fn delivery_has_transfer_cap_true_when_some() {
        let d = RecvSharedV3Delivery {
            sender_tid: 1,
            message_len: 0,
            message_flags: 0,
            transferred_cap: Some(42),
        };
        assert!(d.has_transfer_cap());
    }

    #[test]
    fn delivery_has_transfer_cap_false_when_none() {
        let d = RecvSharedV3Delivery {
            sender_tid: 1,
            message_len: 0,
            message_flags: 0,
            transferred_cap: None,
        };
        assert!(!d.has_transfer_cap());
    }

    #[test]
    fn output_accessor_sender_tid_and_message_len() {
        let d = RecvSharedV3Delivery {
            sender_tid: 99,
            message_len: 7,
            message_flags: 0,
            transferred_cap: None,
        };
        assert_eq!(d.sender_tid, 99);
        assert_eq!(d.message_len, 7);
    }

    #[test]
    fn output_accessor_message_flags() {
        let d = RecvSharedV3Delivery {
            sender_tid: 0,
            message_len: 0,
            message_flags: 0xDEAD,
            transferred_cap: None,
        };
        assert_eq!(d.message_flags, 0xDEAD);
    }

    #[test]
    fn delivery_status_is_always_ok() {
        let d = RecvSharedV3Delivery {
            sender_tid: 0,
            message_len: 0,
            message_flags: 0,
            transferred_cap: None,
        };
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
        assert_eq!(u32::from_le_bytes(buf[0..4].try_into().unwrap()), RECV_V3_VERSION);
        assert_eq!(
            u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            RECV_V3_MIN_REQUEST_LEN
        );
        assert_eq!(u64::from_le_bytes(buf[8..16].try_into().unwrap()), 7);
        assert_eq!(u64::from_le_bytes(buf[16..24].try_into().unwrap()), 0x1000);
        assert_eq!(u64::from_le_bytes(buf[24..32].try_into().unwrap()), 128);
        assert_eq!(u64::from_le_bytes(buf[32..40].try_into().unwrap()), 0x2000);
        assert_eq!(u64::from_le_bytes(buf[40..48].try_into().unwrap()), 80);
        assert_eq!(u32::from_le_bytes(buf[48..52].try_into().unwrap()), 0, "map_intent");
        assert_eq!(u32::from_le_bytes(buf[52..56].try_into().unwrap()), 0, "flags");
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
            sender_tid: 0,
            message_len: 0,
            message_flags: 0,
            transferred_cap,
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
        wire[0..4].copy_from_slice(&3u32.to_le_bytes());   // RECV_V3_VERSION
        wire[4..8].copy_from_slice(&80u32.to_le_bytes());  // RECV_V3_MIN_OUTPUT_LEN
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
        };
        assert!(!delivery.has_transfer_cap());
        assert_eq!(delivery.sender_tid, 7);
        assert_eq!(delivery.message_len, 5);
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
        let via_helper = RecvSharedV3Delivery::from_output(&output)
            .expect("helper must decode OK status");

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
        };

        assert_eq!(via_helper, manual, "from_output and manual decode must agree");
    }
}
