#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/qemu-smoke-common.sh"

KERNEL_IMAGE=${KERNEL_IMAGE:-build/yarm-riscv64.bin}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-build/initramfs-core.cpio}
TIMEOUT_SECS=${TIMEOUT_SECS:-30}
QEMU_SMOKE_STRICT=${QEMU_SMOKE_STRICT:-0}
QEMU_MACHINE=${QEMU_MACHINE:-virt}
QEMU_CPU=${QEMU_CPU:-rv64}
QEMU_MEMORY=${QEMU_MEMORY:-512M}
QEMU_SMP=${QEMU_SMP:-1}
QEMU_BIOS=${QEMU_BIOS:-default}
KERNEL_CMDLINE=${KERNEL_CMDLINE:-"console=ttyS0 rdinit=/init"}

require_file_or_warn "$KERNEL_IMAGE" "$QEMU_SMOKE_STRICT" "kernel image"
require_file_or_warn "$INITRAMFS_IMAGE" "$QEMU_SMOKE_STRICT" "initramfs image"
require_qemu_or_warn "qemu-system-riscv64" "$QEMU_SMOKE_STRICT"

LOGFILE=${LOGFILE:-qemu-core.log}
rm -f "$LOGFILE"

echo "[info] qemu command: qemu-system-riscv64 -machine $QEMU_MACHINE -cpu $QEMU_CPU -m $QEMU_MEMORY -smp $QEMU_SMP -bios $QEMU_BIOS -kernel $KERNEL_IMAGE -initrd $INITRAMFS_IMAGE -append '$KERNEL_CMDLINE'"

if run_qemu_timeout_to_log "$TIMEOUT_SECS" "$LOGFILE" qemu-system-riscv64 \
  -machine "$QEMU_MACHINE" \
  -cpu "$QEMU_CPU" \
  -m "$QEMU_MEMORY" \
  -smp "$QEMU_SMP" \
  -nographic \
  -bios "$QEMU_BIOS" \
  -kernel "$KERNEL_IMAGE" \
  -initrd "$INITRAMFS_IMAGE" \
  -append "$KERNEL_CMDLINE" \
; then
  QEMU_STATUS=0
else
  QEMU_STATUS=$?
fi

MARKER_REGEX="YARM_BOOT_OK|YARM_PROC_VFS_OK|YARM_INIT_START|YARM_INIT_DONE|BusyBox|/ #|Welcome|\[ui\] boot-to-shell marker"
INIT_SERVER_REGEX="init_server|first server|first-server"

if check_common_boot_markers "$LOGFILE" "$MARKER_REGEX" "$INIT_SERVER_REGEX"; then
  exit 0
fi

echo "[warn] boot shell and init-server markers not detected (status=$QEMU_STATUS)"
if [[ "$QEMU_SMOKE_STRICT" == "1" ]]; then
  exit 1
fi
exit 0
