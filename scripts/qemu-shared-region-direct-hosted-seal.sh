#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198E2A — SHARED-REGION DIRECT post-lock HOSTED SEAL.
#
# Proves the architecture-neutral post-global-lock shared-region DIRECT delivery transaction
# (Phase-A snapshot + post-lock executor + single idempotent rollback) through hosted
# production-path tests. No QEMU, no live architecture class, no arch retirement gate. Each case
# asserts its own invariant, so leaked_caps / leaked_mappings / leaked_transactions /
# duplicate_wakes are 0 by construction (the seal fails unless every case passes).
#
# Emits on success:
#   SECOND_COHORT_SHARED_REGION_DIRECT_HOSTED_SEAL cases=<n> leaked_caps=0 leaked_mappings=0 \
#       leaked_transactions=0 duplicate_wakes=0 result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

note() { echo "[shared-region-direct-seal] $*"; }

CASES=(
  memoryobject_snapshot_captures_authoritative_identity
  dmaregion_offset_length_preserved
  envelope_pin_transfers_into_snapshot
  source_exit_after_snapshot_does_not_invalidate
  receiver_exits_before_cap_mint
  receiver_generation_replacement_blocks_publish
  receiver_cnode_full_shared_direct
  missing_map_right_rejected
  write_request_without_write_right_rejected
  map_failure_rolls_back
  metadata_copy_failure_rolls_back
  successful_delivery_one_cap_one_mapping_one_wake
  rollback_is_idempotent
  stale_cleanup_token_cannot_release_replacement
  reply_and_ordinary_paths_excluded_from_shared_txn
  stale_after_map_before_publish_rolls_back_exactly_once
  mapping_execute_permission_never_enabled
  bad_region_rejected
)
expected="${#CASES[@]}"

log="${TMPDIR:-/tmp}/shared-region-direct-hosted-seal.$$.log"
note "running ${expected} shared-region DIRECT post-lock cases (hosted, no QEMU)"
# Multiple test-name filters go to libtest (after `--`); cargo takes one positional.
if ! cargo test --features hosted-dev --lib -- --test-threads=1 "${CASES[@]}" >"$log" 2>&1; then
  echo "[shared-region-direct-seal][fail] cases did not all pass:"
  grep -E "FAILED|error\[|panicked|test result" "$log" | head
  echo "SECOND_COHORT_SHARED_REGION_DIRECT_HOSTED_SEAL cases=${expected} result=fail"
  rm -f "$log"
  exit 1
fi
passed="$(grep -oE 'result: ok\. [0-9]+ passed' "$log" | grep -oE '[0-9]+' | head -1)"
rm -f "$log"
if [[ "${passed:-0}" -ne "$expected" ]]; then
  echo "[shared-region-direct-seal][fail] expected ${expected} passing cases, got ${passed:-0}"
  echo "SECOND_COHORT_SHARED_REGION_DIRECT_HOSTED_SEAL cases=${expected} passed=${passed:-0} result=fail"
  exit 1
fi
note "${passed}/${expected} shared-region DIRECT cases passed"
echo "SECOND_COHORT_SHARED_REGION_DIRECT_HOSTED_SEAL cases=${passed} leaked_caps=0 leaked_mappings=0 leaked_transactions=0 duplicate_wakes=0 result=ok"
