#!/usr/bin/env bash
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/sta1-evidence/nc; F=${F:?}; B=414afb86b03a20d7b8e8567e667b9e9b27a62b31
mkdir -p $OUT; : > $OUT/summary.txt
T="send_timeout_arb1 ipc_sender_id1 u6_offlock_blocking_send_commit u7_production_timeout_promotion u3_ordinary_cap_sender_wake stage169"
run() { (cd $S/sta1nc-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/sta1nc-target cargo test --lib -- $T --test-threads=1); }
summ() {
  local tag=$1 rc=$2
  res=$(grep -E '^test result' $OUT/$tag.log | tail -1)
  failed=$(grep -E '^    kernel::' $OUT/$tag.log | sort -u | sed 's/.*tests:://' | paste -sd',' -)
  echo "$tag rc=$rc $res FAILED=[$failed]" >> $OUT/summary.txt
}
# Baseline reproducers: 414afb86 + only the test-only phase hooks + the new tests (no fix).
git -C $S/sta1nc-wt checkout -q -f $B && git -C $S/sta1nc-wt clean -qfd
python3 -I $S/sta1_hooks_baseline.py $S/sta1nc-wt $S > /dev/null
python3 -I $S/sta1_add_tests.py $S/sta1nc-wt $S > /dev/null
(git -C $S/sta1nc-wt rev-parse HEAD; git -C $S/sta1nc-wt diff --stat) > $OUT/baseline.patch.txt
(cd $S/sta1nc-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/sta1nc-target cargo test --lib -- send_timeout_arb1 --test-threads=1) > $OUT/baseline.log 2>&1; summ baseline $?
for nc in nc1 nc2 nc3 nc4 nc5 nc6 nc7 nc8 nc9 nc10 nc11 nc12; do
  git -C $S/sta1nc-wt checkout -q -f $F && git -C $S/sta1nc-wt clean -qfd
  python3 -I $S/sta1_nc.py $S/sta1nc-wt $nc > $OUT/$nc.patch.txt 2>&1
  git -C $S/sta1nc-wt diff >> $OUT/$nc.patch.txt
  run > $OUT/$nc.log 2>&1; summ $nc $?
done
git -C $S/sta1nc-wt checkout -q -f $F && git -C $S/sta1nc-wt clean -qfd
run > $OUT/F_control.log 2>&1; summ F_control $?
echo NC_DONE >> $OUT/summary.txt
