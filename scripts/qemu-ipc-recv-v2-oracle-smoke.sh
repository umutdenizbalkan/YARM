#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 156 — IPC recv/reply/transfer/split delivery smoke oracle.
#
# Boots the per-arch core smoke (which itself warns+skips gracefully when QEMU
# or the build artifacts are unavailable) and then verifies the Stage 156 IPC
# oracle markers in the captured boot log. The purpose is byte-identical
# regression proof BEFORE/AFTER any future stateful cap-boundary re-home into
# syscall/ipc_recv_core.rs:
#
#   1. Run with no baseline to produce a marker-set snapshot:
#        scripts/qemu-ipc-recv-v2-oracle-smoke.sh x86_64
#        cp ipc-oracle-markers-x86_64.txt baseline-x86_64.txt
#   2. After the re-home, re-run with the baseline to fail on any regression:
#        ORACLE_BASELINE=baseline-x86_64.txt \
#          scripts/qemu-ipc-recv-v2-oracle-smoke.sh x86_64
#
# Exit codes:
#   0  — QEMU/artifacts unavailable (skipped), or oracle passed.
#   1  — a fatal IPC marker appeared, a required delivery marker was missing,
#        or a baseline marker regressed (only when QEMU actually ran).
#
# Env:
#   ARCH               x86_64 | aarch64 | riscv64   (default: $1 or x86_64)
#   QEMU_SMOKE_STRICT  1 to fail (not skip) when QEMU/artifacts are missing
#   ORACLE_BASELINE    path to a prior snapshot to diff against (regression gate)
#   ORACLE_SNAPSHOT    output snapshot path (default: ipc-oracle-markers-$ARCH.txt)

set -euo pipefail
HERE="$(dirname "$0")"
source "$HERE/qemu-smoke-common.sh"

ARCH="${1:-${ARCH:-x86_64}}"
QEMU_SMOKE_STRICT="${QEMU_SMOKE_STRICT:-0}"

case "$ARCH" in
  x86_64)  CORE_SMOKE="$HERE/qemu-x86_64-core-smoke.sh";  QEMU_BIN="qemu-system-x86_64" ;;
  aarch64) CORE_SMOKE="$HERE/qemu-aarch64-core-smoke.sh"; QEMU_BIN="qemu-system-aarch64" ;;
  riscv64) CORE_SMOKE="$HERE/qemu-riscv64-core-smoke.sh"; QEMU_BIN="qemu-system-riscv64" ;;
  *) echo "[err] unknown ARCH: $ARCH (expected x86_64|aarch64|riscv64)"; exit 1 ;;
esac

# Stage 198B1 Part B: per-invocation scratch dir so concurrent same-arch oracle
# runs (e.g. the plain and ordinary-cap seals both booting x86_64) never share or
# overwrite the CWD serial/analysis logs. Keyed by ORACLE_RUN_ID (default: PID),
# so every invocation is isolated. The seal runners read this wrapper's STDOUT,
# not these files, so relocating them is transparent to callers.
ORACLE_RUN_ID="${ORACLE_RUN_ID:-$$}"
ORACLE_SCRATCH_DIR="${ORACLE_SCRATCH_DIR:-${TMPDIR:-/tmp}/yarm-oracle-${ARCH}-${ORACLE_RUN_ID}}"
mkdir -p "$ORACLE_SCRATCH_DIR"
CORE_LOG="$ORACLE_SCRATCH_DIR/qemu-${ARCH}-core.log"
ORACLE_SNAPSHOT="${ORACLE_SNAPSHOT:-$ORACLE_SCRATCH_DIR/ipc-oracle-markers-$ARCH.txt}"

# Oracle coverage mode (Stage 157):
#   basic    (default) — prove >=1 recv-v2 meta delivery (Stage 156 contract,
#                         unchanged). reply/transfer/rollback/wake only recorded.
#   extended           — additionally require the reply-cap one-shot and
#                         transfer-cap materialize markers, which now fire on the
#                         LIVE D1/D5 split path that every spawn cycle drives.
ORACLE_MODE="${ORACLE_MODE:-basic}"

# Stage 170 (IPC-FINAL): a single strict, repeatable acceptance profile that
# freezes the accepted recv-v2 IPC surface. IPC_FINAL=1 enables ALL proof
# workloads (queued-split + rollback + sender-wake) and extended mode (reply-cap
# + transfer-cap), then hard-requires the full accepted marker set with
# line-start anchoring for the sender-wake ORDER marker and a strict failure gate
# (see the IPC_FINAL block near the end). It is NOT a new behavior stage — it
# only tightens the acceptance checks.
IPC_FINAL="${IPC_FINAL:-0}"
if [[ "$IPC_FINAL" == "1" ]]; then
  echo "[info] ipc-oracle: IPC_FINAL=1 — Stage 170 strict IPC-FINAL acceptance profile"
  ORACLE_MODE="extended"
  YARM_IPC_RECV_PROOF_QUEUED_SPLIT=1
  YARM_IPC_RECV_PROOF_ROLLBACK=1
  YARM_IPC_RECV_PROOF_SENDER_WAKE=1
fi

# Stage 159BC/D — userspace IPC recv-v2 oracle proof workload (default-off).
# When the kernel is booted with `yarm.ipc_recv_proof=1`, the init control-plane
# runs a deterministic loopback workload (send-to-self enqueue + recv-from-self
# drain) that drives specific kernel recv-v2 delivery markers. These per-subtest
# requirements are OFF by default and enabled independently; a requirement is
# only checked when its knob is set (the boot must actually have been launched
# with the proof knob for them to appear). Each pairs a userspace SEQUENCE marker
# (the workload observed the expected *syscall return*, NOT the kernel path) with
# the kernel delivery marker that is the authoritative proof.
YARM_IPC_RECV_PROOF_QUEUED_SPLIT="${YARM_IPC_RECV_PROOF_QUEUED_SPLIT:-0}"
YARM_IPC_RECV_PROOF_ROLLBACK="${YARM_IPC_RECV_PROOF_ROLLBACK:-0}"
YARM_IPC_RECV_PROOF_SENDER_WAKE="${YARM_IPC_RECV_PROOF_SENDER_WAKE:-0}"
# Stage 193B — IpcSend-plain LIVE oracle proof (default-off). When set, boots with
# yarm.ipc_recv_proof=1 + yarm.ipc_send_plain_oracle=1 and hard-requires the 193A
# IpcSendPlain boundary-split markers to appear LIVE (fired by the plain-send
# oracle workload). Mutually exclusive with the sender-wake proof.
YARM_IPC_SEND_PLAIN_ORACLE="${YARM_IPC_SEND_PLAIN_ORACLE:-0}"
# Stage 193C — IpcSend ordinary cap-transfer LIVE oracle proof (default-off). When
# set, boots with yarm.ipc_recv_proof=1 + yarm.ipc_send_cap_oracle=1 and hard-requires
# the 193C IpcSendOrdinaryCap boundary-split markers to appear LIVE (fired by the
# cap-transfer oracle workload). Mutually exclusive with plain oracle + sender-wake.
YARM_IPC_SEND_CAP_ORACLE="${YARM_IPC_SEND_CAP_ORACLE:-0}"
# Stage 193D — IpcSend reply-cap transfer LIVE oracle proof (default-off). When set,
# boots with yarm.ipc_recv_proof=1 + yarm.ipc_send_reply_cap_oracle=1 and hard-requires
# the 193D IpcSendReplyCap boundary-split markers to appear LIVE. Mutually exclusive with
# the plain / ordinary-cap oracles + sender-wake.
YARM_IPC_SEND_REPLY_CAP_ORACLE="${YARM_IPC_SEND_REPLY_CAP_ORACLE:-0}"
# Stage 193E — IpcSend plain no-waiter enqueue LIVE oracle proof (default-off). When set,
# boots with yarm.ipc_recv_proof=1 + yarm.ipc_send_enqueue_oracle=1 and hard-requires the
# 193E IpcSendPlainEnqueue boundary-split markers to appear LIVE.
YARM_IPC_SEND_ENQUEUE_ORACLE="${YARM_IPC_SEND_ENQUEUE_ORACLE:-0}"
# Stage 193F — IpcSend ordinary-cap no-waiter enqueue LIVE oracle proof (default-off). When
# set, boots with yarm.ipc_recv_proof=1 + yarm.ipc_send_cap_enqueue_oracle=1 and hard-requires
# the 193F IpcSendOrdinaryCapEnqueue boundary-split markers to appear LIVE.
YARM_IPC_SEND_CAP_ENQUEUE_ORACLE="${YARM_IPC_SEND_CAP_ENQUEUE_ORACLE:-0}"

