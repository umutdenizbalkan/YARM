#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 200D-0C1 — AArch64 LIVE ExitCurrentTask (NR 16) exact-commit runner.
#
# PREPARED by Stage 200D-0C1; EXECUTED by Stage 200D-0C2. It is NOT run in the preparation
# stage — the preparation seal explicitly reports live_boots=0.
#
# It proves that a disposable EL0 task invoking NR 16 never returns, that the exiting task's
# EL0 frame is never restored, that the disposition is consumed AFTER the broad
# `SpinLock<KernelState>` is released and AFTER every shared post-lock drain, and that the
# system continues to terminal health afterwards.
#
#   RUN_A  feature-off preservation — the image must carry NO oracle literal
#   RUN_B  feature-on live cell     — one boot, one disposable exit, system stays healthy
#
# Fail-closed conditions, each independently covered by a static hosted test
# (`stage200d0c1_runner_static`): dirty tree, SHA drift, tree-hash drift, reused log,
# reused artifact, missing feature forwarding, early QEMU exit, multiple boot instances,
# missing terminal health, any hard-fail marker, and any ordering violation.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/aarch64-exit-current-task}
TIMEOUT_SECS=${TIMEOUT_SECS:-180}
IDLE_MAX_SECS=${IDLE_MAX_SECS:-180}
KTARGET=${KTARGET:-targets/aarch64-yarm-none.json}
KPROFILE=${KPROFILE:-aarch64-none}
KELF=${KELF:-target/aarch64-yarm-none/${KPROFILE}/kernel_boot}
KBIN=${KBIN:-build-aarch64/yarm-aarch64.bin}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
FEATURE=aarch64-exit-current-task-oracle
SEAL=STAGE_200D0C2_AARCH64_EXIT_CURRENT_TASK_LIVE_SEAL

fail=0
note() { echo "[aarch64-exit] $*"; }
die()  { echo "[aarch64-exit][fail] $*"; fail=1; }

# ── exact-commit identity ────────────────────────────────────────────────────────────
if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "$SEAL result=fail reason=dirty_tree"
  exit 1
fi
SHA0=$(git rev-parse HEAD)
TREE0=$(git rev-parse HEAD^{tree})
note "exact commit sha=$SHA0 tree=$TREE0 (clean)"

recheck_exact_commit() {
  local what="$1"
  [[ "$(git rev-parse HEAD)" == "$SHA0" ]]         || die "[$what] SHA drifted"
  [[ "$(git rev-parse HEAD^{tree})" == "$TREE0" ]] || die "[$what] tree hash drifted"
  git diff --quiet && git diff --cached --quiet    || die "[$what] tree became dirty"
}

rm -rf "$LOGDIR"; mkdir -p "$LOGDIR"

declare -A LOG_SEEN=()
fresh_log() {
  local p="$1"
  [[ -n "${LOG_SEEN[$p]:-}" ]] && { die "log reuse: $p already used"; return 1; }
  [[ -e "$p" ]] && { die "log reuse: $p already exists"; return 1; }
  LOG_SEEN["$p"]=1; return 0
}

# Reject a stale artifact: every image consumed by a boot must have been written by THIS run.
RUN_START=$(date +%s)
fresh_artifact() {
  local p="$1"
  [[ -e "$p" ]] || { die "artifact missing: $p"; return 1; }
  local mtime; mtime=$(stat -c %Y "$p" 2>/dev/null || echo 0)
  (( mtime >= RUN_START )) || { die "artifact reused from an earlier run: $p"; return 1; }
  return 0
}

objcopy_tool() {
  if command -v llvm-objcopy >/dev/null 2>&1; then echo llvm-objcopy;
  elif command -v rust-objcopy >/dev/null 2>&1; then echo rust-objcopy;
  else return 1; fi
}

