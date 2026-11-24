#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# VFS-CORRECT1A — the LIVE read-path witness, per architecture.
#
# Usage: scripts/qemu-vfs-correct-witness-smoke.sh [x86_64|aarch64|riscv64] [smp]
#
# One clean boot of the default kernel with init built with `vfs-correct-witness` (the feature is
# inert in every other package): init, an ordinary client, reads the fixture
# /initramfs/etc/vfs-fixture through the production client wrappers, the resident VFS router and
# initramfs_srv, across inline-completion boundaries, then exercises end of file, close, a missing
# path, a path with no mount and an invalid fd (init/vfs_correct_witness.rs). The same boot's PM
# loads driver_manager through VFS_OP_FILE_GRANT_RO + SpawnFromMemoryObject (NR 29).
#
# The verdict comes from scripts/grade-vfs-correct-witness.py (its docstring has the rules), which
# compares the logged bytes with scripts/fixtures/vfs-fixture.bin and with this build's
# rootfs/init, plus the standard core smoke. Logs are never edited: a failed run stays failed.
set -uo pipefail
cd "$(dirname "$0")/.."

ARCH=${1:-x86_64}
SMP=${2:-${QEMU_SMP:-2}}
case "$ARCH" in
  x86_64)
    BUILD_SCRIPT=scripts/build-qemu-x86_64-artifacts.sh
    SMOKE=scripts/qemu-x86_64-core-smoke.sh
    INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio
    DEST=build-x86_64/kernel_boot.elf
    ;;
  aarch64)
    BUILD_SCRIPT=scripts/build-qemu-aarch64-artifacts.sh
    SMOKE=scripts/qemu-aarch64-core-smoke.sh
    INITRAMFS_IMAGE=build-aarch64/initramfs-core.cpio
    DEST=build-aarch64/yarm-aarch64.bin
    ;;
  riscv64)
    BUILD_SCRIPT=scripts/build-qemu-riscv64-artifacts.sh
    SMOKE=scripts/qemu-riscv64-core-smoke.sh
    INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio
    DEST=build-riscv64/yarm-riscv64.bin
    ;;
  *) echo "[vfs-correct-witness][fail] unknown arch: $ARCH"; exit 1 ;;
esac

WITNESS_FEATURE=vfs-correct-witness
LOGDIR=${LOGDIR:-/tmp/vfs-correct-witness-$ARCH-smp$SMP}
TIMEOUT_SECS=${TIMEOUT_SECS:-480}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"
ROOTFS_INIT="build-$ARCH/rootfs/init"

fail=0
note() { echo "[vfs-correct-witness] $*"; }
die()  { echo "[vfs-correct-witness][fail] $*"; fail=1; }

if [[ "${SKIP_BUILD:-0}" != 1 ]]; then
  note "building $ARCH artifacts with $WITNESS_FEATURE"
  BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $WITNESS_FEATURE" \
    "$BUILD_SCRIPT" >"$LOGDIR/build.log" 2>&1 || die "artifact build failed"
fi
if (( fail )); then
  echo "VFS_CORRECT_WITNESS_SEAL arch=$ARCH smp=$SMP result=fail reason=build"
  exit 1
fi
cp "$ROOTFS_INIT" "$LOGDIR/rootfs-init.bin" 2>/dev/null || true

note "booting QEMU -smp $SMP"
env \
  KERNEL_IMAGE="$DEST" \
  ARTIFACT_REQUIRE="kernel=$WITNESS_FEATURE initramfs=$WITNESS_FEATURE" \
  INITRAMFS_IMAGE="$INITRAMFS_IMAGE" \
  QEMU_SMP="$SMP" \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  TERMINAL_IDLE_MARKER="VC1A_END" \
  "$SMOKE" >"$LOGDIR/core-smoke.log" 2>&1
core_rc=$?
echo "$core_rc" >"$LOGDIR/core-smoke.rc"
grep -a -m1 'ARTIFACT_BINDING' "$LOGDIR/core-smoke.log" >"$LOGDIR/artifact-binding.txt" 2>/dev/null || true

[[ -s "$BOOT_LOG" ]] || { echo "VFS_CORRECT_WITNESS_SEAL arch=$ARCH smp=$SMP result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
sha256sum "$BOOT_LOG" "$DEST" "$INITRAMFS_IMAGE" "$ROOTFS_INIT" scripts/fixtures/vfs-fixture.bin \
  >"$LOGDIR/sha256.txt" 2>/dev/null || true

python3 -I scripts/grade-vfs-correct-witness.py --self-test >/dev/null || { echo "VFS_CORRECT_WITNESS_SEAL arch=$ARCH smp=$SMP result=fail reason=grader_self_test"; exit 1; }
python3 -I scripts/grade-vfs-correct-witness.py --arch "$ARCH" --smp "$SMP" --core-rc "$core_rc" \
  --fixture scripts/fixtures/vfs-fixture.bin --init-rootfs "$LOGDIR/rootfs-init.bin" "$NORM" \
  | tee "$LOGDIR/grade.txt"
exit "${PIPESTATUS[0]}"
