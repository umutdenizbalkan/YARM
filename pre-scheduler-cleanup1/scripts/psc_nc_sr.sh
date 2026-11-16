#!/usr/bin/env bash
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=$1; OUT=$2; shift 2; mkdir -p $OUT; touch $OUT/summary.txt
W=$S/psc-nc-wt; [ -d $W ] || git -C $S/psc-wt worktree add -q --detach $W $F
for c in "$@" control; do
  grep -q "^$c " $OUT/summary.txt && continue
  git -C $W checkout -q -f $F && git -C $W clean -qfd
  [ $c != control ] && python3 -I $S/psc_nc_sr.py $W $c > $OUT/$c.patch.txt 2>&1 && git -C $W diff >> $OUT/$c.patch.txt
  (cd $W && CARGO_TARGET_DIR=$S/psc-nc-target timeout 1500 cargo test --lib -- --test-threads=1 sr_place1 stage199d_shared_region_enqueue_rejection stage198e3b2b_drain_switch) > $OUT/$c.log 2>&1
  rc=$?
  echo "$c rc=$rc $(grep -a 'test result' $OUT/$c.log | tail -1) FAILED=[$(grep -aE '^test .* FAILED$' $OUT/$c.log | sed -E 's/^test kernel::boot::tests:://; s/ \.\.\. FAILED$//' | tr '\n' ',')]" >> $OUT/summary.txt
done
