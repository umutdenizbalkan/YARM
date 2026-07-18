#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198E3D-S — SHARED-REGION DIRECT 3/3 COMBINED SEAL (serialized master).
#
# Runs one FRESH, focused `QEMU_SMP=1` DIRECT shared-region oracle boot per
# architecture, SERIALLY, by invoking the already-accepted per-architecture smoke
# scripts (which each rebuild fail-closed artifacts + boot + parse in detail — this
# runner does NOT duplicate their parsers). It captures each per-arch seal from
# THIS run's captured stdout (a unique RUN_ID log dir), so an individual seal from
# an OLD log can never satisfy the combined run.
#
# Per architecture the sub-smoke requires (see the per-arch script):
#   IPCSEND_SHARED_REGION_OBJECT_OK / MAP_OK / LIFECYCLE_OK  arch=<a> class=direct (each x1)
#   GLOBAL_LOCK_RETIRE_CLASS_BEGIN/DONE class=IpcSendSharedRegionDirect result=ok  (x1)
#   <ARCH>_SHARED_REGION_DIRECT_LIVE_ORACLE_DONE ... result=ok                     (x1)
#   DISPATCH_POST_WORK_DONE kind=blocked_waiter_shared_region result=ok            (x1)
#   mapped_pages=2 fresh_cap=1 readonly=1 first_release=ok second_release=rejected
#   wakes=1 continuations=1, exactly one successful send, no fuse/enqueue/leak.
#
# Each arch uses its ESTABLISHED target-specific transfer window + release decoder:
#   x86_64  VA=0x4000_0000  separate-error-register (RCX) release decode
#   aarch64 VA=0x2000_0000  a0/x0 value-or-error (page-aligned) release decode
#   riscv64 VA=0x2000_0000  a0 value-or-error (page-aligned) release decode
#
# The aggregate seal is emitted ONLY after all three fresh per-arch seals pass in
# THIS serialized run:
#   SECOND_COHORT_SHARED_REGION_DIRECT_MATRIX_SEAL arches=3 classes=1 live_cells=3 \
#     fuse_trips=0 duplicate_wakes=0 result=ok
#
# IpcSendSharedRegionEnqueue is NOT part of this matrix (unsupported for production
# retirement); any enqueue evidence in any arch fails the run.
set -uo pipefail
cd "$(dirname "$0")/.."

RUN_ID="${RUN_ID:-$(date +%s)-$$}"
LOGROOT="${LOGROOT:-/tmp/shared-region-direct-matrix-seal/${RUN_ID}}"
mkdir -p "$LOGROOT"
note() { echo "[sr-direct-matrix-seal] $*"; }

overall=0
seals_ok=0
fuse_total=0
dupwake_total=0

# run_cell <arch> <smoke-script> <arch-boot-norm-log>
# Runs ONE fresh per-arch smoke serially, captures its stdout in THIS run's dir,
# and accepts the cell only if the per-arch LIVE seal for THAT arch is present in
# THIS run's captured output (never a pre-existing log).
run_cell() {
  local arch="$1" script="$2" bootnorm="$3"
  local log="$LOGROOT/${arch}.log"
  local expect="SECOND_COHORT_SHARED_REGION_DIRECT_LIVE_SEAL arch=${arch} classes=1 live_cells=1 fuse_trips=0 result=ok"
  note "── running ${arch} (serial; fresh build+boot; log=$log) ──"
  local rc=0
  QEMU_SMP=1 bash "$script" >"$log" 2>&1 || rc=$?
  if [[ "$rc" -eq 124 ]]; then
    note "${arch}: TIMEOUT (exit 124)"
    echo "MATRIX_SEAL_CELL arch=${arch} result=timeout"
    overall=1
    return
  fi
  if grep -qaF -- "$expect" "$log"; then
    note "${arch}: OK"
    echo "MATRIX_SEAL_CELL arch=${arch} result=ok"
    seals_ok=$((seals_ok + 1))
  else
    note "${arch}: FAIL (rc=$rc; expected fresh '$expect') — see $log"
    echo "MATRIX_SEAL_CELL arch=${arch} result=fail rc=$rc"
    overall=1
  fi
  # Cross-check the boot log this smoke just produced for aggregate fuse/dup-wake
  # accounting (the per-arch script already fails closed on these; this only sums).
  if [[ -f "$bootnorm" ]]; then
    local f d
    # `grep -c` prints the count and exits 1 on zero matches; the assignment swallows the
    # exit status, and `tr -d` strips any stray newline so the arithmetic never sees "0\n0".
    f=$(grep -acF "SHARED_REGION_CANCEL_FUSE_SET" "$bootnorm" 2>/dev/null | tr -d '[:space:]')
    # A duplicate wake would show a SECOND post-work completion for the same class.
    d=$(grep -acF "DISPATCH_POST_WORK_DONE kind=blocked_waiter_shared_region result=ok" "$bootnorm" 2>/dev/null | tr -d '[:space:]')
    f=${f:-0}; d=${d:-0}
    fuse_total=$((fuse_total + f))
    [[ "$d" -gt 1 ]] && dupwake_total=$((dupwake_total + (d - 1)))
  fi
}

run_cell x86_64  scripts/qemu-shared-region-direct-x86_64-smoke.sh  /tmp/shared-region-direct-x86_64/boot.norm.log
run_cell aarch64 scripts/qemu-shared-region-direct-aarch64-smoke.sh /tmp/shared-region-direct-aarch64/boot.norm.log
run_cell riscv64 scripts/qemu-shared-region-direct-riscv64-smoke.sh /tmp/shared-region-direct-riscv64/boot.norm.log

note "cells passed=${seals_ok}/3 fuse_total=${fuse_total} duplicate_wakes=${dupwake_total}"

# The aggregate seal requires ALL THREE fresh per-arch seals from THIS run, no fuse
# trip, and no duplicate wake. Fewer than three current-run live cells fails closed.
if [[ "$overall" -ne 0 || "$seals_ok" -ne 3 || "$fuse_total" -ne 0 || "$dupwake_total" -ne 0 ]]; then
  echo "SECOND_COHORT_SHARED_REGION_DIRECT_MATRIX_SEAL arches=3 classes=1 live_cells=${seals_ok} fuse_trips=${fuse_total} duplicate_wakes=${dupwake_total} result=fail"
  exit 1
fi

echo "SECOND_COHORT_SHARED_REGION_DIRECT_MATRIX_SEAL arches=3 classes=1 live_cells=3 fuse_trips=0 duplicate_wakes=0 result=ok"
