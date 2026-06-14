#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

output=build-rpi5/kernel_2712_hh.img
target_dir=${CARGO_TARGET_DIR:-target}
target_name=aarch64-rpi5-stage2-highhalf-none
profile=aarch64-none

usage() {
  cat <<USAGE
Usage: $0 [--output PATH]

Build the explicit RPi5 HH-3 diagnostic image. The output defaults to:
  build-rpi5/kernel_2712_hh.img

This script never replaces build-rpi5/kernel_2712.img.
USAGE
}

fail() {
  echo "[error] $*" >&2
  exit 1
}

while (($# > 0)); do
  case "$1" in
    --output)
      (($# >= 2)) || fail "--output requires a path"
      output=$2
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

[[ -n "$output" ]] || fail "--output must not be empty"
if [[ "$output" == "build-rpi5/kernel_2712.img" ]]; then
  fail "refusing to overwrite the default RPi5 artifact; choose an HH-specific output"
fi

cargo +nightly build \
  -Z build-std=core,alloc,compiler_builtins,panic_abort \
  -Z build-std-features=compiler-builtins-mem \
  -Z json-target-spec \
  --target targets/aarch64-rpi5-stage2-highhalf-none.json \
  --profile "$profile" \
  --no-default-features \
  --features rpi5-highhalf \
  -p yarm \
  --bin kernel_boot

elf="$target_dir/$target_name/$profile/kernel_boot"
[[ -f "$elf" ]] || fail "expected HH ELF was not produced: $elf"

objcopy_bin=${LLVM_OBJCOPY:-}
if [[ -z "$objcopy_bin" ]]; then
  objcopy_bin=$(command -v llvm-objcopy || true)
fi
[[ -n "$objcopy_bin" ]] || fail "llvm-objcopy is required to produce the raw HH image"

mkdir -p -- "$(dirname -- "$output")"
"$objcopy_bin" -O binary "$elf" "$output"

readelf_bin=${READELF:-}
if [[ -z "$readelf_bin" ]]; then
  readelf_bin=$(command -v readelf || true)
fi
[[ -n "$readelf_bin" ]] || fail "readelf is required to validate the HH ELF"
readelf_report="$output.readelf.txt"
"$readelf_bin" -W -h -l -s "$elf" > "$readelf_report"

grep -Eq 'Entry point address:[[:space:]]+0x80000$' "$readelf_report" ||
  fail "HH ELF entry is not the low trampoline at 0x80000"
grep -Eq 'LOAD[[:space:]].*0x0*80000[[:space:]]+0x0*80000' "$readelf_report" ||
  fail "HH ELF does not contain the expected low boot LOAD"
grep -Eq 'LOAD[[:space:]].*0xffffff8[0-9a-f]+[[:space:]]+0x0*[0-9a-f]+' "$readelf_report" ||
  fail "HH ELF does not contain a high-VMA/low-LMA kernel LOAD"
grep -Eq 'ffffff8000000000[[:space:]].*__kernel_va_offset$' "$readelf_report" ||
  fail "HH ELF kernel VA offset symbol is missing or incorrect"

ttbr0=$("$readelf_bin" -W -s "$elf" | awk '$NF == "__hh_ttbr0_root" { print $2; exit }')
ttbr1=$("$readelf_bin" -W -s "$elf" | awk '$NF == "__hh_ttbr1_root" { print $2; exit }')
[[ -n "$ttbr0" && -n "$ttbr1" ]] || fail "HH ELF TTBR root symbols are missing"
[[ "$ttbr0" != "$ttbr1" ]] || fail "HH ELF TTBR root symbols are not distinct"

echo "[ok] RPi5 HH ELF: $elf"
echo "[ok] RPi5 HH raw image: $output"
echo "[ok] RPi5 HH readelf proof: $readelf_report"
echo "[ok] TTBR0 root: 0x$ttbr0"
echo "[ok] TTBR1 root: 0x$ttbr1"
