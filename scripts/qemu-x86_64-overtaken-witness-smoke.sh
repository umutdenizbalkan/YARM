#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-SMP3-ACCEPTANCE §2 — the x86_64 overtaken-deferral witness: one strict `-smp 2` boot of a
# kernel built with `x86-overtaken-witness` on the SMP oracle's reply profile with
# `yarm.x86_64_overtaken_witness=1`, graded by `grade-overtaken-witness.py` from the kernel's
# sealed summary.
#
# Usage: LOGDIR=... scripts/qemu-x86_64-overtaken-witness-smoke.sh
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-x86_64-overtaken-witness}
TIMEOUT_SECS=${TIMEOUT_SECS:-240}
KTARGET=targets/x86_64-yarm-none.json
KPROFILE=x86-none
BUILD_STD=core,alloc,compiler_builtins,panic_abort
FEATURE=x86-overtaken-witness
CMDLINE="console=ttyS0 rdinit=/init yarm.x86_64_ipccall_direct_smp_oracle=1 yarm.x86_64_ipccall_direct_smp_recv_v2_server=1 yarm.x86_64_ipccall_direct_smp_request=1 yarm.x86_64_ipccall_direct_smp_reply=1 yarm.ap_user_dispatch=1 yarm.x86_64_overtaken_witness=1"
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
seal_fail() { echo "OVT_WITNESS_SEAL arch=x86_64 result=fail reason=$1"; exit 1; }

if [[ "${REGRADE:-0}" != "1" ]]; then
  echo "[overtaken-witness] building base artifacts into $BUILD_DIR"
  rm -rf "$BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/build.log" 2>&1 || seal_fail build
  echo "[overtaken-witness] building kernel_boot with --features $FEATURE"
  cargo build -Z "build-std=$BUILD_STD" -Z json-target-spec --target "$KTARGET" \
    --profile "$KPROFILE" --no-default-features --features "$FEATURE" \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 || seal_fail kernel_build
  cp "target/x86_64-yarm-none/$KPROFILE/kernel_boot" "$BUILD_DIR/kernel_boot.elf" || seal_fail copy
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l) features=$FEATURE cmdline=$CMDLINE"
    echo "qemu=$(qemu-system-x86_64 --version | head -1) machine=q35 cpu=qemu64 smp=2"
    sha256sum "$BUILD_DIR/kernel_boot.elf" "$BUILD_DIR/initramfs-core.cpio"
  } >"$LOGDIR/artifact-identity.txt"

  source scripts/lib/qemu-x86-deterministic.sh
  QEMU_ARGV=(
    qemu-system-x86_64 -machine q35 -cpu qemu64 -m 512M -smp 2
    -nographic -monitor none -serial stdio -no-reboot -no-shutdown
    -kernel "$BUILD_DIR/kernel_boot.elf" -initrd "$BUILD_DIR/initramfs-core.cpio"
    -append "$CMDLINE"
  )
  QEMU_TERMINAL_MARKERS=("OVT_SUM part=6/6")
  FATAL_RE="KERNEL PANIC|RUST PANIC|panicked at|DOUBLE FAULT|OVERTAKEN_DEFERRAL_UNAUTHENTICATED|X86_USER_FPU_HOME_UNAUTHENTICATED|X86_POST_LOCK_DISPATCH_FATAL|OVT_WITNESS_[A-Z_]*FAIL"
  echo "[overtaken-witness] booting -smp 2 (ceiling ${TIMEOUT_SECS}s)"
  qemu_run_deterministic "$BOOT_LOG" "$FATAL_RE" "$TIMEOUT_SECS" "${QEMU_ARGV[@]}"
  rc=$?
  echo "[overtaken-witness] qemu lifecycle: ${QEMU_LIFECYCLE_RESULT} (rc=$rc)"
fi
[[ -s "$BOOT_LOG" ]] || seal_fail no_boot_log
python3 scripts/grade-overtaken-witness.py "$BOOT_LOG" x86_64
