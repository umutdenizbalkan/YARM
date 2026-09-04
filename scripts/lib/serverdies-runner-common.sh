#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 200D-2B1C — shared body for the three ServerDies exact-commit runners.
#
# The three ports differ only in target/profile/feature/selector and the arch tag inside the
# kernel-side markers; the PROOF is identical, so it lives here once rather than being
# triplicated and drifting. Each arch runner sources this file and calls `serverdies_main`.
#
# Two runs, in order:
#   RUN_A  feature-off preservation — the image must carry NO oracle literal, while every
#          production server-death literal remains present (the mechanism is not gated).
#   RUN_B  feature-on live cell     — one fresh boot in which the authorized replier exits
#          without replying and the caller regains liveness through PeerDeath/ServerDied.
#
# This file performs NO boot of its own and claims NO live cell; it is the readiness harness
# that Stage 200D-2B1C prepares. Running it is a later stage's act.

set -uo pipefail

# ── exact-commit identity ──────────────────────────────────────────────────────────────
# A runner that cannot name the exact tree it proved is not evidence. The SHA and tree are
# captured before any build and re-checked after every phase; any drift or dirt fails closed.
serverdies_freeze_commit() {
  if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "STAGE_200D2B1C_${ARCH_TAG^^}_SERVER_DIES_SEAL result=fail reason=dirty_tree"
    exit 1
  fi
  SHA0=$(git rev-parse HEAD)
  TREE0=$(git rev-parse HEAD^{tree})
  note "exact commit sha=$SHA0 tree=$TREE0 (clean)"
}

serverdies_recheck_commit() {
  local what="$1"
  [[ "$(git rev-parse HEAD)" == "$SHA0" ]] || die "[$what] SHA drifted"
  [[ "$(git rev-parse HEAD^{tree})" == "$TREE0" ]] || die "[$what] tree hash drifted"
  git diff --quiet && git diff --cached --quiet || die "[$what] tree became dirty"
}

# ── the marker contract ────────────────────────────────────────────────────────────────
# Ordered: each must appear, and after the one before it. This is the causal chain from the
# server's own exit through the post-lock drain to the caller's validated ServerDied, and on
# to quiescence.
#
# ORDER CORRECTION (first real run of this prepared runner): `IPC_SERVER_DEATH_OK` was
# listed AFTER `IPC_SERVER_DEATH_USER_VALIDATED`, which is causally impossible. `_OK` is the
# KERNEL's completion attestation, emitted inside `complete_server_death_over` immediately
# after the caller enqueue; `USER_VALIDATED` is a ring-3 `USER_LOG` the caller can only emit
# BECAUSE that completion enqueued and the scheduler then dispatched it. The already-sealed
# reply-timeout runner uses the same shape — kernel `IPC_REPLY_TIMEOUT_OK` first, userspace
# `..._DONE` after. The earlier attempts never reached the caller wake, so the inversion was
# never exposed. Nothing about the SET of required markers was weakened by the correction;
# the tail below adds the quiescent attestations rather than removing anything.
#
# 199D-SD3 (§4) — SCENARIO SCOPE. The chain used to be matched by identity-free literals,
# and two of its markers are emitted by EVERY task exit in the boot, not only by the
# scenario's: the deferred RESERVATION and the architecture's exit-disposition consumption.
# On a boot where any unrelated task exits first — the fork/COW proof child does, thousands
# of lines earlier — the "first occurrence" the order check compared against belonged to
# that exit, and the run failed on two markers that were entirely correct.
#
# The chain is therefore matched against the WITNESSED transaction: the dying server
# `{tid, asid}`, the reply record `{index, generation}` its reverse link named, and the
# caller `{tid, asid}` the completion was published for. `serverdies_resolve_identity`
# reads all of that out of the log and requires the completion to name the same reply
# record the captured link did, so the identity is a causal join rather than an assumption
# that only one scenario is present.
#
# (The terminal cell's epoch is not exposed in any live marker; the reply record's
# generation is the incarnation discriminator that is, and it is what every scoped literal
# below carries.)
serverdies_required_markers() {
  cat <<MARKERS
IPC_SERVER_DEATH_REQUEST_RECEIVED
IPC_SERVER_DEATH_REPLY_CAP_RECEIVED
IPC_SERVER_DEATH_EXIT_ENTERED nr=16 role=server
EXIT_TASK_SPLIT_ENTER tid=${SD_SERVER_TID} asid=${SD_SERVER_ASID} result=ok
IPC_SERVER_DEATH_DEFERRED_RESERVED server_tid=${SD_SERVER_TID} server_asid=${SD_SERVER_ASID}
IPC_SERVER_DEATH_SCOPE_ARMED record_index=${SD_RECORD_INDEX} record_generation=${SD_RECORD_GENERATION} server_tid=${SD_SERVER_TID} server_asid=${SD_SERVER_ASID} link_present=1
IPC_SERVER_DEATH_LINK_CAPTURED server_tid=${SD_SERVER_TID} server_asid=${SD_SERVER_ASID} record_index=${SD_RECORD_INDEX} record_generation=${SD_RECORD_GENERATION}
IPC_SERVER_DEATH_DEFERRED_PUBLISHED server_tid=${SD_SERVER_TID} server_asid=${SD_SERVER_ASID} record_index=${SD_RECORD_INDEX} record_generation=${SD_RECORD_GENERATION}
EXIT_TASK_CLAIM_RETIRED tid=${SD_SERVER_TID} asid=${SD_SERVER_ASID}
IPC_SERVER_DEATH_BROAD_LOCK_RELEASED
IPC_SERVER_DEATH_POST_LOCK_DRAIN_BEGIN
IPC_SERVER_DEATH_TERMINAL_CLAIM terminal=PeerDeath result=won record_index=${SD_RECORD_INDEX} record_generation=${SD_RECORD_GENERATION} caller_tid=${SD_CALLER_TID} caller_asid=${SD_CALLER_ASID}
IPC_SERVER_DEATH_COMPLETION_COMMITTED code=10 caller_tid=${SD_CALLER_TID} caller_asid=${SD_CALLER_ASID} record_index=${SD_RECORD_INDEX} record_generation=${SD_RECORD_GENERATION}
IPC_SERVER_DEATH_CALLER_ENQUEUED caller_tid=${SD_CALLER_TID} caller_asid=${SD_CALLER_ASID}
IPC_SERVER_DEATH_OK
IPC_SERVER_DEATH_TRANSITION_AUDIT vector=[1, 1, 1, 1, 1, 1, 1, 1, 1] result_before_enqueue=1 result=ok
IPC_SERVER_DEATH_USER_VALIDATED result=ServerDied code=10
IPC_SERVER_DEATH_SURVIVOR_PROGRESS_OK
IPC_SERVER_DEATH_SYSTEM_HEALTH_OK
IPC_SERVER_DEATH_LINK_BALANCE_QUIESCENT
MARKERS
}

