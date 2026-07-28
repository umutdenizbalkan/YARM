#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 200D-2B1C — x86_64 ServerDies exact-commit runner.
# Stage 200D-2B1D-x86 — machine/console/artifact corrections so it actually boots.
#
# Port-specific wiring only; the proof body is `scripts/lib/serverdies-runner-common.sh`.
# Stage 200D-2B1C prepared this runner. Stage 200D-2B1D-x86 is the first stage to run it, and
# corrected the invocation below. Nothing about the GRADING moved:
# the marker chain, its ordering, the forbidden set, the identity checks, the wake/winner
# counts and the two-sided binary audit all live in the shared body and are untouched.
#
# What was wrong with the prepared invocation, and why each fix is necessary rather than
# convenient:
#
#   -initrd was missing        The ServerDies oracle IS a userspace task inside init_server,
#                              which ships in `initramfs-core.cpio`. Direct-booting the bare
#                              kernel gives a system with no init at all, so the scenario
#                              could never run. This is the fix that mattered.
#   raw target/ ELF            The bootable image is the STAGED `build-x86_64/kernel_boot.elf`
#                              produced by `build-qemu-x86_64-artifacts.sh` (PVH note, matching
#                              initramfs). `target/.../kernel_boot` is the unstaged build
#                              product, and the two are not interchangeable.
#   console args missing       Without `-serial stdio -monitor none` and `console=ttyS0` the
#                              kernel's output never reaches the log, so every marker check
#                              would fail for a reason unrelated to the mechanism.
#   `rdinit=/init` missing     Without it the kernel does not start init_server.
#
# `-no-reboot -no-shutdown` and the single-boot discipline are kept exactly as prepared.
set -uo pipefail
cd "$(dirname "$0")/.."
source "$(dirname "$0")/lib/serverdies-runner-common.sh"

ARCH_TAG=x86_64
KTARGET=targets/x86_64-yarm-none.json
KPROFILE=x86-none
KELF=target/x86_64-yarm-none/x86-none/kernel_boot
FEATURE=x86-ipc-reply-timeout-oracle
SELECTOR=yarm.x86_64_ipc_reply_timeout_oracle
TIMEOUT_SECS=${TIMEOUT_SECS:-180}

# The staged, bootable artifacts (what QEMU actually consumes).
BOOT_KERNEL=${BOOT_KERNEL:-build-x86_64/kernel_boot.elf}
BOOT_INITRD=${BOOT_INITRD:-build-x86_64/initramfs-core.cpio}
BASE_CMDLINE=${BASE_CMDLINE:-"console=ttyS0 rdinit=/init"}

# Stage the bootable artifacts, then put the ORACLE-ON kernel in place.
#
# The artifact script drives one `BOOTSTRAP_FEATURE_ARGS` across the kernel AND every server
# crate, and `x86-ipc-reply-timeout-oracle` is a `yarm` feature that the server crates do not
# have — passing it to the whole run fails the server builds. So the servers and the
# initramfs are staged normally, and only the kernel image is replaced with the feature-on
# build that RUN_B already produced. The staged kernel is then re-verified to actually carry
# the oracle, so a silent fallback to the feature-off image cannot pass as a live cell.
serverdies_stage_boot_artifacts() {
  ./scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/stage.log" 2>&1 || return 1
  [[ -f "$KELF" ]] || { echo "[serverdies] missing feature-on kernel: $KELF"; return 1; }
  cp "$KELF" "$BOOT_KERNEL" || return 1
  [[ -f "$BOOT_INITRD" ]] || { echo "[serverdies] missing initramfs: $BOOT_INITRD"; return 1; }
  # The booted image MUST be the oracle-on one.
  strings -a "$BOOT_KERNEL" | grep -qF "IPC_REPLY_TIMEOUT_COLLECTOR_GATE" \
    || { echo "[serverdies] staged kernel is not oracle-on"; return 1; }
}

# One boot, fresh log, no reboot/shutdown so a triple fault cannot masquerade as a clean exit.
serverdies_boot_once() {
  local log="$1"
  timeout --foreground "${TIMEOUT_SECS}s" qemu-system-x86_64 \
    -machine q35 -cpu qemu64 -m 512M -smp 1 \
    -nographic -monitor none -serial stdio \
    -no-reboot -no-shutdown \
    -kernel "$BOOT_KERNEL" -initrd "$BOOT_INITRD" \
    -append "${BASE_CMDLINE} ${SELECTOR}=server-dies" >"$log" 2>&1 || true
  grep -q "YARM_BOOT_OK" "$log"
}

serverdies_main
