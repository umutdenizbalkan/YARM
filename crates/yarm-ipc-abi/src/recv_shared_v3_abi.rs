// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stable ABI types and constants for the future `recv_shared_v3` receive interface.
//!
//! # Status: draft, not live
//!
//! No public syscall number has been assigned.  Kernel dispatch is disabled
//! (helper-only).  These definitions reserve ABI space for Stage 42+.
//!
//! # Encoding
//!
//! All fields are native Rust types (no explicit `#[repr(C)]`) populated
//! field-by-field by the kernel.  When the live syscall is added in a future
//! stage, a `#[repr(C)]` annotation will be applied at that point to lock
//! the wire layout.
//!
//! # Authoritative vs future fields
//!
//! Fields marked **FUTURE (unavailable)** are always zero/sentinel in this
//! stage.  They are present only to reserve record positions.

/// ABI version embedded in all request and output records.
pub const RECV_V3_VERSION: u32 = 3;

/// Minimum byte length of a well-formed request record.
pub const RECV_V3_MIN_REQUEST_LEN: u32 = 64;

/// Minimum byte length of a well-formed output record.
pub const RECV_V3_MIN_OUTPUT_LEN: u32 = 80;

/// Map-intent flag: map the transferred region read-only.
pub const RECV_V3_MAP_READ: u32 = 0x1;

/// Map-intent flag: map the transferred region read-write (implies READ).
pub const RECV_V3_MAP_WRITE: u32 = 0x2;

/// Sentinel for `transferred_cap` when no capability was transferred.
pub const RECV_V3_NO_TRANSFER_CAP: u64 = u64::MAX;

/// Sentinel for FUTURE fields that are unavailable in this ABI version.
pub const RECV_V3_FIELD_UNAVAILABLE: u64 = 0;

/// Syscall ABI version written into `RecvSharedV3Output::abi_version`.
pub const RECV_V3_ABI_VERSION: u32 = 10;

/// Output status: operation succeeded.
pub const RECV_V3_STATUS_OK: u32 = 0;
/// Output status: would have blocked (non-blocking recv, no message ready).
pub const RECV_V3_STATUS_WOULD_BLOCK: u32 = 1;
/// Output status: deadline elapsed before a message arrived.
pub const RECV_V3_STATUS_TIMED_OUT: u32 = 2;
/// Output status: endpoint capability is invalid or inaccessible.
pub const RECV_V3_STATUS_INVALID_CAP: u32 = 3;
/// Output status: request record failed kernel validation.
pub const RECV_V3_STATUS_BAD_REQUEST: u32 = 4;

// ── Object kind ──────────────────────────────────────────────────────────────

/// Kind of a transferred capability.
///
/// All variants other than [`Unknown`](Self::Unknown) are **FUTURE
/// (unavailable)**: the kernel does not populate
/// `RecvSharedV3Output::object_kind` in Stage 40+41.  The field is always
/// zero (`Unknown`) until object introspection is added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum RecvSharedV3ObjectKind {
    /// Unknown / unavailable — field value 0.  Default in this stage.
    Unknown = 0,
    /// FUTURE: the transferred cap is a memory object.
    MemoryObject = 1,
    /// FUTURE: the transferred cap is an endpoint.
    Endpoint = 2,
    /// FUTURE: the transferred cap is a reply capability.
    ReplyCap = 3,
    /// FUTURE: the transferred cap is a notification object.
    Notification = 4,
    /// FUTURE: other or unrecognised kind (forward-compatibility sentinel).
    Other = 0xFF,
}

impl RecvSharedV3ObjectKind {
    /// Convert a raw u32 field value, returning [`Unknown`](Self::Unknown) for
    /// unrecognised values.
    pub fn from_raw(v: u32) -> Self {
        match v {
            1 => Self::MemoryObject,
            2 => Self::Endpoint,
            3 => Self::ReplyCap,
            4 => Self::Notification,
            0xFF => Self::Other,
            _ => Self::Unknown,
        }
    }
}

// ── Request record ───────────────────────────────────────────────────────────