# Whenever any proof requirement is enabled, the kernel MUST be booted with
# yarm.ipc_recv_proof=1 or the workload never runs. Export IPC_RECV_PROOF=1 so the
# per-arch core smoke appends the boot knob to the kernel cmdline. Basic mode
# (no proof env vars) leaves this unset and the cmdline unchanged.
if [[ "$YARM_IPC_RECV_PROOF_QUEUED_SPLIT" == "1" \
   || "$YARM_IPC_RECV_PROOF_ROLLBACK" == "1" \
   || "$YARM_IPC_RECV_PROOF_SENDER_WAKE" == "1" \
   || "$YARM_IPC_SEND_PLAIN_ORACLE" == "1" \
   || "$YARM_IPC_SEND_CAP_ORACLE" == "1" \
   || "$YARM_IPC_SEND_REPLY_CAP_ORACLE" == "1" \
   || "$YARM_IPC_SEND_ENQUEUE_ORACLE" == "1" \
   || "$YARM_IPC_SEND_CAP_ENQUEUE_ORACLE" == "1" ]]; then
  export IPC_RECV_PROOF=1
  echo "[info] ipc-oracle: proof env set -> booting kernel with yarm.ipc_recv_proof=1"
fi
# Stage 193B — the send-plain oracle is isolated behind its own boot sub-knob
# yarm.ipc_send_plain_oracle=1, which gates the receiver-blocked coordination hook
# AND the plain-send oracle workload (and the coordination endpoint provisioning
# into init slot 14). Export it so the core smoke appends the sub-knob only when the
# send-plain oracle is being proven.
if [[ "$YARM_IPC_SEND_PLAIN_ORACLE" == "1" ]]; then
  export IPC_SEND_PLAIN_ORACLE=1
  echo "[info] ipc-oracle: send-plain oracle env set -> booting kernel with yarm.ipc_send_plain_oracle=1"
fi
# Stage 193C — the send ordinary-cap oracle is isolated behind its own boot sub-knob
# yarm.ipc_send_cap_oracle=1 (gates the coordination hook + the cap-transfer oracle
# workload + coord provisioning into init slot 13). Export it so the core smoke
# appends the sub-knob only when the cap oracle is being proven.
if [[ "$YARM_IPC_SEND_CAP_ORACLE" == "1" ]]; then
  export IPC_SEND_CAP_ORACLE=1
  echo "[info] ipc-oracle: send-cap oracle env set -> booting kernel with yarm.ipc_send_cap_oracle=1"
fi
# Stage 193D — the send reply-cap oracle is isolated behind its own boot sub-knob
# yarm.ipc_send_reply_cap_oracle=1 (gates the coordination hook + the reply-cap oracle
# workload + the transferable reply-cap provisioning). Export it so the core smoke
# appends the sub-knob only when the reply-cap oracle is being proven.
if [[ "$YARM_IPC_SEND_REPLY_CAP_ORACLE" == "1" ]]; then
  export IPC_SEND_REPLY_CAP_ORACLE=1
  echo "[info] ipc-oracle: send-reply-cap oracle env set -> booting kernel with yarm.ipc_send_reply_cap_oracle=1"
fi
# Stage 193E — the send enqueue oracle is isolated behind its own boot sub-knob
# yarm.ipc_send_enqueue_oracle=1 (gates the plain no-waiter enqueue oracle workload).
# Export it so the core smoke appends the sub-knob only when the enqueue oracle is proven.
if [[ "$YARM_IPC_SEND_ENQUEUE_ORACLE" == "1" ]]; then
  export IPC_SEND_ENQUEUE_ORACLE=1
  echo "[info] ipc-oracle: send-enqueue oracle env set -> booting kernel with yarm.ipc_send_enqueue_oracle=1"
fi
# Stage 193F — the send ordinary-cap enqueue oracle is isolated behind its own boot sub-knob
# yarm.ipc_send_cap_enqueue_oracle=1. Export it so the core smoke appends the sub-knob only
# when the cap-enqueue oracle is proven.
if [[ "$YARM_IPC_SEND_CAP_ENQUEUE_ORACLE" == "1" ]]; then
  export IPC_SEND_CAP_ENQUEUE_ORACLE=1
  echo "[info] ipc-oracle: send-cap-enqueue oracle env set -> booting kernel with yarm.ipc_send_cap_enqueue_oracle=1"
fi
# Stage 163 — the sender-wake proof is isolated behind its own boot sub-knob
# yarm.ipc_recv_proof_sender_wake=1, which gates BOTH the kernel proof-gated
# waiter-present coordination hook AND the userspace deterministic sender-wake
# workload (and the provisioning of the second coordination endpoint E2). The
# queued-split + rollback proofs deliberately do NOT set it, so their boots are
# byte-for-byte unchanged. Export it so the per-arch core smoke appends the
# sub-knob to the kernel cmdline only when sender-wake is being proven.
if [[ "$YARM_IPC_RECV_PROOF_SENDER_WAKE" == "1" ]]; then
  export IPC_RECV_PROOF_SENDER_WAKE=1
  echo "[info] ipc-oracle: sender-wake proof env set -> booting kernel with yarm.ipc_recv_proof_sender_wake=1"
fi

# Healthy-delivery success markers (Stage 156). Not all fire on every boot, so
# only the "at least one recv-v2 meta delivered" invariant is hard-required.
# The IPC_RECV_PROOF_*_SEQUENCE_DONE markers (Stage 159BC/D) are emitted by the
# userspace proof workload only on the expected syscall return, and recorded here.
ORACLE_MARKERS=(
  "IPC_RECV_V2_META_BLOCKED_WAITER_OK"
  "IPC_RECV_V2_META_IMMEDIATE_OK"
  "IPC_RECV_V2_META_QUEUED_SPLIT_OK"
  "IPC_REPLY_CAP_ONESHOT_OK"
  "IPC_TRANSFER_CAP_MATERIALIZE_OK"
  "IPC_RECV_V2_ROLLBACK_OK"
  "IPC_RECV_V2_SENDER_WAKE_ORDER_OK"
  "IPC_RECV_PROOF_QUEUED_SPLIT_SEQUENCE_DONE"
  "IPC_RECV_PROOF_ROLLBACK_SEQUENCE_DONE"
  "IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE"
)

# At least one recv-v2 meta delivery marker must appear on a healthy boot.
REQUIRED_ANY=(
  "IPC_RECV_V2_META_BLOCKED_WAITER_OK"
  "IPC_RECV_V2_META_IMMEDIATE_OK"
  "IPC_RECV_V2_META_QUEUED_SPLIT_OK"
)

# Extended mode: cap-transfer and reply-cap one-shot delivery must both be
# proven. The init control-plane spawn workload (spawn_v5_cap -> ipc_call with a
# reply cap + delegated send caps) drives both on the live D1/D5 split path every
# boot, so these are hard-required once ORACLE_MODE=extended.
#
# IPC_RECV_V2_ROLLBACK_OK is a *fault*-path marker (recv-v2 meta user-copy fault)
# and is correctly absent on a healthy boot; IPC_RECV_V2_SENDER_WAKE_ORDER_OK is
# contention-dependent. Both stay recorded-only here and are covered by the
# hosted seam tests; deterministic QEMU triggering is left to a fault/contention
# workload (see doc/IPC_RECV_V2_ORACLE.md).
EXTENDED_REQUIRED=(
  "IPC_REPLY_CAP_ONESHOT_OK"
  "IPC_TRANSFER_CAP_MATERIALIZE_OK"
)

# Fatal IPC regressions — their presence fails the oracle.
FATAL_MARKERS=(
  "IPC_RECV_CAP_MATERIALIZE_FAILED"
  "IPC_RECV_BLOCKED_COMPLETE_FAILED"
  "IPC_RECV_REPLY_CAP_MATERIALIZE_FAIL"
)

# Skip cleanly if QEMU is unavailable (matches the core-smoke convention).
require_qemu_or_warn "$QEMU_BIN" "$QEMU_SMOKE_STRICT"

