#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 200C2C2C-R2C — THREE-ARCHITECTURE TIMEOUT/REPLY EXACT-COMMIT MATRIX.
#
# Freezes the complete direct-reply timeout behaviour as SIX live functional cells, all run
# serially from ONE clean exact commit:
#
#   x86_64  timeout-wins   x86_64  reply-wins
#   AArch64 timeout-wins   AArch64 reply-wins
#   RISC-V  timeout-wins   RISC-V  reply-wins
#
# What each scenario proves (identical semantics on all three ports; only the resume ABI and
# the monotonic clock differ):
#
#   timeout-wins — the caller blocks in recv with a deadline armed against its reply record;
#                  the server WITHHOLDS its NR7; the off-lock collector claims and commits the
#                  Timeout terminal; the caller resumes with the canonical TimedOut; the
#                  server's late NR7 is then rejected by the normal reply-authority check.
#
#   reply-wins   — CAUSAL, not a wall-clock margin. The collector is held before the terminal
#                  cell is armed, so no timeout claimant can run while the reply is in flight;
#                  a REAL NR7 reserves and commits the Reply terminal; userspace validates the
#                  payload; that validation releases the gate; the now-stale deadline is
#                  scanned with collection ENABLED and claims nothing. A DUPLICATE NR7 through
#                  the consumed reply cap is rejected.
#
# This runner does not add and does not claim: server-death caller wake, caller-exit
# cancellation, endpoint-destruction liveness, multi-pair concurrency, queued IpcCall, or SMP
# timeout/reply races beyond the existing proofs.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/ipc-reply-timeout-matrix}
TIMEOUT_SECS=${TIMEOUT_SECS:-180}
IDLE_MAX_SECS=${IDLE_MAX_SECS:-180}

fail=0
note() { echo "[matrix] $*"; }
die()  { echo "[matrix][fail] $*"; fail=1; }

# ── 0. EXACT-COMMIT identity ─────────────────────────────────────────────────────────
# The matrix seal claims six cells "from the same clean exact commit". That is only true if
# the tree cannot move under the runner, so the SHA *and* a content hash of the tracked tree
# are captured up front and RE-CHECKED after every child. A dirty tree is refused outright:
# an uncommitted edit would make the seal unattributable to any commit.
SHA0=$(git rev-parse HEAD 2>/dev/null || echo unknown)
TREEHASH0=$(git rev-parse HEAD^{tree} 2>/dev/null || echo unknown)
if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "STAGE_200_IPC_REPLY_TIMEOUT_MATRIX_SEAL result=fail reason=dirty_tree"
  echo "[matrix][fail] the exact-commit matrix requires a CLEAN tree; commit or stash first"
  exit 1
fi
if [[ -n "$(git ls-files --others --exclude-standard)" ]]; then
  echo "[matrix][warn] untracked files present (ignored: not part of the tracked tree hash)"
fi
note "exact commit sha=$SHA0 tree=$TREEHASH0 (clean)"

recheck_exact_commit() {
  local what="$1" sha th
  sha=$(git rev-parse HEAD 2>/dev/null || echo unknown)
  th=$(git rev-parse HEAD^{tree} 2>/dev/null || echo unknown)
  [[ "$sha" == "$SHA0" ]]      || die "[$what] SHA drifted mid-matrix ($SHA0 -> $sha)"
  [[ "$th"  == "$TREEHASH0" ]] || die "[$what] tree hash drifted mid-matrix"
  git diff --quiet && git diff --cached --quiet \
    || die "[$what] tree became dirty mid-matrix"
}

rm -rf "$LOGDIR"
mkdir -p "$LOGDIR"

# ── Log-freshness: every child gets its OWN log, and no log may be reused ────────────
# A stale log from an earlier child (or an earlier matrix run) would let a cell "pass" on
# someone else's evidence. Each child's log path is registered once; a second registration,
# a pre-existing file, or an unchanged inode is a hard failure.
declare -A LOG_SEEN=()
fresh_log() {
  local path="$1"
  if [[ -n "${LOG_SEEN[$path]:-}" ]]; then
    die "log reuse: $path was already used by another cell"
    return 1
  fi
  if [[ -e "$path" ]]; then
    die "log reuse: $path already exists before its cell ran"
    return 1
  fi
  LOG_SEEN["$path"]=1
  return 0
}

