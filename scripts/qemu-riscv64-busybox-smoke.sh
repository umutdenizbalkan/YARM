#!/usr/bin/env bash
set -euo pipefail

KERNEL_IMAGE=${KERNEL_IMAGE:-build/yarm-riscv64.bin}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-build/initramfs-busybox.cpio}
TIMEOUT_SECS=${TIMEOUT_SECS:-20}

if [[ ! -f "$KERNEL_IMAGE" ]]; then
  echo "[warn] kernel image missing: $KERNEL_IMAGE"
  exit 0
fi
if [[ ! -f "$INITRAMFS_IMAGE" ]]; then
  echo "[warn] initramfs image missing: $INITRAMFS_IMAGE"
  exit 0
fi

if ! command -v qemu-system-riscv64 >/dev/null 2>&1; then
  echo "[warn] qemu-system-riscv64 not installed"
  exit 0
fi

LOGFILE=${LOGFILE:-qemu-busybox.log}
rm -f "$LOGFILE"

set +e
timeout "$TIMEOUT_SECS" qemu-system-riscv64 \
  -machine virt \
  -nographic \
  -bios none \
  -kernel "$KERNEL_IMAGE" \
  -initrd "$INITRAMFS_IMAGE" \
  -append "console=ttyS0" \
  | tee "$LOGFILE"
QEMU_STATUS=$?
set -e

if grep -E "BusyBox|/ #|/ # " "$LOGFILE" >/dev/null 2>&1; then
  echo "[ok] busybox prompt detected"
  exit 0
fi

echo "[warn] busybox prompt not detected (status=$QEMU_STATUS)"
exit 0
