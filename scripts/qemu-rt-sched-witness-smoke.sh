#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# RT-SCHED1A — the LIVE RT selection witness, per architecture (`kernel::rt_witness`).
#
# Usage: scripts/qemu-rt-sched-witness-smoke.sh [x86_64|aarch64|riscv64] [smp]
#
# RT-SCHED1B: SCHED_BUDGET=on (the default) adds `yarm.sched_budget=on` to the boot, and the
# witness then also runs its budget phases E (a budgeted high-priority user task exhausting and
# returning only at its release while lower work runs) and F (a budgeted cooperative kernel thread
# charged and parked at its scheduling point); SCHED_BUDGET=off boots RT without budgets (the
# RT-SCHED1A witness alone). The grader is told which, and checks the boot's own configuration
# line agrees.
#
# One boot of the port's standard core image whose kernel is rebuilt with
# `<port>-rt-sched-witness`, with the deployment configuration `yarm.sched_profile=rt`. The whole
# standard core smoke runs under the RT profile (its terminal marker is the witness's `RTW_DONE`),
# and the witness's sealed record is graded by scripts/grade-rt-sched-witness.py: a local wake and
# a remote (real IPI) wake each preempting a running lower-priority user task, an equal-priority
# wake keeping FIFO with no tick rotation, and a higher-priority kernel thread honoured at a running
# kernel thread's scheduling point — by phase ordering and exact task identities.
#
# Raw serial log, normalized log, grader output and artifact hashes are kept in $LOGDIR. Logs are
# never edited: a failed run stays failed. SKIP_BASE=1 reuses the port's existing base artifacts
# (initramfs and servers, which carry no witness code); SKIP_BUILD=1 also reuses the kernel.
set -uo pipefail
cd "$(dirname "$0")/.."

ARCH=${1:-x86_64}
SMP=${2:-${QEMU_SMP:-2}}
SCHED_BUDGET=${SCHED_BUDGET:-on}
case "$SCHED_BUDGET" in on|off) ;; *) echo "RTW_WITNESS_SEAL arch=$ARCH result=fail reason=bad_sched_budget"; exit 2 ;; esac
case "$ARCH" in
  x86_64)
    FEATURE=x86-rt-sched-witness
    BUILD_SCRIPT=scripts/build-qemu-x86_64-artifacts.sh
    SMOKE=scripts/qemu-x86_64-core-smoke.sh
    INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio
    DEST=build-x86_64/kernel_boot.elf
    NEEDS_OBJCOPY=0
    ;;
  aarch64)
    FEATURE=aarch64-rt-sched-witness
    BUILD_SCRIPT=scripts/build-qemu-aarch64-artifacts.sh
    SMOKE=scripts/qemu-aarch64-core-smoke.sh
    INITRAMFS_IMAGE=build-aarch64/initramfs-core.cpio
    DEST=build-aarch64/yarm-aarch64.bin
    NEEDS_OBJCOPY=1
    ;;
  riscv64)
    FEATURE=riscv64-rt-sched-witness
    BUILD_SCRIPT=scripts/build-qemu-riscv64-artifacts.sh
    SMOKE=scripts/qemu-riscv64-core-smoke.sh
    INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio
    DEST=build-riscv64/yarm-riscv64.bin
    NEEDS_OBJCOPY=1
    ;;
  *) echo "RTW_WITNESS_SEAL arch=$ARCH result=fail reason=unsupported_arch"; exit 2 ;;
esac

LOGDIR=${LOGDIR:-/tmp/rt-sched-witness-$ARCH-smp$SMP}
# Only the AArch64 core smoke stops at the terminal marker; the x86_64 and RISC-V ones always run
# QEMU to their budget, so their defaults are the shortest that cover the witness.
case "$ARCH" in
  riscv64) TIMEOUT_SECS=${TIMEOUT_SECS:-240} ;;
  x86_64) TIMEOUT_SECS=${TIMEOUT_SECS:-600} ;;
  *) TIMEOUT_SECS=${TIMEOUT_SECS:-900} ;;
esac
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"
seal() { echo "RTW_WITNESS_SEAL arch=$ARCH smp=$SMP budget=$SCHED_BUDGET $*"; }

python3 -I scripts/grade-rt-sched-witness.py --self-test >"$LOGDIR/selftest.txt" 2>&1 \
  || { cat "$LOGDIR/selftest.txt"; seal "result=fail reason=grader_self_test"; exit 1; }
tail -1 "$LOGDIR/selftest.txt"

if [[ "${SKIP_BUILD:-0}" != 1 ]]; then
  if [[ "${SKIP_BASE:-0}" != 1 ]]; then
    echo "[rt-sched-witness] building base $ARCH artifacts"
    "$BUILD_SCRIPT" >"$LOGDIR/build.log" 2>&1 || { seal "result=fail reason=base_build"; exit 1; }
  fi
  echo "[rt-sched-witness] rebuilding the kernel with $FEATURE"
  # BUILD-GATES1: build and publish the feature kernel with its no-FP check and provenance.
  source scripts/lib/artifact-binding.sh
  if (( NEEDS_OBJCOPY )); then
    artifact_build_kernel "$ARCH" "--no-default-features --features $FEATURE" "${DEST%.bin}.elf" "$DEST" \
      >"$LOGDIR/kbuild.log" 2>&1 || { seal "result=fail reason=kernel_build"; exit 1; }
  else
    artifact_build_kernel "$ARCH" "--no-default-features --features $FEATURE" "$DEST" \
      >"$LOGDIR/kbuild.log" 2>&1 || { seal "result=fail reason=kernel_build"; exit 1; }
  fi
fi

echo "[rt-sched-witness] booting QEMU -smp $SMP with yarm.sched_profile=rt yarm.sched_budget=$SCHED_BUDGET"
env \
  KERNEL_IMAGE="$DEST" \
  ARTIFACT_REQUIRE="kernel=$FEATURE" \
  INITRAMFS_IMAGE="$INITRAMFS_IMAGE" \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.sched_profile=rt yarm.sched_budget=$SCHED_BUDGET" \
  QEMU_SMP="$SMP" \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  TERMINAL_IDLE_MARKER="RTW_DONE" \
  "$SMOKE" >"$LOGDIR/core-smoke.log" 2>&1
core_rc=$?
echo "$core_rc" >"$LOGDIR/core-smoke.rc"
grep -a -m1 'ARTIFACT_BINDING' "$LOGDIR/core-smoke.log" >"$LOGDIR/artifact-binding.txt" 2>/dev/null || true

[[ -s "$BOOT_LOG" ]] || { seal "result=fail reason=no_boot_log core_rc=$core_rc"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
sha256sum "$BOOT_LOG" "$DEST" "$INITRAMFS_IMAGE" >"$LOGDIR/sha256.txt" 2>/dev/null || true

python3 -I scripts/grade-rt-sched-witness.py --arch "$ARCH" --smp "$SMP" --budget "$SCHED_BUDGET" "$NORM" | tee "$LOGDIR/grade.txt"
grade_rc=${PIPESTATUS[0]}
if (( core_rc != 0 )); then
  seal "result=fail reason=core_smoke core_rc=$core_rc grade_rc=$grade_rc"
  exit 1
fi
if (( grade_rc != 0 )); then
  seal "result=fail reason=grade"
  exit 1
fi
seal "result=pass"
