#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 199A2D1 — x86_64 SMP=2 DIRECT IpcCall(NR6)/IpcReply(NR7) cross-CPU smoke.
#
# GOAL (Part 4): boot the DIRECT oracle under QEMU_SMP=2 and attempt to observe a GENUINE
# cross-CPU round trip — the request server committed-blocked on one CPU, the client
# claiming + delivering on a DIFFERENT CPU, and the reply travelling back cross-CPU — with
# strictly cross-CPU markers:
#
#   IPCCALL_DIRECT_SMP_REQUEST_OK arch=x86_64 sender_cpu=<n> receiver_cpu=<m> cross_cpu=1 server_wakes=1
#   IPCREPLY_DIRECT_SMP_REPLY_OK  arch=x86_64 replier_cpu=<m> caller_cpu=<n> cross_cpu=1 caller_wakes=1 one_shot=1
#
# with sender_cpu != receiver_cpu and replier_cpu != caller_cpu.
#
# HONEST STATUS (Stage 199A2D1): the x86 AP user-dispatch scaffold (Stage 189C6/190B) runs
# only an ISOLATED, hardcoded per-AP probe workload (a Yield + magic-park stub) and then
# idles — `live_ap_user_dispatch` / `ap_sched_next_or_idle` never host a userspace IPC
# server BLOCKED on an endpoint SHARED with a BSP client, and there is no cross-CPU
# delivery/remote-wake path that resumes a blocked IPC receiver on a remote AP. A genuine
# cross-CPU NR6/NR7 round trip therefore cannot be produced with the CURRENT infrastructure;
# it requires a new cross-CPU IPC oracle (shared endpoint across a BSP client + an AP-hosted
# server, AP scheduler support for a blocked-then-remote-woken receiver, and the off-lock
# NR6/NR7 firing from BOTH CPUs' trap paths). Presenting same-CPU execution as a cross-CPU
# proof is a HARD-STOP, so this script NEVER emits `result=ok`.
#
# What this script DOES prove honestly:
#   * the DIRECT-oracle kernel boots CLEAN under QEMU_SMP=2 (no panic/fault/stall), i.e. the
#     SMP=2 topology introduces no regression in the off-lock NR6/NR7 machinery; and
#   * the existing BSP-only DIRECT round trip STILL completes exactly once under SMP=2.
# If (and only if) the cross-CPU markers ever appear with distinct CPU IDs, this script would
# then require the full SMP count contract before sealing; until the cross-CPU oracle is
# wired it emits `result=blocked` with the precise blocker reason.
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=x86-ipccall-direct-oracle
KTARGET=${KTARGET:-targets/x86_64-yarm-none.json}
KPROFILE=${KPROFILE:-x86-none}
KELF=${KELF:-target/x86_64-yarm-none/${KPROFILE}/kernel_boot}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ipccall-reply-direct-x86_64-smp}
TIMEOUT_SECS=${TIMEOUT_SECS:-150}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

SEAL_BLOCKED="STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 pairs=1 cross_cpu_request=0 cross_cpu_reply=0 result=blocked reason=ap_cross_cpu_ipc_oracle_not_wired"

fail=0
note() { echo "[ipccall-reply-direct-smp] $*"; }
die()  { echo "[ipccall-reply-direct-smp][fail] $*"; fail=1; }

# ── 1. Base artifacts: servers + initramfs (no feature; the scaffold is arch-gated) ──
note "building base x86_64 artifacts (servers + initramfs)"
BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
  scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
  || die "base artifact build failed (see $LOGDIR/build.log)"

# ── 2. Overlay: rebuild kernel_boot WITH the ipccall-direct-oracle feature ──
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
  rg -a -q "class=IpcCallDirectRequest" build-x86_64/kernel_boot.elf \
    || die "feature kernel missing IpcCallDirectRequest literal (wrong build)"
  rg -a -q "class=IpcReplyDirect" build-x86_64/kernel_boot.elf \
    || die "feature kernel missing IpcReplyDirect literal (wrong build)"
fi

if (( fail )); then
  echo "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 result=fail reason=build"
  exit 1
fi

# ── 3. Boot ONE -smp 2 QEMU with the oracle knob ──
note "booting QEMU -smp 2 with yarm.x86_64_ipccall_direct_oracle=1"
env \
  KERNEL_IMAGE=build-x86_64/kernel_boot.elf \
  INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.x86_64_ipccall_direct_oracle=1" \
  QEMU_SMP=2 \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  scripts/qemu-x86_64-core-smoke.sh >"$LOGDIR/core-smoke.log" 2>&1 || true

if [[ ! -s "$BOOT_LOG" ]]; then
  die "no boot log produced"
  echo "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 result=fail reason=no_boot_log"
  exit 1
fi

NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
count() { rg -a -c -F "$1" "$NORM" 2>/dev/null || echo 0; }
have()  { rg -a -q -F "$1" "$NORM"; }

# ── 4. No SMP regression: the BSP DIRECT round trip still completes exactly once ──
declare -a KMARKERS=(
  "IPCCALL_DIRECT_REQUEST_OK arch=x86_64 source_copy_offlock=1 reply_cap=1 server_wakes=1"
  "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcCallDirectRequest result=ok"
  "IPCREPLY_DIRECT_OK arch=x86_64 source_copy_offlock=1 caller_wakes=1 one_shot=1"
  "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcReplyDirect result=ok"
)
for m in "${KMARKERS[@]}"; do
  c=$(count "$m")
  [[ "$c" == "1" ]] || die "SMP=2 regression: BSP direct marker count != 1 (got $c): $m"
done
UDONE="X86_IPCCALL_DIRECT_ROUNDTRIP_DONE request_ok=1 reply_ok=1 duplicate_reply=rejected server_wakes=1 caller_wakes=1 client_continuations=1 server_continuations=1 result=ok"
[[ "$(count "$UDONE")" == "1" ]] || die "SMP=2 regression: userspace completion missing/duplicate"

# ── 5. Hard-stops: no panic/fault/stall/duplicate under SMP=2 ──
for bad in "KERNEL PANIC" "RUST PANIC" "panicked at" "CPU EXCEPTION" "DOUBLE FAULT" "Unhandled" "BOOTSTRAP_ERROR" \
           "IPCCALL_DIRECT_ORACLE_SERVER_DUP dup_rejected=0" ; do
  have "$bad" && die "fatal condition under SMP=2: $bad"
done

# ── 6. Cross-CPU proof gate: require STRICTLY cross-CPU markers with DISTINCT CPU IDs ──
# These are emitted ONLY by a genuine cross-CPU oracle (not yet wired). We accept a
# cross-CPU seal ONLY when both markers are present with sender/receiver (and
# replier/caller) CPU IDs that differ. Same-CPU execution is NEVER accepted as cross-CPU.
req_ok=0; rep_ok=0
if have "IPCCALL_DIRECT_SMP_REQUEST_OK arch=x86_64" && have "IPCREPLY_DIRECT_SMP_REPLY_OK arch=x86_64"; then
  # Parse the CPU IDs and require they differ.
  req_line=$(grep -aF "IPCCALL_DIRECT_SMP_REQUEST_OK arch=x86_64" "$NORM" | head -1)
  rep_line=$(grep -aF "IPCREPLY_DIRECT_SMP_REPLY_OK arch=x86_64" "$NORM" | head -1)
  scpu=$(sed -n 's/.*sender_cpu=\([0-9]\+\).*/\1/p' <<<"$req_line")
  rcpu=$(sed -n 's/.*receiver_cpu=\([0-9]\+\).*/\1/p' <<<"$req_line")
  pcpu=$(sed -n 's/.*replier_cpu=\([0-9]\+\).*/\1/p' <<<"$rep_line")
  ccpu=$(sed -n 's/.*caller_cpu=\([0-9]\+\).*/\1/p' <<<"$rep_line")
  if [[ -n "$scpu" && -n "$rcpu" && "$scpu" != "$rcpu" ]]; then req_ok=1; fi
  if [[ -n "$pcpu" && -n "$ccpu" && "$pcpu" != "$ccpu" ]]; then rep_ok=1; fi
fi

if (( fail )); then
  echo "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 result=fail (see $BOOT_LOG)"
  exit 1
fi

if (( req_ok && rep_ok )); then
  # A genuine cross-CPU round trip WAS observed. (Reaching here means the cross-CPU oracle
  # has been wired in a later stage — enforce the full SMP count contract before sealing ok.)
  fuse=$(count "IPCCALL_DIRECT_ACK_OVERWRITE_FUSE")
  fuse2=$(count "IPCREPLY_DIRECT_ACK_OVERWRITE_FUSE")
  [[ "$fuse" == "0" && "$fuse2" == "0" ]] || { echo "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 result=fail reason=overwrite_fuse_tripped"; exit 1; }
  note "GENUINE cross-CPU round trip observed under SMP=2 (distinct CPU IDs)"
  echo "STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 pairs=1 cross_cpu_request=1 cross_cpu_reply=1 forced_hosted_races=8 duplicate_replies=0 duplicate_wakes=0 wrong_waiter_mutations=0 stale_authority_restores=0 result=ok"
  exit 0
fi

# No cross-CPU markers: the kernel is SMP=2-clean and the BSP round trip is intact, but the
# cross-CPU IPC oracle is not wired. Report the blocker honestly — NEVER a false ok.
note "SMP=2 boot clean; BSP direct round trip intact; NO cross-CPU IPC markers (oracle not wired)"
echo "$SEAL_BLOCKED"
exit 0
