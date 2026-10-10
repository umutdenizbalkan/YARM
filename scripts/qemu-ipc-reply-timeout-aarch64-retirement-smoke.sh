#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 200C2C1 — AArch64 LIVE reply-receive TIMEOUT OFF-LOCK RETIREMENT smoke (two fresh boots).
#
# The AArch64 port of the accepted x86_64 IpcReplyTimeout retirement cell. It REUSES the arch-neutral
# collector + per-CPU deferred-work drain + completion transaction verbatim, wired into the AArch64
# trap-entry post-lock area, and proves the two live outcomes on `-smp 1`, EACH from a fresh boot of
# the SAME clean tree:
#
#   A. timeout-wins  — the production off-lock collector publishes one deferred work item and the
#                      off-lock drain completes it: the blocked recv-v2 caller resumes with the
#                      canonical TimedOut written to its saved trap frame, the class reports
#                      scan_broad_lock=0, and the retirement seal is emitted. The late NR7 is rejected.
#   B. reply-wins    — the server's NR7 wins terminal ownership first (a reversible ClaimedByReply
#                      lease blocks any concurrent timeout claim); the reply copies, the record +
#                      terminal complete as Reply, the deadline lease completes, and the caller resumes
#                      with the exact payload. A later production scan passes the old deadline
#                      harmlessly (no timeout work/wake).
#
# LIVE RETIREMENT seal: the reply-timeout class deadline scan runs OFF the broad KernelState
# (IPC_REPLY_TIMEOUT_LOCK_STATUS arch=aarch64 scan_broad_lock=0), and the runner emits
# GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=IpcReplyTimeout exactly once (timeout-wins boot).
# Ordinary receive timeouts stay on their existing in-lock path (NOT retired here).
#
# On both fresh boots passing, the runner (not userspace) emits:
#   STAGE_200C_REPLY_TIMEOUT_AARCH64_RETIREMENT_SEAL arch=aarch64 classes=1 live_cells=1 ...
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=aarch64-ipc-reply-timeout-oracle
KTARGET=${KTARGET:-targets/aarch64-yarm-none.json}
KPROFILE=${KPROFILE:-aarch64-none}
KELF=${KELF:-target/aarch64-yarm-none/${KPROFILE}/kernel_boot}
KBIN=${KBIN:-build-aarch64/yarm-aarch64.bin}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ipc-reply-timeout-retirement-aarch64}
TIMEOUT_SECS=${TIMEOUT_SECS:-180}
IDLE_MAX_SECS=${IDLE_MAX_SECS:-180}
mkdir -p "$LOGDIR"

fail=0
note() { echo "[ipc-reply-timeout-retire-aarch64] $*"; }
die()  { echo "[ipc-reply-timeout-retire-aarch64][fail] $*"; fail=1; }
# BL5a: `--self-test` exercises the oracle-scoped accounting helpers against synthetic fixtures
# ONLY. It needs no artifacts, so every build and boot step below is skipped.
SELF_TEST=0
[[ "${1:-}" == "--self-test" ]] && SELF_TEST=1

# ── SHA + clean-tree capture (re-checked between the two fresh boots) ──
SHA0=$(git rev-parse HEAD 2>/dev/null || echo unknown)
clean_tree() { git diff --quiet && git diff --cached --quiet; }
if clean_tree; then TREE0=clean; else TREE0=dirty; fi
note "sha=$SHA0 tree=$TREE0"

recheck_sha_clean() {
  local sha; sha=$(git rev-parse HEAD 2>/dev/null || echo unknown)
  [[ "$sha" == "$SHA0" ]] || die "SHA drifted mid-run ($SHA0 -> $sha)"
  if clean_tree; then :; else [[ "$TREE0" == "dirty" ]] || die "tree became dirty mid-run"; fi
}

objcopy_tool() {
  if command -v llvm-objcopy >/dev/null 2>&1; then echo llvm-objcopy;
  elif command -v rust-objcopy >/dev/null 2>&1; then echo rust-objcopy;
  else return 1; fi
}

# ── 1. Base artifacts (servers + initramfs; the userspace oracle is arch-gated) ──
if (( ! SELF_TEST )); then
  note "building base aarch64 artifacts (servers + initramfs)"
  BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-aarch64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || die "base artifact build failed (see $LOGDIR/build.log)"
fi

OBJCOPY=""
if (( ! fail && ! SELF_TEST )); then OBJCOPY=$(objcopy_tool) || die "no objcopy available"; fi

# ── 2. Feature-ON kernel + integrity: it MUST carry the AArch64 retirement literals ──
if (( ! fail && ! SELF_TEST )); then
  note "building kernel_boot with --features $FEATURE"
  cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" \
    --no-default-features --features "$FEATURE" \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 \
    || die "feature kernel_boot build failed (see $LOGDIR/kbuild.log)"
fi
if (( ! fail && ! SELF_TEST )); then
  "$OBJCOPY" -O binary "$KELF" "$KBIN" >"$LOGDIR/objcopy.log" 2>&1 \
    || die "objcopy of feature kernel failed (see $LOGDIR/objcopy.log)"
  # The AArch64 kernel carries the arch=aarch64 attribution + the class literal.
  rg -a -q "aarch64" "$KBIN" || die "feature kernel missing aarch64 attribution"
  rg -a -q "class=IpcReplyTimeout" "$KBIN" || die "feature kernel missing IpcReplyTimeout class literal"
  # Cross-arch hygiene: no x86_64/riscv64 reply-timeout attribution in the AArch64 kernel.
  rg -a -q "IPC_REPLY_TIMEOUT_OK arch=x86_64" "$KBIN" && die "x86_64 reply-timeout attribution in aarch64 kernel"
  rg -a -q "IPC_REPLY_TIMEOUT_OK arch=riscv64" "$KBIN" && die "riscv64 reply-timeout attribution in aarch64 kernel"
fi

