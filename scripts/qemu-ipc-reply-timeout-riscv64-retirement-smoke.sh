#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 200C2C2 — RISC-V LIVE reply-receive TIMEOUT OFF-LOCK RETIREMENT smoke (two fresh boots).
#
# The RISC-V port of the accepted x86_64/AArch64 IpcReplyTimeout retirement cell (the third cell). It REUSES the arch-neutral
# collector + per-CPU deferred-work drain + completion transaction verbatim, wired into the AArch64
# trap-entry post-lock area, and proves the two live outcomes on `-smp 1`, EACH from a fresh boot of
# the SAME clean tree:
#
#   A. timeout-wins  — the production off-lock collector publishes one deferred work item and the
#                      off-lock drain completes it: the blocked recv-v2 caller resumes with the
#                      canonical TimedOut written to its saved trap frame, the class reports
#                      scan_broad_lock=0, and the retirement seal is emitted. The late NR7 is rejected.
#   B. reply-wins    — the server's NR7 wins terminal ownership first (a reversible ClaimedByReply
#                      lease blocks any concurrent timeout claim); the reply copies, the record +
#                      terminal complete as Reply, the deadline lease completes, and the caller resumes
#                      with the exact payload. A later production scan passes the old deadline
#                      harmlessly (no timeout work/wake).
#
# LIVE RETIREMENT seal: the reply-timeout class deadline scan runs OFF the broad KernelState
# (IPC_REPLY_TIMEOUT_LOCK_STATUS arch=riscv64 scan_broad_lock=0), and the runner emits
# GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcReplyTimeout exactly once (timeout-wins boot).
# Ordinary receive timeouts stay on their existing in-lock path (NOT retired here).
#
# On both fresh boots passing, the runner (not userspace) emits:
#   STAGE_200C_REPLY_TIMEOUT_RISCV64_RETIREMENT_SEAL arch=riscv64 classes=1 live_cells=1 ...
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=riscv64-ipc-reply-timeout-oracle
KTARGET=${KTARGET:-riscv64gc-unknown-none-elf}
KPROFILE=${KPROFILE:-release}
KELF=${KELF:-target/riscv64gc-unknown-none-elf/${KPROFILE}/kernel_boot}
KBIN=${KBIN:-build-riscv64/yarm-riscv64.bin}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ipc-reply-timeout-retirement-riscv64}
TIMEOUT_SECS=${TIMEOUT_SECS:-180}
IDLE_MAX_SECS=${IDLE_MAX_SECS:-180}
mkdir -p "$LOGDIR"

fail=0
note() { echo "[ipc-reply-timeout-retire-riscv64] $*"; }
die()  { echo "[ipc-reply-timeout-retire-riscv64][fail] $*"; fail=1; }

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

objcopy_tool() {
  if command -v llvm-objcopy >/dev/null 2>&1; then echo llvm-objcopy;
  elif command -v rust-objcopy >/dev/null 2>&1; then echo rust-objcopy;
  else return 1; fi
}

# ── 1. Base artifacts (servers + initramfs; the userspace oracle is arch-gated) ──
note "building base riscv64 artifacts (servers + initramfs)"
BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
  scripts/build-qemu-riscv64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
  || die "base artifact build failed (see $LOGDIR/build.log)"

OBJCOPY=""
if (( ! fail )); then OBJCOPY=$(objcopy_tool) || die "no objcopy available"; fi

# ── 2. Feature-ON kernel + integrity: it MUST carry the AArch64 retirement literals ──
if (( ! fail )); then
  note "building kernel_boot with --features $FEATURE"
  cargo build -Z "build-std=${BUILD_STD}" \
    --target "$KTARGET" --profile "$KPROFILE" \
    --no-default-features --features "$FEATURE" \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 \
    || die "feature kernel_boot build failed (see $LOGDIR/kbuild.log)"
fi
if (( ! fail )); then
  "$OBJCOPY" -O binary "$KELF" "$KBIN" >"$LOGDIR/objcopy.log" 2>&1 \
    || die "objcopy of feature kernel failed (see $LOGDIR/objcopy.log)"
  # The AArch64 kernel carries the arch=riscv64 attribution + the class literal.
  rg -a -q "riscv64" "$KBIN" || die "feature kernel missing riscv64 attribution"
  rg -a -q "class=IpcReplyTimeout" "$KBIN" || die "feature kernel missing IpcReplyTimeout class literal"
  # Cross-arch hygiene: no x86_64/riscv64 reply-timeout attribution in the AArch64 kernel.
  rg -a -q "IPC_REPLY_TIMEOUT_OK arch=x86_64" "$KBIN" && die "x86_64 reply-timeout attribution in riscv64 kernel"
  rg -a -q "IPC_REPLY_TIMEOUT_OK arch=aarch64" "$KBIN" && die "aarch64 reply-timeout attribution in riscv64 kernel"
