#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 200D-2B1C — riscv64 ServerDies exact-commit runner.
# QEMU-BASELINE1 §4 — staging and boot corrections so it actually runs the transaction.
#
# Port-specific wiring only; the proof body is `scripts/lib/serverdies-runner-common.sh`.
# Stage 200D-2B1C prepared this runner. QEMU-BASELINE1 is the first stage to run it, and
# corrected the invocation below. Nothing about the GRADING moved: the marker chain, its
# ordering, the forbidden set, the identity checks, the wake/winner counts and the two-sided
# binary audit all live in the shared body and are untouched.
#
# What was wrong with the prepared invocation (the same defect the x86_64 runner had):
#
#   -initrd was missing        The ServerDies oracle IS a userspace task inside init_server,
#                              which ships in `initramfs-core.cpio`. The prepared boot reached
#                              `BOOT_FATAL_INITRAMFS_MISSING` and panicked before any userspace
#                              ran, so the scenario never started. This is the fix that mattered.
#   raw target/ ELF            The booted artifact is the raw `build-riscv64/yarm-riscv64.bin`
#                              the artifact script publishes, loaded behind OpenSBI
#                              (`-bios default`), exactly as the core smoke boots it.
#   console args missing       `console=ttyS0 rdinit=/init` is the core profile's command line;
#                              without `rdinit=/init` the kernel does not start init_server.
#
# `-no-reboot -no-shutdown` and the single-boot discipline are kept exactly as prepared.
set -uo pipefail
cd "$(dirname "$0")/.."
source "$(dirname "$0")/lib/serverdies-runner-common.sh"

ARCH_TAG=riscv64
KTARGET=riscv64gc-unknown-none-elf
KPROFILE=release
KELF=target/riscv64gc-unknown-none-elf/release/kernel_boot
FEATURE=riscv64-ipc-reply-timeout-oracle
SELECTOR=yarm.riscv_ipc_reply_timeout_oracle
TIMEOUT_SECS=${TIMEOUT_SECS:-180}

# The staged, bootable artifacts (what QEMU actually consumes).
BOOT_KERNEL=${BOOT_KERNEL:-build-riscv64/yarm-riscv64.bin}
BOOT_INITRD=${BOOT_INITRD:-build-riscv64/initramfs-core.cpio}
BASE_CMDLINE=${BASE_CMDLINE:-"console=ttyS0 rdinit=/init"}
QEMU_BIN=${QEMU_BIN:-qemu-system-riscv64}

serverdies_stage_boot_artifacts() {
  serverdies_stage_raw_image riscv64 "$BOOT_KERNEL" "$BOOT_INITRD"
}

# One boot, fresh log, no reboot/shutdown so a triple fault cannot masquerade as a clean exit.
serverdies_boot_once() {
  local log="$1"
  timeout --foreground "${TIMEOUT_SECS}s" "$QEMU_BIN" \
    -machine virt -cpu rv64 -m 512M -smp 1 \
    -nographic -monitor none -serial stdio \
    -no-reboot -no-shutdown \
    -bios default -kernel "$BOOT_KERNEL" -initrd "$BOOT_INITRD" \
    -append "${BASE_CMDLINE} ${SELECTOR}=server-dies" >"$log" 2>&1 || true
  grep -q "YARM_BOOT_OK" "$log"
}

serverdies_main