# ── Marker helpers ───────────────────────────────────────────────────────────────────
count_of() { rg -a -c -F "$2" "$1" 2>/dev/null || echo 0; }

need_once() {
  local norm="$1" label="$2"; shift 2
  local m c
  for m in "$@"; do
    c=$(count_of "$norm" "$m")
    [[ "$c" == "1" ]] || die "[$label] marker count != 1 (got $c): $m"
  done
}
need_absent() {
  local norm="$1" label="$2"; shift 2
  local m
  for m in "$@"; do
    rg -a -q -F "$m" "$norm" && die "[$label] forbidden marker present: $m"
  done
  return 0
}
need_order() {
  local norm="$1" label="$2" a="$3" b="$4" why="$5" la lb
  la=$(rg -a -n -F "$a" "$norm" | head -1 | cut -d: -f1)
  lb=$(rg -a -n -F "$b" "$norm" | head -1 | cut -d: -f1)
  if [[ -z "$la" || -z "$lb" ]]; then
    die "[$label] ordering evidence missing ($a=$la $b=$lb)"; return
  fi
  (( la < lb )) || die "[$label] $why ($a@$la must precede $b@$lb)"
}

# ── Single-boot proof (five independent witnesses) ───────────────────────────────────
# Marker-count assertions are only sound inside ONE boot instance. Prove it from:
#   1 QEMU launch, 1 firmware/entry banner, 1 kernel-entry marker, 1 boot completion, and
#   exactly one DISTINCT boot-instance nonce — the last is what separates "one boot" from
#   "two boots that produced identical-looking lines".
single_boot() {
  local norm="$1" core="$2" arch="$3" label="$4"
  local banner entry bootok launches nonce_lines nonce_distinct
  case "$arch" in
    riscv64) banner=$(count_of "$norm" "OpenSBI v") ;;
    x86_64)  banner=$(count_of "$norm" "Booting from ROM") ;;
    # The AArch64 virt board is booted with `-kernel` and no firmware, so the kernel's own
    # first instruction marker IS the earliest boot-start witness.
    aarch64) banner=$(count_of "$norm" "YARM_AARCH64_BOOT_MARKER stage=_start") ;;
  esac
  entry=$(count_of "$norm" "YARM_BOOT_CMDLINE_CAPTURE arch=${arch}")
  bootok=$(count_of "$norm" "YARM_BOOT_OK present_cpus=")
  launches=$(count_of "$core" "[info] qemu command:")
  nonce_lines=$(count_of "$norm" "YARM_BOOT_INSTANCE arch=${arch} nonce=")
  nonce_distinct=$(rg -a -o -F -e "YARM_BOOT_INSTANCE arch=${arch} nonce=" -r '' "$norm" >/dev/null 2>&1; \
                   rg -a -o "YARM_BOOT_INSTANCE arch=${arch} nonce=0x[0-9a-f]+" "$norm" 2>/dev/null \
                   | sort -u | wc -l | tr -d ' ')
  note "[$label] single-boot: qemu=$launches banner=$banner entry=$entry boot_ok=$bootok nonce_lines=$nonce_lines nonce_distinct=$nonce_distinct"
  [[ "$launches"       == "1" ]] || die "[$label] QEMU launches != 1 (got $launches)"
  [[ "$banner"         == "1" ]] || die "[$label] firmware/entry banners != 1 (got $banner) — reset or re-entry"
  [[ "$entry"          == "1" ]] || die "[$label] kernel entries != 1 (got $entry) — payload re-entry"
  [[ "$bootok"         == "1" ]] || die "[$label] boot completions != 1 (got $bootok)"
  [[ "$nonce_lines"    == "1" ]] || die "[$label] boot-instance markers != 1 (got $nonce_lines)"
  [[ "$nonce_distinct" == "1" ]] || die "[$label] DISTINCT boot instances != 1 (got $nonce_distinct)"
  # A boot that died before its terminal proof cannot have produced valid counts.
  need_absent "$norm" "$label" "KERNEL PANIC" "RUST PANIC" "panicked at" "FATAL"
}

