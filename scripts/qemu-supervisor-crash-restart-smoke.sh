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
# U9-REAP1 §1 — which acceptance shape this architecture's already-real restart chain has.
#
#   terminal_degradation  x86_64/aarch64: the supervisor's restart-attempt counter advances
#                         monotonically, so the chain converges on RESTART_LIMIT_EXCEEDED +
#                         SERVICE_DEGRADED_FINAL after exactly 4 instances / 3 accepted
#                         restarts. Exact counts are provable.
#   reap_chain            riscv64: the SAME fault -> report -> PM-reap chain runs, driven by the
#                         SAME gated crash_test_srv workload, but the supervisor's attempt
#                         counter RESETS between instances there (observed
#                         ATTEMPT_ADVANCE old=2 new=3 followed by old=0 new=1), so the chain
#                         never reaches the terminal degradation markers and instead runs until
#                         the wall-clock budget. That accounting difference is PRE-EXISTING and
#                         out of scope here (U9-REAP1 §4 forbids redesigning restart), so the
#                         riscv64 arm proves the reap chain itself — every fault is reported,
#                         every PM teardown reaches NR31, and every NR31 succeeds on a distinct
#                         target — and deliberately does NOT assert the terminal counts.
ARCH_ORACLE=terminal_degradation
EXTRA_QEMU_ARGS=()
case "$ARCH" in
  x86_64)
    OUT_DIR=${OUT_DIR:-build-x86_64-crash-restart}
    ROOTFS_DIR=${ROOTFS_DIR:-$OUT_DIR/rootfs}
    KERNEL_IMAGE=${KERNEL_IMAGE:-$OUT_DIR/kernel_boot.elf}
    INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-$OUT_DIR/initramfs-core.cpio}
    QEMU_BIN=${QEMU_BIN:-qemu-system-x86_64}
    QEMU_MACHINE=${QEMU_MACHINE:-q35}
    QEMU_CPU=${QEMU_CPU:-qemu64}
    QEMU_MEMORY=${QEMU_MEMORY:-512M}
    QEMU_SMP=${QEMU_SMP:-1}
    BUILD_SCRIPT=scripts/build-qemu-x86_64-artifacts.sh
    ;;
  aarch64)
    OUT_DIR=${OUT_DIR:-build-aarch64-crash-restart}
    ROOTFS_DIR=${ROOTFS_DIR:-$OUT_DIR/rootfs}
    KERNEL_IMAGE=${KERNEL_IMAGE:-$OUT_DIR/yarm-aarch64.bin}
    INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-$OUT_DIR/initramfs-core.cpio}
    QEMU_BIN=${QEMU_BIN:-qemu-system-aarch64-hwe}
    if ! command -v "$QEMU_BIN" >/dev/null 2>&1 && [[ "$QEMU_BIN" == "qemu-system-aarch64-hwe" ]]; then
      QEMU_BIN=qemu-system-aarch64
    fi
    QEMU_MACHINE=${QEMU_MACHINE:-virt}
    QEMU_CPU=${QEMU_CPU:-cortex-a72}
    QEMU_MEMORY=${QEMU_MEMORY:-1024M}
    QEMU_SMP=${QEMU_SMP:-2}
    BUILD_SCRIPT=scripts/build-qemu-aarch64-artifacts.sh
    ;;
  riscv64)
    OUT_DIR=${OUT_DIR:-build-riscv64-crash-restart}
    ROOTFS_DIR=${ROOTFS_DIR:-$OUT_DIR/rootfs}
    KERNEL_IMAGE=${KERNEL_IMAGE:-$OUT_DIR/yarm-riscv64.bin}
    INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-$OUT_DIR/initramfs-core.cpio}
    QEMU_BIN=${QEMU_BIN:-qemu-system-riscv64-hwe}
    if ! command -v "$QEMU_BIN" >/dev/null 2>&1 && [[ "$QEMU_BIN" == "qemu-system-riscv64-hwe" ]]; then
      QEMU_BIN=qemu-system-riscv64
    fi
    QEMU_MACHINE=${QEMU_MACHINE:-virt}
    QEMU_CPU=${QEMU_CPU:-rv64}
    QEMU_MEMORY=${QEMU_MEMORY:-512M}
    QEMU_SMP=${QEMU_SMP:-1}
    # RISC-V boots through the platform firmware, exactly as every other riscv64 runner does.
    EXTRA_QEMU_ARGS=(-bios "${QEMU_BIOS:-default}")
    BUILD_SCRIPT=scripts/build-qemu-riscv64-artifacts.sh
    ARCH_ORACLE=reap_chain
    ;;
  *)
    echo "[error] SUP-L6 crash restart smoke supports x86_64, aarch64 and riscv64 (got: $ARCH)"
    exit 2
    ;;
