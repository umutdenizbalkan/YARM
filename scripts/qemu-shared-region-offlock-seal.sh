#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198E3B2B / 198E3B2B1 / 198E3B2B2 — SHARED-REGION OFF-LOCK DRAIN + ATOMIC, GENERATION-BEARING
# IDENTITY FINALIZATION SEAL.
#
# Proves the REAL post-work drain (`drain_dispatch_post_work` → the switched
# `execute_blocked_waiter_shared_region_delivery`) runs the shared-region transaction end-to-end
# through `SharedRegionOffLockCtx` — no broad `&mut KernelState`, no `with(...)`/`with_cpu(...)`
# re-entry — AND that blocked-receiver finalization is the CLAIM-THEN-COMMIT protocol over a COMPLETE
# generation-bearing endpoint-waiter identity (`ReceiverWaiterIdentity { tid, asid }`):
#   Phase 1 prevalidate (rank 2, NO mutation)
#   Phase 2 exact IDENTITY + GENERATION waiter claim (rank 3, remove once) — numeric TID alone is never
#           the authority, so a replacement task (reused TID, new ASID) can never be claimed/cleared
#   Phase 3 commit (rank 2, clear registers + Runnable ONLY for a still-live identity match; a dead or
#           replaced receiver leaves the claimed waiter stale and is NEVER restored)
#   Phase 4 enqueue (rank 1, once, non-fallible, last visible action)
# Each case asserts its own invariant, so the seal's counters hold by construction (it fails unless
# every case passes):
#   - registers are NEVER cleared before an exact waiter claim (register_clear_before_claim=0),
#   - the endpoint GENERATION is checked during the claim, and a same-numeric-TID/different-ASID or a
#     destroyed/recreated endpoint is rejected (waiter_claim_generation_safe=1),
#   - no endpoint-waiter authority is granted by numeric TID alone
#     (numeric_tid_only_waiter_authority=0),
#   - a claimed waiter belonging to a vanished/replaced incarnation is never re-installed
#     (stale_waiter_restores=0),
#   - a replacement waiter is never cleared, and no live blocked task is left without a waiter
#     (live_task_without_waiter=0),
#   - exactly one enqueue on success, none on any failure (duplicate_wakes=0).
# Hosted-only, no QEMU.
#
# Emits on success:
#   SECOND_COHORT_SHARED_REGION_OFFLOCK_SEAL broad_kernel_borrows=0 waiter_claim_generation_safe=1 \
#       numeric_tid_only_waiter_authority=0 stale_waiter_restores=0 register_clear_before_claim=0 \
#       live_task_without_waiter=0 duplicate_wakes=0 result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

note() { echo "[shared-region-offlock-seal] $*"; }

CASES=(
  # the switched drain, driven end-to-end through SharedRegionOffLockCtx
  direct_success_clears_then_wakes
  replacement_waiter_rolls_back_zero_wake
  missing_waiter_rolls_back_zero_wake
  copy_and_map_failure_leave_receiver_blocked
  receiver_asid_replacement_rolls_back
  repeated_drain_no_duplicate_wake
  enqueue_origin_succeeds_through_offlock
  markers_success_only_and_producers_dormant
  # source contract: the drain executes through the off-lock context (no broad borrow)
  offlock_ctx_source_contracts
  # Stage 198E3B2B1 — atomic claim-then-commit finalization interleavings
  generation_change_between_prevalidate_and_claim
  endpoint_recreated_at_same_index_rejects_claim
  waiter_replaced_by_other_tid_rejects_claim
  same_numeric_tid_different_asid_is_not_authority
  missing_waiter_rejects_claim
  failed_claim_leaves_registers_byte_identical
  failed_claim_leaves_valid_waiter_untouched
  successful_claim_removes_exactly_one_waiter
  receiver_exit_after_claim_is_gone_dead_no_restore
  receiver_asid_change_after_claim_no_restore
  replaced_restore_never_strands_never_clobbers
  failed_finalization_preserves_registers_end_to_end
  success_clears_regs_runnable_one_enqueue
  repeated_finalization_cannot_enqueue_twice
  markers_stay_success_only_no_publish_on_stale
  producers_remain_dormant_off_knob
  # Stage 198E3B2B2 — generation-bearing endpoint waiter identity
  publication_stores_full_identity
  claim_requires_full_identity_and_generation
  replacement_publishes_and_is_not_removable_by_stale_identity
  identity_keyed_cleanup_removes_only_matching
  no_numeric_tid_only_waiter_comparison_in_production
)
expected="${#CASES[@]}"

log="${TMPDIR:-/tmp}/shared-region-offlock-seal.$$.log"
note "running ${expected} off-lock drain + atomic-finalization cases (hosted, no QEMU)"
if ! cargo test --features hosted-dev --lib -- --test-threads=1 "${CASES[@]}" >"$log" 2>&1; then
  echo "[shared-region-offlock-seal][fail] cases did not all pass:"
  grep -E "FAILED|error\[|panicked|test result" "$log" | head
  echo "SECOND_COHORT_SHARED_REGION_OFFLOCK_SEAL cases=${expected} result=fail"
  rm -f "$log"
  exit 1
fi
passed="$(grep -oE 'result: ok\. [0-9]+ passed' "$log" | grep -oE '[0-9]+' | head -1)"
rm -f "$log"
if [[ "${passed:-0}" -ne "$expected" ]]; then
  echo "[shared-region-offlock-seal][fail] expected ${expected} passing cases, got ${passed:-0}"
  echo "SECOND_COHORT_SHARED_REGION_OFFLOCK_SEAL cases=${expected} passed=${passed:-0} result=fail"
  exit 1
fi
note "${passed}/${expected} off-lock drain + atomic-finalization cases passed"
echo "SECOND_COHORT_SHARED_REGION_OFFLOCK_SEAL broad_kernel_borrows=0 waiter_claim_generation_safe=1 numeric_tid_only_waiter_authority=0 stale_waiter_restores=0 register_clear_before_claim=0 live_task_without_waiter=0 duplicate_wakes=0 result=ok"
