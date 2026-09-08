#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 200D-0D1 — RISC-V LIVE ExitCurrentTask (NR 16) exact-commit runner.
#
# PREPARED by Stage 200D-0D1; EXECUTED by Stage 200D-0D2. It is NOT run in the preparation
# stage — the preparation seal explicitly reports live_boots=0.
#
# It proves that a disposable U-mode task invoking NR 16 never returns, that the exiting task's
# saved context and `sepc` never reach `sret`, that the disposition is consumed AFTER the broad
# `SpinLock<KernelState>` is released and AFTER every Phase-3 drain, and that the system
# continues to terminal health afterwards.
#
# The RISC-V pipeline is NOT the x86_64 or AArch64 one:
#
#   Phase 1  pre-lock split dispatch (DebugLog / FutexWake / gated NR6-7) — NR16 never eligible
#   Phase 2  `shared.with_cpu` broad-lock phase: syscall dispatch, exit_task, disposition
#            published, then the Stage 200D-0D1 in-lock bypass suppresses the ecall result
#            export and (with no replacement) the canonical restore  [broad_lock=1]
#   ── with_cpu returns: broad guard dropped ──                       [broad_lock=0]
#   Phase 3  drain_dispatch_post_work, reply-timeout collector+drain, server-death drain,
#            foundation oracle, 196D/196E/196G switch drains, THEN the exit consumer
#   bridge   frame write-back + SATP + `sret`
#
#   RUN_A  feature-off preservation — the image must carry NO oracle literal
#   RUN_B  feature-on live cell     — one boot, one disposable exit, system stays healthy
#
# Fail-closed conditions, each independently covered by a static hosted test
# (`stage200d0d1_riscv_exit_prep`): dirty tree, SHA drift, tree-hash drift, reused log, reused
# artifact, missing feature forwarding, declared-but-unwritten selector, declared-but-unspawned
# entry, missing `broad_lock=0`, consumer before drains, old-frame or `sepc` restoration, early
# QEMU exit, multiple boot instances, missing terminal health.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/riscv-exit-current-task}
TIMEOUT_SECS=${TIMEOUT_SECS:-180}
IDLE_MAX_SECS=${IDLE_MAX_SECS:-180}
KTARGET=${KTARGET:-riscv64gc-unknown-none-elf}
KPROFILE=${KPROFILE:-release}
KELF=${KELF:-target/riscv64gc-unknown-none-elf/${KPROFILE}/kernel_boot}
KBIN=${KBIN:-build-riscv64/yarm-riscv64.bin}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
FEATURE=riscv-exit-current-task-oracle
SEAL=STAGE_200D0D2_RISCV_EXIT_CURRENT_TASK_LIVE_SEAL

fail=0
note() { echo "[riscv-exit] $*"; }
die()  { echo "[riscv-exit][fail] $*"; fail=1; }

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
  note "RUN_A: building feature-OFF RISC-V kernel and auditing its literals"
  cargo build -Z "build-std=${BUILD_STD}" \
    --target "$KTARGET" --profile "$KPROFILE" --no-default-features \
    -p yarm --bin kernel_boot >"$LOGDIR/build-off.log" 2>&1 \
    || die "feature-off build failed"