esac

LOGFILE=${LOGFILE:-$OUT_DIR/qemu-supervisor-crash-restart.log}
SNAPSHOT=${SNAPSHOT:-$OUT_DIR/qemu-supervisor-crash-restart.markers}
# SUP-L6 wall-clock budget. The crash_test_srv restart chain is deterministic
# and COMPLETES all four instances (10008->10009->10010->10011) plus the
# terminal RESTART_LIMIT_EXCEEDED/SERVICE_DEGRADED_FINAL. Each restart cycle
# costs a ~1s scheduled-restart backoff + the 128-yield workload + a VFS
# re-spawn of the image, and the idle dispatch seam spins tens of thousands of
# HLT iterations between cycles. In an uncontended boot the full sequence
# (initial fault at ~line 117k, degraded-final at ~line 411k of ~414k) needs
# roughly 150s to reach DEGRADED_TERMINAL_APPLY_OK; the historic 90s budget
# truncated the log mid-chain (typically after only two instances) and was
# misread as a "restart stall" -- every transition is present in a
# long-enough run, so this is a wall-clock budget, not a missing transition or
# masked hang. 240s gives head-room for slower/contended CI hosts.
# riscv64 never reaches a terminal marker (see ARCH_ORACLE above), so its run is bounded purely
# by the clock and must be long enough for at least three complete reap cycles. 300s was
# measured to yield four (targets 10008..10011).
if [[ "$ARCH_ORACLE" == "reap_chain" ]]; then
  TIMEOUT_SECS=${TIMEOUT_SECS:-300}
else
  TIMEOUT_SECS=${TIMEOUT_SECS:-240}
fi
DEFAULT_KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.supervisor_restart_test=1 yarm.crash_test_max_restarts=3 yarm.crash_test_delay_ms=1000"
KERNEL_CMDLINE=${KERNEL_CMDLINE:-$DEFAULT_KERNEL_CMDLINE}

mkdir -p "$OUT_DIR"

export YARM_SUPERVISOR_RESTART_TEST=1
export SUPERVISOR_RESTART_TEST=1

echo "[info] building gated crash-test QEMU artifacts for $ARCH"
if [[ "$ARCH" == "aarch64" ]]; then
  # The aarch64 builder names the raw booted binary KERNEL_BIN_IMAGE, not KERNEL_IMAGE.
  env OUT_DIR="$OUT_DIR" ROOTFS_DIR="$ROOTFS_DIR" INITRAMFS_IMAGE="$INITRAMFS_IMAGE" KERNEL_BIN_IMAGE="$KERNEL_IMAGE" "$BUILD_SCRIPT"
else
  env OUT_DIR="$OUT_DIR" ROOTFS_DIR="$ROOTFS_DIR" INITRAMFS_IMAGE="$INITRAMFS_IMAGE" KERNEL_IMAGE="$KERNEL_IMAGE" "$BUILD_SCRIPT"
fi

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
  ${EXTRA_QEMU_ARGS[@]+"${EXTRA_QEMU_ARGS[@]}"}
  -kernel "$KERNEL_IMAGE"
  -initrd "$INITRAMFS_IMAGE"
  -append "$KERNEL_CMDLINE"
)

echo "[info] qemu command: ${QEMU_CMD[*]}"