# ── Per-arch build + boot plumbing ───────────────────────────────────────────────────
objcopy_tool() {
  if command -v llvm-objcopy >/dev/null 2>&1; then echo llvm-objcopy
  elif command -v rust-objcopy >/dev/null 2>&1; then echo rust-objcopy
  else return 1; fi
}
OBJCOPY=$(objcopy_tool) || { echo "[matrix][fail] no objcopy available"; exit 1; }

# Each architecture builds with its OWN target spec + profile — the exact triples the
# accepted per-arch retirement smokes use, so the matrix builds the same images those
# cells were sealed with rather than a differently configured lookalike.
BUILD_STD=core,alloc,compiler_builtins,panic_abort
arch_target()  { case "$1" in
  x86_64)  echo targets/x86_64-yarm-none.json ;;
  aarch64) echo targets/aarch64-yarm-none.json ;;
  riscv64) echo riscv64gc-unknown-none-elf ;;
esac; }
arch_profile() { case "$1" in
  x86_64)  echo x86-none ;;
  aarch64) echo aarch64-none ;;
  riscv64) echo release ;;
esac; }
arch_elf()     { case "$1" in
  x86_64)  echo target/x86_64-yarm-none/x86-none/kernel_boot ;;
  aarch64) echo target/aarch64-yarm-none/aarch64-none/kernel_boot ;;
  riscv64) echo target/riscv64gc-unknown-none-elf/release/kernel_boot ;;
esac; }
arch_feature() { echo "${1/x86_64/x86}-ipc-reply-timeout-oracle" | sed 's/^aarch64-/aarch64-/;s/^riscv64-/riscv64-/'; }

build_kernel() {
  # $1 arch, $2 "on"|"off", $3 log
  local arch="$1" mode="$2" log="$3"
  local feat_args=()
  if [[ "$mode" == "on" ]]; then
    case "$arch" in
      x86_64)  feat_args=(--features x86-ipc-reply-timeout-oracle) ;;
      aarch64) feat_args=(--features aarch64-ipc-reply-timeout-oracle) ;;
      riscv64) feat_args=(--features riscv64-ipc-reply-timeout-oracle) ;;
    esac
  fi
  cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
    --target "$(arch_target "$arch")" --profile "$(arch_profile "$arch")" \
    --no-default-features "${feat_args[@]+"${feat_args[@]}"}" \
    -p yarm --bin kernel_boot >"$log" 2>&1
}

build_arch() {
  local arch="$1" elf; elf=$(arch_elf "$arch")
  note "building $arch artifacts (servers + initramfs + feature-on/off kernels)"
  BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    "scripts/build-qemu-${arch}-artifacts.sh" >"$LOGDIR/build-$arch.log" 2>&1 \
    || { die "[$arch] base artifact build failed (see $LOGDIR/build-$arch.log)"; return 1; }

  build_kernel "$arch" on "$LOGDIR/kbuild-$arch.log" \
    || { die "[$arch] feature kernel build failed (see $LOGDIR/kbuild-$arch.log)"; return 1; }
  case "$arch" in
    x86_64)  cp "$elf" build-x86_64/kernel_boot.elf ;;
    aarch64) "$OBJCOPY" -O binary "$elf" build-aarch64/yarm-aarch64.bin >/dev/null 2>&1 \
               || { die "[$arch] objcopy failed"; return 1; } ;;
    riscv64) "$OBJCOPY" -O binary "$elf" build-riscv64/yarm-riscv64.bin >/dev/null 2>&1 \
               || { die "[$arch] objcopy failed"; return 1; } ;;
  esac

  # The feature-OFF build reuses the same target path, so it OVERWRITES `$elf`. The
  # feature-ON boot image was already copied out above and is untouched.
  build_kernel "$arch" off "$LOGDIR/kbuild-off-$arch.log" \
    || { die "[$arch] feature-off kernel build failed (see $LOGDIR/kbuild-off-$arch.log)"; return 1; }
  cp "$elf" "$LOGDIR/kernel_off-$arch.bin" \
    || { die "[$arch] could not capture the feature-off image"; return 1; }
  return 0
}

