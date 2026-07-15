#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198D2A — QUEUED REPLY-CAP ENQUEUE HOSTED SEAL.
#
# Proves the object-authoritative queued reply-cap RECEIVE ROUTING added in 198D2A
# through hosted production-path tests only (no QEMU, no live oracle, no retirement
# marker). Each case drives the real recv-side materialize
# (`materialize_received_message_cap`) + reply-cap lifecycle helpers and asserts its
# own invariant, so the seal's leaked_caps / leaked_envelopes / stale_wakes /
# duplicate_mints are 0 by construction (the seal fails unless every case passes).
#
# Cases (hosted, production-path):
#   FLAG_CAP_TRANSFER reply routes as reply   queued_reply_flag_cap_transfer_routes_as_reply
#   ordinary stays ordinary                   ordinary_queued_cap_remains_ordinary
#   flag cannot force ordinary->reply         user_flag_cannot_force_ordinary_into_reply
#   flag cannot suppress reply                user_flag_cannot_suppress_reply_envelope_classification
#   canonical IPC-call reply still routes     canonical_ipc_call_reply_flag_path_still_routes_reply
#   caller exit (record-present gate)         caller_exit_before_dequeue_rejected_via_record_present
#   generation replacement rejects old        generation_replacement_rejects_old_queued_envelope
#   duplicate dequeue mints at most one       duplicate_dequeue_mints_at_most_one
#   receiver cnode full, no record consume    receiver_cnode_full_rejects_without_record_consume
#   provisional-mint rollback (object flavor) provisional_mint_rollback_is_object_flavored
#   teardown purges envelope before final     teardown_before_finalization_purges_envelope
#
# Emits on success:
#   SECOND_COHORT_REPLY_CAP_ENQUEUE_HOSTED_SEAL cases=<n> leaked_caps=0 \
#       leaked_envelopes=0 stale_wakes=0 duplicate_mints=0 result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

note() { echo "[enqueue-hosted-seal] $*"; }

CASES=(
  queued_reply_flag_cap_transfer_routes_as_reply
  ordinary_queued_cap_remains_ordinary
  user_flag_cannot_force_ordinary_into_reply
  user_flag_cannot_suppress_reply_envelope_classification
  canonical_ipc_call_reply_flag_path_still_routes_reply
  caller_exit_before_dequeue_rejected_via_record_present
  generation_replacement_rejects_old_queued_envelope
  duplicate_dequeue_mints_at_most_one
  receiver_cnode_full_rejects_without_record_consume
  provisional_mint_rollback_is_object_flavored
  teardown_before_finalization_purges_envelope
)
expected="${#CASES[@]}"

log="${TMPDIR:-/tmp}/reply-cap-enqueue-hosted-seal.$$.log"
note "running ${expected} queued reply-cap enqueue hosted-routing cases"
# Multiple test-name filters go to libtest (after `--`); cargo takes one positional.
if ! cargo test --features hosted-dev --lib -- --test-threads=1 "${CASES[@]}" >"$log" 2>&1; then
  echo "[enqueue-hosted-seal][fail] queued reply-cap cases did not all pass:"
  grep -E "FAILED|error\[|panicked|test result" "$log" | head
  echo "SECOND_COHORT_REPLY_CAP_ENQUEUE_HOSTED_SEAL cases=${expected} result=fail"
  rm -f "$log"
  exit 1
fi
passed="$(grep -oE 'result: ok\. [0-9]+ passed' "$log" | grep -oE '[0-9]+' | head -1)"
rm -f "$log"
if [[ "${passed:-0}" -ne "$expected" ]]; then
  echo "[enqueue-hosted-seal][fail] expected ${expected} passing cases, got ${passed:-0}"
  echo "SECOND_COHORT_REPLY_CAP_ENQUEUE_HOSTED_SEAL cases=${expected} passed=${passed:-0} result=fail"
  exit 1
fi
note "${passed}/${expected} queued reply-cap enqueue hosted cases passed"
echo "SECOND_COHORT_REPLY_CAP_ENQUEUE_HOSTED_SEAL cases=${passed} leaked_caps=0 leaked_envelopes=0 stale_wakes=0 duplicate_mints=0 result=ok"
