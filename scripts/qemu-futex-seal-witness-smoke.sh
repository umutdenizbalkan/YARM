#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# FUTEX-SEAL1 — the LIVE futex seal witness, per architecture.
#
# Usage: scripts/qemu-futex-seal-witness-smoke.sh [x86_64|aarch64|riscv64] [smp]
#   SEAL_PROFILE=gp (default) | rtb   — rtb boots `yarm.sched_profile=rt yarm.sched_budget=on`.
#   SEAL_CELLS="os nr17 pb" — the cells to run, in the cell order (default: pb, the paired broadcast).
#   SEAL_GRADE=1 (default) | 0        — 0 skips the grader (development checkpoints only).
#
# One clean boot with `yarm.futex_requeue_witness=1`: the kernel built with `futex-seal-witness`
# (which implies `futex-time-witness`: its markers record every timed wait's admission, deadline and
# terminal owner; `kernel::seal_witness` holds the budget sequence's record), init with
# `futex-seal-witness`, whose selector-18 cell runs `yarm_user_rt::futex_sync` over the real system
# calls (see the cell's doc in init/futex_seal_witness.rs).
#
# The verdict comes from scripts/grade-futex-seal-witness.py (its docstring has the rules), plus the
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
  *) echo "[futex-seal-witness][fail] unknown arch: $ARCH"; exit 1 ;;
esac

FEATURE="$FEATURE,futex-seal-witness"
WITNESS_FEATURE=futex-requeue-witness,futex-time-witness,futex-seal-witness
SEAL_CELLS=${SEAL_CELLS:-pb}
for c in $SEAL_CELLS; do
  case "$c" in
    os|nr17|lh|pb|rb) WITNESS_FEATURE="$WITNESS_FEATURE,futex-seal-$c" ;;
    *) echo "FUTEX_SEAL_WITNESS_SEAL arch=$ARCH result=fail reason=bad_cell"; exit 2 ;;
  esac
done
SEAL_PROFILE=${SEAL_PROFILE:-gp}
case "$SEAL_PROFILE" in
  gp) PROFILE_ARGS="" ;;
  rtb) PROFILE_ARGS=" yarm.sched_profile=rt yarm.sched_budget=on" ;;
  *) echo "FUTEX_SEAL_WITNESS_SEAL arch=$ARCH result=fail reason=bad_profile"; exit 2 ;;
esac
LOGDIR=${LOGDIR:-/tmp/futex-seal-witness-$ARCH-smp$SMP}
TIMEOUT_SECS=${TIMEOUT_SECS:-480}
SMP=${SMP:-2}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[futex-seal-witness] $*"; }
die()  { echo "[futex-seal-witness][fail] $*"; fail=1; }

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
  echo "FUTEX_SEAL_WITNESS_SEAL arch=$ARCH smp=$SMP result=fail reason=build"
  exit 1
fi

note "booting QEMU -smp $SMP with yarm.futex_requeue_witness=1"
env \
  KERNEL_IMAGE="$DEST" \
  ARTIFACT_REQUIRE="kernel=$FEATURE initramfs=$WITNESS_FEATURE" \
  INITRAMFS_IMAGE="$INITRAMFS_IMAGE" \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.futex_requeue_witness=1$PROFILE_ARGS" \
  QEMU_SMP="$SMP" \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  TERMINAL_IDLE_MARKER="FSEAL_ALL_END" \
  "$SMOKE" >"$LOGDIR/core-smoke.log" 2>&1
core_rc=$?
echo "$core_rc" >"$LOGDIR/core-smoke.rc"
grep -a -m1 'ARTIFACT_BINDING' "$LOGDIR/core-smoke.log" >"$LOGDIR/artifact-binding.txt" 2>/dev/null || true

[[ -s "$BOOT_LOG" ]] || { echo "FUTEX_SEAL_WITNESS_SEAL arch=$ARCH smp=$SMP result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
sha256sum "$BOOT_LOG" "$DEST" "$INITRAMFS_IMAGE" >"$LOGDIR/sha256.txt" 2>/dev/null || true

if [[ "${SEAL_GRADE:-1}" == 0 ]]; then
  echo "FUTEX_SEAL_WITNESS_SEAL arch=$ARCH smp=$SMP cells="$SEAL_CELLS" profile=$SEAL_PROFILE result=ungraded core_rc=$core_rc"
  exit 3
fi
python3 -I scripts/grade-futex-seal-witness.py --self-test >/dev/null || { echo "FUTEX_SEAL_WITNESS_SEAL arch=$ARCH smp=$SMP result=fail reason=grader_self_test"; exit 1; }
python3 -I scripts/grade-futex-seal-witness.py --arch "$ARCH" --smp "$SMP" --profile "$SEAL_PROFILE" --cells "$SEAL_CELLS" --summary "$LOGDIR/summary.json" \
  --core-rc "$core_rc" --core-log "$LOGDIR/smoke.log" --core-log "$LOGDIR/core-smoke.log" "$NORM" | tee "$LOGDIR/grade.txt"
exit "${PIPESTATUS[0]}"