# Delegate the boot to the existing per-arch core smoke. It captures the QEMU
# serial output to $LOGFILE (=$CORE_LOG) AND tees it to ITS stdout. Stage 163B:
# the previous oracle analyzed $CORE_LOG, which could disagree with the actually
# captured run output (stale file, or a path mismatch). To remove ALL ambiguity we
# capture the core-smoke's COMBINED stdout/stderr into one explicit oracle run log
# and then analyze a single normalized analysis log (that captured output UNION the
# raw serial $CORE_LOG, so the raw markers are present no matter which path carries
# them). Every marker check below reads ONLY $ANALYSIS_LOG via one helper.
export LOGFILE="$CORE_LOG"
# Stage 198B1 Part B: keep the analysis/run logs in the per-invocation scratch
# dir (isolated across concurrent same-arch runs).
CORE_RUN_LOG="$ORACLE_SCRATCH_DIR/ipc-oracle-core-stdout-$ARCH.log"
ANALYSIS_LOG="$ORACLE_SCRATCH_DIR/ipc-oracle-run-$ARCH.log"
rm -f "$CORE_RUN_LOG" "$ANALYSIS_LOG"
echo "[info] ipc-oracle: booting $ARCH via $CORE_SMOKE (serial log: $CORE_LOG, run log: $CORE_RUN_LOG)"
set +e
QEMU_SMOKE_STRICT="$QEMU_SMOKE_STRICT" LOGFILE="$CORE_LOG" "$CORE_SMOKE" 2>&1 | tee "$CORE_RUN_LOG"
CORE_STATUS=${PIPESTATUS[0]}
set -e

# Build the single analysis log: the captured core-smoke run output first, then the
# raw serial $CORE_LOG appended, both with CR normalized to LF so every marker that
# reached EITHER sink is visible to one consistent text scan.
: >"$ANALYSIS_LOG"
[[ -f "$CORE_RUN_LOG" ]] && tr '\r' '\n' <"$CORE_RUN_LOG" >>"$ANALYSIS_LOG"
[[ -f "$CORE_LOG" ]] && tr '\r' '\n' <"$CORE_LOG" >>"$ANALYSIS_LOG"

if [[ ! -s "$ANALYSIS_LOG" ]]; then
  echo "[warn] ipc-oracle: no boot output produced (QEMU/artifacts likely unavailable); skipping"
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

if [[ "$CORE_STATUS" -ne 0 ]]; then
  echo "[err] ipc-oracle: core smoke for $ARCH failed (status $CORE_STATUS)"
  exit 1
fi

# Single source of truth for every marker check: fixed-string match against the one
# analysis log. Using `-F` (literal) avoids any regex/word-boundary surprises.
marker_present() {
  # $1 = literal marker, $2 = file (defaults to $ANALYSIS_LOG)
  local marker="$1" file="${2:-$ANALYSIS_LOG}"
  [[ -s "$file" ]] || return 1
  rg -F -q -a "$marker" "$file"
}

# Stage 170 (IPC-FINAL): line-start anchored presence. Only a marker that begins
# a serial line counts — an informational "[info] absent: MARKER" echo or an
# "[ok] ... present: MARKER" line (which have a "[...]" prefix) is NOT a match,
# so a strict check can never be satisfied by an absence report or a wrapper echo.
marker_present_linestart() {
  # $1 = literal marker (no regex metachars in our marker names), $2 = file
  local marker="$1" file="${2:-$ANALYSIS_LOG}"
  [[ -s "$file" ]] || return 1
  rg -q -a -e "^${marker}" "$file"
}

ANALYSIS_BYTES=$(wc -c <"$ANALYSIS_LOG" 2>/dev/null | tr -d ' ')
echo "[info] ipc-oracle: analyzing markers in $ANALYSIS_LOG (bytes=${ANALYSIS_BYTES:-0}) — single source for initial scan + proof_require"
: >"$ORACLE_SNAPSHOT"
present=()
for m in "${ORACLE_MARKERS[@]}"; do
  if marker_present "$m"; then
    echo "[ok]   present: $m"
    echo "$m" >>"$ORACLE_SNAPSHOT"
    present+=("$m")
  else
    echo "[info] absent : $m"
  fi
done
sort -u -o "$ORACLE_SNAPSHOT" "$ORACLE_SNAPSHOT"
echo "[info] ipc-oracle: marker snapshot written to $ORACLE_SNAPSHOT"

rc=0

# Fatal markers must not appear.
for f in "${FATAL_MARKERS[@]}"; do
  if marker_present "$f"; then
    echo "[err] ipc-oracle: fatal IPC marker present: $f"
    rc=1
  fi
done

# Stage 182 (REMOVE-FALLBACKS): the graduated seams are the ONLY x86_64 production path.
# No old global-lock fallback, no emergency opt-out, and no unexpected in-lock dispatch
# may ever appear — assert their ABSENCE (the fallback was removed, not disabled).
#
# Stage 183 (SMP-LIVE): under x86_64 -smp >1, additionally forbid the SMP error markers
# (TLB remote-ACK timeout, lost/duplicate wake, online-accounting corruption). These are
# only emitted on a real SMP invariant violation, so their ABSENCE is the SMP proof.
for f in \
  "UNLOCK_GRADUATED_DEFERRED reason=emergency_optout" \
  "UNLOCK_GRADUATED_FALLBACK path=" \
  "UNLOCK_GRADUATED_UNEXPECTED_INLOCK_DISPATCH" \
  "X86_TLB_REMOTE_ACK_TIMEOUT" \
  "D6_SMP_DUP_WAKE_FAIL" \
  "D6_SMP_LOST_WAKE_FAIL" \
  "X86_SMP_ONLINE_ACCOUNTING_BAD"; do
  if marker_present "$f"; then
    echo "[err] ipc-oracle: REMOVE-FALLBACKS/SMP-LIVE: forbidden fallback/SMP-error marker fired: $f"
    rc=1
  fi
done

# Stage 183.6: under x86_64 -smp >1 the blocking sender-wake workload drives real
# D6 out-of-lock dispatch while APs are scheduler-online. The single-DISPATCHER
# topology (only the BSP dispatches; wake-only APs run no dispatcher) keeps the
# accepted out-of-lock slice live, so the queue-advancing dispatch must NOT fall
# back in-lock — require the D6-SMP-dispatch proof marker and the sender-wake order.
if [[ "${QEMU_SMP:-1}" -gt 1 ]]; then
  for m in \
    "D6_SMP_DISPATCH_OK" \
    "IPC_RECV_V2_SENDER_WAKE_ORDER_OK" \
    "IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE"; do
    if marker_present "$m"; then
      echo "[ok]   183.6 SMP sender-wake marker present: $m"
    else
      echo "[err] ipc-oracle: 183.6: SMP sender-wake marker missing: $m"
      rc=1
    fi
  done
fi

# At least one recv-v2 meta delivery must be proven.
any_required=0
for r in "${REQUIRED_ANY[@]}"; do
  if marker_present "$r"; then
    any_required=1
    break
  fi
done
if [[ "$any_required" -ne 1 ]]; then
  echo "[err] ipc-oracle: no recv-v2 meta delivery marker present (delivery regressed)"
  rc=1
fi

# Extended mode: reply-cap + transfer-cap delivery must both be proven.
if [[ "$ORACLE_MODE" == "extended" ]]; then
  echo "[info] ipc-oracle: extended mode — requiring reply-cap + transfer-cap delivery"
  for r in "${EXTENDED_REQUIRED[@]}"; do
    if marker_present "$r"; then
      echo "[ok]   extended-required present: $r"
    else
      echo "[err] ipc-oracle: extended-required marker absent: $r"
      rc=1
    fi
  done
elif [[ "$ORACLE_MODE" != "basic" ]]; then
  echo "[err] ipc-oracle: unknown ORACLE_MODE='$ORACLE_MODE' (expected basic|extended)"
  rc=1
fi

# Stage 159BC/D — independent userspace proof-workload requirements. Each is only
# enforced when its knob is set (the boot must have used yarm.ipc_recv_proof=1).
#
# A requirement always needs the userspace SEQUENCE marker (the workload ran and
# observed the expected syscall return). The kernel delivery marker is the
# authoritative proof of the *path*; whether it is REQUIRED is arch-dependent:
#
#   * x86_64 — the trap-entry split recv fast path that emits the queued-split /
#     queued-split-rollback kernel markers is exercised; kernel markers REQUIRED.
#   * AArch64 — the proof recv currently falls back to the legacy_full_path
#     (YARM_RECV_CORE_ADAPTER kind=legacy_full_path); the queued-split kernel
#     markers are NOT emitted there. This is a separate AArch64 split-recv
#     routing/parity issue, not a workload defect. The kernel markers are
#     recorded but NOT required on AArch64: their absence is reported as DEFERRED
#     (not a pass, not a failure); if they ever appear, that is reported as PASS.
#   * riscv64 — uses the raw trap path (no split dispatch); same DEFERRED policy.
case "$ARCH" in
  x86_64) PROOF_KERNEL_REQUIRED=1 ;;
  *)      PROOF_KERNEL_REQUIRED=0 ;;
esac

