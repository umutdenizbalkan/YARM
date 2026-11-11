#!/usr/bin/env bash
cd /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/frq-wt
for a in aarch64 riscv64 x86_64; do
  LOGDIR=../frq-wit3/$a-smp2 timeout 3000 scripts/qemu-futex-requeue-witness-smoke.sh $a 2 > ../frq-wit3/$a.txt 2>&1
  echo "$a rc=$? $(tail -1 ../frq-wit3/$a.txt)" >> ../frq-wit3/summary.txt
done
echo WIT_DONE >> ../frq-wit3/summary.txt
