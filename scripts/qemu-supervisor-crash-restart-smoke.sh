#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Deterministic SUP-L6 crash_test_srv restart-count smoke oracle.
#
# This script is intentionally strict: it builds/stages the gated crash_test_srv
# image, boots QEMU, and then proves exact marker counts. If runtime plumbing is
# missing, the marker oracle fails and reports the missing path; it never fakes a
# successful restart-count proof.

set -euo pipefail

ARCH=${1:-x86_64}
if [[ "$ARCH" != "x86_64" ]]; then
  echo "[error] SUP-L6 crash restart smoke currently supports x86_64 only (got: $ARCH)"
  exit 2
fi

OUT_DIR=${OUT_DIR:-build-x86_64-crash-restart}
ROOTFS_DIR=${ROOTFS_DIR:-$OUT_DIR/rootfs}
KERNEL_IMAGE=${KERNEL_IMAGE:-$OUT_DIR/kernel_boot.elf}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-$OUT_DIR/initramfs-core.cpio}
LOGFILE=${LOGFILE:-$OUT_DIR/qemu-supervisor-crash-restart.log}
SNAPSHOT=${SNAPSHOT:-$OUT_DIR/qemu-supervisor-crash-restart.markers}
TIMEOUT_SECS=${TIMEOUT_SECS:-90}
QEMU_BIN=${QEMU_BIN:-qemu-system-x86_64}
QEMU_MACHINE=${QEMU_MACHINE:-q35}
QEMU_CPU=${QEMU_CPU:-qemu64}
QEMU_MEMORY=${QEMU_MEMORY:-512M}
QEMU_SMP=1
DEFAULT_KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.supervisor_restart_test=1 yarm.crash_test_max_restarts=3 yarm.crash_test_delay_ms=1000"
KERNEL_CMDLINE=${KERNEL_CMDLINE:-$DEFAULT_KERNEL_CMDLINE}

mkdir -p "$OUT_DIR"

export OUT_DIR ROOTFS_DIR
export YARM_SUPERVISOR_RESTART_TEST=1
export SUPERVISOR_RESTART_TEST=1

echo "[info] building gated crash-test QEMU artifacts for $ARCH"
scripts/build-qemu-x86_64-artifacts.sh

if [[ ! -f "$KERNEL_IMAGE" ]]; then
  echo "[error] kernel image missing after build: $KERNEL_IMAGE"
  exit 1
fi
if [[ ! -f "$INITRAMFS_IMAGE" ]]; then
  echo "[error] initramfs image missing after build: $INITRAMFS_IMAGE"
  exit 1
fi
if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  echo "[error] $QEMU_BIN not installed; cannot run SUP-L6 QEMU proof"
  exit 2
fi

QEMU_CMD=(
  "$QEMU_BIN"
  -machine "$QEMU_MACHINE"
  -cpu "$QEMU_CPU"
  -m "$QEMU_MEMORY"
  -smp "$QEMU_SMP"
  -nographic
  -monitor none
  -serial stdio
  -no-reboot
  -no-shutdown
  -kernel "$KERNEL_IMAGE"
  -initrd "$INITRAMFS_IMAGE"
  -append "$KERNEL_CMDLINE"
)

echo "[info] qemu command: ${QEMU_CMD[*]}"
rm -f "$LOGFILE" "$SNAPSHOT" "$LOGFILE.normalized"
set +e
if command -v timeout >/dev/null 2>&1; then
  timeout --foreground "${TIMEOUT_SECS}s" stdbuf -oL -eL "${QEMU_CMD[@]}" 2>&1 | tee "$LOGFILE"
  QEMU_STATUS=${PIPESTATUS[0]}
else
  stdbuf -oL -eL "${QEMU_CMD[@]}" 2>&1 | tee "$LOGFILE"
  QEMU_STATUS=${PIPESTATUS[0]}
fi
set -e

tr '\r' '\n' <"$LOGFILE" >"$LOGFILE.normalized"
LOG_NORM="$LOGFILE.normalized"

count_marker() {
  local marker=$1
  rg -a -c "\\b${marker}\\b" "$LOG_NORM" 2>/dev/null || echo 0
}