# One `key=value` field out of one marker line. Anchored on the whitespace before the key,
# so `record_index` never matches `reply_record_index`.
serverdies_field() {
  printf '%s' "$1" | tr -d '\r' | sed -n "s/.*[[:space:]]$2=\([^[:space:]]*\).*/\1/p"
}

# Resolve the witnessed transaction's identity from the log, failing closed.
#
# Both anchor lines must be unique — more than one captured link or more than one published
# completion in a single boot is itself a defect, not something to pick a winner from — and
# the completion must name the SAME reply record the captured link did.
serverdies_resolve_identity() {
  local log=$1 captured committed n f
  n=$(grep -c -F "IPC_SERVER_DEATH_LINK_CAPTURED " "$log" || true)
  [[ "$n" == "1" ]] || { die "RUN_B expected exactly one captured reverse link, saw $n"; return 1; }
  n=$(grep -c -F "IPC_SERVER_DEATH_COMPLETION_COMMITTED " "$log" || true)
  [[ "$n" == "1" ]] || { die "RUN_B expected exactly one committed completion, saw $n"; return 1; }
  captured=$(grep -m1 -F "IPC_SERVER_DEATH_LINK_CAPTURED " "$log")
  committed=$(grep -m1 -F "IPC_SERVER_DEATH_COMPLETION_COMMITTED " "$log")
  SD_SERVER_TID=$(serverdies_field "$captured" server_tid)
  SD_SERVER_ASID=$(serverdies_field "$captured" server_asid)
  SD_RECORD_INDEX=$(serverdies_field "$captured" record_index)
  SD_RECORD_GENERATION=$(serverdies_field "$captured" record_generation)
  SD_CALLER_TID=$(serverdies_field "$committed" caller_tid)
  SD_CALLER_ASID=$(serverdies_field "$committed" caller_asid)
  for f in "$SD_SERVER_TID" "$SD_SERVER_ASID" "$SD_RECORD_INDEX" "$SD_RECORD_GENERATION" \
           "$SD_CALLER_TID" "$SD_CALLER_ASID"; do
    [[ "$f" =~ ^[0-9]+$ ]] || { die "RUN_B could not resolve the witnessed identity"; return 1; }
  done
  if [[ "$(serverdies_field "$committed" record_index)" != "$SD_RECORD_INDEX" ]] \
     || [[ "$(serverdies_field "$committed" record_generation)" != "$SD_RECORD_GENERATION" ]]; then
    die "RUN_B completion names a different reply record than the captured link"
    return 1
  fi
  note "RUN_B witnessed scenario: server={${SD_SERVER_TID},${SD_SERVER_ASID}} caller={${SD_CALLER_TID},${SD_CALLER_ASID}} record={${SD_RECORD_INDEX},${SD_RECORD_GENERATION}}"
}

