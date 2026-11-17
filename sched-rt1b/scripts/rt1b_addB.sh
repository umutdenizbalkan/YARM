#!/usr/bin/env bash
# RT-SCHED1B addendum B on C2 = 1e6cfd40 (d2027640 + the phase-E grader correction; no kernel
# change). Steps in order, every log kept, nothing rerun in place. The main run's W3 failure and
# its invalid NC2b (SKIP_BASE refused by the artifact binding) stay in qual/ as recorded.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C2=1e6cfd40
Q=$S/rt1b-evidence/qual
O=$S/rt1b-evidence/addB
W=$S/rt1b-wt
SUM=$O/summary.txt
mkdir -p $O
step() {
  local name=$1; shift
  local por; por=$(git -C $W status --porcelain | wc -l)
  echo "=== $name [$(git -C $W rev-parse --short=12 HEAD) porcelain=$por]: $* (start $(date -u +%H:%M:%S))" >> $SUM
  local t0=$SECONDS
  ( cd $W && "$@" ) > $O/$name.log 2>&1
  local rc=$?
  echo "$name rc=$rc secs=$((SECONDS - t0))" >> $SUM
}
echo "rt1b addB start $(date -u +%FT%TZ) C2=$C2 sha=$(sha256sum $0 | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $C2
[[ "$(git -C $W rev-parse --short=8 HEAD)" == "$C2" ]] || { echo "worktree not at $C2" >> $SUM; exit 1; }
step B1_grader_self_test python3 -I scripts/grade-rt-sched-witness.py --self-test
step B2_regrade_W1_x86_64 python3 -I scripts/grade-rt-sched-witness.py --arch x86_64 --smp 2 --budget on $Q/W1/boot.norm.log
step B2_regrade_W2_aarch64 python3 -I scripts/grade-rt-sched-witness.py --arch aarch64 --smp 2 --budget on $Q/W2/boot.norm.log
step B2_regrade_W4_riscv64_budget_off python3 -I scripts/grade-rt-sched-witness.py --arch riscv64 --smp 2 --budget off $Q/W4/boot.norm.log
step B2_regrade_W3_failed_riscv64 python3 -I scripts/grade-rt-sched-witness.py --arch riscv64 --smp 2 --budget on $Q/W3/boot.norm.log
step B3_W3_riscv64_smp2 env -u CARGO_TARGET_DIR SCHED_BUDGET=on LOGDIR=$O/W3 scripts/qemu-rt-sched-witness-smoke.sh riscv64 2
RUNS="python3 -I $S/rt1b-nc/runs.py"
step B4_NC2b_exhausted_runs_live_riscv64 bash -c "$RUNS && git diff --stat && env -u CARGO_TARGET_DIR SCHED_BUDGET=on LOGDIR=$O/NC2b scripts/qemu-rt-sched-witness-smoke.sh riscv64 2; rc=\$?; git checkout -- src/kernel/scheduler.rs; exit \$rc"
echo "post-NC porcelain=$(git -C $W status --porcelain | wc -l)" >> $SUM
echo "rt1b addB end $(date -u +%FT%TZ)" >> $SUM
echo RT1B_ADDB_DONE >> $SUM