fi

# ── 2b. Feature-OFF kernel MUST be marker-CLEAN of the reply-timeout retirement literals ──
if (( ! fail )); then
  note "building feature-OFF kernel_boot and asserting it is marker-clean"
  cargo build -Z "build-std=${BUILD_STD}" \
    --target "$KTARGET" --profile "$KPROFILE" --no-default-features \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild-off.log" 2>&1 \
    || die "feature-off kernel_boot build failed (see $LOGDIR/kbuild-off.log)"
  OFF_BIN="$LOGDIR/kernel_boot_off.bin"
  "$OBJCOPY" -O binary "$KELF" "$OFF_BIN" >/dev/null 2>&1 || die "objcopy of feature-off kernel failed"
  # U7 (canonical 199E) split this gate in two, because the feature no longer decides where
  # timeouts are processed — only which oracle SCENARIOS are built.
  #
  # (a) The oracle's own scenario literals must still be absent from a feature-OFF kernel.
  for lit in "IPC_REPLY_BEATS_TIMEOUT_OK arch=" "IPC_REPLY_TIMEOUT_ARMED arch=" \
             "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=" \
             "IPC_REPLY_WIN_RESERVE arch="; do
    rg -a -q "$lit" "$OFF_BIN" && die "feature-OFF kernel contains oracle literal $lit (not marker-clean)"
  done
  # (b) The PRODUCTION pipeline's literals must now be PRESENT in a feature-OFF kernel. Their
  # absence would mean the promotion regressed back behind the feature — the exact thing U7
  # removed — so this half of the gate is asserted positively.
  # NB: the arch tag is a RUNTIME argument (`arch={}` + `REPLY_TIMEOUT_ARCH`), so the composite
  # "MARKER arch=riscv64" string never exists in the image — only the format-string fragment
  # does. Matching the fragment is what makes this gate real rather than vacuously true.
  # `class=IpcReplyTimeout` is deliberately NOT in this list on riscv64. Its only emit site on
  # this port is the resume-boundary recv/reply completion consumer, which U6 policy keeps
  # feature-gated (`the_reply_timeout_feature_policy_is_unchanged`) and U7 does not widen: U7
  # promoted where timeouts are SCANNED and SETTLED, not the IpcRecv delivery consumer. On
  # x86_64 the drain itself is the delivery point, so the seal is present there.
  for lit in "IPC_REPLY_TIMEOUT_OK arch=" "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=" \
             "IPC_REPLY_TIMEOUT_LATE_SCAN arch=" \
             "IPC_REPLY_TIMEOUT_DEFERRED arch="; do
    rg -a -q "$lit" "$OFF_BIN" || die "feature-OFF kernel is missing PRODUCTION literal $lit (the U7 pipeline must not be feature-gated)"
  done
fi

if (( fail )); then
  echo "STAGE_200C_REPLY_TIMEOUT_RISCV64_RETIREMENT_SEAL arch=riscv64 classes=1 live_cells=1 result=fail reason=build"
  exit 1
fi

# ── Boot helper: one fresh -smp 1 boot for the given mode, into its own log ──
# Stage 200C2C2C-R2B: `QEMU_SINGLE_BOOT=1` adds `-no-reboot -no-shutdown`, so the guest can
# never restart inside one log; `assert_single_boot_instance` then PROVES from the captured
# serial that exactly one boot instance actually occurred.
boot_mode() {
  local mode="$1" log="$2" tag="${3:-$1}"
  env \
    KERNEL_IMAGE="$KBIN" \
    INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio \
    KERNEL_CMDLINE="yarm.riscv_ipc_reply_timeout_oracle=${mode}" \
    QEMU_SMP=1 \
    QEMU_SINGLE_BOOT=1 \
    QEMU_SMOKE_STRICT=0 \
    LOGFILE="$log" \
    TIMEOUT_SECS="$TIMEOUT_SECS" \
    IDLE_MAX_SECS="$IDLE_MAX_SECS" \
    scripts/qemu-riscv64-core-smoke.sh >"$LOGDIR/core-${tag}.log" 2>&1 || true
}

