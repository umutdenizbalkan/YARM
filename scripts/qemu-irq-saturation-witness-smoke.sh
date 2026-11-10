#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# IRQ-DELIVERY1 — the Notification saturation window of the device-interrupt witnesses, on the
# port's own witness machine: RISC-V UART0/PLIC, AArch64 PL011/GICv2, x86_64 COM1/I/O APIC.
#
# Usage: ARCH=riscv64|aarch64|x86_64 scripts/qemu-irq-saturation-witness-smoke.sh
#   LOGDIR=...        where build/boot logs land (default /tmp/qemu-irq-saturation-witness-$ARCH)
#   SKIP_BUILD=1      reuse $LOGDIR/build
#   REGRADE=1         re-grade an existing boot log without booting
#
# One boot built with the port's witness feature plus `irq-saturation-witness`. Items 1..=8 are
# the witness's own idle/user items. Then init stops receiving and announces
# `IRQ1_UART_SAT_READY`; the host driver injects ten bytes, each only after the kernel's per-claim
# DRAIN marker for the previous one, so each is a real device interrupt claimed and completed by
# the production owners while the depth-8 witness Notification is not drained. Eight queue, two
# coalesce; init then receives eight events plus the one pending line, and the boot continues to
# the witness's post-disable silence and isolation phases.
#
# Graded by scripts/grade-irq-saturation-witness.py (see its header). Never retries.
set -uo pipefail
cd "$(dirname "$0")/.."

ARCH=${ARCH:?ARCH=riscv64|aarch64|x86_64}
case "$ARCH" in
  riscv64) FEAT=riscv-uart-irq-witness; KBIN=yarm-riscv64.bin; KELF=yarm-riscv64.elf ;;
  aarch64) FEAT=aarch64-pl011-irq-witness; KBIN=yarm-aarch64.bin; KELF=yarm-aarch64.elf ;;
  x86_64)  FEAT=x86_64-uart-irq-witness; KBIN=kernel_boot.elf; KELF=kernel_boot.debug.elf ;;
  *) echo "IRQSAT_WITNESS_SEAL arch=$ARCH result=fail reason=arch"; exit 1 ;;
esac
FEATS="$FEAT,irq-saturation-witness"
LOGDIR=${LOGDIR:-/tmp/qemu-irq-saturation-witness-$ARCH}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
NORM="$LOGDIR/boot.norm.log"
KELF_COPY="$LOGDIR/kernel_boot.elf"

if [[ "${SKIP_BUILD:-0}" != "1" && "${REGRADE:-0}" != "1" ]]; then
  echo "[irqsat-witness] building $ARCH artifacts with $FEATS"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $FEATS" \
    "scripts/build-qemu-$ARCH-artifacts.sh" >"$LOGDIR/build.log" 2>&1 \
    || { echo "IRQSAT_WITNESS_SEAL arch=$ARCH result=fail reason=build"; exit 1; }
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
    echo "IRQSAT_WITNESS_SEAL arch=$ARCH result=fail reason=artifact-binding"; exit 1
  fi
  cat "$LOGDIR/artifact-binding.txt"
  echo "[irqsat-witness] booting (dedicated socket serial, host injection on READY / DRAIN)"
  # shellcheck disable=SC2086
  python3 scripts/qemu-riscv64-uart-irq-driver.py --arch "$ARCH" \
    --kernel "$BUILD_DIR/$KBIN" --initrd "$BUILD_DIR/initramfs-core.cpio" \
    --log "$BOOT_LOG" --timeout "${TIMEOUT_SECS:-300}" ${DRIVER_ARGS:-}
  driver_status=$?
fi
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
fail=0
[[ $driver_status -eq 0 ]] || { echo "[irqsat-witness][fail] driver status $driver_status (0 = witness summary seen)"; fail=1; }
python3 -I scripts/grade-irq-saturation-witness.py --arch "$ARCH" "$NORM" || fail=1
if (( fail )); then
  echo "IRQSAT_WITNESS_SEAL arch=$ARCH result=fail"
  exit 1
fi
echo "IRQSAT_WITNESS_SEAL arch=$ARCH items=8 saturation_interrupts=10 queued=8 coalesced=2 received=9 result=ok"
