#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

usage() {
  cat <<USAGE
Usage: $0 --kernel-input PATH [options]

Create a Raspberry Pi 5 Stage 1 hardware-smoke boot directory.

Required:
  --kernel-input PATH    Raw Stage 1 kernel image to copy as kernel_2712.img.

Options:
  --initrd-input PATH    Optional newc CPIO image to stage as initramfs-stage2a.cpio.
  --boot-dir PATH        Output directory (default: build/rpi5-stage1-boot).
  --phase PHASE          Stop phase: entry, uart, dtb, mmu, or kernel
                         (default: uart).
  --cmdline-extra TEXT   Append TEXT to the generated one-line cmdline.txt.
  --os-check-off         Add os_check=0 to config.txt for bare-metal testing.
  --enable-rp1-uart      Add enable_rp1_uart=1; Pi 5-specific and opt-in.
  --force                Replace generator-owned files if they already exist.
  -h, --help             Show this help.

This creates config.txt, cmdline.txt, kernel_2712.img, and
README-RPI5-STAGE1.txt. When --initrd-input is supplied it also creates
initramfs-stage2a.cpio. It does not download firmware or claim full Pi 5 support.
USAGE
}

fail() {
  echo "[error] $*" >&2
  exit 1
}

kernel_input=
initrd_input=
boot_dir=build/rpi5-stage1-boot
phase=uart
cmdline_extra=
os_check_off=false
enable_rp1_uart=false
force=false

