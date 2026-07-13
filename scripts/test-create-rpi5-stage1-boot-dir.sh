#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
generator="$repo_root/scripts/create-rpi5-stage1-boot-dir.sh"
hh_builder="$repo_root/scripts/build-rpi5-highhalf-artifact.sh"
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

hh_marker_fixture="$tmp_dir/hh-marker-fixture.img"
for marker in \
  RPI5_HH_LOW_ENTRY \
  RPI5_HH_PLAN_DONE \
  RPI5_HH_ENABLE_DONE \
  RPI5_HH_JUMP_HIGH \
  RPI5_HH_HIGH_ENTRY_OK \
  RPI5_HH_BSS_CLEAR_BEGIN \
  RPI5_HH_BSS_CLEAR_DONE \
  RPI5_HH_RUST_ENTRY \
  RPI5_HH_RUST_AFTER_ENTRY \
  RPI5_HH_READ_PC_BEGIN \
  RPI5_HH_READ_PC_CAPTURED \
  RPI5_HH_READ_PC_PRINT_BEGIN \
  RPI5_HH_HEX_BEGIN \
  RPI5_HH_HEX_DIGIT_BEGIN \
  RPI5_HH_HEX_DIGIT_DONE \
  RPI5_HH_HEX_DONE \
  RPI5_HH_HEX_FAILED \
  'RPI5_HH3_FAULT_BOUNDARY reason=hex_output' \
  RPI5_HH_READ_PC_DONE \
  RPI5_HH_READ_PC_FAILED \
  'RPI5_HH3_FAULT_BOUNDARY reason=pc_read_or_print' \
  RPI5_HH_READ_SP_BEGIN \
  RPI5_HH_READ_SP_CAPTURED \
  RPI5_HH_SP_HEX_BEGIN \
  RPI5_HH_SP_HEX_DIGIT_BEGIN \
  RPI5_HH_SP_HEX_DIGIT_DONE \
  RPI5_HH_SP_HEX_DONE \
  RPI5_HH_SP_HEX_FAILED \
  'RPI5_HH3_FAULT_BOUNDARY reason=sp_hex_output' \
  RPI5_HH_READ_SP_DONE \
  RPI5_HH_READ_VBAR_BEGIN \
  RPI5_HH_READ_VBAR_CAPTURED \
  RPI5_HH_VBAR_HEX_BEGIN \
  RPI5_HH_VBAR_HEX_DIGIT_BEGIN \
  RPI5_HH_VBAR_HEX_DIGIT_DONE \
  RPI5_HH_VBAR_HEX_DONE \
  RPI5_HH_VBAR_HEX_FAILED \
  'RPI5_HH3_FAULT_BOUNDARY reason=vbar_hex_output' \
  RPI5_HH_READ_VBAR_DONE \
  RPI5_HH_READ_TTBR_BEGIN \
  RPI5_HH_READ_TTBR_DONE \
  RPI5_HH_PRINT_REGS_BEGIN \
  RPI5_HH_PRINT_REGS_FIRST_BEGIN \
  RPI5_HH_PRINT_REGS_FIRST_HEX_BEGIN \
  RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_BEGIN \
  RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_DONE \
  RPI5_HH_PRINT_REGS_FIRST_HEX_DONE \
  RPI5_HH_PRINT_REGS_FIRST_HEX_FAILED \
  'RPI5_HH3_FAULT_BOUNDARY reason=print_regs_first_hex_output' \
  RPI5_HH_PRINT_REGS_FIRST_DONE \
  RPI5_HH_PRINT_REGS_SP_BEGIN \
  RPI5_HH_PRINT_REGS_SP_HEX_BEGIN \
  RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_BEGIN \
  RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_DONE \
  RPI5_HH_PRINT_REGS_SP_HEX_DONE \
  RPI5_HH_PRINT_REGS_SP_HEX_FAILED \
  'RPI5_HH3_FAULT_BOUNDARY reason=print_regs_sp_hex_output' \
  RPI5_HH_PRINT_REGS_SP_DONE \
  RPI5_HH_PRINT_REGS_BYPASS_FOR_HH3_PROOF \
  RPI5_HH_PRINT_REGS_DONE \
  RPI5_HH3_PRECHECK_DONE \
  RPI5_HH_REGISTERS_OK \
  RPI5_HH_RUST_UART_OK \
  RPI5_HH3_DONE \
  RPI5_HH4_BEGIN \
  RPI5_HH4_DTB_PTR_BEGIN \
  RPI5_HH4_DTB_PTR_OK \
  RPI5_HH4_DTB_VIRT_OK \
  'RPI5_HH4_DTB_PTR_FAILED reason=' \
  RPI5_HH4_UART_STILL_OK \
  'RPI5_HH4_FAULT_BOUNDARY reason=' \
  RPI5_HH4_DONE \
  RPI5_HH5_BEGIN \
  RPI5_HH5_DTB_CHOSEN_BEGIN \
  RPI5_HH5_FDT_HEADER_BEGIN \
  RPI5_HH5_FDT_HEADER_OK \
  RPI5_HH5_FDT_BLOCKS_BEGIN \
  RPI5_HH5_FDT_BLOCKS_OK \
  RPI5_HH5_FDT_CHOSEN_SCAN_BEGIN \
  RPI5_HH5_FDT_CHOSEN_FOUND \
  RPI5_HH5_FDT_CHOSEN_SCAN_DONE \
  RPI5_HH5_FDT_INITRD_PROPS_BEGIN \
  RPI5_HH5_FDT_INITRD_PROPS_DONE \
  'RPI5_HH5_DTB_WALK_FAILED reason=' \
  RPI5_HH5_DTB_CHOSEN_OK \
  RPI5_HH5_INITRD_BEGIN \
  'RPI5_HH5_INITRD_RANGE phys_start=0x' \
  'RPI5_HH5_INITRD_VIRT virt_start=0x' \
  RPI5_HH5_INITRD_OK \
  'RPI5_HH5_INITRD_FAILED reason=' \
  RPI5_HH5_ALLOC_BRIDGE_BEGIN \
  'RPI5_HH5_ALLOC_BRIDGE_RANGE phys=0x' \
  RPI5_HH5_ALLOC_BRIDGE_OK \
  'RPI5_HH5_ALLOC_BRIDGE_FAILED reason=' \
  RPI5_HH5_HANDOFF_BEGIN \
  'RPI5_HH5_HANDOFF_OK virt=0x' \
  RPI5_HH5_NORMAL_BOOT_AUDIT_BEGIN \
  RPI5_HH5_NORMAL_BOOT_AUDIT_DONE \
  'RPI5_HH5_BOOT_INPUT_OK virt=0x' \
  RPI5_HH5_ALLOC_ADAPTER_BEGIN \
  RPI5_HH5_ALLOC_ADAPTER_LAYOUT_BEGIN \
  RPI5_HH5_ALLOC_ADAPTER_LAYOUT_OK \
  RPI5_HH5_ALLOC_ADAPTER_STORAGE_BEGIN \
  'RPI5_HH5_ALLOC_ADAPTER_STORAGE_OK virt=0x' \
  RPI5_HH5_ALLOC_ADAPTER_ZERO_BEGIN \
  RPI5_HH5_ALLOC_ADAPTER_ZERO_DONE \
  RPI5_HH5_ALLOC_ADAPTER_INIT_BEGIN \
  RPI5_HH5_ALLOC_ADAPTER_INIT_RANGE_BEGIN \
  'RPI5_HH5_ALLOC_ADAPTER_INIT_RANGE_OK pages=0x' \
  'RPI5_HH5_ALLOC_ADAPTER_INIT_CAPACITY_OK capacity=0x' \
  RPI5_HH5_ALLOC_ADAPTER_INIT_CALL_BEGIN \
  RPI5_HH5_ALLOC_ADAPTER_INIT_CALL_DONE \
  RPI5_HH5_ALLOC_ADAPTER_INIT_DONE \
  RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_BEGIN \
  RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_CALL_BEGIN \
  RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_CALL_DONE \
  RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_VALIDATE_BEGIN \
  RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_VALIDATE_OK \
  'RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_OK frame=0x' \
  RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_BEGIN \
  RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_CALL_BEGIN \
  RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_CALL_DONE \
  RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_OK \
  'RPI5_HH5_ALLOC_ADAPTER_RANGE usable_start=0x' \
  RPI5_HH5_ALLOC_ADAPTER_OK \
  'RPI5_HH5_ALLOC_ADAPTER_FAILED reason=' \
  RPI5_HH5_ENTER_KERNEL_BEGIN \
  RPI5_KERNEL_ENTRY_BEGIN \
  RPI5_KERNEL_DTB_PARSE_BEGIN \
  RPI5_KERNEL_DTB_PARSE_OK \
  RPI5_KERNEL_INITRD_OK \
  RPI5_KERNEL_PMEM_BEGIN \
  'RPI5_KERNEL_PMEM_OK free_pages=0x' \
  RPI5_KERNEL_BOOTINFO_OK \
  RPI5_BOOT4_GLOBAL_HEAP_AUDIT_BEGIN \
  RPI5_BOOT4_GLOBAL_HEAP_AUDIT_DONE \
  RPI5_KERNEL_GLOBAL_HEAP_BEGIN \
  'RPI5_KERNEL_GLOBAL_HEAP_RANGE virt=0x' \
  RPI5_KERNEL_GLOBAL_HEAP_OK \
  'RPI5_KERNEL_GLOBAL_HEAP_FAILED reason=' \
  RPI5_KERNEL_VM_BEGIN \
  RPI5_KERNEL_VM_LAYOUT_OK \
  RPI5_KERNEL_VM_OK \
  'RPI5_KERNEL_VM_FAILED reason=' \
  RPI5_BOOT4_PHYSMAP_AUDIT_BEGIN \
  RPI5_BOOT4_PHYSMAP_AUDIT_DONE \
  RPI5_KERNEL_GLOBAL_ALLOCATOR_BEGIN \
  RPI5_KERNEL_GLOBAL_ALLOCATOR_PT_STORAGE_BEGIN \
  'RPI5_KERNEL_GLOBAL_ALLOCATOR_PT_STORAGE_OK virt=0x' \
  RPI5_KERNEL_GLOBAL_ALLOCATOR_PT_ZERO_BEGIN \
  RPI5_KERNEL_GLOBAL_ALLOCATOR_PT_ZERO_DONE \
  RPI5_KERNEL_GLOBAL_ALLOCATOR_PT_INIT_BEGIN \
  RPI5_KERNEL_GLOBAL_ALLOCATOR_PT_INIT_DONE \
  'RPI5_KERNEL_GLOBAL_ALLOCATOR_HEAP_RANGE phys=0x' \
  RPI5_KERNEL_PHYSMAP_SWITCH_BEGIN \
  'RPI5_KERNEL_PHYSMAP_SWITCH_OK offset=0x' \
  RPI5_KERNEL_GLOBAL_ALLOCATOR_PHYSMAP_OK \
  RPI5_KERNEL_GLOBAL_ALLOCATOR_PROBE_BEGIN \
  RPI5_KERNEL_GLOBAL_ALLOCATOR_PROBE_ALLOC_BEGIN \
  'RPI5_KERNEL_GLOBAL_ALLOCATOR_PROBE_ALLOC_OK ptr=0x' \
  RPI5_KERNEL_GLOBAL_ALLOCATOR_PROBE_SENTINEL_OK \
  'RPI5_KERNEL_GLOBAL_ALLOCATOR_PROBE_OK ptr=0x' \
  RPI5_KERNEL_GLOBAL_ALLOCATOR_HIGHMAP_OK \
  RPI5_BOOT5_KERNELSTATE_AUDIT_BEGIN \
  RPI5_BOOT5_KERNELSTATE_AUDIT_STORAGE \
  RPI5_BOOT5_KERNELSTATE_AUDIT_CPU0 \
  RPI5_BOOT5_KERNELSTATE_AUDIT_SCHEDULER \
  RPI5_BOOT5_KERNELSTATE_AUDIT_TASK \
  RPI5_BOOT5_KERNELSTATE_AUDIT_IPC \
  RPI5_BOOT5_KERNELSTATE_AUDIT_CAP \
  RPI5_BOOT5_KERNELSTATE_AUDIT_VM \
  RPI5_BOOT5_KERNELSTATE_AUDIT_DONE \
  RPI5_BOOT5_HANDOFF_BRIDGE_BEGIN \
  RPI5_BOOT5_HANDOFF_DTB_OK \
  RPI5_BOOT5_HANDOFF_INITRD_OK \
  RPI5_BOOT5_HANDOFF_MEMORY_OK \
  RPI5_BOOT5_HANDOFF_RESERVED_OK \
  RPI5_BOOT5_HANDOFF_HEAP_OK \
  RPI5_BOOT5_HANDOFF_PHYSMAP_OK \
  RPI5_BOOT5_HANDOFF_BRIDGE_OK \
  'RPI5_BOOT5_HANDOFF_FAILED reason=' \
  'RPI5_KERNEL_GLOBAL_ALLOCATOR_FAILED reason=' \
  'RPI5_BOOT4_FAULT_BOUNDARY reason=' \
  'RPI5_HH5_FAULT_BOUNDARY reason=' \
  'RPI5_HH5_DEFERRED reason=' \
  'RPI5_HH5_DONE status=deferred' \
  RPI5_HH5_ENTER_USER_ATTEMPT; do
  printf '%s\n' "$marker" >> "$hh_marker_fixture"
