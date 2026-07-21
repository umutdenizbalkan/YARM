#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 199A2D2C2B2 — x86_64 LIVE cross-CPU NR6 request: CPU-0 client → CPU-1 recv-v2 resume.
#
# Boots QEMU_SMP=2 with the SMP oracle + the recv-v2-server + the cross-CPU request sub-selectors and
# proves the COMPLETE real userspace-to-userspace cross-CPU request direction:
#   real CPU-1 recv-v2 server blocks → real CPU-0 userspace client invokes NR6 → accepted direct
#   transaction delivers cross-CPU → server becomes RunnableSaved on CPU 1 → CPU 0 sends the real
#   reschedule IPI → CPU 1 restores the saved recv-v2 frame → the server continues after recv-v2 once.
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=x86-ipccall-direct-smp-oracle
KTARGET=${KTARGET:-targets/x86_64-yarm-none.json}
KPROFILE=${KPROFILE:-x86-none}
KELF=${KELF:-target/x86_64-yarm-none/${KPROFILE}/kernel_boot}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ap-cross-cpu-request}
TIMEOUT_SECS=${TIMEOUT_SECS:-180}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[cross-cpu-request] $*"; }
die()  { echo "[cross-cpu-request][fail] $*"; fail=1; }

note "building base x86_64 artifacts"
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
if (( fail )); then echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL arch=x86_64 smp=2 result=fail reason=build"; exit 1; fi

note "booting QEMU -smp 2"
env \
  KERNEL_IMAGE=build-x86_64/kernel_boot.elf \
  INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.x86_64_ipccall_direct_smp_oracle=1 yarm.x86_64_ipccall_direct_smp_recv_v2_server=1 yarm.x86_64_ipccall_direct_smp_request=1 yarm.ap_user_dispatch=1" \
  QEMU_SMP=2 \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  scripts/qemu-x86_64-core-smoke.sh >"$LOGDIR/core-smoke.log" 2>&1 || true

if [[ ! -s "$BOOT_LOG" ]]; then die "no boot log"; echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL arch=x86_64 smp=2 result=fail reason=no_boot_log"; exit 1; fi
NORM="$LOGDIR/boot.norm.log"; tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
count() { rg -a -c -F "$1" "$NORM" 2>/dev/null || echo 0; }
have()  { rg -a -q -F "$1" "$NORM"; }

# Ordered live sequence (each exactly once, correct CPUs).
have "X86_AP_ONLINE cpu=1" || die "CPU 1 not online"
[[ "$(count "X86_AP_GENERIC_DISPATCH_OK cpu=1 mode=fresh")" == "1" ]] || die "fresh dispatch != 1"
[[ "$(count "X86_AP_RECV_V2_SERVER_ENTERED cpu=1")" == "1" ]] || die "SERVER_ENTERED != 1"
[[ "$(count "IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1")" == "1" ]] || die "server-blocked != 1"
[[ "$(count "X86_BSP_NR6_REQUEST_SENT cpu=0")" == "1" ]] || die "client NR6 sent != 1"
[[ "$(count "X86_AP_RESCHEDULE_IPI_SENT sender_cpu=0 receiver_cpu=1")" == "1" ]] || die "IPI sent != 1"
[[ "$(count "X86_AP_RESCHEDULE_IPI_RECEIVED cpu=1")" == "1" ]] || die "IPI received != 1"
# The RECEIVED marker carries hardcoded `pending=1 dispatch_in_handler=0`; tolerate stray bytes from
# concurrent CPU0/CPU1 UART interleaving (the semantic content is kernel-fixed). Reject only an
# explicit in-handler dispatch.
have "X86_AP_RESCHEDULE_IPI_RECEIVED cpu=1 pending=1" || die "IPI received without pending=1"
have "dispatch_in_handler=1" && die "IPI handler performed a dispatch"
[[ "$(count "X86_AP_SAVED_DISPATCH_OK cpu=1 mode=saved")" == "1" ]] || die "saved dispatch != 1"
[[ "$(count "X86_AP_RECV_V2_CONTINUED cpu=1")" == "1" ]] || die "recv-v2 continuation != 1"
[[ "$(count "IPCCALL_DIRECT_SMP_REQUEST_OK sender_cpu=0 receiver_cpu=1 cross_cpu=1")" == "1" ]] || die "request-ok != 1"

# Negatives / hard-stops.
[[ "$(count "X86_AP_RECV_V2_VALIDATE_FAIL")" == "0" ]] || die "server validation failed"
[[ "$(count "X86_BSP_NR6_REQUEST cpu=0 result=fail")" == "0" ]] || die "client NR6 failed"
have "X86_AP_SAVED_DISPATCH_OK cpu=0" && die "saved dispatch on CPU 0 (migration)"
have "X86_AP_RECV_V2_CONTINUED cpu=0" && die "continuation on CPU 0 (migration)"
for bad in "KERNEL PANIC" "RUST PANIC" "panicked at" "CPU EXCEPTION" "DOUBLE FAULT" "Unhandled" "BOOTSTRAP_ERROR" "IPCCALL_DIRECT_ACK_OVERWRITE_FUSE"; do
  have "$bad" && die "fatal condition: $bad"
done

if (( fail )); then echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL arch=x86_64 smp=2 result=fail (see $BOOT_LOG)"; exit 1; fi

note "genuine cross-CPU NR6 request proven (CPU-0 client -> CPU-1 recv-v2 resume, real IPI)"
echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL arch=x86_64 smp=2 pairs=1 sender_cpu=0 receiver_cpu=1 cross_cpu=1 request_copies=1 server_wakes=1 server_continuations=1 duplicate_deliveries=0 duplicate_wakes=0 wrong_cpu_continuations=0 result=ok"