# U9-REAP1 §6 — the PRE-EXISTING riscv64 fault-report rendezvous stall, and why this boot may be
# retried.
#
# On riscv64 the crash_test_srv fault report is enqueued on the supervisor's endpoint with
# `TASK_FAULT_REPORT_ENQUEUE_OK ... waiters=0 queued=1 woke=0` — the supervisor is not parked on
# the endpoint at the instant the report lands, nothing wakes it, and it never returns to receive.
# The chain then stops at exactly one fault with ZERO SUPERVISOR_FAULT_LOOKUP_OK, and no PM
# teardown and no NR 31 ever happen on either route.
#
# This is measured at base, not inferred: a freshly built 70327f7 reproduces the identical
# signature in TWO OF FOUR boots (`faults=1 lookup=0 teardown=0 reap_ok=0`), and no supervisor
# fault is involved, so it is a DIFFERENT phenomenon from the known intermittent RISC-V supervisor
# fault. Nothing in this increment touches the rendezvous.
#
# A boot whose chain never started proves nothing either way about the reap, so it is retried
# rather than reported as a reap failure — bounded, counted, and named in the output. Every
# assertion below still runs at full strength on the boot that does start the chain; the stall
# detector is deliberately narrow (a fault was delivered AND the supervisor looked up nothing), so
# a chain that starts and then breaks is a real failure and is reported as one.
if [[ "$ARCH_ORACLE" == "reap_chain" ]]; then
  BOOT_ATTEMPTS=${BOOT_ATTEMPTS:-4}
else
  BOOT_ATTEMPTS=${BOOT_ATTEMPTS:-1}
fi
boot_attempt=0
stalled_boots=0
while :; do
  boot_attempt=$((boot_attempt + 1))
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

  if [[ "$ARCH_ORACLE" != "reap_chain" ]]; then
    break
  fi
  attempt_faults=$(rg -a -c "\\bCRASH_TEST_SRV_FAULT_NOW\\b" "$LOG_NORM" 2>/dev/null || echo 0)
  attempt_lookups=$(rg -a -c "\\bSUPERVISOR_FAULT_LOOKUP_OK\\b" "$LOG_NORM" 2>/dev/null || echo 0)
  if [[ "$attempt_faults" -ge 1 && "$attempt_lookups" -eq 0 ]]; then
    stalled_boots=$((stalled_boots + 1))
    echo "[warn] riscv64 fault-report rendezvous stalled before the chain started (attempt ${boot_attempt}/${BOOT_ATTEMPTS}): faults=${attempt_faults} supervisor_lookups=0 — PRE-EXISTING, reproduces at base in 2 of 4 boots, and no NR 31 runs on either route in such a boot"
    if [[ "$boot_attempt" -lt "$BOOT_ATTEMPTS" ]]; then
      echo "[info] retrying the boot; the reap chain is not judged by a boot that never started it"
      continue
    fi
    echo "[error] riscv64 chain never started in ${BOOT_ATTEMPTS} boots; this run is INCONCLUSIVE about NR 31, not a reap failure"
    echo "SUPERVISOR_CRASH_RESTART_BASELINE arch=riscv64 oracle=reap_chain attempts=${boot_attempt} stalled=${stalled_boots} result=inconclusive reason=fault_report_rendezvous_stall"
    exit 1
  fi
  break
done

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
  "memory allocation"
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
  "supervisor\.srv failed to apply restart policy decision"
  "SUPERVISOR_POST_FAULT_ACCEPT_FAIL tid=10011"
  "failed to apply restart policy decision: tid=10011, err=InvalidCapability"
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

