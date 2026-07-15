#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198B1 Part B — COMBINED RETIREMENT SEAL (Model 1: serialized master).
#
# Runs the functional cohort seals STRICTLY SEQUENTIALLY — first-cohort
# (12 cells), second-cohort plain (6 cells), second-cohort ordinary-cap (6
# cells), second-cohort reply-cap-direct (3 cells), and the supervisor
# crash-restart baseline — never concurrently. Serialization is the isolation model: only ONE
# QEMU runs at a time, so there is no CPU/memory starvation (the root cause of
# the Stage 198B AArch64-enqueue 5/6 partial and the first-cohort exit-124
# timeout) and no shared log/artifact/socket contention. Each seal gets a
# unique per-run LOGDIR so repeated runs cannot cross-contaminate.
#
# Prints one line per seal and a final COMBINED_RETIREMENT_SEAL verdict.
set -euo pipefail
cd "$(dirname "$0")/.."

RUN_ID="${RUN_ID:-$(date +%s)-$$}"
LOGROOT="${LOGROOT:-/tmp/combined-retirement-seal/${RUN_ID}}"
mkdir -p "$LOGROOT"
note() { echo "[combined-seal] $*"; }

overall=0      # any cell failed (gates COMBINED_RETIREMENT_SEAL)
func_fail=0    # any of the 4 FUNCTIONAL cohorts failed (gates SECOND_COHORT_PROGRESS)

run_seal() { # run_seal <name> <script> <expected-final-marker> [class] [extra-env...]
  local name="$1" script="$2" expect="$3" class="${4:-func}"
  shift 4 2>/dev/null || shift $#
  local extra_env=("$@")
  local logdir="$LOGROOT/$name"
  local log="$LOGROOT/${name}.log"
  mkdir -p "$logdir"
  note "── running $name (serial; LOGDIR=$logdir) ──"
  local rc=0
  # ORACLE_RUN_ID keys the oracle wrapper's scratch dir uniquely per seal+run.
  # `env "${extra_env[@]}" bash …` applies any per-cell overrides (e.g. a larger
  # crash-restart wall-clock budget); an empty array degrades to `env bash …`.
  LOGDIR="$logdir" ORACLE_RUN_ID="${RUN_ID}-${name}" QEMU_SMOKE_STRICT=1 \
    env "${extra_env[@]}" bash "$script" > "$log" 2>&1 || rc=$?
  if grep -qa -- "$expect" "$log"; then
    note "$name: OK ($expect)"
    echo "COMBINED_SEAL_CELL name=$name result=ok"
  else
    note "$name: FAIL (rc=$rc; expected '$expect') — see $log"
    echo "COMBINED_SEAL_CELL name=$name result=fail rc=$rc"
    overall=1
    [[ "$class" == "func" ]] && func_fail=1
  fi
  # A timeout manifests as exit 124 from the inner `timeout` wrapper; surface it.
  if [[ "$rc" -eq 124 ]]; then
    note "$name: TIMEOUT (exit 124)"
    echo "COMBINED_SEAL_CELL name=$name result=timeout"
    overall=1
    [[ "$class" == "func" ]] && func_fail=1
  fi
}

# The crash-restart baseline drives the LONGEST single QEMU run in the battery
# (~414k serial lines to reach DEGRADED_TERMINAL_APPLY_OK). Run it FIRST, while
# the container still has full CPU burst headroom, and give it a generous
# wall-clock budget: late in a long serialized run the host can throttle to
# baseline and the default 240s truncates the chain mid-restart (a wall-clock
# artifact, not a missing transition).
run_seal crash_restart  scripts/qemu-supervisor-crash-restart-smoke.sh \
  "SUPERVISOR_CRASH_RESTART_BASELINE .*result=ok" baseline TIMEOUT_SECS=600

run_seal first_cohort   scripts/qemu-first-cohort-retirement-seal.sh \
  "FIRST_COHORT_CROSS_ARCH_SEAL arches=3 classes=4 result=ok" func
run_seal plain          scripts/qemu-second-cohort-plain-seal.sh \
  "SECOND_COHORT_PLAIN_SEAL arches=3 classes=2 live_cells=6 result=ok" func
run_seal ordinary_cap   scripts/qemu-second-cohort-ordinary-cap-seal.sh \
  "SECOND_COHORT_ORDINARY_CAP_SEAL arches=3 classes=2 live_cells=6 result=ok" func
run_seal reply_cap_direct scripts/qemu-second-cohort-reply-cap-direct-seal.sh \
  "SECOND_COHORT_REPLY_CAP_DIRECT_SEAL arches=3 classes=1 live_cells=3 result=ok" func

# Second-cohort progress reflects ONLY the four functional cohorts; it is not
# gated on the crash-restart baseline preservation cell.
if [[ "$func_fail" -eq 0 ]]; then
  echo "SECOND_COHORT_PROGRESS first=12 plain=6 ordinary_cap=6 reply_cap_direct=3 result=ok"
fi

if [[ "$overall" -eq 0 ]]; then
  echo "COMBINED_RETIREMENT_SEAL first=12 plain=6 ordinary_cap=6 reply_cap_direct=3 crash_restart=ok result=ok"
  exit 0
fi
echo "COMBINED_RETIREMENT_SEAL result=fail"
exit 1
