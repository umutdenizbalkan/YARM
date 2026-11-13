#!/usr/bin/env bash
# FUTEX-TIME1 Addendum E (ft1_schedule.md): FX6 SCHED-LOCK1 riscv64 x5 at B and x5 at F'', interleaved.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/ft1-evidence/addE; mkdir -p $OUT; touch $OUT/summary.txt
WT=$S/ft1-wt; BWT=$S/ea7-wt
[ "$(git -C $WT rev-parse HEAD)" = "$(git -C $WT rev-parse $(cat $S/ft1_fprime))" ] || { echo "ft1-wt not at F''"; exit 2; }
[ "$(git -C $BWT rev-parse HEAD)" = "$(git -C $BWT rev-parse 6ba03af9)" ] || { echo "ea7-wt not at B"; exit 2; }
stamp() { echo "[$(git -C $1 rev-parse --short=8 HEAD) porcelain=$(git -C $1 status --porcelain | wc -l)]"; }
done_l() { grep -q "^$1 \[" $OUT/summary.txt; }
interrupted() { [ -e $OUT/$1 ] && mv $OUT/$1 $OUT/$1.interrupted.$(date +%s); [ -e $OUT/$1.txt ] && mv $OUT/$1.txt $OUT/$1.txt.interrupted.$(date +%s); true; }
grep -q '^addE start' $OUT/summary.txt || echo "addE start $(date -u +%FT%TZ)" >> $OUT/summary.txt
for n in 1 2 3 4 5; do
  for t in B F; do
    L=FX6E-$t-$n; done_l $L && continue; interrupted $L
    [ $t = B ] && d=$BWT || d=$WT
    (cd $d && env -u CARGO_TARGET_DIR ARCH=riscv64 SMP=2 LOGDIR=$OUT/$L timeout 2400 scripts/qemu-sched-lock1-witness-smoke.sh) > $OUT/$L.txt 2>&1
    rc=$?
    echo "$L $(stamp $d) rc=$rc $(grep -a 'SL1W_SEAL' $OUT/$L.txt | tail -1) | $(grep -a 'SL1W_GRADE' $OUT/$L.txt | tail -1 | cut -c1-220)" >> $OUT/summary.txt
  done
done
echo "addE end $(date -u +%FT%TZ)" >> $OUT/summary.txt; echo ADDE_DONE >> $OUT/summary.txt