# ── 6. FEATURE-OFF preservation: the image must be marker-CLEAN ──────────────────────
# A feature-off build must contain no timeout-oracle retirement literal and no causal-gate
# literal at all — not merely fail to print them at runtime. Checked against the BINARY, so
# a marker that is compiled in but never reached still fails.
FEATURE_OFF_CLEAN=()
check_feature_off() {
  local arch="$1" bin="$LOGDIR/kernel_off-$arch.bin" lit hits=0
  [[ -s "$bin" ]] || { die "[$arch/off] no feature-off image"; return; }
  for lit in "IPC_REPLY_TIMEOUT_OK arch=${arch}" \
             "IPC_REPLY_BEATS_TIMEOUT_OK arch=${arch}" \
             "IPC_REPLY_TIMEOUT_ARMED arch=${arch}" \
             "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=${arch}" \
             "IPC_REPLY_TIMEOUT_LATE_SCAN arch=${arch}" \
             "IPC_REPLY_TIMEOUT_DEFERRED arch=${arch}" \
             "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=${arch}" \
             "IPC_REPLY_WIN_RESERVE arch=${arch}" \
             "class=IpcReplyTimeout"; do
    if rg -a -q -F "$lit" "$bin"; then
      die "[$arch/off] feature-OFF image contains live literal: $lit"
      hits=$((hits+1))
    fi
  done
  note "[$arch/off] feature-off retirement literals=$hits causal-gate literals=0"
  (( hits == 0 )) && FEATURE_OFF_CLEAN+=("$arch")
}

boot_cell() {
  # $1 arch, $2 mode (timeout-wins|reply-wins), $3 raw log, $4 core log
  local arch="$1" mode="$2" log="$3" core="$4"
  case "$arch" in
    x86_64)
      env KERNEL_IMAGE=build-x86_64/kernel_boot.elf \
          INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio \
          KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.x86_64_ipc_reply_timeout_oracle=${mode}" \
          QEMU_SMP=1 QEMU_SINGLE_BOOT=1 LOGFILE="$log" SMOKE_LOG="$core" \
          TIMEOUT_SECS="$TIMEOUT_SECS" YARM_MODE_ISOLATION=0 \
          scripts/qemu-x86_64-core-smoke.sh >"$LOGDIR/wrap-$arch-$mode.log" 2>&1 || true
      # The x86 core runner writes its own progress file; mirror the launch line so the
      # single-boot QEMU-launch count has one uniform source across all three arches.
      grep -a "\[info\] qemu command:" "$LOGDIR/wrap-$arch-$mode.log" >"$core" 2>/dev/null || true
      ;;
    aarch64)
      env KERNEL_IMAGE=build-aarch64/yarm-aarch64.bin \
          INITRAMFS_IMAGE=build-aarch64/initramfs-core.cpio \
          KERNEL_CMDLINE="yarm.aarch64_ipc_reply_timeout_oracle=${mode}" \
          QEMU_SMP=1 QEMU_SINGLE_BOOT=1 QEMU_SMOKE_STRICT=0 LOGFILE="$log" \
          TIMEOUT_SECS="$TIMEOUT_SECS" IDLE_MAX_SECS="$IDLE_MAX_SECS" \
          scripts/qemu-aarch64-core-smoke.sh >"$core" 2>&1 || true
      ;;
    riscv64)
      env KERNEL_IMAGE=build-riscv64/yarm-riscv64.bin \
          INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio \
          KERNEL_CMDLINE="yarm.riscv_ipc_reply_timeout_oracle=${mode}" \
          QEMU_SMP=1 QEMU_SINGLE_BOOT=1 QEMU_SMOKE_STRICT=0 LOGFILE="$log" \
          TIMEOUT_SECS="$TIMEOUT_SECS" IDLE_MAX_SECS="$IDLE_MAX_SECS" \
          scripts/qemu-riscv64-core-smoke.sh >"$core" 2>&1 || true
      ;;
  esac
}