# ── 2b. Feature-OFF kernel MUST be marker-CLEAN of the reply-timeout retirement literals ──
if (( ! fail && ! SELF_TEST )); then
  note "building feature-OFF kernel_boot and asserting it is marker-clean"
  cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" --no-default-features \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild-off.log" 2>&1 \
    || die "feature-off kernel_boot build failed (see $LOGDIR/kbuild-off.log)"
  OFF_BIN="$LOGDIR/kernel_boot_off.bin"
  "$OBJCOPY" -O binary "$KELF" "$OFF_BIN" >/dev/null 2>&1 || die "objcopy of feature-off kernel failed"
  # U7 (canonical 199E) split this gate in two, because the feature no longer decides where
  # timeouts are processed — only which oracle SCENARIOS are built.
  #
  # (a) The oracle's own scenario literals must still be absent from a feature-OFF kernel.
  #
  # Canonical 199E moved "IPC_REPLY_TIMEOUT_ARMED arch=" out of this half and into (b). It is
  # no longer an oracle-only literal: `KernelState::arm_production_reply_deadline` is
  # unconditional production code — no `#[cfg]`, no runtime selector — and emits the same
  # arch-neutral `IPC_REPLY_TIMEOUT_ARMED arch={}` format string at the committed
  # reply-receive block point, so the literal is compiled into EVERY image, feature-OFF
  # included. Asserting its absence would now assert that production registration is absent.
  for lit in "IPC_REPLY_BEATS_TIMEOUT_OK arch="; do
    rg -a -q "$lit" "$OFF_BIN" && die "feature-OFF kernel contains oracle literal $lit (not marker-clean)"
  done
  # (b) The PRODUCTION pipeline's literals must now be PRESENT in a feature-OFF kernel. Their
  # absence would mean the promotion regressed back behind the feature — the exact thing U7
  # removed — so this half of the gate is asserted positively.
  # NB: the arch tag is a RUNTIME argument (`arch={}` + `REPLY_TIMEOUT_ARCH`), so the composite
  # "MARKER arch=aarch64" string never exists in the image — only the format-string fragment
  # does. Matching the fragment is what makes this gate real rather than vacuously true.
  # `class=IpcReplyTimeout` is deliberately NOT in this list on aarch64. Its only emit site on
  # this port is the resume-boundary recv/reply completion consumer, which U6 policy keeps
  # feature-gated (`the_reply_timeout_feature_policy_is_unchanged`) and U7 does not widen: U7
  # promoted where timeouts are SCANNED and SETTLED, not the IpcRecv delivery consumer. On
  # x86_64 the drain itself is the delivery point, so the seal is present there.
  for lit in "IPC_REPLY_TIMEOUT_OK arch=" "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=" \
             "IPC_REPLY_TIMEOUT_LATE_SCAN arch=" \
             "IPC_REPLY_TIMEOUT_DEFERRED arch=" \
             "IPC_REPLY_TIMEOUT_ARMED arch="; do
    rg -a -q "$lit" "$OFF_BIN" || die "feature-OFF kernel is missing PRODUCTION literal $lit (the U7 pipeline must not be feature-gated)"
  done
fi

if (( fail )); then
  echo "STAGE_200C_REPLY_TIMEOUT_AARCH64_RETIREMENT_SEAL arch=aarch64 classes=1 live_cells=1 result=fail reason=build"
  exit 1
fi

# ── Boot helper: one fresh -smp 1 boot for the given mode, into its own log ──
boot_mode() {
  local mode="$1" log="$2"
  env \
    KERNEL_IMAGE="$KBIN" \
    INITRAMFS_IMAGE=build-aarch64/initramfs-core.cpio \
    KERNEL_CMDLINE="yarm.aarch64_ipc_reply_timeout_oracle=${mode}" \
    QEMU_SMP=1 \
    QEMU_SMOKE_STRICT=0 \
    LOGFILE="$log" \
    TIMEOUT_SECS="$TIMEOUT_SECS" \
    IDLE_MAX_SECS="$IDLE_MAX_SECS" \
    scripts/qemu-aarch64-core-smoke.sh >"$LOGDIR/core-${mode}.log" 2>&1 || true
}

verify_log() {
  local norm="$1"; shift
  local m c
  for m in "$@"; do
    c=$(rg -a -c -F "$m" "$norm" 2>/dev/null || echo 0)
    [[ "$c" == "1" ]] || die "marker count != 1 (got $c): $m"
  done
}
# ── Canonical 199E: ORACLE-SCOPED settlement accounting ──────────────────────────────────────
#
# Production reply/call registration is live on every boot now, so the ARMED and OK marker
# FAMILIES are no longer oracle-specific: an unrelated production caller legitimately arms its own
# reply deadline in the same cell, and where its scheduler-tick deadline elapses it legitimately
# settles too. Counting the family and calling the result "the oracle's" was therefore wrong, and
# it is wrong in the dangerous direction — it fails on correct behaviour.
#
# These helpers scope the oracle's assertions to the oracle's EXACT caller identity, taken from
# the provisioning marker rather than hardcoded, and keep a family-level bound that a duplicate
# settlement cannot satisfy. Nothing is loosened: the identity check is STRICTER than the family
# count it replaces, every settlement line must still carry the full field contract, and the
# oracle's one-shot terminal seals stay at exactly one.
oracle_init_tid() {
  rg -a -o -m1 'IPC_REPLY_TIMEOUT_ORACLE_PROVISION_OK init_tid=[0-9]+' "$1" 2>/dev/null \
    | rg -a -o '[0-9]+$' || true
}

# Exactly ONE registration for the oracle's own caller identity.
verify_oracle_armed_once() {
  local norm="$1" arch="$2" tid c
  tid="$(oracle_init_tid "$norm")"
  [[ -n "$tid" ]] || die "no oracle provisioning marker: the oracle identity cannot be scoped"
  c=$(rg -a -c -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} caller_tid=${tid} " "$norm" 2>/dev/null || echo 0)
  [[ "$c" == "1" ]] || die "oracle-identity ARMED count != 1 (got $c) for caller_tid=${tid}"
}

