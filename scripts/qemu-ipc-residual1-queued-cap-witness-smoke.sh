#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# U9-IPC-RESIDUAL1 §4 — the QUEUED cap-bearing reply witness, per architecture.
#
# Usage: scripts/qemu-ipc-residual1-queued-cap-witness-smoke.sh [x86_64|aarch64|riscv64]
#
# What it proves, in ONE clean boot, over DISPOSABLE authority (the endpoint and memory object
# `provision_init_shared_region_oracle` hands init — nothing the running system depends on):
#
#   * NR 6 with NO receiver parked takes the BUFFERED lane and enqueues the request
#     (`IPCCALL_QUEUED_SPLIT_OK`), returning to the caller immediately;
#   * the request is received and its one-shot reply capability materialized;
#   * NR 7 carrying a TRANSFERRED CAPABILITY to a caller that is NOT blocked takes the QUEUED
#     lane (`IPC_REPLY_QUEUED_SPLIT_OK`) — the shape the selector used to decline;
#   * the capability actually ARRIVES: the caller's next receive materializes it locally
#     (`cap=1`, `delivered_cap` non-zero). A queued cap-bearing reply that dropped its
#     capability would report `cap=0` here.
#   * init's essential endpoint capability still resolves afterwards (`intact=1`).
#
# And, from the kernel's own per-trap census, that neither direction entered the terminal broad
# acquisition on this boot: `broad_entries=0` for nr6 and nr7, with the production seal green.
#
# Fails on: a missing or duplicated witness line, result=fail, any terminal-broad entry, or any
# fatal trap/panic/timeout.
set -uo pipefail
cd "$(dirname "$0")/.."

ARCH=${1:-x86_64}
case "$ARCH" in
  x86_64)
    FEATURE=x86-shared-region-direct-oracle
    KTARGET=targets/x86_64-yarm-none.json
    KPROFILE=x86-none
    KELF=target/x86_64-yarm-none/${KPROFILE}/kernel_boot
    BUILD_SCRIPT=scripts/build-qemu-x86_64-artifacts.sh
    SMOKE=scripts/qemu-x86_64-core-smoke.sh
    INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio
    DEST=build-x86_64/kernel_boot.elf
    NEEDS_OBJCOPY=0
    ;;
  aarch64)
    FEATURE=aarch64-shared-region-direct-oracle
    KTARGET=targets/aarch64-yarm-none.json
    KPROFILE=aarch64-none
    KELF=target/aarch64-yarm-none/${KPROFILE}/kernel_boot
    BUILD_SCRIPT=scripts/build-qemu-aarch64-artifacts.sh
    SMOKE=scripts/qemu-aarch64-core-smoke.sh
    INITRAMFS_IMAGE=build-aarch64/initramfs-core.cpio
    DEST=build-aarch64/yarm-aarch64.bin
    NEEDS_OBJCOPY=1
    ;;
  riscv64)
    FEATURE=riscv-shared-region-direct-oracle
    KTARGET=riscv64gc-unknown-none-elf
    KPROFILE=release
    KELF=target/riscv64gc-unknown-none-elf/${KPROFILE}/kernel_boot
    BUILD_SCRIPT=scripts/build-qemu-riscv64-artifacts.sh
    SMOKE=scripts/qemu-riscv64-core-smoke.sh
    INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio
    DEST=build-riscv64/yarm-riscv64.bin
    NEEDS_OBJCOPY=1
    ;;
  *) echo "[ipc-residual1-witness][fail] unknown arch: $ARCH"; exit 1 ;;
esac

BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ipc-residual1-queued-cap-$ARCH}
TIMEOUT_SECS=${TIMEOUT_SECS:-180}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[ipc-residual1-witness] $*"; }
die()  { echo "[ipc-residual1-witness][fail] $*"; fail=1; }

note "building base $ARCH artifacts"
BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
  "$BUILD_SCRIPT" >"$LOGDIR/build.log" 2>&1 || die "base artifact build failed"

note "rebuilding kernel_boot with $FEATURE"
JSON_SPEC_ARG=()
[[ "$KTARGET" == *.json ]] && JSON_SPEC_ARG=(-Z json-target-spec)
cargo build --no-default-features --features "$FEATURE" \
  --target "$KTARGET" --profile "$KPROFILE" \
  -Z build-std="$BUILD_STD" "${JSON_SPEC_ARG[@]}" \
  -p yarm --bin kernel_boot \
  >"$LOGDIR/kbuild.log" 2>&1 || die "feature kernel build failed"

