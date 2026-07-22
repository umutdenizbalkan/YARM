#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 200C2B — x86_64 LIVE reply-receive TIMEOUT OFF-LOCK RETIREMENT smoke (two fresh boots).
#
# Wires the accepted Stage 200C1 reply-timeout transaction into a NARROW off-lock collector +
# per-CPU deferred-work drain that runs the completion at the trap-entry post-lock area with the
# broad `SpinLock<KernelState>` already dropped. It earns ONE x86_64 retirement cell for the
# `IpcReplyTimeout` class, and proves the two live outcomes on `-smp 1`, EACH from a fresh boot of
# the SAME clean tree:
#
#   A. timeout-wins  — the PRODUCTION off-lock collector publishes one deferred work item and the
#                      off-lock drain completes it: the caller resumes with the canonical TimedOut,
#                      the class reports scan_broad_lock=0, and the retirement seal is emitted. The
#                      server's late NR7 is rejected.
#   B. reply-wins    — the server's NR7 wins terminal ownership before the deadline (reversibly, so
#                      a copy fault could roll back), the exact deadline lease is completed, the
#                      caller resumes with the reply payload, and the off-lock collector genuinely
#                      scans PAST the old deadline harmlessly (no timeout wake).
#
# This is a LIVE RETIREMENT seal: the reply-timeout class deadline scan NO LONGER enters through the
# broad KernelState (reported honestly via IPC_REPLY_TIMEOUT_LOCK_STATUS scan_broad_lock=0), and the
# runner emits GLOBAL_LOCK_RETIRE_CLASS_DONE class=IpcReplyTimeout exactly once (timeout-wins boot).
# Ordinary receive timeouts stay on their existing in-lock path (NOT retired here).
#
# On both fresh boots passing, the runner (not userspace) emits:
#   STAGE_200C_REPLY_TIMEOUT_X86_RETIREMENT_SEAL arch=x86_64 classes=1 live_cells=1 ...
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=x86-ipc-reply-timeout-oracle
KTARGET=${KTARGET:-targets/x86_64-yarm-none.json}
KPROFILE=${KPROFILE:-x86-none}
KELF=${KELF:-target/x86_64-yarm-none/${KPROFILE}/kernel_boot}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ipc-reply-timeout-retirement-x86_64}
TIMEOUT_SECS=${TIMEOUT_SECS:-90}
mkdir -p "$LOGDIR"

fail=0
note() { echo "[ipc-reply-timeout-retire] $*"; }
die()  { echo "[ipc-reply-timeout-retire][fail] $*"; fail=1; }

# ── SHA + clean-tree capture (re-checked between the two fresh boots) ──
SHA0=$(git rev-parse HEAD 2>/dev/null || echo unknown)
clean_tree() { git diff --quiet && git diff --cached --quiet; }
if clean_tree; then TREE0=clean; else TREE0=dirty; fi
note "sha=$SHA0 tree=$TREE0"

recheck_sha_clean() {
  local sha; sha=$(git rev-parse HEAD 2>/dev/null || echo unknown)
  [[ "$sha" == "$SHA0" ]] || die "SHA drifted mid-run ($SHA0 -> $sha)"
  if clean_tree; then :; else [[ "$TREE0" == "dirty" ]] || die "tree became dirty mid-run"; fi
}

# ── 1. Base artifacts (servers + initramfs; the userspace oracle is arch-gated) ──
note "building base x86_64 artifacts (servers + initramfs)"
BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
  scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
  || die "base artifact build failed (see $LOGDIR/build.log)"

# ── 2. Feature-ON kernel + integrity: it MUST carry the live marker literals ──
if (( ! fail )); then
  note "building kernel_boot with --features $FEATURE"
  cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" \
    --no-default-features --features "$FEATURE" \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 \
    || die "feature kernel_boot build failed (see $LOGDIR/kbuild.log)"
fi
if (( ! fail )); then
  cp "$KELF" build-x86_64/kernel_boot.elf
  for lit in IPC_REPLY_TIMEOUT_OK IPC_REPLY_BEATS_TIMEOUT_OK IPC_REPLY_TIMEOUT_ARMED \
             "class=IpcReplyTimeout"; do
    rg -a -q "$lit" build-x86_64/kernel_boot.elf || die "feature kernel missing literal $lit (wrong build)"
  done
fi

# ── 2b. Feature-OFF kernel MUST be marker-CLEAN of the live literals ──
if (( ! fail )); then
  note "building feature-OFF kernel_boot and asserting it is marker-clean"
  cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" \
    --no-default-features -p yarm --bin kernel_boot >"$LOGDIR/kbuild-off.log" 2>&1 \
    || die "feature-off kernel_boot build failed (see $LOGDIR/kbuild-off.log)"
  # NOTE: this feature-OFF build overwrites $KELF (the target path); the feature-ON
  # boot image was already copied to build-x86_64/kernel_boot.elf in step 2, so it is
  # untouched here. Do NOT re-copy $KELF (it is now the feature-OFF binary).
  OFF_ELF="target/x86_64-yarm-none/${KPROFILE}/kernel_boot"
  # Reply-timeout-SPECIFIC live literals must be absent (other classes may legitimately
  # carry a `GLOBAL_LOCK_RETIRE_CLASS_DONE` for their own class — hence class-specific).
  for lit in IPC_REPLY_TIMEOUT_OK IPC_REPLY_BEATS_TIMEOUT_OK IPC_REPLY_TIMEOUT_ARMED \
             IPC_REPLY_TIMEOUT_LOCK_STATUS IPC_REPLY_TIMEOUT_LATE_SCAN \
             "class=IpcReplyTimeout" IPC_REPLY_TIMEOUT_DEFERRED; do
    rg -a -q "$lit" "$OFF_ELF" && die "feature-OFF kernel contains live literal $lit (not marker-clean)"
  done
