#!/usr/bin/env bash
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/nl1-evidence/nc; F=6a04065
: > $OUT/summary.txt
for nc in nc1 nc2 nc3 nc4 nc5 nc6 nc7 nc8 nc9 nc10 nc11; do
  git -C $S/nc-wt checkout -q -f $F
  python3 -I $S/nl1_nc.py $S/nc-wt $nc > $OUT/$nc.patch.txt 2>&1
  git -C $S/nc-wt diff >> $OUT/$nc.patch.txt
  (cd $S/nc-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/nc-target cargo test --lib -- irq_generation1 notif_lifetime1 stage22 stage23 u9f_split --test-threads=1) > $OUT/$nc.log 2>&1
  rc=$?
  res=$(grep -E '^test result' $OUT/$nc.log | tail -1)
  failed=$(grep -E '^    kernel::' $OUT/$nc.log | sort -u | sed 's/.*tests:://' | paste -sd',' -)
  echo "$nc rc=$rc $res FAILED=[$failed]" >> $OUT/summary.txt
done
git -C $S/nc-wt checkout -q -f $F
echo NC_DONE >> $OUT/summary.txt