while (($# > 0)); do
  case "$1" in
    --kernel-input)
      (($# >= 2)) || fail "--kernel-input requires a path"
      kernel_input=$2
      shift 2
      ;;
    --initrd-input)
      (($# >= 2)) || fail "--initrd-input requires a path"
      initrd_input=$2
      shift 2
      ;;
    --boot-dir)
      (($# >= 2)) || fail "--boot-dir requires a path"
      boot_dir=$2
      shift 2
      ;;
    --phase)
      (($# >= 2)) || fail "--phase requires entry, uart, dtb, mmu, or kernel"
      phase=$2
      shift 2
      ;;
    --cmdline-extra)
      (($# >= 2)) || fail "--cmdline-extra requires a string"
      cmdline_extra=$2
      shift 2
      ;;
    --os-check-off)
      os_check_off=true
      shift
      ;;
    --enable-rp1-uart)
      enable_rp1_uart=true
      shift
      ;;
    --force)
      force=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      (($# == 0)) || fail "unexpected positional argument: $1"
      ;;
    -*)
      fail "unknown option: $1 (use --help for usage)"
      ;;
    *)
      fail "unexpected positional argument: $1 (use --kernel-input and --boot-dir)"
      ;;
  esac
done

[[ -n "$kernel_input" ]] || fail "--kernel-input is required (use --help for usage)"
[[ -f "$kernel_input" ]] || fail "kernel input is not a file: $kernel_input"
if [[ -n "$initrd_input" ]]; then
  [[ -f "$initrd_input" ]] || fail "initrd input is not a file: $initrd_input"
fi
[[ -n "$boot_dir" ]] || fail "--boot-dir must not be empty"
case "$phase" in
  entry|uart|dtb|mmu|kernel) ;;
  *) fail "invalid phase '$phase'; expected entry, uart, dtb, mmu, or kernel" ;;
esac
if [[ "$cmdline_extra" == *$'\n'* || "$cmdline_extra" == *$'\r'* ]]; then
  fail "--cmdline-extra must be a single line"
fi

output_files=(
  "$boot_dir/config.txt"
  "$boot_dir/cmdline.txt"
  "$boot_dir/kernel_2712.img"
  "$boot_dir/README-RPI5-STAGE1.txt"
)
if [[ -n "$initrd_input" ]]; then
  output_files+=("$boot_dir/initramfs-stage2a.cpio")
fi
if [[ "$force" != true ]]; then
  for output_file in "${output_files[@]}"; do
    [[ ! -e "$output_file" ]] || fail "output already exists: $output_file (use --force to replace generator-owned files)"
  done
fi

mkdir -p -- "$boot_dir" || fail "could not create boot directory: $boot_dir"
[[ -d "$boot_dir" ]] || fail "boot directory was not created: $boot_dir"
[[ -w "$boot_dir" ]] || fail "boot directory is not writable: $boot_dir"

cmdline="yarm.platform=auto yarm.boot_phase=$phase yarm.max_cpus=1"
if [[ -n "$cmdline_extra" ]]; then
  cmdline+=" $cmdline_extra"
fi

cp -- "$kernel_input" "$boot_dir/kernel_2712.img"
if [[ -n "$initrd_input" ]]; then
  cp -- "$initrd_input" "$boot_dir/initramfs-stage2a.cpio"
fi

{
  echo "# Raspberry Pi 5 YARM Stage 1 UART bring-up scaffold; not full Pi 5 support."
  echo "kernel=kernel_2712.img"
  echo "arm_64bit=1"
  echo "enable_uart=1"
  echo "uart_2ndstage=1"
  if [[ -n "$initrd_input" ]]; then
    # Raspberry Pi firmware's initramfs directive is space-separated (no '=').
    # followkernel asks firmware to place the blob after the loaded kernel and
    # publish linux,initrd-start/end in /chosen.
    echo "initramfs initramfs-stage2a.cpio followkernel"
  fi
  if [[ "$os_check_off" == true ]]; then
    echo "os_check=0"
  fi
  if [[ "$enable_rp1_uart" == true ]]; then
    echo "enable_rp1_uart=1"
  fi
} > "$boot_dir/config.txt"

printf '%s\n' "$cmdline" > "$boot_dir/cmdline.txt"

cat > "$boot_dir/README-RPI5-STAGE1.txt" <<EOF_README
Raspberry Pi 5 YARM Stage 1 hardware-smoke boot directory
==========================================================

This directory is a deliberately limited Stage 1 diagnostic scaffold. It does
not claim full Raspberry Pi 5 support and does not include Raspberry Pi firmware.
Copy these files alongside suitable Raspberry Pi 5 firmware on the FAT boot
partition. The staged kernel still requires a verified Pi 5 link/load layout.

Selected stop phase: $phase
Generated cmdline: $cmdline

Expected serial markers, in order:
  RPI5_BOOT_00_ENTRY
  RPI5_BOOT_01_DTB_PTR
  RPI5_BOOT_02_UART_SELECTED
  RPI5_BOOT_03_UART_OK

Stage2A initrd diagnostics (boot_phase=kernel):
  RPI5_INITRD_DETECT_BEGIN
  RPI5_INITRD_DTB_PROPS
  RPI5_INITRD_RANGE
  RPI5_INITRD_RESERVED
  RPI5_INITRD_CPIO_MAGIC_OK
  RPI5_INITRD_CPIO_FIRST_ENTRY
  RPI5_INITRD_READY
  RPI5_STAGE2A_DONE

To stage an initrd, rerun this generator with:
  --initrd-input build-aarch64/initramfs-core.cpio

The generator copies it as initramfs-stage2a.cpio and adds:
  initramfs initramfs-stage2a.cpio followkernel

Stage2A validates and reserves the archive and reads only its first newc entry.
It does not unpack the archive or spawn userspace.

Troubleshooting
---------------
| Serial result                         | Likely boundary to investigate |
|---------------------------------------|--------------------------------|
| no output                             | Firmware files, power, serial wiring/voltage, baud rate, image load/entry, and uart_2ndstage output. |
| only RPI5_BOOT_00_ENTRY               | DTB pointer handoff or DTB header/access before UART discovery. |
| only RPI5_BOOT_01_DTB_PTR             | /chosen/stdout-path, aliases, PL011 status/compatible, or DTB address translation. |
| only RPI5_BOOT_02_UART_SELECTED       | Selected UART MMIO base, clock/configuration, or transmitter readiness. |
| reaches RPI5_BOOT_03_UART_OK          | Stage 1 UART path succeeded; investigate the selected later phase without treating this as full boot support. |

Generated files
---------------
  config.txt             Pi firmware configuration for this smoke test.
  cmdline.txt            YARM platform, stop phase, CPU limit, and extras.
  kernel_2712.img        Exact copy of the --kernel-input file.
  initramfs-stage2a.cpio Present only when --initrd-input is supplied.
  README-RPI5-STAGE1.txt This guide.

Optional firmware settings are intentionally absent unless requested:
  os_check=0             Generated only with --os-check-off.
  enable_rp1_uart=1      Generated only with --enable-rp1-uart.
EOF_README

cat <<REPORT
[ok] staged Raspberry Pi 5 Stage 1 boot directory: $boot_dir
[ok] kernel: $boot_dir/kernel_2712.img
[ok] phase: $phase
[ok] cmdline: $cmdline
[warn] this scaffold does not build firmware files or prove the linked image load address
[warn] this is Stage 1 diagnostics; it does not claim full Raspberry Pi 5 support
REPORT
if [[ -n "$initrd_input" ]]; then
  echo "[ok] initrd: $boot_dir/initramfs-stage2a.cpio"
fi
