#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
generator="$repo_root/scripts/create-rpi5-stage1-boot-dir.sh"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

fail() {
  echo "[test error] $*" >&2
  exit 1
}

assert_file() {
  [[ -f "$1" ]] || fail "missing generated file: $1"
}

assert_contains() {
  local file=$1
  local text=$2
  grep -Fq -- "$text" "$file" || fail "$file does not contain: $text"
}

assert_not_contains() {
  local file=$1
  local text=$2
  if grep -Fq -- "$text" "$file"; then
    fail "$file unexpectedly contains: $text"
  fi
}

kernel="$tmp_dir/fake-stage1.img"
printf 'fake-rpi5-stage1-kernel\n' > "$kernel"

default_boot="$tmp_dir/default-boot"
"$generator" --kernel-input "$kernel" --boot-dir "$default_boot" >/dev/null
for name in config.txt cmdline.txt kernel_2712.img README-RPI5-STAGE1.txt; do
  assert_file "$default_boot/$name"
done
cmp "$kernel" "$default_boot/kernel_2712.img" >/dev/null || fail "staged kernel differs from input"
[[ $(<"$default_boot/cmdline.txt") == "yarm.platform=auto yarm.boot_phase=uart yarm.max_cpus=1" ]] || fail "unexpected default cmdline"
for setting in 'kernel=kernel_2712.img' 'arm_64bit=1' 'enable_uart=1' 'uart_2ndstage=1'; do
  assert_contains "$default_boot/config.txt" "$setting"
done
assert_not_contains "$default_boot/config.txt" 'os_check=0'
assert_not_contains "$default_boot/config.txt" 'enable_rp1_uart=1'
for marker in RPI5_BOOT_00_ENTRY RPI5_BOOT_01_DTB_PTR RPI5_BOOT_02_UART_SELECTED RPI5_BOOT_03_UART_OK; do
  assert_contains "$default_boot/README-RPI5-STAGE1.txt" "$marker"
done
for result in 'no output' 'only RPI5_BOOT_00_ENTRY' 'only RPI5_BOOT_01_DTB_PTR' 'only RPI5_BOOT_02_UART_SELECTED' 'reaches RPI5_BOOT_03_UART_OK'; do
  assert_contains "$default_boot/README-RPI5-STAGE1.txt" "$result"
done

options_boot="$tmp_dir/options-boot"
"$generator" \
  --kernel-input "$kernel" \
  --boot-dir "$options_boot" \
  --phase dtb \
  --cmdline-extra 'console=ttyAMA10,115200 diagnostic=yes' \
  --os-check-off \
  --enable-rp1-uart >/dev/null
[[ $(<"$options_boot/cmdline.txt") == "yarm.platform=auto yarm.boot_phase=dtb yarm.max_cpus=1 console=ttyAMA10,115200 diagnostic=yes" ]] || fail "phase or cmdline extra was not generated correctly"
assert_contains "$options_boot/config.txt" 'os_check=0'
assert_contains "$options_boot/config.txt" 'enable_rp1_uart=1'

if "$generator" --kernel-input "$kernel" --boot-dir "$default_boot" >"$tmp_dir/no-force.out" 2>&1; then
  fail "existing generated files were overwritten without --force"
fi
assert_contains "$tmp_dir/no-force.out" 'use --force'
printf 'replacement-kernel\n' > "$kernel"
"$generator" --kernel-input "$kernel" --boot-dir "$default_boot" --phase entry --force >/dev/null
assert_contains "$default_boot/cmdline.txt" 'yarm.boot_phase=entry'
cmp "$kernel" "$default_boot/kernel_2712.img" >/dev/null || fail "--force did not replace the staged kernel"

if "$generator" --kernel-input "$tmp_dir/missing.img" --boot-dir "$tmp_dir/missing-boot" >"$tmp_dir/missing.out" 2>&1; then
  fail "missing kernel input unexpectedly succeeded"
fi
assert_contains "$tmp_dir/missing.out" '[error] kernel input is not a file:'
[[ ! -e "$tmp_dir/missing-boot" ]] || fail "boot directory was created after missing kernel validation"

if "$generator" --kernel-input "$kernel" --boot-dir "$tmp_dir/bad-phase" --phase userspace >"$tmp_dir/phase.out" 2>&1; then
  fail "invalid phase unexpectedly succeeded"
fi
assert_contains "$tmp_dir/phase.out" "invalid phase 'userspace'"

printf '[ok] Raspberry Pi 5 Stage 1 boot-directory generator tests passed\n'