proof_require() {
  # $1 = human label, $2 = userspace SEQUENCE marker, $3 = kernel marker
  local label="$1" seq_marker="$2" kern_marker="$3"
  local have_seq=0 have_kern=0
  # Stage 163B: analyze the SAME single analysis log the initial scan used
  # ($ANALYSIS_LOG = captured core-smoke run output UNION raw serial), via the one
  # `marker_present` helper — never a separate file/snapshot/mechanism, so a marker
  # the initial scan found can never be reported absent here.
  if marker_present "$seq_marker"; then
    have_seq=1
  fi
  if marker_present "$kern_marker"; then
    have_kern=1
  fi
  echo "[info] ipc-oracle: proof $label: analyzing $ANALYSIS_LOG (have_seq=$have_seq have_kern=$have_kern)"
  # The userspace sequence marker is always required: the workload must have run
  # and observed the expected syscall return.
  if [[ "$have_seq" -ne 1 ]]; then
    echo "[err] ipc-oracle: proof $label: sequence marker absent ($seq_marker) — workload did not run/observe expected return"
    rc=1
    return
  fi
  if [[ "$have_kern" -eq 1 ]]; then
    echo "[ok]   proof $label: PASS ($seq_marker + $kern_marker)"
  elif [[ "$PROOF_KERNEL_REQUIRED" -eq 1 ]]; then
    echo "[err] ipc-oracle: proof $label: kernel marker absent ($kern_marker) — required on $ARCH"
    rc=1
  else
    echo "[warn] ipc-oracle: proof $label: DEFERRED on $ARCH — sequence present ($seq_marker) but kernel marker $kern_marker absent (split-recv falls back to legacy_full_path; not a pass, not a failure)"
  fi
}

# Stage 181C: sender-wake failure triage. When the sender-wake sequence marker is
# absent, distinguish (a) workload never started, (b) started but FORK failed,
# (c) started but the blocking send failed, (d) started but only the order/seq
# marker is missing. The graduated default-on regression manifests as (b): the
# workload fills the endpoint, reaches fork, and fork returns Internal (err=255).
# Surface the exact fork failure code + the kernel's normalized FORK_COW_FAIL
# reason so the seam is unambiguous instead of the opaque "sequence marker absent".
diagnose_sender_wake() {
  echo "[info] ipc-oracle: sender-wake triage — mode UNLOCK_GRADUATED=${UNLOCK_GRADUATED:-<default:graduated>} D2_RECV_GENUINE=${D2_RECV_GENUINE:-0} D2_SEND_GENUINE=${D2_SEND_GENUINE:-0} D6_GENUINE=${D6_GENUINE:-0}"
  if ! marker_present "IPC_RECV_PROOF_SENDER_WAKE_BEGIN"; then
    echo "[diag] sender-wake: WORKLOAD ABSENT — no IPC_RECV_PROOF_SENDER_WAKE_BEGIN (the userspace workload never started)"
    return
  fi
  if marker_present "IPC_RECV_PROOF_SENDER_WAKE_FORK_FAILED"; then
    # Extract the last fork-failed line and its decoded code/meaning.
    local ff dec reason
    ff=$(rg -N -a "IPC_RECV_PROOF_SENDER_WAKE_FORK_FAILED" "$ANALYSIS_LOG" | tail -n1)
    echo "[diag] sender-wake: WORKLOAD STARTED but FORK FAILED"
    echo "[diag]   $ff"
    # Nearest kernel-side normalized reason (Stage 181C FORK_COW_FAIL) if present.
    if marker_present "FORK_COW_FAIL reason="; then
      reason=$(rg -N -a "FORK_COW_FAIL reason=" "$ANALYSIS_LOG" | tail -n1)
      echo "[diag]   kernel reason: $reason"
    elif marker_present "FORK_PROOF_RETURN_ERR"; then
      reason=$(rg -N -a "FORK_PROOF_RETURN_ERR" "$ANALYSIS_LOG" | tail -n1)
      echo "[diag]   kernel reason: $reason"
    else
      echo "[diag]   kernel reason: (no FORK_COW_FAIL / FORK_PROOF_RETURN_ERR marker — rerun with the sender-wake sub-knob so proof-gated fork markers fire)"
    fi
    # Stage 181C: a cap_full/CapabilityFull register failure is PT-pool/heap exhaustion
    # (the child cnode-slot Vec cannot be allocated), NOT the aggregate slot budget.
    # Surface the pool headroom + per-owner cnode breakdown + any graduated pool leak.
    if marker_present "FORK_PROOF_ALLOC_CHILD_CAPACITY"; then
      echo "[diag]   $(rg -N -a "FORK_PROOF_ALLOC_CHILD_CAPACITY" "$ANALYSIS_LOG" | tail -n1)"
    fi
    if marker_present "FORK_PROOF_ALLOC_CHILD_POOL"; then
      echo "[diag]   $(rg -N -a "FORK_PROOF_ALLOC_CHILD_POOL" "$ANALYSIS_LOG" | tail -n1)"
    fi
    if marker_present "UNLOCK_GRADUATED_POOL_LEAK"; then
      echo "[diag]   graduated one-shot proof LEAKED PT-pool frames:"
      echo "[diag]   $(rg -N -a "UNLOCK_GRADUATED_POOL_LEAK|UNLOCK_GRADUATED_POOL_BEFORE|UNLOCK_GRADUATED_POOL_AFTER" "$ANALYSIS_LOG" | tail -n3)"
      echo "[diag]   => the graduated one-shot proof itself is the PT-pool leaker."
    fi
    if marker_present "FORK_PROOF_ALLOC_CHILD_CNODE_OWNER"; then
      local n_owners
      n_owners=$(rg -N -c -a "FORK_PROOF_ALLOC_CHILD_CNODE_OWNER" "$ANALYSIS_LOG")
      echo "[diag]   per-owner cnode breakdown ($n_owners owners; showing up to 12):"
      rg -N -a "FORK_PROOF_ALLOC_CHILD_CNODE_OWNER" "$ANALYSIS_LOG" | tail -n12 \
        | sed 's/^/[diag]     /'
    fi
    echo "[diag]   => sender-wake fork failed under the current mode; this is a fork/COW regression, not a plumbing or missing-order-marker issue."
    return
  fi
  if marker_present "IPC_RECV_PROOF_SENDER_WAKE_FORK_BEGIN" \
     && ! marker_present "IPC_RECV_PROOF_SENDER_WAKE_CHILD_ENTRY"; then
    echo "[diag] sender-wake: WORKLOAD STARTED, fork reached but child never entered (no CHILD_ENTRY) — inspect FORK_COW_* / scheduler markers"
    return
  fi
  echo "[diag] sender-wake: WORKLOAD STARTED and forked; sequence/order marker missing — inspect blocking-send + IPC_RECV_V2_SENDER_WAKE_ORDER_OK path"
}

if [[ "$YARM_IPC_RECV_PROOF_QUEUED_SPLIT" == "1" ]]; then
  echo "[info] ipc-oracle: proof queued-split: REQUIRED"
  proof_require "queued-split" "IPC_RECV_PROOF_QUEUED_SPLIT_SEQUENCE_DONE" "IPC_RECV_V2_META_QUEUED_SPLIT_OK"
else
  echo "[info] ipc-oracle: proof queued-split: not required"
fi

if [[ "$YARM_IPC_RECV_PROOF_ROLLBACK" == "1" ]]; then
  echo "[info] ipc-oracle: proof rollback: REQUIRED"
  proof_require "rollback" "IPC_RECV_PROOF_ROLLBACK_SEQUENCE_DONE" "IPC_RECV_V2_ROLLBACK_OK"
else
  echo "[info] ipc-oracle: proof rollback: not required"
fi

