// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! **Draft helpers for `recv_shared_v3` — NOT a live syscall.**
//!
//! No syscall number has been assigned.  These helpers let callers construct
//! and inspect `recv_shared_v3` request/output records without invoking any
//! syscall.  They will be wired to a live syscall in a future stage once the
//! ABI is confirmed stable.
//!
//! # Stability
//!
//! The types in this module re-export from [`yarm_ipc_abi::recv_shared_v3_abi`].
//! The module name and public API are considered **draft**; breaking changes
//! are expected before the live syscall is added.

pub use yarm_ipc_abi::recv_shared_v3_abi::{
    RecvSharedV3ObjectKind, RecvSharedV3Output, RecvSharedV3Request,
    RecvSharedV3ValidationError,
    RECV_V3_ABI_VERSION, RECV_V3_FIELD_UNAVAILABLE, RECV_V3_MAP_READ, RECV_V3_MAP_WRITE,
    RECV_V3_MIN_OUTPUT_LEN, RECV_V3_MIN_REQUEST_LEN, RECV_V3_NO_TRANSFER_CAP,
    RECV_V3_STATUS_BAD_REQUEST, RECV_V3_STATUS_INVALID_CAP, RECV_V3_STATUS_OK,
    RECV_V3_STATUS_TIMED_OUT, RECV_V3_STATUS_WOULD_BLOCK, RECV_V3_VERSION,
    validate_output, validate_request,
};

// ── Builder ───────────────────────────────────────────────────────────────────

/// Builder for constructing a [`RecvSharedV3Request`] record.
///
/// **Draft — no live syscall.**
#[derive(Debug, Clone)]
pub struct RecvSharedV3Builder {
    req: RecvSharedV3Request,
}

impl RecvSharedV3Builder {
    /// Start with a blocking receive on the given endpoint.
    pub fn new(endpoint_cap: u64, payload_ptr: u64, payload_len: u64) -> Self {
        Self {
            req: RecvSharedV3Request::new_blocking(endpoint_cap, payload_ptr, payload_len),
        }
    }

    /// Set metadata output buffer for shared-memory / cap-transfer results.
    pub fn metadata(mut self, ptr: u64, len: u64) -> Self {
        self.req.metadata_ptr = ptr;
        self.req.metadata_len = len;
        self
    }

    /// Set map intent (use [`RECV_V3_MAP_READ`] / [`RECV_V3_MAP_WRITE`] bits).
    pub fn map_intent(mut self, intent: u32) -> Self {
        self.req.map_intent = intent;
        self
    }

    /// Set a deadline timeout (0 = non-blocking, `u64::MAX` = block forever).
    pub fn timeout_ticks(mut self, ticks: u64) -> Self {
        self.req.timeout_ticks = ticks;
        self
    }

    /// Validate and return the request record.
    ///
    /// Returns `Err` if the constructed record violates any constraint.
    pub fn build(self) -> Result<RecvSharedV3Request, RecvSharedV3ValidationError> {
        validate_request(&self.req)?;
        Ok(self.req)
    }

    /// Return the record without validation (for testing invalid cases).
    pub fn build_unchecked(self) -> RecvSharedV3Request {
        self.req
    }
}

// ── Output helpers ────────────────────────────────────────────────────────────

/// Allocate a zeroed output record suitable for passing to the kernel.
///
/// **Draft — no live syscall.**
pub fn alloc_output() -> RecvSharedV3Output {
    RecvSharedV3Output::new_zeroed()
}

/// Returns `true` if `out.result_status == RECV_V3_STATUS_OK`.
pub fn output_is_ok(out: &RecvSharedV3Output) -> bool {
    out.result_status == RECV_V3_STATUS_OK
}

/// Returns `true` if a capability was transferred (transferred_cap is not the
/// `RECV_V3_NO_TRANSFER_CAP` sentinel).
pub fn output_has_transfer_cap(out: &RecvSharedV3Output) -> bool {
    !out.has_no_transfer_cap()
}

/// Returns `true` if the output contains a valid shared-memory mapping.
pub fn output_has_mapping(out: &RecvSharedV3Output) -> bool {
    out.mapped_base != 0 && out.page_rounded_mapped_len != 0
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_minimal_blocking_is_valid() {
        let req = RecvSharedV3Builder::new(1, 0x1000, 128).build().unwrap();
        assert_eq!(req.version, RECV_V3_VERSION);
        assert_eq!(req.timeout_ticks, u64::MAX);
        assert_eq!(req.map_intent, 0);
    }

    #[test]
    fn builder_with_metadata_and_map_intent_is_valid() {
        let req = RecvSharedV3Builder::new(2, 0x1000, 128)
            .metadata(0x2000, 80)
            .map_intent(RECV_V3_MAP_READ)
            .build()
            .unwrap();
        assert_eq!(req.metadata_ptr, 0x2000);
        assert_eq!(req.map_intent, RECV_V3_MAP_READ);
    }

    #[test]
    fn builder_map_intent_without_metadata_is_invalid() {
        let err = RecvSharedV3Builder::new(1, 0x1000, 64)
            .map_intent(RECV_V3_MAP_READ)
            .build()
            .unwrap_err();
        assert_eq!(err, RecvSharedV3ValidationError::MetaMapIntentConflict);
    }

    #[test]
    fn builder_nonblocking() {
        let req = RecvSharedV3Builder::new(3, 0x1000, 64)
            .timeout_ticks(0)
            .build()
            .unwrap();
        assert_eq!(req.timeout_ticks, 0);
    }

    #[test]
    fn alloc_output_is_valid() {
        let out = alloc_output();
        assert_eq!(validate_output(&out), Ok(()));
        assert!(out.has_no_transfer_cap());
        assert!(!output_has_mapping(&out));
    }

    #[test]
    fn output_helpers_work() {
        let out = alloc_output();
        assert!(output_is_ok(&out));
        assert!(!output_has_transfer_cap(&out));
        assert!(!output_has_mapping(&out));
    }

    #[test]
    fn object_kind_unknown_for_zero() {
        let out = alloc_output();
        assert_eq!(out.decoded_object_kind(), RecvSharedV3ObjectKind::Unknown);
    }
}
