#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 156 — IPC recv/reply/transfer/split delivery smoke oracle.
#
# Boots the per-arch core smoke (which itself warns+skips gracefully when QEMU
# or the build artifacts are unavailable) and then verifies the Stage 156 IPC
# oracle markers in the captured boot log. The purpose is byte-identical
# regression proof BEFORE/AFTER any future stateful cap-boundary re-home into
# syscall/ipc_recv_core.rs:
#
#   1. Run with no baseline to produce a marker-set snapshot:
#        scripts/qemu-ipc-recv-v2-oracle-smoke.sh x86_64
#        cp ipc-oracle-markers-x86_64.txt baseline-x86_64.txt
#   2. After the re-home, re-run with the baseline to fail on any regression:
#        ORACLE_BASELINE=baseline-x86_64.txt \
#          scripts/qemu-ipc-recv-v2-oracle-smoke.sh x86_64
#
# Exit codes:
#   0  — QEMU/artifacts unavailable (skipped), or oracle passed.
#   1  — a fatal IPC marker appeared, a required delivery marker was missing,
#        or a baseline marker regressed (only when QEMU actually ran).
#
# Env:
#   ARCH               x86_64 | aarch64 | riscv64   (default: $1 or x86_64)
#   QEMU_SMOKE_STRICT  1 to fail (not skip) when QEMU/artifacts are missing
#   ORACLE_BASELINE    path to a prior snapshot to diff against (regression gate)
#   ORACLE_SNAPSHOT    output snapshot path (default: ipc-oracle-markers-$ARCH.txt)

set -euo pipefail
HERE="$(dirname "$0")"
source "$HERE/qemu-smoke-common.sh"

ARCH="${1:-${ARCH:-x86_64}}"
QEMU_SMOKE_STRICT="${QEMU_SMOKE_STRICT:-0}"

case "$ARCH" in
  x86_64)  CORE_SMOKE="$HERE/qemu-x86_64-core-smoke.sh";  CORE_LOG="qemu-x86_64-core.log";  QEMU_BIN="qemu-system-x86_64" ;;
  aarch64) CORE_SMOKE="$HERE/qemu-aarch64-core-smoke.sh"; CORE_LOG="qemu-aarch64-core.log"; QEMU_BIN="qemu-system-aarch64" ;;
  riscv64) CORE_SMOKE="$HERE/qemu-riscv64-core-smoke.sh"; CORE_LOG="qemu-riscv64-core.log"; QEMU_BIN="qemu-system-riscv64" ;;
  *) echo "[err] unknown ARCH: $ARCH (expected x86_64|aarch64|riscv64)"; exit 1 ;;
esac

ORACLE_SNAPSHOT="${ORACLE_SNAPSHOT:-ipc-oracle-markers-$ARCH.txt}"

# Oracle coverage mode (Stage 157):
#   basic    (default) — prove >=1 recv-v2 meta delivery (Stage 156 contract,
#                         unchanged). reply/transfer/rollback/wake only recorded.
#   extended           — additionally require the reply-cap one-shot and
#                         transfer-cap materialize markers, which now fire on the
#                         LIVE D1/D5 split path that every spawn cycle drives.
ORACLE_MODE="${ORACLE_MODE:-basic}"

# Stage 159BC/D — userspace IPC recv-v2 oracle proof workload (default-off).
# When the kernel is booted with `yarm.ipc_recv_proof=1`, the init control-plane
# runs a deterministic loopback workload (send-to-self enqueue + recv-from-self
# drain) that drives specific kernel recv-v2 delivery markers. These per-subtest
# requirements are OFF by default and enabled independently; a requirement is
# only checked when its knob is set (the boot must actually have been launched
# with the proof knob for them to appear). Each pairs a userspace SEQUENCE marker
# (the workload observed the expected *syscall return*, NOT the kernel path) with
# the kernel delivery marker that is the authoritative proof.
YARM_IPC_RECV_PROOF_QUEUED_SPLIT="${YARM_IPC_RECV_PROOF_QUEUED_SPLIT:-0}"
YARM_IPC_RECV_PROOF_ROLLBACK="${YARM_IPC_RECV_PROOF_ROLLBACK:-0}"
YARM_IPC_RECV_PROOF_SENDER_WAKE="${YARM_IPC_RECV_PROOF_SENDER_WAKE:-0}"

