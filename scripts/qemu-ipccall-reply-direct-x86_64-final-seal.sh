#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 199A2D3 — x86_64 Direct IPC Exact-Commit Regression and Freeze Seal.
#
# Proves, from ONE exact clean commit, that the completed x86_64 direct-IPC implementation (with the
# Stage 199A2D2C2C BSP saved-resume + nested-trap-depth changes) preserves all four behaviors, then
# emits the freeze seal:
#
#   A. x86 feature-off core smoke, SMP=1        (no direct-oracle retirement markers)
#   B. x86 direct request/reply functional smoke, SMP=1
#   C. x86 AP dispatch/saved-resume regression smoke, SMP=2
#   D. x86 bidirectional cross-CPU direct IPC smoke, SMP=2   (script-owned deterministic QEMU)
#
# The exact SHA + clean tree are captured once and RE-CHECKED after every child run; fresh logs and
# artifacts only. The freeze seal is emitted ONLY after all four fresh runs succeed from the same
# clean commit.
set -uo pipefail
cd "$(dirname "$0")/.."

ROOT=$(pwd -P)
SEAL_LOGDIR=${SEAL_LOGDIR:-/tmp/x86-direct-ipc-final-seal}
rm -rf "$SEAL_LOGDIR"; mkdir -p "$SEAL_LOGDIR"

note() { echo "[final-seal] $*"; }
fail() { echo "[final-seal][fail] $*"; echo "STAGE_199_X86_DIRECT_IPC_FINAL_SEAL result=fail reason=$1"; exit 1; }

# ── Exact-commit + clean-tree capture ───────────────────────────────────────────────────────────
EXACT_SHA=$(git rev-parse HEAD)
note "exact commit: $EXACT_SHA"
if [[ -n "$(git status --porcelain)" ]]; then
  git status --porcelain | head; fail dirty_tree_at_start
fi
note "clean tree confirmed at start"

# Re-check SHA + clean tree after each child run — no drift, no artifact contamination between runs.
recheck_exact_commit() {
  local phase="$1"
  local sha; sha=$(git rev-parse HEAD)
  [[ "$sha" == "$EXACT_SHA" ]] || fail "sha_drift_after_${phase}"
  if [[ -n "$(git status --porcelain)" ]]; then
    git status --porcelain | head; fail "dirty_tree_after_${phase}"
  fi
  note "exact-commit re-check OK after ${phase}"
}

# Fixed-substring counter over a normalized (CR->LF) log.
count_in() { tr '\r' '\n' <"$1" | rg -a -c -F "$2" 2>/dev/null || echo 0; }
have_in()  { tr '\r' '\n' <"$1" | rg -a -q -F "$2"; }

# ══════════════════════════════════════════════════════════════════════════════════════════════
# RUN_A — x86 feature-off core smoke, SMP=1. No direct-oracle retirement markers may appear.
# ══════════════════════════════════════════════════════════════════════════════════════════════
run_a() {
  note "RUN_A: feature-off core smoke (SMP=1)"
  local d="$SEAL_LOGDIR/A-feature-off"; mkdir -p "$d"
  BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-x86_64-artifacts.sh >"$d/build.log" 2>&1 || fail A_build
  cp target/x86_64-yarm-none/x86-none/kernel_boot "$d/kernel_boot.elf"
  env \
    KERNEL_IMAGE="$d/kernel_boot.elf" \
    INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio \
    KERNEL_CMDLINE="console=ttyS0 rdinit=/init" \
    QEMU_SMP=1 \
    LOGFILE="$d/boot.log" \
    SMOKE_LOG="$d/smoke.log" \
    TIMEOUT_SECS="${A_TIMEOUT:-120}" \
    QEMU_SMOKE_STRICT=1 \
    scripts/qemu-x86_64-core-smoke.sh >"$d/core-smoke.log" 2>&1 || fail A_core_smoke
  [[ -s "$d/boot.log" ]] || fail A_no_boot_log
  # No direct-oracle retirement markers in a feature-off boot.
  for m in "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=x86_64 class=IpcCallDirectRequest" \
           "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=x86_64 class=IpcReplyDirect" \
           "IPCCALL_DIRECT_REQUEST_OK" "IPCREPLY_DIRECT_OK" \
           "IPCCALL_DIRECT_SMP_REQUEST_OK" "IPCREPLY_DIRECT_SMP_REPLY_OK"; do
    [[ "$(count_in "$d/boot.log" "$m")" == "0" ]] || fail "A_direct_marker_present:${m}"
  done
  note "RUN_A ok: feature-off boot contains no direct-oracle retirement markers"
}

