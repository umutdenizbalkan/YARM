#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198E3B2B — SHARED-REGION DRAIN SWITCHED TO OFF-LOCK EXECUTION SEAL.
#
# Proves the REAL post-work drain (`drain_dispatch_post_work` → the switched
# `execute_blocked_waiter_shared_region_delivery`) runs the shared-region transaction end-to-end
# through `SharedRegionOffLockCtx` — no broad `&mut KernelState`, no `with(...)`/`with_cpu(...)`
# re-entry, no `shared_region_execute`. Each case asserts its own invariant so the seal's zero-counts
# hold by construction:
#   - the finalize order (blocked-return regs + endpoint waiter slot cleared BEFORE the single wake),
#   - stale/replacement/missing-waiter → rollback with ZERO wake (receiver stays Blocked),
#   - copy/map/ASID-replacement failures → rollback (receiver stays Blocked, nothing leaked),
#   - exactly ONE scheduler enqueue on success and NO duplicate wake on a repeated drain,
#   - the post-work stash slot cleared on success,
#   - retirement/attestation markers emitted ONLY in the success arm (never from the rolled-back arm),
#   - producers dormant off the oracle knob.
# Hosted-only, no QEMU.
#
# Emits on success:
#   SECOND_COHORT_SHARED_REGION_OFFLOCK_SEAL broad_kernel_borrows=0 user_copy_under_lock=0 \
#       tlb_wait_under_lock=0 waiter_clear_after_wake=0 leaked_caps=0 leaked_mappings=0 \
#       leaked_transactions=0 duplicate_wakes=0 result=ok
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
)
expected="${#CASES[@]}"

log="${TMPDIR:-/tmp}/shared-region-offlock-seal.$$.log"
note "running ${expected} off-lock drain cases (hosted, no QEMU)"
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
note "${passed}/${expected} off-lock drain cases passed"
echo "SECOND_COHORT_SHARED_REGION_OFFLOCK_SEAL broad_kernel_borrows=0 user_copy_under_lock=0 tlb_wait_under_lock=0 waiter_clear_after_wake=0 leaked_caps=0 leaked_mappings=0 leaked_transactions=0 duplicate_wakes=0 result=ok"