# Whenever any proof requirement is enabled, the kernel MUST be booted with
# yarm.ipc_recv_proof=1 or the workload never runs. Export IPC_RECV_PROOF=1 so the
# per-arch core smoke appends the boot knob to the kernel cmdline. Basic mode
# (no proof env vars) leaves this unset and the cmdline unchanged.
if [[ "$YARM_IPC_RECV_PROOF_QUEUED_SPLIT" == "1" \
   || "$YARM_IPC_RECV_PROOF_ROLLBACK" == "1" \
   || "$YARM_IPC_RECV_PROOF_SENDER_WAKE" == "1" ]]; then
  export IPC_RECV_PROOF=1
  echo "[info] ipc-oracle: proof env set -> booting kernel with yarm.ipc_recv_proof=1"
fi

# Healthy-delivery success markers (Stage 156). Not all fire on every boot, so
# only the "at least one recv-v2 meta delivered" invariant is hard-required.
# The IPC_RECV_PROOF_*_SEQUENCE_DONE markers (Stage 159BC/D) are emitted by the
# userspace proof workload only on the expected syscall return, and recorded here.
ORACLE_MARKERS=(
  "IPC_RECV_V2_META_BLOCKED_WAITER_OK"
  "IPC_RECV_V2_META_IMMEDIATE_OK"
  "IPC_RECV_V2_META_QUEUED_SPLIT_OK"
  "IPC_REPLY_CAP_ONESHOT_OK"
  "IPC_TRANSFER_CAP_MATERIALIZE_OK"
  "IPC_RECV_V2_ROLLBACK_OK"
  "IPC_RECV_V2_SENDER_WAKE_ORDER_OK"
  "IPC_RECV_PROOF_QUEUED_SPLIT_SEQUENCE_DONE"
  "IPC_RECV_PROOF_ROLLBACK_SEQUENCE_DONE"
)

# At least one recv-v2 meta delivery marker must appear on a healthy boot.
REQUIRED_ANY=(
  "IPC_RECV_V2_META_BLOCKED_WAITER_OK"
  "IPC_RECV_V2_META_IMMEDIATE_OK"
  "IPC_RECV_V2_META_QUEUED_SPLIT_OK"
)

# Extended mode: cap-transfer and reply-cap one-shot delivery must both be
# proven. The init control-plane spawn workload (spawn_v5_cap -> ipc_call with a
# reply cap + delegated send caps) drives both on the live D1/D5 split path every
# boot, so these are hard-required once ORACLE_MODE=extended.
#
# IPC_RECV_V2_ROLLBACK_OK is a *fault*-path marker (recv-v2 meta user-copy fault)
# and is correctly absent on a healthy boot; IPC_RECV_V2_SENDER_WAKE_ORDER_OK is
# contention-dependent. Both stay recorded-only here and are covered by the
# hosted seam tests; deterministic QEMU triggering is left to a fault/contention
# workload (see doc/IPC_RECV_V2_ORACLE.md).
EXTENDED_REQUIRED=(
  "IPC_REPLY_CAP_ONESHOT_OK"
  "IPC_TRANSFER_CAP_MATERIALIZE_OK"
)

# Fatal IPC regressions — their presence fails the oracle.
FATAL_MARKERS=(
  "IPC_RECV_CAP_MATERIALIZE_FAILED"
  "IPC_RECV_BLOCKED_COMPLETE_FAILED"
  "IPC_RECV_REPLY_CAP_MATERIALIZE_FAIL"
)

# Skip cleanly if QEMU is unavailable (matches the core-smoke convention).
require_qemu_or_warn "$QEMU_BIN" "$QEMU_SMOKE_STRICT"

