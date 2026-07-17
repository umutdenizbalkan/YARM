#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198E3B2A — SHARED-REGION OFF-LOCK SEAM SEAL.
#
# Proves the full off-lock seam foundation + SharedRegionOffLockCtx: every bounded SharedKernel split
# seam is behaviorally identical to its broad-borrow reference sibling, and driving the accepted
# `run_shared_region_txn` through the OFF-LOCK context produces the SAME publish/rollback outcomes as
# the reference path — with no broad KernelState borrow, no cached domain pointer, no nested VM/memory
# or capability/memory locks, no TLB wait or reclaim under a lock, and the user copy off every lock.
# Hosted-only, no QEMU. Each case asserts its own invariant, so the seal's zero-counts hold by
# construction (it fails unless every case passes).
#
# Emits on success:
#   SECOND_COHORT_SHARED_REGION_SEAM_SEAL broad_kernel_borrows=0 cached_domain_pointers=0 \
#       vm_memory_nested_locks=0 cap_memory_nested_locks=0 reclaim_before_shootdown=0 \
#       user_copy_under_lock=0 equivalence_failures=0 result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

note() { echo "[shared-region-seam-seal] $*"; }

CASES=(
  # substrate equivalence (rank 2/3/6 _locked helpers + raw receiver read)
  pin_refcount_locked_equivalent
  map_refcount_locked_equivalent
  phys_base_locked_correct
  active_mapping_locked_equivalent
  consume_cancel_locked_mutates_only_intended
  receiver_alive_raw_read_generation_bearing
  locked_seam_helpers_take_subsystem_not_kernelstate
  # off-lock context end-to-end equivalence + safety
  offlock_success_single_and_multi_page
  offlock_map_fault_rolls_back_exact_prefix
  offlock_copy_fault_rolls_back
  offlock_receiver_asid_replacement_rejected
  offlock_cap_mint_failure_zero_wake
  offlock_cancel_overflow_fuse_authoritative
  offlock_ctx_source_contracts
)
expected="${#CASES[@]}"

log="${TMPDIR:-/tmp}/shared-region-seam-seal.$$.log"
note "running ${expected} off-lock seam cases (hosted, no QEMU)"
if ! cargo test --features hosted-dev --lib -- --test-threads=1 "${CASES[@]}" >"$log" 2>&1; then
  echo "[shared-region-seam-seal][fail] cases did not all pass:"
  grep -E "FAILED|error\[|panicked|test result" "$log" | head
  echo "SECOND_COHORT_SHARED_REGION_SEAM_SEAL cases=${expected} result=fail"
  rm -f "$log"
  exit 1
fi
passed="$(grep -oE 'result: ok\. [0-9]+ passed' "$log" | grep -oE '[0-9]+' | head -1)"
rm -f "$log"
if [[ "${passed:-0}" -ne "$expected" ]]; then
  echo "[shared-region-seam-seal][fail] expected ${expected} passing cases, got ${passed:-0}"
  echo "SECOND_COHORT_SHARED_REGION_SEAM_SEAL cases=${expected} passed=${passed:-0} result=fail"
  exit 1
fi
note "${passed}/${expected} off-lock seam cases passed"
echo "SECOND_COHORT_SHARED_REGION_SEAM_SEAL broad_kernel_borrows=0 cached_domain_pointers=0 vm_memory_nested_locks=0 cap_memory_nested_locks=0 reclaim_before_shootdown=0 user_copy_under_lock=0 equivalence_failures=0 result=ok"
