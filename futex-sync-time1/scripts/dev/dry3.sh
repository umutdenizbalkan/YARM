#!/bin/bash
# FUTEX-SYNC-TIME1 NC dry run 3 (NC3 sharpened, busy-poll guard) (dev, not qualification): fresh source copy, own target dir.
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
D=$S/fst1-ncdry3; T=$S/fst1-ncdry2-target; O=$S/fst1-nc/dry3
rm -rf $D; mkdir -p $D $O
(cd $S/fst1-wt && tar --exclude=./target --exclude=./build-* -cf - .) | (cd $D && tar xf -)
rm -f $D/.git
cd $D; cp crates/yarm-user-rt/src/lib.rs $O/lib.rs.orig
export CARGO_TARGET_DIR=$T CARGO_BUILD_JOBS=2
for nc in nc1_reset_deadline_on_retry nc2_timeout_without_reacquire nc3_drop_contended nc4_no_predicate_recheck; do
  cp $O/lib.rs.orig crates/yarm-user-rt/src/lib.rs
  python3 $S/fst1-nc/nc.py $nc > $O/${nc}_apply.log 2>&1 || { echo "$nc APPLY_FAIL" >> $O/summary.txt; continue; }
  timeout 900 cargo test -p yarm-user-rt --lib futex_sync_tests -- --test-threads=1 > $O/${nc}_rt.log 2>&1; rrt=$?
  timeout 900 cargo test --lib sync_time1 -- --test-threads=1 > $O/${nc}_k.log 2>&1; rk=$?
  frt=$(grep -c '\.\.\. FAILED' $O/${nc}_rt.log); fk=$(grep -c '\.\.\. FAILED' $O/${nc}_k.log)
  echo "$nc rt_rc=$rrt rt_failed=$frt k_rc=$rk k_failed=$fk" >> $O/summary.txt
done
cp $O/lib.rs.orig crates/yarm-user-rt/src/lib.rs
cmp $O/lib.rs.orig $S/fst1-wt/crates/yarm-user-rt/src/lib.rs && echo RESTORED_IDENTICAL >> $O/summary.txt
echo ALLDONE >> $O/summary.txt