require_count() {
  local marker=$1 expected=$2 actual
  actual=$(count_marker "$marker")
  printf '%s=%s\n' "$marker" "$actual" >>"$SNAPSHOT"
  if [[ "$actual" -ne "$expected" ]]; then
    echo "[error] marker count mismatch: $marker expected=$expected actual=$actual"
    return 1
  fi
}

require_present() {
  local marker=$1 actual
  actual=$(count_marker "$marker")
  if [[ "$actual" -lt 1 ]]; then
    echo "[error] required path marker missing: $marker"
    return 1
  fi
}

fatal_patterns=(
  "panic"
  "PANIC"
  "FATAL"
  "DOUBLE_FAULT"
  "DATA_ABORT"
  "SERROR"
  "KERNEL_FAULT"
  "FAULT_BOUNDARY"
  "Vm\\(Full\\)"
  "CapabilityFull"
  "MissingRight"
  "BLOCKED_WOULDBLOCK_FATAL"
  "SUPERVISOR_RESTART_TOKEN_QUERY_FAIL tid=10008 reason=recv"
  "SUPERVISOR_PM_RESTART_REPLY_REJECTED_STATE tid=10009 request_id=2 failure=ResourceUnavailable"
  "SPAWN_TASK_STACK_FAIL tid=10010"
  "KSPAWN_SPAWN_TASK_FAIL tid=10010"
  "PM_RESTART_SPAWN_FAIL request_id=2 target_tid=10009 reason=TableFull"
  "PM_RESTART_TEARDOWN_OLD_FAIL old_tid=10008"
  "SUPERVISOR_RESTART_RETRY_EXHAUSTED tid=10009"
  "WrongObject.*token-query"
  "StaleCapability.*token-query"
)
for pattern in "${fatal_patterns[@]}"; do
  if rg -a -n "$pattern" "$LOG_NORM" >/dev/null 2>&1; then
    echo "[error] fatal marker found in QEMU log: $pattern"
    exit 1
  fi
done

oracle_failed=0
require_count "CRASH_TEST_SRV_ENTRY" 4 || oracle_failed=1
require_count "CRASH_TEST_SRV_READY" 4 || oracle_failed=1
exit_count=$(count_marker "CRASH_TEST_SRV_EXIT_NOW")
fault_count=$(count_marker "CRASH_TEST_SRV_FAULT_NOW")
printf 'CRASH_TEST_SRV_EXIT_NOW=%s\n' "$exit_count" >>"$SNAPSHOT"
printf 'CRASH_TEST_SRV_FAULT_NOW=%s\n' "$fault_count" >>"$SNAPSHOT"
if [[ "$exit_count" -ne 4 && "$fault_count" -ne 4 ]]; then
  echo "[error] terminal marker count mismatch: expected either EXIT_NOW=4 or FAULT_NOW=4, got EXIT_NOW=$exit_count FAULT_NOW=$fault_count"
  oracle_failed=1
fi
require_count "PM_RESTART_REPLY_ACCEPTED" 3 || oracle_failed=1
require_count "SUPERVISOR_PM_RESTART_STATE_UPDATED" 3 || oracle_failed=1
require_count "SUPERVISOR_RESTART_LIMIT_EXCEEDED" 1 || oracle_failed=1
require_count "SUPERVISOR_SERVICE_DEGRADED_FINAL" 1 || oracle_failed=1

accepted=$(count_marker "PM_RESTART_REPLY_ACCEPTED")
state_updates=$(count_marker "SUPERVISOR_PM_RESTART_STATE_UPDATED")
if [[ "$accepted" -ge 4 ]]; then
  echo "[error] PM_RESTART_REPLY_ACCEPTED count must be less than 4 (actual=$accepted)"
  oracle_failed=1
fi
if [[ "$state_updates" -ge 4 ]]; then
  echo "[error] SUPERVISOR_PM_RESTART_STATE_UPDATED count must be less than 4 (actual=$state_updates)"
  oracle_failed=1
fi

