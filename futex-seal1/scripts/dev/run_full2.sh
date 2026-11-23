#!/bin/bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
cd $S/fst1-wt
for a in riscv64 aarch64 x86_64; do
  for p in gp rtb; do
    to=900; [[ $a == riscv64 ]] && to=360
    env -u CARGO_TARGET_DIR TIMEOUT_SECS=$to SEAL_CELLS="os nr17 lh pb rb" SEAL_PROFILE=$p LOGDIR=$S/fs1-dev/full2-$a-$p scripts/qemu-futex-seal-witness-smoke.sh $a 2 > $S/fs1-dev/full2-$a-$p.out 2>&1
    echo "full2-$a-$p rc=$? $(tail -1 $S/fs1-dev/full2-$a-$p.out | cut -c1-300)" >> $S/fs1-dev/full2-summary.txt
  done
done
echo ALLDONE >> $S/fs1-dev/full2-summary.txt