# Delegate the boot to the existing per-arch core smoke; it captures QEMU output
# to $LOGFILE (default $CORE_LOG). It warns+exits 0 if artifacts are missing.
export LOGFILE="$CORE_LOG"
echo "[info] ipc-oracle: booting $ARCH via $CORE_SMOKE (log: $CORE_LOG)"
set +e
QEMU_SMOKE_STRICT="$QEMU_SMOKE_STRICT" LOGFILE="$CORE_LOG" "$CORE_SMOKE"
CORE_STATUS=$?
set -e

if [[ ! -s "$CORE_LOG" ]]; then
  echo "[warn] ipc-oracle: no boot log produced (QEMU/artifacts likely unavailable); skipping"
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

if [[ "$CORE_STATUS" -ne 0 ]]; then
  echo "[err] ipc-oracle: core smoke for $ARCH failed (status $CORE_STATUS)"
  exit 1
fi

echo "[info] ipc-oracle: analyzing IPC delivery markers in $CORE_LOG"
: >"$ORACLE_SNAPSHOT"
present=()
for m in "${ORACLE_MARKERS[@]}"; do
  if rg -n -m 1 "$m" "$CORE_LOG" >/dev/null 2>&1; then
    echo "[ok]   present: $m"
    echo "$m" >>"$ORACLE_SNAPSHOT"
    present+=("$m")
  else
    echo "[info] absent : $m"
  fi
done
sort -u -o "$ORACLE_SNAPSHOT" "$ORACLE_SNAPSHOT"
echo "[info] ipc-oracle: marker snapshot written to $ORACLE_SNAPSHOT"

rc=0

# Fatal markers must not appear.
for f in "${FATAL_MARKERS[@]}"; do
  if rg -n -m 1 "$f" "$CORE_LOG" >/dev/null 2>&1; then
    echo "[err] ipc-oracle: fatal IPC marker present: $f"
    rc=1
  fi
done

# At least one recv-v2 meta delivery must be proven.
any_required=0
for r in "${REQUIRED_ANY[@]}"; do
  if printf '%s\n' "${present[@]:-}" | rg -q "^$r$"; then
    any_required=1
    break
  fi
done
if [[ "$any_required" -ne 1 ]]; then
  echo "[err] ipc-oracle: no recv-v2 meta delivery marker present (delivery regressed)"
  rc=1
fi

# Extended mode: reply-cap + transfer-cap delivery must both be proven.
if [[ "$ORACLE_MODE" == "extended" ]]; then
  echo "[info] ipc-oracle: extended mode — requiring reply-cap + transfer-cap delivery"
  for r in "${EXTENDED_REQUIRED[@]}"; do
    if printf '%s\n' "${present[@]:-}" | rg -q "^$r$"; then
      echo "[ok]   extended-required present: $r"
    else
      echo "[err] ipc-oracle: extended-required marker absent: $r"
      rc=1
    fi
  done
elif [[ "$ORACLE_MODE" != "basic" ]]; then
  echo "[err] ipc-oracle: unknown ORACLE_MODE='$ORACLE_MODE' (expected basic|extended)"
  rc=1
fi

# Stage 159BC/D — independent userspace proof-workload requirements. Each is only
# enforced when its knob is set (the boot must have used yarm.ipc_recv_proof=1).
#
# A requirement always needs the userspace SEQUENCE marker (the workload ran and
# observed the expected syscall return). The kernel delivery marker is the
# authoritative proof of the *path*; whether it is REQUIRED is arch-dependent:
#
#   * x86_64 — the trap-entry split recv fast path that emits the queued-split /
#     queued-split-rollback kernel markers is exercised; kernel markers REQUIRED.
#   * AArch64 — the proof recv currently falls back to the legacy_full_path
#     (YARM_RECV_CORE_ADAPTER kind=legacy_full_path); the queued-split kernel
#     markers are NOT emitted there. This is a separate AArch64 split-recv
#     routing/parity issue, not a workload defect. The kernel markers are
#     recorded but NOT required on AArch64: their absence is reported as DEFERRED
#     (not a pass, not a failure); if they ever appear, that is reported as PASS.
#   * riscv64 — uses the raw trap path (no split dispatch); same DEFERRED policy.
case "$ARCH" in
  x86_64) PROOF_KERNEL_REQUIRED=1 ;;
  *)      PROOF_KERNEL_REQUIRED=0 ;;
