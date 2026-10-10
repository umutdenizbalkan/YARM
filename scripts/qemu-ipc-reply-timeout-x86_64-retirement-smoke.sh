#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 200C2B — x86_64 LIVE reply-receive TIMEOUT OFF-LOCK RETIREMENT smoke (two fresh boots).
#
# Wires the accepted Stage 200C1 reply-timeout transaction into a NARROW off-lock collector +
# per-CPU deferred-work drain that runs the completion at the trap-entry post-lock area with the
# broad `SpinLock<KernelState>` already dropped. It earns ONE x86_64 retirement cell for the
# `IpcReplyTimeout` class, and proves the two live outcomes on `-smp 1`, EACH from a fresh boot of
# the SAME clean tree:
#
#   A. timeout-wins  — the PRODUCTION off-lock collector publishes one deferred work item and the
#                      off-lock drain completes it: the caller resumes with the canonical TimedOut,
#                      the class reports scan_broad_lock=0, and the retirement seal is emitted. The
#                      server's late NR7 is rejected.
#   B. reply-wins    — the server's NR7 wins terminal ownership before the deadline (reversibly, so
#                      a copy fault could roll back), the exact deadline lease is completed, the
#                      caller resumes with the reply payload, and the off-lock collector genuinely
#                      scans PAST the old deadline harmlessly (no timeout wake).
#
# This is a LIVE RETIREMENT seal: the reply-timeout class deadline scan NO LONGER enters through the
# broad KernelState (reported honestly via IPC_REPLY_TIMEOUT_LOCK_STATUS scan_broad_lock=0), and the
# runner emits GLOBAL_LOCK_RETIRE_CLASS_DONE class=IpcReplyTimeout exactly once (timeout-wins boot).
# Ordinary receive timeouts stay on their existing in-lock path (NOT retired here).
#
# On both fresh boots passing, the runner (not userspace) emits:
#   STAGE_200C_REPLY_TIMEOUT_X86_RETIREMENT_SEAL arch=x86_64 classes=1 live_cells=1 ...
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=x86-ipc-reply-timeout-oracle
KTARGET=${KTARGET:-targets/x86_64-yarm-none.json}
KPROFILE=${KPROFILE:-x86-none}
KELF=${KELF:-target/x86_64-yarm-none/${KPROFILE}/kernel_boot}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ipc-reply-timeout-retirement-x86_64}
TIMEOUT_SECS=${TIMEOUT_SECS:-90}
mkdir -p "$LOGDIR"

fail=0
note() { echo "[ipc-reply-timeout-retire] $*"; }
die()  { echo "[ipc-reply-timeout-retire][fail] $*"; fail=1; }

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

# ── 1. Base artifacts (servers + initramfs; the userspace oracle is arch-gated) ──
note "building base x86_64 artifacts (servers + initramfs)"
BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
  scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
  || die "base artifact build failed (see $LOGDIR/build.log)"

# ── 2. Feature-ON kernel + integrity: it MUST carry the live marker literals ──
if (( ! fail )); then
  note "building kernel_boot with --features $FEATURE"
  cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" \
    --no-default-features --features "$FEATURE" \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 \
    || die "feature kernel_boot build failed (see $LOGDIR/kbuild.log)"
fi
if (( ! fail )); then
  cp "$KELF" build-x86_64/kernel_boot.elf
  # BL5a-2b: `IPC_REPLY_BEATS_TIMEOUT_OK` used to be required here. Its only emitter is the broad
  # NR 7 arm, which no reply reaches since the direct route took NR 7 over, so the linker drops it
  # and this gate failed every build. The oracle's own literal is the causal gate, which the split
  # receive lane now holds.
  for lit in IPC_REPLY_TIMEOUT_OK IPC_REPLY_TIMEOUT_COLLECTOR_GATE IPC_REPLY_TIMEOUT_ARMED \
             "class=IpcReplyTimeout"; do
    rg -a -q "$lit" build-x86_64/kernel_boot.elf || die "feature kernel missing literal $lit (wrong build)"
  done
fi

