#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 200D-0B3 — x86_64 LIVE ExitCurrentTask (NR 16) exact-commit runner (CORRECTED).
#
# Supersedes the Stage 200D-0B1 preparation and the Stage 200D-0B2 live seal, both of which
# accepted two FALSE ordering claims: `EXIT_TASK_BROAD_LOCK_RELEASED` and
# `EXIT_TASK_POST_LOCK_DRAIN_DONE` were emitted from inside `SharedKernel::with_cpu`, where the
# broad `SpinLock<KernelState>` was still held and no post-lock drain had run.
#
# This runner enforces the REAL x86_64 pipeline. Each marker now carries the lock state that
# actually held where it was emitted, and the runner rejects the old sequence outright:
#
#   in-lock   (broad_lock=1)  DISPOSITION_CONSUMED, EXITING_NOT_CURRENT, ABSENCE_VALIDATED,
#                             RESTORE_OWNER_PREPARED
#   post-lock (broad_lock=0)  BROAD_LOCK_RELEASED (first statement after with_cpu returns),
#                             POST_LOCK_DRAIN_DONE (after every shared drain completes)
#   epilogue  (broad_lock=0)  COMMON_EPILOGUE_OWNER (after the real iret-frame commit and the
#                             single depth clear, before the iretq/sysretq to ring 3)
#
# It proves that a disposable userspace task invoking NR 16 never returns, that the exiting
# frame is never restored, that the actual user return follows the drains, and that the system
# continues to terminal health afterwards.
#
#   RUN_A  feature-off preservation — the image must carry NO oracle literal
#   RUN_B  feature-on live cell     — one boot, one disposable exit, system stays healthy
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/x86-exit-current-task}
TIMEOUT_SECS=${TIMEOUT_SECS:-120}
KTARGET=targets/x86_64-yarm-none.json
KPROFILE=x86-none
KELF=target/x86_64-yarm-none/${KPROFILE}/kernel_boot
BUILD_STD=core,alloc,compiler_builtins,panic_abort
FEATURE=x86-exit-current-task-oracle

fail=0
note() { echo "[x86-exit] $*"; }
die()  { echo "[x86-exit][fail] $*"; fail=1; }

# ── exact-commit identity ────────────────────────────────────────────────────────────
if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "STAGE_200D0B3_X86_EXIT_CURRENT_TASK_REFREEZE_SEAL result=fail reason=dirty_tree"
  exit 1
fi
SHA0=$(git rev-parse HEAD)
TREE0=$(git rev-parse HEAD^{tree})
note "exact commit sha=$SHA0 tree=$TREE0 (clean)"

recheck_exact_commit() {
  local what="$1"
  [[ "$(git rev-parse HEAD)" == "$SHA0" ]]      || die "[$what] SHA drifted"
  [[ "$(git rev-parse HEAD^{tree})" == "$TREE0" ]] || die "[$what] tree hash drifted"
  git diff --quiet && git diff --cached --quiet || die "[$what] tree became dirty"
}

rm -rf "$LOGDIR"; mkdir -p "$LOGDIR"

