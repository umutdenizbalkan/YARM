#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198B1 Part D — SECOND-COHORT ORDINARY-CAP NEGATIVE + ROLLBACK SEAL.
#
# Proves the ordinary-cap IpcSend delivery path fails CLOSED: every negative /
# fault-injection case rolls back cleanly with no receiver wake, no partial
# payload/metadata visible, no receiver-cap leak, no transfer-envelope leak, no
# duplicate waiter/queue entry, and no ordinary/reply misclassification. The
# cases are hosted fault-injection tests that drive the ACTUAL production
# transaction/finalization helpers:
#
#   DIRECT (blocked-waiter) path — produce_blocked_waiter_ordinary_cap_delivery
#   + SharedKernel::drain_dispatch_post_work (real producer + executor seam):
#     - cap materialization failure           -> mint rolled back, source
#                                                cap_refcount restored (no leak)
#     - invalid receiver payload destination  -> Phase-A rejection, no stash,
#                                                envelope retained (retryable)
#     - invalid receiver metadata destination -> Phase-A rejection, no stash,
#                                                nothing minted (no cap leak)
#     - reply-cap message                     -> NOT produced (no ordinary/reply
#                                                misclassification)
#     - plain / shared-region message         -> NOT produced (class guard)
#     - no trap drainer                        -> NOT produced (stash discipline)
#     - missing/consumed source envelope       -> synchronous error, no stash
#     - executor seam + rollback structure     -> copy-fault rolls back the mint
#
#   ENQUEUE (recv-boundary) path — complete_recv_boundary_ordinary_cap seam:
#     - phony/dead source cap on the enqueue transfer -> materialization fails
#     - queued mem-object cap transfer  -> routed through the boundary seam
#     - queued endpoint cap transfer    -> routed through the boundary seam
#
# leaked_caps / leaked_envelopes / duplicate_wakes are 0 by CONSTRUCTION: each of
# the above asserts the corresponding invariant (refcount restored, no stash, no
# duplicate wake) and the seal fails unless every one passes. This seal does NOT
# increase CNode or queue capacity and exercises NO reply-cap / shared-region /
# D2 retirement path.
#
# Emits, on success:
#   SECOND_COHORT_ORDINARY_CAP_NEGATIVE_SEAL direct_cases=<n> enqueue_cases=<m> \
#       leaked_caps=0 leaked_envelopes=0 duplicate_wakes=0 result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

note() { echo "[neg-seal] $*"; }

DIRECT_TESTS=(
  stage188c_materialize_failure_rolls_back_no_leak
  stage188c_invalid_receiver_payload_dest_rejected_no_stash
  stage188c_invalid_receiver_meta_dest_rejected_no_stash_no_mint
  stage188c_reply_cap_not_produced
  stage188c_plain_not_produced
  stage188c_shared_region_not_produced
  stage188c_no_drainer_no_produce
  stage188c_missing_envelope_synchronous_error_no_stash
  stage188c_executor_uses_seams_and_rollback
)
ENQUEUE_TESTS=(
  stage46_direct_enqueue_phony_cap_transfer_fails_materialization
  stage187b_queued_mem_cap_transfer_uses_boundary_seam
  stage187b_queued_endpoint_cap_transfer_uses_boundary_seam
)

# run_group writes the passing-case count into the global GROUP_PASSED and
# returns non-zero (with diagnostics on stdout) on any failure. It deliberately
# does NOT use command substitution so cargo diagnostics reach the terminal.
GROUP_PASSED=0
run_group() { # <label> <expected_count> <test names...>
  local label="$1"; shift
  local expected="$1"; shift
  local log="${TMPDIR:-/tmp}/neg-seal-${label}.$$.log"
  note "running ${label} group (${expected} cases)"
  # Multiple test-name filters must be passed to libtest (after `--`); cargo
  # only accepts a single positional TESTNAME. libtest OR-matches the filters.
  if ! cargo test --features hosted-dev --lib -- --test-threads=1 "$@" >"$log" 2>&1; then
    echo "[neg-seal][fail] ${label} group did not pass:"
    grep -E "FAILED|error\[|panicked|test result" "$log" | head
    return 1
  fi
  GROUP_PASSED="$(grep -oE 'result: ok\. [0-9]+ passed' "$log" | grep -oE '[0-9]+' | head -1)"
  if [[ "${GROUP_PASSED:-0}" -ne "$expected" ]]; then
    echo "[neg-seal][fail] ${label}: expected ${expected} passing cases, got ${GROUP_PASSED}"
    grep -E "test result" "$log" | head
    return 1
  fi
  note "${label} group: ${GROUP_PASSED}/${expected} cases passed"
}

run_group direct "${#DIRECT_TESTS[@]}" "${DIRECT_TESTS[@]}" || exit 1
direct_pass="$GROUP_PASSED"
run_group enqueue "${#ENQUEUE_TESTS[@]}" "${ENQUEUE_TESTS[@]}" || exit 1
enqueue_pass="$GROUP_PASSED"

echo "SECOND_COHORT_ORDINARY_CAP_NEGATIVE_SEAL direct_cases=${direct_pass} enqueue_cases=${enqueue_pass} leaked_caps=0 leaked_envelopes=0 duplicate_wakes=0 result=ok"
