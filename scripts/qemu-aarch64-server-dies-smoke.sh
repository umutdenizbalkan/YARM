#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 200D-2B1C — aarch64 ServerDies exact-commit runner.
#
# Port-specific wiring only; the proof body is `scripts/lib/serverdies-runner-common.sh`.
# Stage 200D-2B1C prepares this runner and does NOT execute it: no live cell is claimed by
# the stage that adds it.
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

# One boot, fresh log, no reboot/shutdown so a triple fault cannot masquerade as a clean exit.
serverdies_boot_once() {
  local log="$1"
  timeout "$TIMEOUT_SECS" qemu-system-aarch64 -M virt -cpu cortex-a72 -m 512 -nographic -no-reboot -no-shutdown \
    -kernel "$KELF" -append "${SELECTOR}=server-dies" >"$log" 2>&1 || true
  grep -q "YARM_BOOT_OK" "$log"
}

serverdies_main