declare -A LOG_SEEN=()
fresh_log() {
  local p="$1"
  [[ -n "${LOG_SEEN[$p]:-}" ]] && { die "log reuse: $p already used"; return 1; }
  [[ -e "$p" ]] && { die "log reuse: $p already exists"; return 1; }
  LOG_SEEN["$p"]=1; return 0
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

# ── RUN_A: feature-off preservation ─────────────────────────────────────────────────
note "RUN_A: building feature-OFF x86_64 kernel and auditing its literals"
cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
  --target "$KTARGET" --profile "$KPROFILE" --no-default-features \
  -p yarm --bin kernel_boot >"$LOGDIR/build-off.log" 2>&1 \
  || die "feature-off build failed"
OFF_BIN="$LOGDIR/kernel_off.bin"
cp "$KELF" "$OFF_BIN" 2>/dev/null || die "no feature-off image"
OFF_HITS=0
for lit in "EXIT_TASK_USER_ENTERED" "EXIT_TASK_SYSTEM_HEALTH_OK" \
           "EXIT_TASK_SYSCALL_RETURNED" "EXIT_TASK_ORACLE_SPAWNED" \
           "EXIT_TASK_SURVIVOR_PROGRESS_OK" "yarm.x86_64_exit_current_task_oracle" \
           "STAGE_200D0B3_X86_EXIT_CURRENT_TASK_REFREEZE_SEAL"; do
  if rg -a -q -F "$lit" "$OFF_BIN"; then die "feature-off image contains $lit"; OFF_HITS=$((OFF_HITS+1)); fi
done
note "RUN_A feature-off oracle literals=$OFF_HITS"
recheck_exact_commit RUN_A

# ── RUN_B: feature-on live exit cell ────────────────────────────────────────────────
note "RUN_B: building feature-ON artifacts"
BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $FEATURE" \
  scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/build-on.log" 2>&1 \
  || die "feature-on artifact build failed"
cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
  --target "$KTARGET" --profile "$KPROFILE" --no-default-features --features "$FEATURE" \
  -p yarm --bin kernel_boot >"$LOGDIR/kbuild-on.log" 2>&1 \
  || die "feature-on kernel build failed"
cp "$KELF" build-x86_64/kernel_boot.elf 2>/dev/null || die "could not stage the feature-on image"

RAW="$LOGDIR/exit.raw.log"; CORE="$LOGDIR/exit.core.log"; NORM="$LOGDIR/exit.norm.log"
fresh_log "$RAW" && fresh_log "$CORE" && fresh_log "$NORM" || true

if (( ! fail )); then
  note "RUN_B: one fresh -smp 1 boot with the exit oracle armed"
  env KERNEL_IMAGE=build-x86_64/kernel_boot.elf \
      INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio \
      KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.x86_64_exit_current_task_oracle=1" \
      QEMU_SMP=1 QEMU_SINGLE_BOOT=1 LOGFILE="$RAW" SMOKE_LOG="$CORE" \
      TIMEOUT_SECS="$TIMEOUT_SECS" YARM_MODE_ISOLATION=0 \
      scripts/qemu-x86_64-core-smoke.sh >"$LOGDIR/wrap.log" 2>&1 || true
  grep -a "\[info\] qemu command:" "$LOGDIR/wrap.log" >"$CORE" 2>/dev/null || true
  tr '\r' '\n' <"$RAW" >"$NORM" 2>/dev/null || true
  [[ -s "$NORM" ]] || die "RUN_B produced no boot log"
fi

if (( ! fail )); then
  L=RUN_B
  # Hard-fail markers first: an early QEMU exit or a returned syscall invalidates everything.
  need_absent "$NORM" "$L" \
    "EXIT_TASK_SYSCALL_RETURNED" \
    "EXIT_TASK_OLD_FRAME_RESTORED" \
    "EXIT_TASK_EXITING_STILL_CURRENT" \
    "EXIT_TASK_WRONG_IDENTITY" \
    "EXIT_TASK_DUPLICATE_DISPOSITION" \
    "EXIT_TASK_TRAP_DEPTH_ERROR" \
    "EXIT_TASK_RESELECTED_EXITING_TASK" \
    "KERNEL PANIC" "RUST PANIC" "panicked at" "FATAL"

  # The old FALSE markers must not appear at all. Their absence is what makes this a
  # correction rather than a rewording: an image still emitting either of them is emitting a
  # claim about the broad lock that was not true where it was made.
  need_absent "$NORM" "$L" \
    "EXIT_TASK_BROAD_LOCK_RELEASED arch=x86_64 cpu=0 result=ok" \
    "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=x86_64 cpu=0 broad_lock=0 result=ok" \
    "EXIT_TASK_TRAP_DEPTH_OWNER arch=x86_64" \
    "EXIT_TASK_RESTORE_OWNER arch=x86_64 owner=replacement exiting_tid"

  need_once "$NORM" "$L" \
    "EXIT_TASK_USER_ENTERED role=disposable arch=x86_64" \
    "EXIT_TASK_PREFLIGHT_OK" \
    "EXIT_TASK_LIFECYCLE_TRANSITION" \
    "EXIT_TASK_DISPOSITION_PUBLISHED" \
    "EXIT_TASK_DISPOSITION_CONSUMED arch=x86_64" \
    "EXIT_TASK_EXITING_NOT_CURRENT arch=x86_64" \
    "EXIT_TASK_ABSENCE_VALIDATED arch=x86_64" \
    "EXIT_TASK_RESTORE_OWNER_PREPARED arch=x86_64" \
    "EXIT_TASK_BROAD_LOCK_RELEASED arch=x86_64" \
    "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=x86_64" \
    "EXIT_TASK_COMMON_EPILOGUE_OWNER arch=x86_64" \
    "EXIT_TASK_SURVIVOR_PROGRESS_OK"
  [[ "$(count_of "$NORM" "EXIT_TASK_SYSCALL_DISPATCHED nr=16")" == "1" ]] \
    || die "[$L] NR16 dispatches != 1"

  # ── lock-state correctness, marker by marker ──────────────────────────────────────
  # Every in-lock marker must say broad_lock=1 and every post-lock marker broad_lock=0. A
  # marker carrying the wrong value is the exact defect this stage corrects, so each is
  # checked as a WHOLE line rather than by name.
  for m in "EXIT_TASK_DISPOSITION_CONSUMED arch=x86_64" \
           "EXIT_TASK_EXITING_NOT_CURRENT arch=x86_64" \
           "EXIT_TASK_ABSENCE_VALIDATED arch=x86_64" \
           "EXIT_TASK_RESTORE_OWNER_PREPARED arch=x86_64"; do
    rg -a -F "$m" "$NORM" | rg -a -q -F "broad_lock=1" \
      || die "[$L] in-lock marker does not state broad_lock=1: $m"
    rg -a -F "$m" "$NORM" | rg -a -q -F "broad_lock=0" \
      && die "[$L] in-lock marker falsely claims broad_lock=0: $m"
  done
  for m in "EXIT_TASK_BROAD_LOCK_RELEASED arch=x86_64" \
           "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=x86_64" \
           "EXIT_TASK_COMMON_EPILOGUE_OWNER arch=x86_64"; do
    rg -a -F "$m" "$NORM" | rg -a -q -F "broad_lock=0" \
      || die "[$L] post-lock marker does not state broad_lock=0: $m"
  done
  # The drain marker must attest that ALL drains ran, not merely that a drain point exists.
  rg -a -q -F "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=x86_64" "$NORM" \
    && rg -a -F "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=x86_64" "$NORM" | rg -a -q -F "drains=all" \
    || die "[$L] post-lock drain marker does not attest drains=all"
  # Exactly one prepared owner, and it is a replacement or idle — never the exiting task.
  [[ "$(count_of "$NORM" "EXIT_TASK_RESTORE_OWNER_PREPARED arch=x86_64")" == "1" ]] \
    || die "[$L] prepared restore-owner attestations != 1"
  rg -a -F "EXIT_TASK_RESTORE_OWNER_PREPARED arch=x86_64" "$NORM" \
    | rg -a -q -e "owner=replacement" -e "owner=idle" \
    || die "[$L] prepared restore owner is neither replacement nor idle"
  # The epilogue owns exactly one depth clear.
  [[ "$(count_of "$NORM" "EXIT_TASK_COMMON_EPILOGUE_OWNER arch=x86_64")" == "1" ]] \
    || die "[$L] common-epilogue ownership attestations != 1"
  rg -a -F "EXIT_TASK_COMMON_EPILOGUE_OWNER arch=x86_64" "$NORM" | rg -a -q -F "clears=1" \
    || die "[$L] common-epilogue marker does not attest clears=1"
  # The user return actually happened after the drains: the epilogue marker only fires once
  # the iret frame has been committed on a path reached after the shared handler returned.
  rg -a -F "EXIT_TASK_COMMON_EPILOGUE_OWNER arch=x86_64" "$NORM" | rg -a -q -F "frame_committed=1" \
    || die "[$L] common-epilogue marker does not attest the real frame commit"

  # ── corrected semantic ordering of the whole chain ────────────────────────────────
  #
  # DOCUMENTED DEVIATION from the proposed Stage 200D-0B3 list: ABSENCE_VALIDATED is required
  # BEFORE the lock-release marker, not after the epilogue marker. Absence is identity work and
  # is performed by the in-lock consumer, alongside the not-current check it belongs with.
  # Emitting it from the epilogue would attest the exiting task's absence AFTER the replacement
  # frame had already been committed — validating a precondition after acting on it. The source
  # was not adjusted to fit the list; the list is adjusted to the real source path.
  need_order "$NORM" "$L" "EXIT_TASK_USER_ENTERED" "EXIT_TASK_SYSCALL_DISPATCHED nr=16" \
    "userspace entry precedes the syscall"
  need_order "$NORM" "$L" "EXIT_TASK_SYSCALL_DISPATCHED nr=16" "EXIT_TASK_PREFLIGHT_OK" \
    "dispatch precedes preflight"
  need_order "$NORM" "$L" "EXIT_TASK_PREFLIGHT_OK" "EXIT_TASK_LIFECYCLE_TRANSITION" \
    "preflight precedes the lifecycle transition"
  need_order "$NORM" "$L" "EXIT_TASK_LIFECYCLE_TRANSITION" "EXIT_TASK_DISPOSITION_PUBLISHED" \
    "teardown precedes the disposition"
  need_order "$NORM" "$L" "EXIT_TASK_DISPOSITION_PUBLISHED" "EXIT_TASK_DISPOSITION_CONSUMED arch=x86_64" \
    "the disposition is consumed after it is published, both under the broad lock"
  need_order "$NORM" "$L" "EXIT_TASK_DISPOSITION_CONSUMED arch=x86_64" "EXIT_TASK_EXITING_NOT_CURRENT arch=x86_64" \
    "identity is validated after consumption"
  need_order "$NORM" "$L" "EXIT_TASK_EXITING_NOT_CURRENT arch=x86_64" "EXIT_TASK_ABSENCE_VALIDATED arch=x86_64" \
    "full-identity absence follows the not-current check"
  need_order "$NORM" "$L" "EXIT_TASK_ABSENCE_VALIDATED arch=x86_64" "EXIT_TASK_RESTORE_OWNER_PREPARED arch=x86_64" \
    "the restore owner is prepared after absence is established"
  need_order "$NORM" "$L" "EXIT_TASK_RESTORE_OWNER_PREPARED arch=x86_64" "EXIT_TASK_BROAD_LOCK_RELEASED arch=x86_64" \
    "owner preparation happens under the lock, the release is attested after"
  need_order "$NORM" "$L" "EXIT_TASK_BROAD_LOCK_RELEASED arch=x86_64" "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=x86_64" \
    "the post-lock drains run after the broad lock is actually released"
  need_order "$NORM" "$L" "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=x86_64" "EXIT_TASK_COMMON_EPILOGUE_OWNER arch=x86_64" \
    "the epilogue commits the user frame only after every drain completed"
  need_order "$NORM" "$L" "EXIT_TASK_COMMON_EPILOGUE_OWNER arch=x86_64" "EXIT_TASK_SURVIVOR_PROGRESS_OK" \
    "a surviving task makes progress after the exit"
  need_order "$NORM" "$L" "EXIT_TASK_SURVIVOR_PROGRESS_OK" "EXIT_TASK_SYSTEM_HEALTH_OK" \
    "terminal health is the last thing proven"

  # Single boot instance.
  launches=$(count_of "$CORE" "[info] qemu command:")
  banner=$(count_of "$NORM" "Booting from ROM")
  entry=$(count_of "$NORM" "YARM_BOOT_CMDLINE_CAPTURE arch=x86_64")
  bootok=$(count_of "$NORM" "YARM_BOOT_OK present_cpus=")
  nonces=$(rg -a -o "YARM_BOOT_INSTANCE arch=x86_64 nonce=0x[0-9a-f]+" "$NORM" 2>/dev/null | sort -u | wc -l | tr -d ' ')
  note "single-boot: qemu=$launches banner=$banner entry=$entry boot_ok=$bootok distinct_nonces=$nonces"
  [[ "$launches" == "1" ]] || die "[$L] QEMU launches != 1"
  [[ "$banner"   == "1" ]] || die "[$L] firmware banners != 1"
  [[ "$entry"    == "1" ]] || die "[$L] kernel entries != 1"
  [[ "$bootok"   == "1" ]] || die "[$L] boot completions != 1"
  [[ "$nonces"   == "1" ]] || die "[$L] DISTINCT boot instances != 1"

  # Terminal health LAST: a run that died early cannot pass, and QEMU is only
  # terminated by the core runner after this marker appears.
  if ! rg -a -q -F "EXIT_TASK_SYSTEM_HEALTH_OK" "$NORM"; then
    die "[$L] qemu exited before terminal proof (no EXIT_TASK_SYSTEM_HEALTH_OK)"
  fi
  recheck_exact_commit RUN_B
fi

if (( fail )); then
  echo "STAGE_200D0B3_X86_EXIT_CURRENT_TASK_REFREEZE_SEAL arch=x86_64 result=fail"
  exit 1
fi

cat <<SEAL
STAGE_200D0B3_X86_EXIT_CURRENT_TASK_REFREEZE_SEAL
arch=x86_64
syscall_nr=16
user_entries=1
accepted_exits=1
dispositions_published=1
dispositions_consumed=1
consumer_inside_broad_lock=1
consumer_side_effects_under_broad_lock=0
broad_lock_release_after_consumer=1
post_lock_drains_after_release=1
actual_user_returns_after_drains=1
normal_result_writes_to_old_frame=0
old_frame_restores=0
syscall_returns_after_accept=0
trap_depth_errors=0
wrong_current_task=0
exiting_task_reselections=0
replacement_or_idle=1
absence_validations=1
system_health_completions=1
duplicate_dispositions=0
single_boot_failures=0
false_ordering_markers_remaining=0
exact_commit=${SHA0}
exact_tree=${TREE0}
result=ok
SEAL