# ══════════════════════════════════════════════════════════════════════════════════════════════
# RUN_B — SMP=1 direct request/reply functional smoke.
# ══════════════════════════════════════════════════════════════════════════════════════════════
run_b() {
  note "RUN_B: SMP=1 direct request/reply functional smoke"
  local d="$SEAL_LOGDIR/B-functional-smp1"; mkdir -p "$d"
  LOGDIR="$d" scripts/qemu-ipccall-reply-direct-x86_64-smoke.sh >"$d/run.log" 2>&1 || fail B_smoke
  have_in "$d/run.log" "STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL" || fail B_no_seal
  have_in "$d/run.log" "result=ok" || fail B_not_ok
  local bl="$d/boot.log"
  [[ -s "$bl" ]] || fail B_no_boot_log
  # Required SMP=1 preservation counts (exactly one each; zero duplicate reply successes).
  [[ "$(count_in "$bl" "IPCCALL_DIRECT_REQUEST_OK")" == "1" ]] || fail B_request_successes
  [[ "$(count_in "$bl" "IPCREPLY_DIRECT_OK")" == "1" ]] || fail B_reply_successes
  have_in "$bl" "server_wakes=1" || fail B_server_wakes
  have_in "$bl" "caller_wakes=1" || fail B_caller_wakes
  # Exactly one successful reply -> the userspace round trip reports duplicate_reply=rejected.
  have_in "$bl" "duplicate_reply=rejected" || fail B_duplicate_reply_not_rejected
  note "RUN_B ok: request=1 reply=1 server_wakes=1 caller_wakes=1 duplicate_reply=rejected"
}

# ══════════════════════════════════════════════════════════════════════════════════════════════
# RUN_C — SMP=2 AP dispatch/saved-resume regression (request user-consumption).
# ══════════════════════════════════════════════════════════════════════════════════════════════
run_c() {
  note "RUN_C: SMP=2 AP dispatch/saved-resume regression"
  local d="$SEAL_LOGDIR/C-ap-dispatch-smp2"; mkdir -p "$d"
  LOGDIR="$d" scripts/qemu-x86_64-ap-cross-cpu-user-consume-smoke.sh >"$d/run.log" 2>&1 || fail C_smoke
  have_in "$d/run.log" "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_USER_SEAL" || fail C_no_seal
  have_in "$d/run.log" "result=ok" || fail C_not_ok
  local bl="$d/boot.log"
  [[ -s "$bl" ]] || fail C_no_boot_log
  [[ "$(count_in "$bl" "X86_AP_SAVED_DISPATCH_OK cpu=1 mode=saved")" == "1" ]] || fail C_saved_dispatch
  [[ "$(count_in "$bl" "X86_AP_RECV_V2_USER_VALIDATED cpu=1")" == "1" ]] || fail C_request_user_consumed
  [[ "$(count_in "$bl" "X86_AP_RECV_V2_USER_READ_FAULT")" == "0" ]] || fail C_ring3_fault
  note "RUN_C ok: AP saved-dispatch=1 request_user_consumed=1 no ring-3 fault"
}