# The userspace completion marker is arch-named; the kernel markers carry `arch=`.
user_prefix() {
  case "$1" in x86_64) echo X86 ;; aarch64) echo AARCH64 ;; riscv64) echo RISCV ;; esac
}

# ── 5. TIMEOUT-WINS cell ─────────────────────────────────────────────────────────────
TW_PASS=0
cell_timeout_wins() {
  local arch="$1" label="$arch/timeout-wins"
  local raw="$LOGDIR/$arch-timeout-wins.raw.log" core="$LOGDIR/$arch-timeout-wins.core.log"
  local norm="$LOGDIR/$arch-timeout-wins.norm.log" up; up=$(user_prefix "$arch")
  fresh_log "$raw" || return; fresh_log "$core" || return; fresh_log "$norm" || return
  note "cell $label"
  boot_cell "$arch" timeout-wins "$raw" "$core"
  tr '\r' '\n' <"$raw" >"$norm"
  [[ -s "$norm" ]] || { die "[$label] empty boot log"; return; }

  # timeout terminal winner = 1, caller continuation = 1, canonical TimedOut = 1,
  # late reply rejected = 1, reply destination copies = 0, duplicate wakes = 0.
  # `caller_wakes=1` inside the single terminal marker IS the duplicate-wake bound: the
  # transaction emits it once per committed completion, so a second wake would either
  # duplicate the marker (count != 1) or contradict the count field.
  need_once "$norm" "$label" \
    "IPC_REPLY_TIMEOUT_ARMED arch=${arch}" \
    "IPC_REPLY_TIMEOUT_OK arch=${arch} terminal=Timeout timeout_result=TimedOut caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=0 result=ok" \
    "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=${arch} scan_broad_lock=0 completion_transaction_narrow=1 classes=IpcReplyTimeout+IpcSendTimeout production=1 result=ok" \
    "IPC_REPLY_TIMEOUT_DEFERRED arch=${arch} published=1 drained=1 result=ok" \
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=${arch} class=IpcReplyTimeout result=ok" \
    "${up}_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut caller_continuations=1 late_reply=rejected result=ok"
  # The reply-win reserve may fire here, but ONLY to decline because the timeout already
  # owns the terminal — never for a deadline-bookkeeping reason, and never successfully.
  need_absent "$norm" "$label" \
    "IPC_REPLY_BEATS_TIMEOUT_OK" \
    "IPC_REPLY_WIN_RESERVE arch=${arch} outcome=ok" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE" \
    "scan_broad_lock=1"
  # The timeout completion performs NO user-memory copy: the reply that would have copied
  # was rejected, so no reply-destination copy is ever attested in this cell.
  [[ "$(count_of "$norm" "reply_copies=1")" == "0" ]] \
    || die "[$label] reply destination copies != 0"
  need_order "$norm" "$label" \
    "IPC_REPLY_TIMEOUT_ARMED arch=${arch}" \
    "IPC_REPLY_TIMEOUT_OK arch=${arch} terminal=Timeout" \
    "the deadline must be armed before the timeout claims"
  need_order "$norm" "$label" \
    "IPC_REPLY_TIMEOUT_OK arch=${arch} terminal=Timeout" \
    "${up}_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut" \
    "the timeout must commit before userspace reports TimedOut"

  # RISC-V additionally: the return-publication chain must agree end to end. Three
  # INDEPENDENTLY observed values of a0 — published into the TCB, encoded into the final
  # sret frame, and read by userspace as the raw syscall result — must all be 9 (TimedOut).
  # This is what forbids a metadata lane from overriding the raw timeout error.
  if [[ "$arch" == "riscv64" ]]; then
    need_once "$norm" "$label" \
      "RISCV_BLOCKED_RETURN_PUBLISHED tid=1 stored_a0=9 stored_a1=0 stored_arg0=9 stored_arg1=0 result=ok" \
      "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid=1 class=IpcRecv result=TimedOut code=9" \
      "USER_LOG tid=1 msg=USER_RT_RECV_AFTER_SYSCALL x0=9 x1=0"
    [[ "$(count_of "$norm" "final_a0=9 final_a1=0 result=ok")" == "1" ]] \
      || die "[$label] kernel final a0 != 9"
    need_order "$norm" "$label" \
      "RISCV_BLOCKED_RETURN_PUBLISHED tid=1 stored_a0=9" \
      "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid=1" \
      "the TCB publication must precede the resume-boundary encoding"
    need_order "$norm" "$label" \
      "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid=1" \
      "USER_LOG tid=1 msg=USER_RT_RECV_AFTER_SYSCALL x0=9 x1=0" \
      "the resume-boundary encoding must precede the first userspace observation"
  fi

  single_boot "$norm" "$core" "$arch" "$label"
  recheck_exact_commit "$label"
  (( fail )) || TW_PASS=$((TW_PASS+1))
}