# No DUPLICATE settlement anywhere, and every settlement carries the exact field contract.
verify_no_duplicate_settlement() {
  local norm="$1" arch="$2" full="$3" armed ok okfull
  armed=$(rg -a -c -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} " "$norm" 2>/dev/null || echo 0)
  ok=$(rg -a -c -F "IPC_REPLY_TIMEOUT_OK arch=${arch} terminal=Timeout" "$norm" 2>/dev/null || echo 0)
  okfull=$(rg -a -c -F "$full" "$norm" 2>/dev/null || echo 0)
  (( ok >= 1 )) || die "no timeout settlement at all"
  (( ok <= armed )) || die "settlements ($ok) exceed registrations ($armed) — duplicate settlement"
  [[ "$okfull" == "$ok" ]] \
    || die "a settlement line does not carry the exact contract ($okfull of $ok match): $full"
}

# The reply-wins variant: the ORACLE's record must not be settled by timeout, but an unrelated
# production caller's own deadline may legitimately elapse in the same boot, so zero settlements
# is permitted while a settlement WITHOUT a matching registration (a duplicate) is not. The
# oracle's own outcome stays sealed by the identity-bearing reply-win markers the cell asserts.
verify_settlements_within_registrations() {
  local norm="$1" arch="$2" armed ok
  armed=$(rg -a -c -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} " "$norm" 2>/dev/null || echo 0)
  ok=$(rg -a -c -F "IPC_REPLY_TIMEOUT_OK arch=${arch} terminal=Timeout" "$norm" 2>/dev/null || echo 0)
  (( ok <= armed )) || die "settlements ($ok) exceed registrations ($armed) — duplicate settlement"
}

forbid_log() {
  local norm="$1"; shift
  local m
  for m in "$@"; do
    if rg -a -q -F "$m" "$norm"; then die "forbidden marker present: $m"; fi
  done
}

# ── Canonical 199E-R3: ORACLE-SCOPED COMPLETION accounting ───────────────────────────────────
#
# The same correction the ARMED/OK families already received, extended to the two families that
# still counted globally. With ProductionTick default-on, an unrelated production caller (the
# supervisor, tid 2) legitimately arms and settles its OWN reply deadline in this same boot, so
# `count == 1` on a family failed on correct behaviour: the two events named two DIFFERENT tasks,
# each delivered exactly once.
#
# Nothing is loosened. Each family now carries BOTH
#   (a) an oracle-identity bound — exactly one event for the oracle's own caller, and none for a
#       second occurrence of that caller's exact generation; and
#   (b) a GLOBAL duplicate bound — no identity anywhere may appear twice for one completion stage.
# An unrelated caller can therefore never satisfy the oracle's assertion, and a genuine duplicate
# still fails even when it belongs to a caller the oracle does not own.
#
# FIELD INVENTORY (this is what each marker actually carries, and it bounds what can be checked):
#   IPC_REPLY_TIMEOUT_ORACLE_PROVISION_OK  init_tid                       — no ASID
#   IPC_REPLY_TIMEOUT_ARMED                caller_tid caller_asid record_index record_generation
#   AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED  tid blocked_generation    — no ASID
#   IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED  arch terminal result          — NO identity at all
#
# So the strongest available tuples are: registration = {tid, asid, record_index,
# record_generation}; delivery = {tid, blocked_generation}. LIMITATION, recorded deliberately:
# the delivery marker carries no ASID, so a replacement incarnation that reused the numeric TID
# within one boot would be indistinguishable there — the registration bound, which DOES carry the
# ASID, is what closes that gap for the oracle's own caller. The COMMITTED marker carries no
# identity whatsoever and cannot be field-scoped at all; it is bound positionally inside the
# oracle's own identity-scoped window plus a one-to-one pairing with the settlements.
# Widening either marker is a kernel change and is out of scope for this repair.

# Extract a numeric `key=value` field from one marker line. The leading space is required so
# `tid=` cannot match `caller_tid=`.
marker_field() {
  sed -n "s/.*[[:space:]]$2=\([0-9][0-9]*\).*/\1/p" <<<"$1" | head -1
}

# The oracle's REGISTRATION identity: caller tid from the provisioning marker (never hardcoded,
# never positional), then the ASID / record coordinates from that caller's own ARMED line.
# Fails closed when the provisioning marker is missing, when the oracle caller has no
# registration, or when either carries a malformed identity.
ORACLE_TID=""; ORACLE_ASID=""; ORACLE_RECORD_INDEX=""; ORACLE_RECORD_GEN=""
derive_oracle_identity() {
  local norm="$1" arch="$2" n armed
  ORACLE_TID=""; ORACLE_ASID=""; ORACLE_RECORD_INDEX=""; ORACLE_RECORD_GEN=""
  n=$(rg -a -c -F "IPC_REPLY_TIMEOUT_ORACLE_PROVISION_OK init_tid=" "$norm" 2>/dev/null || echo 0)
  [[ "$n" == "1" ]]     || { die "oracle provisioning marker count != 1 (got $n): the oracle identity cannot be scoped"; return 1; }
  ORACLE_TID="$(oracle_init_tid "$norm")"
  [[ -n "$ORACLE_TID" ]]     || { die "oracle provisioning marker carries no init_tid: identity malformed"; return 1; }
  n=$(rg -a -c -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} caller_tid=${ORACLE_TID} " "$norm" 2>/dev/null || echo 0)
  [[ "$n" == "1" ]]     || { die "oracle-identity ARMED count != 1 (got $n) for caller_tid=${ORACLE_TID}"; return 1; }
  armed=$(rg -a -m1 -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} caller_tid=${ORACLE_TID} " "$norm")
  ORACLE_ASID="$(marker_field "$armed" caller_asid)"
  ORACLE_RECORD_INDEX="$(marker_field "$armed" record_index)"
  ORACLE_RECORD_GEN="$(marker_field "$armed" record_generation)"
  [[ -n "$ORACLE_ASID" && -n "$ORACLE_RECORD_INDEX" && -n "$ORACLE_RECORD_GEN" ]]     || { die "oracle registration is malformed (asid=$ORACLE_ASID idx=$ORACLE_RECORD_INDEX gen=$ORACLE_RECORD_GEN)"; return 1; }
  note "oracle identity: tid=${ORACLE_TID} asid=${ORACLE_ASID} record=${ORACLE_RECORD_INDEX}/${ORACLE_RECORD_GEN}"
}