# Any of these in a live log is fatal: the server returned from NR16, a wrong incarnation was
# accepted, a competing claimant won, or an accounting relationship broke.
serverdies_forbidden_markers() {
  cat <<FORBIDDEN
IPC_SERVER_DEATH_EXIT_RETURNED
IPC_SERVER_DEATH_WRONG_SERVER_IDENTITY
IPC_SERVER_DEATH_WRONG_RECORD_GENERATION
IPC_SERVER_DEATH_WRONG_CALLER_IDENTITY
IPC_SERVER_DEATH_WRONG_ENDPOINT_GENERATION
IPC_SERVER_DEATH_WRONG_TIMEOUT_GENERATION
IPC_SERVER_DEATH_DUPLICATE_COMPLETION
IPC_SERVER_DEATH_DUPLICATE_WAKE
IPC_SERVER_DEATH_DUPLICATE_DEFERRED
IPC_SERVER_DEATH_DUPLICATE_TRANSITION
IPC_SERVER_DEATH_TRANSITION_COUNT
IPC_SERVER_DEATH_SCOPE_CONFLICT
IPC_SERVER_DEATH_SCOPE_UNARMED
IPC_SERVER_DEATH_LINK_LEAK
IPC_SERVER_DEATH_RECORD_LEAK
IPC_SERVER_DEATH_DEFERRED_LEAK
IPC_SERVER_DEATH_TIMEOUT_WON
IPC_SERVER_DEATH_LATE_REPLY_ACCEPTED
IPC_SERVER_DEATH_STALE_AUTHORITY_RESTORED
EXIT_TASK_EXITING_STILL_CURRENT
EXIT_TASK_WRONG_IDENTITY
EXIT_TASK_RESELECTED_EXITING_TASK
FORBIDDEN
}

# The oracle-gated literals that a FEATURE-OFF image must not contain...
serverdies_oracle_literals() {
  cat <<ORACLE
IPC_REPLY_TIMEOUT_COLLECTOR_GATE
IPC_SERVER_DEATH_LINK_LEAK
IPC_SERVER_DEATH_RECORD_LEAK
IPC_SERVER_DEATH_DEFERRED_LEAK
IPC_SERVER_DEATH_DUPLICATE_COMPLETION
IPC_SERVER_DEATH_DUPLICATE_WAKE
IPC_SERVER_DEATH_DUPLICATE_TRANSITION
IPC_SERVER_DEATH_TRANSITION_COUNT
IPC_SERVER_DEATH_TRANSITION_AUDIT
IPC_SERVER_DEATH_TIMEOUT_WON
IPC_SERVER_DEATH_LATE_REPLY_ACCEPTED
IPC_SERVER_DEATH_STALE_AUTHORITY_RESTORED
IPC_SERVER_DEATH_WRONG_TIMEOUT_GENERATION
IPC_SERVER_DEATH_LATE_TIMEOUT_SCANNED
IPC_SERVER_DEATH_LINK_BALANCE_QUIESCENT
IPC_SERVER_DEATH_LINK_BALANCE_DEFERRED
IPC_SERVER_DEATH_SCOPE_ARMED
IPC_SERVER_DEATH_SCOPE_CONFLICT
IPC_SERVER_DEATH_SCOPE_UNARMED
IPC_SERVER_DEATH_FOREIGN_LINK_CLOSE
IPC_SERVER_DEATH_FOREIGN_TRANSITION
ORACLE
}