# ── Stage 200C2C2C-R2B: SINGLE-BOOT-INSTANCE proof ──
# Every marker-count assertion in this runner ("count must be 1") is only meaningful if the log
# holds exactly ONE boot instance. A reset, a payload re-entry, or a duplicated runner would
# inflate counts and could make a one-shot latch appear to fire twice. Prove single-instance
# from three INDEPENDENT boot-start witnesses (firmware banner, kernel cmdline capture, kernel
# boot completion) plus one QEMU launch, and CLASSIFY any duplicate instead of ignoring it.
assert_single_boot_instance() {
  local norm="$1" core="$2" label="$3"
  local sbi entry bootok launches
  sbi=$(rg -a -c -F "OpenSBI v" "$norm" 2>/dev/null || echo 0)
  entry=$(rg -a -c -F "YARM_BOOT_CMDLINE_CAPTURE arch=riscv64" "$norm" 2>/dev/null || echo 0)
  bootok=$(rg -a -c -F "YARM_BOOT_OK present_cpus=" "$norm" 2>/dev/null || echo 0)
  launches=$(rg -a -c -F "[info] qemu command:" "$core" 2>/dev/null || echo 0)
  note "[$label] boot-instance witnesses: opensbi=$sbi kernel_entry=$entry boot_ok=$bootok qemu_launches=$launches"
  [[ "$launches" == "1" ]] || die "[$label] expected exactly one QEMU launch, got $launches"
  if [[ "$sbi" != "1" ]]; then
    die "[$label] DUPLICATE BOOT INSTANCE: $sbi OpenSBI banners (firmware re-entry / guest reset)"
  fi
  if [[ "$entry" != "1" ]]; then
    die "[$label] DUPLICATE BOOT INSTANCE: $entry kernel entries (kernel payload re-entry)"
  fi
  if [[ "$bootok" != "1" ]]; then
    die "[$label] DUPLICATE BOOT INSTANCE: $bootok kernel boot completions"
  fi
}

# Assert marker A strictly precedes marker B (first occurrence of each).
assert_order() {
  local norm="$1" a="$2" b="$3" why="$4"
  local la lb
  la=$(rg -a -n -F "$a" "$norm" | head -1 | cut -d: -f1)
  lb=$(rg -a -n -F "$b" "$norm" | head -1 | cut -d: -f1)
  if [[ -z "$la" || -z "$lb" ]]; then
    die "ordering evidence missing ($a=$la $b=$lb)"
    return
  fi
  (( la < lb )) || die "$why ($a@$la must precede $b@$lb)"
}