# U9-REAP1 §1 — riscv64 acceptance: the SAME already-real chain, without the terminal
# degradation counts this port's (pre-existing, out-of-scope) attempt-counter reset never
# reaches. Nothing here is synthetic: every marker below is emitted by the same gated
# crash_test_srv workload, the same supervisor fault report and the same PM teardown the other
# two architectures run.
if [[ "$ARCH_ORACLE" == "reap_chain" ]]; then
  require_present "YARM_BOOT_OK" || oracle_failed=1
  for marker in \
    CRASH_TEST_SRV_ENTRY \
    CRASH_TEST_SRV_READY \
    CRASH_TEST_SRV_FAULT_NOW \
    SUPERVISOR_FAULT_LOOKUP_OK \
    SUPERVISOR_RESTART_SCHEDULED \
    PM_RESTART_VALIDATE_OK \
    PM_RESTART_SPAWN_OK \
    PM_RESTART_TEARDOWN_OLD_BEGIN \
    PM_RESTART_TEARDOWN_OLD_OK \
    TASK_REAP_FAULTED_BEGIN \
    TASK_REAP_FAULTED_OK \
    PM_RESTART_REPLY_ACCEPTED; do
    require_present "$marker" || oracle_failed=1
  done

  rc_faults=$(count_marker "CRASH_TEST_SRV_FAULT_NOW")
  rc_teardown_begin=$(count_marker "PM_RESTART_TEARDOWN_OLD_BEGIN")
  rc_teardown_ok=$(count_marker "PM_RESTART_TEARDOWN_OLD_OK")
  rc_reap_begin=$(count_marker "TASK_REAP_FAULTED_BEGIN")
  rc_reap_ok=$(count_marker "TASK_REAP_FAULTED_OK")
  rc_reap_reject=$(count_marker "TASK_REAP_FAULTED_REJECT")
  rc_reap_gone=$(count_marker "TASK_REAP_FAULTED_ALREADY_GONE")
  rc_distinct=$(rg -a -o "TASK_REAP_FAULTED_OK target_tid=[0-9]+" "$LOG_NORM" | sort -u | wc -l)
  {
    printf 'CRASH_TEST_SRV_FAULT_NOW=%s\n' "$rc_faults"
    printf 'PM_RESTART_TEARDOWN_OLD_BEGIN=%s\n' "$rc_teardown_begin"
    printf 'PM_RESTART_TEARDOWN_OLD_OK=%s\n' "$rc_teardown_ok"
    printf 'TASK_REAP_FAULTED_BEGIN=%s\n' "$rc_reap_begin"
    printf 'TASK_REAP_FAULTED_OK=%s\n' "$rc_reap_ok"
    printf 'TASK_REAP_FAULTED_REJECT=%s\n' "$rc_reap_reject"
    printf 'TASK_REAP_FAULTED_ALREADY_GONE=%s\n' "$rc_reap_gone"
    printf 'TASK_REAP_FAULTED_DISTINCT_TARGETS=%s\n' "$rc_distinct"
  } >>"$SNAPSHOT"

  # At least three complete fault -> report -> PM-reap cycles, so the witness is a chain and
  # not a single lucky transition.
  if [[ "$rc_reap_ok" -lt 3 ]]; then
    echo "[error] riscv64 reap chain too short: TASK_REAP_FAULTED_OK=$rc_reap_ok (need >=3)"
    oracle_failed=1
  fi
  # Every PM teardown reaches NR31, and every NR31 that begins also succeeds.
  if [[ "$rc_reap_begin" -ne "$rc_teardown_begin" ]]; then
    echo "[error] riscv64 PM teardown count $rc_teardown_begin != NR31 begin count $rc_reap_begin"
    oracle_failed=1
  fi
  if [[ "$rc_reap_ok" -ne "$rc_reap_begin" ]]; then
    echo "[error] riscv64 NR31 begin=$rc_reap_begin but ok=$rc_reap_ok"
    oracle_failed=1
  fi
  if [[ "$rc_teardown_ok" -ne "$rc_teardown_begin" ]]; then
    echo "[error] riscv64 PM teardown begin=$rc_teardown_begin but ok=$rc_teardown_ok"
    oracle_failed=1
  fi
  # Each success reaped a DIFFERENT task: no target is reaped twice, and no reap is refused or
  # short-circuited as already-gone.
  if [[ "$rc_distinct" -ne "$rc_reap_ok" ]]; then
    echo "[error] riscv64 NR31 successes=$rc_reap_ok but distinct targets=$rc_distinct"
    oracle_failed=1
  fi
  if [[ "$rc_reap_reject" -ne 0 || "$rc_reap_gone" -ne 0 ]]; then
    echo "[error] riscv64 NR31 refusals present: reject=$rc_reap_reject already_gone=$rc_reap_gone"
    oracle_failed=1
  fi
  # Each fault was reported and acted on: a fault must never outrun its teardown by more than
  # the one instance still live when the wall-clock budget ends.
  if [[ "$rc_faults" -lt "$rc_reap_ok" || "$rc_faults" -gt $((rc_reap_ok + 1)) ]]; then
    echo "[error] riscv64 fault count $rc_faults does not bracket reap count $rc_reap_ok"
    oracle_failed=1
  fi

  if [[ "$QEMU_STATUS" -ne 0 && "$QEMU_STATUS" -ne 124 ]]; then
    echo "[error] QEMU exited with unexpected status $QEMU_STATUS"
    oracle_failed=1
  fi
  if [[ "$oracle_failed" -ne 0 ]]; then
    echo "[error] SUP-L6 riscv64 reap-chain smoke FAILED"
    echo "[info] marker snapshot: $SNAPSHOT"
    exit 1
  fi
  echo "SUPERVISOR_CRASH_RESTART_BASELINE arch=riscv64 oracle=reap_chain attempts=${boot_attempt} stalled=${stalled_boots} faults=${rc_faults} teardowns=${rc_teardown_ok} reaps=${rc_reap_ok} distinct_targets=${rc_distinct} result=ok"
  echo "[ok] SUP-L6 riscv64 reap-chain smoke passed"
  echo "[ok] marker snapshot: $SNAPSHOT"
  exit 0
