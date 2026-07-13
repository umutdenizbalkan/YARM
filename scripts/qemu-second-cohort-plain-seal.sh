#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198A — SECOND-COHORT PLAIN IpcSend CROSS-ARCHITECTURE PARITY SEAL.
#
# Runs the plain-IpcSend live oracles against FRESH artifacts on x86_64, AArch64 and RISC-V and
# asserts BOTH plain success cells are proven LIVE on all three arches:
#
#   1. Plain send to an already recv-v2-blocked receiver  → class=IpcSendPlain
#   2. Plain no-waiter enqueue + later recv-v2 dequeue     → class=IpcSendPlainEnqueue
#
# For each (arch, class) the seal is "live" when the arch-tagged retirement marker AND the
# canonical per-arch oracle attestation both appear in a fresh QEMU boot:
#
#   GLOBAL_LOCK_RETIRE_CLASS_DONE arch=<arch> class=IpcSendPlain result=ok
#   IPCSEND_PLAIN_BLOCKED_RECEIVER_ORACLE_DONE arch=<arch> result=ok payload_len=8 receiver_resumes=1
#   GLOBAL_LOCK_RETIRE_CLASS_DONE arch=<arch> class=IpcSendPlainEnqueue result=ok
#   IPCSEND_PLAIN_ENQUEUE_ORACLE_DONE arch=<arch> result=ok payload_len=8 dequeue_count=1
#
# All 6 cells (3 arches × 2 classes) must be live-proven — there is NO source-guard substitute.
# NO capability transfer is exercised in this stage (plain payload only). The final seal requires:
#   SECOND_COHORT_PLAIN_MATRIX arches=3 classes=2 live_cells=6 result=ok
#   SECOND_COHORT_PLAIN_SEAL arches=3 classes=2 live_cells=6 result=ok
#
# The seal markers below are emitted BY THIS SCRIPT from the per-arch logs — no kernel markers were
# added to fabricate the matrix. Exits non-zero on any missing proof or any forbidden marker.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/second-cohort-plain-seal}
mkdir -p "$LOGDIR"
STRICT=${QEMU_SMOKE_STRICT:-1}
fail=0
CLASSES=(IpcSendPlain IpcSendPlainEnqueue)

note() { echo "[seal] $*"; }
die()  { echo "[seal][fail] $*"; fail=1; }

# ── 1. Require fresh artifacts + record hashes/mtimes ──
note "artifact hashes / mtimes:"
for f in build-x86_64/kernel_boot.elf build-aarch64/yarm-aarch64.bin build-riscv64/yarm-riscv64.bin; do
  if [[ ! -f "$f" ]]; then die "missing artifact: $f (build fresh first)"; continue; fi
  printf '  %s  %s  %s\n' "$(sha256sum "$f" | cut -d' ' -f1)" "$(stat -c '%y' "$f" | cut -d'.' -f1)" "$f"
done
(( fail )) && { echo "SECOND_COHORT_PLAIN_SEAL arches=3 classes=2 result=fail reason=missing_artifacts"; exit 1; }

# ── 2. Run the plain-blocked + plain-enqueue oracle per arch, one log per (arch, cell) ──
# The oracle wrapper (qemu-ipc-recv-v2-oracle-smoke.sh) injects yarm.ipc_recv_proof=1 plus the
# selected sub-knob and hard-requires the arch-tagged retirement marker + the per-arch attestation.
# The wrapper hardcodes its own kernel-serial LOGFILE and tees the full boot serial to its stdout,
# so we capture the wrapper's stdout/stderr (which contains every kernel marker) into our per-cell
# log rather than relying on LOGFILE (which the wrapper overrides).
run() { # run <logfile> <env=val...> scripts/qemu-ipc-recv-v2-oracle-smoke.sh <arch>
  local log="$1"; shift
  note "run: $* (log=$log)"
  env QEMU_SMOKE_STRICT="$STRICT" "$@" >"$log" 2>&1 || true
}

for arch in x86_64 aarch64 riscv64; do
  run "$LOGDIR/${arch}_plain.log"   YARM_IPC_SEND_PLAIN_ORACLE=1   scripts/qemu-ipc-recv-v2-oracle-smoke.sh "$arch"
  run "$LOGDIR/${arch}_enqueue.log" YARM_IPC_SEND_ENQUEUE_ORACLE=1 scripts/qemu-ipc-recv-v2-oracle-smoke.sh "$arch"
done

