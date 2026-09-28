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
# QEMU-SMP1 §4: graded end to end through the production owners — caller blocks, reply settles
# once, one wake arrival, production selection on CPU 0, exact continuation, further progress, and
# both tasks blocked again with neither CPU parked.
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=x86-ipccall-direct-smp-oracle
KTARGET=${KTARGET:-targets/x86_64-yarm-none.json}
KPROFILE=${KPROFILE:-x86-none}
KELF=${KELF:-target/x86_64-yarm-none/${KPROFILE}/kernel_boot}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ap-cross-cpu-reply}
# REGRADE=1 grades an existing "$LOGDIR/boot.log" without building or booting.
REGRADE=${REGRADE:-}
TIMEOUT_SECS=${TIMEOUT_SECS:-300}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[reply] $*"; }
die()  { echo "[reply][fail] $*"; fail=1; }

if [[ -z "$REGRADE" ]]; then
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
fi

# ── Stage 199A2D3: DETERMINISTIC QEMU lifecycle — the SCRIPT owns termination. ────────────────
# Launch fresh QEMU → monitor a fresh log → wait for ALL final bidirectional proof markers →
# scan fatal each poll → terminate QEMU from the script → wait for exit → only then seal.
source "$(dirname "$0")/lib/qemu-x86-deterministic.sh"
if [[ -z "$REGRADE" ]]; then

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
# QEMU-SMP1 §4 — the terminal condition is the END of the whole chain, not its middle: the resumed
# client has made further progress and reported the state-derived summary, the server has observed
# the duplicate's refusal in ring 3, and BOTH tasks have blocked again (CPU 1 back in its
# interruptible idle, CPU 0 free for the rest of the system). The script terminates QEMU only then.
QEMU_TERMINAL_MARKERS=(
  "X86_SMP_REPLY_PROGRESS_SUMMARY"
  "X86_SMP_REPLY_USER_ECHO X86_AP_DUPLICATE_REPLY_REFUSED_OBSERVED cpu=1 err=InvalidCapability result=ok"
  "X86_SMP_ORACLE_BLOCKED cpu=1 tid=20205 endpoint=6 wait_gen=2"
  "X86_SMP_ORACLE_BLOCKED cpu=0 tid=21205 endpoint=7 wait_gen=2"
)
FATAL_RE="KERNEL PANIC|RUST PANIC|panicked at|DOUBLE FAULT|Unhandled|BOOTSTRAP_ERROR|IPCCALL_DIRECT_ACK_OVERWRITE_FUSE|IPCREPLY_DIRECT_ACK_OVERWRITE_FUSE|X86_BSP_REPLY_VALIDATE_FAIL|X86_AP_RECV_V2_VALIDATE_FAIL|X86_AP_RECV_V2_USER_READ_FAULT|X86_TLB_SHOOTDOWN_FAIL|X86_USER_FPU_HOME_UNAUTHENTICATED|X86_AP_SAVED_RESUME_REFUSED|X86_AP_REPLY_SEND_FAIL|X86_AP_DUPLICATE_REPLY_NOT_REFUSED|RECV_AGAIN_RETURNED|X86_BSP_CLIENT_PROGRESS cpu=0 result=fail"

note "booting QEMU -smp 2 (script-owned deterministic lifecycle, ceiling ${TIMEOUT_SECS}s)"
qemu_run_deterministic "$BOOT_LOG" "$FATAL_RE" "$TIMEOUT_SECS" "${QEMU_ARGV[@]}"
LIFECYCLE_RC=$?
note "qemu lifecycle: ${QEMU_LIFECYCLE_RESULT} (rc=${LIFECYCLE_RC})"
if (( LIFECYCLE_RC != 0 )); then
  echo "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 result=fail reason=${QEMU_LIFECYCLE_RESULT}"; exit 1
fi
fi

if [[ ! -s "$BOOT_LOG" ]]; then die "no boot log"; echo "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 result=fail reason=no_boot_log"; exit 1; fi
NORM="$LOGDIR/boot.norm.log"; tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
count() { rg -a -c -F "$1" "$NORM" 2>/dev/null || echo 0; }
have()  { rg -a -q -F "$1" "$NORM"; }

