#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 199A2D2C2C — x86_64 complete BIDIRECTIONAL cross-CPU direct IPC (NR6 request + NR7 reply).
#
# Boots QEMU_SMP=2 with the reply sub-selector and proves the COMPLETE two-direction round trip:
#   forward  — CPU-0 client NR6 → CPU-1 recv-v2 server resume + ring-3 request validation (as sealed);
#   reverse  — CPU-0 client blocks in recv-v2 on its reply endpoint (blocked-caller ack) → CPU-1 server
#              issues a genuine NR7 with the Reply cap it read in ring 3 → accepted off-lock reply txn
#              enqueues the caller on CPU 0 → CPU-1→CPU-0 reschedule IPI → CPU-0 saved-frame resume →
#              CPU-0 ring-3 reply validation → duplicate NR7 refused (one-shot barrier).
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=x86-ipccall-direct-smp-oracle
KTARGET=${KTARGET:-targets/x86_64-yarm-none.json}
KPROFILE=${KPROFILE:-x86-none}
KELF=${KELF:-target/x86_64-yarm-none/${KPROFILE}/kernel_boot}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ap-cross-cpu-reply}
TIMEOUT_SECS=${TIMEOUT_SECS:-300}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[reply] $*"; }
die()  { echo "[reply][fail] $*"; fail=1; }

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
if (( fail )); then echo "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 result=fail reason=build"; exit 1; fi

# ── Stage 199A2D3: DETERMINISTIC QEMU lifecycle — the SCRIPT owns termination. ────────────────
# Launch fresh QEMU → monitor a fresh log → wait for ALL final bidirectional proof markers →
# scan fatal each poll → terminate QEMU from the script → wait for exit → only then seal.
source "$(dirname "$0")/lib/qemu-x86-deterministic.sh"

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
  die "qemu-system-x86_64 not installed"
  echo "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 result=fail reason=no_qemu"; exit 1
fi

QEMU_ARGV=(
  qemu-system-x86_64
  -machine "${QEMU_MACHINE:-q35}" -cpu "${QEMU_CPU:-qemu64}" -m "${QEMU_MEMORY:-512M}" -smp 2
  -nographic -monitor none -serial stdio -no-reboot -no-shutdown
  -kernel build-x86_64/kernel_boot.elf
  -initrd build-x86_64/initramfs-core.cpio
  -append "console=ttyS0 rdinit=/init yarm.x86_64_ipccall_direct_smp_oracle=1 yarm.x86_64_ipccall_direct_smp_recv_v2_server=1 yarm.x86_64_ipccall_direct_smp_request=1 yarm.x86_64_ipccall_direct_smp_reply=1 yarm.ap_user_dispatch=1"
)
# The terminal condition = the LAST bidirectional-proof markers of the reverse direction; once all
# three are present the round trip is complete and the script terminates QEMU.
QEMU_TERMINAL_MARKERS=(
  "X86_BSP_REPLY_USER_VALIDATED cpu=0"
  "IPCREPLY_DIRECT_SMP_REPLY_OK sender_cpu=1 receiver_cpu=0 cross_cpu=1"
  "IPCREPLY_DIRECT_SMP_DUPLICATE_REFUSED arch=x86_64"
)
FATAL_RE="KERNEL PANIC|RUST PANIC|panicked at|DOUBLE FAULT|Unhandled|BOOTSTRAP_ERROR|IPCCALL_DIRECT_ACK_OVERWRITE_FUSE|IPCREPLY_DIRECT_ACK_OVERWRITE_FUSE|X86_BSP_REPLY_VALIDATE_FAIL|X86_AP_RECV_V2_VALIDATE_FAIL|X86_AP_RECV_V2_USER_READ_FAULT"

note "booting QEMU -smp 2 (script-owned deterministic lifecycle, ceiling ${TIMEOUT_SECS}s)"
qemu_run_deterministic "$BOOT_LOG" "$FATAL_RE" "$TIMEOUT_SECS" "${QEMU_ARGV[@]}"
LIFECYCLE_RC=$?
note "qemu lifecycle: ${QEMU_LIFECYCLE_RESULT} (rc=${LIFECYCLE_RC})"
if (( LIFECYCLE_RC != 0 )); then
  echo "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 result=fail reason=${QEMU_LIFECYCLE_RESULT}"; exit 1
fi

if [[ ! -s "$BOOT_LOG" ]]; then die "no boot log"; echo "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 result=fail reason=no_boot_log"; exit 1; fi
NORM="$LOGDIR/boot.norm.log"; tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
count() { rg -a -c -F "$1" "$NORM" 2>/dev/null || echo 0; }
have()  { rg -a -q -F "$1" "$NORM"; }

# ── Forward (request) direction must still hold (the sealed B2/B3 round trip). ────────────────
[[ "$(count "IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1")" == "1" ]] || die "server-blocked != 1"
[[ "$(count "X86_BSP_NR6_REQUEST_SENT cpu=0")" == "1" ]] || die "client NR6 sent != 1"
[[ "$(count "IPCCALL_DIRECT_SMP_REQUEST_OK sender_cpu=0 receiver_cpu=1 cross_cpu=1")" == "1" ]] || die "request-ok != 1"
[[ "$(count "X86_AP_RECV_V2_USER_VALIDATED cpu=1")" == "1" ]] || die "server user-validated != 1"

