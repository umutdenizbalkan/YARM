#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# RISCV-ECALL-A7-RET1 — the LIVE RISC-V ecall register-contract witness.
#
# Usage: scripts/qemu-riscv-ecall-witness-smoke.sh [smp]
#
# One clean RISC-V boot with `yarm.futex_requeue_witness=1`, kernel and init both built with
# `riscv-ecall-witness`: init's selector-18 cell runs the ecall witness (Yield, FutexWait parked /
# woken early / mismatch, requeue then wake, a blocking IpcRecv completed by init's send, a timer-
# preempted register spin, and every thread's fresh entry registers). Each probed ecall seeds its
# input and unnamed registers, carries a per-cycle KEY in a6, and reads every register back; the
# kernel prints one RISCV_ECALL_WITNESS_RESUME marker per keyed write-back naming the arm.
#
# The verdict comes from scripts/grade-riscv-ecall-witness.py (its docstring has the rules), plus
# the standard core smoke. Raw serial log, normalized log and grader output are kept in $LOGDIR.
set -uo pipefail
cd "$(dirname "$0")/.."

ARCH=riscv64
SMP=${1:-${QEMU_SMP:-2}}
# futex-wait-witness: the kernel's wait windows, so a witness FutexWait can be woken before it
# physically parks (FUTEX-WAIT-ATOMIC1's windows; the ecall witness uses their magic word).
FEATURE=riscv-shared-region-direct-oracle,riscv-ecall-witness,futex-wait-witness
WITNESS_FEATURE=futex-requeue-witness,riscv-ecall-witness
BUILD_SCRIPT=scripts/build-qemu-riscv64-artifacts.sh
SMOKE=scripts/qemu-riscv64-core-smoke.sh
INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio
DEST=build-riscv64/yarm-riscv64.bin
LOGDIR=${LOGDIR:-/tmp/riscv-ecall-witness-smp$SMP}
TIMEOUT_SECS=${TIMEOUT_SECS:-300}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[riscv-ecall-witness] $*"; }
die()  { echo "[riscv-ecall-witness][fail] $*"; fail=1; }

if [[ "${SKIP_BUILD:-0}" != 1 ]]; then
  note "building base $ARCH artifacts with $WITNESS_FEATURE"
  BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $WITNESS_FEATURE" \
    "$BUILD_SCRIPT" >"$LOGDIR/build.log" 2>&1 || die "base artifact build failed"
  note "rebuilding the kernel with $FEATURE"
  # BUILD-GATES1: build and publish the feature kernel with its no-FP check and provenance.
  source scripts/lib/artifact-binding.sh
  artifact_build_kernel "$ARCH" "--no-default-features --features $FEATURE" "${DEST%.bin}.elf" "$DEST" \
    >"$LOGDIR/kbuild.log" 2>&1 || die "feature kernel build failed"
fi
if (( fail )); then
  echo "RISCV_ECALL_WITNESS_SEAL arch=$ARCH smp=$SMP result=fail reason=build"
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
  TERMINAL_IDLE_MARKER="EA7W_DONE" \
  "$SMOKE" >"$LOGDIR/core-smoke.log" 2>&1
core_rc=$?
echo "$core_rc" >"$LOGDIR/core-smoke.rc"
grep -a -m1 'ARTIFACT_BINDING' "$LOGDIR/core-smoke.log" >"$LOGDIR/artifact-binding.txt" 2>/dev/null || true

[[ -s "$BOOT_LOG" ]] || { echo "RISCV_ECALL_WITNESS_SEAL arch=$ARCH smp=$SMP result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
sha256sum "$BOOT_LOG" "$DEST" "$INITRAMFS_IMAGE" >"$LOGDIR/sha256.txt" 2>/dev/null || true

python3 -I scripts/grade-riscv-ecall-witness.py --self-test >/dev/null || { echo "RISCV_ECALL_WITNESS_SEAL arch=$ARCH smp=$SMP result=fail reason=grader_self_test"; exit 1; }
python3 -I scripts/grade-riscv-ecall-witness.py --smp "$SMP" \
  --core-rc "$core_rc" "$NORM" | tee "$LOGDIR/grade.txt"
exit "${PIPESTATUS[0]}"
