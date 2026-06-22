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

# Healthy-delivery success markers (Stage 156). Not all fire on every boot, so
# only the "at least one recv-v2 meta delivered" invariant is hard-required.
ORACLE_MARKERS=(
  "IPC_RECV_V2_META_BLOCKED_WAITER_OK"
  "IPC_RECV_V2_META_IMMEDIATE_OK"
  "IPC_RECV_V2_META_QUEUED_SPLIT_OK"
  "IPC_REPLY_CAP_ONESHOT_OK"
  "IPC_TRANSFER_CAP_MATERIALIZE_OK"
  "IPC_RECV_V2_ROLLBACK_OK"
  "IPC_RECV_V2_SENDER_WAKE_ORDER_OK"
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