if [[ "$YARM_IPC_RECV_PROOF_SENDER_WAKE" == "1" ]]; then
  # Stage 163: sender-wake requires BOTH the kernel delivery marker
  # IPC_RECV_V2_SENDER_WAKE_ORDER_OK and the userspace sequence marker
  # IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE. The pure-userspace race (Stage
  # 161/162 deferral: a workload cannot deterministically create AND observe a
  # blocked sender before the receiver drain on a multi-CPU boot) is closed by a
  # proof-gated kernel coordination hook: when the sub-knob is set, the kernel
  # pushes a one-byte signal onto the second endpoint E2 inside the SAME
  # enqueue_sender_waiter critical section as the waiter enqueue, so the receiver
  # only drains E1 after the sender is provably a waiter — making the handshake
  # deterministic without CPU pinning. The userspace SEQUENCE marker is therefore
  # always required. The kernel SENDER_WAKE_ORDER marker is emitted only on the
  # trap-entry split-recv fast path (apply_split_sender_wake_plan); like the
  # queued-split kernel marker it is REQUIRED on x86_64 and DEFERRED on
  # AArch64/riscv64 (whose proof recv falls back to legacy_full_path) — exactly
  # the per-arch policy proof_require already applies.
  echo "[info] ipc-oracle: proof sender-wake: REQUIRED"
  # Stage 181B: deterministic plumbing pre-check. The sender-wake WORKLOAD only runs
  # if yarm.ipc_recv_proof_sender_wake=1 actually reached the kernel cmdline, which the
  # kernel confirms with `YARM_IPC_RECV_PROOF_SENDER_WAKE_SET enabled=true`. If that
  # marker is ABSENT, the sub-knob never reached the kernel (a runner/oracle plumbing
  # bug) — fail HERE with an unambiguous message instead of the confusing downstream
  # "sequence marker absent / workload did not run". (Requires YARM_IPC_RECV_PROOF_
  # SENDER_WAKE=1 which the oracle exports as IPC_RECV_PROOF_SENDER_WAKE=1 so the core
  # smoke appends the sub-knob.)
  if ! marker_present "YARM_IPC_RECV_PROOF_SENDER_WAKE_SET enabled=true"; then
    echo "[err] ipc-oracle: sender-wake requested but yarm.ipc_recv_proof_sender_wake=1 did NOT reach the kernel cmdline"
    echo "[err]   (YARM_IPC_RECV_PROOF_SENDER_WAKE_SET enabled=true absent) — runner/oracle plumbing bug, not a workload failure."
    echo "[hint] invoke as: YARM_IPC_RECV_PROOF_SENDER_WAKE=1 scripts/qemu-ipc-recv-v2-oracle-smoke.sh $ARCH"
    exit 1
  fi
  echo "[ok]   ipc-oracle: sender-wake sub-knob reached the kernel (YARM_IPC_RECV_PROOF_SENDER_WAKE_SET enabled=true)"
  # Stage 181C: when the workload started but did NOT complete the sequence, run the
  # fork/send/order triage so the failing seam (esp. fork Internal under graduated
  # default-on) is reported explicitly rather than as an opaque missing marker.
  if ! marker_present "IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE"; then
    diagnose_sender_wake
  fi
  proof_require "sender-wake" "IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE" "IPC_RECV_V2_SENDER_WAKE_ORDER_OK"
  # Stage 181C: advisory — even when sender-wake PASSES, surface any residual graduated
  # one-shot proof PT-pool leak + the per-step breakdown so the net delta is localized.
  # (This does not change pass/fail; the kernel's UNLOCK_GRADUATED_POOL_LEAK guard is the
  # authoritative signal and is intentionally NOT silenced here.)
  if marker_present "UNLOCK_GRADUATED_POOL_LEAK"; then
    echo "[warn] ipc-oracle: graduated one-shot proof still shows a residual PT-pool delta:"
    echo "[warn]   $(rg -N -a "UNLOCK_GRADUATED_POOL_BEFORE|UNLOCK_GRADUATED_POOL_AFTER|UNLOCK_GRADUATED_POOL_LEAK" "$ANALYSIS_LOG" | tail -n3)"
    if marker_present "UNLOCK_GRADUATED_D3_STEP"; then
      echo "[warn]   per-step PT-pool trace (attributes the residual to a step):"
      rg -N -a "UNLOCK_GRADUATED_D3_STEP|UNLOCK_GRADUATED_D3_SCRATCH_CACHE_DROPPED" "$ANALYSIS_LOG" \
        | sed 's/^/[warn]     /'
    fi
  fi
else
  echo "[info] ipc-oracle: proof sender-wake: not required"
fi

# Stage 193B — IpcSend-plain LIVE oracle acceptance. When YARM_IPC_SEND_PLAIN_ORACLE=1,
# the boot must fire the 193A IpcSendPlain boundary split LIVE: init plain-sends to a
# forked, recv-v2-blocked child. Require BOTH the userspace oracle DONE marker AND the
# kernel boundary-split + retirement markers, plus the child's byte-identical recv, and
# reject the boundary FAIL marker.
if [[ "$YARM_IPC_SEND_PLAIN_ORACLE" == "1" ]]; then
  echo "[info] ipc-oracle: proof send-plain oracle: REQUIRED"
  # Plumbing pre-check: the sub-knob must have reached the kernel cmdline.
  if ! marker_present "YARM_IPC_SEND_PLAIN_ORACLE_SET enabled=true"; then
    echo "[err] ipc-oracle: send-plain oracle requested but yarm.ipc_send_plain_oracle=1 did NOT reach the kernel cmdline"
    echo "[err]   (YARM_IPC_SEND_PLAIN_ORACLE_SET enabled=true absent) — runner/oracle plumbing bug."
    echo "[hint] invoke as: YARM_IPC_SEND_PLAIN_ORACLE=1 scripts/qemu-ipc-recv-v2-oracle-smoke.sh $ARCH"
    exit 1
  fi
  echo "[ok]   ipc-oracle: send-plain oracle sub-knob reached the kernel"
  # Stage 198A (SECOND-COHORT PLAIN PARITY): the retirement marker is now arch-tagged
  # and the oracle emits the canonical per-arch blocked-receiver attestation. Both are
  # required LIVE on every arch ($ARCH selects x86_64 / aarch64 / riscv64).
  SEND_PLAIN_REQUIRED=(
    "IPC_SEND_PLAIN_ORACLE_WAITER_OBSERVED"
    "IPC_SEND_BOUNDARY_SPLIT_BEGIN"
    "IPC_SEND_BOUNDARY_PLAIN_SNAPSHOT_OK"
    "IPC_SEND_BOUNDARY_USER_COPY_OK"
    "IPC_SEND_BOUNDARY_WAKE_OK"
    "IPC_SEND_BOUNDARY_SPLIT_DONE result=ok"
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=$ARCH class=IpcSendPlain result=ok"
    "IPCSEND_PLAIN_BLOCKED_RECEIVER_ORACLE_DONE arch=$ARCH result=ok payload_len=8 receiver_resumes=1"
    "IPC_SEND_PLAIN_LIVE_ORACLE_DONE result=ok"
  )
  for m in "${SEND_PLAIN_REQUIRED[@]}"; do
    if marker_present "$m"; then
      echo "[ok]   send-plain oracle marker present: $m"
    else
      echo "[err] ipc-oracle: send-plain oracle marker absent: $m"
      rc=1
    fi
  done
  # The woken child must have observed the byte-identical plain payload (no cap).
  if rg -q -a -e 'IPC_SEND_PLAIN_ORACLE_CHILD_RECV_OK payload_match=1 transferred_cap=0' "$ANALYSIS_LOG"; then
    echo "[ok]   send-plain oracle: child received byte-identical plain payload (no cap)"
  else
    echo "[err] ipc-oracle: send-plain oracle: child did NOT observe the byte-identical plain payload"
    rc=1
  fi
  # The boundary split must NOT have failed, and the send must not have deferred/failed.
  for f in "IPC_SEND_BOUNDARY_SPLIT_FAIL" "IPC_SEND_PLAIN_ORACLE_SEND_FAILED" \
           "IPC_SEND_PLAIN_ORACLE_FORK_FAILED" "IPC_SEND_PLAIN_ORACLE_WAITER_UNEXPECTED"; do
    if marker_present "$f"; then
      echo "[err] ipc-oracle: send-plain oracle fatal marker present: $f"
      rc=1
    fi
  done
  [[ "$rc" -eq 0 ]] && echo "[ok] ipc-oracle: send-plain LIVE oracle PASSED ($ARCH)"
else
  echo "[info] ipc-oracle: proof send-plain oracle: not required"
fi