# ── Forward (request) direction must still hold (the sealed B2/B3 round trip). ────────────────
[[ "$(count "X86_BSP_NR6_REQUEST_SENT cpu=0")" == "1" ]] || die "client NR6 sent != 1"
[[ "$(count "IPCCALL_DIRECT_SMP_REQUEST_OK sender_cpu=0 receiver_cpu=1 cross_cpu=1")" == "1" ]] || die "request-ok != 1"
[[ "$(count "X86_AP_RECV_V2_USER_VALIDATED cpu=1")" == "1" ]] || die "server user-validated != 1"

# ── Reverse (reply) direction, graded from the PRODUCTION owners (QEMU-SMP1 §4). ────────────
# Identities come from the provisioning markers, never assumed. The SMP1 user markers are graded
# through their synchronous kernel echoes (`X86_SMP_REPLY_USER_ECHO`); a `USER_LOG` line can be
# dropped by the shared printk ring while both CPUs log.
SERVER_TID="$(rg -a -o -r '$1' 'X86_AP_RECV_V2_SERVER_PROVISIONED base_tid=([0-9]+)' "$NORM" | head -1)"
SERVER_EP="$(rg -a -o -r '$1' 'X86_AP_RECV_V2_SERVER_PROVISIONED .* endpoint_index=([0-9]+)' "$NORM" | head -1)"
CLIENT_TID="$(rg -a -o -r '$1' 'X86_BSP_NR6_CLIENT_PROVISIONED client_tid=([0-9]+)' "$NORM" | head -1)"
[[ "$SERVER_TID" == "20205" && "$SERVER_EP" == "6" && "$CLIENT_TID" == "21205" ]] \
  || die "provisioned identities (server=$SERVER_TID ep=$SERVER_EP client=$CLIENT_TID) differ from the terminal markers"
# 0. The server's committed block on CPU 1, from the request TRANSACTION RECORD (QEMU-SMP1-SEAL).
#    The one-shot IPCCALL_DIRECT_SMP_SERVER_BLOCKED line goes through the asynchronous printk ring
#    and can be lost while both CPUs log (lost live at 3107cc0a); the same marker body records the
#    block, after its re-verification, and the record is reported synchronously once complete.
[[ "$(rg -a -c 'X86_SMP_REQUEST_TXN_SEAL steps=7 recorded=7 duplicates=0 target_cpu=1 ' "$NORM" || echo 0)" == "1" ]] \
  || die "request transaction record not sealed complete exactly once"
[[ "$(rg -a -c "X86_SMP_REQUEST_TXN step=blocked seq=[1-9][0-9]* f0=${SERVER_TID} f1=[1-9][0-9]* " "$NORM" || echo 0)" == "1" ]] \
  || die "server block on cpu 1 not recorded for server ${SERVER_TID}"
