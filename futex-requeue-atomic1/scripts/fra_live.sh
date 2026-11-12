#!/usr/bin/env bash
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/fra-evidence/live; mkdir -p $OUT; : > $OUT/summary.txt
WT=$S/fra-wt
echo "candidate=$(git -C $WT rev-parse --short=8 HEAD) porcelain=$(git -C $WT status --porcelain | wc -l)" >> $OUT/summary.txt
for a in x86_64 aarch64 riscv64; do
  (cd $WT && RACE=1 REQUIRE_CROSS_CPU=1 LOGDIR=$OUT/race-$a timeout 2400 scripts/qemu-futex-requeue-witness-smoke.sh $a 2) > $OUT/race-$a.txt 2>&1
  echo "race-$a rc=$? $(grep -a 'FUTEX_REQUEUE_WITNESS_SEAL' $OUT/race-$a.txt | tail -1)" >> $OUT/summary.txt
done
# Live negative control: NC1 (the transfer-then-unprotected-wake gap) built into the race witness.
LWT=$S/fra-livenc-wt
[ -d $LWT ] || git -C /home/user/YARM worktree add -q --detach $LWT ${F:?}
git -C $LWT checkout -q -f ${F:?} && git -C $LWT clean -qfd
python3 -I $S/fra_nc.py $LWT nc1 > $OUT/livenc1.patch.txt 2>&1; git -C $LWT diff >> $OUT/livenc1.patch.txt
git -C $LWT add -A && git -C $LWT -c user.name=nc -c user.email=nc@localhost commit -q -m "NC1 (live control, never pushed)"
(cd $LWT && RACE=1 REQUIRE_CROSS_CPU=1 LOGDIR=$OUT/livenc1-x86_64 timeout 2400 scripts/qemu-futex-requeue-witness-smoke.sh x86_64 2) > $OUT/livenc1-x86_64.txt 2>&1
echo "livenc1-x86_64 rc=$? $(grep -a 'FUTEX_REQUEUE_WITNESS_SEAL' $OUT/livenc1-x86_64.txt | tail -1)" >> $OUT/summary.txt
grep -a 'race:' $OUT/livenc1-x86_64.txt | head -5 >> $OUT/summary.txt
echo LIVE_DONE >> $OUT/summary.txt