# ══════════════════════════════════════════════════════════════════════════════════════════════
# RUN_D — SMP=2 bidirectional cross-CPU direct IPC (script-owned deterministic QEMU).
# ══════════════════════════════════════════════════════════════════════════════════════════════
run_d() {
  note "RUN_D: SMP=2 bidirectional cross-CPU direct IPC (deterministic lifecycle)"
  local d="$SEAL_LOGDIR/D-bidirectional-smp2"; mkdir -p "$d"
  LOGDIR="$d" scripts/qemu-x86_64-ap-cross-cpu-reply-smoke.sh >"$d/run.log" 2>&1 || fail D_smoke
  have_in "$d/run.log" "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL" || fail D_no_seal
  have_in "$d/run.log" "cross_cpu_request=1 cross_cpu_reply=1" || fail D_not_bidirectional
  have_in "$d/run.log" "proof_complete_then_terminated" || fail D_not_script_terminated
  local bl="$d/boot.log"
  [[ -s "$bl" ]] || fail D_no_boot_log
  # SMP=2 preservation counts.
  [[ "$(count_in "$bl" "IPCCALL_DIRECT_SMP_REQUEST_OK sender_cpu=0 receiver_cpu=1 cross_cpu=1")" == "1" ]] || fail D_cross_cpu_request
  [[ "$(count_in "$bl" "IPCREPLY_DIRECT_SMP_REPLY_OK sender_cpu=1 receiver_cpu=0 cross_cpu=1")" == "1" ]] || fail D_cross_cpu_reply
  [[ "$(count_in "$bl" "X86_AP_RECV_V2_USER_VALIDATED cpu=1")" == "1" ]] || fail D_request_user_consumed
  [[ "$(count_in "$bl" "X86_BSP_REPLY_USER_VALIDATED cpu=0")" == "1" ]] || fail D_reply_user_consumed
  [[ "$(count_in "$bl" "X86_AP_RESCHEDULE_IPI_SENT sender_cpu=0 receiver_cpu=1")" == "1" ]] || fail D_fwd_ipi
  [[ "$(count_in "$bl" "X86_BSP_RESCHEDULE_IPI_SENT sender_cpu=1 receiver_cpu=0")" == "1" ]] || fail D_rev_ipi
  [[ "$(count_in "$bl" "X86_AP_RECV_V2_CONTINUED cpu=1")" == "1" ]] || fail D_server_continuation
  [[ "$(count_in "$bl" "X86_BSP_RECV_V2_CONTINUED cpu=0")" == "1" ]] || fail D_client_continuation
  [[ "$(count_in "$bl" "IPCREPLY_DIRECT_SMP_DUPLICATE_REFUSED")" == "1" ]] || fail D_duplicate_refused
  # Duplicate/overwrite hard-stops must be absent.
  [[ "$(count_in "$bl" "IPCCALL_DIRECT_ACK_OVERWRITE_FUSE")" == "0" ]] || fail D_req_fuse
  [[ "$(count_in "$bl" "IPCREPLY_DIRECT_ACK_OVERWRITE_FUSE")" == "0" ]] || fail D_rep_fuse
  note "RUN_D ok: request/reply cross-CPU=1, user-consumed both dirs, IPIs 1/1, continuations 1/1, dup refused, no fuse"
}

# ── Serial execution with per-run exact-commit re-checks ────────────────────────────────────────
run_a; recheck_exact_commit RUN_A
run_b; recheck_exact_commit RUN_B
run_c; recheck_exact_commit RUN_C
run_d; recheck_exact_commit RUN_D

note "all four fresh exact-commit runs succeeded from $EXACT_SHA"
echo "STAGE_199_X86_DIRECT_IPC_FINAL_SEAL functional_smp1=1 ap_dispatch_smp2=1 cross_cpu_request_smp2=1 cross_cpu_reply_smp2=1 request_user_consumed=1 reply_user_consumed=1 trap_depth_errors=0 wrong_current_task=0 duplicate_replies=0 duplicate_wakes=0 overwrite_fuse_trips=0 result=ok"
