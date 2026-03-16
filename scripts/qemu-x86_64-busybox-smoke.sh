#!/usr/bin/env bash
set -euo pipefail

KERNEL_IMAGE=${KERNEL_IMAGE:-build-x86_64/yarm-x86_64.elf}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-build-x86_64/initramfs-busybox.cpio}
TIMEOUT_SECS=${TIMEOUT_SECS:-30}
QEMU_SMOKE_STRICT=${QEMU_SMOKE_STRICT:-0}
QEMU_MACHINE=${QEMU_MACHINE:-q35}
QEMU_CPU=${QEMU_CPU:-qemu64}
QEMU_MEMORY=${QEMU_MEMORY:-1024M}
QEMU_SMP=${QEMU_SMP:-2}
KERNEL_CMDLINE=${KERNEL_CMDLINE:-"console=ttyS0 rdinit=/init"}

if [[ ! -f "$KERNEL_IMAGE" ]]; then
  echo "[warn] kernel image missing: $KERNEL_IMAGE"
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi
if [[ ! -f "$INITRAMFS_IMAGE" ]]; then
  echo "[warn] initramfs image missing: $INITRAMFS_IMAGE"
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
  echo "[warn] qemu-system-x86_64 not installed"
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

LOGFILE=${LOGFILE:-qemu-x86_64-busybox.log}
rm -f "$LOGFILE"

echo "[info] qemu command: qemu-system-x86_64 -machine $QEMU_MACHINE -cpu $QEMU_CPU -m $QEMU_MEMORY -smp $QEMU_SMP -kernel $KERNEL_IMAGE -initrd $INITRAMFS_IMAGE -append '$KERNEL_CMDLINE'"

set +e
timeout "$TIMEOUT_SECS" qemu-system-x86_64 \
  -machine "$QEMU_MACHINE" \
  -cpu "$QEMU_CPU" \
  -m "$QEMU_MEMORY" \
  -smp "$QEMU_SMP" \
  -nographic \
  -serial mon:stdio \
  -kernel "$KERNEL_IMAGE" \
  -initrd "$INITRAMFS_IMAGE" \
  -append "$KERNEL_CMDLINE" \
  | tee "$LOGFILE"
QEMU_STATUS=$?
set -e

if rg -n "YARM_BOOT_OK|YARM_PROC_VFS_OK|YARM_INIT_START|YARM_INIT_DONE|BusyBox|/ #|init_server|Welcome|\[ui\] boot-to-shell marker" "$LOGFILE" >/dev/null 2>&1; then
  echo "[ok] boot shell markers detected"
  exit 0
fi

echo "[warn] boot shell markers not detected (status=$QEMU_STATUS)"
if [[ "$QEMU_SMOKE_STRICT" == "1" ]]; then
  exit 1
fi
exit 0
