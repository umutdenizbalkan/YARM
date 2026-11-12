#!/usr/bin/env bash
# Live runs at F: the wait witness on three ports (SMP 2), and the live negative control NC1 (the
# separated compare and registration, window B in the gap) on x86_64.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=${F:?}; OUT=$S/fwa-evidence/live; mkdir -p $OUT; : > $OUT/summary.txt
WT=$S/fwa-wt
echo "candidate=$(git -C $WT rev-parse --short=8 HEAD) porcelain=$(git -C $WT status --porcelain | wc -l)" >> $OUT/summary.txt
for a in x86_64 aarch64 riscv64; do
  (cd $WT && LOGDIR=$OUT/wait-$a timeout 2400 scripts/qemu-futex-wait-witness-smoke.sh $a 2) > $OUT/wait-$a.txt 2>&1
  echo "wait-$a rc=$? $(grep -a 'FUTEX_WAIT_WITNESS_SEAL' $OUT/wait-$a.txt | tail -1)" >> $OUT/summary.txt
done
LWT=$S/fwalive-wt
[ -d $LWT ] || git -C $WT worktree add -q --detach $LWT $F
git -C $LWT checkout -q -f $F && git -C $LWT clean -qfd
python3 -I $S/fwa_nc.py $LWT nc1 > $OUT/livenc1.patch.txt 2>&1; git -C $LWT diff >> $OUT/livenc1.patch.txt
git -C $LWT add -A && git -C $LWT -c user.name=nc -c user.email=nc@localhost commit -q -m "NC1 (live control, never pushed)"
(cd $LWT && LOGDIR=$OUT/livenc1-x86_64 timeout 2400 scripts/qemu-futex-wait-witness-smoke.sh x86_64 2) > $OUT/livenc1-x86_64.txt 2>&1
echo "livenc1-x86_64 rc=$? $(grep -a 'FUTEX_WAIT_WITNESS_SEAL' $OUT/livenc1-x86_64.txt | tail -1)" >> $OUT/summary.txt
grep -a '\[futex-wait-grade\]\[fail\]' $OUT/livenc1-x86_64.txt | head -5 >> $OUT/summary.txt
git -C $LWT checkout -q -f $F; rm -rf $LWT/build-*
echo LIVE_DONE >> $OUT/summary.txt