count_of() { rg -a -c -F "$2" "$1" 2>/dev/null || echo 0; }
need_once() {
  local n="$1" l="$2"; shift 2
  for m in "$@"; do
    local c; c=$(count_of "$n" "$m")
    [[ "$c" == "1" ]] || die "[$l] marker count != 1 (got $c): $m"
  done
}
need_absent() {
  local n="$1" l="$2"; shift 2
  for m in "$@"; do
    rg -a -q -F "$m" "$n" && die "[$l] forbidden marker present: $m"
  done
  return 0
}
need_order() {
  local n="$1" l="$2" a="$3" b="$4" why="$5" la lb
  la=$(rg -a -n -F "$a" "$n" | head -1 | cut -d: -f1)
  lb=$(rg -a -n -F "$b" "$n" | head -1 | cut -d: -f1)
  [[ -n "$la" && -n "$lb" ]] || { die "[$l] ordering evidence missing ($a=$la $b=$lb)"; return; }
  (( la < lb )) || die "[$l] $why ($a@$la must precede $b@$lb)"
}

OBJCOPY=$(objcopy_tool) || die "no objcopy available"

# ── RUN_A: feature-off preservation ─────────────────────────────────────────────────
if (( ! fail )); then
  note "RUN_A: building feature-OFF AArch64 kernel and auditing its literals"
  cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" --no-default-features \
    -p yarm --bin kernel_boot >"$LOGDIR/build-off.log" 2>&1 \
    || die "feature-off build failed"
fi
if (( ! fail )); then
  OFF_BIN="$LOGDIR/kernel_off.bin"
  "$OBJCOPY" -O binary "$KELF" "$OFF_BIN" >/dev/null 2>&1 || die "objcopy of feature-off kernel failed"
  fresh_artifact "$OFF_BIN"
  # Oracle-only literals: the disposable task, its selector knob, the boot slot-5 write and
  # the future live-seal literal. The production NR16 syscall, the disposition publication and
  # the AArch64 post-lock consumer are UNCONDITIONAL and are deliberately NOT audited here.
  OFF_HITS=0
  for lit in "EXIT_TASK_USER_ENTERED role=disposable arch=aarch64" \
             "EXIT_TASK_SYSTEM_HEALTH_OK arch=aarch64" \
             "EXIT_TASK_SYSCALL_RETURNED arch=aarch64" \
             "EXIT_TASK_ORACLE_SPAWNED arch=aarch64" \
             "EXIT_TASK_SURVIVOR_PROGRESS_OK arch=aarch64" \
             "EXIT_TASK_ORACLE_SLOTS arch=aarch64" \
             "yarm.aarch64_exit_current_task_oracle" \
             "YARM_AARCH64_EXIT_CURRENT_TASK_ORACLE_SET" \
             "$SEAL"; do
    if rg -a -q -F "$lit" "$OFF_BIN"; then die "feature-off image contains $lit"; OFF_HITS=$((OFF_HITS+1)); fi
  done
  note "RUN_A feature-off oracle literals=$OFF_HITS"
  recheck_exact_commit RUN_A
fi

# ── RUN_B: feature-on live exit cell ────────────────────────────────────────────────
if (( ! fail )); then
  note "RUN_B: building feature-ON servers + initramfs"
  BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $FEATURE" \
    scripts/build-qemu-aarch64-artifacts.sh >"$LOGDIR/build-on.log" 2>&1 \
    || die "feature-on artifact build failed"
fi
if (( ! fail )); then
  note "RUN_B: building feature-ON kernel"
  cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" \
    --no-default-features --features "$FEATURE" \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild-on.log" 2>&1 \
    || die "feature-on kernel build failed"
  "$OBJCOPY" -O binary "$KELF" "$KBIN" >/dev/null 2>&1 || die "objcopy of feature-on kernel failed"
  fresh_artifact "$KBIN"
  fresh_artifact build-aarch64/initramfs-core.cpio
fi

