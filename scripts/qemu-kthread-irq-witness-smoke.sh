#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# KTHREAD1-ACCEPTANCE §2 — a real device interrupt taken while a KERNEL THREAD runs, on each port.
#
# Usage: ARCH=riscv64|aarch64|x86_64 scripts/qemu-kthread-irq-witness-smoke.sh
#   LOGDIR=...        where build/boot logs land (default /tmp/qemu-kthread-irq-witness-$ARCH)
#   SKIP_BUILD=1      reuse the artifacts already in $LOGDIR/build
#   REGRADE=1         grade the existing $LOGDIR/boot.log again without booting
#   DRIVER_ARGS=...   extra driver flags
#
# One one-CPU boot of the port's device-interrupt witness (RISC-V UART0/PLIC, AArch64 PL011/GICv2,
# x86_64 COM1/I/O APIC) built with `kthread-irq-witness`. Items 1..=8 are the witness's own idle
# and user items; items 9..=12 are KERNEL-THREAD items: the kernel's witness thread
# (`src/kernel/kirq_witness.rs`) opens the port's gate from inside a masked, register-patterned
# section and unmasks, so the device interrupt is taken on that thread, through the port's
# kernel-thread vector and the production claim/delivery/completion owners. The host driver
# (scripts/qemu-riscv64-uart-irq-driver.py) injects each item's byte on its READY line.
# Graded by scripts/grade-kthread-irq-witness.py (see its header). Never retries.
set -uo pipefail
cd "$(dirname "$0")/.."

ARCH=${ARCH:?ARCH=riscv64|aarch64|x86_64}
case "$ARCH" in
  riscv64) FEAT=riscv-uart-irq-witness; KBIN=yarm-riscv64.bin; KELF=yarm-riscv64.elf ;;
  aarch64) FEAT=aarch64-pl011-irq-witness; KBIN=yarm-aarch64.bin; KELF=yarm-aarch64.elf ;;
  x86_64)  FEAT=x86_64-uart-irq-witness; KBIN=kernel_boot.elf; KELF=kernel_boot.debug.elf ;;
  *) echo "KIRQ_WITNESS_SEAL arch=$ARCH result=fail reason=arch"; exit 1 ;;
esac
FEATS="$FEAT,kthread-irq-witness"
LOGDIR=${LOGDIR:-/tmp/qemu-kthread-irq-witness-$ARCH}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
NORM="$LOGDIR/boot.norm.log"
KELF_COPY="$LOGDIR/kernel_boot.elf"

if [[ "${SKIP_BUILD:-0}" != "1" && "${REGRADE:-0}" != "1" ]]; then
  echo "[kirq-witness] building $ARCH artifacts with $FEATS"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $FEATS" \
    "scripts/build-qemu-$ARCH-artifacts.sh" >"$LOGDIR/build.log" 2>&1 \
    || { echo "KIRQ_WITNESS_SEAL arch=$ARCH result=fail reason=build"; exit 1; }
  cp "$BUILD_DIR/$KELF" "$KELF_COPY"
fi

if [[ "${REGRADE:-0}" == "1" ]]; then
  driver_status=$(grep -a -o '#HOST_SUMMARY .* status=[0-9]*' "$BOOT_LOG" | sed 's/.*status=//' | tail -1)
  driver_status=${driver_status:-9}
else
  # BUILD-GATES1: boot only the recorded, no-FP-checked pair this run expects.
  source scripts/lib/artifact-binding.sh
  if ! artifact_boot_guard "$ARCH" "$BUILD_DIR/$KBIN" "$BUILD_DIR/initramfs-core.cpio" \
      "kernel=$FEATS" "initramfs=$FEATS" >"$LOGDIR/artifact-binding.txt" 2>&1; then
    cat "$LOGDIR/artifact-binding.txt"
    echo "KIRQ_WITNESS_SEAL arch=$ARCH result=fail reason=artifact-binding"; exit 1
  fi
  cat "$LOGDIR/artifact-binding.txt"
  echo "[kirq-witness] booting (dedicated socket serial, host injection on READY)"
  # shellcheck disable=SC2086
  python3 scripts/qemu-riscv64-uart-irq-driver.py --arch "$ARCH" \
    --kernel "$BUILD_DIR/$KBIN" --initrd "$BUILD_DIR/initramfs-core.cpio" \
    --log "$BOOT_LOG" --timeout "${TIMEOUT_SECS:-300}" ${DRIVER_ARGS:-}
  driver_status=$?
fi
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
fail=0
[[ $driver_status -eq 0 ]] || { echo "[kirq-witness][fail] driver status $driver_status (0 = witness summary seen)"; fail=1; }
python3 -I scripts/grade-kthread-irq-witness.py --arch "$ARCH" --kernel-elf "$KELF_COPY" "$NORM" || fail=1
if (( fail )); then
  echo "KIRQ_WITNESS_SEAL arch=$ARCH result=fail"
  exit 1
fi
echo "KIRQ_WITNESS_SEAL arch=$ARCH items=12 kthread_items=4 result=ok"