# ── Reverse (reply) direction — the new proof. ────────────────────────────────────────────────
# 1. CPU-0 caller blocks on its reply endpoint (blocked-caller ack published).
[[ "$(count "IPCREPLY_DIRECT_SMP_CALLER_BLOCKED arch=x86_64 caller_cpu=0")" == "1" ]] || die "caller-blocked != 1"
# 2. CPU-1 → CPU-0 reverse reschedule IPI (sent by CPU 1 after the caller enqueue on CPU 0). This is
#    the AUTHORITATIVE, deterministic IPI proof (exactly one requested per reply).
[[ "$(count "X86_BSP_RESCHEDULE_IPI_SENT sender_cpu=1 receiver_cpu=0")" == "1" ]] || die "reverse IPI sent != 1"
# 3. CPU-0's 0xF1 handler marker (pending=1, no dispatch in the handler) is BEST-EFFORT: the resume is
#    driven by CPU-0's trap-return poll of the committed reply, so a timer tick can win the race and
#    produce the terminal markers before the 0xF1 handler runs. Accept 0 or 1 (never a spurious >1),
#    and — when present — require dispatch_in_handler=0 (never a dispatch inside the handler).
RECEIVED_N="$(count "X86_BSP_RESCHEDULE_IPI_RECEIVED cpu=0")"
[[ "$RECEIVED_N" == "0" || "$RECEIVED_N" == "1" ]] || die "reverse IPI received > 1 ($RECEIVED_N)"
if [[ "$RECEIVED_N" == "1" ]]; then
  [[ "$(count "X86_BSP_RESCHEDULE_IPI_RECEIVED cpu=0 pending=1 dispatch_in_handler=0")" == "1" ]] || die "reverse IPI received without dispatch_in_handler=0"
fi
# 4. CPU-0 saved-frame resume of the client's committed recv-v2 continuation (mode=saved).
[[ "$(count "X86_BSP_SAVED_DISPATCH_OK cpu=0 mode=saved")" == "1" ]] || die "BSP saved-dispatch != 1"
# 5. The resumed CPU-0 client validated the reply via DIRECT ring-3 loads (payload+length+meta).
[[ "$(count "X86_BSP_REPLY_USER_VALIDATED cpu=0")" == "1" ]] || die "reply user-validated != 1"
[[ "$(count "X86_BSP_RECV_V2_CONTINUED cpu=0")" == "1" ]] || die "reply recv-v2 continued != 1"
# 6. Terminal reply-OK marker (gated on the client's ring-3 validation + one committed reply).
[[ "$(count "IPCREPLY_DIRECT_SMP_REPLY_OK sender_cpu=1 receiver_cpu=0 cross_cpu=1")" == "1" ]] || die "reply-ok != 1"
# 7. Duplicate NR7 refused exactly once (the Consumed record is the one-shot barrier).
[[ "$(count "IPCREPLY_DIRECT_SMP_DUPLICATE_REFUSED arch=x86_64")" == "1" ]] || die "duplicate-refused != 1"

# ── Hard-stops: no fault after resume, no validation failure, no fuse/migration/panic. ─────────
[[ "$(count "X86_AP_RECV_V2_USER_READ_FAULT")" == "0" ]] || die "ring-3 user-read fault"
[[ "$(count "X86_BSP_REPLY_VALIDATE_FAIL")" == "0" ]] || die "client ring-3 reply validation failed"
[[ "$(count "X86_AP_RECV_V2_VALIDATE_FAIL")" == "0" ]] || die "server ring-3 validation failed"
have "X86_BSP_REPLY_USER_VALIDATED cpu=1" && die "reply validation on wrong CPU (1)"
for bad in "KERNEL PANIC" "RUST PANIC" "panicked at" "DOUBLE FAULT" "Unhandled" "BOOTSTRAP_ERROR" \
           "IPCCALL_DIRECT_ACK_OVERWRITE_FUSE" "IPCREPLY_DIRECT_ACK_OVERWRITE_FUSE"; do
  have "$bad" && die "fatal condition: $bad"
done

if (( fail )); then echo "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 result=fail (see $BOOT_LOG)"; exit 1; fi

note "genuine BIDIRECTIONAL cross-CPU direct IPC proven (NR6 request + NR7 reply, both ring-3 validated)"
echo "STAGE_199_IPCREPLY_DIRECT_SMP_REPLY_USER_SEAL arch=x86_64 smp=2 sender_cpu=1 receiver_cpu=0 cross_cpu=1 saved_resume=1 ring3_payload_read=1 ring3_metadata_read=1 duplicate_replies_refused=1 result=ok"
echo "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 cross_cpu_request=1 cross_cpu_reply=1 request_copies=1 reply_copies=1 server_wakes=1 caller_wakes=1 duplicate_deliveries=0 duplicate_replies=0 result=ok"