fi
if (( ! fail )); then
  OFF_BIN="$LOGDIR/kernel_off.bin"
  "$OBJCOPY" -O binary "$KELF" "$OFF_BIN" >/dev/null 2>&1 || die "objcopy of feature-off kernel failed"
  fresh_artifact "$OFF_BIN"
  # Oracle-only literals: the disposable task, its knob strings, the boot slot-5 write and the
  # future live-seal literal. The production NR16 syscall, the disposition publication, the
  # in-lock bypass and the RISC-V post-lock consumer are UNCONDITIONAL and NOT audited here.
  OFF_HITS=0
  for lit in "EXIT_TASK_USER_ENTERED role=disposable arch=riscv64" \
             "EXIT_TASK_SYSTEM_HEALTH_OK arch=riscv64" \
             "EXIT_TASK_SYSCALL_RETURNED arch=riscv64" \
             "EXIT_TASK_ORACLE_SPAWNED arch=riscv64" \
             "EXIT_TASK_SURVIVOR_PROGRESS_OK arch=riscv64" \
             "EXIT_TASK_ORACLE_SLOTS arch=riscv64" \
             "yarm.riscv_exit_current_task_oracle" \
             "YARM_RISCV_EXIT_CURRENT_TASK_ORACLE_SET" \
             "$SEAL"; do
    if rg -a -q -F "$lit" "$OFF_BIN"; then die "feature-off image contains $lit"; OFF_HITS=$((OFF_HITS+1)); fi
  done
  note "RUN_A feature-off oracle literals=$OFF_HITS"
  recheck_exact_commit RUN_A
fi

# Feature-off initramfs must be clean too — the disposable task lives in userspace.
if (( ! fail )); then
  note "RUN_A: building feature-OFF RISC-V servers + initramfs"
  BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-riscv64-artifacts.sh >"$LOGDIR/build-off-artifacts.log" 2>&1 \
    || die "feature-off artifact build failed"
  for lit in "EXIT_TASK_USER_ENTERED role=disposable arch=riscv64" \
             "EXIT_TASK_SYSTEM_HEALTH_OK arch=riscv64" \
             "EXIT_TASK_ORACLE_SPAWNED arch=riscv64"; do
    rg -a -q -F "$lit" build-riscv64/initramfs-core.cpio \
      && die "feature-off initramfs contains $lit"
  done
  recheck_exact_commit RUN_A_INITRAMFS
fi

# ── RUN_B: feature-on live exit cell ────────────────────────────────────────────────
if (( ! fail )); then
  note "RUN_B: building feature-ON servers + initramfs"
  BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $FEATURE" \
    scripts/build-qemu-riscv64-artifacts.sh >"$LOGDIR/build-on.log" 2>&1 \
    || die "feature-on artifact build failed"
fi
if (( ! fail )); then
  note "RUN_B: building feature-ON kernel"
  cargo build -Z "build-std=${BUILD_STD}" \
    --target "$KTARGET" --profile "$KPROFILE" \
    --no-default-features --features "$FEATURE" \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild-on.log" 2>&1 \
    || die "feature-on kernel build failed"
  "$OBJCOPY" -O binary "$KELF" "$KBIN" >/dev/null 2>&1 || die "objcopy of feature-on kernel failed"
  fresh_artifact "$KBIN"
  fresh_artifact build-riscv64/initramfs-core.cpio
fi

# Feature forwarding proof: the feature must have reached BOTH the kernel and the userspace
# image, not just the kernel. The kernel carries the boot-side literals; the initramfs carries
# the disposable task's own marker.
if (( ! fail )); then
  rg -a -q -F "EXIT_TASK_ORACLE_SLOTS arch=riscv64" "$KBIN" \
    || die "feature-on kernel missing the slot-5 provisioning literal (feature not forwarded)"
  rg -a -q -F "yarm.riscv_exit_current_task_oracle" "$KBIN" \
    || die "feature-on kernel missing the boot knob literal"
  rg -a -q -F "EXIT_TASK_USER_ENTERED role=disposable arch=riscv64" build-riscv64/initramfs-core.cpio \
    || die "initramfs missing the disposable task literal (feature not forwarded to userspace)"
  # Cross-arch hygiene: a RISC-V image must not carry another architecture's exit cell.
  for foreign in "EXIT_TASK_USER_ENTERED role=disposable arch=x86_64" \
                 "EXIT_TASK_USER_ENTERED role=disposable arch=aarch64"; do
    rg -a -q -F "$foreign" build-riscv64/initramfs-core.cpio \
      && die "foreign exit-oracle literal present in the RISC-V initramfs: $foreign"
  done
  recheck_exact_commit RUN_B_BUILD
fi