done
"$hh_builder" --validate-image "$hh_marker_fixture" >/dev/null
sed '/RPI5_HH3_DONE/d' "$hh_marker_fixture" > "$tmp_dir/hh-marker-missing.img"
if "$hh_builder" --validate-image "$tmp_dir/hh-marker-missing.img" \
  >"$tmp_dir/hh-marker-missing.out" 2>&1; then
  fail "HH marker validator accepted an incomplete raw image"
fi
assert_contains "$tmp_dir/hh-marker-missing.out" \
  'HH raw image is missing required marker: RPI5_HH3_DONE'

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
assert_not_contains "$default_boot/config.txt" 'initramfs '
assert_not_contains "$default_boot/README-RPI5-STAGE1.txt" 'Explicit high-half diagnostic mode'
[[ ! -e "$default_boot/initramfs-stage2a.cpio" ]] || fail "default boot unexpectedly staged initrd"
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

initrd="$tmp_dir/fake-initramfs.cpio"
printf '070701fake-stage2a-cpio\n' > "$initrd"
initrd_boot="$tmp_dir/initrd-boot"
"$generator" \
  --kernel-input "$kernel" \
  --initrd-input "$initrd" \
  --boot-dir "$initrd_boot" \
  --phase kernel >/dev/null
assert_file "$initrd_boot/initramfs-stage2a.cpio"
cmp "$initrd" "$initrd_boot/initramfs-stage2a.cpio" >/dev/null || fail "staged initrd differs from input"
assert_contains "$initrd_boot/config.txt" 'initramfs initramfs-stage2a.cpio followkernel'
assert_contains "$initrd_boot/README-RPI5-STAGE1.txt" '--initrd-input build-aarch64/initramfs-core.cpio'
assert_contains "$initrd_boot/README-RPI5-STAGE1.txt" 'RPI5_STAGE2A_DONE'
assert_contains "$initrd_boot/README-RPI5-STAGE1.txt" 'It does not unpack the archive or spawn userspace.'

