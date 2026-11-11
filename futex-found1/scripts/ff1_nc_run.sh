#!/usr/bin/env bash
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/ff1-evidence/nc; F=${F:?}
mkdir -p $OUT; : > $OUT/summary.txt
WT=$S/ff1nc-wt
[ -d $WT ] || git -C $S/ff1-wt worktree add -q --detach $WT $F
run() { (cd $WT && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/ff1nc-target cargo test --lib -- futex robust --test-threads=1); }
summ() {
  local tag=$1 rc=$2
  res=$(grep -aE '^test result' $OUT/$tag.log | tail -1)
  failed=$(awk '/^failures:$/{f++} f==2 && /^    /{print $1}' $OUT/$tag.log | sed 's/.*tests:://' | sort -u | paste -sd',' -)
  echo "$tag rc=$rc $res FAILED=[$failed]" >> $OUT/summary.txt
}
for nc in nc1 nc2 nc3 nc4 nc5 nc6 nc7 nc8 nc9 nc10; do
  git -C $WT checkout -q -f $F && git -C $WT clean -qfd
  python3 -I $S/ff1_nc.py $WT $nc > $OUT/$nc.patch.txt 2>&1
  git -C $WT diff >> $OUT/$nc.patch.txt
  run > $OUT/$nc.log 2>&1; summ $nc $?
done
git -C $WT checkout -q -f $F && git -C $WT clean -qfd
run > $OUT/F_control.log 2>&1; summ F_control $?
echo NC_DONE >> $OUT/summary.txt
