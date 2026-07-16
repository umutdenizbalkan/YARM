#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198E2A1 — SHARED-REGION TRANSACTION CANCELLATION + PARTIAL-MAP RACE SEAL.
#
# Proves the executor-owned (protocol A) single-cleanup-owner contract for the post-lock
# shared-region DIRECT transaction: exactly one cleanup owner under receiver-exit, cancellation, and
# partial multi-page mapping races; the mapped prefix is unmapped exactly; no page/writeback/wake
# after cancellation; generation-bearing teardown matching. Hosted-only, no QEMU. Each case asserts
# its own invariant, so orphan_pages / duplicate_unmaps / duplicate_revokes / duplicate_pin_releases
# / stale_publications are 0 by construction (the seal fails unless every case passes).
#
# Emits on success:
#   SECOND_COHORT_SHARED_REGION_TXN_RACE_SEAL cases=<n> orphan_pages=0 duplicate_unmaps=0 \
#       duplicate_revokes=0 duplicate_pin_releases=0 stale_publications=0 result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

note() { echo "[shared-region-race-seal] $*"; }

CASES=(
  cancel_before_first_page
  cancel_after_first_page_multipage
  cancel_between_later_pages
  page_n_failure_rolls_back_prefix
  teardown_request_cancel_is_honored_by_executor
  executor_publishes_before_teardown_request
  cleanup_is_single_owner_and_idempotent
  no_map_writeback_or_wake_after_cancellation
  delayed_old_tid_teardown_does_not_affect_replacement_asid
  stale_executor_cannot_publish_after_registry_removal
  successful_multipage_publishes_once
  rollback_idempotent_across_states
)
expected="${#CASES[@]}"

log="${TMPDIR:-/tmp}/shared-region-txn-race-seal.$$.log"
note "running ${expected} shared-region transaction race cases (hosted, no QEMU)"
# Multiple test-name filters go to libtest (after `--`); cargo takes one positional.
if ! cargo test --features hosted-dev --lib -- --test-threads=1 "${CASES[@]}" >"$log" 2>&1; then
  echo "[shared-region-race-seal][fail] cases did not all pass:"
  grep -E "FAILED|error\[|panicked|test result" "$log" | head
  echo "SECOND_COHORT_SHARED_REGION_TXN_RACE_SEAL cases=${expected} result=fail"
  rm -f "$log"
  exit 1
fi
passed="$(grep -oE 'result: ok\. [0-9]+ passed' "$log" | grep -oE '[0-9]+' | head -1)"
rm -f "$log"
if [[ "${passed:-0}" -ne "$expected" ]]; then
  echo "[shared-region-race-seal][fail] expected ${expected} passing cases, got ${passed:-0}"
  echo "SECOND_COHORT_SHARED_REGION_TXN_RACE_SEAL cases=${expected} passed=${passed:-0} result=fail"
  exit 1
fi
note "${passed}/${expected} shared-region transaction race cases passed"
echo "SECOND_COHORT_SHARED_REGION_TXN_RACE_SEAL cases=${passed} orphan_pages=0 duplicate_unmaps=0 duplicate_revokes=0 duplicate_pin_releases=0 stale_publications=0 result=ok"
