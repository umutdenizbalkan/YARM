#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198B — SECOND-COHORT ORDINARY-CAP IpcSend CROSS-ARCHITECTURE PARITY SEAL.
#
# Runs the ordinary-cap IpcSend live oracles against FRESH artifacts on x86_64, AArch64 and RISC-V
# and asserts BOTH ordinary-cap success cells are proven LIVE on all three arches:
#
#   1. Ordinary-cap transfer to an already recv-v2-blocked receiver  → class=IpcSendOrdinaryCap
#   2. Ordinary-cap no-waiter enqueue + later recv-v2 dequeue         → class=IpcSendOrdinaryCapEnqueue
#
# For each (arch, class) the seal is "live" when the arch-tagged retirement marker AND the canonical
# per-arch oracle attestation (fresh receiver-local cap + AUTHORITATIVE object identity, proven by a
# round-trip probe through the materialized cap) both appear in a fresh QEMU boot:
#
#   GLOBAL_LOCK_RETIRE_CLASS_DONE arch=<arch> class=IpcSendOrdinaryCap result=ok
#   IPCSEND_ORDINARY_CAP_BLOCKED_RECEIVER_ORACLE_DONE arch=<arch> result=ok payload_len=8 receiver_resumes=1 fresh_cap=1 object_identity_ok=1
#   GLOBAL_LOCK_RETIRE_CLASS_DONE arch=<arch> class=IpcSendOrdinaryCapEnqueue result=ok
#   IPCSEND_ORDINARY_CAP_ENQUEUE_ORACLE_DONE arch=<arch> result=ok payload_len=8 dequeue_count=1 fresh_cap=1 object_identity_ok=1
#
# All 6 cells (3 arches × 2 classes) must be live-proven — there is NO source-guard substitute.
# NO reply-cap / shared-region / D2 path is exercised. The final seal requires:
#   SECOND_COHORT_ORDINARY_CAP_MATRIX arches=3 classes=2 live_cells=6 result=ok
#   SECOND_COHORT_ORDINARY_CAP_SEAL arches=3 classes=2 live_cells=6 result=ok
#
# The seal markers below are emitted BY THIS SCRIPT from the per-arch logs — no kernel markers were
# added to fabricate the matrix. Exits non-zero on any missing proof or any forbidden marker.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/second-cohort-ordinary-cap-seal}
mkdir -p "$LOGDIR"
STRICT=${QEMU_SMOKE_STRICT:-1}
fail=0

note() { echo "[seal] $*"; }
die()  { echo "[seal][fail] $*"; fail=1; }

# ── 1. Require fresh artifacts + record hashes/mtimes ──
note "artifact hashes / mtimes:"
for f in build-x86_64/kernel_boot.elf build-aarch64/yarm-aarch64.bin build-riscv64/yarm-riscv64.bin; do
  if [[ ! -f "$f" ]]; then die "missing artifact: $f (build fresh first)"; continue; fi
  printf '  %s  %s  %s\n' "$(sha256sum "$f" | cut -d' ' -f1)" "$(stat -c '%y' "$f" | cut -d'.' -f1)" "$f"
done
(( fail )) && { echo "SECOND_COHORT_ORDINARY_CAP_SEAL arches=3 classes=2 result=fail reason=missing_artifacts"; exit 1; }

# ── 2. Run the ordinary-cap blocked + enqueue oracle per arch, one log per (arch, cell) ──
run() { # run <logfile> <env=val...> scripts/qemu-ipc-recv-v2-oracle-smoke.sh <arch>
  local log="$1"; shift
  note "run: $* (log=$log)"
  # The oracle wrapper hardcodes its own kernel-serial LOGFILE and tees the full boot serial to its
  # stdout, so capture the wrapper's stdout/stderr (which contains every kernel marker) into our
  # per-cell log rather than relying on LOGFILE (which the wrapper overrides).
  env QEMU_SMOKE_STRICT="$STRICT" "$@" >"$log" 2>&1 || true
}

for arch in x86_64 aarch64 riscv64; do
  run "$LOGDIR/${arch}_capdirect.log"  YARM_IPC_SEND_CAP_ORACLE=1         scripts/qemu-ipc-recv-v2-oracle-smoke.sh "$arch"
  run "$LOGDIR/${arch}_capenqueue.log" YARM_IPC_SEND_CAP_ENQUEUE_ORACLE=1 scripts/qemu-ipc-recv-v2-oracle-smoke.sh "$arch"
done