/// Versioned request record for `recv_shared_v3`.
///
/// Userspace constructs this record and passes it to the kernel via syscall 31.
/// Use [`validate_request`] to pre-validate before passing to the kernel.
///
/// `#[repr(C)]` is applied here to lock the wire layout for Stage 42+43:
/// the kernel reads this struct from user memory at fixed field offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct RecvSharedV3Request {
    /// Must equal [`RECV_V3_VERSION`].
    pub version: u32,
    /// Total byte length of this record; must be ≥ [`RECV_V3_MIN_REQUEST_LEN`].
    pub record_len: u32,
    /// Endpoint capability ID to receive on.
    pub endpoint_cap: u64,
    /// Pointer to user payload buffer (userspace virtual address).
    pub payload_ptr: u64,
    /// Capacity of user payload buffer in bytes.
    pub payload_len: u64,
    /// Pointer to output metadata record buffer (0 if not needed).
    pub metadata_ptr: u64,
    /// Capacity of output metadata record in bytes.
    pub metadata_len: u64,
    /// Mapping intent flags ([`RECV_V3_MAP_READ`] | [`RECV_V3_MAP_WRITE`] or 0).
    /// Must be 0 when `metadata_ptr == 0`.
    pub map_intent: u32,
    /// Behaviour flags — reserved; must be 0 in this version.
    pub flags: u32,
    /// Timeout ticks (0 = non-blocking, [`u64::MAX`] = block forever).
    pub timeout_ticks: u64,
    /// Reserved; must be zero.
    pub reserved: [u64; 2],
}

impl RecvSharedV3Request {
    /// Construct the minimal valid blocking request for the given endpoint.
    pub const fn new_blocking(endpoint_cap: u64, payload_ptr: u64, payload_len: u64) -> Self {
        Self {
            version: RECV_V3_VERSION,
            record_len: RECV_V3_MIN_REQUEST_LEN,
            endpoint_cap,
            payload_ptr,
            payload_len,
            metadata_ptr: 0,
            metadata_len: 0,
            map_intent: 0,
            flags: 0,
            timeout_ticks: u64::MAX,
            reserved: [0; 2],
        }
    }

    /// Construct a non-blocking probe request for the given endpoint.
    pub const fn new_nonblocking(endpoint_cap: u64, payload_ptr: u64, payload_len: u64) -> Self {
        Self {
            timeout_ticks: 0,
            ..Self::new_blocking(endpoint_cap, payload_ptr, payload_len)
        }
    }

    /// Construct a blocking request with shared-memory mapping output.
    pub const fn new_with_metadata(
        endpoint_cap: u64,
        payload_ptr: u64,
        payload_len: u64,
        metadata_ptr: u64,
        metadata_len: u64,
        map_intent: u32,
    ) -> Self {
        Self {
            version: RECV_V3_VERSION,
            record_len: RECV_V3_MIN_REQUEST_LEN,
            endpoint_cap,
            payload_ptr,
            payload_len,
            metadata_ptr,
            metadata_len,
            map_intent,
            flags: 0,
            timeout_ticks: u64::MAX,
            reserved: [0; 2],
        }
    }
}

// ── Output record ─────────────────────────────────────────────────────────────

/// Versioned output record written by the kernel for `recv_shared_v3`.
///
/// Fields marked **FUTURE (unavailable)** are always zero in this stage.
///
/// `#[repr(C)]` is applied here to lock the wire layout for Stage 42+43.
/// The kernel writes 80 bytes (see [`RECV_V3_MIN_OUTPUT_LEN`]) at the
/// fixed field offsets; userspace reads this record at those same offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct RecvSharedV3Output {
    /// Must equal [`RECV_V3_VERSION`] (kernel-written).
    pub version: u32,
    /// Total byte length; must be ≥ [`RECV_V3_MIN_OUTPUT_LEN`].
    pub record_len: u32,
    /// Syscall ABI version — [`RECV_V3_ABI_VERSION`] currently.
    pub abi_version: u32,
    /// Result status; 0 = [`RECV_V3_STATUS_OK`].
    pub result_status: u32,

    // ── Authoritative fields ──────────────────────────────────────────────
    /// Sender thread ID (authoritative).
    pub sender_tid: u64,
    /// Received payload length in bytes (authoritative).
    pub message_len: u32,
    /// Message flags from `Message::flags` (authoritative).
    pub message_flags: u32,

    // ── Cap-transfer (available after cap materialization is on split path) ─
    /// Receiver-local transferred capability ID.
    /// [`RECV_V3_NO_TRANSFER_CAP`] if none transferred.
    pub transferred_cap: u64,

    // ── FUTURE: object introspection ──────────────────────────────────────
    /// **FUTURE (unavailable)**: transferred object kind; 0 now.
    pub object_kind: u32,
    /// **FUTURE (unavailable)**: transferred object generation; 0 now.
    pub object_generation: u64,
    /// **FUTURE (unavailable)**: effective rights on transferred cap; 0 now.
    pub effective_rights: u32,
    /// **FUTURE (unavailable)**: exact object size in bytes; 0 now.
    pub exact_object_size: u64,

    // ── Shared-memory mapping (available after VM mapping on split path) ──
    /// Shared-memory region offset (0 if no OPCODE_SHARED_MEM transfer).
    pub region_offset: u64,
    /// **FUTURE (unavailable)**: exact unrounded region length; 0 now.
    pub exact_region_len: u64,
    /// Mapped virtual base address (0 if no mapping performed).
    pub mapped_base: u64,
    /// Page-rounded mapped length (0 if no mapping performed).
    pub page_rounded_mapped_len: u64,
    /// Actual mapping permissions granted (0 if no mapping).
    pub actual_mapping_perm: u32,

    // ── FUTURE ────────────────────────────────────────────────────────────
    /// **FUTURE (unavailable)**: cleanup token identity; 0 now.
    pub cleanup_token: u64,
    /// **FUTURE**: VFS shared I/O request ID / descriptor generation; 0 now.
    pub request_id: u64,
}

