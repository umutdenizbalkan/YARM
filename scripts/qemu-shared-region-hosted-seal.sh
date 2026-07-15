#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198E1 — SHARED-REGION IpcSend HOSTED AUDIT SEAL.
#
# Hosted-only AUDIT seal (NOT a live retirement seal, no QEMU). Proves the shared-region IpcSend
# path's classification, rights/attenuation, transfer/mapping lifecycle, and teardown reclamation
# through production-path hosted tests. Composes the new stage198e1 cases with the existing
# stage54_* mapping-plan and stage56_*/stage60_* cleanup-token / rollback cases. Each case asserts
# its own invariant, so leaked_caps / leaked_mappings / leaked_envelopes / stale_releases are 0 by
# construction (the seal fails unless every case passes).
#
# Verdict (from doc/STAGE_198E1_SHARED_REGION_AUDIT.md):
#   IpcSendSharedRegionDirect  = NEEDS_BOUNDED_FIX  -> direct=blocked
#   IpcSendSharedRegionEnqueue = NEEDS_BOUNDED_FIX  -> enqueue=blocked
# (the map + TLB shootdown + user-copy still run under the broad lock; the post-lock map/copy
# boundary snapshot does not yet exist for shared-region).
#
# Emits on success:
#   SECOND_COHORT_SHARED_REGION_HOSTED_SEAL direct=blocked enqueue=blocked cases=<n> \
#       leaked_caps=0 leaked_mappings=0 leaked_envelopes=0 stale_releases=0 result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

note() { echo "[shared-region-seal] $*"; }

CASES=(
  # ── Stage 198E1 shared-region-specific classification + lifecycle ─────────────
  shared_region_object_types_are_memoryobject_and_dmaregion
  memoryobject_transfer_materializes_fresh_cap_preserving_identity
  reply_object_excluded_from_shared_region_transfer
  ordinary_cap_cannot_force_mapping_without_authority
  shared_region_envelope_pins_and_teardown_reclaims
  shared_region_transfer_envelope_is_one_shot
  invalid_stale_source_handle_rejected
  receiver_cnode_full_rollback_for_shared_transfer
  active_mapping_keyed_by_owner_and_generation_cap
  receiver_teardown_purges_active_mappings
  full_teardown_leaves_no_leak
  # ── Existing mapping-plan WRITE/MAP gates + attenuation ───────────────────────
  stage54_mapping_plan_read_only_for_map_read_intent
  stage54_mapping_plan_read_write_for_map_readwrite_intent
  stage54_mapping_plan_insufficient_rights_when_write_requested_but_cap_read_only
  stage54_mapping_plan_insufficient_rights_when_map_bit_missing
  # ── Existing cleanup-token stale/duplicate + writeback/mapping rollback ───────
  stage56_stale_token_after_realloc_gives_stale_generation
  stage56_duplicate_release_gives_already_released
  stage60_output_writeback_fail_rolls_back_mapping
)
expected="${#CASES[@]}"

log="${TMPDIR:-/tmp}/shared-region-hosted-seal.$$.log"
note "running ${expected} shared-region audit cases (hosted, no QEMU)"
# Multiple test-name filters go to libtest (after `--`); cargo takes one positional.
if ! cargo test --features hosted-dev --lib -- --test-threads=1 "${CASES[@]}" >"$log" 2>&1; then
  echo "[shared-region-seal][fail] shared-region audit cases did not all pass:"
  grep -E "FAILED|error\[|panicked|test result" "$log" | head
  echo "SECOND_COHORT_SHARED_REGION_HOSTED_SEAL direct=blocked enqueue=blocked cases=${expected} result=fail"
  rm -f "$log"
  exit 1
fi
passed="$(grep -oE 'result: ok\. [0-9]+ passed' "$log" | grep -oE '[0-9]+' | head -1)"
rm -f "$log"
if [[ "${passed:-0}" -ne "$expected" ]]; then
  echo "[shared-region-seal][fail] expected ${expected} passing cases, got ${passed:-0}"
  echo "SECOND_COHORT_SHARED_REGION_HOSTED_SEAL direct=blocked enqueue=blocked cases=${expected} passed=${passed:-0} result=fail"
  exit 1
fi
note "${passed}/${expected} shared-region audit cases passed"
echo "SECOND_COHORT_SHARED_REGION_HOSTED_SEAL direct=blocked enqueue=blocked cases=${passed} leaked_caps=0 leaked_mappings=0 leaked_envelopes=0 stale_releases=0 result=ok"