# Feature forwarding proof: the feature must have reached BOTH the kernel and the userspace
# image, not just the kernel. The kernel carries the boot-side literals; the initramfs carries
# the disposable task's own marker.
if (( ! fail )); then
  rg -a -q -F "EXIT_TASK_ORACLE_SLOTS arch=aarch64" "$KBIN" \
    || die "feature-on kernel missing the slot-5 provisioning literal (feature not forwarded)"
  rg -a -q -F "yarm.aarch64_exit_current_task_oracle" "$KBIN" \
    || die "feature-on kernel missing the boot knob literal"
  rg -a -q -F "EXIT_TASK_USER_ENTERED role=disposable arch=aarch64" build-aarch64/initramfs-core.cpio \
    || die "initramfs missing the disposable task literal (feature not forwarded to userspace)"
  # Cross-arch hygiene: an AArch64 image must not carry another architecture's exit cell.
  rg -a -q -F "EXIT_TASK_USER_ENTERED role=disposable arch=x86_64" build-aarch64/initramfs-core.cpio \
    && die "x86_64 exit-oracle literal present in the AArch64 initramfs"
  recheck_exact_commit RUN_B_BUILD
fi

RAW="$LOGDIR/exit.raw.log"; CORE="$LOGDIR/exit.core.log"; NORM="$LOGDIR/exit.norm.log"
if (( ! fail )); then
  fresh_log "$RAW" && fresh_log "$CORE" && fresh_log "$NORM" || true
fi

if (( ! fail )); then
  note "RUN_B: one fresh -smp 1 boot with the AArch64 exit oracle armed"
  env KERNEL_IMAGE="$KBIN" \
      INITRAMFS_IMAGE=build-aarch64/initramfs-core.cpio \
      KERNEL_CMDLINE="yarm.aarch64_exit_current_task_oracle=1" \
      QEMU_SMP=1 QEMU_SINGLE_BOOT=1 QEMU_SMOKE_STRICT=0 \
      LOGFILE="$RAW" \
      TIMEOUT_SECS="$TIMEOUT_SECS" IDLE_MAX_SECS="$IDLE_MAX_SECS" \
      scripts/qemu-aarch64-core-smoke.sh >"$LOGDIR/wrap.log" 2>&1 || true
  grep -a "\[info\] qemu command:" "$LOGDIR/wrap.log" >"$CORE" 2>/dev/null || true
  tr '\r' '\n' <"$RAW" >"$NORM" 2>/dev/null || true
  [[ -s "$NORM" ]] || die "RUN_B produced no boot log"
fi

