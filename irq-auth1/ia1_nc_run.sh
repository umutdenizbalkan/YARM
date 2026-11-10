#!/usr/bin/env bash
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/ia1-evidence/nc; F=${F:-578daf4}; B=41403e1
mkdir -p $OUT; : > $OUT/summary.txt
T="irq_auth1 irq notification stage21 stage22 stage23 notif_lifetime1 notif_waiter1 exit_task route_external"
run() { (cd $S/nc-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/nc-target cargo test --lib -- $T --test-threads=1); }
summ() {
  local tag=$1 rc=$2
  res=$(grep -E '^test result' $OUT/$tag.log | tail -1)
  failed=$(grep -E '^    kernel::' $OUT/$tag.log | sort -u | sed 's/.*tests:://' | paste -sd',' -)
  echo "$tag rc=$rc $res FAILED=[$failed]" >> $OUT/summary.txt
}
git -C $S/nc-wt checkout -q -f $B && git -C $S/nc-wt clean -qfd src
git -C $S/nc-wt rev-parse HEAD > $OUT/baseline.patch.txt
python3 -I $S/ia1_baseline_tree.py $S/nc-wt $S/ia1_baseline_tests.rs >> $OUT/baseline.patch.txt 2>&1
git -C $S/nc-wt diff >> $OUT/baseline.patch.txt
(cd $S/nc-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/nc-target cargo test --lib irq_auth1_baseline -- --test-threads=1) > $OUT/baseline.log 2>&1; summ baseline $?
for nc in nc1 nc2 nc3 nc4 nc5 nc6 nc7 nc8 nc9 nc10 nc11 nc12; do
  git -C $S/nc-wt checkout -q -f $F && git -C $S/nc-wt clean -qfd src
  python3 -I $S/ia1_nc.py $S/nc-wt $nc > $OUT/$nc.patch.txt 2>&1
  git -C $S/nc-wt diff >> $OUT/$nc.patch.txt
  run > $OUT/$nc.log 2>&1; summ $nc $?
done
git -C $S/nc-wt checkout -q -f $F && git -C $S/nc-wt clean -qfd src
run > $OUT/F_control.log 2>&1; summ F_control $?
echo NC_DONE >> $OUT/summary.txt
