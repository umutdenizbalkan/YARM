// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

// SUP-L1 compatibility bridge for the SUP-7/SUP-8 PM restart ABI review tests.
//
// The canonical fixed-size codec now lives in the global process IPC ABI layer
// (`yarm_ipc_abi::process_abi`). Keep the historical review names as re-exports
// so review/golden-vector tests exercise the promoted implementation instead of
// a second divergent codec.

#[allow(unused_imports)]
pub use yarm_ipc_abi::process_abi::{
    PM_RESTART_REPLY_FAILURE_OFFSET, PM_RESTART_REPLY_REQUEST_ID_OFFSET,
    PM_RESTART_REPLY_RETRY_TICK_OFFSET, PM_RESTART_REPLY_STATUS_OFFSET, PM_RESTART_REPLY_V1_LEN,
    PM_RESTART_REPLY_VERSION_OFFSET, PM_RESTART_REQUEST_ID_OFFSET,
    PM_RESTART_REQUEST_REASON_OFFSET, PM_RESTART_REQUEST_SERVICE_NAME_LEN_OFFSET,
    PM_RESTART_REQUEST_SERVICE_NAME_OFFSET, PM_RESTART_REQUEST_TARGET_TID_OFFSET,
    PM_RESTART_REQUEST_TOKEN_FINGERPRINT_OFFSET, PM_RESTART_REQUEST_TOKEN_OWNER_OFFSET,
    PM_RESTART_REQUEST_TOKEN_RESERVED_OFFSET, PM_RESTART_REQUEST_V1_LEN,
    PM_RESTART_REQUEST_VERSION_OFFSET,
    PM_RESTART_SERVICE_NAME_MAX as PM_RESTART_REVIEW_SERVICE_NAME_MAX,
    PM_RESTART_VERSION_V1 as PM_RESTART_REVIEW_VERSION_V1,
    PmRestartCodecError as PmRestartReviewCodecError, PmRestartFailure as PmRestartReviewFailure,
    PmRestartReason as PmRestartReviewReason, PmRestartReplyStatus as PmRestartReviewReplyStatus,
    PmRestartReplyV1 as PmRestartReplyV1Review, PmRestartRequestV1 as PmRestartRequestV1Review,
    PmRestartTokenDescriptor as PmRestartReviewTokenDescriptor, Sup4PmRestartOracleDescriptor,
    Sup4PmRestartOracleReplyDescriptor, accepted_reply, decode_pm_restart_reply_v1,
    decode_pm_restart_request_v1, encode_pm_restart_reply_v1, encode_pm_restart_request_v1,
    oracle_from_reply, oracle_from_request, reply_from_sup4_oracle, request_from_sup4_oracle,
};