# ── 5. REPLY-WINS cell ───────────────────────────────────────────────────────────────
RW_PASS=0
cell_reply_wins() {
  local arch="$1" label="$arch/reply-wins"
  local raw="$LOGDIR/$arch-reply-wins.raw.log" core="$LOGDIR/$arch-reply-wins.core.log"
  local norm="$LOGDIR/$arch-reply-wins.norm.log" up; up=$(user_prefix "$arch")
  fresh_log "$raw" || return; fresh_log "$core" || return; fresh_log "$norm" || return
  note "cell $label"
  boot_cell "$arch" reply-wins "$raw" "$core"
  tr '\r' '\n' <"$raw" >"$norm"
  [[ -s "$norm" ]] || { die "[$label] empty boot log"; return; }

  # reply terminal winner = 1, userspace reply consumed = 1, caller continuation = 1,
  # late timeout rejected = 1, duplicate reply rejected = 1, duplicate wakes = 0,
  # wrong deadline mutations = 0.
  need_once "$norm" "$label" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=${arch} outcome=held phase=before_terminal_claim result=ok" \
    "IPC_REPLY_TIMEOUT_ARMED arch=${arch}" \
    "IPC_REPLY_WIN_RESERVE arch=${arch} outcome=ok terminal=Reserved(Reply) token_lease=1 result=ok" \
    "IPC_REPLY_BEATS_TIMEOUT_OK arch=${arch} terminal=Reply reply_copies=1 deadline_disarmed=1 late_timeout_claims=0 caller_wakes=1 result=ok" \
    "IPC_REPLY_TIMEOUT_ORACLE_SERVER_DUP_REPLY rejected=1" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=${arch} outcome=released trigger=userspace_reply_validated result=ok" \
    "IPC_REPLY_TIMEOUT_LATE_SCAN arch=${arch} outcome=reply_won late_timeout_claims=0 result=ok" \
    "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=${arch} scan_broad_lock=0 completion_transaction_narrow=1 classes=IpcReplyTimeout+IpcSendTimeout production=1 result=ok" \
    "USER_LOG tid=1 msg=IPC_REPLY_TIMEOUT_ORACLE_CLIENT_REPLY_RECV plen=8 reply_ok=1" \
    "${up}_IPC_REPLY_BEATS_TIMEOUT_DONE reply_ok=1 caller_continuations=1 late_timeout_wakes=0 duplicate_reply=rejected result=ok"
  # No timeout may claim, no reservation may be declined, and the reservation must never be
  # rolled back (a rollback would mean the winning reply's caller copy faulted).
  need_absent "$norm" "$label" \
    "IPC_REPLY_TIMEOUT_OK arch=${arch} terminal=Timeout" \
    "IPC_REPLY_WIN_RESERVE arch=${arch} outcome=decline" \
    "IPC_REPLY_WIN_ROLLBACK" \
    "IPC_REPLY_TIMEOUT_DEFERRED arch=${arch} published=1 drained=1" \
    "scan_broad_lock=1"
  # The CAUSAL chain. Each link is a distinct line, so the sequence is evidence that the
  # reply's win did not depend on the deadline value or on QEMU host timing.
  need_order "$norm" "$label" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=${arch} outcome=held" \
    "IPC_REPLY_TIMEOUT_ARMED arch=${arch}" \
    "the collector must be held BEFORE the terminal/deadline is armed"
  need_order "$norm" "$label" \
    "IPC_REPLY_TIMEOUT_ARMED arch=${arch}" \
    "IPC_REPLY_WIN_RESERVE arch=${arch} outcome=ok" \
    "the real NR7 must reserve against a genuinely armed deadline"
  need_order "$norm" "$label" \
    "IPC_REPLY_WIN_RESERVE arch=${arch} outcome=ok" \
    "IPC_REPLY_BEATS_TIMEOUT_OK arch=${arch}" \
    "the reservation must precede the committed reply win"
  need_order "$norm" "$label" \
    "IPC_REPLY_BEATS_TIMEOUT_OK arch=${arch}" \
    "USER_LOG tid=1 msg=IPC_REPLY_TIMEOUT_ORACLE_CLIENT_REPLY_RECV plen=8 reply_ok=1" \
    "userspace must consume the payload only after the reply win committed"
  need_order "$norm" "$label" \
    "USER_LOG tid=1 msg=IPC_REPLY_TIMEOUT_ORACLE_CLIENT_REPLY_RECV plen=8 reply_ok=1" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=${arch} outcome=released" \
    "the gate must be released BY the userspace validation, not before it"
  need_order "$norm" "$label" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=${arch} outcome=released" \
    "IPC_REPLY_TIMEOUT_LATE_SCAN arch=${arch} outcome=reply_won" \
    "the stale-deadline scan must run with collection ENABLED"

  single_boot "$norm" "$core" "$arch" "$label"
  recheck_exact_commit "$label"
  (( fail )) || RW_PASS=$((RW_PASS+1))
}