# Every (tid, blocked_generation) pair observed on the delivery family, one per line.
delivered_identities() {
  sed -n 's/.*AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid=\([0-9][0-9]*\) .*blocked_generation=\([0-9][0-9]*\) .*/\1:\2/p' "$1"
}

# (1) exactly one delivery for the ORACLE's identity, and (2) no second delivery for that exact
# identity+generation. An unrelated production caller cannot satisfy either, because both match
# on the oracle's own tid.
verify_oracle_completion_delivered_once() {
  local norm="$1" tid="$2" n line gen g
  n=$(rg -a -c -F "AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid=${tid} " "$norm" 2>/dev/null || echo 0)
  [[ "$n" == "1" ]]     || die "oracle-identity completion delivery count != 1 (got $n) for tid=${tid}"
  line=$(rg -a -m1 -F "AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid=${tid} " "$norm" 2>/dev/null || true)
  [[ -n "$line" ]] || return
  gen="$(marker_field "$line" blocked_generation)"
  [[ -n "$gen" ]] || { die "the oracle's completion delivery carries no blocked_generation"; return; }
  g=$(delivered_identities "$norm" | rg -a -c -F "${tid}:${gen}" 2>/dev/null || echo 0)
  [[ "$g" == "1" ]]     || die "duplicate completion delivery for tid=${tid} blocked_generation=${gen} (got $g)"
  # The AArch64 consumer prints the numeric result it encoded, so the canonical TimedOut is `9`.
  rg -a -q -F "AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid=${tid} class=IpcRecv result=9 " "$norm"     || die "the oracle's completion delivery does not carry the canonical IpcRecv/TimedOut (9) contract"
}

# (4) GLOBAL duplicate detection, across every identity in the boot — including callers the
# oracle does not own. This is what the family `count == 1` used to provide, kept in full.
verify_no_duplicate_completion_delivery() {
  local norm="$1" dup
  dup=$(delivered_identities "$norm" | sort | uniq -d | head -3 | tr '\n' ' ')
  [[ -z "${dup// /}" ]]     || die "duplicate completion delivery for the same identity+generation: ${dup}"
}