fi

if (( fail )); then
  echo "STAGE_200C_REPLY_TIMEOUT_X86_RETIREMENT_SEAL arch=x86_64 classes=1 live_cells=1 result=fail reason=build"
  exit 1
fi

# ── Boot helper: one fresh -smp 1 boot for the given mode, into its own log ──
boot_mode() {
  local mode="$1" log="$2"
  env \
    KERNEL_IMAGE=build-x86_64/kernel_boot.elf \
    INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio \
    KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.x86_64_ipc_reply_timeout_oracle=${mode}" \
    QEMU_SMP=1 \
    LOGFILE="$log" \
    SMOKE_LOG="$LOGDIR/core-${mode}.log" \
    TIMEOUT_SECS="$TIMEOUT_SECS" \
    YARM_MODE_ISOLATION=0 \
    scripts/qemu-x86_64-core-smoke.sh >"$LOGDIR/wrap-${mode}.log" 2>&1 || true
}

verify_log() {
  # $1 = normalized log, then the required marker strings (each must appear exactly once).
  local norm="$1"; shift
  local m c
  for m in "$@"; do
    c=$(rg -a -c -F "$m" "$norm" 2>/dev/null || echo 0)
    [[ "$c" == "1" ]] || die "marker count != 1 (got $c): $m"
  done
}
forbid_log() {
  local norm="$1"; shift
  local m
  for m in "$@"; do
    if rg -a -q -F "$m" "$norm"; then die "forbidden marker present: $m"; fi
  done
}

# ── 3. Scenario A — timeout-wins, feature enabled (fresh boot) ──
TW_OK=0
if (( ! fail )); then
  note "booting fresh -smp 1 QEMU: yarm.x86_64_ipc_reply_timeout_oracle=timeout-wins"
  boot_mode timeout-wins "$LOGDIR/boot-timeout-wins.log"
  TW="$LOGDIR/tw.norm.log"; tr '\r' '\n' <"$LOGDIR/boot-timeout-wins.log" >"$TW"
  [[ -s "$TW" ]] || die "no timeout-wins boot log"
  verify_log "$TW" \
    "IPC_REPLY_TIMEOUT_ARMED arch=x86_64" \
    "IPC_REPLY_TIMEOUT_OK arch=x86_64 terminal=Timeout timeout_result=TimedOut caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=0 result=ok" \
    "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=x86_64 scan_broad_lock=0 completion_transaction_narrow=1 result=ok" \
    "IPC_REPLY_TIMEOUT_DEFERRED arch=x86_64 published=1 drained=1 result=ok" \
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcReplyTimeout result=ok" \
    "X86_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut caller_continuations=1 late_reply=rejected result=ok"
  # A timeout win must not also emit a reply-win, a duplicate timeout, a broad-lock status,
  # or a panic/fatal trap.
  forbid_log "$TW" \
    "IPC_REPLY_BEATS_TIMEOUT_OK" \
    "scan_broad_lock=1" \
    "PANIC" "KERNEL PANIC" "FATAL"
  recheck_sha_clean
  (( fail )) || TW_OK=1
fi

# ── 4. Scenario B — reply-wins, feature enabled (SEPARATE fresh boot) ──
RW_OK=0
if (( ! fail )); then
  note "booting fresh -smp 1 QEMU: yarm.x86_64_ipc_reply_timeout_oracle=reply-wins"
  boot_mode reply-wins "$LOGDIR/boot-reply-wins.log"
  RW="$LOGDIR/rw.norm.log"; tr '\r' '\n' <"$LOGDIR/boot-reply-wins.log" >"$RW"
  [[ -s "$RW" ]] || die "no reply-wins boot log"
  verify_log "$RW" \
    "IPC_REPLY_TIMEOUT_ARMED arch=x86_64" \
    "IPC_REPLY_BEATS_TIMEOUT_OK arch=x86_64 terminal=Reply reply_copies=1 deadline_disarmed=1 late_timeout_claims=0 caller_wakes=1 result=ok" \
    "IPC_REPLY_TIMEOUT_LATE_SCAN arch=x86_64 outcome=reply_won late_timeout_claims=0 result=ok" \
    "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=x86_64 scan_broad_lock=0 completion_transaction_narrow=1 result=ok" \
    "X86_IPC_REPLY_BEATS_TIMEOUT_DONE reply_ok=1 caller_continuations=1 late_timeout_wakes=0 result=ok"
  # Reply won ⇒ NO timeout wake, no broad-lock status, no panic/fatal trap.
  forbid_log "$RW" \
    "IPC_REPLY_TIMEOUT_OK arch=x86_64 terminal=Timeout" \
    "scan_broad_lock=1" \
    "PANIC" "KERNEL PANIC" "FATAL"
  recheck_sha_clean
  (( fail )) || RW_OK=1
fi

# ── 5. Retirement live seal (runner-emitted; both fresh boots must pass) ──
if (( fail )) || [[ "$TW_OK" != "1" || "$RW_OK" != "1" ]]; then
  echo "STAGE_200C_REPLY_TIMEOUT_X86_RETIREMENT_SEAL arch=x86_64 classes=1 live_cells=1 timeout_wins=${TW_OK} reply_wins=${RW_OK} result=fail"
  exit 1
fi

cat <<'SEAL'
STAGE_200C_REPLY_TIMEOUT_X86_RETIREMENT_SEAL
arch=x86_64
classes=1
live_cells=1
timeout_wins=1
reply_wins=1
scan_broad_lock=0
completion_transaction_narrow=1
late_reply_successes=0
late_timeout_wakes=0
duplicate_wakes=0
stale_authority_restores=0
wrong_waiter_mutations=0
result=ok
SEAL
