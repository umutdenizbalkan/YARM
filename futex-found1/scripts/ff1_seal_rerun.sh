#!/usr/bin/env bash
# FUTEX-FOUND1 appended rerun (R1): the first-cohort seal on CI-exact strict artifacts, F then B.
# FX1/FB1 ran on whatever artifacts earlier steps left; the seal's strict smokes refuse those.
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/ff1-evidence/qual; SUM=$OUT/summary.txt
step() { local id=$1 dir=$2; shift 2; local t0=$(date +%s)
  echo "=== $id [$(git -C "$dir" rev-parse --short=12 HEAD) porcelain=$(git -C "$dir" status --porcelain | wc -l)]: $* (start $(date -u +%T))" >> $SUM
  ( cd "$dir" && "$@" ) > "$OUT/$id.log" 2>&1; echo "$id rc=$? secs=$(( $(date +%s) - t0 ))" >> $SUM; }
echo "rerun R1 start $(date -u +%FT%TZ) reason=seal_artifacts_not_ci_exact" >> $SUM
for side in F B; do
  WT=$S/ff1-wt; [ $side = B ] && WT=$S/er1-wt
  step R1_${side}_build_x86_64_strict "$WT" env ARTIFACTS_STRICT=1 BOOTSTRAP_FEATURE_ARGS="--no-default-features --features timer-contract-witness" scripts/build-qemu-x86_64-artifacts.sh
  step R1_${side}_build_aarch64_strict "$WT" env ARTIFACTS_STRICT=1 scripts/build-qemu-aarch64-artifacts.sh
  step R1_${side}_build_riscv64_strict "$WT" env ARTIFACTS_STRICT=1 scripts/build-qemu-riscv64-artifacts.sh
  step R1_${side}_first_cohort_seal "$WT" env LOGDIR="$OUT/R1_${side}" scripts/qemu-first-cohort-retirement-seal.sh
done
echo RERUN_R1_DONE >> $SUM
