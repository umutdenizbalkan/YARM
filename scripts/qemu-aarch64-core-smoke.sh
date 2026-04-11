#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail
source "$(dirname "$0")/qemu-smoke-common.sh"

KERNEL_IMAGE=${KERNEL_IMAGE:-build-aarch64/yarm-aarch64.elf}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-build-aarch64/initramfs-core.cpio}
TIMEOUT_SECS=${TIMEOUT_SECS:-30}
QEMU_SMOKE_STRICT=${QEMU_SMOKE_STRICT:-0}
QEMU_MACHINE=${QEMU_MACHINE:-virt}
QEMU_CPU=${QEMU_CPU:-cortex-a72}
QEMU_MEMORY=${QEMU_MEMORY:-1024M}
QEMU_SMP=${QEMU_SMP:-2}
# Keep kernel cmdline empty by default until AArch64 command-line parsing is
# explicitly validated. Override if needed via KERNEL_CMDLINE=...
KERNEL_CMDLINE=${KERNEL_CMDLINE:-}

require_file_or_warn "$KERNEL_IMAGE" "$QEMU_SMOKE_STRICT" "kernel image"
require_file_or_warn "$INITRAMFS_IMAGE" "$QEMU_SMOKE_STRICT" "initramfs image"
QEMU_BIN=${QEMU_BIN:-qemu-system-aarch64-hwe}
if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  QEMU_BIN=qemu-system-aarch64
fi
require_qemu_or_warn "$QEMU_BIN" "$QEMU_SMOKE_STRICT"

LOGFILE=${LOGFILE:-qemu-aarch64-core.log}
rm -f "$LOGFILE"

QEMU_ARGS=(
  -machine "$QEMU_MACHINE"
  -cpu "$QEMU_CPU"
  -m "$QEMU_MEMORY"
  -smp "$QEMU_SMP"
  -nographic
  -monitor none
  -serial stdio
  -kernel "$KERNEL_IMAGE"
  -initrd "$INITRAMFS_IMAGE"
)
if [[ -n "$KERNEL_CMDLINE" ]]; then
  QEMU_ARGS+=(-append "$KERNEL_CMDLINE")
fi

echo "[info] qemu command: $QEMU_BIN ${QEMU_ARGS[*]}"

MARKER_REGEX="YARM_AARCH64_BOOT_MARKER|YARM_BOOT_OK|YARM_PROC_VFS_OK|YARM_INIT_START|YARM_INIT_DONE|BusyBox|/ #|Welcome|\[ui\] boot-to-shell marker"
INIT_SERVER_REGEX="init_server|first server|first-server"
EARLY_MARKER_SEQUENCE=(
  "YARM_AARCH64_BOOT_MARKER stage=_start"
  "YARM_AARCH64_BOOT_MARKER stage=prepare_arch_boot"
  "YARM_AARCH64_BOOT_MARKER stage=vbar_el1_ready"
  "YARM_AARCH64_BOOT_MARKER stage=mmu_enabled"
  "YARM_AARCH64_BOOT_MARKER stage=run_with_prepared_kernel"
  "YARM_BOOT_OK"
  "YARM_INIT_START"
  "YARM_INIT_DONE"
)

if run_qemu_timeout_to_log "$TIMEOUT_SECS" "$LOGFILE" "$QEMU_BIN" "${QEMU_ARGS[@]}"; then
  QEMU_STATUS=0
else
  QEMU_STATUS=$?
fi

if check_common_boot_markers "$LOGFILE" "$MARKER_REGEX" "$INIT_SERVER_REGEX"; then
  if ! check_required_patterns "$LOGFILE" "${EARLY_MARKER_SEQUENCE[@]}"; then
    echo "[warn] aarch64 strict required markers are incomplete"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    exit 0
  fi
  if ! check_log_sequence "$LOGFILE" "${EARLY_MARKER_SEQUENCE[@]}"; then
    echo "[warn] aarch64 early boot marker sequence missing or out of order"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    exit 0
  fi

  if ! check_required_patterns "$LOGFILE" \
      "YARM_TIMER_IRQ_DELIVERED" \
      "YARM_TIMER_EOI_DONE" \
      "YARM_SCHED_TICK"; then
    echo "[warn] aarch64 timer progression markers missing"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    exit 0
  fi
  echo "[ok] aarch64 strict marker progression detected"
  exit 0
fi

echo "[warn] boot shell and init-server markers not detected (status=$QEMU_STATUS)"
if [[ -f "$LOGFILE" ]]; then
  echo "[info] last 20 log lines from $LOGFILE"
  tail -n 20 "$LOGFILE" || true
fi

if [[ "$QEMU_SMOKE_STRICT" == "1" ]]; then
  exit 1
fi
exit 0
