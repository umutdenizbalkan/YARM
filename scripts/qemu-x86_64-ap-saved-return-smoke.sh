#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 199A2D2C2A — x86_64 GENERIC AP saved-frame RESUME smoke (Yield continuation).
#
# Boots QEMU_SMP=2 with the SMP oracle + AP user-dispatch armed and proves the idle-dispatcher →
# saved-user-frame return on CPU 1: a scheduler-selected proof task freshly enters ring 3, runs a
# real Yield syscall, its post-syscall continuation is saved, and CPU 1's idle dispatcher restores
# that saved frame through canonical assembly so the task continues AFTER the syscall exactly once.
#
# Required ordered live sequence:
#   X86_AP_ONLINE cpu=1
#   X86_AP_GENERIC_DISPATCH_OK cpu=1 mode=fresh ... result=ok
#   USER_LOG ... X86_AP_SAVED_RESUME_BEFORE
#   X86_AP_SAVED_FRAME_COMMITTED cpu=1 syscall=Yield ... result=ok
#   X86_AP_SAVED_DISPATCH_OK cpu=1 mode=saved ... result=ok
#   USER_LOG ... X86_AP_SAVED_FRAME_RESUMED cpu=1 ... continuations=1 result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=x86-ipccall-direct-smp-oracle
KTARGET=${KTARGET:-targets/x86_64-yarm-none.json}
KPROFILE=${KPROFILE:-x86-none}
KELF=${KELF:-target/x86_64-yarm-none/${KPROFILE}/kernel_boot}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ap-saved-return}
TIMEOUT_SECS=${TIMEOUT_SECS:-150}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[ap-saved-return] $*"; }
die()  { echo "[ap-saved-return][fail] $*"; fail=1; }

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
if (( ! fail )); then cp "$KELF" build-x86_64/kernel_boot.elf; fi
if (( fail )); then echo "STAGE_199_X86_AP_SAVED_RETURN_SEAL arch=x86_64 smp=2 result=fail reason=build"; exit 1; fi

note "booting QEMU -smp 2"
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

if [[ ! -s "$BOOT_LOG" ]]; then die "no boot log"; echo "STAGE_199_X86_AP_SAVED_RETURN_SEAL arch=x86_64 smp=2 result=fail reason=no_boot_log"; exit 1; fi
NORM="$LOGDIR/boot.norm.log"; tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
count() { rg -a -c -F "$1" "$NORM" 2>/dev/null || echo 0; }
have()  { rg -a -q -F "$1" "$NORM"; }

have "X86_AP_ONLINE cpu=1" || die "CPU 1 not online"
[[ "$(count "X86_AP_GENERIC_DISPATCH_OK cpu=1 mode=fresh")" == "1" ]] || die "fresh dispatch != 1"
[[ "$(count "X86_AP_SAVED_RESUME_BEFORE")" == "1" ]] || die "BEFORE marker != 1"
[[ "$(count "X86_AP_SAVED_FRAME_COMMITTED cpu=1 syscall=Yield")" == "1" ]] || die "saved-frame-committed != 1"
[[ "$(count "X86_AP_SAVED_DISPATCH_OK cpu=1 mode=saved")" == "1" ]] || die "saved dispatch != 1"
[[ "$(count "X86_AP_SAVED_FRAME_RESUMED cpu=1")" == "1" ]] || die "RESUMED continuation != 1"

# Hard-stops.
for bad in "KERNEL PANIC" "RUST PANIC" "panicked at" "CPU EXCEPTION" "DOUBLE FAULT" "Unhandled" "BOOTSTRAP_ERROR"; do
  have "$bad" && die "fatal condition: $bad"
done
have "X86_AP_SAVED_DISPATCH_OK cpu=0" && die "saved dispatch on CPU 0"
have "X86_AP_SAVED_FRAME_RESUMED cpu=0" && die "continuation on CPU 0"

if (( fail )); then echo "STAGE_199_X86_AP_SAVED_RETURN_SEAL arch=x86_64 smp=2 result=fail (see $BOOT_LOG)"; exit 1; fi

note "genuine idle-dispatcher saved-frame resume proven (fresh entry -> Yield -> saved continuation)"
echo "STAGE_199_X86_AP_SAVED_RETURN_SEAL arch=x86_64 smp=2 cpu=1 fresh_entries=1 saved_dispatches=1 continuations=1 duplicate_entries=0 duplicate_continuations=0 wrong_cpu_continuations=0 result=ok"