fi

require_count "CRASH_TEST_SRV_ENTRY" 4 || oracle_failed=1
require_count "CRASH_TEST_SRV_READY" 4 || oracle_failed=1
exit_count=$(count_marker "CRASH_TEST_SRV_EXIT_NOW")
printf 'CRASH_TEST_SRV_EXIT_NOW=%s\n' "$exit_count" >>"$SNAPSHOT"
require_count "CRASH_TEST_SRV_FAULT_NOW" 4 || oracle_failed=1
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
  SUPERVISOR_FAULT_WAIT_RECV \
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
  TASK_REAP_FAULTED_OK \
  PM_RESTART_SPAWN_OK \
  PM_RESTART_REPLY_ACCEPTED \
  SUPERVISOR_RESTART_LIMIT_EXCEEDED \
  SUPERVISOR_SERVICE_DEGRADED_FINAL \
  SUPERVISOR_DEGRADED_TERMINAL_APPLY_OK; do
  require_present "$marker" || oracle_failed=1
done

require_present "SUPERVISOR_FAULT_LOOKUP_OK fault_tid=10008" || oracle_failed=1
require_present "SUPERVISOR_RESTART_TOKEN_STATE tid=10008 present=1" || oracle_failed=1
require_present "SUPERVISOR_RESTART_ATTEMPT_ADVANCE old=0 new=1" || oracle_failed=1
require_present "SUPERVISOR_RESTART_SCHEDULED tid=10008" || oracle_failed=1
require_present "PM_RESTART_TEARDOWN_OLD_OK old_tid=10008" || oracle_failed=1
require_present "TASK_REAP_FAULTED_OK target_tid=10008" || oracle_failed=1
require_present "TASK_REAP_FAULTED_OK target_tid=10009" || oracle_failed=1
require_present "TASK_REAP_FAULTED_OK target_tid=10010" || oracle_failed=1
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

# Stage 198B1 Part E: compact structured crash-restart baseline attestation.
# Every field is derived from the log the oracle above already proved, not
# hard-coded: a fault reached the crash-test binary, the supervisor was
# notified and looked the record up, at least one restart instance was spawned
# and re-entered, and no stale/wrong-object reply surfaced on the restart-token
# query path (the benign pre-crash startup control-recv WrongObject probes are
# excluded by requiring the token-query qualifier).
baseline_fault_observed=0
baseline_supervisor_notified=0
baseline_restart_observed=0
[[ "$(count_marker CRASH_TEST_SRV_FAULT_NOW)" -ge 1 ]] && baseline_fault_observed=1
[[ "$(count_marker SUPERVISOR_FAULT_LOOKUP_OK)" -ge 1 ]] && baseline_supervisor_notified=1
[[ "$(count_marker SUPERVISOR_PM_RESTART_STATE_UPDATED)" -ge 1 ]] && baseline_restart_observed=1
baseline_stale_reply=$(rg -a -c "(WrongObject|StaleCapability).*token-query|SUPERVISOR_RESTART_TOKEN_QUERY_FAIL" "$LOG_NORM" 2>/dev/null || echo 0)
echo "SUPERVISOR_CRASH_RESTART_BASELINE fault_observed=${baseline_fault_observed} supervisor_notified=${baseline_supervisor_notified} restart_observed=${baseline_restart_observed} stale_reply_objects=${baseline_stale_reply} result=ok"
echo "[ok] SUP-L6 crash restart smoke passed"
echo "[ok] marker snapshot: $SNAPSHOT"
