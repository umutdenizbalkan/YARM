#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 199A2D2C2B3 — x86_64 AP saved-resume USER-MEMORY CONSUMPTION proof.
#
# Boots QEMU_SMP=2 with the cross-CPU request oracle and proves that the remotely-awakened CPU-1
# recv-v2 server, after the sealed saved-frame resume, executes NORMAL ring-3 loads and itself
# validates the delivered request payload + length + recv-v2 metadata + fresh receiver-local Reply cap
# — NO ring-3 fault after saved dispatch. Root-cause fix: EFER.NXE is now enabled on the AP (the NX
# bit on non-executable user data pages was previously treated as reserved, faulting every AP ring-3
# data read).
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=x86-ipccall-direct-smp-oracle
KTARGET=${KTARGET:-targets/x86_64-yarm-none.json}
KPROFILE=${KPROFILE:-x86-none}
KELF=${KELF:-target/x86_64-yarm-none/${KPROFILE}/kernel_boot}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ap-cross-cpu-user}
TIMEOUT_SECS=${TIMEOUT_SECS:-200}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[user-consume] $*"; }
die()  { echo "[user-consume][fail] $*"; fail=1; }

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
if (( fail )); then echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_USER_SEAL arch=x86_64 smp=2 result=fail reason=build"; exit 1; fi

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

if [[ ! -s "$BOOT_LOG" ]]; then die "no boot log"; echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_USER_SEAL arch=x86_64 smp=2 result=fail reason=no_boot_log"; exit 1; fi
NORM="$LOGDIR/boot.norm.log"; tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
count() { rg -a -c -F "$1" "$NORM" 2>/dev/null || echo 0; }
have()  { rg -a -q -F "$1" "$NORM"; }

# The full cross-CPU request sequence must still hold.
[[ "$(count "IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1")" == "1" ]] || die "server-blocked != 1"
[[ "$(count "X86_BSP_NR6_REQUEST_SENT cpu=0")" == "1" ]] || die "client NR6 sent != 1"
[[ "$(count "X86_AP_RESCHEDULE_IPI_SENT sender_cpu=0 receiver_cpu=1")" == "1" ]] || die "IPI sent != 1"
[[ "$(count "X86_AP_RESCHEDULE_IPI_RECEIVED cpu=1")" == "1" ]] || die "IPI received != 1"
[[ "$(count "X86_AP_SAVED_DISPATCH_OK cpu=1 mode=saved")" == "1" ]] || die "saved dispatch != 1"
[[ "$(count "IPCCALL_DIRECT_SMP_REQUEST_OK sender_cpu=0 receiver_cpu=1 cross_cpu=1")" == "1" ]] || die "request-ok != 1"

# The NEW proof: the resumed CPU-1 server validated the delivered data via DIRECT ring-3 loads. The
# stub emits X86_AP_RECV_V2_USER_VALIDATED ONLY after all three ring-3 checks pass (payload bytes ==
# "NR6-REQ!", meta payload_len == 8, meta reply cap != 0), else X86_AP_RECV_V2_VALIDATE_FAIL. Its
# field values are stub-fixed; tolerate concurrent-CPU UART interleaving in the middle of the line.
[[ "$(count "X86_AP_RECV_V2_USER_VALIDATED cpu=1")" == "1" ]] || die "user-validated marker != 1"

# Hard-stops: no ring-3 fault after resume, no validation failure, no migration/duplication.
[[ "$(count "X86_AP_RECV_V2_USER_READ_FAULT")" == "0" ]] || die "ring-3 user-read fault after resume"
[[ "$(count "X86_AP_RECV_V2_VALIDATE_FAIL")" == "0" ]] || die "server ring-3 validation failed"
have "X86_AP_RECV_V2_USER_VALIDATED cpu=0" && die "validation on wrong CPU (0)"
for bad in "KERNEL PANIC" "RUST PANIC" "panicked at" "DOUBLE FAULT" "Unhandled" "BOOTSTRAP_ERROR" "IPCCALL_DIRECT_ACK_OVERWRITE_FUSE"; do
  have "$bad" && die "fatal condition: $bad"
done

if (( fail )); then echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_USER_SEAL arch=x86_64 smp=2 result=fail (see $BOOT_LOG)"; exit 1; fi

note "genuine CPU-1 ring-3 user-memory consumption proven (payload+length+meta+reply-cap validated in userspace)"
echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_USER_SEAL arch=x86_64 smp=2 sender_cpu=0 receiver_cpu=1 cross_cpu=1 saved_resume=1 ring3_payload_read=1 ring3_metadata_read=1 ring3_reply_cap_read=1 duplicate_deliveries=0 duplicate_wakes=0 result=ok"
