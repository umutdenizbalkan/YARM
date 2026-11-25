#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# VFS-CORRECT1C — the LIVE client fd-namespace witness, per architecture.
#
# Usage: scripts/qemu-vfs-fd-witness-smoke.sh [x86_64|aarch64|riscv64] [smp] [wide|narrow]
#
# One clean boot with init, VFS and ramfs built with `vfs-fd-witness` and init also with
# `vfs-correct-witness` (the VFS-CORRECT1A read cell). Profile `wide` (the default) builds the
# kernel with `cap-gen-witness` too (every capability identity above 2^32); `narrow` does not.
#
# init (init/vfs_fd_witness.rs) spawns a second ramfs, mounts it at /ram/b, and drives the
# two-backend handle collision, canonical paths, the full table, the failed-reply rollback, a
# second thread and a fork child through the resident VFS. The verdict is
# scripts/grade-vfs-fd-witness.py and scripts/grade-vfs-correct-witness.py (the 1A fixture,
# FILE_GRANT_RO and NR 29) on the same boot, plus the standard core smoke. Logs are never edited:
# a failed run stays failed.
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
  *) echo "[vfs-fd-witness][fail] unknown arch: $ARCH"; exit 1 ;;
esac

case "$PROFILE" in
  wide)   FEATURES=cap-gen-witness,vfs-correct-witness,vfs-fd-witness; WIDE_ARG=--wide ;;
  narrow) FEATURES=vfs-correct-witness,vfs-fd-witness; WIDE_ARG= ;;
  *) echo "[vfs-fd-witness][fail] unknown profile: $PROFILE"; exit 1 ;;
esac
# IPC-D2-RULE1: a strict x86_64 core smoke grades the timer-contract witness, so it refuses any
# initramfs built without `timer-contract-witness` (scripts/qemu-x86_64-core-smoke.sh, BL5c). A
# strict x86_64 witness run builds it too; every other port and every non-strict run is unchanged.
if [[ "${QEMU_SMOKE_STRICT:-0}" == "1" && "$ARCH" == "x86_64" ]]; then
  FEATURES="$FEATURES,timer-contract-witness"
fi
LOGDIR=${LOGDIR:-/tmp/vfs-fd-witness-$ARCH-smp$SMP-$PROFILE}
TIMEOUT_SECS=${TIMEOUT_SECS:-480}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"
ROOTFS_INIT="build-$ARCH/rootfs/init"

fail=0
note() { echo "[vfs-fd-witness] $*"; }
die()  { echo "[vfs-fd-witness][fail] $*"; fail=1; }
SEAL="VFS_FD_WITNESS_SMOKE arch=$ARCH smp=$SMP profile=$PROFILE"

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

# Lines this witness produces on purpose, which the AArch64 core smoke's generic blocker scan would
# flag: its one injected fault. VFS answers one OPENAT through a stale reply capability (the same
# slot, the next generation); the kernel refuses that reply pre-lock and VFS logs the refusal.
# grade-vfs-fd-witness.py requires each line exactly once. Nothing else is passed. In particular
# D2_PUBLISH_RACE_UNWIND is graded, in every core smoke and in this witness's grader, by the one
# checker (scripts/check-d2-publish-race.py, IPC-D2-RULE1, doc/AI_AGENT_RULES.md §14.3), never
# excused: at SMP >= 2 the witness's second thread and fork child can run on another CPU than VFS
# and reach the split receive route's queue-publication race, which passes only when the checker
# attributes it one-to-one and finds it recovered.
INJECTED='IPCREPLY_DIRECT_REFUSED_PRE_LOCK record_index=18446744073709551615 record_generation=18446744073709551615 replier_tid=[0-9]+ reason=reply_cap err=InvalidCapability reply_copies=0 caller_wakes=0$|VFS_CLIENT_REPLY_REFUSED reply_cap=[0-9]+ err=InvalidCapability$'
note "booting QEMU -smp $SMP"
env \
  KERNEL_IMAGE="$DEST" \
  ARTIFACT_REQUIRE="kernel=$FEATURES initramfs=$FEATURES" \
  INITRAMFS_IMAGE="$INITRAMFS_IMAGE" \
  QEMU_SMP="$SMP" \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  TERMINAL_IDLE_MARKER="VC1C_FD_WITNESS_SEAL" \
  WITNESS_BLOCKER_EXCLUDE_REGEX="$INJECTED" \
  "$SMOKE" >"$LOGDIR/core-smoke.log" 2>&1
core_rc=$?
echo "$core_rc" >"$LOGDIR/core-smoke.rc"
grep -a -m1 'ARTIFACT_BINDING' "$LOGDIR/core-smoke.log" >"$LOGDIR/artifact-binding.txt" 2>/dev/null || true

[[ -s "$BOOT_LOG" ]] || { echo "$SEAL result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
sha256sum "$BOOT_LOG" "$DEST" "$INITRAMFS_IMAGE" "$ROOTFS_INIT" scripts/fixtures/vfs-fixture.bin \
  >"$LOGDIR/sha256.txt" 2>/dev/null || true

for g in scripts/grade-vfs-fd-witness.py scripts/grade-vfs-correct-witness.py; do
  python3 -I "$g" --self-test >/dev/null || { echo "$SEAL result=fail reason=grader_self_test:$g"; exit 1; }
done
python3 -I scripts/grade-vfs-fd-witness.py --arch "$ARCH" --smp "$SMP" --core-rc "$core_rc" $WIDE_ARG \
  "$NORM" | tee "$LOGDIR/grade-fd.txt"
rc_fd=${PIPESTATUS[0]}
python3 -I scripts/grade-vfs-correct-witness.py --arch "$ARCH" --smp "$SMP" --core-rc "$core_rc" \
  --fixture scripts/fixtures/vfs-fixture.bin --init-rootfs "$LOGDIR/rootfs-init.bin" "$NORM" \
  | tee "$LOGDIR/grade-correct.txt"
rc_correct=${PIPESTATUS[0]}
if (( rc_fd == 0 && rc_correct == 0 )); then
  echo "$SEAL result=ok"
  exit 0
fi
echo "$SEAL result=fail fd_rc=$rc_fd correct_rc=$rc_correct"
exit 1
