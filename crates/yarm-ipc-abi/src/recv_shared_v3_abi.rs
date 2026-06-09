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

/// Extended output record length that includes `exact_region_len` (Stage 50).
///
/// A caller providing at least this many bytes for `metadata_len` will receive
/// the DmaRegion sub-region byte length at offset 80.  Callers with 80-byte
/// buffers receive the original fields unchanged.
pub const RECV_V3_EXTENDED_OUTPUT_LEN: u32 = 88;

/// Mapped output record length: covers shared-mapping metadata through
/// `actual_mapping_perm @104..108` (Stage 54+55).
///
/// A caller providing at least this many bytes for `metadata_len` will receive
/// `mapped_base @88`, `page_rounded_mapped_len @96`, and
/// `actual_mapping_perm @104` when live shared-memory mapping is enabled.
/// **Currently all three fields are always 0** — `map_intent != 0` still
/// returns `InvalidArgs` in the current stage.
pub const RECV_V3_MAPPED_OUTPUT_LEN: u32 = 108;

/// Map-intent flag: map the transferred region read-only.
pub const RECV_V3_MAP_READ: u32 = 0x1;

/// Map-intent flag: map the transferred region read-write (implies READ).
pub const RECV_V3_MAP_WRITE: u32 = 0x2;

/// Sentinel for `transferred_cap` when no capability was transferred.
pub const RECV_V3_NO_TRANSFER_CAP: u64 = u64::MAX;

/// Sentinel for FUTURE fields that are unavailable in this ABI version.
pub const RECV_V3_FIELD_UNAVAILABLE: u64 = 0;

/// Sentinel for `cleanup_token` when no live shared-memory mapping exists (Stage 54+).
///
/// The `cleanup_token` output field is always 0 in the current stage — no VM
/// mapping is performed and no cleanup identity is allocated.  This sentinel
/// must be tested before any cleanup action is taken in userspace.
pub const RECV_V3_CLEANUP_TOKEN_NONE: u64 = 0;

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

/// Kind of a transferred capability (available from Stage 47+48; DmaRegion from Stage 52+53).
///
/// Populated in `RecvSharedV3Output::object_kind` whenever a cap is
/// materialized.  [`Unknown`](Self::Unknown) (0) is written when no
/// capability was transferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum RecvSharedV3ObjectKind {
    /// No capability transferred, or kind is genuinely unrecognised.
    Unknown = 0,
    /// The transferred cap wraps a memory object (anonymous or file-backed).
    MemoryObject = 1,
    /// The transferred cap wraps an IPC endpoint.
    Endpoint = 2,
    /// The transferred cap is a one-shot reply capability.
    ReplyCap = 3,
    /// The transferred cap wraps a notification object.
    Notification = 4,
    /// The transferred cap is a DmaRegion sub-region slice (Stage 52+53).
    /// `exact_region_len` carries the authoritative sub-region byte length;
    /// `exact_object_size` is 0 (DmaRegion has no separate object-size concept).
    DmaRegion = 5,
    /// Other or forward-compatibility kind not listed above.
    Other = 0xFF,
}

// ── Effective-rights bit constants ────────────────────────────────────────────

/// Effective-rights bit: receiver may read from the transferred object.
pub const RECV_V3_CAP_RIGHTS_READ: u32 = 0x01;
/// Effective-rights bit: receiver may write to the transferred object.
pub const RECV_V3_CAP_RIGHTS_WRITE: u32 = 0x02;
/// Effective-rights bit: receiver may map the transferred object into its address space.
pub const RECV_V3_CAP_RIGHTS_MAP: u32 = 0x04;
/// Effective-rights bit: receiver may send on the transferred endpoint.
pub const RECV_V3_CAP_RIGHTS_SEND: u32 = 0x08;
/// Effective-rights bit: receiver may receive on the transferred endpoint.
pub const RECV_V3_CAP_RIGHTS_RECEIVE: u32 = 0x10;
/// Effective-rights bit: receiver may use the transferred cap for scheduling.
pub const RECV_V3_CAP_RIGHTS_SCHEDULE: u32 = 0x20;
/// Effective-rights bit: receiver may signal on the transferred notification.
pub const RECV_V3_CAP_RIGHTS_SIGNAL: u32 = 0x40;
/// Effective-rights bit: receiver may wait on the transferred notification.
pub const RECV_V3_CAP_RIGHTS_WAIT: u32 = 0x80;