# The COMMITTED family carries no identity, so it is bound two independent ways, and together
# they are strictly stronger than the global `count == 1` they replace:
#   (a) POSITIONAL — exactly one commit lies inside the oracle's own window, delimited by two
#       INDEPENDENTLY identity-scoped lines: the oracle's ARMED and the oracle's DELIVERED;
#   (b) PAIRED — every reply-timeout SETTLEMENT (`IPC_REPLY_TIMEOUT_OK`) is followed by exactly one
#       commit before the next settlement, and no commit appears without one. A duplicate, an
#       orphan, a missing or a reordered commit fails while N legitimate settlements pass.
#
# BL5a: the commit is tied to the reply-timeout SETTLEMENT population, never to the CONSUMED one.
# CONSUMED is the production resume boundary of EVERY blocked-receive completion (199E-A64RC made
# it ungated), so an ordinary receive timeout consumes through it with the same
# `class=IpcRecv result=9`; the RISC-V twin of this check failed on every correct boot for exactly
# that reason. The settlement is the reply-timeout class's own event, emitted once per settled
# terminal, so it is the population a commit belongs to.
verify_oracle_completion_committed_once() {
  local norm="$1" arch="$2" tid="$3" a d inwin total distinct
  a=$(rg -a -n -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} caller_tid=${tid} " "$norm" 2>/dev/null | head -1 | cut -d: -f1)
  d=$(rg -a -n -F "AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid=${tid} " "$norm" 2>/dev/null | head -1 | cut -d: -f1)
  [[ -n "$a" && -n "$d" ]]     || { die "cannot bound the oracle completion window (armed=$a delivered=$d)"; return; }
  (( a < d )) || { die "the oracle's registration must precede its completion delivery ($a,$d)"; return; }
  inwin=$(rg -a -n -F "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=${arch} terminal=Timeout result=ok" "$norm" 2>/dev/null           | cut -d: -f1 | awk -v lo="$a" -v hi="$d" '$1 > lo && $1 < hi' | wc -l | tr -d ' ')
  [[ "$inwin" == "1" ]]     || die "oracle-window COMPLETION_COMMITTED count != 1 (got $inwin) between lines $a and $d"
  local pairing
  pairing=$(rg -a -n -e "IPC_REPLY_TIMEOUT_OK arch=${arch} terminal=Timeout" \
              -e "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=${arch} terminal=Timeout result=ok" "$norm" 2>/dev/null \
    | awk '/IPC_REPLY_TIMEOUT_OK/ { if (open) { print "missing commit before line " $0; bad=1 } open=1; s++; next }
           /IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED/ { if (!open) { print "orphan or duplicate commit at line " $0; bad=1 } open=0; c++; next }
           END { if (open) { print "the last settlement was never committed"; bad=1 }
                 if (s != c) { print "settlements=" s " commits=" c; bad=1 }
                 if (s == 0) { print "no settlement at all"; bad=1 }
                 exit bad }' | cut -d: -f1,2 | head -3 | tr '\n' ' ')
  [[ -z "$pairing" ]] || die "reply-timeout settlements and COMPLETION_COMMITTED do not pair one-to-one: ${pairing}"
}

# The ORACLE's OWN ordered chain. Every position is selected by the oracle's identity, so an
# unrelated production caller's earlier registration/commit/delivery — tid 2's, which legitimately
# precedes all of these in a default-ProductionTick boot — can never stand in for one of them. The
# commit position is the one INSIDE the oracle's window, not the family's first occurrence.
verify_oracle_ordered_chain() {
  local norm="$1" arch="$2" tid="$3" oa oc od ou
  oa=$(rg -a -n -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} caller_tid=${tid} " "$norm" 2>/dev/null | head -1 | cut -d: -f1)
  od=$(rg -a -n -F "AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid=${tid} " "$norm" 2>/dev/null | head -1 | cut -d: -f1)
  oc=$(rg -a -n -F "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=${arch} terminal=Timeout result=ok" "$norm" 2>/dev/null \
       | cut -d: -f1 | awk -v lo="${oa:-0}" -v hi="${od:-0}" '$1 > lo && $1 < hi' | head -1)
  ou=$(rg -a -n -F "USER_LOG tid=${tid} msg=AARCH64_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut" "$norm" 2>/dev/null | head -1 | cut -d: -f1)
  if [[ -n "$oa" && -n "$oc" && -n "$od" && -n "$ou" ]]; then
    (( oa < oc )) || die "oracle: registration must precede its own committed completion ($oa,$oc)"
    (( oc < od )) || die "oracle: its committed completion must precede its own delivery ($oc,$od)"
    (( od < ou )) || die "oracle: its delivery must precede its own userspace completion ($od,$ou)"
  else
    die "oracle-scoped marker sequence incomplete (oa=$oa oc=$oc od=$od ou=$ou)"
  fi
}

# BL5a — the late reply after a timeout win is refused by ONE of TWO owners:
#   * the LEGACY reserve decline inside the broad handler — `IPC_REPLY_WIN_RESERVE ... outcome=decline
#     reason=TimeoutAlreadyClaimed` (no other decline reason is acceptable, and in particular none
#     may mention deadline bookkeeping);
#   * the PRE-LOCK refusal DIRECT3-CAP-FINAL §7 added, which answers the same typed error at the
#     verdict without entering the broad dispatcher — `IPCREPLY_DIRECT_REFUSED_PRE_LOCK`, scoped to
#     the ORACLE's record and generation and INERT (no copy, no wake, no mutation).
# Exactly one refusal in total, and it must lie after the oracle's committed completion and before
# its userspace completion, followed by the server observing the rejection.
verify_late_reply_refused_once() {
  local norm="$1" arch="$2" tid="$3" legacy declines prelock_any prelock inert at oc ou
  legacy=$(rg -a -c -F "IPC_REPLY_WIN_RESERVE arch=${arch} outcome=decline reason=TimeoutAlreadyClaimed result=ok" "$norm" 2>/dev/null || echo 0)
  declines=$(rg -a -c -F "IPC_REPLY_WIN_RESERVE arch=${arch} outcome=decline" "$norm" 2>/dev/null || echo 0)
  prelock_any=$(rg -a -c -F "IPCREPLY_DIRECT_REFUSED_PRE_LOCK record_index=${ORACLE_RECORD_INDEX} record_generation=${ORACLE_RECORD_GEN} " "$norm" 2>/dev/null || echo 0)
  prelock=$(rg -a -c -e "^IPCREPLY_DIRECT_REFUSED_PRE_LOCK record_index=${ORACLE_RECORD_INDEX} record_generation=${ORACLE_RECORD_GEN} replier_tid=[0-9]+ .*err=WrongObject" "$norm" 2>/dev/null || echo 0)
  inert=$(rg -a -c -e "^IPCREPLY_DIRECT_REFUSED_PRE_LOCK record_index=${ORACLE_RECORD_INDEX} record_generation=${ORACLE_RECORD_GEN} replier_tid=[0-9]+ terminal=Settled reply_copies=0 caller_wakes=0 mutations=0 err=WrongObject result=ok" "$norm" 2>/dev/null || echo 0)
  (( declines == legacy )) || { die "a reply-win reserve declined for another reason (${declines} declines, ${legacy} TimeoutAlreadyClaimed)"; return; }
  (( legacy + prelock_any == 1 )) || { die "the late reply must be refused exactly once (legacy=${legacy} pre_lock=${prelock_any})"; return; }
  (( prelock_any == prelock && prelock == inert )) || { die "the pre-lock refusal is not an inert WrongObject refusal (lines=${prelock_any} wrong_object=${prelock} inert=${inert})"; return; }
  if (( legacy == 1 )); then
    at=$(rg -a -n -F "IPC_REPLY_WIN_RESERVE arch=${arch} outcome=decline reason=TimeoutAlreadyClaimed" "$norm" | head -1 | cut -d: -f1)
  else
    at=$(rg -a -n -F "IPCREPLY_DIRECT_REFUSED_PRE_LOCK record_index=${ORACLE_RECORD_INDEX} record_generation=${ORACLE_RECORD_GEN} " "$norm" | head -1 | cut -d: -f1)
  fi
  oc=$(rg -a -n -F "AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid=${tid} " "$norm" 2>/dev/null | head -1 | cut -d: -f1)
  oc=$(rg -a -n -F "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=${arch} terminal=Timeout result=ok" "$norm" 2>/dev/null \
       | cut -d: -f1 | awk -v hi="${oc:-0}" '$1 < hi' | tail -1)
  ou=$(rg -a -n -F "USER_LOG tid=${tid} msg=AARCH64_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut" "$norm" 2>/dev/null | head -1 | cut -d: -f1)
  if [[ -z "$at" || -z "$oc" || -z "$ou" ]] || (( at <= oc || at >= ou )); then
    die "the late-reply refusal must lie after the oracle's committed completion and before its userspace completion (commit=${oc:-none} refusal=${at:-none} done=${ou:-none})"
    return
  fi
  rg -a -q -e "IPC_REPLY_TIMEOUT_ORACLE_SERVER_LATE_REPLY rejected=1" "$norm" \
    || die "the oracle server never observed its late reply being rejected"
}


# ── Canonical 199E-R3: SELF-TEST for the oracle-scoped accounting ───────────────────────────
#
# `scripts/qemu-ipc-reply-timeout-aarch64-retirement-smoke.sh --self-test` runs the scoped
# helpers against synthetic fixtures and exits. It needs no QEMU, no build and no artifacts, so
# the accounting logic is provable in isolation from the live cell it guards — including the
# failure directions, which a passing live boot can never demonstrate.
#
# The fixtures encode a default-ProductionTick boot: tid 2 is an unrelated production caller that
# legitimately arms and settles its own reply deadline BEFORE the oracle (tid 1) does anything.
fixture_log() {
  local kind="$1"
  case "$kind" in
    oracle_only) ;;
    *)
      # An unrelated production caller settles first, in full.
      echo "IPC_REPLY_TIMEOUT_ARMED arch=aarch64 caller_tid=2 caller_asid=2 record_index=0 record_generation=1 terminal_epoch=1 token_slot=0 token_generation=1 deadline=7 result=ok"
      echo "$FIXTURE_OK"
      echo "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=aarch64 terminal=Timeout result=ok"
      echo "AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid=2 class=IpcRecv result=9 blocked_generation=1 elr=0x000000000040d1d8 result=ok"
      if [[ "$kind" == "unrelated_duplicate" ]]; then
        echo "AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid=2 class=IpcRecv result=9 blocked_generation=1 elr=0x000000000040d1d8 result=ok"
      fi
      ;;
  esac
  [[ "$kind" == "no_provision" ]] \
    || echo "IPC_REPLY_TIMEOUT_ORACLE_PROVISION_OK init_tid=1 req_cap=65539 rep_cap=65540 req_eidx=6 rep_eidx=7 mode=1"
  [[ "$kind" == "malformed_armed" ]] \
    && echo "IPC_REPLY_TIMEOUT_ARMED arch=aarch64 caller_tid=1 caller_asid=x record_index=y record_generation=z result=ok"
  [[ "$kind" == "malformed_armed" ]] \
    || echo "IPC_REPLY_TIMEOUT_ARMED arch=aarch64 caller_tid=1 caller_asid=1 record_index=1 record_generation=17 terminal_epoch=1 token_slot=0 token_generation=1 deadline=10209 result=ok"
  case "$kind" in
    oracle_missing_delivery) ;;   # oracle registers, never settles
    *)
      [[ "$kind" == "commit_without_settlement" ]] || echo "$FIXTURE_OK"
      [[ "$kind" == "settlement_without_commit" ]] \
        || echo "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=aarch64 terminal=Timeout result=ok"
      echo "AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid=1 class=IpcRecv result=9 blocked_generation=18 elr=0x000000000040d1d8 result=ok"
      if [[ "$kind" == "oracle_duplicate" ]]; then
        echo "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=aarch64 terminal=Timeout result=ok"
        echo "AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid=1 class=IpcRecv result=9 blocked_generation=18 elr=0x000000000040d1d8 result=ok"
      fi
      # BL5a: the late reply's refusal, by one owner or the other (or a malformed one).
      case "$kind" in
        refusal_none) ;;
        refusal_legacy) echo "IPC_REPLY_WIN_RESERVE arch=aarch64 outcome=decline reason=TimeoutAlreadyClaimed result=ok" ;;
        refusal_both)
          echo "IPC_REPLY_WIN_RESERVE arch=aarch64 outcome=decline reason=TimeoutAlreadyClaimed result=ok"
          echo "$FIXTURE_PRELOCK" ;;
        refusal_not_inert) echo "${FIXTURE_PRELOCK/reply_copies=0 caller_wakes=0 mutations=0/reply_copies=1 caller_wakes=0 mutations=1}" ;;
        refusal_other_record) echo "${FIXTURE_PRELOCK/record_index=1 record_generation=17/record_index=7 record_generation=17}" ;;
        refusal_other_reason) echo "IPC_REPLY_WIN_RESERVE arch=aarch64 outcome=decline reason=DeadlineBookkeeping result=ok"
                              echo "$FIXTURE_PRELOCK" ;;
        *) echo "$FIXTURE_PRELOCK" ;;
      esac
      [[ "$kind" == "refusal_none" ]] || echo "USER_LOG tid=10008 msg=IPC_REPLY_TIMEOUT_ORACLE_SERVER_LATE_REPLY rejected=1 err=true"
      echo "USER_LOG tid=1 msg=AARCH64_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut caller_continuations=1 late_reply=rejected result=ok"
      ;;
  esac
  # BL5a: the boot also holds the supervisor's ordinary receive timeouts, which deliver through the
  # same resume-boundary marker with the same class and result — hundreds per boot, each its own
  # blocked generation, none of them a reply-timeout completion.
  if [[ "$kind" == "with_ordinary_receive_timeouts" ]]; then
    for g in 40 41 42 43 44 45 46 47; do
      echo "AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid=2 class=IpcRecv result=9 blocked_generation=${g} elr=0x000000000040d1d8 result=ok"
    done
  fi
}
FIXTURE_OK="IPC_REPLY_TIMEOUT_OK arch=aarch64 terminal=Timeout timeout_result=TimedOut caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=0 result=ok"
FIXTURE_PRELOCK="IPCREPLY_DIRECT_REFUSED_PRE_LOCK record_index=1 record_generation=17 replier_tid=10008 terminal=Settled reply_copies=0 caller_wakes=0 mutations=0 err=WrongObject result=ok"