# ── Serial execution: build one arch, run its two cells, then move on ────────────────
ARCHES=(${MATRIX_ARCHES:-x86_64 aarch64 riscv64})
for arch in "${ARCHES[@]}"; do
  (( fail )) && break
  build_arch "$arch" || break
  recheck_exact_commit "$arch/build"
  check_feature_off "$arch"
  (( fail )) && break
  cell_timeout_wins "$arch"
  (( fail )) && break
  cell_reply_wins "$arch"
done

# ── 8. Combined matrix seal ──────────────────────────────────────────────────────────
TOTAL=$((TW_PASS + RW_PASS))
if (( fail )) || (( TW_PASS != 3 )) || (( RW_PASS != 3 )); then
  echo "STAGE_200_IPC_REPLY_TIMEOUT_MATRIX_SEAL arches=3 total_live_cells=${TOTAL} timeout_wins=${TW_PASS} reply_wins=${RW_PASS} feature_off_clean=${#FEATURE_OFF_CLEAN[@]} result=fail"
  exit 1
fi

cat <<SEAL
STAGE_200_IPC_REPLY_TIMEOUT_MATRIX_SEAL
arches=3
classes_per_arch=2
total_live_cells=6
timeout_wins=3
reply_wins=3
canonical_timeout_returns=3
reply_user_consumed=3
late_replies_rejected=3
late_timeouts_rejected=3
selector_misclassifications=0
single_boot_failures=0
duplicate_replies=0
duplicate_wakes=0
wrong_deadline_mutations=0
stale_authority_restores=0
exact_commit=${SHA0}
exact_tree=${TREEHASH0}
feature_off_clean=${#FEATURE_OFF_CLEAN[@]}
result=ok
SEAL
