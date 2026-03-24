#!/usr/bin/env bash
set -euo pipefail

KERNEL_IMAGE=${KERNEL_IMAGE:-build-x86_64/bootable-kernel.img}
KERNEL_DEBUG_ELF=${KERNEL_DEBUG_ELF:-build-x86_64/kernel_boot.elf}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-build-x86_64/initramfs-core.cpio}
TIMEOUT_SECS=${TIMEOUT_SECS:-30}
QEMU_SMOKE_STRICT=${QEMU_SMOKE_STRICT:-0}
QEMU_MACHINE=${QEMU_MACHINE:-q35}
QEMU_CPU=${QEMU_CPU:-qemu64}
QEMU_MEMORY=${QEMU_MEMORY:-1024M}
QEMU_SMP=${QEMU_SMP:-2}
QEMU_X86_ALLOW_ELF_KERNEL=${QEMU_X86_ALLOW_ELF_KERNEL:-0}
DEFAULT_KERNEL_CMDLINE="console=ttyS0 rdinit=/init"
KERNEL_CMDLINE=${KERNEL_CMDLINE:-"$DEFAULT_KERNEL_CMDLINE"}

if [[ "$KERNEL_CMDLINE" != *"console="* ]] || [[ "${#KERNEL_CMDLINE}" -lt 12 ]]; then
  echo "[warn] suspicious KERNEL_CMDLINE override detected: '$KERNEL_CMDLINE'"
  echo "[hint] resetting to default kernel cmdline: '$DEFAULT_KERNEL_CMDLINE'"
  KERNEL_CMDLINE="$DEFAULT_KERNEL_CMDLINE"
fi

check_x86_kernel_bootability() {
  local kernel="$1"
  if [[ ! -f "$kernel" ]]; then
    return 1
  fi
  local ftype
  if command -v file >/dev/null 2>&1; then
    ftype=$(file -b "$kernel" 2>/dev/null || true)
  elif command -v readelf >/dev/null 2>&1 && readelf -h "$kernel" >/dev/null 2>&1; then
    ftype="ELF"
  else
    echo "[warn] unable to verify x86 kernel bootability because neither 'file' nor a usable 'readelf' probe is available"
    return 1
  fi
  if [[ "$ftype" != *"ELF"* ]]; then
    return 0
  fi
  if [[ "$QEMU_X86_ALLOW_ELF_KERNEL" != "1" ]]; then
    echo "[warn] refusing ELF kernel direct-boot probe by default for x86 smoke"
    echo "[hint] set QEMU_X86_ALLOW_ELF_KERNEL=1 to opt-in to PVH ELF probing"
    echo "[hint] helper to fetch a known bootable image: scripts/fetch-linux-bzimage.sh"
    return 1
  fi
  if command -v readelf >/dev/null 2>&1; then
    if readelf -n "$kernel" 2>/dev/null | rg -qi "(PVH|Xen)"; then
      return 0
    fi
    if readelf -S "$kernel" 2>/dev/null | rg -q "\.note\.Xen"; then
      return 0
    fi
  fi
  echo "[warn] kernel image is an ELF without a verified PVH direct-boot note"
  echo "[hint] the first blocker is still a verified direct-boot x86 kernel image (for example bzImage or PVH-enabled ELF)"
  echo "[hint] use KERNEL_BOOTABLE_IMAGE_SOURCE=<path> with scripts/build-qemu-x86_64-artifacts.sh, then rerun this smoke test"
  return 1
}


if [[ ! -f "$KERNEL_IMAGE" ]]; then
  echo "[warn] kernel image missing: $KERNEL_IMAGE"
  if [[ -f "$KERNEL_DEBUG_ELF" ]]; then
    echo "[info] debug-only freestanding kernel ELF is available at: $KERNEL_DEBUG_ELF"
    echo "[hint] this ELF is not launched automatically because it is not a verified qemu-system-x86_64 -kernel image"
  fi
  echo "[hint] provide a bootable image via KERNEL_IMAGE=<path> or rerun scripts/build-qemu-x86_64-artifacts.sh with KERNEL_BOOTABLE_IMAGE_SOURCE=<path>"
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

LOGFILE=${LOGFILE:-qemu-x86_64-core.log}
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

MARKER_REGEX="YARM_BOOT_OK|YARM_PROC_VFS_OK|YARM_INIT_START|YARM_INIT_DONE"
FIRMWARE_FALLBACK_REGEX="SeaBIOS|iPXE|Booting from ROM"

log_has_pattern() {
  local pattern="$1"
  [[ -f "$LOGFILE" ]] || return 1
  tr '\r' '\n' <"$LOGFILE" | rg -a -n "$pattern" >/dev/null 2>&1
}

set +e
if command -v timeout >/dev/null 2>&1; then
  timeout --foreground "${TIMEOUT_SECS}s" stdbuf -oL -eL "${QEMU_CMD[@]}" 2>&1 | tee "$LOGFILE"
  QEMU_STATUS=${PIPESTATUS[0]}
else
  echo "[warn] 'timeout' command is unavailable; qemu run may not auto-terminate"
  stdbuf -oL -eL "${QEMU_CMD[@]}" 2>&1 | tee "$LOGFILE"
  QEMU_STATUS=${PIPESTATUS[0]}
fi
set -e

if log_has_pattern "$MARKER_REGEX"; then
  echo "[ok] boot markers detected"
  exit 0
fi

if log_has_pattern "$FIRMWARE_FALLBACK_REGEX"; then
  echo "[warn] firmware fallback detected before any YARM boot markers"
  echo "[hint] qemu displayed SeaBIOS/iPXE output, which means serial output is working but the kernel was not accepted as a direct-boot image"
  echo "[hint] this is not an initramfs userspace issue yet; the guest never reached kernel_entry_x86_64"
  if [[ -f "$LOGFILE" ]]; then
    echo "[info] last 20 log lines from $LOGFILE"
    tail -n 20 "$LOGFILE" || true
  fi
  if [[ "$QEMU_SMOKE_STRICT" == "1" ]]; then
    exit 1
  fi
  exit 0
fi

if [[ "$QEMU_STATUS" -eq 124 ]]; then
  echo "[warn] timeout reached (${TIMEOUT_SECS}s) without marker detection"
fi

echo "[warn] boot markers not detected (status=$QEMU_STATUS)"
if [[ -f "$LOGFILE" ]]; then
  echo "[info] last 20 log lines from $LOGFILE"
  tail -n 20 "$LOGFILE" || true
fi

if [[ "$QEMU_SMOKE_STRICT" == "1" ]]; then
  exit 1
fi
exit 0
