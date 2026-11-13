#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# FUTEX-TIME1 — the LIVE timed-FutexWait witness, per architecture.
#
# Usage: scripts/qemu-futex-time-witness-smoke.sh [x86_64|aarch64|riscv64] [smp]
#
# One clean boot with `yarm.futex_requeue_witness=1`, kernel and init both built with
# `futex-time-witness`: init's selector-18 cell then runs the timed-wait sequences A–G (see the cell's
# doc in init/service.rs): A/F timeouts with every other user thread blocked, B wake-before-expiry,
# C requeue-then-timeout, D requeue-then-wake, G a cross-CPU wake/timeout race with the kernel's
# post-claim window (opened only while a sibling runs on another CPU), and E a timeout that expires
# while its home run queue is genuinely full.
#
# The verdict comes from scripts/grade-futex-time-witness.py (its docstring has the rules), plus the
# standard core smoke. Raw serial log, normalized log and grader output are kept in $LOGDIR. Logs are
# never edited: a failed run stays failed.
set -uo pipefail
cd "$(dirname "$0")/.."

ARCH=${1:-x86_64}
SMP=${2:-${QEMU_SMP:-2}}
case "$ARCH" in
  x86_64)
    FEATURE=x86-shared-region-direct-oracle
    BUILD_SCRIPT=scripts/build-qemu-x86_64-artifacts.sh
    SMOKE=scripts/qemu-x86_64-core-smoke.sh
    INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio
    DEST=build-x86_64/kernel_boot.elf
    NEEDS_OBJCOPY=0
    ;;
  aarch64)
    FEATURE=aarch64-shared-region-direct-oracle
    BUILD_SCRIPT=scripts/build-qemu-aarch64-artifacts.sh
    SMOKE=scripts/qemu-aarch64-core-smoke.sh
    INITRAMFS_IMAGE=build-aarch64/initramfs-core.cpio
    DEST=build-aarch64/yarm-aarch64.bin
    NEEDS_OBJCOPY=1
    ;;
  riscv64)
    FEATURE=riscv-shared-region-direct-oracle
    BUILD_SCRIPT=scripts/build-qemu-riscv64-artifacts.sh
    SMOKE=scripts/qemu-riscv64-core-smoke.sh
    INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio
    DEST=build-riscv64/yarm-riscv64.bin
    NEEDS_OBJCOPY=1
    ;;
  *) echo "[futex-time-witness][fail] unknown arch: $ARCH"; exit 1 ;;
esac

FEATURE="$FEATURE,futex-time-witness"
WITNESS_FEATURE=futex-requeue-witness,futex-time-witness
LOGDIR=${LOGDIR:-/tmp/futex-time-witness-$ARCH-smp$SMP}
TIMEOUT_SECS=${TIMEOUT_SECS:-600}
SMP=${SMP:-2}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[futex-time-witness] $*"; }
die()  { echo "[futex-time-witness][fail] $*"; fail=1; }

if [[ "${SKIP_BUILD:-0}" != 1 ]]; then
  note "building base $ARCH artifacts with $WITNESS_FEATURE"
  BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $WITNESS_FEATURE" \
    "$BUILD_SCRIPT" >"$LOGDIR/build.log" 2>&1 || die "base artifact build failed"

  note "rebuilding kernel_boot with $FEATURE"
  # BUILD-GATES1: build and publish the feature kernel with its no-FP check and provenance.
  source scripts/lib/artifact-binding.sh
  if (( NEEDS_OBJCOPY )); then
    artifact_build_kernel "$ARCH" "--no-default-features --features $FEATURE" "${DEST%.bin}.elf" "$DEST" \
      >"$LOGDIR/kbuild.log" 2>&1 || die "feature kernel build failed"
  else
    artifact_build_kernel "$ARCH" "--no-default-features --features $FEATURE" "$DEST" \
      >"$LOGDIR/kbuild.log" 2>&1 || die "feature kernel build failed"
  fi
fi
if (( fail )); then
  echo "FUTEX_TIME_WITNESS_SEAL arch=$ARCH smp=$SMP result=fail reason=build"
  exit 1
fi

note "booting QEMU -smp $SMP with yarm.futex_requeue_witness=1"
env \
  KERNEL_IMAGE="$DEST" \
  ARTIFACT_REQUIRE="kernel=$FEATURE initramfs=$WITNESS_FEATURE" \
  INITRAMFS_IMAGE="$INITRAMFS_IMAGE" \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.futex_requeue_witness=1" \
  QEMU_SMP="$SMP" \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  TERMINAL_IDLE_MARKER="FTW_DONE" \
  "$SMOKE" >"$LOGDIR/core-smoke.log" 2>&1
core_rc=$?
echo "$core_rc" >"$LOGDIR/core-smoke.rc"
grep -a -m1 'ARTIFACT_BINDING' "$LOGDIR/core-smoke.log" >"$LOGDIR/artifact-binding.txt" 2>/dev/null || true

[[ -s "$BOOT_LOG" ]] || { echo "FUTEX_TIME_WITNESS_SEAL arch=$ARCH smp=$SMP result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
sha256sum "$BOOT_LOG" "$DEST" "$INITRAMFS_IMAGE" >"$LOGDIR/sha256.txt" 2>/dev/null || true

python3 -I scripts/grade-futex-time-witness.py --self-test >/dev/null || { echo "FUTEX_TIME_WITNESS_SEAL arch=$ARCH smp=$SMP result=fail reason=grader_self_test"; exit 1; }
python3 -I scripts/grade-futex-time-witness.py --arch "$ARCH" --smp "$SMP" \
  --core-rc "$core_rc" --core-log "$LOGDIR/smoke.log" --core-log "$LOGDIR/core-smoke.log" "$NORM" | tee "$LOGDIR/grade.txt"
exit "${PIPESTATUS[0]}"