RAW="$LOGDIR/exit.raw.log"; CORE="$LOGDIR/exit.core.log"; NORM="$LOGDIR/exit.norm.log"
if (( ! fail )); then
  fresh_log "$RAW" && fresh_log "$CORE" && fresh_log "$NORM" || true
fi

if (( ! fail )); then
  note "RUN_B: one fresh -smp 1 boot with the RISC-V exit oracle armed"
  env KERNEL_IMAGE="$KBIN" \
      INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio \
      KERNEL_CMDLINE="yarm.riscv_exit_current_task_oracle=1" \
      QEMU_SMP=1 QEMU_SINGLE_BOOT=1 QEMU_SMOKE_STRICT=0 \
      LOGFILE="$RAW" \
      TIMEOUT_SECS="$TIMEOUT_SECS" IDLE_MAX_SECS="$IDLE_MAX_SECS" \
      scripts/qemu-riscv64-core-smoke.sh >"$LOGDIR/wrap.log" 2>&1 || true
  grep -a "\[info\] qemu command:" "$LOGDIR/wrap.log" >"$CORE" 2>/dev/null || true
  tr '\r' '\n' <"$RAW" >"$NORM" 2>/dev/null || true
  [[ -s "$NORM" ]] || die "RUN_B produced no boot log"
fi

