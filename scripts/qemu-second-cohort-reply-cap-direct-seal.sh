#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198C2B — SECOND-COHORT REPLY-CAP DIRECT-DELIVERY CROSS-ARCH SEAL.
#
# Runs the reply-cap DIRECT one-shot live oracle against FRESH artifacts on x86_64,
# AArch64 and RISC-V and asserts the single reply-cap-direct cell is LIVE on all three:
#
#   1. Direct reply-cap transfer to an already recv-v2-blocked receiver → class=IpcSendReplyCap
#
# For each arch the cell is "live" when ALL of the following appear in a fresh QEMU boot:
#   GLOBAL_LOCK_RETIRE_CLASS_DONE arch=<arch> class=IpcSendReplyCap result=ok
#   IPCSEND_REPLY_CAP_OBJECT_IDENTITY_OK arch=<arch> object_match=1 target_match=1 reply_metadata=1
#   IPCSEND_REPLY_CAP_RIGHTS_OK arch=<arch> delegation=1 destination_rights_ok=1 source_cap_present=1
#   IPCSEND_REPLY_CAP_ONE_SHOT_OK arch=<arch> first_reply=ok second_reply=rejected caller_wakes=1 duplicate_reply=0
#   IPC_SEND_REPLY_CAP_ORACLE_CHILD_RECV_OK payload_match=1 reply_cap=1 reply_is_fresh=1
#
# NO reply-cap ENQUEUE / shared-region / D2 path is exercised (the FORBIDDEN set below
# rejects them). Emits the final seal markers from the per-arch logs — no kernel markers
# were added to fabricate the matrix. Exits non-zero on any missing proof or forbidden marker.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/second-cohort-reply-cap-direct-seal}
mkdir -p "$LOGDIR"
STRICT=${QEMU_SMOKE_STRICT:-1}
fail=0

note() { echo "[seal] $*"; }
die()  { echo "[seal][fail] $*"; fail=1; }

# ── 1. Require fresh artifacts + record hashes/mtimes ──
note "artifact hashes / mtimes:"
for f in build-x86_64/kernel_boot.elf build-aarch64/yarm-aarch64.bin build-riscv64/yarm-riscv64.bin; do
  if [[ ! -f "$f" ]]; then die "missing artifact: $f (build fresh first)"; continue; fi
  printf '  %s  %s  %s\n' "$(sha256sum "$f" | cut -d' ' -f1)" "$(stat -c '%y' "$f" | cut -d'.' -f1)" "$f"
done
(( fail )) && { echo "SECOND_COHORT_REPLY_CAP_DIRECT_SEAL arches=3 classes=1 result=fail reason=missing_artifacts"; exit 1; }

# ── 2. Run the reply-cap direct one-shot oracle per arch, one log per arch ──
run() { # run <logfile> <arch>
  local log="$1" arch="$2"
  note "run: reply-cap direct oracle ($arch) (log=$log)"
  env QEMU_SMOKE_STRICT="$STRICT" YARM_IPC_SEND_REPLY_CAP_ORACLE=1 \
    scripts/qemu-ipc-recv-v2-oracle-smoke.sh "$arch" >"$log" 2>&1 || true
}

for arch in x86_64 aarch64 riscv64; do
  run "$LOGDIR/${arch}_replycap.log" "$arch"
done

# ── 3. Reject forbidden markers across every log ──
FORBIDDEN='IPC_SEND_REPLY_CAP_BOUNDARY_SPLIT_FAIL|DISPATCH_POST_WORK_FAIL kind=blocked_waiter_reply_cap|class=IpcSendReplyCapEnqueue|class=IpcSendSharedRegion|IPC_SEND_REPLY_CAP_ORACLE_ONE_SHOT_FAIL|RISCV_TRAP_HANDLE_FAILED|RISCV_TRAP_UNHANDLED|reason=trap_from_s_mode|FATAL|!BN'
for log in "$LOGDIR"/*_replycap.log; do
  [[ -f "$log" ]] || continue
  if rg -a -n "$FORBIDDEN" "$log" >/dev/null 2>&1; then
    die "forbidden marker in $(basename "$log"): $(rg -a -oN "$FORBIDDEN" "$log" | head -1)"
  fi
done

# ── 4. Per-arch seal: all five live proofs present ──
live_cells=0
seal_reply_cap() { # <arch> <logfile>
  local arch="$1" log="$2"
  local retire="GLOBAL_LOCK_RETIRE_CLASS_DONE arch=${arch} class=IpcSendReplyCap result=ok"
  local ident="IPCSEND_REPLY_CAP_OBJECT_IDENTITY_OK arch=${arch} object_match=1 target_match=1 reply_metadata=1"
  local rights="IPCSEND_REPLY_CAP_RIGHTS_OK arch=${arch} delegation=1 destination_rights_ok=1 source_cap_present=1"
  local oneshot="IPCSEND_REPLY_CAP_ONE_SHOT_OK arch=${arch} first_reply=ok second_reply=rejected caller_wakes=1 duplicate_reply=0"
  local fresh="IPC_SEND_REPLY_CAP_ORACLE_CHILD_RECV_OK payload_match=1 reply_cap=1 reply_is_fresh=1"
  local ok=1
  for m in "$retire" "$ident" "$rights" "$oneshot" "$fresh"; do
    rg -a -N "$m" "$log" >/dev/null 2>&1 || { ok=0; die "arch=${arch} missing: ${m}"; }
  done
  if [[ "$ok" -eq 1 ]]; then
    echo "SECOND_COHORT_REPLY_CAP_DIRECT_SEAL arch=${arch} class=IpcSendReplyCap result=ok proof=live one_shot=attested"
    live_cells=$((live_cells+1)); return 0
  fi
  echo "SECOND_COHORT_REPLY_CAP_DIRECT_SEAL arch=${arch} class=IpcSendReplyCap result=MISSING"
  return 1
}

echo "── second-cohort reply-cap direct seal matrix ──"
for arch in x86_64 aarch64 riscv64; do
  seal_reply_cap "$arch" "$LOGDIR/${arch}_replycap.log" || true
done

# ── 5. Final cross-architecture seal (require all 3 cells live) ──
if [[ $live_cells -eq 3 && $fail -eq 0 ]]; then
  echo "SECOND_COHORT_REPLY_CAP_DIRECT_SEAL arches=3 classes=1 live_cells=3 result=ok"
  exit 0
fi
echo "SECOND_COHORT_REPLY_CAP_DIRECT_SEAL arches=3 classes=1 live_cells=${live_cells} result=fail"
exit 1