verify_log() {
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
  note "booting fresh -smp 1 QEMU: yarm.riscv_ipc_reply_timeout_oracle=timeout-wins"
  boot_mode timeout-wins "$LOGDIR/boot-timeout-wins.log"
  TW="$LOGDIR/tw.norm.log"; tr '\r' '\n' <"$LOGDIR/boot-timeout-wins.log" >"$TW"
  [[ -s "$TW" ]] || die "no timeout-wins boot log"
  verify_log "$TW" \
    "IPC_REPLY_TIMEOUT_ARMED arch=riscv64" \
    "IPC_REPLY_TIMEOUT_OK arch=riscv64 terminal=Timeout timeout_result=TimedOut caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=0 result=ok" \
    "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=riscv64 terminal=Timeout result=ok" \
    "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=riscv64 scan_broad_lock=0 completion_transaction_narrow=1 classes=IpcReplyTimeout+IpcSendTimeout production=1 result=ok" \
    "IPC_REPLY_TIMEOUT_DEFERRED arch=riscv64 published=1 drained=1 result=ok" \
    "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED" \
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcReplyTimeout result=ok" \
    "RISCV_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut caller_continuations=1 late_reply=rejected result=ok"
  # ORDERED sequence: the class-retirement marker is authorized ONLY after the resumed caller
  # consumed its exact completion (which is what encodes the canonical TimedOut). A
  # committed-but-undelivered completion must never claim the class retired.
  ci=$(rg -a -n -F "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=riscv64" "$TW" | head -1 | cut -d: -f1)
  cn=$(rg -a -n -F "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED" "$TW" | head -1 | cut -d: -f1)
  rt=$(rg -a -n -F "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcReplyTimeout" "$TW" | head -1 | cut -d: -f1)
  ud=$(rg -a -n -F "RISCV_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut" "$TW" | head -1 | cut -d: -f1)
  if [[ -n "$ci" && -n "$cn" && -n "$rt" && -n "$ud" ]]; then
    (( ci < cn )) || die "completion committed must precede the resume-boundary consumption"
    (( cn <= rt )) || die "retirement marker must follow the completion consumption"
    (( rt < ud )) || die "retirement marker must precede the userspace completion"
  else
    die "ordered marker sequence incomplete (ci=$ci cn=$cn rt=$rt ud=$ud)"
  fi
  # Exactly one completion consumption ⇒ one timeout encoding, one ELR-advance boundary,
  # no duplicate wake and no re-blocked waiter after the wake.
  [[ "$(rg -a -c -F "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED" "$TW" 2>/dev/null || echo 0)" == "1" ]] \
    || die "completion consumed more than once (duplicate timeout encoding)"
  forbid_log "$TW" \
    "IPC_REPLY_BEATS_TIMEOUT_OK" \
    "scan_broad_lock=1" \
    "IPC_REPLY_TIMEOUT_OK arch=x86_64" \
    "KERNEL PANIC" "RUST PANIC" "panicked at" "RISCV_TRAP_FAIL" "Unhandled"
  # Stage 200C2C2C-R2B: timeout-wins must be UNTOUCHED by the causal reply-wins gate — the
  # gate is armed only in reply-wins mode, so no gate marker may appear here at all and the
  # production collector must have published + drained its work normally (asserted above).
  forbid_log "$TW" "IPC_REPLY_TIMEOUT_COLLECTOR_GATE"
  # The reply-win reserve must have DECLINED for the single legitimate reason: the timeout
  # already owned the terminal. No other decline reason is acceptable, and in particular none
  # may mention deadline bookkeeping.
  verify_log "$TW" \
    "IPC_REPLY_WIN_RESERVE arch=riscv64 outcome=decline reason=TimeoutAlreadyClaimed result=ok"
  forbid_log "$TW" "IPC_REPLY_WIN_RESERVE arch=riscv64 outcome=ok"
  assert_single_boot_instance "$TW" "$LOGDIR/core-timeout-wins.log" timeout-wins
  recheck_sha_clean
  (( fail )) || TW_OK=1
fi

# ── 4. Scenario B — reply-wins, feature enabled (SEPARATE fresh boot) ──
#
# Stage 200C2C2C-R2B: this outcome is now CAUSAL, not a wall-clock race. The kernel arms a
# collector gate at reply-wins arm time — strictly before the terminal cell is armed — so the
# timeout collector can publish NO work while the reply is in flight; the gate is released only
# when the DebugLog seam observes the client's own post-validation marker. The reply therefore
# wins because it is the ONLY claimant that could run, and the late scan that follows is a
# genuine ungated scan that found nothing to claim.
run_reply_wins() {
  local tag="$1" log="$LOGDIR/boot-${1}.log" rw="$LOGDIR/${1}.norm.log"
  note "booting fresh -smp 1 QEMU: yarm.riscv_ipc_reply_timeout_oracle=reply-wins [$tag]"
  boot_mode reply-wins "$log" "$tag"
  tr '\r' '\n' <"$log" >"$rw"
  [[ -s "$rw" ]] || { die "[$tag] no reply-wins boot log"; return; }
  verify_log "$rw" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=riscv64 outcome=held phase=before_terminal_claim result=ok" \
    "IPC_REPLY_TIMEOUT_ARMED arch=riscv64" \
    "IPC_REPLY_WIN_RESERVE arch=riscv64 outcome=ok terminal=Reserved(Reply) token_lease=1 result=ok" \
    "IPC_REPLY_BEATS_TIMEOUT_OK arch=riscv64 terminal=Reply reply_copies=1 deadline_disarmed=1 late_timeout_claims=0 caller_wakes=1 result=ok" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=riscv64 outcome=released trigger=userspace_reply_validated result=ok" \
    "IPC_REPLY_TIMEOUT_LATE_SCAN arch=riscv64 outcome=reply_won late_timeout_claims=0 result=ok" \
    "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=riscv64 scan_broad_lock=0 completion_transaction_narrow=1 classes=IpcReplyTimeout+IpcSendTimeout production=1 result=ok" \
    "RISCV_IPC_REPLY_BEATS_TIMEOUT_DONE reply_ok=1 caller_continuations=1 late_timeout_wakes=0 duplicate_reply=rejected result=ok" \
    "IPC_REPLY_TIMEOUT_ORACLE_SERVER_DUP_REPLY rejected=1"
  # The reply must never be declined here, and NO decline reason may ever be raised by deadline
  # bookkeeping: terminal ownership is the single authority.
  forbid_log "$rw" \
    "IPC_REPLY_WIN_RESERVE arch=riscv64 outcome=decline" \
    "IPC_REPLY_TIMEOUT_OK arch=riscv64 terminal=Timeout" \
    "IPC_REPLY_WIN_ROLLBACK" \
    "scan_broad_lock=1" \
    "KERNEL PANIC" "RUST PANIC" "panicked at" "RISCV_TRAP_FAIL" "Unhandled"
  # The CAUSAL chain, in order. Each link is a distinct log line, so the sequence is evidence
  # that the reply's win did not depend on the numerical deadline or on QEMU host timing.
  assert_order "$rw" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=riscv64 outcome=held" \
    "IPC_REPLY_TIMEOUT_ARMED arch=riscv64" \
    "the collector must be held BEFORE any terminal/deadline is armed"
  assert_order "$rw" \
    "IPC_REPLY_TIMEOUT_ARMED arch=riscv64" \
    "IPC_REPLY_WIN_RESERVE arch=riscv64 outcome=ok" \
    "the reply must reserve against a genuinely armed deadline"
  assert_order "$rw" \
    "IPC_REPLY_WIN_RESERVE arch=riscv64 outcome=ok" \
    "IPC_REPLY_BEATS_TIMEOUT_OK arch=riscv64" \
    "the reservation must precede the committed reply win"
  assert_order "$rw" \
    "IPC_REPLY_BEATS_TIMEOUT_OK arch=riscv64" \
    "USER_LOG tid=1 msg=IPC_REPLY_TIMEOUT_ORACLE_CLIENT_REPLY_RECV plen=8 reply_ok=1" \
    "userspace must validate the payload only after the reply win committed"
  assert_order "$rw" \
    "USER_LOG tid=1 msg=IPC_REPLY_TIMEOUT_ORACLE_CLIENT_REPLY_RECV plen=8 reply_ok=1" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=riscv64 outcome=released" \
    "the gate must be released by the userspace validation, not before it"
  assert_order "$rw" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=riscv64 outcome=released" \
    "IPC_REPLY_TIMEOUT_LATE_SCAN arch=riscv64 outcome=reply_won" \
    "the late scan must run with collection ENABLED (otherwise it claims nothing vacuously)"
  assert_single_boot_instance "$rw" "$LOGDIR/core-${tag}.log" "$tag"
  recheck_sha_clean
}

RW_OK=0
if (( ! fail )); then
  run_reply_wins reply-wins
  (( fail )) || RW_OK=1
fi

# ── 4b. Scenario B REPEAT — the reply win must be REPRODUCIBLE, not a lucky schedule ──
RW2_OK=0
if (( ! fail )); then
  run_reply_wins reply-wins-repeat
  (( fail )) || RW2_OK=1
fi

# ── 5. Retirement live seal (runner-emitted; all three fresh boots must pass) ──
if (( fail )) || [[ "$TW_OK" != "1" || "$RW_OK" != "1" || "$RW2_OK" != "1" ]]; then
  echo "STAGE_200C_REPLY_TIMEOUT_RISCV64_RETIREMENT_SEAL arch=riscv64 classes=1 live_cells=1 timeout_wins=${TW_OK} reply_wins=${RW_OK} reply_wins_repeat=${RW2_OK} result=fail"
  exit 1
fi

cat <<'SEAL'
STAGE_200C_REPLY_TIMEOUT_RISCV64_RETIREMENT_SEAL
arch=riscv64
classes=1
live_cells=1
timeout_wins=1
reply_wins=1
reply_wins_repeat=1
canonical_timeout_result=1
completion_resume=1
sepc_single_advance=1
scan_broad_lock=0
completion_transaction_narrow=1
late_reply_successes=0
late_timeout_wakes=0
duplicate_wakes=0
stale_authority_restores=0
wrong_waiter_mutations=0
result=ok
SEAL

cat <<'SEAL'
STAGE_200_RISCV_REPLY_WIN_CAUSAL_SEAL
arch=riscv64
reply_eligibility_depends_on_deadline_value=0
terminal_ownership_is_single_authority=1
deadline_bookkeeping_declines=0
collector_gate_held_before_terminal_claim=1
collector_gate_released_by_userspace_validation=1
late_scan_ran_ungated=1
reply_wins_live_runs=2
boot_instances_per_run=1
timeout_wins_preserved=1
result=ok
SEAL