if [[ ! -f "$KELF" ]]; then
  die "feature kernel ELF missing"
elif (( NEEDS_OBJCOPY )); then
  if command -v llvm-objcopy >/dev/null 2>&1; then OBJCOPY=llvm-objcopy
  elif command -v rust-objcopy >/dev/null 2>&1; then OBJCOPY=rust-objcopy
  else OBJCOPY=""; die "no objcopy available to produce the raw kernel image"; fi
  [[ -n "$OBJCOPY" ]] && { "$OBJCOPY" -O binary "$KELF" "$DEST" >"$LOGDIR/objcopy.log" 2>&1 \
    || die "objcopy of the feature kernel failed"; }
else
  cp "$KELF" "$DEST" || die "feature kernel copy failed"
fi
KERNEL_IMAGE="$DEST"

if (( fail )); then
  echo "IPC_RESIDUAL1_QUEUED_CAP_SEAL arch=$ARCH result=fail reason=build"
  exit 1
fi

note "booting QEMU -smp 1 with yarm.ipc_residual1_queued_cap_witness=1"
env \
  KERNEL_IMAGE="$KERNEL_IMAGE" \
  INITRAMFS_IMAGE="$INITRAMFS_IMAGE" \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.ipc_residual1_queued_cap_witness=1" \
  QEMU_SMP=1 \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  "$SMOKE" >"$LOGDIR/core-smoke.log" 2>&1 || true

[[ -s "$BOOT_LOG" ]] || { echo "IPC_RESIDUAL1_QUEUED_CAP_SEAL arch=$ARCH result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"

count() { grep -a -F -- "$1" "$NORM" 2>/dev/null | wc -l | tr -d ' '; }
have()  { grep -a -q -F -- "$1" "$NORM"; }

# ── The witness itself ──
[[ "$(count 'IPC_RESIDUAL1_QUEUED_CAP_WITNESS_BEGIN')" == "1" ]] || die "witness did not start exactly once"
line=$(grep -a -m1 -- 'IPC_RESIDUAL1_QUEUED_CAP_WITNESS call=' "$NORM" || true)
for prop in "call=1" "reply=1" "cap=1" "intact=1" "result=1"; do
  case "$line" in
    *" $prop"*) ;;
    *) die "witness missing $prop  [$line]" ;;
  esac
done
[[ "$(count 'IPC_RESIDUAL1_QUEUED_CAP_WITNESS_DONE')" == "1" ]] || die "witness completion missing"
have 'IPC_RESIDUAL1_QUEUED_CAP_WITNESS_DONE delivered_cap=0' && die "the queued reply dropped its capability"
grep -a -q 'IPC_RESIDUAL1_QUEUED_CAP_WITNESS_DONE .* result=ok' "$NORM" || die "witness result is not ok"

# ── The two lanes it exercises, both on the split route ──
(( $(count 'IPCCALL_QUEUED_SPLIT_OK') >= 1 )) || die "the NR 6 buffered lane did not run"
(( $(count 'IPC_REPLY_QUEUED_SPLIT_OK') >= 1 )) || die "the NR 7 queued lane did not run"

# ── Neither direction entered the terminal broad acquisition ──
for d in nr6 nr7; do
  q=$(grep -a -m1 -- "IPC_DIRECT_PRODUCTION_QUIESCENT dir=$d " "$NORM" || true)
  [[ -n "$q" ]] || { die "no quiescent census for $d"; continue; }
  case "$q" in
    *" broad_entries=0"*) ;;
    *) die "$d entered the terminal broad acquisition: [$q]" ;;
  esac
done
[[ "$(count 'IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL nr6_ok=1 nr7_ok=1 census_ok=1 result=ok')" == "1" ]] \
  || die "the direct-IPC production seal is not green"

# ── Nothing may have gone wrong on the way ──
for bad in \
  'IPCREPLY_DIRECT_TERMINAL_LOST' \
  'YARM_SPLIT_DISPATCH_FALLBACK' \
  'KERNEL PANIC' 'panicked at' 'UNHANDLED'; do
  have "$bad" && die "fatal or unexpected condition in boot log: $bad"
done

if (( fail )); then
  echo "IPC_RESIDUAL1_QUEUED_CAP_SEAL arch=$ARCH result=fail"
  exit 1
fi
note "a cap-bearing reply to an unblocked caller was queued and its capability delivered"
echo "IPC_RESIDUAL1_QUEUED_CAP_SEAL arch=$ARCH buffered_call=1 queued_cap_reply=1 broad_entries=0 result=ok"
