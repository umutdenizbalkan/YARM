#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

output=build-rpi5/kernel_2712_hh.img
validate_image=
target_dir=${CARGO_TARGET_DIR:-target}
target_name=aarch64-rpi5-stage2-highhalf-none
profile=aarch64-none

usage() {
  cat <<USAGE
Usage: $0 [--output PATH]
       $0 --validate-image PATH

Build the explicit RPi5 HH-3 diagnostic image. The output defaults to:
  build-rpi5/kernel_2712_hh.img

This script never replaces build-rpi5/kernel_2712.img.
The validation-only mode checks an existing raw image without rebuilding it.
USAGE
}

fail() {
  echo "[error] $*" >&2
  exit 1
}

required_markers=(
  RPI5_HH_LOW_ENTRY
  RPI5_HH_PLAN_DONE
  RPI5_HH_ENABLE_DONE
  RPI5_HH_JUMP_HIGH
  RPI5_HH_HIGH_ENTRY_OK
  RPI5_HH_RUST_ENTRY
  RPI5_HH_RUST_AFTER_ENTRY
  RPI5_HH_READ_PC_BEGIN
  RPI5_HH_READ_PC_CAPTURED
  RPI5_HH_READ_PC_PRINT_BEGIN
  RPI5_HH_HEX_BEGIN
  RPI5_HH_HEX_DIGIT_BEGIN
  RPI5_HH_HEX_DIGIT_DONE
  RPI5_HH_HEX_DONE
  RPI5_HH_HEX_FAILED
  'RPI5_HH3_FAULT_BOUNDARY reason=hex_output'
  RPI5_HH_READ_PC_DONE
  RPI5_HH_READ_PC_FAILED
  'RPI5_HH3_FAULT_BOUNDARY reason=pc_read_or_print'
  RPI5_HH_READ_SP_BEGIN
  RPI5_HH_READ_SP_CAPTURED
  RPI5_HH_SP_HEX_BEGIN
  RPI5_HH_SP_HEX_DIGIT_BEGIN
  RPI5_HH_SP_HEX_DIGIT_DONE
  RPI5_HH_SP_HEX_DONE
  RPI5_HH_SP_HEX_FAILED
  'RPI5_HH3_FAULT_BOUNDARY reason=sp_hex_output'
  RPI5_HH_READ_SP_DONE
  RPI5_HH_READ_VBAR_BEGIN
  RPI5_HH_READ_VBAR_CAPTURED
  RPI5_HH_VBAR_HEX_BEGIN
  RPI5_HH_VBAR_HEX_DIGIT_BEGIN
  RPI5_HH_VBAR_HEX_DIGIT_DONE
  RPI5_HH_VBAR_HEX_DONE
  RPI5_HH_VBAR_HEX_FAILED
  'RPI5_HH3_FAULT_BOUNDARY reason=vbar_hex_output'
  RPI5_HH_READ_VBAR_DONE
  RPI5_HH_READ_TTBR_BEGIN
  RPI5_HH_READ_TTBR_DONE
  RPI5_HH_PRINT_REGS_BEGIN
  RPI5_HH_PRINT_REGS_FIRST_BEGIN
  RPI5_HH_PRINT_REGS_FIRST_HEX_BEGIN
  RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_BEGIN
  RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_DONE
  RPI5_HH_PRINT_REGS_FIRST_HEX_DONE
  RPI5_HH_PRINT_REGS_FIRST_HEX_FAILED
  'RPI5_HH3_FAULT_BOUNDARY reason=print_regs_first_hex_output'
  RPI5_HH_PRINT_REGS_FIRST_DONE
  RPI5_HH_PRINT_REGS_SP_BEGIN
  RPI5_HH_PRINT_REGS_SP_HEX_BEGIN
  RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_BEGIN
  RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_DONE
  RPI5_HH_PRINT_REGS_SP_HEX_DONE
  RPI5_HH_PRINT_REGS_SP_HEX_FAILED
  'RPI5_HH3_FAULT_BOUNDARY reason=print_regs_sp_hex_output'
  RPI5_HH_PRINT_REGS_SP_DONE
  RPI5_HH_PRINT_REGS_BYPASS_FOR_HH3_PROOF
  RPI5_HH_PRINT_REGS_DONE
  RPI5_HH3_PRECHECK_DONE
  RPI5_HH_REGISTERS_OK
  RPI5_HH_RUST_UART_OK
  RPI5_HH3_DONE
  RPI5_HH4_BEGIN
  RPI5_HH4_DTB_PTR_BEGIN
  RPI5_HH4_DTB_PTR_OK
  RPI5_HH4_DTB_VIRT_OK
  'RPI5_HH4_DTB_PTR_FAILED reason='
  RPI5_HH4_UART_STILL_OK
  'RPI5_HH4_FAULT_BOUNDARY reason='
  RPI5_HH4_DONE
  RPI5_HH5_BEGIN
  RPI5_HH5_DTB_CHOSEN_BEGIN
  RPI5_HH5_FDT_HEADER_BEGIN
  RPI5_HH5_FDT_HEADER_OK
  RPI5_HH5_FDT_BLOCKS_BEGIN
  RPI5_HH5_FDT_BLOCKS_OK
  RPI5_HH5_FDT_CHOSEN_SCAN_BEGIN
  RPI5_HH5_FDT_CHOSEN_FOUND
  RPI5_HH5_FDT_CHOSEN_SCAN_DONE
  RPI5_HH5_FDT_INITRD_PROPS_BEGIN
  RPI5_HH5_FDT_INITRD_PROPS_DONE
  'RPI5_HH5_DTB_WALK_FAILED reason='
  RPI5_HH5_DTB_CHOSEN_OK
  RPI5_HH5_INITRD_BEGIN
  'RPI5_HH5_INITRD_RANGE phys_start=0x'
  'RPI5_HH5_INITRD_VIRT virt_start=0x'
  RPI5_HH5_INITRD_OK
  'RPI5_HH5_INITRD_FAILED reason='
  RPI5_HH5_ALLOC_BRIDGE_BEGIN
  'RPI5_HH5_ALLOC_BRIDGE_RANGE phys=0x'
  RPI5_HH5_ALLOC_BRIDGE_OK
  'RPI5_HH5_ALLOC_BRIDGE_FAILED reason='
  RPI5_HH5_HANDOFF_BEGIN
  'RPI5_HH5_HANDOFF_OK virt=0x'
  RPI5_HH5_NORMAL_BOOT_AUDIT_BEGIN
  RPI5_HH5_NORMAL_BOOT_AUDIT_DONE
  'RPI5_HH5_BOOT_INPUT_OK virt=0x'
  RPI5_HH5_ALLOC_ADAPTER_BEGIN
  RPI5_HH5_ALLOC_ADAPTER_LAYOUT_BEGIN
  RPI5_HH5_ALLOC_ADAPTER_LAYOUT_OK
  RPI5_HH5_ALLOC_ADAPTER_STORAGE_BEGIN
  'RPI5_HH5_ALLOC_ADAPTER_STORAGE_OK virt=0x'
  RPI5_HH5_ALLOC_ADAPTER_ZERO_BEGIN
  RPI5_HH5_ALLOC_ADAPTER_ZERO_DONE
  RPI5_HH5_ALLOC_ADAPTER_INIT_BEGIN
  RPI5_HH5_ALLOC_ADAPTER_INIT_RANGE_BEGIN
  'RPI5_HH5_ALLOC_ADAPTER_INIT_RANGE_OK pages=0x'
  'RPI5_HH5_ALLOC_ADAPTER_INIT_CAPACITY_OK capacity=0x'
  RPI5_HH5_ALLOC_ADAPTER_INIT_CALL_BEGIN
  RPI5_HH5_ALLOC_ADAPTER_INIT_CALL_DONE
  RPI5_HH5_ALLOC_ADAPTER_INIT_DONE
  RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_BEGIN
  RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_CALL_BEGIN
  RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_CALL_DONE
  RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_VALIDATE_BEGIN
  RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_VALIDATE_OK
  'RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_OK frame=0x'
  RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_BEGIN
  RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_CALL_BEGIN
  RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_CALL_DONE
  RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_OK
  'RPI5_HH5_ALLOC_ADAPTER_RANGE usable_start=0x'
  RPI5_HH5_ALLOC_ADAPTER_OK
  'RPI5_HH5_ALLOC_ADAPTER_FAILED reason='
  RPI5_HH5_ENTER_KERNEL_BEGIN
  RPI5_KERNEL_ENTRY_BEGIN
  RPI5_KERNEL_DTB_PARSE_BEGIN
  RPI5_KERNEL_DTB_PARSE_OK
  RPI5_KERNEL_INITRD_OK
  RPI5_KERNEL_PMEM_BEGIN
  'RPI5_KERNEL_PMEM_OK free_pages=0x'
  RPI5_KERNEL_BOOTINFO_OK
  'RPI5_HH5_FAULT_BOUNDARY reason='
  'RPI5_HH5_DEFERRED reason='
  'RPI5_HH5_DONE status=deferred'
  RPI5_HH5_ENTER_USER_ATTEMPT
)