line_of() { rg -a -n -F "$1" "$NORM" | head -1 | cut -d: -f1; }
# 1. The caller blocks on its reply endpoint through the split receive owner (its synchronous
#    echo, X86_SMP_ORACLE_BLOCKED), and the reply record is armed for exactly this caller/replier pair. (The oracle-only CALLER_BLOCKED marker is emitted
#    by a broad-route helper the split route never calls; it is no longer graded.)
BLOCK1="$(rg -a -o -r '$1' "X86_SMP_ORACLE_BLOCKED cpu=0 tid=${CLIENT_TID} endpoint=([0-9]+) wait_gen=1\b" "$NORM" | head -1)"
[[ -n "$BLOCK1" ]] || die "caller did not block on cpu 0 (wait_gen=1)"
ARMED="$(rg -a -c "IPC_REPLY_TERMINAL_ARMED_SPLIT caller_tid=${CLIENT_TID} .* replier_tid=${SERVER_TID} " "$NORM" || echo 0)"
[[ "$ARMED" == "1" ]] || die "reply record armed for caller/replier != 1 ($ARMED)"
GEN="$(rg -a -o -r '$1' "IPC_REPLY_TERMINAL_ARMED_SPLIT caller_tid=${CLIENT_TID} .* record_generation=([0-9]+) replier_tid=${SERVER_TID} " "$NORM" | head -1)"
# 2. The reply settles ONCE, for exactly that record generation.
[[ "$(rg -a -c "IPCREPLY_DIRECT_TERMINAL_CLAIM .*replier_tid=${SERVER_TID} " "$NORM" || echo 0)" == "1" ]] || die "reply claims by the server != 1"
[[ "$(count "IPCREPLY_DIRECT_TERMINAL_CLAIM record_index=0 record_generation=${GEN} replier_tid=${SERVER_TID} terminal=Reply resolution=commit settled=1 result=ok")" == "1" ]] || die "reply claim for generation ${GEN} != 1"
# 3. The duplicate NR7 is refused ONCE, with no copy and no wake, and the server observes that
#    refusal in ring 3 (the old one-shot marker sat behind a check the fast-revoked cap never
#    reaches; the refusal is the capability owner's).
[[ "$(count "IPCREPLY_DIRECT_REFUSED_PRE_LOCK record_index=18446744073709551615 record_generation=18446744073709551615 replier_tid=${SERVER_TID} reason=reply_cap err=InvalidCapability reply_copies=0 caller_wakes=0")" == "1" ]] || die "duplicate pre-lock refusal != 1"
[[ "$(count "X86_SMP_REPLY_USER_ECHO X86_AP_DUPLICATE_REPLY_REFUSED_OBSERVED cpu=1 err=InvalidCapability result=ok")" == "1" ]] || die "server did not observe the refusal once"
# 4. One logical wake to CPU 0, and exactly one 0xF1 ARRIVAL there — counted by the pure-asm stub
#    itself into CPU 0's per-CPU record (a hardware arrival may coalesce but can never duplicate).
[[ "$(count "X86_BSP_RESCHEDULE_IPI_SENT sender_cpu=1 receiver_cpu=0")" == "1" ]] || die "reverse IPI sent != 1"
SUMMARY="$(rg -a -o 'X86_SMP_REPLY_PROGRESS_WAKES .*' "$NORM" | head -1) $(rg -a -o 'X86_SMP_REPLY_PROGRESS_SUMMARY .*' "$NORM" | head -1)"
field() { sed -n "s/.* $1=\([0-9]*\).*/\1/p" <<<"$SUMMARY"; }
[[ "$(field cpu0_wake_arrivals)" == "1" ]] || die "cpu0 0xF1 arrivals != 1 ($SUMMARY)"
[[ "$(( $(field cpu0_kernel_origin) + $(field cpu0_user_origin) ))" == "1" ]] || die "cpu0 arrival origins do not sum to 1"
[[ "$(field reply_delivered)" == "1" ]] || die "committed replies != 1"
[[ "$(field client_tid)" == "$CLIENT_TID" ]] || die "summary client differs"
[[ "$(field cpu1_tlb_req_gen)" == "$(field cpu1_tlb_ack_gen)" ]] || die "cpu1 TLB request outstanding (req != ack)"
# 5. The PRODUCTION scheduler selects the caller on CPU 0 after the claim, and before its
#    continuation runs; nothing else resumes it (the oracle BSP resume is retired). Which
#    production drain makes that selection depends on what else is runnable on CPU 0 when the
#    0xF1 wake lands: an idle CPU's timer/yield drain dequeues the caller directly
#    (`*_DEQUEUE_OK cpu=0 … =<caller>`); if a task whose receive deadline expires on the same
#    tick is ahead of it, that task runs, blocks in its receive, and the blocking-receive drain
#    selects the caller (`D2_RECV_GENUINE_DISPATCH_DONE result=switch cpu=0 incoming=<caller>`);
#    if a task exits on CPU 0 when the wake lands, the exit owner's post-drain revalidation
#    commits the caller as its replacement (`EXIT_TASK_OWNER_REVALIDATED … cpu=0 …
#    committed=replacement next_tid=<caller>`, seen live at e14c914a). All three go through the
#    one queue-advance selection owner.
[[ "$(count "X86_BSP_SAVED_DISPATCH_OK")" == "0" ]] || die "retired oracle BSP resume ran"
CLAIM_AT="$(line_of "IPCREPLY_DIRECT_TERMINAL_CLAIM record_index=0 record_generation=${GEN} replier_tid=${SERVER_TID} ")"
CONT_AT="$(line_of "X86_SMP_REPLY_USER_ECHO X86_BSP_RECV_V2_CONTINUED cpu=0")"
SELECT_AT="$(rg -a -n -e "DEQUEUE_OK cpu=0 (tid|incoming)=${CLIENT_TID}\b" -e "D2_RECV_GENUINE_DISPATCH_DONE result=switch cpu=0 incoming=${CLIENT_TID}\b" -e "EXIT_TASK_OWNER_REVALIDATED arch=x86_64 cpu=0 prepared=idle committed=replacement next_tid=${CLIENT_TID} " "$NORM" | cut -d: -f1 | awk -v a="${CLAIM_AT:-0}" -v b="${CONT_AT:-0}" '$1>a && $1<b' | head -1)"
[[ -n "$CLAIM_AT" && -n "$CONT_AT" && -n "$SELECT_AT" ]] || die "no production selection of the caller on cpu 0 between the claim and its continuation"
rg -a -q -e "DEQUEUE_OK cpu=1 (tid|incoming)=${CLIENT_TID}\b" -e "D2_RECV_GENUINE_DISPATCH_DONE result=switch cpu=1 incoming=${CLIENT_TID}\b" -e "EXIT_TASK_OWNER_REVALIDATED arch=x86_64 cpu=1 .* next_tid=${CLIENT_TID} " "$NORM" && die "caller selected on the wrong CPU"
# 6. The exact continuation returns to ring 3 ONCE, validates the reply, and makes further
#    progress (a Yield round trip), then both tasks block again — no park, no spin.
[[ "$(count "X86_SMP_REPLY_USER_ECHO X86_BSP_RECV_V2_CONTINUED cpu=0")" == "1" ]] || die "reply recv-v2 continued != 1"
[[ "$(count "X86_BSP_REPLY_USER_VALIDATED cpu=0")" == "1" ]] || die "reply user-validated != 1"
[[ "$(count "IPCREPLY_DIRECT_SMP_REPLY_OK sender_cpu=1 receiver_cpu=0 cross_cpu=1")" == "1" ]] || die "reply-ok != 1"
[[ "$(count "X86_SMP_REPLY_USER_ECHO X86_BSP_CLIENT_PROGRESS cpu=0 step=post_reply_yield result=ok")" == "1" ]] || die "client further progress != 1"
PROG_AT="$(line_of "X86_SMP_REPLY_USER_ECHO X86_BSP_CLIENT_PROGRESS cpu=0 step=post_reply_yield")"
(( CONT_AT < PROG_AT )) || die "progress precedes the continuation"
[[ "$(rg -a -c "X86_SMP_ORACLE_BLOCKED cpu=1 tid=${SERVER_TID} endpoint=${SERVER_EP} wait_gen=2$" "$NORM" || echo 0)" == "1" ]] || die "server did not block again on cpu 1"
[[ "$(rg -a -c "X86_SMP_ORACLE_BLOCKED cpu=0 tid=${CLIENT_TID} endpoint=${BLOCK1} wait_gen=2$" "$NORM" || echo 0)" == "1" ]] || die "client did not block again on cpu 0"
# 7. No CPU was parked by the profile, no shootdown timed out, no home was unauthenticated.
[[ "$(count "X86_TLB_SHOOTDOWN_FAIL")" == "0" ]] || die "TLB shootdown failure"
[[ "$(count "X86_USER_FPU_COMMIT_REFUSED")" == "0" ]] || die "FP home commit refused"
[[ "$(count "X86_USER_FPU_HOME_UNAUTHENTICATED")" == "0" ]] || die "FP home unauthenticated"
[[ "$(count "X86_AP_SAVED_RESUME_REFUSED")" == "0" ]] || die "AP saved resume refused"

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
echo "STAGE_199_IPCREPLY_DIRECT_SMP_REPLY_USER_SEAL arch=x86_64 smp=2 sender_cpu=1 receiver_cpu=0 cross_cpu=1 saved_resume=1 production_selection=1 further_progress=1 reblocked=2 ring3_payload_read=1 ring3_metadata_read=1 duplicate_replies_refused=1 result=ok"
echo "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 cross_cpu_request=1 cross_cpu_reply=1 request_copies=1 reply_copies=1 server_wakes=1 caller_wakes=1 duplicate_deliveries=0 duplicate_replies=0 result=ok"
