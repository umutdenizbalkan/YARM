#!/usr/bin/env bash
# FUTEX-TIME1 Addendum F (ft1_schedule.md).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/ft1-evidence/addF; mkdir -p $OUT; touch $OUT/summary.txt
WT=$S/ft1-wt; BWT=$S/ea7-wt; LWT=$S/ft1live-wt; FPP=$(cat $S/ft1_fprime); B=6ba03af9
[ "$(git -C $WT rev-parse HEAD)" = "$(git -C $WT rev-parse $FPP)" ] || { echo "ft1-wt not at F''"; exit 2; }
[ "$(git -C $BWT rev-parse HEAD)" = "$(git -C $BWT rev-parse $B)" ] || { echo "ea7-wt not at B"; exit 2; }
stamp() { echo "[$(git -C $1 rev-parse --short=8 HEAD) porcelain=$(git -C $1 status --porcelain | wc -l)]"; }
done_l() { grep -q "^$1 \[" $OUT/summary.txt; }
interrupted() { [ -e $OUT/$1 ] && mv $OUT/$1 $OUT/$1.interrupted.$(date +%s); [ -e $OUT/$1.txt ] && mv $OUT/$1.txt $OUT/$1.txt.interrupted.$(date +%s); true; }
fx6() { # fx6 <label> <dir>
  done_l $1 && return 0; interrupted $1
  (cd $2 && env -u CARGO_TARGET_DIR ARCH=riscv64 SMP=2 LOGDIR=$OUT/$1 timeout 2400 scripts/qemu-sched-lock1-witness-smoke.sh) > $OUT/$1.txt 2>&1; local rc=$?
  echo "$1 $(stamp $2) rc=$rc $(grep -a 'SL1W_SEAL' $OUT/$1.txt | tail -1) | $(grep -a 'SL1W_GRADE' $OUT/$1.txt | tail -1 | cut -c1-220)" >> $OUT/summary.txt; }
ls1() { done_l $1 && return 0; interrupted $1
  (cd $2 && env -u CARGO_TARGET_DIR ARCH=riscv64 LOGDIR=$OUT/$1 timeout 2400 scripts/qemu-irq-saturation-witness-smoke.sh) > $OUT/$1.txt 2>&1; local rc=$?
  local split=$(grep -a -B1 '^#HOST_INJECT' $OUT/$1/boot.log 2>/dev/null | grep -a -v '^#HOST_INJECT' | grep -a -v '^--$' | grep -acvE '^$|\r$|[A-Za-z_]+=.*$' )
  echo "$1 $(stamp $2) rc=$rc host_inject_after_partial_line=$split $(grep -aE 'IRQSAT_GRADE|SEAL' $OUT/$1.txt | tail -1 | cut -c1-200)" >> $OUT/summary.txt; }
grep -q '^addF start' $OUT/summary.txt || echo "addF start $(date -u +%FT%TZ)" >> $OUT/summary.txt
for t in B F; do
  [ $t = B ] && R=$B || R=$FPP
  if ! grep -q "^FX6F-${t}d-3 \[" $OUT/summary.txt; then
    git -C $LWT checkout -q -f $R && git -C $LWT clean -qfd
    python3 -I $S/ft1_addF_delay.py $LWT > $OUT/delay-$t.patch.txt 2>&1; git -C $LWT diff >> $OUT/delay-$t.patch.txt
    git -C $LWT -c user.name=nc -c user.email=nc@localhost commit -qam "addF scratch delay on $t (never pushed)"
    for n in 1 2 3; do fx6 FX6F-${t}d-$n $LWT; done
  fi
done
git -C $LWT checkout -q -f $FPP; rm -rf $LWT/build-*
for n in $(seq 1 10); do fx6 FX6F-B-$n $BWT; fx6 FX6F-F-$n $WT; done
for n in 1 2 3; do ls1 LSF-B-$n $BWT; ls1 LSF-F-$n $WT; done
echo "addF end $(date -u +%FT%TZ)" >> $OUT/summary.txt; echo ADDF_DONE >> $OUT/summary.txt