# ── 3. Reject forbidden markers across every log ──
# Any boundary FAIL, dispatch-post-work fail, or a transferred cap on a PLAIN cell breaks parity.
# The cap check is SCOPED to the plain oracle's OWN recv markers (`..._CHILD_RECV_OK` for the
# blocked-receiver cell, `..._ENQUEUE_ORACLE_RECV_OK` for the enqueue cell) so it does NOT trip on
# the unrelated cap-transferring sends every boot performs (e.g. blkcache backend registration).
# The plain cells MUST observe `transferred_cap=0`; a `transferred_cap=1` there is a parity break.
FORBIDDEN='IPC_SEND_BOUNDARY_SPLIT_FAIL|IPC_SEND_ENQUEUE_BOUNDARY_SPLIT_FAIL|DISPATCH_POST_WORK_FAIL|CHILD_RECV_OK payload_match=[0-9]+ transferred_cap=1|ENQUEUE_ORACLE_RECV_OK payload_match=[0-9]+ transferred_cap=1|RISCV_TRAP_HANDLE_FAILED|RISCV_TRAP_UNHANDLED|reason=trap_from_s_mode|FATAL|!BN'
for log in "$LOGDIR"/*_plain.log "$LOGDIR"/*_enqueue.log; do
  [[ -f "$log" ]] || continue
  if rg -a -n "$FORBIDDEN" "$log" >/dev/null 2>&1; then
    die "forbidden marker in $(basename "$log"): $(rg -a -oN "$FORBIDDEN" "$log" | head -1)"
  fi
done

# ── 4. Per-arch / per-class seal (retirement marker + attestation both LIVE) ──
live_cells=0
seal_plain_blocked() { # seal_plain_blocked <arch> <logfile>
  local arch="$1" log="$2"
  local retire="GLOBAL_LOCK_RETIRE_CLASS_DONE arch=${arch} class=IpcSendPlain result=ok"
  local attest="IPCSEND_PLAIN_BLOCKED_RECEIVER_ORACLE_DONE arch=${arch} result=ok payload_len=8 receiver_resumes=1"
  if rg -a -N "$retire" "$log" >/dev/null 2>&1 && rg -a -N "$attest" "$log" >/dev/null 2>&1; then
    echo "SECOND_COHORT_PLAIN_SEAL arch=${arch} class=IpcSendPlain result=ok proof=live"
    live_cells=$((live_cells+1)); return 0
  fi
  echo "SECOND_COHORT_PLAIN_SEAL arch=${arch} class=IpcSendPlain result=MISSING"
  die "no live proof for arch=${arch} class=IpcSendPlain"; return 1
}
seal_plain_enqueue() { # seal_plain_enqueue <arch> <logfile>
  local arch="$1" log="$2"
  local retire="GLOBAL_LOCK_RETIRE_CLASS_DONE arch=${arch} class=IpcSendPlainEnqueue result=ok"
  local attest="IPCSEND_PLAIN_ENQUEUE_ORACLE_DONE arch=${arch} result=ok payload_len=8 dequeue_count=1"
  if rg -a -N "$retire" "$log" >/dev/null 2>&1 && rg -a -N "$attest" "$log" >/dev/null 2>&1; then
    echo "SECOND_COHORT_PLAIN_SEAL arch=${arch} class=IpcSendPlainEnqueue result=ok proof=live"
    live_cells=$((live_cells+1)); return 0
  fi
  echo "SECOND_COHORT_PLAIN_SEAL arch=${arch} class=IpcSendPlainEnqueue result=MISSING"
  die "no live proof for arch=${arch} class=IpcSendPlainEnqueue"; return 1
}

echo "── second-cohort plain seal matrix ──"
arches_ok=0
for arch in x86_64 aarch64 riscv64; do
  n=0
  seal_plain_blocked "$arch" "$LOGDIR/${arch}_plain.log"   && n=$((n+1))
  seal_plain_enqueue "$arch" "$LOGDIR/${arch}_enqueue.log" && n=$((n+1))
  echo "SECOND_COHORT_PLAIN_SEAL arch=${arch} classes=${n} result=$([[ $n -eq 2 ]] && echo ok || echo fail)"
  [[ $n -eq 2 ]] && arches_ok=$((arches_ok+1))
done

# ── 5. Final cross-architecture seal (require all 6 cells live) ──
if [[ $live_cells -eq 6 ]]; then
  echo "SECOND_COHORT_PLAIN_MATRIX arches=3 classes=2 live_cells=6 result=ok"
else
  echo "SECOND_COHORT_PLAIN_MATRIX arches=3 classes=2 live_cells=${live_cells} result=fail"
  die "expected 6 live cells, found ${live_cells}"
fi

if [[ $arches_ok -eq 3 && $live_cells -eq 6 && $fail -eq 0 ]]; then
  echo "SECOND_COHORT_PLAIN_SEAL arches=3 classes=2 live_cells=6 result=ok"
  exit 0
else
  echo "SECOND_COHORT_PLAIN_SEAL arches=${arches_ok} classes=2 live_cells=${live_cells} result=fail"
  exit 1
fi
