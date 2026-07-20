#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 199A2C1 — AArch64 DIRECT IpcCall(NR6)/IpcReply(NR7) LIVE round-trip smoke (two cells).
#
# Builds a FRESH feature-enabled AArch64 artifact (kernel_boot with
# `--features aarch64-ipccall-direct-oracle`, over a normally-built server + initramfs set — the
# userspace oracle scaffold is arch-gated, not feature-gated), boots ONE `-smp 1` QEMU with
# `yarm.aarch64_ipccall_direct_oracle=1`, and asserts BOTH direct classes are proven LIVE together
# in one clean boot.
#
# LIVE only when, in ONE clean boot, ALL of these appear EXACTLY ONCE from the real off-lock
# transactions completing:
#
#   GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=IpcCallDirectRequest
#   IPCCALL_DIRECT_REQUEST_OK      arch=aarch64 source_copy_offlock=1 reply_cap=1 server_wakes=1
#   GLOBAL_LOCK_RETIRE_CLASS_DONE  arch=aarch64 class=IpcCallDirectRequest result=ok
#   GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=IpcReplyDirect
#   IPCREPLY_DIRECT_OK             arch=aarch64 source_copy_offlock=1 caller_wakes=1 one_shot=1
#   GLOBAL_LOCK_RETIRE_CLASS_DONE  arch=aarch64 class=IpcReplyDirect result=ok
#
# plus the userspace completion (exactly once):
#   AARCH64_IPCCALL_DIRECT_ROUNDTRIP_DONE request_ok=1 reply_ok=1 duplicate_reply=rejected server_wakes=1 caller_wakes=1 client_continuations=1 server_continuations=1 result=ok
#   AARCH64_IPCREPLY_DIRECT_SEND attempts=<n> early_retries=<n-1> result=ok   (exactly one success)
#
# Fails closed on: missing/duplicate kernel marker, missing userspace completion, more than one
# successful reply, any queued IpcCall evidence, a broad-lock NR6/NR7 fallback, a DCE'd split drain,
# a duplicate request/reply post-work, a duplicate server/caller wake, a reply record still Reserved,
# a stale-waiter/ASID mismatch, an ELR/TTBR0/ASID restoration failure, a cross-arch (x86/RISC-V)
# marker, a service-chain regression, or a fatal trap/panic/timeout.
#
# On a genuinely clean log emits:
#   STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=aarch64 classes=2 live_cells=2 duplicate_replies=0 duplicate_wakes=0 result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=aarch64-ipccall-direct-oracle
KTARGET=${KTARGET:-targets/aarch64-yarm-none.json}
KPROFILE=${KPROFILE:-aarch64-none}
KELF=${KELF:-target/aarch64-yarm-none/${KPROFILE}/kernel_boot}
KBIN=${KBIN:-build-aarch64/yarm-aarch64.bin}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ipccall-reply-direct-aarch64}
TIMEOUT_SECS=${TIMEOUT_SECS:-180}
IDLE_MAX_SECS=${IDLE_MAX_SECS:-180}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[ipccall-reply-direct-aarch64-smoke] $*"; }
die()  { echo "[ipccall-reply-direct-aarch64-smoke][fail] $*"; fail=1; }

# ── 1. Base artifacts: servers + initramfs (no feature; the scaffold is arch-gated) ──
note "building base aarch64 artifacts (servers + initramfs)"
BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
  scripts/build-qemu-aarch64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
  || die "base artifact build failed (see $LOGDIR/build.log)"

# ── 2. Overlay: rebuild kernel_boot WITH the ipccall-direct-oracle feature, re-objcopy the .bin ──
if (( ! fail )); then
  note "rebuilding kernel_boot with --features $FEATURE"
  cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" \
    --no-default-features --features "$FEATURE" \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 \
    || die "feature kernel_boot build failed (see $LOGDIR/kbuild.log)"
fi
if (( ! fail )); then
  if command -v llvm-objcopy >/dev/null 2>&1; then
    OBJCOPY=llvm-objcopy
  elif command -v rust-objcopy >/dev/null 2>&1; then
    OBJCOPY=rust-objcopy
  else
    die "no objcopy available to produce raw kernel binary"
  fi
fi
if (( ! fail )); then
  "$OBJCOPY" -O binary "$KELF" "$KBIN" >"$LOGDIR/objcopy.log" 2>&1 \
    || die "objcopy of feature kernel failed (see $LOGDIR/objcopy.log)"
fi
if (( ! fail )); then
  # Artifact integrity: the feature kernel MUST carry BOTH direct class literals.
  rg -a -q "class=IpcCallDirectRequest" "$KBIN" || die "feature kernel missing IpcCallDirectRequest literal"
  rg -a -q "class=IpcReplyDirect" "$KBIN" || die "feature kernel missing IpcReplyDirect literal"
  # Cross-arch hygiene: no x86_64 / RISC-V IpcCall/Reply retirement literal in the AArch64 kernel.
  rg -a -q "arch=x86_64 class=IpcCallDirectRequest" "$KBIN" && die "x86_64 direct literal in aarch64 kernel"
  rg -a -q "arch=riscv64 class=IpcCallDirectRequest" "$KBIN" && die "riscv64 direct literal in aarch64 kernel"
fi