# ...while the production mechanism's own literals must SURVIVE, because server death is not
# an oracle feature. This is the half that makes the audit meaningful rather than vacuous.
serverdies_production_literals() {
  cat <<PROD
IPC_SERVER_DEATH_DEFERRED_RESERVED
IPC_SERVER_DEATH_LINK_CAPTURED
IPC_SERVER_DEATH_DEFERRED_PUBLISHED
IPC_SERVER_DEATH_BROAD_LOCK_RELEASED
IPC_SERVER_DEATH_POST_LOCK_DRAIN_BEGIN
IPC_SERVER_DEATH_TERMINAL_CLAIM
IPC_SERVER_DEATH_COMPLETION_COMMITTED
IPC_SERVER_DEATH_CALLER_ENQUEUED
IPC_SERVER_DEATH_OK
PROD
}

# ── RUN_A: feature-off binary audit ────────────────────────────────────────────────────
serverdies_run_a_feature_off_audit() {
  note "RUN_A feature-off binary audit"
  cargo +nightly build -Z build-std="$BUILD_STD" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" --no-default-features \
    -p yarm --bin kernel_boot >"$LOGDIR/build-off.log" 2>&1 \
    || { die "RUN_A feature-off kernel build failed"; return; }
  serverdies_recheck_commit "RUN_A build"

  local syms leaked=0 missing=0 n
  syms=$(strings -a "$KELF")
  while read -r n; do
    [[ -z "$n" ]] && continue
    if grep -qF "$n" <<<"$syms"; then die "RUN_A oracle literal present feature-off: $n"; leaked=$((leaked + 1)); fi
  done < <(serverdies_oracle_literals)
  while read -r n; do
    [[ -z "$n" ]] && continue
    if ! grep -qF "$n" <<<"$syms"; then die "RUN_A production literal MISSING feature-off: $n"; missing=$((missing + 1)); fi
  done < <(serverdies_production_literals)
  note "RUN_A oracle_literals_present=$leaked production_literals_missing=$missing"
}

