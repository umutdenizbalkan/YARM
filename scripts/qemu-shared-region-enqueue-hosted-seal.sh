#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198E2B — QUEUED SHARED-REGION ENQUEUE HOSTED SEAL.
#
# Proves the QUEUED (no-waiter enqueue → receiver-side dequeue) shared-region cap-transfer reuses
# the SAME origin-neutral post-lock transaction executor as the direct path (`shared_region_execute`)
# with identical classification, rights, mapping, single-rollback, lifecycle, and wake semantics —
# only the `origin_direct=false` proof marker differs. Covers atomic message+envelope dequeue
# ownership (exactly-once consume; no dangling entry; no double consume), the full lifecycle /
# endpoint-teardown matrix, partial-map + copy rollback, and generation-bearing TID-reuse safety.
# Hosted-only, no QEMU. Each case asserts its own invariant, so leaked_messages / leaked_envelopes /
# leaked_caps / leaked_mappings / leaked_transactions / leaked_pins / duplicate_wakes are 0 by
# construction (the seal fails unless every case passes).
#
# Emits on success:
#   SECOND_COHORT_SHARED_REGION_ENQUEUE_HOSTED_SEAL cases=<n> leaked_messages=0 leaked_envelopes=0 \
#       leaked_caps=0 leaked_mappings=0 leaked_transactions=0 leaked_pins=0 duplicate_wakes=0 result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

note() { echo "[shared-region-enqueue-seal] $*"; }

CASES=(
  queued_memoryobject_classified_and_publishes
  queued_dmaregion_classified_and_publishes
  queued_non_shared_object_rejected_at_dequeue
  queued_reply_object_excluded_at_dequeue
  queued_message_and_envelope_consumed_exactly_once
  queued_pin_transfers_without_reference_gap
  queued_read_intent_drops_write_right
  queued_write_intent_without_write_right_rejected
  queued_map_right_missing_rejected
  queued_write_mapping_is_nx
  queued_dmaregion_bounds_enforced
  queued_two_receivers_cannot_consume_same_envelope
  queued_envelope_consumed_message_not_replayable
  queued_stale_handle_dequeue_leaves_no_dangling
  queued_publish_yields_one_cap_one_active_mapping
  queued_map_fault_rolls_back_prefix
  queued_copy_fault_rolls_back
  queued_source_exit_before_dequeue_purges_message_envelope_pin
  queued_receiver_exit_before_dequeue_removes_association
  queued_receiver_exit_after_snapshot_cancels
  queued_endpoint_teardown_before_dequeue_reclaims_together
  queued_endpoint_teardown_after_snapshot_prevents_publication
  queued_reused_tid_new_asid_cannot_consume_or_publish
)
expected="${#CASES[@]}"

log="${TMPDIR:-/tmp}/shared-region-enqueue-seal.$$.log"
note "running ${expected} queued shared-region enqueue cases (hosted, no QEMU)"
# Multiple test-name filters go to libtest (after `--`); cargo takes one positional.
if ! cargo test --features hosted-dev --lib -- --test-threads=1 "${CASES[@]}" >"$log" 2>&1; then
  echo "[shared-region-enqueue-seal][fail] cases did not all pass:"
  grep -E "FAILED|error\[|panicked|test result" "$log" | head
  echo "SECOND_COHORT_SHARED_REGION_ENQUEUE_HOSTED_SEAL cases=${expected} result=fail"
  rm -f "$log"
  exit 1
fi
passed="$(grep -oE 'result: ok\. [0-9]+ passed' "$log" | grep -oE '[0-9]+' | head -1)"
rm -f "$log"
if [[ "${passed:-0}" -ne "$expected" ]]; then
  echo "[shared-region-enqueue-seal][fail] expected ${expected} passing cases, got ${passed:-0}"
  echo "SECOND_COHORT_SHARED_REGION_ENQUEUE_HOSTED_SEAL cases=${expected} passed=${passed:-0} result=fail"
  exit 1
fi
note "${passed}/${expected} queued shared-region enqueue cases passed"
echo "SECOND_COHORT_SHARED_REGION_ENQUEUE_HOSTED_SEAL cases=${passed} leaked_messages=0 leaked_envelopes=0 leaked_caps=0 leaked_mappings=0 leaked_transactions=0 leaked_pins=0 duplicate_wakes=0 result=ok"
