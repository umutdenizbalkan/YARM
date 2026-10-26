#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# KERNEL-NOFP1 — run the final-image FP/SIMD check on one kernel image.
#
#   scripts/check-kernel-nofp.sh ARCH KERNEL_ELF [FEATURES] [BOOT_IMAGE]
#
# ARCH is x86_64, aarch64 or riscv64; KERNEL_ELF is the final linked kernel (`kernel_boot`);
# FEATURES the comma-separated cargo features it was built with; BOOT_IMAGE the raw binary QEMU
# loads, when it is not the ELF itself (AArch64, RISC-V). Prints one `NOFP_CHECK ...` result line
# bound to the image hash, features, toolchain and disassembler, and exits non-zero unless the
# image passes. See `scripts/check-kernel-nofp.py` and doc/SCHEDULING_CONTRACT.md §8.1.
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ $# -lt 2 ]]; then
  echo "usage: $0 x86_64|aarch64|riscv64 KERNEL_ELF [FEATURES] [BOOT_IMAGE]" >&2
  exit 2
fi
toolchain="$(rustc -vV 2>/dev/null | awk '/^release:|^commit-hash:/ {printf "%s%s", sep, $2; sep=" "}')"
args=(--arch "$1" --elf "$2" --features "${3:-}" --toolchain "${toolchain:-unknown}")
if [[ -n "${4:-}" ]]; then
  args+=(--boot-image "$4")
fi
exec python3 -I scripts/check-kernel-nofp.py "${args[@]}"
