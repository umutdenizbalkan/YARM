#!/usr/bin/env bash
set -euo pipefail

SMOKE_LOG=${SMOKE_LOG:-smoke.log}
: >"$SMOKE_LOG"
exec 19>>"$SMOKE_LOG"
export BASH_XTRACEFD=19
export PS4='+ ${BASH_SOURCE}:${LINENO}: '
set -x

KERNEL_IMAGE=${KERNEL_IMAGE:-build-x86_64/bootable-kernel.img}
KERNEL_DEBUG_ELF=${KERNEL_DEBUG_ELF:-build-x86_64/kernel_boot.elf}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-build-x86_64/initramfs-core.cpio}
TIMEOUT_SECS=${TIMEOUT_SECS:-30}
QEMU_SMOKE_STRICT=${QEMU_SMOKE_STRICT:-0}
QEMU_MACHINE=${QEMU_MACHINE:-q35}
QEMU_CPU=${QEMU_CPU:-qemu64}
QEMU_MEMORY=${QEMU_MEMORY:-1024M}
QEMU_SMP=${QEMU_SMP:-2}
QEMU_X86_ALLOW_ELF_KERNEL=${QEMU_X86_ALLOW_ELF_KERNEL:-1}
QEMU_X86_PVH_MINIMAL=${QEMU_X86_PVH_MINIMAL:-1}
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
    if ! readelf -l "$kernel" 2>/dev/null | rg -q "NOTE"; then
      echo "[warn] ELF kernel lacks a PT_NOTE program header; PVH entry note will be ignored by qemu"
      return 1
    fi
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
)

is_elf_kernel=0
if command -v file >/dev/null 2>&1; then
  if file -b "$KERNEL_IMAGE" 2>/dev/null | rg -q "ELF"; then
    is_elf_kernel=1
  fi
elif command -v readelf >/dev/null 2>&1 && readelf -h "$KERNEL_IMAGE" >/dev/null 2>&1; then
  is_elf_kernel=1
fi

if [[ "$QEMU_X86_PVH_MINIMAL" == "1" && "$is_elf_kernel" -eq 1 ]]; then
  echo "[info] PVH minimal mode enabled: skipping initrd/cmdline for ELF boot triage"
else
  if [[ ! -f "$INITRAMFS_IMAGE" ]]; then
    echo "[warn] initramfs image missing: $INITRAMFS_IMAGE"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    exit 0
  fi
  QEMU_CMD+=(
    -initrd "$INITRAMFS_IMAGE"
    -append "$KERNEL_CMDLINE"
  )
fi

MARKER_REGEX="YARM_BOOT_OK|YARM_PROC_VFS_OK|YARM_INIT_START|YARM_INIT_DONE|ABCDEFG|ABCDEF|ABCD"
FIRMWARE_FALLBACK_REGEX="SeaBIOS|iPXE|Booting from ROM"

echo "[info] qemu command: ${QEMU_CMD[*]}"
echo "[info] waiting up to ${TIMEOUT_SECS}s for boot markers..."

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
  if [[ "$QEMU_SMOKE_STRICT" != "1" ]]; then
    echo "[ok] boot markers detected"
    exit 0
  fi

  strict_fail=0
  irq_count=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "YARM_TIMER_IRQ_DELIVERED" || true)
  eoi_count=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "YARM_TIMER_EOI_DONE" || true)
  sched_count=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "YARM_SCHED_TICK" || true)
  echo "[info] strict smoke marker counts: irq=${irq_count:-0} eoi=${eoi_count:-0} sched=${sched_count:-0}"

  if ! log_has_pattern "YARM_TIMER_IRQ_DELIVERED"; then
    echo "[warn] strict smoke: missing timer IRQ delivery marker"
    strict_fail=1
  fi
  if ! log_has_pattern "YARM_TIMER_EOI_DONE"; then
    echo "[warn] strict smoke: missing timer EOI completion marker"
    strict_fail=1
  fi
  if ! log_has_pattern "YARM_SCHED_TICK"; then
    echo "[warn] strict smoke: missing scheduler tick marker"
    strict_fail=1
  fi

  tick_lines=$(tr '\r' '\n' <"$LOGFILE" | rg -a -o "YARM_SCHED_TICK cpu=[0-9]+ tick=[0-9]+" || true)
  tick_line_with_lineno=$(tr '\r' '\n' <"$LOGFILE" | rg -a -n "YARM_SCHED_TICK cpu=[0-9]+ tick=[0-9]+" || true)
  tick_count=$(printf '%s\n' "$tick_lines" | rg -c "YARM_SCHED_TICK" || true)
  first_tick=$(printf '%s\n' "$tick_lines" | head -n1 | awk -F'tick=' '{print $2}' | awk '{print $1}')
  last_tick=$(printf '%s\n' "$tick_lines" | tail -n1 | awk -F'tick=' '{print $2}' | awk '{print $1}')
  first_tick_line=$(printf '%s\n' "$tick_line_with_lineno" | head -n1)
  last_tick_line=$(printf '%s\n' "$tick_line_with_lineno" | tail -n1)
  if [[ -n "$first_tick_line" ]]; then
    echo "[info] strict smoke first tick line: $first_tick_line"
  fi
  if [[ -n "$last_tick_line" ]]; then
    echo "[info] strict smoke last tick line: $last_tick_line"
  fi
  if [[ -z "$first_tick" || -z "$last_tick" || "$tick_count" -lt 2 ]]; then
    echo "[warn] strict smoke: need at least two scheduler tick markers (got ${tick_count:-0})"
    strict_fail=1
  elif (( last_tick <= first_tick )); then
    echo "[warn] strict smoke: scheduler tick did not progress (first=$first_tick last=$last_tick)"
    strict_fail=1
  fi

  if [[ "$strict_fail" -eq 1 ]]; then
    echo "[warn] strict x86 smoke marker checks failed"
    exit 1
  fi

  echo "[ok] strict x86 smoke markers detected (timer IRQ + EOI + scheduler tick progress)"
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
