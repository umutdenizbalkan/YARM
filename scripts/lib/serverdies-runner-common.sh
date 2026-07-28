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
# server's own exit through the post-lock drain to the caller's validated ServerDied.
serverdies_required_markers() {
  cat <<MARKERS
IPC_SERVER_DEATH_REQUEST_RECEIVED
IPC_SERVER_DEATH_REPLY_CAP_RECEIVED
IPC_SERVER_DEATH_EXIT_ENTERED nr=16 role=server
IPC_SERVER_DEATH_DEFERRED_RESERVED
IPC_SERVER_DEATH_LINK_CAPTURED
IPC_SERVER_DEATH_DEFERRED_PUBLISHED
EXIT_TASK_DISPOSITION_CONSUMED arch=${ARCH_TAG}
IPC_SERVER_DEATH_BROAD_LOCK_RELEASED
IPC_SERVER_DEATH_POST_LOCK_DRAIN_BEGIN
IPC_SERVER_DEATH_TERMINAL_CLAIM terminal=PeerDeath result=won
IPC_SERVER_DEATH_COMPLETION_COMMITTED
IPC_SERVER_DEATH_CALLER_ENQUEUED
IPC_SERVER_DEATH_USER_VALIDATED result=ServerDied code=10
IPC_SERVER_DEATH_OK
MARKERS
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

  # Ordered required markers.
  local prev=0 line idx
  while read -r line; do
    [[ -z "$line" ]] && continue
    idx=$(grep -n -m1 -F "$line" "$log" | cut -d: -f1 || true)
    if [[ -z "$idx" ]]; then die "RUN_B required marker missing: $line"; continue; fi
    if (( idx < prev )); then die "RUN_B marker out of order: $line"; fi
    prev=$idx
  done < <(serverdies_required_markers)

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
