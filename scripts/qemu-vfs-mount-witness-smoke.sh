#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# VFS-CORRECT1B — the LIVE mount-authority and capability-width witness, per architecture.
#
# Usage: scripts/qemu-vfs-mount-witness-smoke.sh [x86_64|aarch64|riscv64] [smp] [wide|narrow]
#
# One clean boot with init, VFS and ramfs built with `vfs-mount-witness` and init also with
# `vfs-correct-witness` (the VFS-CORRECT1A read cell). Profile `wide` (the default) builds the
# kernel with `cap-gen-witness` too: every CNode slot starts at generation 0x1_0001, so every
# capability identity the kernel issues exceeds 2^32. Profile `narrow` leaves generations at 1.
#
# init (init/vfs_mount_witness.rs) registers /ram through the production client: an ordinary
# client is refused, refusals at the sender and at VFS, then the authorized stage+commit, then
# reads /ram/vc1b-witness (seeded by ramfs) through VFS. The verdict is both graders:
# scripts/grade-vfs-mount-witness.py (mount authority, identities, bytes, width) and
# scripts/grade-vfs-correct-witness.py (the 1A 300-byte fixture, FILE_GRANT_RO and NR 29), plus
# the standard core smoke. Logs are never edited: a failed run stays failed.
set -uo pipefail
cd "$(dirname "$0")/.."

ARCH=${1:-x86_64}
SMP=${2:-${QEMU_SMP:-2}}
PROFILE=${3:-wide}
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
  *) echo "[vfs-mount-witness][fail] unknown arch: $ARCH"; exit 1 ;;
esac

case "$PROFILE" in
  wide)   FEATURES=cap-gen-witness,vfs-correct-witness,vfs-mount-witness; WIDE_ARG=--wide ;;
  narrow) FEATURES=vfs-correct-witness,vfs-mount-witness; WIDE_ARG= ;;
  *) echo "[vfs-mount-witness][fail] unknown profile: $PROFILE"; exit 1 ;;
esac
LOGDIR=${LOGDIR:-/tmp/vfs-mount-witness-$ARCH-smp$SMP-$PROFILE}
TIMEOUT_SECS=${TIMEOUT_SECS:-480}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"
ROOTFS_INIT="build-$ARCH/rootfs/init"

fail=0
note() { echo "[vfs-mount-witness] $*"; }
die()  { echo "[vfs-mount-witness][fail] $*"; fail=1; }
SEAL="VFS_MOUNT_WITNESS_SEAL arch=$ARCH smp=$SMP profile=$PROFILE"

if [[ "${SKIP_BUILD:-0}" != 1 ]]; then
  note "building $ARCH artifacts with $FEATURES"
  BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $FEATURES" \
    "$BUILD_SCRIPT" >"$LOGDIR/build.log" 2>&1 || die "artifact build failed"
fi
if (( fail )); then
  echo "$SEAL result=fail reason=build"
  exit 1
fi
cp "$ROOTFS_INIT" "$LOGDIR/rootfs-init.bin" 2>/dev/null || true

note "booting QEMU -smp $SMP"
env \
  KERNEL_IMAGE="$DEST" \
  ARTIFACT_REQUIRE="kernel=$FEATURES initramfs=$FEATURES" \
  INITRAMFS_IMAGE="$INITRAMFS_IMAGE" \
  QEMU_SMP="$SMP" \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  TERMINAL_IDLE_MARKER="VC1B_END" \
  "$SMOKE" >"$LOGDIR/core-smoke.log" 2>&1
core_rc=$?
echo "$core_rc" >"$LOGDIR/core-smoke.rc"
grep -a -m1 'ARTIFACT_BINDING' "$LOGDIR/core-smoke.log" >"$LOGDIR/artifact-binding.txt" 2>/dev/null || true

[[ -s "$BOOT_LOG" ]] || { echo "$SEAL result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
sha256sum "$BOOT_LOG" "$DEST" "$INITRAMFS_IMAGE" "$ROOTFS_INIT" scripts/fixtures/vfs-fixture.bin \
  >"$LOGDIR/sha256.txt" 2>/dev/null || true

for g in scripts/grade-vfs-mount-witness.py scripts/grade-vfs-correct-witness.py; do
  python3 -I "$g" --self-test >/dev/null || { echo "$SEAL result=fail reason=grader_self_test:$g"; exit 1; }
done
python3 -I scripts/grade-vfs-mount-witness.py --arch "$ARCH" --smp "$SMP" --core-rc "$core_rc" $WIDE_ARG \
  "$NORM" | tee "$LOGDIR/grade-mount.txt"
rc_mount=${PIPESTATUS[0]}
python3 -I scripts/grade-vfs-correct-witness.py --arch "$ARCH" --smp "$SMP" --core-rc "$core_rc" \
  --fixture scripts/fixtures/vfs-fixture.bin --init-rootfs "$LOGDIR/rootfs-init.bin" "$NORM" \
  | tee "$LOGDIR/grade-correct.txt"
rc_correct=${PIPESTATUS[0]}
if (( rc_mount == 0 && rc_correct == 0 )); then
  echo "$SEAL result=ok"
  exit 0
fi
echo "$SEAL result=fail mount_rc=$rc_mount correct_rc=$rc_correct"
exit 1
