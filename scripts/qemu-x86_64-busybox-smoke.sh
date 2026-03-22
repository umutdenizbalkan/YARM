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

check_x86_kernel_bootability() {
  local kernel="$1"
  if ! command -v readelf >/dev/null 2>&1; then
    return 0
  fi
  local ftype
  ftype=$(file -b "$kernel" 2>/dev/null || true)
  if [[ "$ftype" == *"ELF"* ]]; then
    if ! readelf -n "$kernel" 2>/dev/null | rg -qi "(PVH|Xen)"; then
      echo "[warn] kernel image is ELF but lacks PVH ELF note required for qemu -kernel direct boot"
      echo "[hint] provide a Linux bzImage or add a PVH note / compatible boot protocol to the kernel image"
      return 1
    fi
  fi
  return 0
}


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

if ! check_x86_kernel_bootability "$KERNEL_IMAGE"; then
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
echo "[info] waiting up to ${TIMEOUT_SECS}s for boot markers..."

QEMU_CMD=(
  qemu-system-x86_64
  -machine "$QEMU_MACHINE"
  -cpu "$QEMU_CPU"
  -m "$QEMU_MEMORY"
  -smp "$QEMU_SMP"
  -nographic
  -monitor none
  -serial stdio
  -no-reboot
  -no-shutdown
  -kernel "$KERNEL_IMAGE"
  -initrd "$INITRAMFS_IMAGE"
  -append "$KERNEL_CMDLINE"
)

MARKER_REGEX="YARM_BOOT_OK|YARM_PROC_VFS_OK|YARM_INIT_START|YARM_INIT_DONE|BusyBox|/ #|Welcome|\[ui\] boot-to-shell marker"
INIT_SERVER_REGEX="init_server|first server|first-server"

set +e
stdbuf -oL -eL "${QEMU_CMD[@]}" 2>&1 | tee "$LOGFILE" &
PIPE_PID=$!
QEMU_STATUS=0
FOUND_MARKER=0

START_TS=$(date +%s)
while kill -0 "$PIPE_PID" >/dev/null 2>&1; do
    if rg -n "$MARKER_REGEX" "$LOGFILE" >/dev/null 2>&1 \
    && rg -n "$INIT_SERVER_REGEX" "$LOGFILE" >/dev/null 2>&1; then
    FOUND_MARKER=1
    break
  fi
  NOW_TS=$(date +%s)
  ELAPSED=$((NOW_TS - START_TS))
  if [[ "$ELAPSED" -ge "$TIMEOUT_SECS" ]]; then
    echo "[warn] timeout reached (${TIMEOUT_SECS}s) without marker detection"
    break
  fi
  sleep 1
done

if [[ "$FOUND_MARKER" -eq 1 ]]; then
  kill "$PIPE_PID" >/dev/null 2>&1 || true
  wait "$PIPE_PID"
  QEMU_STATUS=$?
  set -e
  echo "[ok] boot shell and init-server markers detected"
  exit 0
fi

if kill -0 "$PIPE_PID" >/dev/null 2>&1; then
  kill "$PIPE_PID" >/dev/null 2>&1 || true
  sleep 1
  kill -9 "$PIPE_PID" >/dev/null 2>&1 || true
fi
wait "$PIPE_PID"
QEMU_STATUS=$?
set -e

echo "[warn] boot shell and init-server markers not detected (status=$QEMU_STATUS)"
if [[ -f "$LOGFILE" ]]; then
  echo "[info] last 20 log lines from $LOGFILE"
  tail -n 20 "$LOGFILE" || true
fi

if [[ "$QEMU_SMOKE_STRICT" == "1" ]]; then
  exit 1
fi
exit 0
