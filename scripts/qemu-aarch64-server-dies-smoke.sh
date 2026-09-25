#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 200D-2B1C — aarch64 ServerDies exact-commit runner.
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
#   raw target/ ELF            The booted artifact is the raw `build-aarch64/yarm-aarch64.bin`
#                              the artifact script publishes: QEMU passes the DTB (and with it
#                              the initrd location) to a raw image, not to an ELF — the prepared
#                              boot logged `YARM_AARCH64_DTB_STATUS missing_start_info_ptr`.
#   -m 512                     The core profile is 1024M, the RAM the staged kernel's pools are
#                              laid out for; the other machine settings are the core smoke's.
#
# `-no-reboot -no-shutdown` and the single-boot discipline are kept exactly as prepared. One
# CPU, as the x86_64 runner boots: the transaction is single-CPU, and the known AP cross-CPU
# reply/shootdown defect is out of this runner's scope.
set -uo pipefail
cd "$(dirname "$0")/.."
source "$(dirname "$0")/lib/serverdies-runner-common.sh"

ARCH_TAG=aarch64
KTARGET=targets/aarch64-yarm-none.json
KPROFILE=aarch64-none
KELF=target/aarch64-yarm-none/aarch64-none/kernel_boot
FEATURE=aarch64-ipc-reply-timeout-oracle
SELECTOR=yarm.aarch64_ipc_reply_timeout_oracle
TIMEOUT_SECS=${TIMEOUT_SECS:-180}

# The staged, bootable artifacts (what QEMU actually consumes).
BOOT_KERNEL=${BOOT_KERNEL:-build-aarch64/yarm-aarch64.bin}
BOOT_INITRD=${BOOT_INITRD:-build-aarch64/initramfs-core.cpio}
QEMU_BIN=${QEMU_BIN:-qemu-system-aarch64}

serverdies_stage_boot_artifacts() {
  serverdies_stage_raw_image aarch64 "$BOOT_KERNEL" "$BOOT_INITRD"
}

# One boot, fresh log, no reboot/shutdown so a triple fault cannot masquerade as a clean exit.
serverdies_boot_once() {
  local log="$1"
  timeout --foreground "${TIMEOUT_SECS}s" "$QEMU_BIN" \
    -machine virt -cpu cortex-a72 -m 1024M -smp 1 \
    -nographic -monitor none -serial stdio \
    -no-reboot -no-shutdown \
    -kernel "$BOOT_KERNEL" -initrd "$BOOT_INITRD" \
    -append "${SELECTOR}=server-dies" >"$log" 2>&1 || true
  grep -q "YARM_BOOT_OK" "$log"
}

serverdies_main