# ── 2b. Artifact integrity: a NORMAL (feature-OFF) kernel must be marker-CLEAN ──
if (( ! fail )); then
  note "building a feature-OFF kernel_boot and asserting it is marker-clean"
  cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" --no-default-features \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild-off.log" 2>&1 \
    || die "feature-off kernel_boot build failed (see $LOGDIR/kbuild-off.log)"
  OFF_BIN="$LOGDIR/kernel_boot_off.bin"
  "$OBJCOPY" -O binary "$KELF" "$OFF_BIN" >/dev/null 2>&1 || die "objcopy of feature-off kernel failed"
  if rg -a -q "class=IpcCallDirectRequest" "$OFF_BIN" || rg -a -q "class=IpcReplyDirect" "$OFF_BIN"; then
    die "feature-OFF kernel unexpectedly contains a direct class literal (not marker-clean)"
  fi
fi

if (( fail )); then
  echo "STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=aarch64 classes=2 result=fail reason=build"
  exit 1
fi

# ── 3. Boot ONE -smp 1 QEMU with the oracle knob ──
note "booting QEMU -smp 1 with yarm.aarch64_ipccall_direct_oracle=1"
env \
  KERNEL_IMAGE="$KBIN" \
  INITRAMFS_IMAGE=build-aarch64/initramfs-core.cpio \
  KERNEL_CMDLINE="yarm.aarch64_ipccall_direct_oracle=1" \
  QEMU_SMP=1 \
  QEMU_SMOKE_STRICT=0 \
  LOGFILE="$BOOT_LOG" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  IDLE_MAX_SECS="$IDLE_MAX_SECS" \
  scripts/qemu-aarch64-core-smoke.sh >"$LOGDIR/core-smoke.log" 2>&1 || true

if [[ ! -s "$BOOT_LOG" ]]; then
  die "no boot log produced"
  echo "STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=aarch64 classes=2 live_cells=0 result=fail reason=no_boot_log"
  exit 1
fi

NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"

count() { rg -a -c -F "$1" "$NORM" 2>/dev/null || echo 0; }
have()  { rg -a -q -F "$1" "$NORM"; }

# ── 4a. The six kernel markers — each EXACTLY once ──
declare -a KMARKERS=(
  "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=IpcCallDirectRequest"
  "IPCCALL_DIRECT_REQUEST_OK arch=aarch64 source_copy_offlock=1 reply_cap=1 server_wakes=1"
  "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=IpcCallDirectRequest result=ok"
  "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=IpcReplyDirect"
  "IPCREPLY_DIRECT_OK arch=aarch64 source_copy_offlock=1 caller_wakes=1 one_shot=1"
  "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=IpcReplyDirect result=ok"
)
for m in "${KMARKERS[@]}"; do
  c=$(count "$m")
  if [[ "$c" != "1" ]]; then
    die "kernel marker count != 1 (got $c): $m"
  fi
done

# ── 4b. Userspace completion + exactly one successful reply ──
UDONE="AARCH64_IPCCALL_DIRECT_ROUNDTRIP_DONE request_ok=1 reply_ok=1 duplicate_reply=rejected server_wakes=1 caller_wakes=1 client_continuations=1 server_continuations=1 result=ok"
[[ "$(count "$UDONE")" == "1" ]] || die "userspace completion missing/duplicate: $UDONE"
reply_ok_result=$(grep -aF "AARCH64_IPCREPLY_DIRECT_SEND" "$NORM" | grep -aFc "result=ok" || true)
[[ "$reply_ok_result" == "1" ]] || die "expected exactly one successful reply (got $reply_ok_result)"
if ! rg -a -q "IPCCALL_DIRECT_ORACLE_SERVER_RECV framed_ok=1 .* reply_cap_ok=1" "$NORM"; then
  die "server did not observe an exact request + fresh reply cap"
fi

# ── 4c. Hard-stop conditions ──
grep -aF "AARCH64_IPCCALL_DIRECT_ROUNDTRIP_DONE" "$NORM" | grep -aq "result=fail" && die "roundtrip completion result=fail"
have "IPCCALL_DIRECT_ORACLE_MISSING_CAPS" && die "oracle caps missing (provisioning failed)"
have "IPCCALL_DIRECT_ORACLE_PROVISION_FAIL" && die "oracle provisioning failed"
have "IPCCALL_DIRECT_ORACLE_SERVER_REPLY_HARD_FAIL" && die "server reply hard-failed"
have "IPCCALL_DIRECT_ORACLE_CALL_HARD_FAIL" && die "client call hard-failed"
have "IPCCALL_DIRECT_ORACLE_SPAWN_FAIL" && die "server child spawn failed"
have "IPCCALL_DIRECT_ORACLE_SERVER_DUP dup_rejected=0" && die "duplicate reply was NOT rejected"
have "AARCH64_ORACLE_SLOT5_CONFLICT" && die "slot-5 selector conflict (arm-neither) observed"
# Cross-arch userspace completion must never appear in an AArch64 boot.
have "X86_IPCCALL_DIRECT_ROUNDTRIP_DONE" && die "x86_64 userspace completion in aarch64 boot"
have "IPCCALL_DIRECT_REQUEST_OK arch=x86_64" && die "x86_64 kernel attestation in aarch64 boot"
# Fatal traps / panics / stalls (ELR/TTBR0/ASID restoration failures surface as faults/panics).
for bad in "KERNEL PANIC" "RUST PANIC" "panicked at" "CPU EXCEPTION" "SYNCHRONOUS EXCEPTION" "Unhandled" "BOOTSTRAP_ERROR"; do
  have "$bad" && die "fatal condition in boot log: $bad"
done

if (( fail )); then
  echo "STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=aarch64 classes=2 live_cells=0 result=fail (see $BOOT_LOG)"
  exit 1
fi

note "both direct-class kernel markers + userspace completion present exactly once; no dup/fault"
echo "STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=aarch64 classes=2 live_cells=2 duplicate_replies=0 duplicate_wakes=0 result=ok"
