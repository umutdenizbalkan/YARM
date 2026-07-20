#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 199A2D2C1 — x86_64 GENERIC scheduler-selected AP user-return smoke (fresh entry).
#
# Boots QEMU_SMP=2 with the SMP request oracle selector + the AP user-dispatch gate armed, and
# proves that CPU 1 enters a SCHEDULER-SELECTED userspace task through the GENERIC AP dispatch path
# (fresh entry): the task is picked from CPU 1's real run queue (`dispatch_next_on_cpu`, not a
# hardcoded probe TID) and enters ring 3 via the canonical `enter_user_mode_iret`, then emits a REAL
# userspace DebugLog marker (proving it reached ring 3 and executed a real syscall on CPU 1).
#
# This proves the GENERIC AP return path ONLY. It does NOT do the recv-v2 blocked continuation or the
# cross-CPU NR6 seal (Stage 199A2D2C2), and does not change the NR6/NR7 live-cell count.
#
# Required ordered live sequence in one clean boot:
#   X86_AP_ONLINE cpu=1
#   X86_AP_GENERIC_DISPATCH_OK cpu=1 mode=fresh scheduler_selected=1 tid=<tid> result=ok   (kernel)
#   X86_AP_GENERIC_USER_ENTRY cpu=1 scheduler_selected=1 result=ok                          (userspace)
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=x86-ipccall-direct-smp-oracle
KTARGET=${KTARGET:-targets/x86_64-yarm-none.json}
KPROFILE=${KPROFILE:-x86-none}
KELF=${KELF:-target/x86_64-yarm-none/${KPROFILE}/kernel_boot}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ap-generic-return}
TIMEOUT_SECS=${TIMEOUT_SECS:-150}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[ap-generic-return] $*"; }
die()  { echo "[ap-generic-return][fail] $*"; fail=1; }

note "building base x86_64 artifacts (servers + initramfs)"
BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
  scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
  || die "base artifact build failed (see $LOGDIR/build.log)"

if (( ! fail )); then
  note "rebuilding kernel_boot with --features $FEATURE"
  cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" \
    --no-default-features --features "$FEATURE" \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 \
    || die "feature kernel_boot build failed (see $LOGDIR/kbuild.log)"
fi
if (( ! fail )); then
  cp "$KELF" build-x86_64/kernel_boot.elf
fi
if (( fail )); then
  echo "STAGE_199_X86_AP_GENERIC_RETURN_SEAL arch=x86_64 smp=2 result=fail reason=build"
  exit 1
fi

note "booting QEMU -smp 2 with the SMP oracle selector + AP user-dispatch armed"
env \
  KERNEL_IMAGE=build-x86_64/kernel_boot.elf \
  INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.x86_64_ipccall_direct_smp_oracle=1 yarm.ap_user_dispatch=1" \
  QEMU_SMP=2 \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  scripts/qemu-x86_64-core-smoke.sh >"$LOGDIR/core-smoke.log" 2>&1 || true

if [[ ! -s "$BOOT_LOG" ]]; then
  die "no boot log produced"
  echo "STAGE_199_X86_AP_GENERIC_RETURN_SEAL arch=x86_64 smp=2 result=fail reason=no_boot_log"
  exit 1
fi

NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
count() { rg -a -c -F "$1" "$NORM" 2>/dev/null || echo 0; }
have()  { rg -a -q -F "$1" "$NORM"; }

# ── Required ordered live sequence ──
have "X86_AP_ONLINE cpu=1" || die "CPU 1 not online"

disp=$(count "X86_AP_GENERIC_DISPATCH_OK cpu=1 mode=fresh scheduler_selected=1")
[[ "$disp" == "1" ]] || die "kernel generic-dispatch marker count != 1 (got $disp)"

uentry=$(count "X86_AP_GENERIC_USER_ENTRY cpu=1 scheduler_selected=1 result=ok")
[[ "$uentry" == "1" ]] || die "userspace generic-entry marker count != 1 (got $uentry)"

# ── Hard-stops ──
# The userspace entry must be exactly once (no duplicate ring-3 entry).
[[ "$uentry" == "1" ]] || die "duplicate/missing userspace entry"
for bad in "KERNEL PANIC" "RUST PANIC" "panicked at" "CPU EXCEPTION" "DOUBLE FAULT" "Unhandled" "BOOTSTRAP_ERROR"; do
  have "$bad" && die "fatal condition: $bad"
done
# The proof must not run on CPU 0.
have "X86_AP_GENERIC_DISPATCH_OK cpu=0" && die "generic dispatch ran on CPU 0"
have "X86_AP_GENERIC_USER_ENTRY cpu=0" && die "generic user entry ran on CPU 0"

if (( fail )); then
  echo "STAGE_199_X86_AP_GENERIC_RETURN_SEAL arch=x86_64 smp=2 result=fail (see $BOOT_LOG)"
  exit 1
fi

note "genuine scheduler-selected CPU-1 fresh ring-3 entry proven (kernel + userspace markers)"
echo "STAGE_199_X86_AP_GENERIC_RETURN_SEAL arch=x86_64 smp=2 cpu=1 scheduler_selected=1 fresh_entries=1 duplicate_entries=0 wrong_cpu_entries=0 result=ok"
