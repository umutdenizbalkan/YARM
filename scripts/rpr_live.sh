#!/usr/bin/env bash
# Live, SMP 2: the revised wait witness x3 per ISA at F, then the live controls (NC2 on RISC-V
# must fail on the parked return; the same patch on x86_64 must pass).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=${F:?}; OUT=${OUT:-$S/rpr-evidence/live}; mkdir -p $OUT; : > $OUT/summary.txt
WT=$S/rpr-wt
echo "candidate=$(git -C $WT rev-parse --short=8 HEAD) porcelain=$(git -C $WT status --porcelain | wc -l)" >> $OUT/summary.txt
for a in riscv64 aarch64 x86_64; do
  for n in 1 2 3; do
    (cd $WT && LOGDIR=$OUT/wait-$a-$n timeout 2400 scripts/qemu-futex-wait-witness-smoke.sh $a 2) > $OUT/wait-$a-$n.txt 2>&1
    echo "wait-$a-$n rc=$? $(grep -a 'FUTEX_WAIT_WITNESS_SEAL' $OUT/wait-$a-$n.txt | tail -1)" >> $OUT/summary.txt
    grep -a '\[futex-wait-grade\]\[fail\]' $OUT/wait-$a-$n.txt | head -5 | sed 's/^/    /' >> $OUT/summary.txt
  done
done
LWT=$S/rprlive-wt
[ -d $LWT ] || git -C $WT worktree add -q --detach $LWT $F
git -C $LWT checkout -q -f $F && git -C $LWT clean -qfd
python3 -I $S/rpr_nc.py $LWT nc2 > $OUT/livenc2.patch.txt 2>&1; git -C $LWT diff >> $OUT/livenc2.patch.txt
git -C $LWT -c user.name=nc -c user.email=nc@localhost commit -qam "NC2 (live control, never pushed)"
for a in riscv64 x86_64; do
  (cd $LWT && LOGDIR=$OUT/livenc2-$a timeout 2400 scripts/qemu-futex-wait-witness-smoke.sh $a 2) > $OUT/livenc2-$a.txt 2>&1
  echo "livenc2-$a rc=$? $(grep -a 'FUTEX_WAIT_WITNESS_SEAL' $OUT/livenc2-$a.txt | tail -1)" >> $OUT/summary.txt
  grep -a '\[futex-wait-grade\]\[fail\]' $OUT/livenc2-$a.txt | head -4 | sed 's/^/    /' >> $OUT/summary.txt
done
git -C $LWT checkout -q -f $F; rm -rf $LWT/build-*
echo LIVE_DONE >> $OUT/summary.txt