# Stage 193C — IpcSend ordinary cap-transfer LIVE oracle acceptance. When
# YARM_IPC_SEND_CAP_ORACLE=1, the boot must fire the 193C IpcSendOrdinaryCap boundary
# split LIVE: init sends a one-ordinary-cap message to a forked, recv-v2-blocked child.
# Require the userspace oracle DONE marker AND the kernel boundary-split + materialize
# + retirement markers, plus the child's fresh-cap + byte-identical recv, and reject
# the boundary FAIL / DISPATCH_POST_WORK_FAIL markers.
if [[ "$YARM_IPC_SEND_CAP_ORACLE" == "1" ]]; then
  echo "[info] ipc-oracle: proof send-cap oracle: REQUIRED"
  if ! marker_present "YARM_IPC_SEND_CAP_ORACLE_SET enabled=true"; then
    echo "[err] ipc-oracle: send-cap oracle requested but yarm.ipc_send_cap_oracle=1 did NOT reach the kernel cmdline"
    echo "[err]   (YARM_IPC_SEND_CAP_ORACLE_SET enabled=true absent) — runner/oracle plumbing bug."
    echo "[hint] invoke as: YARM_IPC_SEND_CAP_ORACLE=1 scripts/qemu-ipc-recv-v2-oracle-smoke.sh $ARCH"
    exit 1
  fi
  echo "[ok]   ipc-oracle: send-cap oracle sub-knob reached the kernel"
  # Stage 198B (ORDINARY-CAP PARITY): the retirement marker is now arch-tagged and the oracle
  # emits the canonical per-arch blocked-receiver attestation (fresh cap + authoritative object
  # identity). Both required LIVE on every arch ($ARCH selects x86_64 / aarch64 / riscv64).
  SEND_CAP_REQUIRED=(
    "IPC_SEND_CAP_ORACLE_WAITER_OBSERVED"
    "IPC_SEND_CAP_BOUNDARY_SPLIT_BEGIN"
    "IPC_SEND_CAP_BOUNDARY_SNAPSHOT_OK"
    "IPC_SEND_CAP_BOUNDARY_MATERIALIZE_OK"
    "IPC_SEND_CAP_BOUNDARY_USER_COPY_OK"
    "IPC_SEND_CAP_BOUNDARY_WAKE_OK"
    "IPC_SEND_CAP_BOUNDARY_SPLIT_DONE result=ok"
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=$ARCH class=IpcSendOrdinaryCap result=ok"
    "IPCSEND_ORDINARY_CAP_BLOCKED_RECEIVER_ORACLE_DONE arch=$ARCH result=ok payload_len=8 receiver_resumes=1 fresh_cap=1 object_identity_ok=1"
    "IPC_SEND_CAP_LIVE_ORACLE_DONE result=ok"
  )
  for m in "${SEND_CAP_REQUIRED[@]}"; do
    if marker_present "$m"; then
      echo "[ok]   send-cap oracle marker present: $m"
    else
      echo "[err] ipc-oracle: send-cap oracle marker absent: $m"
      rc=1
    fi
  done
  # The woken child must have received a FRESH receiver-local cap (not the sender-local
  # handle) AND the byte-identical payload.
  if rg -q -a -e 'IPC_SEND_CAP_ORACLE_CHILD_RECV_OK payload_match=1 transferred_cap=1 cap_is_fresh=1' "$ANALYSIS_LOG"; then
    echo "[ok]   send-cap oracle: child received a fresh receiver-local cap + byte-identical payload"
  else
    echo "[err] ipc-oracle: send-cap oracle: child did NOT observe a fresh cap + byte-identical payload"
    rc=1
  fi
  # Stage 198B: the kernel's AUTHORITATIVE object-identity comparison must confirm the materialized
  # receiver-local cap references the SAME endpoint object the sender transferred (match=1).
  if rg -q -a -e 'IPC_ORDINARY_CAP_OBJECT_IDENTITY .*match=1' "$ANALYSIS_LOG"; then
    echo "[ok]   send-cap oracle: kernel authoritative object-identity match=1 (same object preserved)"
  else
    echo "[err] ipc-oracle: send-cap oracle: kernel object-identity match=1 marker absent"
    rc=1
  fi
  for f in "IPC_SEND_CAP_BOUNDARY_SPLIT_FAIL" "IPC_SEND_CAP_ORACLE_SEND_FAILED" \
           "IPC_SEND_CAP_ORACLE_FORK_FAILED" "IPC_SEND_CAP_ORACLE_WAITER_UNEXPECTED" \
           "DISPATCH_POST_WORK_FAIL kind=blocked_waiter_ordinary_cap"; do
    if marker_present "$f"; then
      echo "[err] ipc-oracle: send-cap oracle fatal marker present: $f"
      rc=1
    fi
  done
  [[ "$rc" -eq 0 ]] && echo "[ok] ipc-oracle: send-cap ordinary LIVE oracle PASSED ($ARCH)"
else
  echo "[info] ipc-oracle: proof send-cap oracle: not required"
fi

# Stage 193D — IpcSend reply-cap transfer LIVE oracle acceptance. When
# YARM_IPC_SEND_REPLY_CAP_ORACLE=1, the boot must fire the 193D IpcSendReplyCap boundary
# split LIVE: init transfers a one-shot reply cap to a forked, recv-v2-blocked child.
# Require the userspace oracle DONE marker AND the kernel boundary-split + materialize +
# retirement markers, plus the child's fresh-reply-cap + byte-identical recv, and reject
# the boundary FAIL / DISPATCH_POST_WORK_FAIL markers.
if [[ "$YARM_IPC_SEND_REPLY_CAP_ORACLE" == "1" ]]; then
  echo "[info] ipc-oracle: proof send-reply-cap oracle: REQUIRED"
  if ! marker_present "YARM_IPC_SEND_REPLY_CAP_ORACLE_SET enabled=true"; then
    echo "[err] ipc-oracle: send-reply-cap oracle requested but yarm.ipc_send_reply_cap_oracle=1 did NOT reach the kernel cmdline"
    echo "[err]   (YARM_IPC_SEND_REPLY_CAP_ORACLE_SET enabled=true absent) — runner/oracle plumbing bug."
    echo "[hint] invoke as: YARM_IPC_SEND_REPLY_CAP_ORACLE=1 scripts/qemu-ipc-recv-v2-oracle-smoke.sh $ARCH"
    exit 1
  fi
  echo "[ok]   ipc-oracle: send-reply-cap oracle sub-knob reached the kernel"
  SEND_REPLY_CAP_REQUIRED=(
    "IPC_SEND_REPLY_CAP_ORACLE_WAITER_OBSERVED"
    "IPC_SEND_REPLY_CAP_BOUNDARY_SPLIT_BEGIN"
    "IPC_SEND_REPLY_CAP_BOUNDARY_SNAPSHOT_OK"
    "IPC_SEND_REPLY_CAP_BOUNDARY_MATERIALIZE_OK"
    "IPC_SEND_REPLY_CAP_BOUNDARY_USER_COPY_OK"
    "IPC_SEND_REPLY_CAP_BOUNDARY_WAKE_OK"
    "IPC_SEND_REPLY_CAP_BOUNDARY_SPLIT_DONE result=ok"
    "GLOBAL_LOCK_RETIRE_CLASS_DONE class=IpcSendReplyCap result=ok"
    "IPC_SEND_REPLY_CAP_LIVE_ORACLE_DONE result=ok"
  )
  for m in "${SEND_REPLY_CAP_REQUIRED[@]}"; do
    if marker_present "$m"; then
      echo "[ok]   send-reply-cap oracle marker present: $m"
    else
      echo "[err] ipc-oracle: send-reply-cap oracle marker absent: $m"
      rc=1
    fi
  done
  # The woken child must have received a FRESH receiver-local one-shot reply cap (not the
  # sender-local handle) AND the byte-identical payload.
  if rg -q -a -e 'IPC_SEND_REPLY_CAP_ORACLE_CHILD_RECV_OK payload_match=1 reply_cap=1 reply_is_fresh=1' "$ANALYSIS_LOG"; then
    echo "[ok]   send-reply-cap oracle: child received a fresh receiver-local reply cap + byte-identical payload"
  else
    echo "[err] ipc-oracle: send-reply-cap oracle: child did NOT observe a fresh reply cap + byte-identical payload"
    rc=1
  fi
  for f in "IPC_SEND_REPLY_CAP_BOUNDARY_SPLIT_FAIL" "IPC_SEND_REPLY_CAP_ORACLE_SEND_FAILED" \
           "IPC_SEND_REPLY_CAP_ORACLE_FORK_FAILED" "IPC_SEND_REPLY_CAP_ORACLE_WAITER_UNEXPECTED" \
           "DISPATCH_POST_WORK_FAIL kind=blocked_waiter_reply_cap"; do
    if marker_present "$f"; then
      echo "[err] ipc-oracle: send-reply-cap oracle fatal marker present: $f"
      rc=1
    fi
  done
  [[ "$rc" -eq 0 ]] && echo "[ok] ipc-oracle: send-reply-cap LIVE oracle PASSED ($ARCH)"
else
  echo "[info] ipc-oracle: proof send-reply-cap oracle: not required"
fi

