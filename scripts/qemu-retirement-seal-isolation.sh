#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198B1 Part B — RETIREMENT SEAL ISOLATION proof.
#
# Runs the complete combined retirement seal (Model 1 serialized master) THREE
# consecutive times, each in its own log root, and asserts:
#   - all three runs exit 0,
#   - no run hit a timeout (inner exit 124 → COMBINED_SEAL_CELL result=timeout),
#   - every expected cohort cell appears exactly once per run (no missing, no
#     duplicate → no cross-run contamination).
#
# Emits: RETIREMENT_SEAL_ISOLATION repeated_runs=3 contaminated_runs=0
#        timeout_runs=0 result=ok
set -euo pipefail
cd "$(dirname "$0")/.."

RUNS="${RUNS:-3}"
LOGROOT="${LOGROOT:-/tmp/retirement-seal-isolation}"
mkdir -p "$LOGROOT"
note() { echo "[isolation] $*"; }

timeout_runs=0
contaminated_runs=0
failed_runs=0

for i in $(seq 1 "$RUNS"); do
  run_log="$LOGROOT/combined-run-${i}.log"
  note "── combined seal run ${i}/${RUNS} ──"
  rc=0
  RUN_ID="isolation-run${i}-$(date +%s)" LOGROOT="$LOGROOT/run${i}" \
    bash scripts/qemu-combined-retirement-seal.sh > "$run_log" 2>&1 || rc=$?

  # Timeout?
  if grep -qa "result=timeout" "$run_log"; then
    note "run ${i}: TIMEOUT observed"
    timeout_runs=$((timeout_runs + 1))
  fi

  # Each of the three cohort cells must appear exactly ONCE with result=ok.
  run_ok=1
  for cell in first_cohort plain ordinary_cap; do
    cell_count="$(grep -ca "COMBINED_SEAL_CELL name=${cell} result=ok" "$run_log" || true)"
    if [[ "$cell_count" -ne 1 ]]; then
      note "run ${i}: cell '${cell}' appeared ${cell_count} times (expected exactly 1) — contamination/miss"
      run_ok=0
    fi
  done
  if [[ "$run_ok" -ne 1 ]]; then
    contaminated_runs=$((contaminated_runs + 1))
  fi

  if [[ "$rc" -ne 0 ]] || ! grep -qa "COMBINED_RETIREMENT_SEAL .*result=ok" "$run_log"; then
    note "run ${i}: combined seal did NOT pass (rc=$rc)"
    failed_runs=$((failed_runs + 1))
  else
    note "run ${i}: combined seal result=ok"
  fi
done

if [[ "$failed_runs" -eq 0 && "$contaminated_runs" -eq 0 && "$timeout_runs" -eq 0 ]]; then
  echo "RETIREMENT_SEAL_ISOLATION repeated_runs=${RUNS} contaminated_runs=0 timeout_runs=0 result=ok"
  exit 0
fi
echo "RETIREMENT_SEAL_ISOLATION repeated_runs=${RUNS} contaminated_runs=${contaminated_runs} timeout_runs=${timeout_runs} failed_runs=${failed_runs} result=fail"
exit 1