for marker in \
  SUPERVISOR_PM_RESTART_SEND_BEGIN \
  SUPERVISOR_PM_RESTART_REPLY_WAIT_BEGIN \
  SUPERVISOR_PM_RESTART_REPLY_RECV \
  SUPERVISOR_PM_RESTART_REPLY_SHAPE_OK \
  SUPERVISOR_PM_RESTART_REPLY_DECODE_OK \
  SUPERVISOR_PM_RESTART_REPLY_ACCEPTED \
  SUPERVISOR_RESTART_LINEAGE_UPDATE_OK \
  SUPERVISOR_RESTART_LINEAGE_INDEX_OK \
  SUPERVISOR_EVENT_LOOP_TICK \
  SUPERVISOR_MANAGED_RECORD_REGISTER_OK \
  SUPERVISOR_CRASH_TEST_RECORD_READY \
  SUPERVISOR_IDLE_WAIT_SELECT \
  SUPERVISOR_CONTROL_WAIT_SKIPPED \
  SUPERVISOR_FAULT_WAIT_BEGIN \
  SUPERVISOR_FAULT_LOOKUP_BEGIN \
  SUPERVISOR_FAULT_LOOKUP_OK \
  SUPERVISOR_RESTART_ATTEMPT_ADVANCE \
  PM_RESTART_V1_DECODE_OK \
  PM_RESTART_SENDER_OK \
  PM_RESTART_VALIDATE_OK \
  PM_RESTART_ACCOUNTING_BEGIN \
  PM_RESTART_RESERVE_REPLACEMENT_OK \
  PM_RESTART_SPAWN_BEGIN \
  PM_RESTART_TEARDOWN_OLD_BEGIN \
  PM_RESTART_TEARDOWN_OLD_OK \
  PM_RESTART_SPAWN_OK \
  PM_RESTART_REPLY_ACCEPTED \
  SUPERVISOR_RESTART_LIMIT_EXCEEDED \
  SUPERVISOR_SERVICE_DEGRADED_FINAL; do
  require_present "$marker" || oracle_failed=1
done

require_present "SUPERVISOR_FAULT_LOOKUP_OK fault_tid=10008" || oracle_failed=1
require_present "SUPERVISOR_RESTART_TOKEN_STATE tid=10008 present=1" || oracle_failed=1
require_present "SUPERVISOR_RESTART_ATTEMPT_ADVANCE old=0 new=1" || oracle_failed=1
require_present "SUPERVISOR_RESTART_SCHEDULED tid=10008" || oracle_failed=1
require_present "PM_RESTART_TEARDOWN_OLD_OK old_tid=10008" || oracle_failed=1
require_present "PM_RESTART_SPAWN_OK target_tid=10009 replacement_tid=10010" || oracle_failed=1
require_present "SUPERVISOR_PM_RESTART_STATE_UPDATED tid=10010 replacement_tid=10010 attempt=2" || oracle_failed=1
if ! rg -a "SUPERVISOR_FAULT_(WAIT|DRAIN)_RECV tid=10008" "$LOG_NORM" >/dev/null 2>&1; then
  echo "[error] required fault receive marker missing for tid=10008 (expected WAIT_RECV or DRAIN_RECV)"
  oracle_failed=1
fi

# SUP-L7H: pending-fault replay is a race fallback, not an unconditional
# smoke requirement. If the crash-test record was not ready before the first
# fault, require the fallback stash/replay path; otherwise the direct
# registered-fault path is sufficient.
if ! grep -q "SUPERVISOR_CRASH_TEST_RECORD_READY tid=10008" "$LOG_NORM"; then
  require_present "SUPERVISOR_FAULT_PENDING_STASH tid=10008" || oracle_failed=1
  require_present "SUPERVISOR_FAULT_PENDING_REPLAY_OK tid=10008" || oracle_failed=1
fi

if [[ "$QEMU_STATUS" -ne 0 && "$QEMU_STATUS" -ne 124 ]]; then
  echo "[error] QEMU exited with unexpected status $QEMU_STATUS"
  oracle_failed=1
fi

if [[ "$oracle_failed" -ne 0 ]]; then
  echo "[error] SUP-L6 crash restart smoke FAILED"
  echo "[info] marker snapshot: $SNAPSHOT"
  echo "[info] if CRASH_TEST markers are absent, likely missing runtime gate propagation, initial crash_test spawn/registration, or PM/supervisor gate enablement."
  exit 1
fi

echo "[ok] SUP-L6 crash restart smoke passed"
echo "[ok] marker snapshot: $SNAPSHOT"
