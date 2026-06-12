#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 <stage1-kernel-image> [output-directory]" >&2
  exit 2
fi

kernel_image=$1
output_directory=${2:-build/rpi5-stage1-boot}

if [[ ! -f "$kernel_image" ]]; then
  echo "[error] kernel image not found: $kernel_image" >&2
  exit 1
fi

mkdir -p "$output_directory"
cp "$kernel_image" "$output_directory/kernel_2712.img"
cat > "$output_directory/config.txt" <<'CONFIG'
# Raspberry Pi 5 YARM Stage 1 UART-only scaffold.
arm_64bit=1
enable_uart=1
kernel=kernel_2712.img
CONFIG
cat > "$output_directory/cmdline.txt" <<'CMDLINE'
yarm.platform=auto yarm.boot_phase=uart yarm.max_cpus=1
CMDLINE

cat <<REPORT
[ok] staged Raspberry Pi 5 Stage 1 boot directory: $output_directory
[ok] kernel: $output_directory/kernel_2712.img
[warn] this scaffold does not build firmware files or prove the current linked image load address
[warn] this is UART-only Stage 1; it does not enable RP1, PCIe, SMP, initrd, or userspace boot
REPORT
