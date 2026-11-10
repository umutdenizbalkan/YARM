#!/usr/bin/env bash
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/nw1-evidence/nc; F=8c31d9e; B=dc5c8fd
mkdir -p $OUT; : > $OUT/summary.txt
T="notif_waiter1 exit_task_ notification stage22 stage23 u9f_split notif_lifetime1 irq_generation1 stage199d_wa2b_wake_owner_census"
run() { (cd $S/nc-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/nc-target cargo test --lib -- $T --test-threads=1); }
summ() {
  local tag=$1 rc=$2
  res=$(grep -E '^test result' $OUT/$tag.log | tail -1)
  failed=$(grep -E '^    kernel::' $OUT/$tag.log | sort -u | sed 's/.*tests:://' | paste -sd',' -)
  echo "$tag rc=$rc $res FAILED=[$failed]" >> $OUT/summary.txt
}
# baseline reproducers: B + hook + unused owner module + F's tests
git -C $S/nc-wt checkout -q -f $B && git -C $S/nc-wt clean -qfd src
git -C $S/nc-wt show-ref -q; git -C $S/nc-wt rev-parse HEAD > $OUT/baseline.patch.txt
python3 -I $S/nw1_baseline_tree.py $S/nc-wt $S/nw1-wt >> $OUT/baseline.patch.txt 2>&1
git -C $S/nc-wt diff >> $OUT/baseline.patch.txt; git -C $S/nc-wt status --porcelain >> $OUT/baseline.patch.txt
run > $OUT/baseline.log 2>&1; summ baseline $?
git -C $S/nc-wt checkout -q -f $B && git -C $S/nc-wt clean -qfd src
for nc in nc1 nc2 nc3 nc4 nc5 nc6 nc7 nc8 nc9 nc10 nc11; do
  git -C $S/nc-wt checkout -q -f $F && git -C $S/nc-wt clean -qfd src
  python3 -I $S/nw1_nc.py $S/nc-wt $nc > $OUT/$nc.patch.txt 2>&1
  git -C $S/nc-wt diff >> $OUT/$nc.patch.txt
  run > $OUT/$nc.log 2>&1; summ $nc $?
done
git -C $S/nc-wt checkout -q -f $F && git -C $S/nc-wt clean -qfd src
run > $OUT/F_control.log 2>&1; summ F_control $?
echo NC_DONE >> $OUT/summary.txt