# Stage 193E — IpcSend plain no-waiter enqueue LIVE oracle acceptance. When
# YARM_IPC_SEND_ENQUEUE_ORACLE=1, the boot must fire the 193E IpcSendPlainEnqueue boundary
# split LIVE: init plain-sends to the loopback with no blocked receiver (the message
# enqueues), then recv-drains it byte-identical. Require the userspace oracle DONE marker AND
# the kernel enqueue-boundary + retirement markers; reject the boundary FAIL marker.
if [[ "$YARM_IPC_SEND_ENQUEUE_ORACLE" == "1" ]]; then
  echo "[info] ipc-oracle: proof send-enqueue oracle: REQUIRED"
  if ! marker_present "YARM_IPC_SEND_ENQUEUE_ORACLE_SET enabled=true"; then
    echo "[err] ipc-oracle: send-enqueue oracle requested but yarm.ipc_send_enqueue_oracle=1 did NOT reach the kernel cmdline"
    echo "[err]   (YARM_IPC_SEND_ENQUEUE_ORACLE_SET enabled=true absent) — runner/oracle plumbing bug."
    echo "[hint] invoke as: YARM_IPC_SEND_ENQUEUE_ORACLE=1 scripts/qemu-ipc-recv-v2-oracle-smoke.sh $ARCH"
    exit 1
  fi
  echo "[ok]   ipc-oracle: send-enqueue oracle sub-knob reached the kernel"
  # Stage 198A (SECOND-COHORT PLAIN PARITY): the retirement marker is now arch-tagged
  # and the oracle emits the canonical per-arch enqueue attestation. Both required LIVE
  # on every arch ($ARCH selects x86_64 / aarch64 / riscv64).
  SEND_ENQUEUE_REQUIRED=(
    "IPC_SEND_ENQUEUE_ORACLE_SEND_OK"
    "IPC_SEND_ENQUEUE_BOUNDARY_SPLIT_BEGIN"
    "IPC_SEND_ENQUEUE_BOUNDARY_SNAPSHOT_OK"
    "IPC_SEND_ENQUEUE_BOUNDARY_ENQUEUE_OK"
    "IPC_SEND_ENQUEUE_BOUNDARY_SENDER_STATE_OK"
    "IPC_SEND_ENQUEUE_BOUNDARY_SPLIT_DONE result=ok"
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=$ARCH class=IpcSendPlainEnqueue result=ok"
    "IPCSEND_PLAIN_ENQUEUE_ORACLE_DONE arch=$ARCH result=ok payload_len=8 dequeue_count=1"
    "IPC_SEND_ENQUEUE_LIVE_ORACLE_DONE result=ok"
  )
  for m in "${SEND_ENQUEUE_REQUIRED[@]}"; do
    if marker_present "$m"; then
      echo "[ok]   send-enqueue oracle marker present: $m"
    else
      echo "[err] ipc-oracle: send-enqueue oracle marker absent: $m"
      rc=1
    fi
  done
  # The later recv must have drained the queued message byte-identical (no cap).
  if rg -q -a -e 'IPC_SEND_ENQUEUE_ORACLE_RECV_OK payload_match=1 transferred_cap=0' "$ANALYSIS_LOG"; then
    echo "[ok]   send-enqueue oracle: receiver-later dequeue delivered the byte-identical plain message"
  else
    echo "[err] ipc-oracle: send-enqueue oracle: receiver-later dequeue did NOT deliver byte-identical"
    rc=1
  fi
  for f in "IPC_SEND_ENQUEUE_BOUNDARY_SPLIT_FAIL" "IPC_SEND_ENQUEUE_ORACLE_SEND_FAILED" \
           "IPC_SEND_ENQUEUE_ORACLE_MSG_BUILD_FAIL"; do
    if marker_present "$f"; then
      echo "[err] ipc-oracle: send-enqueue oracle fatal marker present: $f"
      rc=1
    fi
  done
  [[ "$rc" -eq 0 ]] && echo "[ok] ipc-oracle: send-enqueue plain LIVE oracle PASSED ($ARCH)"
else
  echo "[info] ipc-oracle: proof send-enqueue oracle: not required"
fi

# Stage 193F — IpcSend ordinary-cap no-waiter enqueue LIVE oracle acceptance. When
# YARM_IPC_SEND_CAP_ENQUEUE_ORACLE=1, the boot must fire the 193F IpcSendOrdinaryCapEnqueue
# boundary split LIVE: init transfers an ordinary cap to the loopback with no blocked receiver
# (the message enqueues, envelope preserved), then recv-drains it and materializes a fresh
# receiver-local cap. Require the userspace oracle DONE marker AND the kernel cap-enqueue
# boundary + retirement markers + the recv-side IPC_TRANSFER_CAP_MATERIALIZE_OK; reject FAIL.
if [[ "$YARM_IPC_SEND_CAP_ENQUEUE_ORACLE" == "1" ]]; then
  echo "[info] ipc-oracle: proof send-cap-enqueue oracle: REQUIRED"
  if ! marker_present "YARM_IPC_SEND_CAP_ENQUEUE_ORACLE_SET enabled=true"; then
    echo "[err] ipc-oracle: send-cap-enqueue oracle requested but yarm.ipc_send_cap_enqueue_oracle=1 did NOT reach the kernel cmdline"
    echo "[err]   (YARM_IPC_SEND_CAP_ENQUEUE_ORACLE_SET enabled=true absent) — runner/oracle plumbing bug."
    echo "[hint] invoke as: YARM_IPC_SEND_CAP_ENQUEUE_ORACLE=1 scripts/qemu-ipc-recv-v2-oracle-smoke.sh $ARCH"
    exit 1
  fi
  echo "[ok]   ipc-oracle: send-cap-enqueue oracle sub-knob reached the kernel"
  # Stage 198B (ORDINARY-CAP PARITY): arch-tagged retirement + the canonical per-arch enqueue
  # attestation (fresh cap + authoritative object identity). Both required LIVE on every arch.
  SEND_CAP_ENQUEUE_REQUIRED=(
    "IPC_SEND_CAP_ENQUEUE_ORACLE_SEND_OK"
    "IPC_SEND_CAP_ENQUEUE_BOUNDARY_SPLIT_BEGIN"
    "IPC_SEND_CAP_ENQUEUE_BOUNDARY_SNAPSHOT_OK"
    "IPC_SEND_CAP_ENQUEUE_BOUNDARY_ENQUEUE_OK"
    "IPC_SEND_CAP_ENQUEUE_BOUNDARY_TRANSFER_STATE_OK"
    "IPC_SEND_CAP_ENQUEUE_BOUNDARY_SENDER_STATE_OK"
    "IPC_SEND_CAP_ENQUEUE_BOUNDARY_SPLIT_DONE result=ok"
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=$ARCH class=IpcSendOrdinaryCapEnqueue result=ok"
    "IPCSEND_ORDINARY_CAP_ENQUEUE_ORACLE_DONE arch=$ARCH result=ok payload_len=8 dequeue_count=1 fresh_cap=1 object_identity_ok=1"
    "IPC_TRANSFER_CAP_MATERIALIZE_OK"
    "IPC_SEND_CAP_ENQUEUE_LIVE_ORACLE_DONE result=ok"
  )
  for m in "${SEND_CAP_ENQUEUE_REQUIRED[@]}"; do
    if marker_present "$m"; then
      echo "[ok]   send-cap-enqueue oracle marker present: $m"
    else
      echo "[err] ipc-oracle: send-cap-enqueue oracle marker absent: $m"
      rc=1
    fi
  done
  # The receiver-later dequeue must materialize a FRESH receiver-local cap + byte-identical.
  if rg -q -a -e 'IPC_SEND_CAP_ENQUEUE_ORACLE_RECV_OK payload_match=1 cap_is_fresh=1' "$ANALYSIS_LOG"; then
    echo "[ok]   send-cap-enqueue oracle: receiver-later dequeue materialized a fresh receiver-local cap"
  else
    echo "[err] ipc-oracle: send-cap-enqueue oracle: receiver-later dequeue did NOT materialize a fresh cap"
    rc=1
  fi
  # Stage 198B: kernel AUTHORITATIVE object-identity comparison (same endpoint object preserved).
  if rg -q -a -e 'IPC_ORDINARY_CAP_OBJECT_IDENTITY .*match=1' "$ANALYSIS_LOG"; then
    echo "[ok]   send-cap-enqueue oracle: kernel authoritative object-identity match=1"
  else
    echo "[err] ipc-oracle: send-cap-enqueue oracle: kernel object-identity match=1 marker absent"
    rc=1
  fi
  for f in "IPC_SEND_CAP_ENQUEUE_BOUNDARY_SPLIT_FAIL" "IPC_SEND_CAP_ENQUEUE_ORACLE_SEND_FAILED" \
           "IPC_SEND_CAP_ENQUEUE_ORACLE_MSG_BUILD_FAIL"; do
    if marker_present "$f"; then
      echo "[err] ipc-oracle: send-cap-enqueue oracle fatal marker present: $f"
      rc=1
    fi
  done
  [[ "$rc" -eq 0 ]] && echo "[ok] ipc-oracle: send-cap-enqueue ordinary LIVE oracle PASSED ($ARCH)"
