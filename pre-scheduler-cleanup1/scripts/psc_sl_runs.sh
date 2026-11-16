#!/usr/bin/env bash
# SCHED-LOCK1-ORDER1 live evidence: the SCHED-LOCK1 witness on RISC-V SMP 2 with the scratch delay v3,
# baseline (2866be83) vs fix, interleaved x3. Resumable.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/psc-evidence/sl/delay3; mkdir -p $OUT; touch $OUT/summary.txt
for i in 1 2 3 4 5 6; do for k in base fix; do
  L=SLD-$k-$i; grep -q "^$L " $OUT/summary.txt && continue
  W=$S/psc-sld-$k
  (cd $W && env -u CARGO_TARGET_DIR ARCH=riscv64 SMP=2 LOGDIR=$OUT/$L timeout 2400 scripts/qemu-sched-lock1-witness-smoke.sh) > $OUT/$L.txt 2>&1
  rc=$?
  echo "$L [$(git -C $W rev-parse --short=8 HEAD)+$(git -C $W status --porcelain | wc -l)dirty] rc=$rc $(grep -a "SLD_SCRATCH" $OUT/$L/*.log 2>/dev/null | tail -1 | sed "s/.*SLD_SCRATCH/SLD_SCRATCH/") $(grep -a SL1W_GRADE $OUT/$L.txt | tail -1 | cut -c1-260)" >> $OUT/summary.txt
done; done
echo DONE6 >> $OUT/summary.txt
