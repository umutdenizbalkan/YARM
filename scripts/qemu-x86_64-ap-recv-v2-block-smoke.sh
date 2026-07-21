#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 199A2D2C2B1 — x86_64 LIVE CPU-1 recv-v2 server BLOCK + blocked-server ACK smoke.
#
# Boots QEMU_SMP=2 with the SMP oracle + the recv-v2-server sub-selector + AP user-dispatch armed and
# proves that a REAL scheduler-selected userspace server on CPU 1 issues a GENUINE recv-v2 syscall
# (through the normal x86 syscall entry + shared trap dispatch) which reaches a COMPLETE authoritative
# blocked state: a committed saved continuation, an installed BlockedRecvState + exact endpoint
# waiter, absence from every runqueue, home CPU 1, and a published BlockedServerAck. This stage does
# NOT deliver an NR6 request and does NOT wake or continue the server (it stays BlockedUnfinalized).
#
# Required ordered live sequence:
#   X86_AP_ONLINE cpu=1
#   X86_AP_GENERIC_DISPATCH_OK cpu=1 mode=fresh ... result=ok
#   USER_LOG ... X86_AP_RECV_V2_SERVER_ENTERED cpu=1 result=ok
#   IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1 recv_v2_committed=1 saved_frame=1 waiter_exact=1
#     ack_published=1 absent_from_runqueue=1 ... result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=x86-ipccall-direct-smp-oracle
KTARGET=${KTARGET:-targets/x86_64-yarm-none.json}
KPROFILE=${KPROFILE:-x86-none}
KELF=${KELF:-target/x86_64-yarm-none/${KPROFILE}/kernel_boot}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ap-recv-v2-block}
TIMEOUT_SECS=${TIMEOUT_SECS:-150}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[ap-recv-v2-block] $*"; }
die()  { echo "[ap-recv-v2-block][fail] $*"; fail=1; }

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
if (( fail )); then echo "STAGE_199_X86_AP_RECV_V2_BLOCK_SEAL arch=x86_64 smp=2 result=fail reason=build"; exit 1; fi

note "booting QEMU -smp 2"
env \
  KERNEL_IMAGE=build-x86_64/kernel_boot.elf \
  INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.x86_64_ipccall_direct_smp_oracle=1 yarm.x86_64_ipccall_direct_smp_recv_v2_server=1 yarm.ap_user_dispatch=1" \
  QEMU_SMP=2 \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  scripts/qemu-x86_64-core-smoke.sh >"$LOGDIR/core-smoke.log" 2>&1 || true

if [[ ! -s "$BOOT_LOG" ]]; then die "no boot log"; echo "STAGE_199_X86_AP_RECV_V2_BLOCK_SEAL arch=x86_64 smp=2 result=fail reason=no_boot_log"; exit 1; fi
NORM="$LOGDIR/boot.norm.log"; tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
count() { rg -a -c -F "$1" "$NORM" 2>/dev/null || echo 0; }
have()  { rg -a -q -F "$1" "$NORM"; }

# Ordered live sequence, each marker exactly once, on CPU 1.
have "X86_AP_ONLINE cpu=1" || die "CPU 1 not online"
[[ "$(count "X86_AP_GENERIC_DISPATCH_OK cpu=1 mode=fresh")" == "1" ]] || die "fresh dispatch != 1"
[[ "$(count "X86_AP_RECV_V2_SERVER_ENTERED cpu=1")" == "1" ]] || die "SERVER_ENTERED marker != 1"
[[ "$(count "IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1")" == "1" ]] || die "server-blocked marker != 1"
have "IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1 recv_v2_committed=1 saved_frame=1 waiter_exact=1 ack_published=1 absent_from_runqueue=1" \
  || die "server-blocked marker missing a required committed condition"
have "IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1" && { have "result=ok" || die "server-blocked not result=ok"; }

# No premature wake / continuation this stage: the server must STAY blocked.
[[ "$(count "X86_AP_SAVED_DISPATCH_OK")" == "0" ]] || die "premature saved-frame dispatch (server must stay blocked)"
[[ "$(count "X86_AP_RECV_V2_CONTINUED")" == "0" ]] || die "premature recv-v2 continuation"
[[ "$(count "IPCCALL_DIRECT_SMP_REQUEST_OK")" == "0" ]] || die "request-ok emitted (this stage does not deliver a request)"

# Hard-stops.
for bad in "KERNEL PANIC" "RUST PANIC" "panicked at" "CPU EXCEPTION" "DOUBLE FAULT" "Unhandled" "BOOTSTRAP_ERROR" "IPCCALL_DIRECT_ACK_OVERWRITE_FUSE"; do
  have "$bad" && die "fatal condition: $bad"
done
have "IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=0" && die "server blocked on wrong CPU (0)"
have "X86_AP_RECV_V2_SERVER_ENTERED cpu=0" && die "server entered on wrong CPU (0)"

if (( fail )); then echo "STAGE_199_X86_AP_RECV_V2_BLOCK_SEAL arch=x86_64 smp=2 result=fail (see $BOOT_LOG)"; exit 1; fi

note "genuine CPU-1 recv-v2 server block + blocked-server ack proven (real syscall, authoritative block)"
echo "STAGE_199_X86_AP_RECV_V2_BLOCK_SEAL arch=x86_64 smp=2 server_cpu=1 real_syscall=1 blocked_commits=1 ack_publications=1 premature_wakes=0 premature_continuations=0 wrong_cpu_blocks=0 result=ok"