if (( ! fail )); then
  L=RUN_B
  # Hard-fail markers first: an early exit or a returned syscall invalidates everything.
  need_absent "$NORM" "$L" \
    "EXIT_TASK_SYSCALL_RETURNED" \
    "EXIT_TASK_OLD_FRAME_RESTORED" \
    "EXIT_TASK_EXITING_STILL_CURRENT" \
    "EXIT_TASK_WRONG_IDENTITY" \
    "EXIT_TASK_DUPLICATE_DISPOSITION" \
    "EXIT_TASK_TRAP_DEPTH_ERROR" \
    "EXIT_TASK_RESELECTED_EXITING_TASK" \
    "EXIT_TASK_ORACLE_SPAWN_FAIL" \
    "KERNEL PANIC" "RUST PANIC" "panicked at" "SYNCHRONOUS EXCEPTION" "Unhandled" "FATAL"

  need_once "$NORM" "$L" \
    "EXIT_TASK_USER_ENTERED role=disposable arch=aarch64" \
    "EXIT_TASK_PREFLIGHT_OK" \
    "EXIT_TASK_LIFECYCLE_TRANSITION" \
    "EXIT_TASK_DISPOSITION_PUBLISHED" \
    "EXIT_TASK_INLOCK_BYPASS_ARMED arch=aarch64" \
    "EXIT_TASK_BROAD_LOCK_RELEASED arch=aarch64" \
    "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=aarch64" \
    "EXIT_TASK_DISPOSITION_CONSUMED arch=aarch64" \
    "EXIT_TASK_EXITING_NOT_CURRENT arch=aarch64" \
    "EXIT_TASK_ABSENCE_VALIDATED arch=aarch64" \
    "EXIT_TASK_TRAP_DEPTH_OWNER arch=aarch64" \
    "EXIT_TASK_SURVIVOR_PROGRESS_OK arch=aarch64" \
    "EXIT_TASK_SYSTEM_HEALTH_OK arch=aarch64"
  [[ "$(count_of "$NORM" "EXIT_TASK_SYSCALL_DISPATCHED nr=16")" == "1" ]] \
    || die "[$L] NR16 dispatches != 1"
  [[ "$(count_of "$NORM" "EXIT_TASK_RESTORE_OWNER arch=aarch64")" == "1" ]] \
    || die "[$L] restore-owner attestations != 1"
  # ── lock-state correctness, marker by marker (Stage 200D-0C2) ─────────────────────
  # Every marker emitted from the post-lock section must STATE the lock condition that held
  # where it was emitted, not merely be positioned after the boundary. Stage 200D-0B3 removed
  # markers that named a lock state they did not have; this is the AArch64 sibling check, and
  # it is a whole-line assertion so a marker cannot pass on its name alone.
  for m in "EXIT_TASK_BROAD_LOCK_RELEASED arch=aarch64" \
           "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=aarch64" \
           "EXIT_TASK_DISPOSITION_CONSUMED arch=aarch64"; do
    rg -a -F "$m" "$NORM" | rg -a -q -F "broad_lock=0" \
      || die "[$L] post-lock marker does not state broad_lock=0: $m"
    rg -a -F "$m" "$NORM" | rg -a -q -F "broad_lock=1" \
      && die "[$L] post-lock marker falsely claims broad_lock=1: $m"
  done
  # The in-lock bypass decision is the one AArch64 exit marker emitted UNDER the broad lock,
  # and it must not claim otherwise.
  rg -a -F "EXIT_TASK_INLOCK_BYPASS_ARMED arch=aarch64" "$NORM" | rg -a -q -F "broad_lock=0" \
    && die "[$L] the in-lock bypass marker falsely claims broad_lock=0"
  # The release marker must name the guard it outlived.
  rg -a -F "EXIT_TASK_BROAD_LOCK_RELEASED arch=aarch64" "$NORM" | rg -a -q -F "holder=with_cpu" \
    || die "[$L] release marker does not name the broad-lock holder"
  # The drain marker must attest that ALL drains ran, not merely that a drain point exists.
  rg -a -F "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=aarch64" "$NORM" | rg -a -q -F "drains=all" \
    || die "[$L] post-lock drain marker does not attest drains=all"

  # Trap-depth ownership: AArch64 has no software depth counter, so the consumer must attest
  # exactly zero clears. A non-zero count would mean a second cleanup owner appeared.
  [[ "$(count_of "$NORM" "EXIT_TASK_TRAP_DEPTH_OWNER arch=aarch64 cpu=0 owner=hardware_eret clears=0")" == "1" ]] \
    || die "[$L] trap-depth ownership is not the single hardware-ERET owner with zero clears"

  # Semantic ordering of the whole chain.
  need_order "$NORM" "$L" "EXIT_TASK_USER_ENTERED" "EXIT_TASK_SYSCALL_DISPATCHED nr=16" \
    "userspace entry precedes the syscall"
  need_order "$NORM" "$L" "EXIT_TASK_SYSCALL_DISPATCHED nr=16" "EXIT_TASK_PREFLIGHT_OK" \
    "dispatch precedes preflight"
  need_order "$NORM" "$L" "EXIT_TASK_PREFLIGHT_OK" "EXIT_TASK_LIFECYCLE_TRANSITION" \
    "preflight precedes the lifecycle transition"
  need_order "$NORM" "$L" "EXIT_TASK_LIFECYCLE_TRANSITION" "EXIT_TASK_DISPOSITION_PUBLISHED" \
    "teardown precedes the disposition"
  need_order "$NORM" "$L" "EXIT_TASK_DISPOSITION_PUBLISHED" "EXIT_TASK_INLOCK_BYPASS_ARMED arch=aarch64" \
    "the in-lock bypass is armed from the published disposition"
  need_order "$NORM" "$L" "EXIT_TASK_INLOCK_BYPASS_ARMED arch=aarch64" "EXIT_TASK_BROAD_LOCK_RELEASED arch=aarch64" \
    "the bypass is decided under the lock, the release attested after"
  need_order "$NORM" "$L" "EXIT_TASK_BROAD_LOCK_RELEASED arch=aarch64" "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=aarch64" \
    "the consumer runs after the broad lock is released"
  need_order "$NORM" "$L" "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=aarch64" "EXIT_TASK_DISPOSITION_CONSUMED arch=aarch64" \
    "the consumer runs after every post-lock drain"
  need_order "$NORM" "$L" "EXIT_TASK_DISPOSITION_CONSUMED arch=aarch64" "EXIT_TASK_EXITING_NOT_CURRENT arch=aarch64" \
    "identity is validated after consumption"
  need_order "$NORM" "$L" "EXIT_TASK_EXITING_NOT_CURRENT arch=aarch64" "EXIT_TASK_ABSENCE_VALIDATED arch=aarch64" \
    "full-identity absence follows the not-current check"
  need_order "$NORM" "$L" "EXIT_TASK_ABSENCE_VALIDATED arch=aarch64" "EXIT_TASK_RESTORE_OWNER arch=aarch64" \
    "the restore owner is named after absence is established"
  need_order "$NORM" "$L" "EXIT_TASK_RESTORE_OWNER arch=aarch64" "EXIT_TASK_SURVIVOR_PROGRESS_OK arch=aarch64" \
    "a surviving task makes progress after the exit"
  need_order "$NORM" "$L" "EXIT_TASK_SURVIVOR_PROGRESS_OK arch=aarch64" "EXIT_TASK_SYSTEM_HEALTH_OK arch=aarch64" \
    "terminal health is the last thing proven"

  # Single boot instance: one QEMU launch, one firmware banner, one kernel entry, one boot
  # completion and exactly one distinct boot nonce.
  launches=$(count_of "$CORE" "[info] qemu command:")
  entry=$(count_of "$NORM" "YARM_BOOT_CMDLINE_CAPTURE arch=aarch64")
  bootok=$(count_of "$NORM" "YARM_BOOT_OK present_cpus=")
  nonces=$(rg -a -o "YARM_BOOT_INSTANCE arch=aarch64 nonce=0x[0-9a-f]+" "$NORM" 2>/dev/null | sort -u | wc -l | tr -d ' ')
  [[ "$launches" == "1" ]] || die "[$L] QEMU launches != 1 (got $launches)"
  [[ "$entry"    == "1" ]] || die "[$L] kernel entries != 1 (got $entry)"
  [[ "$bootok"   == "1" ]] || die "[$L] boot completions != 1 (got $bootok)"
  [[ "$nonces"   == "1" ]] || die "[$L] distinct boot nonces != 1 (got $nonces)"

  recheck_exact_commit RUN_B
fi

if (( fail )); then
  echo "$SEAL arch=aarch64 live_cells=0 result=fail"
  exit 1
fi

echo "$SEAL arch=aarch64 sha=$SHA0 tree=$TREE0 live_cells=1 boots=1 nr16_dispatches=1 \
consumer_after_broad_lock_release=1 consumer_after_post_lock_drain=1 consumer_before_arch_restore=1 \
old_frame_restores=0 syscall_returns=0 trap_depth_clears=0 feature_off_oracle_literals=0 result=ok"
