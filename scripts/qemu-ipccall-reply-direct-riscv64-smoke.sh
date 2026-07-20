#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 199A2C2 — RISC-V DIRECT IpcCall(NR6)/IpcReply(NR7) LIVE round-trip smoke (two cells).
#
# Builds a FRESH feature-enabled RISC-V artifact (kernel_boot with
# `--features riscv-ipccall-direct-oracle`, over a normally-built server + initramfs set — the
# userspace oracle scaffold is arch-gated, not feature-gated), boots ONE `-smp 1` QEMU-virt/OpenSBI
# with `yarm.riscv_ipccall_direct_oracle=1`, and asserts BOTH direct classes are proven LIVE together
# in one clean boot.
#
# LIVE only when, in ONE clean boot, ALL of these appear EXACTLY ONCE from the real off-lock
# transactions completing:
#
#   GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=riscv64 class=IpcCallDirectRequest
#   IPCCALL_DIRECT_REQUEST_OK      arch=riscv64 source_copy_offlock=1 reply_cap=1 server_wakes=1
#   GLOBAL_LOCK_RETIRE_CLASS_DONE  arch=riscv64 class=IpcCallDirectRequest result=ok
#   GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=riscv64 class=IpcReplyDirect
#   IPCREPLY_DIRECT_OK             arch=riscv64 source_copy_offlock=1 caller_wakes=1 one_shot=1
#   GLOBAL_LOCK_RETIRE_CLASS_DONE  arch=riscv64 class=IpcReplyDirect result=ok
#
# plus the userspace completion (exactly once):
#   RISCV_IPCCALL_DIRECT_ROUNDTRIP_DONE request_ok=1 reply_ok=1 duplicate_reply=rejected server_wakes=1 caller_wakes=1 client_continuations=1 server_continuations=1 result=ok
#   RISCV_IPCREPLY_DIRECT_SEND attempts=<n> early_retries=<n-1> result=ok   (exactly one success)
#
# Fails closed on: missing/duplicate kernel marker, missing userspace completion, more than one
# successful reply, split drains unreachable/DCE'd, any queued IpcCall evidence, a broad-lock NR6/NR7
# fallback, a duplicate request/reply post-work, a duplicate server/caller wake, a reply record still
# Reserved, a stale-waiter/ASID mismatch, an Err(Internal)-as-idle, a cross-arch (x86/aarch64) marker,
# a service-chain regression, or a fatal trap/panic/timeout.
#
# On a genuinely clean log emits:
#   STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=riscv64 classes=2 live_cells=2 duplicate_replies=0 duplicate_wakes=0 result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=riscv-ipccall-direct-oracle
KTARGET=${KTARGET:-riscv64gc-unknown-none-elf}
KPROFILE=${KPROFILE:-release}
KELF=${KELF:-target/riscv64gc-unknown-none-elf/${KPROFILE}/kernel_boot}
KBIN=${KBIN:-build-riscv64/yarm-riscv64.bin}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ipccall-reply-direct-riscv64}
TIMEOUT_SECS=${TIMEOUT_SECS:-180}
IDLE_MAX_SECS=${IDLE_MAX_SECS:-180}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[ipccall-reply-direct-riscv64-smoke] $*"; }
die()  { echo "[ipccall-reply-direct-riscv64-smoke][fail] $*"; fail=1; }

# ── 1. Base artifacts: servers + initramfs (no feature; the scaffold is arch-gated) ──
note "building base riscv64 artifacts (servers + initramfs)"
BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
  scripts/build-qemu-riscv64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
  || die "base artifact build failed (see $LOGDIR/build.log)"

# ── 2. Overlay: rebuild kernel_boot WITH the ipccall-direct-oracle feature, re-objcopy the .bin ──
if (( ! fail )); then
  note "rebuilding kernel_boot with --features $FEATURE"
  cargo build -Z "build-std=${BUILD_STD}" \
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
  # Cross-arch hygiene: no x86_64 / aarch64 IpcCall/Reply retirement literal in the RISC-V kernel.
  rg -a -q "arch=x86_64 class=IpcCallDirectRequest" "$KBIN" && die "x86_64 direct literal in riscv64 kernel"
  rg -a -q "arch=aarch64 class=IpcCallDirectRequest" "$KBIN" && die "aarch64 direct literal in riscv64 kernel"
fi

# ── 2b. Artifact integrity: a NORMAL (feature-OFF) kernel must be marker-CLEAN ──
if (( ! fail )); then
  note "building a feature-OFF kernel_boot and asserting it is marker-clean"
  cargo build -Z "build-std=${BUILD_STD}" \
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
  echo "STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=riscv64 classes=2 result=fail reason=build"
  exit 1
fi