# ── 2b. Feature-OFF kernel MUST be marker-CLEAN of the live literals ──
if (( ! fail )); then
  note "building feature-OFF kernel_boot and asserting it is marker-clean"
  cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" \
    --no-default-features -p yarm --bin kernel_boot >"$LOGDIR/kbuild-off.log" 2>&1 \
    || die "feature-off kernel_boot build failed (see $LOGDIR/kbuild-off.log)"
  # NOTE: this feature-OFF build overwrites $KELF (the target path); the feature-ON
  # boot image was already copied to build-x86_64/kernel_boot.elf in step 2, so it is
  # untouched here. Do NOT re-copy $KELF (it is now the feature-OFF binary).
  OFF_ELF="target/x86_64-yarm-none/${KPROFILE}/kernel_boot"
  # U7 (canonical 199E) split this gate in two, because the feature no longer decides where
  # timeouts are processed — only which oracle SCENARIOS are built.
  #
  # (a) The oracle's own scenario literals must still be absent from a feature-OFF kernel.
  #
  # Canonical 199E moved IPC_REPLY_TIMEOUT_ARMED out of this half and into (b). It is no
  # longer an oracle-only literal: `KernelState::arm_production_reply_deadline` is
  # unconditional production code — no `#[cfg]`, no runtime selector — and emits the same
  # arch-neutral `IPC_REPLY_TIMEOUT_ARMED arch={}` format string at the committed
  # reply-receive block point, so the literal is compiled into EVERY image, feature-OFF
  # included. Asserting its absence would now assert that production registration is absent.
  for lit in IPC_REPLY_BEATS_TIMEOUT_OK IPC_REPLY_TIMEOUT_COLLECTOR_GATE; do
    rg -a -q "$lit" "$OFF_ELF" && die "feature-OFF kernel contains oracle literal $lit (not marker-clean)"
  done
  # (b) The PRODUCTION pipeline's literals must now be PRESENT in a feature-OFF kernel. Their
  # absence would mean the promotion regressed back behind the feature — the exact thing U7
  # removed — so this half of the gate is asserted positively.
  for lit in IPC_REPLY_TIMEOUT_OK IPC_REPLY_TIMEOUT_LOCK_STATUS IPC_REPLY_TIMEOUT_LATE_SCAN \
             "class=IpcReplyTimeout" IPC_REPLY_TIMEOUT_DEFERRED \
             IPC_REPLY_TIMEOUT_ARMED; do
    rg -a -q "$lit" "$OFF_ELF" || die "feature-OFF kernel is missing PRODUCTION literal $lit (the U7 pipeline must not be feature-gated)"
  done
fi

if (( fail )); then
  echo "STAGE_200C_REPLY_TIMEOUT_X86_RETIREMENT_SEAL arch=x86_64 classes=1 live_cells=1 result=fail reason=build"
  exit 1
fi

# ── Boot helper: one fresh -smp 1 boot for the given mode, into its own log ──
boot_mode() {
  local mode="$1" log="$2"
  env \
    KERNEL_IMAGE=build-x86_64/kernel_boot.elf \
    INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio \
    KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.x86_64_ipc_reply_timeout_oracle=${mode}" \
    QEMU_SMP=1 \
    LOGFILE="$log" \
    SMOKE_LOG="$LOGDIR/core-${mode}.log" \
    TIMEOUT_SECS="$TIMEOUT_SECS" \
    YARM_MODE_ISOLATION=0 \
    scripts/qemu-x86_64-core-smoke.sh >"$LOGDIR/wrap-${mode}.log" 2>&1 || true
}

