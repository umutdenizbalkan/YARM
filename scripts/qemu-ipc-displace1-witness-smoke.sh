#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# IPC-DISPLACE1-ACCEPTANCE — the LIVE competing-receiver witness, per architecture.
#
# Usage: scripts/qemu-ipc-displace1-witness-smoke.sh [x86_64|aarch64|riscv64] [smp]
#
# One clean boot, over DISPOSABLE authority (the endpoint `provision_init_shared_region_oracle`
# hands init). Two of init's threads receive on that one endpoint and init sends:
#
#   R1  NR 2 recv-v2, untimed  -> blocks on the production split route and is E's waiter
#   R2  NR 2 recv-v2, untimed  -> refused by the publication owner (ReceiverAlreadyWaiting),
#                                 unwound exactly, answered QueueFull, and runs again
#   S   NR 1 send              -> delivered to R1, which receives it once
#
# for ROUNDS rounds on the same endpoint, then R2 receives normally once R1 has left.
#
# The verdict comes from scripts/grade-ipc-displace1-witness.py, which correlates the cell's own
# lines with the kernel's markers: each refusal must name R1 as the live holder, come from the
# split route's refusal arm with outcome=restored, and leave R1 the waiter; every round's message
# reaches R1 exactly once; placement is read from the kernel's own markers, never assumed.
#
# Raw serial log, normalized log and grader output are kept in $LOGDIR.
set -uo pipefail
cd "$(dirname "$0")/.."

ARCH=${1:-x86_64}
SMP=${2:-${QEMU_SMP:-1}}
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
  *) echo "[ipc-displace1-witness][fail] unknown arch: $ARCH"; exit 1 ;;
esac

WITNESS_FEATURE=ipc-displace1-witness
LOGDIR=${LOGDIR:-/tmp/ipc-displace1-witness-$ARCH-smp$SMP}
TIMEOUT_SECS=${TIMEOUT_SECS:-180}
ROUNDS=${ROUNDS:-16}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[ipc-displace1-witness] $*"; }
die()  { echo "[ipc-displace1-witness][fail] $*"; fail=1; }

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
  echo "IPC_DISPLACE1_WITNESS_SEAL arch=$ARCH smp=$SMP result=fail reason=build"
  exit 1
fi

note "booting QEMU -smp $SMP with yarm.ipc_displace1_witness=1"
env \
  KERNEL_IMAGE="$DEST" \
  ARTIFACT_REQUIRE="kernel=$FEATURE initramfs=$WITNESS_FEATURE" \
  INITRAMFS_IMAGE="$INITRAMFS_IMAGE" \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.ipc_displace1_witness=1" \
  QEMU_SMP="$SMP" \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  TERMINAL_IDLE_MARKER="IPCD1_WITNESS_DONE" \
  "$SMOKE" >"$LOGDIR/core-smoke.log" 2>&1
core_rc=$?
echo "$core_rc" >"$LOGDIR/core-smoke.rc"
grep -a -m1 'ARTIFACT_BINDING' "$LOGDIR/core-smoke.log" >"$LOGDIR/artifact-binding.txt" 2>/dev/null || true

[[ -s "$BOOT_LOG" ]] || { echo "IPC_DISPLACE1_WITNESS_SEAL arch=$ARCH smp=$SMP result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
sha256sum "$BOOT_LOG" "$DEST" "$INITRAMFS_IMAGE" >"$LOGDIR/sha256.txt" 2>/dev/null || true

python3 -I scripts/grade-ipc-displace1-witness.py --arch "$ARCH" --smp "$SMP" --rounds "$ROUNDS" \
  --core-rc "$core_rc" --core-log "$LOGDIR/smoke.log" --core-log "$LOGDIR/core-smoke.log" "$NORM" | tee "$LOGDIR/grade.txt"
exit "${PIPESTATUS[0]}"
