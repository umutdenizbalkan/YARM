#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198E3 — SHARED-REGION IpcSend LIVE CROSS-ARCHITECTURE SEAL (six cells).
#
# Runs the combined direct+enqueue shared-region live oracle against FRESH fail-closed artifacts on
# x86_64, AArch64 and RISC-V and asserts BOTH supported classes are proven LIVE on all three arches:
#
#   1. IpcSendSharedRegionDirect   — send to an already recv-v2-blocked receiver
#   2. IpcSendSharedRegionEnqueue  — no-waiter enqueue + later recv-v2/RecvSharedV3 dequeue
#
# A cell (arch, class) is "live" ONLY when, in a fresh boot, ALL of these appear from the real
# post-lock transaction completion (origin-gated — ordinary/reply/plain/hosted/fallback never emit
# them):
#
#   IPCSEND_SHARED_REGION_OBJECT_OK    arch=<a> class=<c> object_match=1 fresh_cap=1 pin_transfer=1
#   IPCSEND_SHARED_REGION_MAP_OK       arch=<a> class=<c> map_right=1 write_right_ok=1 nx=1 cleanup_token=1
#   IPCSEND_SHARED_REGION_LIFECYCLE_OK arch=<a> class=<c> transaction_published=1 receiver_wakes=1 leaked_state=0
#   GLOBAL_LOCK_RETIRE_CLASS_BEGIN     arch=<a> class=IpcSendSharedRegion<Direct|Enqueue>
#   GLOBAL_LOCK_RETIRE_CLASS_DONE      arch=<a> class=IpcSendSharedRegion<Direct|Enqueue> result=ok
#
# The fail-closed cancellation fuse must NOT trip during a normal oracle run:
#   SHARED_REGION_CANCEL_FUSE_SET count=0
#
# All 6 cells (3 arches × 2 classes) must be live-proven — there is NO source-guard substitute. On
# success this script emits (from the per-arch logs — no kernel marker fabricates the matrix):
#   SECOND_COHORT_SHARED_REGION_SEAL arches=3 classes=2 live_cells=6 fuse_trips=0 result=ok
#
# NOTE (Stage 198E3 foundation): the userspace shared-region live oracle + its per-arch boot
# provisioning are NOT yet wired (the kernel producers/executor + origin-gated markers + fail-closed
# fuse ARE, gated behind the oracle-proof knob). This runner is the executable seal contract for the
# live-wiring continuation; it fails closed until the oracle boots emit the six cells.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/shared-region-live-seal}
mkdir -p "$LOGDIR"
STRICT=${QEMU_SMOKE_STRICT:-1}
fail=0
ARCHES=(x86_64 aarch64 riscv64)
CLASSES=(direct enqueue)

note() { echo "[shared-region-live-seal] $*"; }
die()  { echo "[shared-region-live-seal][fail] $*"; fail=1; }

# ── 1. Require fresh artifacts ──
for f in build-x86_64/kernel_boot.elf build-aarch64/yarm-aarch64.bin build-riscv64/yarm-riscv64.bin; do
  [[ -f "$f" ]] || { die "missing artifact: $f (build fresh first)"; }
done
(( fail )) && { echo "SECOND_COHORT_SHARED_REGION_SEAL arches=3 classes=2 result=fail reason=missing_artifacts"; exit 1; }

# ── 2. Run the combined direct+enqueue shared-region oracle per arch (one log per arch) ──
run() { # run <logfile> <env=val...> scripts/qemu-ipc-recv-v2-oracle-smoke.sh <arch>
  local log="$1"; shift
  note "run: $* (log=$log)"
  env QEMU_SMOKE_STRICT="$STRICT" "$@" >"$log" 2>&1 || true
}
for arch in "${ARCHES[@]}"; do
  run "$LOGDIR/${arch}.log" YARM_IPC_SEND_SHARED_REGION_ORACLE=1 scripts/qemu-ipc-recv-v2-oracle-smoke.sh "$arch"
done

# ── 3. Reject forbidden markers (boundary/dispatch fail, RISC-V trap fault, fuse trip) ──
FORBIDDEN='DISPATCH_POST_WORK_FAIL kind=blocked_waiter_shared_region|SHARED_REGION_CANCEL_FUSE_SET|RISCV_TRAP_HANDLE_FAILED|RISCV_TRAP_UNHANDLED|reason=trap_from_s_mode|FATAL|!BN'
for log in "$LOGDIR"/*.log; do
  [[ -f "$log" ]] || continue
  if rg -a -n "$FORBIDDEN" "$log" >/dev/null 2>&1; then
    die "forbidden marker in $(basename "$log"): $(rg -a -oN "$FORBIDDEN" "$log" | head -1)"
  fi
done

# ── 4. Per-arch / per-class seal (all attestations + retirement markers LIVE) ──
live_cells=0
seal_cell() { # seal_cell <arch> <class> <RetireClass> <logfile>
  local arch="$1" class="$2" rclass="$3" log="$4"
  [[ -f "$log" ]] || { die "$arch/$class: no log"; return; }
  local ok=1
  rg -a -q "IPCSEND_SHARED_REGION_OBJECT_OK arch=$arch class=$class object_match=1 fresh_cap=1 pin_transfer=1" "$log" || ok=0
  rg -a -q "IPCSEND_SHARED_REGION_MAP_OK arch=$arch class=$class map_right=1 write_right_ok=1 nx=1 cleanup_token=1" "$log" || ok=0
  rg -a -q "IPCSEND_SHARED_REGION_LIFECYCLE_OK arch=$arch class=$class transaction_published=1 receiver_wakes=1 leaked_state=0" "$log" || ok=0
  rg -a -q "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=$arch class=$rclass result=ok" "$log" || ok=0
  if (( ok )); then live_cells=$((live_cells+1)); note "LIVE cell: $arch/$class"; else die "$arch/$class not live"; fi
}
for arch in "${ARCHES[@]}"; do
  seal_cell "$arch" direct  IpcSendSharedRegionDirect  "$LOGDIR/${arch}.log"
  seal_cell "$arch" enqueue IpcSendSharedRegionEnqueue "$LOGDIR/${arch}.log"
done

if (( fail )) || (( live_cells != 6 )); then
  echo "SECOND_COHORT_SHARED_REGION_SEAL arches=3 classes=2 live_cells=${live_cells} result=fail"
  exit 1
fi
echo "SECOND_COHORT_SHARED_REGION_SEAL arches=3 classes=2 live_cells=6 fuse_trips=0 result=ok"