# ── 3. Reject forbidden markers across every log ──
# Any boundary FAIL, dispatch-post-work fail, a stale/duplicated wake, an identity-proof failure, or
# ANY reply-cap / shared-region retirement leaking into an ordinary-cap boot breaks the stage scope.
FORBIDDEN='IPC_SEND_CAP_BOUNDARY_SPLIT_FAIL|IPC_SEND_CAP_ENQUEUE_BOUNDARY_SPLIT_FAIL|DISPATCH_POST_WORK_FAIL kind=blocked_waiter_ordinary_cap|ORACLE_IDENTITY_FAIL|class=IpcSendReplyCap|class=IpcSendSharedRegion|RISCV_TRAP_HANDLE_FAILED|RISCV_TRAP_UNHANDLED|reason=trap_from_s_mode|FATAL|!BN'
for log in "$LOGDIR"/*_capdirect.log "$LOGDIR"/*_capenqueue.log; do
  [[ -f "$log" ]] || continue
  if rg -a -n "$FORBIDDEN" "$log" >/dev/null 2>&1; then
    die "forbidden marker in $(basename "$log"): $(rg -a -oN "$FORBIDDEN" "$log" | head -1)"
  fi
done

# ── 4. Per-arch / per-class seal (retirement marker + attestation both LIVE) ──
live_cells=0
seal_cap_direct() { # <arch> <logfile>
  local arch="$1" log="$2"
  local retire="GLOBAL_LOCK_RETIRE_CLASS_DONE arch=${arch} class=IpcSendOrdinaryCap result=ok"
  local attest="IPCSEND_ORDINARY_CAP_BLOCKED_RECEIVER_ORACLE_DONE arch=${arch} result=ok payload_len=8 receiver_resumes=1 fresh_cap=1 object_identity_ok=1"
  if rg -a -N "$retire" "$log" >/dev/null 2>&1 && rg -a -N "$attest" "$log" >/dev/null 2>&1; then
    echo "SECOND_COHORT_ORDINARY_CAP_SEAL arch=${arch} class=IpcSendOrdinaryCap result=ok proof=live"
    live_cells=$((live_cells+1)); return 0
  fi
  echo "SECOND_COHORT_ORDINARY_CAP_SEAL arch=${arch} class=IpcSendOrdinaryCap result=MISSING"
  die "no live proof for arch=${arch} class=IpcSendOrdinaryCap"; return 1
}
seal_cap_enqueue() { # <arch> <logfile>
  local arch="$1" log="$2"
  local retire="GLOBAL_LOCK_RETIRE_CLASS_DONE arch=${arch} class=IpcSendOrdinaryCapEnqueue result=ok"
  local attest="IPCSEND_ORDINARY_CAP_ENQUEUE_ORACLE_DONE arch=${arch} result=ok payload_len=8 dequeue_count=1 fresh_cap=1 object_identity_ok=1"
  if rg -a -N "$retire" "$log" >/dev/null 2>&1 && rg -a -N "$attest" "$log" >/dev/null 2>&1; then
    echo "SECOND_COHORT_ORDINARY_CAP_SEAL arch=${arch} class=IpcSendOrdinaryCapEnqueue result=ok proof=live"
    live_cells=$((live_cells+1)); return 0
  fi
  echo "SECOND_COHORT_ORDINARY_CAP_SEAL arch=${arch} class=IpcSendOrdinaryCapEnqueue result=MISSING"
  die "no live proof for arch=${arch} class=IpcSendOrdinaryCapEnqueue"; return 1
}

echo "── second-cohort ordinary-cap seal matrix ──"
arches_ok=0
for arch in x86_64 aarch64 riscv64; do
  n=0
  seal_cap_direct  "$arch" "$LOGDIR/${arch}_capdirect.log"  && n=$((n+1))
  seal_cap_enqueue "$arch" "$LOGDIR/${arch}_capenqueue.log" && n=$((n+1))
  echo "SECOND_COHORT_ORDINARY_CAP_SEAL arch=${arch} classes=${n} result=$([[ $n -eq 2 ]] && echo ok || echo fail)"
  [[ $n -eq 2 ]] && arches_ok=$((arches_ok+1))
done

# ── 5. Final cross-architecture seal (require all 6 cells live) ──
if [[ $live_cells -eq 6 ]]; then
  echo "SECOND_COHORT_ORDINARY_CAP_MATRIX arches=3 classes=2 live_cells=6 result=ok"
else
  echo "SECOND_COHORT_ORDINARY_CAP_MATRIX arches=3 classes=2 live_cells=${live_cells} result=fail"
  die "expected 6 live cells, found ${live_cells}"
fi

if [[ $arches_ok -eq 3 && $live_cells -eq 6 && $fail -eq 0 ]]; then
  echo "SECOND_COHORT_ORDINARY_CAP_SEAL arches=3 classes=2 live_cells=6 result=ok"
  exit 0
else
  echo "SECOND_COHORT_ORDINARY_CAP_SEAL arches=${arches_ok} classes=2 live_cells=${live_cells} result=fail"
  exit 1
fi
