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

  # ── U9-EXIT1 §6 — THE terminal-edge retirement ────────────────────────────────────
  #
  # Stage 200D-0B3 proved the BROAD pipeline: NR 16 reached the terminal broad-locked
  # dispatcher, published a typed `CurrentTaskExited` disposition, and the in-lock consumer got
  # the exiting task off the CPU before the epilogue committed a replacement frame. U9-EXIT1
  # retires that edge, so every one of those markers is now REQUIRED ABSENT rather than reworded:
  # a run still emitting them is a run whose exit still reached the acquisition this stage exists
  # to remove, and no re-phrasing of the acceptance would make that a pass.
  #
  # Each retired assertion is replaced by the one that states the admission it used to forbid,
  # below — the same discipline U9-REAP1 applied to its 21 NR-31 guards.
  broad_enters=$(count_of "$NORM" "EXIT_TASK_BROAD_ENTER")
  split_enters=$(count_of "$NORM" "EXIT_TASK_SPLIT_ENTER")
  split_declines=$(count_of "$NORM" "EXIT_TASK_SPLIT_DECLINED")
  claims=$(count_of "$NORM" "EXIT_TASK_CLAIM_RETIRED")
  dispatches=$(count_of "$NORM" "EXIT_TASK_SYSCALL_DISPATCHED nr=16")
  note "terminal edge: broad=$broad_enters split=$split_enters declined=$split_declines claims=$claims dispatches=$dispatches"
  [[ "$broad_enters" == "0" ]] \
    || die "[$L] NR 16 still reached the terminal broad dispatcher ($broad_enters entries)"
  [[ "$split_enters" == "1" ]] \
    || die "[$L] split-route entries != 1 (got $split_enters)"
  [[ "$split_declines" == "0" ]] \
    || die "[$L] the split route declined an admitted exit ($split_declines)"
  [[ "$claims" == "1" ]] \
    || die "[$L] exit claims retired != 1 (got $claims)"
  [[ "$dispatches" == "1" ]] || die "[$L] NR16 dispatches != 1 (got $dispatches)"

  # The retired broad pipeline, marker for marker. Its ABSENCE is the retirement.
  need_absent "$NORM" "$L" \
    "EXIT_TASK_PREFLIGHT_OK" \
    "EXIT_TASK_DISPOSITION_PUBLISHED" \
    "EXIT_TASK_DISPOSITION_CONSUMED arch=x86_64" \
    "EXIT_TASK_EXITING_NOT_CURRENT arch=x86_64" \
    "EXIT_TASK_ABSENCE_VALIDATED arch=x86_64" \
    "EXIT_TASK_RESTORE_OWNER_PREPARED arch=x86_64" \
    "EXIT_TASK_BROAD_LOCK_RELEASED arch=x86_64" \
    "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=x86_64" \
    "EXIT_TASK_COMMON_EPILOGUE_OWNER arch=x86_64" \
    "EXIT_TASK_SPLIT_FAILED_CLOSED" \
    "EXIT_TASK_DUPLICATE_DEFERRAL"

  need_once "$NORM" "$L" \
    "EXIT_TASK_USER_ENTERED role=disposable arch=x86_64" \
    "EXIT_TASK_SPLIT_ENTER" \
    "EXIT_TASK_CLAIM_RETIRED" \
    "EXIT_TASK_LIFECYCLE_TRANSITION" \
    "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=exit_current_task_switch_required" \
    "YARM_LOCK_SPLIT_DISPATCH arch=x86_64 nr=16 cpu=0 result=queue_advance_committed" \
    "EXIT_TASK_SURVIVOR_PROGRESS_OK"

  # ── the exiting task's identity, read from the log rather than assumed ─────────────
  EXITING_TID=$(rg -a -o -r '$1' "EXIT_TASK_SYSCALL_DISPATCHED nr=16 tid=([0-9]+)" "$NORM" | head -1)
  [[ -n "$EXITING_TID" ]] || die "[$L] could not resolve the exiting tid"
  note "exiting tid=$EXITING_TID"
  # The claim retired is the SAME incarnation the syscall dispatched, not merely some exit.
  rg -a -q -F "EXIT_TASK_CLAIM_RETIRED tid=$EXITING_TID " "$NORM" \
    || die "[$L] the retired claim does not name the dispatched tid"
  rg -a -q -F "EXIT_TASK_SPLIT_ENTER tid=$EXITING_TID " "$NORM" \
    || die "[$L] the split edge does not name the dispatched tid"

  # ── the exiting frame is never saved, and never returned through ───────────────────
  # `captured=0` is the live form of §3's "do not save or later restore the exiting frame":
  # the shared bridge skips the outgoing capture when this CPU carries an exit deferral.
  rg -a -F "YARM_LOCK_SPLIT_DISPATCH arch=x86_64 nr=16 cpu=0 result=queue_advance_committed" "$NORM" \
    | rg -a -q -F "captured=0" \
    || die "[$L] the exiting frame was captured — §3 forbids saving it"
  rg -a -F "YARM_LOCK_SPLIT_DISPATCH arch=x86_64 nr=16 cpu=0 result=queue_advance_committed" "$NORM" \
    | rg -a -q -F "outgoing=$EXITING_TID" \
    || die "[$L] the committed advance names a different outgoing task"

  # ── the ONE queue advance, checked in the slice that belongs to this exit ──────────
  #
  # The drain's own markers are SHARED with FutexWait and blocking recv, which fire hundreds of
  # times before the exit. A whole-log ordering claim about them would be satisfied by somebody
  # else's advance, so every claim below is made against the slice of the boot log that begins at
  # the accepted exit.
  SLICE="$LOGDIR/exit.after.log"
  awk 'index($0,"EXIT_TASK_SYSCALL_DISPATCHED nr=16"){f=1} f' "$NORM" >"$SLICE"
  [[ -s "$SLICE" ]] || die "[$L] could not slice the log at the accepted exit"

  need_order "$SLICE" "$L" "EXIT_TASK_SYSCALL_DISPATCHED nr=16" "EXIT_TASK_LIFECYCLE_TRANSITION" \
    "the dispatched exit precedes its lifecycle transition"
  need_order "$SLICE" "$L" "EXIT_TASK_LIFECYCLE_TRANSITION" \
    "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=exit_current_task_switch_required" \
    "the transition precedes the queue-advance deferral it commits"
  need_order "$SLICE" "$L" "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=exit_current_task_switch_required" \
    "YARM_LOCK_SPLIT_DISPATCH arch=x86_64 nr=16 cpu=0 result=queue_advance_committed" \
    "the deferral is published before the seam answers QueueAdvanceCommitted"
  need_order "$SLICE" "$L" "YARM_LOCK_SPLIT_DISPATCH arch=x86_64 nr=16 cpu=0 result=queue_advance_committed" \
    "QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED cpu=0 reason=publication_committed" \
    "the committed advance skips the broad dispatch exactly once"
  need_order "$SLICE" "$L" "QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED cpu=0 reason=publication_committed" \
    "QUEUE_ADVANCING_DISPATCH_BEGIN cpu=0" \
    "the EXISTING post-lock drain runs after the broad dispatch is skipped"
  need_order "$SLICE" "$L" "QUEUE_ADVANCING_DISPATCH_BEGIN cpu=0" \
    "QUEUE_ADVANCING_DISPATCH_CURRENT_SET_OK cpu=0" \
    "the drain selects and installs an incoming task"
  need_order "$SLICE" "$L" "QUEUE_ADVANCING_DISPATCH_CURRENT_SET_OK cpu=0" \
    "QUEUE_ADVANCING_DISPATCH_DONE result=ok" \
    "the drain completes"
  need_order "$SLICE" "$L" "QUEUE_ADVANCING_DISPATCH_DONE result=ok" "EXIT_TASK_SURVIVOR_PROGRESS_OK" \
    "a surviving task makes progress after the exit"
  need_order "$SLICE" "$L" "EXIT_TASK_SURVIVOR_PROGRESS_OK" "EXIT_TASK_SYSTEM_HEALTH_OK" \
    "terminal health is the last thing proven"
  need_order "$NORM" "$L" "EXIT_TASK_USER_ENTERED" "EXIT_TASK_SYSCALL_DISPATCHED nr=16" \
    "userspace entry precedes the syscall"
  need_order "$NORM" "$L" "EXIT_TASK_SPLIT_ENTER" "EXIT_TASK_SYSCALL_DISPATCHED nr=16" \
    "the split edge is counted at the route's entry"

  # ── the dead task is never reselected, and the advance lands on somebody else ──────
  RESUMED=$(rg -a -o -r '$1' "QUEUE_ADVANCING_DISPATCH_CURRENT_SET_OK cpu=0 tid=([0-9]+)" "$SLICE" | head -1)
  [[ -n "$RESUMED" ]] || die "[$L] the drain installed no incoming task"
  note "advance resumed tid=$RESUMED (exiting tid=$EXITING_TID)"
  [[ "$RESUMED" != "$EXITING_TID" ]] \
    || die "[$L] the drain reselected the EXITING task"
  rg -a -q -F "USER_CR3_PRE_IRET_CHECK tid=$EXITING_TID" "$SLICE" \
    && die "[$L] a dead task was returned to userspace"

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
terminal_broad_dispatcher_entries=0
split_route_entries=1
split_route_declines=0
exit_claims_retired=1
dispositions_published=0
dispositions_consumed=0
consumer_inside_broad_lock=0
queue_advance_deferrals_committed=1
exiting_frames_captured=0
broad_dispatch_skips_after_commit=1
post_lock_drain_selections=1
exiting_task_reselections=0
dead_task_user_returns=0
syscall_returns_after_accept=0
old_frame_restores=0
trap_depth_errors=0
wrong_current_task=0
survivor_progress=1
system_health_completions=1
duplicate_deferrals=0
split_failed_closed=0
single_boot_failures=0
retired_broad_pipeline_markers_remaining=0
exact_commit=${SHA0}
exact_tree=${TREE0}
result=ok
SEAL