if (( ! fail )); then
  L=RUN_B
  # Hard-fail markers first: an early exit, a returned syscall, or any committed exiting-task
  # state invalidates everything.
  need_absent "$NORM" "$L" \
    "EXIT_TASK_SYSCALL_RETURNED" \
    "EXIT_TASK_OLD_FRAME_RESTORED" \
    "EXIT_TASK_EXITING_STILL_CURRENT" \
    "EXIT_TASK_WRONG_IDENTITY" \
    "EXIT_TASK_DUPLICATE_DISPOSITION" \
    "EXIT_TASK_TRAP_DEPTH_ERROR" \
    "EXIT_TASK_RESELECTED_EXITING_TASK" \
    "EXIT_TASK_EXITING_SEPC_COMMITTED" \
    "EXIT_TASK_EXITING_RESULT_EXPORTED" \
    "EXIT_TASK_ORACLE_SPAWN_FAIL" \
    "RISCV_TYPED_IDLE_INVARIANT_VIOLATION" \
    "RISCV_TRAP_HANDLE_FAILED" \
    "RISCV_BLOCKED_IDLE_NO_PROVENANCE" \
    "KERNEL PANIC" "RUST PANIC" "panicked at" "FATAL"

  # ── U9-EXIT1 §6 — THE terminal-edge retirement ────────────────────────────────────
  #
  # Stage 200D-0D2 proved the BROAD pipeline: NR 16 reached the terminal broad-locked
  # dispatcher, armed an in-lock bypass, and the post-lock consumer got the exiting task off the
  # CPU before the replacement's SRET. U9-EXIT1 retires that edge, so every marker of that
  # pipeline is now REQUIRED ABSENT rather than reworded — a run still emitting them is a run
  # whose exit still reached the acquisition this stage exists to remove.
  #
  # Each retired assertion is replaced below by the one that states the admission it used to
  # forbid, which is the discipline U9-REAP1 applied to its 21 NR-31 guards.
  broad_enters=$(count_of "$NORM" "EXIT_TASK_BROAD_ENTER")
  split_enters=$(count_of "$NORM" "EXIT_TASK_SPLIT_ENTER")
  split_declines=$(count_of "$NORM" "EXIT_TASK_SPLIT_DECLINED")
  claims=$(count_of "$NORM" "EXIT_TASK_CLAIM_RETIRED")
  dispatches=$(count_of "$NORM" "EXIT_TASK_SYSCALL_DISPATCHED nr=16")
  note "terminal edge: broad=$broad_enters split=$split_enters declined=$split_declines claims=$claims dispatches=$dispatches"
  [[ "$broad_enters" == "0" ]] \
    || die "[$L] NR 16 still reached the terminal broad dispatcher ($broad_enters entries)"
  [[ "$split_enters" == "1" ]] || die "[$L] split-route entries != 1 (got $split_enters)"
  [[ "$split_declines" == "0" ]] || die "[$L] the split route declined an admitted exit ($split_declines)"
  [[ "$claims" == "1" ]] || die "[$L] exit claims retired != 1 (got $claims)"
  [[ "$dispatches" == "1" ]] || die "[$L] NR16 dispatches != 1 (got $dispatches)"

  # The retired broad pipeline, marker for marker. Its ABSENCE is the retirement.
  need_absent "$NORM" "$L" \
    "EXIT_TASK_PREFLIGHT_OK" \
    "EXIT_TASK_DISPOSITION_PUBLISHED" \
    "EXIT_TASK_INLOCK_BYPASS_ARMED arch=riscv64" \
    "EXIT_TASK_BROAD_LOCK_RELEASED arch=riscv64" \
    "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=riscv64" \
    "EXIT_TASK_DISPOSITION_CONSUMED arch=riscv64" \
    "EXIT_TASK_EXITING_NOT_CURRENT arch=riscv64" \
    "EXIT_TASK_ABSENCE_VALIDATED arch=riscv64" \
    "EXIT_TASK_SRET_OWNER arch=riscv64" \
    "EXIT_TASK_RESTORE_OWNER arch=riscv64" \
    "EXIT_TASK_RESTORE_DONE arch=riscv64" \
    "EXIT_TASK_SPLIT_FAILED_CLOSED" \
    "EXIT_TASK_DUPLICATE_DEFERRAL"

  need_once "$NORM" "$L" \
    "EXIT_TASK_ORACLE_SLOTS arch=riscv64 slot5=22" \
    "EXIT_TASK_USER_ENTERED role=disposable arch=riscv64" \
    "EXIT_TASK_SPLIT_ENTER" \
    "EXIT_TASK_CLAIM_RETIRED" \
    "EXIT_TASK_LIFECYCLE_TRANSITION" \
    "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=exit_current_task_switch_required" \
    "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=16 cpu=0 result=queue_advance_committed" \
    "EXIT_TASK_SURVIVOR_PROGRESS_OK arch=riscv64" \
    "EXIT_TASK_SYSTEM_HEALTH_OK arch=riscv64"

  # ── the exiting task's identity, read from the log rather than assumed ─────────────
  EXITING_TID=$(rg -a -o -r '$1' "EXIT_TASK_SYSCALL_DISPATCHED nr=16 tid=([0-9]+)" "$NORM" | head -1)
  [[ -n "$EXITING_TID" ]] || die "[$L] could not resolve the exiting tid"
  note "exiting tid=$EXITING_TID"
  rg -a -q -F "EXIT_TASK_CLAIM_RETIRED tid=$EXITING_TID " "$NORM" \
    || die "[$L] the retired claim does not name the dispatched tid"
  rg -a -q -F "EXIT_TASK_SPLIT_ENTER tid=$EXITING_TID " "$NORM" \
    || die "[$L] the split edge does not name the dispatched tid"

  # ── the exiting frame is never saved, and never SRET'd back into ───────────────────
  rg -a -F "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=16 cpu=0 result=queue_advance_committed" "$NORM" \
    | rg -a -q -F "captured=0" \
    || die "[$L] the exiting frame was captured — §3 forbids saving it"
  rg -a -F "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=16 cpu=0 result=queue_advance_committed" "$NORM" \
    | rg -a -q -F "outgoing=$EXITING_TID" \
    || die "[$L] the committed advance names a different outgoing task"

  # ── the ONE queue advance, checked in the slice that belongs to this exit ──────────
  #
  # The drain's markers are SHARED with FutexWait and blocking recv, which fire long before the
  # exit, so a whole-log ordering claim about them would be satisfied by somebody else's advance.
  SLICE="$LOGDIR/exit.after.log"
  awk 'index($0,"EXIT_TASK_SYSCALL_DISPATCHED nr=16"){f=1} f' "$NORM" >"$SLICE"
  [[ -s "$SLICE" ]] || die "[$L] could not slice the log at the accepted exit"

  need_order "$SLICE" "$L" "EXIT_TASK_SYSCALL_DISPATCHED nr=16" "EXIT_TASK_LIFECYCLE_TRANSITION" \
    "the dispatched exit precedes its lifecycle transition"
  need_order "$SLICE" "$L" "EXIT_TASK_LIFECYCLE_TRANSITION" \
    "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=exit_current_task_switch_required" \
    "the transition precedes the queue-advance deferral it commits"
  need_order "$SLICE" "$L" "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=exit_current_task_switch_required" \
    "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=16 cpu=0 result=queue_advance_committed" \
    "the deferral is published before the seam answers QueueAdvanceCommitted"
  need_order "$SLICE" "$L" "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=16 cpu=0 result=queue_advance_committed" \
    "QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED cpu=0 reason=publication_committed" \
    "the committed advance skips the broad dispatch exactly once"
  need_order "$SLICE" "$L" "QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED cpu=0 reason=publication_committed" \
    "RISCV_FUTEX_WAIT_DISPATCH_LOCK_DROPPED_OK cpu=0" \
    "the EXISTING post-lock drain runs with the broad guard already dropped"
  need_order "$SLICE" "$L" "RISCV_FUTEX_WAIT_DISPATCH_LOCK_DROPPED_OK cpu=0" \
    "RISCV_FUTEX_WAIT_DISPATCH_REVERIFY_OK tid=$EXITING_TID" \
    "the drain reverifies THIS exit by exact identity"
  need_order "$SLICE" "$L" "RISCV_FUTEX_WAIT_DISPATCH_REVERIFY_OK tid=$EXITING_TID" \
    "RISCV_FUTEX_WAIT_DISPATCH_CURRENT_SET_OK cpu=0" \
    "the drain selects and installs an incoming task"
  need_order "$SLICE" "$L" "RISCV_FUTEX_WAIT_DISPATCH_CURRENT_SET_OK cpu=0" \
    "RISCV_FUTEX_WAIT_DISPATCH_SRET_ARMED" \
    "the SRET is armed for the incoming task"
  need_order "$SLICE" "$L" "RISCV_FUTEX_WAIT_DISPATCH_SRET_ARMED" \
    "RISCV_FUTEX_WAIT_DISPATCH_DONE result=ok" \
    "the drain completes"
  need_order "$SLICE" "$L" "RISCV_FUTEX_WAIT_DISPATCH_DONE result=ok" \
    "EXIT_TASK_SURVIVOR_PROGRESS_OK arch=riscv64" \
    "a surviving task makes progress after the exit"
  need_order "$SLICE" "$L" "EXIT_TASK_SURVIVOR_PROGRESS_OK arch=riscv64" \
    "EXIT_TASK_SYSTEM_HEALTH_OK arch=riscv64" \
    "terminal health is the last thing proven"
  # The oracle's own arming chain, unchanged by the retirement: the selector is provisioned
  # before the disposable task is spawned, and the task is spawned before it runs. Neither claim
  # is about the route that services NR 16, so neither moves.
  need_order "$NORM" "$L" "EXIT_TASK_ORACLE_SLOTS arch=riscv64 slot5=22" "EXIT_TASK_ORACLE_SPAWNED arch=riscv64" \
    "the selector is provisioned before the task is spawned"
  need_order "$NORM" "$L" "EXIT_TASK_ORACLE_SPAWNED arch=riscv64" "EXIT_TASK_USER_ENTERED role=disposable arch=riscv64" \
    "the task is spawned before it runs"
  need_order "$NORM" "$L" "EXIT_TASK_USER_ENTERED" "EXIT_TASK_SYSCALL_DISPATCHED nr=16" \
    "userspace entry precedes the syscall"
  need_order "$NORM" "$L" "EXIT_TASK_SPLIT_ENTER" "EXIT_TASK_SYSCALL_DISPATCHED nr=16" \
    "the split edge is counted at the route's entry"

  # ── the dead task is never reselected, and never SRET'd into ──────────────────────
  RESUMED=$(rg -a -o -r '$1' "RISCV_FUTEX_WAIT_DISPATCH_CURRENT_SET_OK cpu=0 incoming=([0-9]+)" "$SLICE" | head -1)
  [[ -n "$RESUMED" ]] || die "[$L] the drain installed no incoming task"
  note "advance resumed tid=$RESUMED (exiting tid=$EXITING_TID)"
  [[ "$RESUMED" != "$EXITING_TID" ]] || die "[$L] the drain reselected the EXITING task"
  rg -a -q -F "RISCV_FUTEX_WAIT_DISPATCH_FRAME_OK incoming=$EXITING_TID" "$SLICE" \
    && die "[$L] a dead task's frame was committed for return"
  rg -a -q -F "RISCV_FUTEX_WAIT_DISPATCH_SATP_OK incoming=$EXITING_TID" "$SLICE" \
    && die "[$L] a dead task's address space was installed for return"

  # ── single boot instance ──────────────────────────────────────────────────────────
  launches=$(count_of "$CORE" "[info] qemu command:")
  entry=$(count_of "$NORM" "YARM_BOOT_CMDLINE_CAPTURE arch=riscv64")
  bootok=$(count_of "$NORM" "YARM_BOOT_OK present_cpus=")
  noncelines=$(rg -a -c -e "YARM_BOOT_INSTANCE arch=riscv64 nonce=" "$NORM" 2>/dev/null || echo 0)
  nonces=$(rg -a -o "YARM_BOOT_INSTANCE arch=riscv64 nonce=0x[0-9a-f]+" "$NORM" 2>/dev/null | sort -u | wc -l | tr -d ' ')
  note "single-boot: qemu=$launches entry=$entry boot_ok=$bootok nonce_lines=$noncelines distinct_nonces=$nonces"
  [[ "$launches"   == "1" ]] || die "[$L] QEMU launches != 1 (got $launches)"
  [[ "$entry"      == "1" ]] || die "[$L] kernel entries != 1 (got $entry)"
  [[ "$bootok"     == "1" ]] || die "[$L] boot completions != 1 (got $bootok)"
  [[ "$noncelines" == "1" ]] || die "[$L] boot nonce lines != 1 (got $noncelines)"
  [[ "$nonces"     == "1" ]] || die "[$L] distinct boot nonces != 1 (got $nonces)"

  # Terminal health LAST: a run that died early cannot pass.
  rg -a -q -F "EXIT_TASK_SYSTEM_HEALTH_OK arch=riscv64" "$NORM" \
    || die "[$L] qemu exited before terminal proof (no EXIT_TASK_SYSTEM_HEALTH_OK)"
  recheck_exact_commit RUN_B
fi

if (( fail )); then
  echo "$SEAL arch=riscv64 live_cells=0 result=fail"
  exit 1
fi

cat <<SEALTEXT
$SEAL
arch=riscv64
syscall_nr=16
user_entries=1
accepted_exits=1
terminal_broad_dispatcher_entries=0
split_route_entries=1
split_route_declines=0
exit_claims_retired=1
dispositions_published=0
dispositions_consumed=0
inlock_bypass_armings=0
queue_advance_deferrals_committed=1
exiting_frames_captured=0
broad_dispatch_skips_after_commit=1
post_lock_drain_selections=1
exiting_task_reselections=0
dead_task_user_returns=0
exiting_sepc_commits=0
normal_result_writes_to_old_frame=0
old_frame_restores=0
syscall_returns_after_accept=0
software_trap_depth_clears=0
sret_ownership_errors=0
wrong_current_task=0
survivor_progress_completions=1
system_health_completions=1
duplicate_deferrals=0
split_failed_closed=0
single_boot_failures=0
retired_broad_pipeline_markers_remaining=0
exact_commit=${SHA0}
exact_tree=${TREE0}
result=ok
SEALTEXT