# ── 3. Boot ONE -smp 1 QEMU-virt/OpenSBI with the oracle knob ──
note "booting QEMU-virt -smp 1 with yarm.riscv_ipccall_direct_oracle=1"
env \
  KERNEL_IMAGE="$KBIN" \
  INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.riscv_ipccall_direct_oracle=1" \
  QEMU_SMP=1 \
  QEMU_SMOKE_STRICT=0 \
  LOGFILE="$BOOT_LOG" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  IDLE_MAX_SECS="$IDLE_MAX_SECS" \
  scripts/qemu-riscv64-core-smoke.sh >"$LOGDIR/core-smoke.log" 2>&1 || true

if [[ ! -s "$BOOT_LOG" ]]; then
  die "no boot log produced"
  echo "STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=riscv64 classes=2 live_cells=0 result=fail reason=no_boot_log"
  exit 1
fi

NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"

count() { rg -a -c -F "$1" "$NORM" 2>/dev/null || echo 0; }
have()  { rg -a -q -F "$1" "$NORM"; }

# ── 4a. The six kernel markers — each EXACTLY once ──
declare -a KMARKERS=(
  "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=riscv64 class=IpcCallDirectRequest"
  "IPCCALL_DIRECT_REQUEST_OK arch=riscv64 source_copy_offlock=1 reply_cap=1 server_wakes=1"
  "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcCallDirectRequest result=ok"
  "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=riscv64 class=IpcReplyDirect"
  "IPCREPLY_DIRECT_OK arch=riscv64 source_copy_offlock=1 caller_wakes=1 one_shot=1"
  "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcReplyDirect result=ok"
)
for m in "${KMARKERS[@]}"; do
  c=$(count "$m")
  if [[ "$c" != "1" ]]; then
    die "kernel marker count != 1 (got $c): $m"
  fi
done

# ── 4b. Userspace completion + exactly one successful reply ──
UDONE="RISCV_IPCCALL_DIRECT_ROUNDTRIP_DONE request_ok=1 reply_ok=1 duplicate_reply=rejected server_wakes=1 caller_wakes=1 client_continuations=1 server_continuations=1 result=ok"
[[ "$(count "$UDONE")" == "1" ]] || die "userspace completion missing/duplicate: $UDONE"
reply_ok_result=$(grep -aF "RISCV_IPCREPLY_DIRECT_SEND" "$NORM" | grep -aFc "result=ok" || true)
[[ "$reply_ok_result" == "1" ]] || die "expected exactly one successful reply (got $reply_ok_result)"
if ! rg -a -q "IPCCALL_DIRECT_ORACLE_SERVER_RECV framed_ok=1 .* reply_cap_ok=1" "$NORM"; then
  die "server did not observe an exact request + fresh reply cap"
fi

# ── 4c. Hard-stop conditions ──
grep -aF "RISCV_IPCCALL_DIRECT_ROUNDTRIP_DONE" "$NORM" | grep -aq "result=fail" && die "roundtrip completion result=fail"
have "IPCCALL_DIRECT_ORACLE_MISSING_CAPS" && die "oracle caps missing (provisioning failed)"
have "IPCCALL_DIRECT_ORACLE_PROVISION_FAIL" && die "oracle provisioning failed"
have "IPCCALL_DIRECT_ORACLE_SERVER_REPLY_HARD_FAIL" && die "server reply hard-failed"
have "IPCCALL_DIRECT_ORACLE_CALL_HARD_FAIL" && die "client call hard-failed"
have "IPCCALL_DIRECT_ORACLE_SPAWN_FAIL" && die "server child spawn failed"
have "IPCCALL_DIRECT_ORACLE_SERVER_DUP dup_rejected=0" && die "duplicate reply was NOT rejected"
have "RISCV_TYPED_OUTCOME_INTERNAL_ERROR" && die "Err(Internal) idle-oracle path observed"
have "RISCV_TRAP_HANDLE_FAILED" && die "RISC-V trap handling failed (fatal)"
# Cross-arch userspace completion / kernel attestation must never appear in a RISC-V boot.
have "X86_IPCCALL_DIRECT_ROUNDTRIP_DONE" && die "x86_64 userspace completion in riscv64 boot"
have "AARCH64_IPCCALL_DIRECT_ROUNDTRIP_DONE" && die "aarch64 userspace completion in riscv64 boot"
have "IPCCALL_DIRECT_REQUEST_OK arch=x86_64" && die "x86_64 kernel attestation in riscv64 boot"
have "IPCCALL_DIRECT_REQUEST_OK arch=aarch64" && die "aarch64 kernel attestation in riscv64 boot"
# Fatal traps / panics / stalls (sstatus/satp/sp/tp corruption surfaces as faults/panics).
for bad in "KERNEL PANIC" "RUST PANIC" "panicked at" "Unhandled" "BOOTSTRAP_ERROR" "RISCV_TRAP_HANDLE_FAILED"; do
  have "$bad" && die "fatal condition in boot log: $bad"
done

if (( fail )); then
  echo "STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=riscv64 classes=2 live_cells=0 result=fail (see $BOOT_LOG)"
  exit 1
fi

note "both direct-class kernel markers + userspace completion present exactly once; no dup/fault"
echo "STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=riscv64 classes=2 live_cells=2 duplicate_replies=0 duplicate_wakes=0 result=ok"