impl RecvSharedV3Output {
    /// Construct a zeroed output record with correct version/length/ABI fields.
    pub const fn new_zeroed() -> Self {
        Self {
            version: RECV_V3_VERSION,
            record_len: RECV_V3_MIN_OUTPUT_LEN,
            abi_version: RECV_V3_ABI_VERSION,
            result_status: RECV_V3_STATUS_OK,
            sender_tid: 0,
            message_len: 0,
            message_flags: 0,
            transferred_cap: RECV_V3_NO_TRANSFER_CAP,
            object_kind: 0,
            object_generation: 0,
            effective_rights: 0,
            exact_object_size: 0,
            region_offset: 0,
            exact_region_len: 0,
            mapped_base: 0,
            page_rounded_mapped_len: 0,
            actual_mapping_perm: 0,
            cleanup_token: 0,
            request_id: 0,
        }
    }

    /// Returns true if `transferred_cap` indicates no capability was transferred.
    pub fn has_no_transfer_cap(&self) -> bool {
        self.transferred_cap == RECV_V3_NO_TRANSFER_CAP
    }

    /// Decode the `object_kind` field via [`RecvSharedV3ObjectKind::from_raw`].
    pub fn decoded_object_kind(&self) -> RecvSharedV3ObjectKind {
        RecvSharedV3ObjectKind::from_raw(self.object_kind)
    }
}

// ── Validation ────────────────────────────────────────────────────────────────

/// Validation error for `recv_shared_v3` records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvSharedV3ValidationError {
    /// `version` field does not equal [`RECV_V3_VERSION`].
    BadVersion,
    /// `record_len` is below the minimum for the record type.
    ShortRecord,
    /// A reserved or flags field was non-zero.
    NonzeroReserved,
    /// `map_intent != 0` but `metadata_ptr == 0` (no buffer for result).
    MetaMapIntentConflict,
    /// `map_intent` has unrecognised bits set.
    BadMapIntent,
    /// Output record version or length is invalid.
    BadOutputRecord,
}

/// Validate a request record.
///
/// Returns `Ok(())` if well-formed.  Mirrors the kernel's internal validation
/// exactly; can be called in userspace to pre-validate before the syscall.
pub fn validate_request(req: &RecvSharedV3Request) -> Result<(), RecvSharedV3ValidationError> {
    if req.version != RECV_V3_VERSION {
        return Err(RecvSharedV3ValidationError::BadVersion);
    }
    if req.record_len < RECV_V3_MIN_REQUEST_LEN {
        return Err(RecvSharedV3ValidationError::ShortRecord);
    }
    for &r in &req.reserved {
        if r != 0 {
            return Err(RecvSharedV3ValidationError::NonzeroReserved);
        }
    }
    if req.flags != 0 {
        return Err(RecvSharedV3ValidationError::NonzeroReserved);
    }
    if req.map_intent != 0 && req.metadata_ptr == 0 {
        return Err(RecvSharedV3ValidationError::MetaMapIntentConflict);
    }
    let known = RECV_V3_MAP_READ | RECV_V3_MAP_WRITE;
    if req.map_intent & !known != 0 {
        return Err(RecvSharedV3ValidationError::BadMapIntent);
    }
    Ok(())
}