else
  echo "[info] ipc-oracle: proof send-cap-enqueue oracle: not required"
fi

# Stage 170 (IPC-FINAL): strict frozen acceptance gate. Enabled by IPC_FINAL=1
# (which also turned on all three proof workloads + extended mode above). Every
# accepted IPC-surface marker is HARD-required; the sender-wake ORDER marker is
# line-start anchored; a strict failure gate rejects the Stage 170 regressions.
# Handled COW page faults (PAGE_FAULT followed by PAGE_FAULT_HANDLED_COW) are NOT
# fatal and are deliberately absent from the fatal set below.
if [[ "$IPC_FINAL" == "1" ]]; then
  echo "[info] ipc-oracle: IPC-FINAL strict marker + failure gate"
  # Full accepted recv-v2 IPC surface (deterministic on x86_64 under the full
  # proof profile). extended-mode + proof_require above already require most of
  # these; re-asserting here makes IPC-FINAL a single self-contained gate.
  IPC_FINAL_REQUIRED=(
    "IPC_RECV_V2_META_BLOCKED_WAITER_OK"
    "IPC_RECV_V2_META_IMMEDIATE_OK"
    "IPC_RECV_V2_META_QUEUED_SPLIT_OK"
    "IPC_REPLY_CAP_ONESHOT_OK"
    "IPC_TRANSFER_CAP_MATERIALIZE_OK"
    "IPC_RECV_V2_ROLLBACK_OK"
    "IPC_RECV_PROOF_QUEUED_SPLIT_SEQUENCE_DONE"
    "IPC_RECV_PROOF_ROLLBACK_SEQUENCE_DONE"
  )
  for m in "${IPC_FINAL_REQUIRED[@]}"; do
    if marker_present "$m"; then
      echo "[ok]   IPC-FINAL required marker present: $m"
    else
      echo "[err] ipc-oracle: IPC-FINAL required marker absent: $m"
      rc=1
    fi
  done
  # Sender-wake ORDER marker: line-start anchored (never an absence/echo line).
  if marker_present_linestart "IPC_RECV_V2_SENDER_WAKE_ORDER_OK"; then
    echo "[ok]   IPC-FINAL: line-start IPC_RECV_V2_SENDER_WAKE_ORDER_OK present"
  else
    echo "[err] ipc-oracle: IPC-FINAL: line-start IPC_RECV_V2_SENDER_WAKE_ORDER_OK absent"
    rc=1
  fi
  # Sender-wake sequence must be a real USER_LOG line.
  if rg -q -a -e 'USER_LOG.*IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE' "$ANALYSIS_LOG"; then
    echo "[ok]   IPC-FINAL: USER_LOG sender-wake sequence present"
  else
    echo "[err] ipc-oracle: IPC-FINAL: USER_LOG IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE absent"
    rc=1
  fi
  # Strict failure gate: substring fatal markers.
  IPC_FINAL_FATAL=(
    "BLOCKED_WOULDBLOCK_FATAL"
    "CapabilityFull"
    "TaskTableFull"
    "DOUBLE_FAULT"
    "TRIPLE"
    "PANIC"
    "FATAL"
  )
  for f in "${IPC_FINAL_FATAL[@]}"; do
    if marker_present "$f"; then
      echo "[err] ipc-oracle: IPC-FINAL fatal marker present: $f"
      rc=1
    fi
  done
  # Line-start fatal breadcrumbs (a mid-line occurrence is not a fault escalation).
  for f in "!Fv" "!BNv"; do
    if rg -q -a -e "^${f}" "$ANALYSIS_LOG"; then
      echo "[err] ipc-oracle: IPC-FINAL fatal breadcrumb (line-start): $f"
      rc=1
    fi
  done
  # Stage 171B: page-fault gate — fail ONLY on the EXPLICIT unhandled/fatal
  # page-fault markers, never on benign PAGE_FAULT_* diagnostic lines. A HANDLED
  # fault emits many PAGE_FAULT_* diagnostics (ENTRY / HW_REGS / FRAME_WORDS /
  # FRAME_DECODE / HW_PTE_WALK / RAW / X86_ERROR / CR3_COMPARE) BEFORE the final
  # PAGE_FAULT_HANDLED_COW (or PAGE_FAULT_HANDLED_DEMAND); those are expected.
  for pf_fatal in "PAGE_FAULT_UNHANDLED" "PAGE_FAULT_FATAL" "PAGE_FAULT_NOT_HANDLED"; do
    if marker_present "$pf_fatal"; then
      echo "[err] ipc-oracle: IPC-FINAL: explicit unhandled/fatal page-fault marker: $pf_fatal"
      rc=1
    fi
  done
  # Committed-path D2 recv/send dispatch relocation (only when the knob is set):
  # require the out-of-lock markers and reject a committed-path switch_required
  # in-lock fallback.
  if [[ "${D2_RECV_GENUINE:-0}" == "1" ]]; then
    for m in D2_RECV_GENUINE_DISPATCH_DEFERRED D2_RECV_GENUINE_NO_INLOCK_DISPATCH \
             D2_RECV_GENUINE_GLOBAL_DROPPED D2_RECV_GENUINE_DISPATCH_DONE; do
      if marker_present "$m"; then echo "[ok]   IPC-FINAL: recv marker $m"; else echo "[err] ipc-oracle: IPC-FINAL: recv marker absent: $m"; rc=1; fi
    done
    if tr '\r' '\n' <"$ANALYSIS_LOG" | awk '
        /D2_RECV_GENUINE_PHASE_DISPATCH/ { pending=1; next }
        pending && /D2_RECV_GENUINE_DISPATCH_DEFERRED/ { pending=0; next }
        pending && /reason=switch_required/ { print "BAD"; pending=0; next }
        pending && /D2_RECV_GENUINE_FALLBACK reason=/ { print "BAD"; pending=0; next }
      ' | rg -q "BAD"; then
      echo "[err] ipc-oracle: IPC-FINAL: committed recv path fell back to in-lock dispatch"
      rc=1
    fi
  fi
  if [[ "${D2_SEND_GENUINE:-0}" == "1" ]]; then
    for m in D2_SEND_GENUINE_DISPATCH_DEFERRED D2_SEND_GENUINE_NO_INLOCK_DISPATCH \
             D2_SEND_GENUINE_GLOBAL_DROPPED D2_SEND_GENUINE_DISPATCH_DONE; do
      if marker_present "$m"; then echo "[ok]   IPC-FINAL: send marker $m"; else echo "[err] ipc-oracle: IPC-FINAL: send marker absent: $m"; rc=1; fi
    done
    if tr '\r' '\n' <"$ANALYSIS_LOG" | awk '
        /D2_SEND_GENUINE_PHASE_DISPATCH/ { pending=1; next }
        pending && /D2_SEND_GENUINE_DISPATCH_DEFERRED/ { pending=0; next }
        pending && /reason=switch_required/ { print "BAD"; pending=0; next }
        pending && /D2_SEND_GENUINE_FALLBACK reason=/ { print "BAD"; pending=0; next }
      ' | rg -q "BAD"; then
      echo "[err] ipc-oracle: IPC-FINAL: committed send path fell back to in-lock dispatch"
      rc=1
    fi
  fi
  [[ "$rc" -eq 0 ]] && echo "[ok] ipc-oracle: IPC-FINAL strict acceptance profile PASSED ($ARCH)"
fi

# Regression gate: every baseline marker must still be present.
if [[ -n "${ORACLE_BASELINE:-}" ]]; then
  if [[ ! -f "$ORACLE_BASELINE" ]]; then
    echo "[err] ipc-oracle: ORACLE_BASELINE not found: $ORACLE_BASELINE"
    rc=1
  else
    while IFS= read -r b; do
      [[ -z "$b" ]] && continue
      if ! rg -q "^$b$" "$ORACLE_SNAPSHOT"; then
        echo "[err] ipc-oracle: baseline marker regressed (now absent): $b"
        rc=1
      fi
    done <"$ORACLE_BASELINE"
    [[ "$rc" -eq 0 ]] && echo "[ok] ipc-oracle: no baseline marker regressed"
  fi
fi

if [[ "$rc" -eq 0 ]]; then
  echo "[ok] ipc-oracle: IPC recv/reply/transfer/split delivery oracle passed ($ARCH)"
fi
exit "$rc"