# ── RUN_B: feature-on live cell ────────────────────────────────────────────────────────
# Builds the oracle-on image and boots it ONCE with the ServerDies selector, then grades the
# log. Fresh artifacts and a fresh log every time — a stale log can never be re-graded.
serverdies_run_b_live_cell() {
  note "RUN_B feature-on live cell (selector=server-dies)"
  cargo +nightly build -Z build-std="$BUILD_STD" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" --no-default-features \
    --features "$FEATURE" -p yarm --bin kernel_boot >"$LOGDIR/build-on.log" 2>&1 \
    || { die "RUN_B feature-on kernel build failed"; return; }
  serverdies_recheck_commit "RUN_B build"

  # Stage the bootable artifacts if the port needs a staging step (x86_64 boots a staged
  # kernel + initramfs, not the raw build product). Ports without one boot directly.
  if declare -F serverdies_stage_boot_artifacts >/dev/null; then
    serverdies_stage_boot_artifacts || { die "RUN_B artifact staging failed"; return; }
    serverdies_recheck_commit "RUN_B stage"
  fi

  local log="$LOGDIR/run-b.log"
  rm -f "$log"
  note "boot cmdline: ${SELECTOR}=server-dies"
  if ! serverdies_boot_once "$log"; then
    die "RUN_B boot did not complete"
    return
  fi
  serverdies_recheck_commit "RUN_B boot"

  # Single-boot witnesses: the log must be from THIS boot, not an accumulation.
  local banners
  banners=$(grep -c "YARM_BOOT_OK" "$log" || true)
  [[ "$banners" == "1" ]] || die "RUN_B expected exactly one boot banner, saw $banners"

  # 199D-SD3 (§4): resolve the witnessed transaction before grading anything against it.
  # A boot that cannot name its own scenario has already failed, so this returns early
  # rather than grading an identity-free chain as a fallback.
  serverdies_resolve_identity "$log" || return

  # Ordered required markers, each scoped to the witnessed transaction where the marker is
  # one that other exits also emit.
  local prev=0 line idx
  while read -r line; do
    [[ -z "$line" ]] && continue
    idx=$(grep -n -m1 -F "$line" "$log" | cut -d: -f1 || true)
    if [[ -z "$idx" ]]; then die "RUN_B required marker missing: $line"; continue; fi
    if (( idx < prev )); then die "RUN_B marker out of order: $line"; fi
    prev=$idx
  done < <(serverdies_required_markers)

  # 199D-SD3 (§4): scoping the chain must not cost duplicate detection. The two markers that
  # every exit emits are now matched by the witnessed server's identity, so a SECOND
  # occurrence carrying that same identity is a genuine repeat of the scenario's own exit and
  # must still fail. (The kernel's own nine-vector answers the same question from the other
  # side: `TRANSITION_AUDIT` above is required to read exactly all-ones, and both
  # `DUPLICATE_TRANSITION` and `TRANSITION_COUNT` are forbidden.)
  local dup
  for line in \
    "IPC_SERVER_DEATH_DEFERRED_RESERVED server_tid=${SD_SERVER_TID} server_asid=${SD_SERVER_ASID}" \
    "EXIT_TASK_CLAIM_RETIRED tid=${SD_SERVER_TID} asid=${SD_SERVER_ASID}"; do
    dup=$(grep -c -F "$line" "$log" || true)
    [[ "$dup" == "1" ]] || die "RUN_B scoped marker seen $dup times (expected 1): $line"
  done

  # U9-EXIT1 §6 — the dying server's exit no longer reaches the terminal broad dispatcher, so
  # `EXIT_TASK_DISPOSITION_CONSUMED` (which only the in-lock consumer emits) is re-derived above
  # into the pair the split route emits for the SAME incarnation: the edge marker at the route's
  # entry, and the retired claim. Two facts that marker used to carry are asserted directly here
  # rather than left implied — the broad edge counted zero, and the retired claim states that this
  # exit actually owed and handed off a server-death completion.
  local broad_edges retired
  broad_edges=$(grep -c -F "EXIT_TASK_BROAD_ENTER" "$log" || true)
  [[ "$broad_edges" == "0" ]] \
    || die "RUN_B the dying server's NR 16 still reached the terminal broad dispatcher ($broad_edges)"
  retired=$(grep -m1 -F "EXIT_TASK_CLAIM_RETIRED tid=${SD_SERVER_TID} asid=${SD_SERVER_ASID} " "$log" || true)
  [[ -n "$retired" ]] || die "RUN_B no retired exit claim for the witnessed server"
  case "$retired" in
    *"server_death=1"*) ;;
    *) die "RUN_B the retired claim does not attest the server-death handoff: $retired" ;;
  esac

  # Forbidden markers.
  while read -r line; do
    [[ -z "$line" ]] && continue
    grep -qF "$line" "$log" && die "RUN_B forbidden marker present: $line"
  done < <(serverdies_forbidden_markers)

  # Exactly one caller wake, one terminal winner.
  local wakes winners
  wakes=$(grep -c "IPC_SERVER_DEATH_CALLER_ENQUEUED" "$log" || true)
  winners=$(grep -c "IPC_SERVER_DEATH_TERMINAL_CLAIM terminal=PeerDeath result=won" "$log" || true)
  [[ "$wakes" == "1" ]] || die "RUN_B expected exactly one caller enqueue, saw $wakes"
  [[ "$winners" == "1" ]] || die "RUN_B expected exactly one PeerDeath winner, saw $winners"
  note "RUN_B caller_wakes=$wakes peer_death_winners=$winners"
}

serverdies_main() {
  : "${ARCH_TAG:?arch runner must set ARCH_TAG}"
  : "${KTARGET:?arch runner must set KTARGET}"
  : "${KPROFILE:?arch runner must set KPROFILE}"
  : "${KELF:?arch runner must set KELF}"
  : "${FEATURE:?arch runner must set FEATURE}"
  : "${SELECTOR:?arch runner must set SELECTOR}"
  BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
  LOGDIR=${LOGDIR:-/tmp/serverdies-$ARCH_TAG}
  fail=0
  note() { echo "[serverdies:$ARCH_TAG] $*"; }
  die() { echo "[serverdies:$ARCH_TAG][fail] $*"; fail=1; }

  rm -rf "$LOGDIR"; mkdir -p "$LOGDIR"
  serverdies_freeze_commit
  serverdies_run_a_feature_off_audit
  serverdies_run_b_live_cell
  serverdies_recheck_commit "final"

  if [[ "$fail" == "0" ]]; then
    echo "STAGE_200D2B1C_${ARCH_TAG^^}_SERVER_DIES_SEAL arch=${ARCH_TAG} sha=${SHA0} tree=${TREE0} live_cells=1 caller_wakes=1 peer_death_winners=1 exit_returns=0 feature_off_oracle_literals=0 result=ok"
  else
    echo "STAGE_200D2B1C_${ARCH_TAG^^}_SERVER_DIES_SEAL arch=${ARCH_TAG} result=fail"
    exit 1
  fi
}