/// Validate an output record header.
pub fn validate_output(out: &RecvSharedV3Output) -> Result<(), RecvSharedV3ValidationError> {
    if out.version != RECV_V3_VERSION {
        return Err(RecvSharedV3ValidationError::BadVersion);
    }
    if out.record_len < RECV_V3_MIN_OUTPUT_LEN {
        return Err(RecvSharedV3ValidationError::BadOutputRecord);
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_req() -> RecvSharedV3Request {
        RecvSharedV3Request::new_blocking(1, 0x1000, 128)
    }

    fn minimal_out() -> RecvSharedV3Output {
        RecvSharedV3Output::new_zeroed()
    }

    #[test]
    fn abi_valid_request_accepted() {
        assert_eq!(validate_request(&minimal_req()), Ok(()));
    }

    #[test]
    fn abi_bad_version_rejected() {
        let mut req = minimal_req();
        req.version = 0;
        assert_eq!(validate_request(&req), Err(RecvSharedV3ValidationError::BadVersion));
    }

    #[test]
    fn abi_short_record_rejected() {
        let mut req = minimal_req();
        req.record_len = RECV_V3_MIN_REQUEST_LEN - 1;
        assert_eq!(validate_request(&req), Err(RecvSharedV3ValidationError::ShortRecord));
    }

    #[test]
    fn abi_nonzero_reserved_rejected() {
        let mut req = minimal_req();
        req.reserved[1] = 42;
        assert_eq!(validate_request(&req), Err(RecvSharedV3ValidationError::NonzeroReserved));
    }

    #[test]
    fn abi_nonzero_flags_rejected() {
        let mut req = minimal_req();
        req.flags = 1;
        assert_eq!(validate_request(&req), Err(RecvSharedV3ValidationError::NonzeroReserved));
    }

    #[test]
    fn abi_map_intent_without_metadata_ptr_rejected() {
        let mut req = minimal_req();
        req.map_intent = RECV_V3_MAP_READ;
        req.metadata_ptr = 0;
        assert_eq!(validate_request(&req), Err(RecvSharedV3ValidationError::MetaMapIntentConflict));
    }

    #[test]
    fn abi_unknown_map_intent_bits_rejected() {
        let mut req = minimal_req();
        req.map_intent = 0x10;
        req.metadata_ptr = 0x2000;
        assert_eq!(validate_request(&req), Err(RecvSharedV3ValidationError::BadMapIntent));
    }

    #[test]
    fn abi_read_only_map_intent_accepted() {
        let req = RecvSharedV3Request::new_with_metadata(1, 0x1000, 128, 0x2000, 80, RECV_V3_MAP_READ);
        assert_eq!(validate_request(&req), Ok(()));
    }

    #[test]
    fn abi_read_write_map_intent_accepted() {
        let req = RecvSharedV3Request::new_with_metadata(
            1, 0x1000, 128, 0x2000, 80, RECV_V3_MAP_READ | RECV_V3_MAP_WRITE,
        );
        assert_eq!(validate_request(&req), Ok(()));
    }

    #[test]
    fn abi_valid_output_accepted() {
        assert_eq!(validate_output(&minimal_out()), Ok(()));
    }

    #[test]
    fn abi_output_bad_version_rejected() {
        let mut out = minimal_out();
        out.version = 1;
        assert_eq!(validate_output(&out), Err(RecvSharedV3ValidationError::BadVersion));
    }

    #[test]
    fn abi_output_short_record_rejected() {
        let mut out = minimal_out();
        out.record_len = RECV_V3_MIN_OUTPUT_LEN - 1;
        assert_eq!(validate_output(&out), Err(RecvSharedV3ValidationError::BadOutputRecord));
    }

    #[test]
    fn abi_output_no_transfer_cap_sentinel() {
        let out = minimal_out();
        assert!(out.has_no_transfer_cap());
        assert_eq!(out.transferred_cap, RECV_V3_NO_TRANSFER_CAP);
    }

    #[test]
    fn abi_object_kind_from_raw_roundtrip() {
        assert_eq!(RecvSharedV3ObjectKind::from_raw(0), RecvSharedV3ObjectKind::Unknown);
        assert_eq!(RecvSharedV3ObjectKind::from_raw(1), RecvSharedV3ObjectKind::MemoryObject);
        assert_eq!(RecvSharedV3ObjectKind::from_raw(2), RecvSharedV3ObjectKind::Endpoint);
        assert_eq!(RecvSharedV3ObjectKind::from_raw(99), RecvSharedV3ObjectKind::Unknown);
    }

    #[test]
    fn abi_new_nonblocking_has_zero_timeout() {
        let req = RecvSharedV3Request::new_nonblocking(5, 0x1000, 64);
        assert_eq!(req.timeout_ticks, 0);
        assert_eq!(validate_request(&req), Ok(()));
    }

    #[test]
    fn abi_constants_agree_with_kernel() {
        // These must stay in sync with recv_core.rs recv_shared_v3 module.
        assert_eq!(RECV_V3_VERSION, 3);
        assert_eq!(RECV_V3_MIN_REQUEST_LEN, 64);
        assert_eq!(RECV_V3_MIN_OUTPUT_LEN, 80);
        assert_eq!(RECV_V3_MAP_READ, 0x1);
        assert_eq!(RECV_V3_MAP_WRITE, 0x2);
        assert_eq!(RECV_V3_NO_TRANSFER_CAP, u64::MAX);
    }
}