impl RecvSharedV3ObjectKind {
    /// Convert a raw u32 field value, returning [`Unknown`](Self::Unknown) for
    /// unrecognised values.
    pub fn from_raw(v: u32) -> Self {
        match v {
            1 => Self::MemoryObject,
            2 => Self::Endpoint,
            3 => Self::ReplyCap,
            4 => Self::Notification,
            5 => Self::DmaRegion,
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

    // ── Object introspection (Stage 47+48) ───────────────────────────────
    /// Transferred capability kind; [`RecvSharedV3ObjectKind::Unknown`] (0) if none.
    /// Populated whenever a cap is materialized (Stage 47+48).
    pub object_kind: u32,
    // 4 bytes C-layout padding for u64 alignment follow object_kind (offset 44-47).
    /// Transferred capability object generation; 0 if unavailable (e.g. MemoryObject).
    /// Populated for Endpoint / Notification / Reply caps; 0 for MemoryObject.
    pub object_generation: u64,
    /// Effective rights on the receiver-local transferred cap (`CapRights::bits` as u32).
    /// 0 if no capability was transferred.
    pub effective_rights: u32,
    // 4 bytes C-layout padding for u64 alignment follow effective_rights (offset 60-63).
    /// Exact object size in bytes (Stage 49): page-aligned byte length of the
    /// transferred MemoryObject as stored in the kernel registry.
    /// **0 for all non-MemoryObject cap kinds and when no cap was transferred.**
    /// The value is always a non-zero multiple of the system page size when a
    /// MemoryObject cap was transferred.
    pub exact_object_size: u64,

    // ── Shared-memory mapping (available after VM mapping on split path) ──
    /// Shared-memory region offset (0 if no OPCODE_SHARED_MEM transfer).
    pub region_offset: u64,
    /// Exact DmaRegion sub-region byte length (Stage 50): page-aligned byte length
    /// embedded in the transferred DmaRegion capability.  Authoritative when
    /// `object_kind == DmaRegion (5)` (Stage 52+53).
    /// **0 for MemoryObject, Endpoint, Notification, ReplyCap, and plain messages.**
    /// Only written when the caller provides at least [`RECV_V3_EXTENDED_OUTPUT_LEN`]
    /// bytes in `metadata_len`; reads as 0 from an 80-byte buffer.
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

// ── Cleanup-token scaffold (helper-only, Stage 52+53) ────────────────────────

/// Identity record for a future recv_shared_v3 cleanup obligation (Stage 56+57).
///
/// **FUTURE (unavailable)**: no live mappings exist in the current stage; all
/// instances of this type are conceptual scaffolding.  The corresponding
/// `RecvSharedV3Output::cleanup_token` field is always 0 because:
///   - No VM mapping is performed (`map_intent` returns `InvalidArgs`).
///   - No per-mapping cleanup registry is used in the live path.
///   - No process-exit hook for shared mappings is implemented.
///   - No VFS shared-I/O lifecycle binding is implemented.
///
/// When live mapping is eventually enabled, a non-zero `cleanup_token` in the
/// output record will correspond to an entry in a kernel-side cleanup registry
/// identified by the fields below.
///
/// # Field availability
///
/// | Field | Available |
/// |---|---|
/// | `receiver_cap` | Now (after cap materialisation) |
/// | `object_kind` | Now |
/// | `region_len` | Now (DmaRegion cap field / MemoryObject size) |
/// | `transfer_token` | After live mapping (the returned cleanup token) |
/// | `mapped_base` | After live mapping |
/// | `mapped_len` | After live mapping |
/// | `actual_mapping_perm` | After live mapping |
/// | `map_intent` | Now (request field) |
///
/// # Invariants when `is_active()` is `true` (future)
///
/// - `receiver_cap != u64::MAX`
/// - `object_kind != 0`
/// - `region_len > 0` and `region_len % PAGE_SIZE == 0`
/// - `transfer_token != 0`
/// - `mapped_base != 0`
/// - `mapped_len != 0`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecvSharedV3CleanupIdentity {
    /// Receiver-local materialised capability ID.
    /// `u64::MAX` when no cleanup obligation exists (sentinel).
    pub receiver_cap: u64,
    /// Object kind discriminant of the transferred capability.
    pub object_kind: u32,
    /// Page-aligned byte length of the region.
    /// For `DmaRegion` this is the sub-region length; for `MemoryObject` the
    /// object size.
    pub region_len: u64,
    /// Opaque cleanup token returned by the kernel
    /// (`RecvSharedV3Output::cleanup_token`).
    /// 0 = no live mapping; non-zero = cleanup required.
    pub transfer_token: u64,
    // ── Available after live mapping ────────────────────────────────────────
    /// Mapped virtual base address in the receiver's address space.
    /// 0 until a live mapping is established.
    pub mapped_base: u64,
    /// Page-rounded byte length of the mapping.
    /// 0 until a live mapping is established.
    pub mapped_len: u64,
    /// Actual mapping permissions granted (bitmask of `RECV_V3_MAP_*` bits).
    /// 0 until a live mapping is established.
    pub actual_mapping_perm: u32,
    /// Map intent flags from the original request (`RECV_V3_MAP_READ` etc.).
    /// Available now (from the request); 0 when no mapping was requested.
    pub map_intent: u32,
}

impl RecvSharedV3CleanupIdentity {
    /// Construct the sentinel value representing no active cleanup obligation.
    ///
    /// `receiver_cap` is set to `u64::MAX`; all other fields are 0.
    pub const fn none() -> Self {
        Self {
            receiver_cap: u64::MAX,
            object_kind: 0,
            region_len: 0,
            transfer_token: 0,
            mapped_base: 0,
            mapped_len: 0,
            actual_mapping_perm: 0,
            map_intent: 0,
        }
    }

    /// Returns `true` when this identity represents an active cleanup obligation
    /// (i.e. `transfer_token != 0`).
    ///
    /// Always `false` in the current stage — no live mappings exist.
    pub const fn is_active(&self) -> bool {
        self.transfer_token != 0
    }

    /// Returns `true` when `mapped_base` and `mapped_len` are both non-zero,
    /// indicating that mapping-phase fields have been populated.
    ///
    /// Always `false` in the current stage.
    pub const fn is_mapped(&self) -> bool {
        self.mapped_base != 0 && self.mapped_len != 0
    }

    /// Returns `true` if the pre-mapping structural invariants hold:
    ///
    /// - `receiver_cap != u64::MAX`
    /// - `object_kind != 0`
    /// - `region_len > 0` and a positive multiple of `page_size`
    ///
    /// Does NOT check `mapped_base`, `transfer_token`, or the live registry.
    pub fn is_structurally_valid(&self, page_size: usize) -> bool {
        self.receiver_cap != u64::MAX
            && self.object_kind != 0
            && self.region_len > 0
            && page_size > 0
            && self.region_len % page_size as u64 == 0
    }
}

// ── Layout assertions ─────────────────────────────────────────────────────────

// Verify that the #[repr(C)] struct field offsets match the byte positions
// that write_v3_output_to_user writes.  These must agree with the 80-byte raw
// buffer written by the kernel to the user's metadata_ptr.
const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(RecvSharedV3Output, version) == 0);
    assert!(offset_of!(RecvSharedV3Output, sender_tid) == 16);
    assert!(offset_of!(RecvSharedV3Output, transferred_cap) == 32);
    assert!(offset_of!(RecvSharedV3Output, object_kind) == 40);
    // 4 bytes padding at 44-47 for u64 alignment
    assert!(offset_of!(RecvSharedV3Output, object_generation) == 48);
    assert!(offset_of!(RecvSharedV3Output, effective_rights) == 56);
    // 4 bytes padding at 60-63 for u64 alignment
    assert!(offset_of!(RecvSharedV3Output, exact_object_size) == 64);
    assert!(offset_of!(RecvSharedV3Output, region_offset) == 72);
    assert!(offset_of!(RecvSharedV3Output, exact_region_len) == 80);
    // Fields beyond @88 are never written by the current kernel (write window = min(out_len,88)).
    assert!(offset_of!(RecvSharedV3Output, mapped_base) == 88);
    assert!(offset_of!(RecvSharedV3Output, cleanup_token) == 112);
};

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
        assert_eq!(
            validate_request(&req),
            Err(RecvSharedV3ValidationError::BadVersion)
        );
    }

    #[test]
    fn abi_short_record_rejected() {
        let mut req = minimal_req();
        req.record_len = RECV_V3_MIN_REQUEST_LEN - 1;
        assert_eq!(
            validate_request(&req),
            Err(RecvSharedV3ValidationError::ShortRecord)
        );
    }

    #[test]
    fn abi_nonzero_reserved_rejected() {
        let mut req = minimal_req();
        req.reserved[1] = 42;
        assert_eq!(
            validate_request(&req),
            Err(RecvSharedV3ValidationError::NonzeroReserved)
        );
    }

    #[test]
    fn abi_nonzero_flags_rejected() {
        let mut req = minimal_req();
        req.flags = 1;
        assert_eq!(
            validate_request(&req),
            Err(RecvSharedV3ValidationError::NonzeroReserved)
        );
    }

    #[test]
    fn abi_map_intent_without_metadata_ptr_rejected() {
        let mut req = minimal_req();
        req.map_intent = RECV_V3_MAP_READ;
        req.metadata_ptr = 0;
        assert_eq!(
            validate_request(&req),
            Err(RecvSharedV3ValidationError::MetaMapIntentConflict)
        );
    }

    #[test]
    fn abi_unknown_map_intent_bits_rejected() {
        let mut req = minimal_req();
        req.map_intent = 0x10;
        req.metadata_ptr = 0x2000;
        assert_eq!(
            validate_request(&req),
            Err(RecvSharedV3ValidationError::BadMapIntent)
        );
    }

    #[test]
    fn abi_read_only_map_intent_accepted() {
        let req =
            RecvSharedV3Request::new_with_metadata(1, 0x1000, 128, 0x2000, 80, RECV_V3_MAP_READ);
        assert_eq!(validate_request(&req), Ok(()));
    }

    #[test]
    fn abi_read_write_map_intent_accepted() {
        let req = RecvSharedV3Request::new_with_metadata(
            1,
            0x1000,
            128,
            0x2000,
            80,
            RECV_V3_MAP_READ | RECV_V3_MAP_WRITE,
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
        assert_eq!(
            validate_output(&out),
            Err(RecvSharedV3ValidationError::BadVersion)
        );
    }

    #[test]
    fn abi_output_short_record_rejected() {
        let mut out = minimal_out();
        out.record_len = RECV_V3_MIN_OUTPUT_LEN - 1;
        assert_eq!(
            validate_output(&out),
            Err(RecvSharedV3ValidationError::BadOutputRecord)
        );
    }

    #[test]
    fn abi_output_no_transfer_cap_sentinel() {
        let out = minimal_out();
        assert!(out.has_no_transfer_cap());
        assert_eq!(out.transferred_cap, RECV_V3_NO_TRANSFER_CAP);
    }

    #[test]
    fn abi_object_kind_from_raw_roundtrip() {
        assert_eq!(
            RecvSharedV3ObjectKind::from_raw(0),
            RecvSharedV3ObjectKind::Unknown
        );
        assert_eq!(
            RecvSharedV3ObjectKind::from_raw(1),
            RecvSharedV3ObjectKind::MemoryObject
        );
        assert_eq!(
            RecvSharedV3ObjectKind::from_raw(2),
            RecvSharedV3ObjectKind::Endpoint
        );
        assert_eq!(
            RecvSharedV3ObjectKind::from_raw(99),
            RecvSharedV3ObjectKind::Unknown
        );
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
        assert_eq!(RECV_V3_EXTENDED_OUTPUT_LEN, 88);
        assert_eq!(RECV_V3_MAP_READ, 0x1);
        assert_eq!(RECV_V3_MAP_WRITE, 0x2);
        assert_eq!(RECV_V3_NO_TRANSFER_CAP, u64::MAX);
    }

    #[test]
    fn abi_cap_rights_constants_match_cap_rights_bits() {
        // effective_rights field uses the same bit layout as CapRights::bits().
        assert_eq!(RECV_V3_CAP_RIGHTS_READ, 0x01);
        assert_eq!(RECV_V3_CAP_RIGHTS_WRITE, 0x02);
        assert_eq!(RECV_V3_CAP_RIGHTS_MAP, 0x04);
        assert_eq!(RECV_V3_CAP_RIGHTS_SEND, 0x08);
        assert_eq!(RECV_V3_CAP_RIGHTS_RECEIVE, 0x10);
        assert_eq!(RECV_V3_CAP_RIGHTS_SCHEDULE, 0x20);
        assert_eq!(RECV_V3_CAP_RIGHTS_SIGNAL, 0x40);
        assert_eq!(RECV_V3_CAP_RIGHTS_WAIT, 0x80);
    }

    #[test]
    fn abi_object_kind_values_are_stable() {
        assert_eq!(RecvSharedV3ObjectKind::Unknown as u32, 0);
        assert_eq!(RecvSharedV3ObjectKind::MemoryObject as u32, 1);
        assert_eq!(RecvSharedV3ObjectKind::Endpoint as u32, 2);
        assert_eq!(RecvSharedV3ObjectKind::ReplyCap as u32, 3);
        assert_eq!(RecvSharedV3ObjectKind::Notification as u32, 4);
        assert_eq!(RecvSharedV3ObjectKind::Other as u32, 0xFF);
    }

    #[test]
    fn abi_object_kind_anonymous_memory_object_is_one() {
        // Stage 47+48: MemoryObject kind discriminant written by kernel.
        let mut out = minimal_out();
        out.object_kind = RecvSharedV3ObjectKind::MemoryObject as u32;
        assert_eq!(
            out.decoded_object_kind(),
            RecvSharedV3ObjectKind::MemoryObject
        );
    }

    #[test]
    fn abi_effective_rights_read_write_map_combo() {
        // Verify the expected rights combination for a transferred Anonymous MemoryObject.
        let rwm = RECV_V3_CAP_RIGHTS_READ | RECV_V3_CAP_RIGHTS_WRITE | RECV_V3_CAP_RIGHTS_MAP;
        assert_eq!(rwm, 0x07);
    }

    #[test]
    fn abi_exact_object_size_zero_when_no_cap() {
        // Stage 49: exact_object_size must be 0 when no cap is transferred.
        let out = minimal_out();
        assert_eq!(
            out.exact_object_size, 0,
            "exact_object_size must be 0 when no cap"
        );
    }

    #[test]
    fn abi_exact_object_size_field_at_offset_64() {
        // Layout assertion: exact_object_size lives at byte offset 64 in the output record.
        use core::mem::offset_of;
        assert_eq!(offset_of!(RecvSharedV3Output, exact_object_size), 64);
    }

    // ── Stage 50: exact_region_len field ─────────────────────────────────────

    #[test]
    fn abi_exact_region_len_zero_when_no_cap() {
        // Stage 50: exact_region_len must be 0 when no DmaRegion cap is transferred.
        let out = minimal_out();
        assert_eq!(
            out.exact_region_len, 0,
            "exact_region_len must be 0 when no cap"
        );
    }

    #[test]
    fn abi_exact_region_len_field_at_offset_80() {
        // Layout assertion: exact_region_len lives at byte offset 80.
        use core::mem::offset_of;
        assert_eq!(offset_of!(RecvSharedV3Output, exact_region_len), 80);
    }

    #[test]
    fn abi_extended_output_len_is_88() {
        assert_eq!(RECV_V3_EXTENDED_OUTPUT_LEN, 88);
        assert!(RECV_V3_EXTENDED_OUTPUT_LEN > RECV_V3_MIN_OUTPUT_LEN);
    }

    // ── Stage 52+53: DmaRegion object kind ───────────────────────────────────

    #[test]
    fn abi_dma_region_kind_discriminant_is_five() {
        // DmaRegion is now a first-class object kind with a stable discriminant.
        assert_eq!(RecvSharedV3ObjectKind::DmaRegion as u32, 5);
    }

    #[test]
    fn abi_object_kind_from_raw_dma_region() {
        assert_eq!(
            RecvSharedV3ObjectKind::from_raw(5),
            RecvSharedV3ObjectKind::DmaRegion
        );
    }

    #[test]
    fn abi_object_kind_values_include_dma_region() {
        assert_eq!(RecvSharedV3ObjectKind::Unknown as u32, 0);
        assert_eq!(RecvSharedV3ObjectKind::MemoryObject as u32, 1);
        assert_eq!(RecvSharedV3ObjectKind::Endpoint as u32, 2);
        assert_eq!(RecvSharedV3ObjectKind::ReplyCap as u32, 3);
        assert_eq!(RecvSharedV3ObjectKind::Notification as u32, 4);
        assert_eq!(RecvSharedV3ObjectKind::DmaRegion as u32, 5);
        assert_eq!(RecvSharedV3ObjectKind::Other as u32, 0xFF);
    }

    // ── Stage 52+53: cleanup-token scaffold ──────────────────────────────────

    #[test]
    fn abi_cleanup_token_none_sentinel_is_zero() {
        assert_eq!(RECV_V3_CLEANUP_TOKEN_NONE, 0);
    }

    #[test]
    fn abi_cleanup_token_zero_in_zeroed_output() {
        let out = minimal_out();
        assert_eq!(
            out.cleanup_token, 0,
            "cleanup_token must be 0 in zeroed output"
        );
    }

    #[test]
    fn abi_cleanup_identity_none_is_not_active() {
        let identity = RecvSharedV3CleanupIdentity::none();
        assert!(
            !identity.is_active(),
            "sentinel identity must not be active"
        );
    }

    #[test]
    fn abi_cleanup_identity_none_is_not_structurally_valid() {
        let identity = RecvSharedV3CleanupIdentity::none();
        assert!(
            !identity.is_structurally_valid(4096),
            "sentinel identity must fail structural validation"
        );
    }

    #[test]
    fn abi_cleanup_identity_structurally_valid_requires_page_aligned_len() {
        let mut identity = RecvSharedV3CleanupIdentity::none();
        identity.receiver_cap = 5;
        identity.object_kind = RecvSharedV3ObjectKind::DmaRegion as u32;
        identity.region_len = 4096; // exactly one page
        identity.transfer_token = 0; // still not active, but structurally valid otherwise
        assert!(
            identity.is_structurally_valid(4096),
            "page-aligned len with non-sentinel cap must be valid"
        );
        identity.region_len = 100; // not page-aligned
        assert!(
            !identity.is_structurally_valid(4096),
            "non-page-aligned len must fail validation"
        );
    }

    #[test]
    fn abi_cleanup_identity_requires_non_sentinel_cap() {
        let mut identity = RecvSharedV3CleanupIdentity::none();
        identity.object_kind = RecvSharedV3ObjectKind::DmaRegion as u32;
        identity.region_len = 4096;
        // receiver_cap is still u64::MAX (sentinel) — must fail
        assert!(
            !identity.is_structurally_valid(4096),
            "sentinel receiver_cap must fail structural validation"
        );
    }

    #[test]
    fn abi_cleanup_token_field_at_offset_112() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(RecvSharedV3Output, cleanup_token), 112);
    }

    // ── Stage 54+55: mapped output length and field offsets ──────────────────

    #[test]
    fn abi_mapped_output_len_is_108() {
        assert_eq!(RECV_V3_MAPPED_OUTPUT_LEN, 108);
    }

    #[test]
    fn abi_mapped_base_at_offset_88() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(RecvSharedV3Output, mapped_base), 88);
    }

    #[test]
    fn abi_page_rounded_mapped_len_at_offset_96() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(RecvSharedV3Output, page_rounded_mapped_len), 96);
    }

    #[test]
    fn abi_actual_mapping_perm_at_offset_104() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(RecvSharedV3Output, actual_mapping_perm), 104);
    }

    #[test]
    fn abi_mapped_output_len_covers_actual_mapping_perm() {
        // RECV_V3_MAPPED_OUTPUT_LEN = offset_of(actual_mapping_perm) + size_of::<u32>()
        // = 104 + 4 = 108.
        use core::mem::{offset_of, size_of};
        let field_end = offset_of!(RecvSharedV3Output, actual_mapping_perm) + size_of::<u32>();
        assert_eq!(field_end, RECV_V3_MAPPED_OUTPUT_LEN as usize);
    }

    // ── Stage 56+57: expanded RecvSharedV3CleanupIdentity ───────────────────

    #[test]
    fn abi_cleanup_identity_none_has_all_new_fields_zero() {
        // Stage 56+57: none() must zero all post-mapping fields.
        let id = RecvSharedV3CleanupIdentity::none();
        assert_eq!(id.mapped_base, 0, "mapped_base must be 0 in none()");
        assert_eq!(id.mapped_len, 0, "mapped_len must be 0 in none()");
        assert_eq!(
            id.actual_mapping_perm, 0,
            "actual_mapping_perm must be 0 in none()"
        );
        assert_eq!(id.map_intent, 0, "map_intent must be 0 in none()");
    }

    #[test]
    fn abi_cleanup_identity_is_mapped_false_in_none() {
        // none() has mapped_base=0 and mapped_len=0, so is_mapped() must be false.
        let id = RecvSharedV3CleanupIdentity::none();
        assert!(!id.is_mapped(), "none() identity must not be mapped");
    }

    #[test]
    fn abi_cleanup_identity_is_mapped_requires_both_base_and_len() {
        let mut id = RecvSharedV3CleanupIdentity::none();
        id.mapped_base = 0x4000;
        assert!(!id.is_mapped(), "mapped_len=0 must keep is_mapped false");
        id.mapped_base = 0;
        id.mapped_len = 4096;
        assert!(!id.is_mapped(), "mapped_base=0 must keep is_mapped false");
        id.mapped_base = 0x4000;
        assert!(id.is_mapped(), "both nonzero must make is_mapped true");
    }

    #[test]
    fn abi_cleanup_identity_is_active_and_is_mapped_are_independent() {
        let mut id = RecvSharedV3CleanupIdentity::none();
        id.transfer_token = 1; // active (transfer_token != 0)
        id.mapped_base = 0; // but not yet mapped
        assert!(id.is_active(), "non-zero transfer_token must be active");
        assert!(
            !id.is_mapped(),
            "zero mapped_base must keep is_mapped false"
        );

        id.transfer_token = 0; // not active
        id.mapped_base = 0x4000;
        id.mapped_len = 4096; // mapped but not active
        assert!(!id.is_active(), "zero transfer_token must not be active");
        assert!(id.is_mapped(), "nonzero base+len must be mapped");
    }

    #[test]
    fn abi_cleanup_identity_full_round_trip() {
        // Construct a fully populated identity and verify all fields.
        let mut id = RecvSharedV3CleanupIdentity::none();
        id.receiver_cap = 7;
        id.object_kind = RecvSharedV3ObjectKind::DmaRegion as u32;
        id.region_len = 4096;
        id.transfer_token = 0x0001_0001; // nonzero → active
        id.mapped_base = 0x8000;
        id.mapped_len = 4096;
        id.actual_mapping_perm = RECV_V3_MAP_READ;
        id.map_intent = RECV_V3_MAP_READ;

        assert!(id.is_active());
        assert!(id.is_mapped());
        assert!(id.is_structurally_valid(4096));
        assert_eq!(id.actual_mapping_perm, RECV_V3_MAP_READ);
        assert_eq!(id.map_intent, RECV_V3_MAP_READ);
        assert_eq!(id.mapped_base, 0x8000);
        assert_eq!(id.mapped_len, 4096);
    }
}
