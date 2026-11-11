#!/usr/bin/env bash
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/er1-evidence/nc; F=${F:?}
mkdir -p $OUT; : > $OUT/summary.txt
T="endpoint_rollback1 u9spawnic1_endpoint_cap_txn u9spawn1_sp3_spawn_ledger u9spawntxn3_provisional_rollback irq_auth1"
run() { (cd $S/er1nc-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/er1nc-target cargo test --lib -- $T --test-threads=1); }
summ() {
  local tag=$1 rc=$2
  res=$(grep -E '^test result' $OUT/$tag.log | tail -1)
  failed=$(grep -E '^    kernel::' $OUT/$tag.log | sort -u | sed 's/.*tests:://' | paste -sd',' -)
  echo "$tag rc=$rc $res FAILED=[$failed]" >> $OUT/summary.txt
}
for nc in nc1 nc2 nc3 nc4 nc5 nc6; do
  git -C $S/er1nc-wt checkout -q -f $F && git -C $S/er1nc-wt clean -qfd
  python3 -I $S/er1_nc.py $S/er1nc-wt $nc > $OUT/$nc.patch.txt 2>&1
  git -C $S/er1nc-wt diff >> $OUT/$nc.patch.txt
  run > $OUT/$nc.log 2>&1; summ $nc $?
done
git -C $S/er1nc-wt checkout -q -f $F && git -C $S/er1nc-wt clean -qfd
run > $OUT/F_control.log 2>&1; summ F_control $?
echo NC_DONE >> $OUT/summary.txt