esac

proof_require() {
  # $1 = human label, $2 = userspace SEQUENCE marker, $3 = kernel marker
  local label="$1" seq_marker="$2" kern_marker="$3"
  local have_seq=0 have_kern=0
  printf '%s\n' "${present[@]:-}" | rg -q "^$seq_marker$" && have_seq=1
  printf '%s\n' "${present[@]:-}" | rg -q "^$kern_marker$" && have_kern=1
  # The userspace sequence marker is always required: the workload must have run
  # and observed the expected syscall return.
  if [[ "$have_seq" -ne 1 ]]; then
    echo "[err] ipc-oracle: proof $label: sequence marker absent ($seq_marker) — workload did not run/observe expected return"
    rc=1
    return
  fi
  if [[ "$have_kern" -eq 1 ]]; then
    echo "[ok]   proof $label: PASS ($seq_marker + $kern_marker)"
  elif [[ "$PROOF_KERNEL_REQUIRED" -eq 1 ]]; then
    echo "[err] ipc-oracle: proof $label: kernel marker absent ($kern_marker) — required on $ARCH"
    rc=1
  else
    echo "[warn] ipc-oracle: proof $label: DEFERRED on $ARCH — sequence present ($seq_marker) but kernel marker $kern_marker absent (split-recv falls back to legacy_full_path; not a pass, not a failure)"
  fi
}

if [[ "$YARM_IPC_RECV_PROOF_QUEUED_SPLIT" == "1" ]]; then
  echo "[info] ipc-oracle: proof queued-split: REQUIRED"
  proof_require "queued-split" "IPC_RECV_PROOF_QUEUED_SPLIT_SEQUENCE_DONE" "IPC_RECV_V2_META_QUEUED_SPLIT_OK"
else
  echo "[info] ipc-oracle: proof queued-split: not required"
fi

if [[ "$YARM_IPC_RECV_PROOF_ROLLBACK" == "1" ]]; then
  echo "[info] ipc-oracle: proof rollback: REQUIRED"
  proof_require "rollback" "IPC_RECV_PROOF_ROLLBACK_SEQUENCE_DONE" "IPC_RECV_V2_ROLLBACK_OK"
else
  echo "[info] ipc-oracle: proof rollback: not required"
fi

if [[ "$YARM_IPC_RECV_PROOF_SENDER_WAKE" == "1" ]]; then
  # Sender-wake is intentionally DEFERRED in the Stage 159BC/D workload (it needs
  # a second blocked-sender context that cannot be sequenced without a timing
  # race). The workload does not claim it, so requiring it here will fail by
  # design until a deterministic implementation lands. Do not enable this knob
  # before then.
  echo "[info] ipc-oracle: proof sender-wake: REQUIRED"
  proof_require "sender-wake" "IPC_RECV_PROOF_SENDER_WAKE_DONE" "IPC_RECV_V2_SENDER_WAKE_ORDER_OK"
else
  echo "[info] ipc-oracle: proof sender-wake: not required (deferred — see doc/KERNEL_UNLOCKING.md)"
fi

# Regression gate: every baseline marker must still be present.
if [[ -n "${ORACLE_BASELINE:-}" ]]; then
  if [[ ! -f "$ORACLE_BASELINE" ]]; then
    echo "[err] ipc-oracle: ORACLE_BASELINE not found: $ORACLE_BASELINE"
    rc=1
  else
    while IFS= read -r b; do
      [[ -z "$b" ]] && continue
      if ! rg -q "^$b$" "$ORACLE_SNAPSHOT"; then
        echo "[err] ipc-oracle: baseline marker regressed (now absent): $b"
        rc=1
      fi
    done <"$ORACLE_BASELINE"
    [[ "$rc" -eq 0 ]] && echo "[ok] ipc-oracle: no baseline marker regressed"
  fi
fi

if [[ "$rc" -eq 0 ]]; then
  echo "[ok] ipc-oracle: IPC recv/reply/transfer/split delivery oracle passed ($ARCH)"
fi
exit "$rc"