# Run every scoped check against one fixture. Returns 0 when all pass, 1 when any died.
self_test_checks() {
  local f="$1"
  fail=0
  derive_oracle_identity "$f" aarch64 || true
  if [[ -n "$ORACLE_TID" ]]; then
    verify_oracle_completion_delivered_once "$f" "$ORACLE_TID"
    verify_oracle_completion_committed_once "$f" aarch64 "$ORACLE_TID"
    verify_oracle_ordered_chain "$f" aarch64 "$ORACLE_TID"
    verify_late_reply_refused_once "$f" aarch64 "$ORACLE_TID"
  fi
  verify_no_duplicate_completion_delivery "$f"
  return "$fail"
}

self_test() {
  local dir rc st_fail=0 kind expect
  dir=$(mktemp -d) || { echo "[self-test][fail] mktemp"; return 1; }
  # kind:expect — `pass` means every scoped check accepts the fixture.
  for spec in \
      "oracle_only:pass" \
      "oracle_plus_unrelated:pass" \
      "oracle_missing_delivery:fail" \
      "oracle_duplicate:fail" \
      "unrelated_duplicate:fail" \
      "no_provision:fail" \
      "malformed_armed:fail" \
      "with_ordinary_receive_timeouts:pass" \
      "commit_without_settlement:fail" \
      "settlement_without_commit:fail" \
      "refusal_legacy:pass" \
      "refusal_none:fail" \
      "refusal_both:fail" \
      "refusal_not_inert:fail" \
      "refusal_other_record:fail" \
      "refusal_other_reason:fail"; do
    kind="${spec%%:*}"; expect="${spec##*:}"
    fixture_log "$kind" >"$dir/$kind.log"
    rc=0; ( self_test_checks "$dir/$kind.log" ) >"$dir/$kind.out" 2>&1 || rc=1
    if [[ "$expect" == "pass" && "$rc" != "0" ]]; then
      echo "[self-test][fail] $kind: expected PASS, got FAIL"; sed 's/^/    /' "$dir/$kind.out"; st_fail=1
    elif [[ "$expect" == "fail" && "$rc" == "0" ]]; then
      echo "[self-test][fail] $kind: expected FAIL, got PASS"; st_fail=1
    else
      echo "[self-test][ok] $kind expected=$expect"
    fi
  done
  rm -rf "$dir"
  if (( st_fail )); then
    echo "STAGE_199E_R3_ORACLE_SCOPED_ACCOUNTING_SELFTEST arch=aarch64 result=fail"
    return 1
  fi
  echo "STAGE_199E_R3_ORACLE_SCOPED_ACCOUNTING_SELFTEST arch=aarch64 cases=16 result=ok"
  return 0
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
  exit $?
fi

# BL5a-2 — the class-retirement one-shot, positioned by the reply-timeout delivery it belongs to.
reply_timeout_delivery_pairs() {
  sed -n 's/.*IPC_REPLY_TIMEOUT_ARMED arch=[a-z0-9_]* caller_tid=\([0-9][0-9]*\) .* token_generation=\([0-9][0-9]*\) .*/\1:\2/p' "$1" | sort -u
}
verify_class_retirement_order() {
  local norm="$1" arch="$2" done_marker="$3" ci cn rt ud pairs
  ci=$(rg -a -n -F "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=${arch}" "$norm" | head -1 | cut -d: -f1)
  pairs=$(reply_timeout_delivery_pairs "$norm" | tr '\n' ' ')
  cn=$(rg -a -n "AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid=[0-9]+ .*blocked_generation=[0-9]+ " "$norm" 2>/dev/null \
       | awk -v lo="${ci:-0}" -v pairs="$pairs" '
           BEGIN { n = split(pairs, p, " "); for (i = 1; i <= n; i++) want[p[i]] = 1 }
           { split($0, a, ":"); line = a[1]
             t = $0; sub(/.* tid=/, "", t); sub(/ .*/, "", t)
             g = $0; sub(/.*blocked_generation=/, "", g); sub(/ .*/, "", g)
             if (line + 0 > lo + 0 && ((t ":" g) in want)) { print line; exit } }')
  rt=$(rg -a -n -F "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=${arch} class=IpcReplyTimeout" "$norm" | head -1 | cut -d: -f1)
  ud=$(rg -a -n -F "$done_marker" "$norm" | head -1 | cut -d: -f1)
  if [[ -n "$ci" && -n "$cn" && -n "$rt" && -n "$ud" ]]; then
    (( ci < cn )) || die "completion committed must precede the resume-boundary consumption"
    (( cn <= rt )) || die "retirement marker must follow the completion consumption"
    (( rt < ud )) || die "retirement marker must precede the userspace completion"
  else
    die "ordered marker sequence incomplete (ci=$ci cn=$cn rt=$rt ud=$ud)"
  fi
}

# ── 3. Scenario A — timeout-wins, feature enabled (fresh boot) ──
TW_OK=0
if (( ! fail )); then
  note "booting fresh -smp 1 QEMU: yarm.aarch64_ipc_reply_timeout_oracle=timeout-wins"
  boot_mode timeout-wins "$LOGDIR/boot-timeout-wins.log"
  TW="$LOGDIR/tw.norm.log"; tr '\r' '\n' <"$LOGDIR/boot-timeout-wins.log" >"$TW"
  [[ -s "$TW" ]] || die "no timeout-wins boot log"
  verify_oracle_armed_once "$TW" aarch64
  verify_no_duplicate_settlement "$TW" aarch64 \
    "IPC_REPLY_TIMEOUT_OK arch=aarch64 terminal=Timeout timeout_result=TimedOut caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=0 result=ok"
  # BL5a: the oracle's identity is derived ONCE, from the provisioning marker and the oracle
  # caller's own registration, and every per-caller check below is scoped to it — the same
  # accounting 199E-R3 gave the RISC-V twin. The two completion families used to be asserted here
  # as GLOBAL singletons (`COMPLETION_COMMITTED` through `verify_log`, `CONSUMED` by a bare
  # count), which held only while no other caller completed in the same boot: CONSUMED is the
  # production resume boundary of every blocked receive, and tid 2 arms a production reply
  # deadline of its own in every boot.
  derive_oracle_identity "$TW" aarch64
  verify_log "$TW" \
    "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=aarch64 scan_broad_lock=0 completion_transaction_narrow=1 classes=IpcReplyTimeout+IpcSendTimeout production=1 result=ok" \
    "IPC_REPLY_TIMEOUT_DEFERRED arch=aarch64 published=1 drained=1 result=ok" \
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=IpcReplyTimeout result=ok" \
    "AARCH64_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut caller_continuations=1 late_reply=rejected result=ok"
  if [[ -n "$ORACLE_TID" ]]; then
    verify_oracle_completion_delivered_once "$TW" "$ORACLE_TID"
    verify_oracle_completion_committed_once "$TW" aarch64 "$ORACLE_TID"
  fi
  # GLOBAL duplicate detection across every caller in the boot: exactly one consumption per
  # identity+generation ⇒ one timeout encoding, one ELR-advance boundary, no duplicate wake.
  verify_no_duplicate_completion_delivery "$TW"
  # ORDERED sequence, part 1 — the GLOBAL one-shot. `GLOBAL_LOCK_RETIRE_CLASS_DONE` carries no
  # identity: it is earned by the first reply-timeout completion DELIVERED in the boot, whichever
  # caller owns it. BL5a-2: that delivery is derived from production events, not taken as the
  # family's first line. The resume boundary serves every blocked-receive completion, ordinary
  # receive timeouts included, so the family's first line is only the reply-timeout delivery
  # while ordinary timeouts never fire — which is what a stale reply registration used to cause.
  # A reply-timeout delivery is the one whose `(tid, blocked_generation)` is a registration's
  # `(caller_tid, token_generation)`. `ud` IS the oracle's, scoped by its own USER_LOG tid.
  verify_class_retirement_order "$TW" aarch64 "USER_LOG tid=${ORACLE_TID} msg=AARCH64_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut"
  # ORDERED sequence, part 2 — the ORACLE's OWN chain, every position selected by its identity.
  [[ -n "$ORACLE_TID" ]] && verify_oracle_ordered_chain "$TW" aarch64 "$ORACLE_TID"
  # The late reply must be refused exactly once, by one of its two owners, for the oracle's own
  # record, inertly, between the oracle's committed completion and its userspace completion.
  [[ -n "$ORACLE_TID" ]] && verify_late_reply_refused_once "$TW" aarch64 "$ORACLE_TID"
  forbid_log "$TW" \
    "IPC_REPLY_BEATS_TIMEOUT_OK" \
    "scan_broad_lock=1" \
    "IPC_REPLY_TIMEOUT_OK arch=x86_64" \
    "KERNEL PANIC" "RUST PANIC" "panicked at" "SYNCHRONOUS EXCEPTION" "Unhandled"
  recheck_sha_clean
  (( fail )) || TW_OK=1
fi

# ── 4. Scenario B — reply-wins, feature enabled (SEPARATE fresh boot) ──
RW_OK=0
if (( ! fail )); then
  note "booting fresh -smp 1 QEMU: yarm.aarch64_ipc_reply_timeout_oracle=reply-wins"
  boot_mode reply-wins "$LOGDIR/boot-reply-wins.log"
  RW="$LOGDIR/rw.norm.log"; tr '\r' '\n' <"$LOGDIR/boot-reply-wins.log" >"$RW"
  [[ -s "$RW" ]] || die "no reply-wins boot log"
  verify_log "$RW" \
    "IPC_REPLY_BEATS_TIMEOUT_OK arch=aarch64 terminal=Reply reply_copies=1 deadline_disarmed=1 late_timeout_claims=0 caller_wakes=1 result=ok" \
    "IPC_REPLY_TIMEOUT_LATE_SCAN arch=aarch64 outcome=reply_won late_timeout_claims=0 result=ok" \
    "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=aarch64 scan_broad_lock=0 completion_transaction_narrow=1 classes=IpcReplyTimeout+IpcSendTimeout production=1 result=ok" \
    "AARCH64_IPC_REPLY_BEATS_TIMEOUT_DONE reply_ok=1 caller_continuations=1 late_timeout_wakes=0 duplicate_reply=rejected result=ok" \
    "IPC_REPLY_TIMEOUT_ORACLE_SERVER_DUP_REPLY rejected=1"
  verify_oracle_armed_once "$RW" aarch64
  verify_settlements_within_registrations "$RW" aarch64
  forbid_log "$RW" \
    "scan_broad_lock=1" \
    "KERNEL PANIC" "RUST PANIC" "panicked at" "SYNCHRONOUS EXCEPTION" "Unhandled"
  recheck_sha_clean
  (( fail )) || RW_OK=1
fi

# ── 5. Retirement live seal (runner-emitted; both fresh boots must pass) ──
if (( fail )) || [[ "$TW_OK" != "1" || "$RW_OK" != "1" ]]; then
  echo "STAGE_200C_REPLY_TIMEOUT_AARCH64_RETIREMENT_SEAL arch=aarch64 classes=1 live_cells=1 timeout_wins=${TW_OK} reply_wins=${RW_OK} result=fail"
  exit 1
fi

cat <<'SEAL'
STAGE_200C_REPLY_TIMEOUT_AARCH64_RETIREMENT_SEAL
arch=aarch64
classes=1
live_cells=1
timeout_wins=1
reply_wins=1
canonical_timeout_result=1
completion_reentry=1
elr_single_advance=1
scan_broad_lock=0
completion_transaction_narrow=1
late_reply_successes=0
late_timeout_wakes=0
duplicate_wakes=0
stale_authority_restores=0
wrong_waiter_mutations=0
result=ok
SEAL