hh_kernel="$tmp_dir/fake-kernel_2712_hh.img"
printf 'fake-rpi5-highhalf-kernel\n' > "$hh_kernel"
hh_boot="$tmp_dir/highhalf-boot"
"$generator" \
  --kernel-input "$hh_kernel" \
  --boot-dir "$hh_boot" \
  --phase kernel \
  --highhalf >/dev/null
cmp "$hh_kernel" "$hh_boot/kernel_2712.img" >/dev/null || fail "HH kernel differs from explicit input"
assert_contains "$hh_boot/README-RPI5-STAGE1.txt" 'Explicit high-half diagnostic mode'
assert_contains "$hh_boot/README-RPI5-STAGE1.txt" 'separately built kernel_2712_hh.img artifact'
assert_contains "$hh_boot/README-RPI5-STAGE1.txt" 'RPI5_HH_RUST_ENTRY'
assert_contains "$hh_boot/README-RPI5-STAGE1.txt" 'RPI5_HH3_DONE'
assert_contains "$hh_boot/README-RPI5-STAGE1.txt" 'RPI5_HH4_DONE'
assert_contains "$hh_boot/README-RPI5-STAGE1.txt" 'RPI5_HH5_DTB_CHOSEN_BEGIN'
assert_contains "$hh_boot/README-RPI5-STAGE1.txt" 'RPI5_HH5_HANDOFF_OK virt=0x'
assert_contains "$hh_boot/README-RPI5-STAGE1.txt" 'RPI5_HH5_ALLOC_ADAPTER_OK'
assert_contains "$hh_boot/README-RPI5-STAGE1.txt" 'RPI5_KERNEL_BOOTINFO_OK'
assert_contains "$hh_boot/README-RPI5-STAGE1.txt" 'RPI5_KERNEL_GLOBAL_HEAP_OK'
assert_contains "$hh_boot/README-RPI5-STAGE1.txt" 'RPI5_KERNEL_VM_OK'
assert_contains "$hh_boot/README-RPI5-STAGE1.txt" 'RPI5_KERNEL_GLOBAL_ALLOCATOR_HIGHMAP_OK'
assert_contains "$hh_boot/README-RPI5-STAGE1.txt" \
  'RPI5_HH5_DEFERRED reason=kernel_state_requires_scheduler_init'
assert_contains "$hh_boot/README-RPI5-STAGE1.txt" 'high-alias-only initrd/allocator/handoff bridge'
[[ ! -e "$hh_boot/initramfs-stage2a.cpio" ]] || fail "HH mode unexpectedly required an initrd"

hh_initrd_boot="$tmp_dir/highhalf-initrd-boot"
"$generator" \
  --kernel-input "$hh_kernel" \
  --initrd-input "$initrd" \
  --boot-dir "$hh_initrd_boot" \
  --phase kernel \
  --highhalf >/dev/null
cmp "$initrd" "$hh_initrd_boot/initramfs-stage2a.cpio" >/dev/null ||
  fail "HH mode did not preserve optional initrd staging"
assert_contains "$hh_initrd_boot/config.txt" 'initramfs initramfs-stage2a.cpio followkernel'

if "$generator" --highhalf --boot-dir "$tmp_dir/hh-missing-kernel" >"$tmp_dir/hh-missing.out" 2>&1; then
  fail "HH mode without explicit kernel input unexpectedly succeeded"
fi
assert_contains "$tmp_dir/hh-missing.out" '--kernel-input is required'

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