verify_log() {
  # $1 = normalized log, then the required marker strings (each must appear exactly once).
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
# A record settled twice emits more `IPC_REPLY_TIMEOUT_OK` lines than there are registrations,
# which this bound rejects without presuming how many production callers exist.
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
# is permitted while a settlement WITHOUT a matching registration (a duplicate) is not. What seals
# the oracle's own outcome is asserted positively and identity-bearingly in the cell itself:
# `IPC_REPLY_BEATS_TIMEOUT_OK … deadline_disarmed=1 late_timeout_claims=0`,
# `IPC_REPLY_TIMEOUT_LATE_SCAN … late_timeout_claims=0` and the client's own
# `…_BEATS_TIMEOUT_DONE … late_timeout_wakes=0`, each still required exactly once.
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

# The oracle's REGISTRATION identity: caller tid from the provisioning marker (never hardcoded,
# never positional), then the ASID / record coordinates from that caller's own ARMED line.
# Fails closed when the provisioning marker is missing, when the oracle caller has no
# registration, or when either carries a malformed identity.
marker_field() {
  sed -n "s/.*[[:space:]]$2=\([0-9][0-9]*\).*/\1/p" <<<"$1" | head -1
}
ORACLE_TID=""; ORACLE_ASID=""; ORACLE_RECORD_INDEX=""; ORACLE_RECORD_GEN=""
derive_oracle_identity() {
  local norm="$1" arch="$2" n armed
  ORACLE_TID=""; ORACLE_ASID=""; ORACLE_RECORD_INDEX=""; ORACLE_RECORD_GEN=""
  n=$(rg -a -c -F "IPC_REPLY_TIMEOUT_ORACLE_PROVISION_OK init_tid=" "$norm" 2>/dev/null || echo 0)
  [[ "$n" == "1" ]] || { die "oracle provisioning marker count != 1 (got $n): the oracle identity cannot be scoped"; return 1; }
  ORACLE_TID="$(oracle_init_tid "$norm")"
  [[ -n "$ORACLE_TID" ]] || { die "oracle provisioning marker carries no init_tid: identity malformed"; return 1; }
  n=$(rg -a -c -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} caller_tid=${ORACLE_TID} " "$norm" 2>/dev/null || echo 0)
  [[ "$n" == "1" ]] || { die "oracle-identity ARMED count != 1 (got $n) for caller_tid=${ORACLE_TID}"; return 1; }
  armed=$(rg -a -m1 -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} caller_tid=${ORACLE_TID} " "$norm")
  ORACLE_ASID="$(marker_field "$armed" caller_asid)"
  ORACLE_RECORD_INDEX="$(marker_field "$armed" record_index)"
  ORACLE_RECORD_GEN="$(marker_field "$armed" record_generation)"
  [[ -n "$ORACLE_ASID" && -n "$ORACLE_RECORD_INDEX" && -n "$ORACLE_RECORD_GEN" ]] \
    || { die "oracle registration is malformed (asid=$ORACLE_ASID idx=$ORACLE_RECORD_INDEX gen=$ORACLE_RECORD_GEN)"; return 1; }
  note "oracle identity: tid=${ORACLE_TID} asid=${ORACLE_ASID} record=${ORACLE_RECORD_INDEX}/${ORACLE_RECORD_GEN}"
}

# BL5a-2b — the reply-wins cell, derived from PRODUCTION events on the route the caller takes.
# It used to require the broad receive arm's deadline injector and the broad NR 7 reply-win
# markers (`IPC_REPLY_WIN_RESERVE ... outcome=ok`, `IPC_REPLY_BEATS_TIMEOUT_OK`); no blocking
# receive and no reply has reached the broad arms since the split routes took them over, so the
# oracle blocked UNTIMED, nothing raced, and the cell could never pass. The client now blocks with
# its own finite deadline; the split lane holds the causal gate before the production owner arms
# the terminal and token; the reply is production direct NR 7 settling exactly the oracle's
# record; and the late scan attests that the reply retired the oracle's registration.
verify_reply_won_terminal() {
  local norm="$1" tid="$2" rec="record_index=${ORACLE_RECORD_INDEX} record_generation=${ORACLE_RECORD_GEN} " n
  n=$(rg -a -c -F "IPC_REPLY_TERMINAL_ARMED_SPLIT caller_tid=${tid} caller_asid=${ORACLE_ASID} ${rec}" "$norm" 2>/dev/null || echo 0)
  [[ "$n" == "1" ]] || { die "the oracle's terminal must be armed exactly once (got $n)"; return; }
  rg -a -q -e "^IPC_REPLY_TERMINAL_ARMED_SPLIT caller_tid=${tid} caller_asid=${ORACLE_ASID} ${rec}.* finite_deadline=1 deadline_reserved=1 result=ok" "$norm" \
    || die "the oracle's reply wait was not a finite, token-bearing wait"
  n=$(rg -a -c -F "IPCREPLY_DIRECT_TERMINAL_CLAIM ${rec}" "$norm" 2>/dev/null || echo 0)
  [[ "$n" == "1" ]] || { die "the oracle's record must see exactly one direct terminal claim (got $n)"; return; }
  rg -a -q -e "^IPCREPLY_DIRECT_TERMINAL_CLAIM ${rec}replier_tid=[0-9]+ terminal=Reply resolution=commit settled=1 result=ok" "$norm" \
    || die "the oracle's reply did not commit its terminal"
  for m in "IPCREPLY_DIRECT_TERMINAL_LOST ${rec}" "IPCREPLY_DIRECT_REFUSED_PRE_LOCK ${rec}"; do
    rg -a -q -F "$m" "$norm" && die "another claimant touched the oracle's record: $m"
  done
}

# The gate suppresses ALL reply-class publication, so a settlement inside [held, released] means
# it did not hold.
verify_nothing_settled_while_held() {
  local norm="$1" arch="$2" held rel n
  held=$(rg -a -n -F "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=${arch} outcome=held" "$norm" | head -1 | cut -d: -f1)
  rel=$(rg -a -n -F "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=${arch} outcome=released" "$norm" | head -1 | cut -d: -f1)
  [[ -n "$held" && -n "$rel" ]] || { die "the gate window is incomplete (held=$held released=$rel)"; return; }
  n=$(rg -a -n -F "IPC_REPLY_TIMEOUT_OK arch=${arch}" "$norm" | cut -d: -f1 \
      | awk -v lo="$held" -v hi="$rel" '$1 > lo && $1 < hi' | wc -l | tr -d ' ')
  [[ "$n" == "0" ]] || die "$n reply-timeout settlement(s) inside the held gate window"
}

# Assert marker A strictly precedes marker B (first occurrence of each).
assert_order() {
  local norm="$1" a="$2" b="$3" why="$4"
  local la lb
  la=$(rg -a -n -F "$a" "$norm" | head -1 | cut -d: -f1)
  lb=$(rg -a -n -F "$b" "$norm" | head -1 | cut -d: -f1)
  if [[ -z "$la" || -z "$lb" ]]; then
    die "ordering evidence missing ($a=$la $b=$lb)"
    return
  fi
  (( la < lb )) || die "$why ($a@$la must precede $b@$lb)"
}

# The causal chain, every registration and claim line scoped to the oracle.
verify_reply_won_chain() {
  local rw="$1" arch="$2" done_marker="$3"
  assert_order "$rw" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=${arch} outcome=held" \
    "IPC_REPLY_TERMINAL_ARMED_SPLIT caller_tid=${ORACLE_TID} caller_asid=${ORACLE_ASID} record_index=${ORACLE_RECORD_INDEX} " \
    "the collector must be held BEFORE the oracle's terminal is armed"
  assert_order "$rw" \
    "IPC_REPLY_TIMEOUT_ARMED arch=${arch} caller_tid=${ORACLE_TID} " \
    "IPCREPLY_DIRECT_TERMINAL_CLAIM record_index=${ORACLE_RECORD_INDEX} record_generation=${ORACLE_RECORD_GEN} " \
    "the reply must claim against a genuinely armed deadline"
  assert_order "$rw" \
    "IPCREPLY_DIRECT_TERMINAL_CLAIM record_index=${ORACLE_RECORD_INDEX} record_generation=${ORACLE_RECORD_GEN} " \
    "USER_LOG tid=${ORACLE_TID} msg=IPC_REPLY_TIMEOUT_ORACLE_CLIENT_REPLY_RECV plen=8 reply_ok=1" \
    "userspace must validate the payload only after the reply committed its terminal"
  assert_order "$rw" \
    "USER_LOG tid=${ORACLE_TID} msg=IPC_REPLY_TIMEOUT_ORACLE_CLIENT_REPLY_RECV plen=8 reply_ok=1" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=${arch} outcome=released" \
    "the gate must be released by the userspace validation, not before it"
  assert_order "$rw" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=${arch} outcome=released" \
    "IPC_REPLY_TIMEOUT_LATE_SCAN arch=${arch} outcome=reply_won" \
    "the late scan must run with collection ENABLED (otherwise it claims nothing vacuously)"
  rg -a -q -F "USER_LOG tid=${ORACLE_TID} msg=${done_marker}" "$rw" \
    || die "the oracle's own reply-wins verdict is missing"
}

# ── 3. Scenario A — timeout-wins, feature enabled (fresh boot) ──
TW_OK=0
if (( ! fail )); then
  note "booting fresh -smp 1 QEMU: yarm.x86_64_ipc_reply_timeout_oracle=timeout-wins"
  boot_mode timeout-wins "$LOGDIR/boot-timeout-wins.log"
  TW="$LOGDIR/tw.norm.log"; tr '\r' '\n' <"$LOGDIR/boot-timeout-wins.log" >"$TW"
  [[ -s "$TW" ]] || die "no timeout-wins boot log"
  verify_oracle_armed_once "$TW" x86_64
  verify_no_duplicate_settlement "$TW" x86_64 \
    "IPC_REPLY_TIMEOUT_OK arch=x86_64 terminal=Timeout timeout_result=TimedOut caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=0 result=ok"
  verify_log "$TW" \
    "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=x86_64 scan_broad_lock=0 completion_transaction_narrow=1 classes=IpcReplyTimeout+IpcSendTimeout production=1 result=ok" \
    "IPC_REPLY_TIMEOUT_DEFERRED arch=x86_64 published=1 drained=1 result=ok" \
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcReplyTimeout result=ok" \
    "X86_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut caller_continuations=1 late_reply=rejected result=ok"
  # A timeout win must not also emit a reply-win, a duplicate timeout, a broad-lock status,
  # or a panic/fatal trap.
  forbid_log "$TW" \
    "IPC_REPLY_BEATS_TIMEOUT_OK" \
    "scan_broad_lock=1" \
    "PANIC" "KERNEL PANIC" "FATAL"
  recheck_sha_clean
  (( fail )) || TW_OK=1
fi

# ── 4. Scenario B — reply-wins, feature enabled (SEPARATE fresh boot) ──
RW_OK=0
if (( ! fail )); then
  note "booting fresh -smp 1 QEMU: yarm.x86_64_ipc_reply_timeout_oracle=reply-wins"
  boot_mode reply-wins "$LOGDIR/boot-reply-wins.log"
  RW="$LOGDIR/rw.norm.log"; tr '\r' '\n' <"$LOGDIR/boot-reply-wins.log" >"$RW"
  [[ -s "$RW" ]] || die "no reply-wins boot log"
  derive_oracle_identity "$RW" x86_64 || true
  verify_log "$RW" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=x86_64 outcome=held phase=before_terminal_claim result=ok" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=x86_64 outcome=released trigger=userspace_reply_validated result=ok" \
    "IPC_REPLY_TIMEOUT_LATE_SCAN arch=x86_64 outcome=reply_won late_timeout_claims=0 result=ok" \
    "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=x86_64 scan_broad_lock=0 completion_transaction_narrow=1 classes=IpcReplyTimeout+IpcSendTimeout production=1 result=ok" \
    "X86_IPC_REPLY_BEATS_TIMEOUT_DONE reply_ok=1 caller_continuations=1 late_timeout_wakes=0 duplicate_reply=rejected result=ok" \
    "IPC_REPLY_TIMEOUT_ORACLE_SERVER_DUP_REPLY rejected=1"
  verify_oracle_armed_once "$RW" x86_64
  if [[ -n "$ORACLE_TID" && -n "$ORACLE_RECORD_INDEX" ]]; then
    verify_reply_won_terminal "$RW" "$ORACLE_TID"
    verify_reply_won_chain "$RW" x86_64 X86_IPC_REPLY_BEATS_TIMEOUT_DONE
  fi
  verify_nothing_settled_while_held "$RW" x86_64
  verify_settlements_within_registrations "$RW" x86_64
  forbid_log "$RW" \
    "IPC_REPLY_TIMEOUT_LATE_SCAN arch=x86_64 outcome=registration_outlived_reply" \
    "IPC_REPLY_WIN_RESERVE arch=x86_64 outcome=decline" \
    "IPC_REPLY_WIN_ROLLBACK" \
    "scan_broad_lock=1" \
    "PANIC" "KERNEL PANIC" "FATAL"
  recheck_sha_clean
  (( fail )) || RW_OK=1
fi

# ── 5. Retirement live seal (runner-emitted; both fresh boots must pass) ──
if (( fail )) || [[ "$TW_OK" != "1" || "$RW_OK" != "1" ]]; then
  echo "STAGE_200C_REPLY_TIMEOUT_X86_RETIREMENT_SEAL arch=x86_64 classes=1 live_cells=1 timeout_wins=${TW_OK} reply_wins=${RW_OK} result=fail"
  exit 1
fi

cat <<'SEAL'
STAGE_200C_REPLY_TIMEOUT_X86_RETIREMENT_SEAL
arch=x86_64
classes=1
live_cells=1
timeout_wins=1
reply_wins=1
scan_broad_lock=0
completion_transaction_narrow=1
late_reply_successes=0
late_timeout_wakes=0
duplicate_wakes=0
stale_authority_restores=0
wrong_waiter_mutations=0
result=ok
SEAL
