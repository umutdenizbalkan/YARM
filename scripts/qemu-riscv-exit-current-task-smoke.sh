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
    "EXIT_TASK_ORACLE_SPAWN_FAIL" \
    "RISCV_TYPED_IDLE_INVARIANT_VIOLATION" \
    "RISCV_TRAP_HANDLE_FAILED" \
    "RISCV_BLOCKED_IDLE_NO_PROVENANCE" \
    "KERNEL PANIC" "RUST PANIC" "panicked at" "FATAL"

  need_once "$NORM" "$L" \
    "EXIT_TASK_ORACLE_SLOTS arch=riscv64 slot5=22" \
    "EXIT_TASK_USER_ENTERED role=disposable arch=riscv64" \
    "EXIT_TASK_PREFLIGHT_OK" \
    "EXIT_TASK_LIFECYCLE_TRANSITION" \
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
    "EXIT_TASK_SURVIVOR_PROGRESS_OK arch=riscv64" \
    "EXIT_TASK_SYSTEM_HEALTH_OK arch=riscv64"
  [[ "$(count_of "$NORM" "EXIT_TASK_SYSCALL_DISPATCHED nr=16")" == "1" ]] \
    || die "[$L] NR16 dispatches != 1"

  # ── per-line lock-state correctness ───────────────────────────────────────────────
  # Every post-lock marker must STATE broad_lock=0; the one in-lock marker must state
  # broad_lock=1. A marker that merely sits after the boundary is not sufficient evidence —
  # this is the contract Stage 200D-0B3 established and Stage 200D-0C2 extended.
  for m in "EXIT_TASK_BROAD_LOCK_RELEASED arch=riscv64" \
           "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=riscv64" \
           "EXIT_TASK_DISPOSITION_CONSUMED arch=riscv64" \
           "EXIT_TASK_EXITING_NOT_CURRENT arch=riscv64" \
           "EXIT_TASK_ABSENCE_VALIDATED arch=riscv64" \
           "EXIT_TASK_SRET_OWNER arch=riscv64" \
           "EXIT_TASK_RESTORE_OWNER arch=riscv64"; do
    rg -a -F "$m" "$NORM" | rg -a -q -F "broad_lock=0" \
      || die "[$L] post-lock marker does not state broad_lock=0: $m"
    rg -a -F "$m" "$NORM" | rg -a -q -F "broad_lock=1" \
      && die "[$L] post-lock marker falsely claims broad_lock=0 elsewhere: $m"
  done
  rg -a -F "EXIT_TASK_INLOCK_BYPASS_ARMED arch=riscv64" "$NORM" | rg -a -q -F "broad_lock=1" \
    || die "[$L] the in-lock bypass marker does not state broad_lock=1"
  rg -a -F "EXIT_TASK_INLOCK_BYPASS_ARMED arch=riscv64" "$NORM" | rg -a -q -F "broad_lock=0" \
    && die "[$L] the in-lock bypass marker falsely claims broad_lock=0"

  # The bypass must have actually suppressed the exiting task's result export.
  rg -a -F "EXIT_TASK_INLOCK_BYPASS_ARMED arch=riscv64" "$NORM" | rg -a -q -F "inlock_result_export=0" \
    || die "[$L] the in-lock bypass did not suppress the exiting task's result export"
  # The drain marker must attest that ALL Phase-3 drains ran.
  rg -a -F "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=riscv64" "$NORM" | rg -a -q -F "drains=all" \
    || die "[$L] post-lock drain marker does not attest drains=all"
  # The release marker must name the guard it outlived.
  rg -a -F "EXIT_TASK_BROAD_LOCK_RELEASED arch=riscv64" "$NORM" | rg -a -q -F "holder=with_cpu" \
    || die "[$L] release marker does not name the broad-lock holder"
  # sret ownership: the trap bridge owns the single sret and the consumer clears no software depth.
  rg -a -F "EXIT_TASK_SRET_OWNER arch=riscv64" "$NORM" | rg -a -q -F "owner=trap_bridge" \
    || die "[$L] sret owner is not the trap bridge"
  rg -a -F "EXIT_TASK_SRET_OWNER arch=riscv64" "$NORM" | rg -a -q -F "software_depth_clears=0" \
    || die "[$L] consumer-specific software depth clears != 0"
  # Absence uses the FULL incarnation and covers the outgoing frame source.
  rg -a -F "EXIT_TASK_ABSENCE_VALIDATED arch=riscv64" "$NORM" | rg -a -q -F "identity=tid_asid" \
    || die "[$L] absence validation is not full-identity"
  rg -a -F "EXIT_TASK_ABSENCE_VALIDATED arch=riscv64" "$NORM" | rg -a -q -F "frame_source=0" \
    || die "[$L] absence validation does not cover the outgoing frame source"
  # Exactly one restore owner, replacement or idle, sourced from the replacement's TCB.
  [[ "$(count_of "$NORM" "EXIT_TASK_RESTORE_OWNER arch=riscv64")" == "1" ]] \
    || die "[$L] restore-owner attestations != 1"
  rg -a -F "EXIT_TASK_RESTORE_OWNER arch=riscv64" "$NORM" \
    | rg -a -q -e "owner=replacement" -e "owner=idle" \
    || die "[$L] restore owner is neither replacement nor idle"
  rg -a -F "EXIT_TASK_RESTORE_DONE arch=riscv64" "$NORM" | rg -a -q -F "frame_source=replacement_tcb" \
    || die "[$L] the outgoing frame was not sourced from the replacement's TCB"

  # ── ordered chain ─────────────────────────────────────────────────────────────────
  need_order "$NORM" "$L" "EXIT_TASK_ORACLE_SLOTS arch=riscv64 slot5=22" "EXIT_TASK_ORACLE_SPAWNED arch=riscv64" \
    "the selector is provisioned before the task is spawned"
  need_order "$NORM" "$L" "EXIT_TASK_ORACLE_SPAWNED arch=riscv64" "EXIT_TASK_USER_ENTERED role=disposable arch=riscv64" \
    "the task is spawned before it runs"
  need_order "$NORM" "$L" "EXIT_TASK_USER_ENTERED role=disposable arch=riscv64" "EXIT_TASK_SYSCALL_DISPATCHED nr=16" \
    "userspace entry precedes the syscall"
  need_order "$NORM" "$L" "EXIT_TASK_SYSCALL_DISPATCHED nr=16" "EXIT_TASK_PREFLIGHT_OK" \
    "dispatch precedes preflight"
  need_order "$NORM" "$L" "EXIT_TASK_PREFLIGHT_OK" "EXIT_TASK_LIFECYCLE_TRANSITION" \
    "preflight precedes the lifecycle transition"
  need_order "$NORM" "$L" "EXIT_TASK_LIFECYCLE_TRANSITION" "EXIT_TASK_DISPOSITION_PUBLISHED" \
    "teardown precedes the disposition"
  need_order "$NORM" "$L" "EXIT_TASK_DISPOSITION_PUBLISHED" "EXIT_TASK_INLOCK_BYPASS_ARMED arch=riscv64" \
    "the in-lock bypass is armed from the published disposition"
  need_order "$NORM" "$L" "EXIT_TASK_INLOCK_BYPASS_ARMED arch=riscv64" "EXIT_TASK_BROAD_LOCK_RELEASED arch=riscv64" \
    "the bypass is decided under the lock, the release attested after"
  need_order "$NORM" "$L" "EXIT_TASK_BROAD_LOCK_RELEASED arch=riscv64" "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=riscv64" \
    "the Phase-3 drains run after the broad lock is actually released"
  need_order "$NORM" "$L" "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=riscv64" "EXIT_TASK_DISPOSITION_CONSUMED arch=riscv64" \
    "the consumer runs after every Phase-3 drain"
  need_order "$NORM" "$L" "EXIT_TASK_DISPOSITION_CONSUMED arch=riscv64" "EXIT_TASK_EXITING_NOT_CURRENT arch=riscv64" \
    "identity is validated after consumption"
  need_order "$NORM" "$L" "EXIT_TASK_EXITING_NOT_CURRENT arch=riscv64" "EXIT_TASK_ABSENCE_VALIDATED arch=riscv64" \
    "full-identity absence follows the not-current check"
  need_order "$NORM" "$L" "EXIT_TASK_ABSENCE_VALIDATED arch=riscv64" "EXIT_TASK_SRET_OWNER arch=riscv64" \
    "sret ownership is stated after absence is established"
  need_order "$NORM" "$L" "EXIT_TASK_SRET_OWNER arch=riscv64" "EXIT_TASK_RESTORE_OWNER arch=riscv64" \
    "the restore owner is named after sret ownership"
  need_order "$NORM" "$L" "EXIT_TASK_RESTORE_OWNER arch=riscv64" "EXIT_TASK_RESTORE_DONE arch=riscv64" \
    "the owner is named before the restore is attested"
  need_order "$NORM" "$L" "EXIT_TASK_RESTORE_DONE arch=riscv64" "EXIT_TASK_SURVIVOR_PROGRESS_OK arch=riscv64" \
    "a surviving task makes progress after the exit"
  need_order "$NORM" "$L" "EXIT_TASK_SURVIVOR_PROGRESS_OK arch=riscv64" "EXIT_TASK_SYSTEM_HEALTH_OK arch=riscv64" \
    "terminal health is the last thing proven"

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
dispositions_published=1
dispositions_consumed=1
broad_lock_release_before_consumer=1
post_lock_drains_before_consumer=1
replacement_restore_after_consumer=1
normal_result_writes_to_old_frame=0
old_frame_restores=0
exiting_sepc_commits=0
syscall_returns_after_accept=0
software_trap_depth_clears=0
sret_ownership_errors=0
wrong_current_task=0
exiting_task_reselections=0
replacement_or_idle=1
absence_validations=1
survivor_progress_completions=1
system_health_completions=1
duplicate_dispositions=0
single_boot_failures=0
exact_commit=${SHA0}
exact_tree=${TREE0}
result=ok
SEALTEXT
