#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# VFS-LIFETIME1 — the LIVE file-grant lifetime witness (audit finding F08), per architecture.
#
# Usage: scripts/qemu-vfs-lifetime-witness-smoke.sh [x86_64|aarch64|riscv64] [smp] [profile]
#   profile  witness       (default) the repaired path, graded by scripts/grade-vfs-lifetime-witness.py
#            nc-keep-relay negative control: VFS keeps its copy of every relayed grant; the grader's
#                          RECLAIM rule must fail
#            nc-revoke     negative control: NR 22 revokes the slot recursively instead of
#                          relinquishing it; the grader's SURVIVAL rule must fail
#
# One clean boot with every package built with `vfs-lifetime-witness` (and the control's own
# feature). init (init/vfs_lifetime_witness.rs) drives launches, five failed handoffs and a holder
# exit through PM, VFS and initramfs, taking a kernel ledger checkpoint after each. Logs are never
# edited: a failed run stays failed.
set -uo pipefail
cd "$(dirname "$0")/.."

ARCH=${1:-x86_64}
SMP=${2:-${QEMU_SMP:-2}}
PROFILE=${3:-witness}
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
  *) echo "[vfs-lifetime-witness][fail] unknown arch: $ARCH"; exit 1 ;;
esac

case "$PROFILE" in
  witness)       FEATURES=vfs-lifetime-witness; EXPECT_RULE= ;;
  nc-keep-relay) FEATURES=vfs-lifetime-witness,vfs-lifetime-nc-keep-relay; EXPECT_RULE=RECLAIM ;;
  nc-revoke)     FEATURES=vfs-lifetime-witness,vfs-lifetime-nc-revoke; EXPECT_RULE=SURVIVAL ;;
  *) echo "[vfs-lifetime-witness][fail] unknown profile: $PROFILE"; exit 1 ;;
esac
# IPC-D2-RULE1: a strict x86_64 core smoke grades the timer-contract witness, so a strict x86_64
# witness run builds it too.
if [[ "${QEMU_SMOKE_STRICT:-0}" == "1" && "$ARCH" == "x86_64" ]]; then
  FEATURES="$FEATURES,timer-contract-witness"
fi
LOGDIR=${LOGDIR:-/tmp/vfs-lifetime-witness-$ARCH-smp$SMP-$PROFILE}
TIMEOUT_SECS=${TIMEOUT_SECS:-600}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[vfs-lifetime-witness] $*"; }
die()  { echo "[vfs-lifetime-witness][fail] $*"; fail=1; }
SEAL="VFS_LIFETIME_WITNESS_SMOKE arch=$ARCH smp=$SMP profile=$PROFILE"

if [[ "${SKIP_BUILD:-0}" != 1 ]]; then
  note "building $ARCH artifacts with $FEATURES"
  BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $FEATURES" \
    "$BUILD_SCRIPT" >"$LOGDIR/build.log" 2>&1 || die "artifact build failed"
fi
if (( fail )); then
  echo "$SEAL result=fail reason=build"
  exit 1
fi

# Lines this witness produces ON PURPOSE that the AArch64 core smoke's generic blocker scan flags,
# and nothing else (each measured on the dev boots of all three ports; the grader's INJECTED rule
# requires each in exactly its count, so an exclusion can never hide a second occurrence):
#   * the two stale-reply injections (initramfs at hop 1, VFS at hop 2): the kernel refuses each
#     reply pre-lock with InvalidCapability (2), and VFS logs its refused client reply (1);
#   * PM's refusal of each of the five image-14 (fixture) requests: four `Malformed` (the rejected
#     or refused handoffs) and the NR 29 refusal, which PM's expected-refusal line repeats.
# D2_PUBLISH_RACE_UNWIND / FAILED_CLOSED are never excused (the core smoke refuses the attempt).
INJECTED='IPCREPLY_DIRECT_REFUSED_PRE_LOCK record_index=18446744073709551615 record_generation=18446744073709551615 replier_tid=[0-9]+ reason=reply_cap err=InvalidCapability reply_copies=0 caller_wakes=0$'
INJECTED+='|VFS_CLIENT_REPLY_REFUSED reply_cap=[0-9]+ err=InvalidCapability$'
INJECTED+='|PM_VFS_SPAWN_FAIL image_id=14 err=(Malformed|Unsupported)$'
INJECTED+='|VL1_PM_SPAWN_REFUSED_EXPECTED image_id=14 err=(Malformed|Unsupported)$'
# PM's loader-failure line for each of the five image-14 requests, which the strict x86_64 and
# AArch64 core smokes otherwise forbid (Phase 3B); the grader's INJECTED rule requires exactly
# these five and no other PM_ELF_ZC_FAIL.
ZC_INJECTED='PM_ELF_ZC_FAIL image_id=14 reason=(grant_ro_malformed status=0 has_cap=1|grant_ro_status status=(5|10) has_cap=0|spawn_from_mo_unsupported)$'
note "booting QEMU -smp $SMP"
env \
  KERNEL_IMAGE="$DEST" \
  ARTIFACT_REQUIRE="kernel=$FEATURES initramfs=$FEATURES" \
  INITRAMFS_IMAGE="$INITRAMFS_IMAGE" \
  QEMU_SMP="$SMP" \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  TERMINAL_IDLE_MARKER="VL1_WITNESS_DONE" \
  WITNESS_BLOCKER_EXCLUDE_REGEX="$INJECTED" \
  WITNESS_ZC_FAIL_EXCLUDE_REGEX="$ZC_INJECTED" \
  "$SMOKE" >"$LOGDIR/core-smoke.log" 2>&1
core_rc=$?
echo "$core_rc" >"$LOGDIR/core-smoke.rc"
grep -a -m1 'ARTIFACT_BINDING' "$LOGDIR/core-smoke.log" >"$LOGDIR/artifact-binding.txt" 2>/dev/null || true

[[ -s "$BOOT_LOG" ]] || { echo "$SEAL result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
sha256sum "$BOOT_LOG" "$DEST" "$INITRAMFS_IMAGE" scripts/fixtures/vfs-fixture.bin \
  >"$LOGDIR/sha256.txt" 2>/dev/null || true

python3 -I scripts/grade-vfs-lifetime-witness.py --self-test >/dev/null \
  || { echo "$SEAL result=fail reason=grader_self_test"; exit 1; }
python3 -I scripts/grade-vfs-lifetime-witness.py --arch "$ARCH" --smp "$SMP" --core-rc "$core_rc" \
  "$NORM" | tee "$LOGDIR/grade-lifetime.txt"
rc=${PIPESTATUS[0]}
if [[ -z "$EXPECT_RULE" ]]; then
  if (( rc == 0 )); then
    echo "$SEAL result=ok"
    exit 0
  fi
  echo "$SEAL result=fail grade_rc=$rc"
  exit 1
fi
# A negative control passes only when the grader fails, and fails for the rule it targets.
if (( rc != 0 )) && grep -q "^\[fail\] $EXPECT_RULE:" "$LOGDIR/grade-lifetime.txt"; then
  echo "$SEAL result=ok control_detected=1 rule=$EXPECT_RULE"
  exit 0
fi
echo "$SEAL result=fail control_detected=0 rule=$EXPECT_RULE grade_rc=$rc"
exit 1