validate_raw_image_markers() {
  local image=$1
  local marker
  [[ -f "$image" ]] || fail "HH raw image is not a file: $image"
  for marker in "${required_markers[@]}"; do
    grep -aFq -- "$marker" "$image" ||
      fail "HH raw image is missing required marker: $marker"
  done
}

while (($# > 0)); do
  case "$1" in
    --output)
      (($# >= 2)) || fail "--output requires a path"
      output=$2
      shift 2
      ;;
    --validate-image)
      (($# >= 2)) || fail "--validate-image requires a path"
      validate_image=$2
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

if [[ -n "$validate_image" ]]; then
  validate_raw_image_markers "$validate_image"
  echo "[ok] RPi5 HH raw image contains all required transition markers: $validate_image"
  exit 0
fi

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

validate_raw_image_markers "$output"

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
empty_ttbr0=$("$readelf_bin" -W -s "$elf" | awk '$NF == "__hh_empty_ttbr0_root" { print $2; exit }')
[[ -n "$ttbr0" && -n "$ttbr1" && -n "$empty_ttbr0" ]] ||
  fail "HH ELF TTBR root symbols are missing"
[[ "$ttbr0" != "$ttbr1" ]] || fail "HH ELF TTBR root symbols are not distinct"
[[ "$empty_ttbr0" != "$ttbr0" && "$empty_ttbr0" != "$ttbr1" ]] ||
  fail "HH ELF empty TTBR0 root is not distinct"
[[ "$empty_ttbr0" =~ 000$ ]] || fail "HH ELF empty TTBR0 root is not page aligned"

echo "[ok] RPi5 HH ELF: $elf"
echo "[ok] RPi5 HH raw image: $output"
echo "[ok] RPi5 HH raw image contains all required transition markers"
echo "[ok] RPi5 HH readelf proof: $readelf_report"
echo "[ok] TTBR0 root: 0x$ttbr0"
echo "[ok] TTBR1 root: 0x$ttbr1"
echo "[ok] empty TTBR0 root: 0x$empty_ttbr0"
