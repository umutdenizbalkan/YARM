#!/bin/bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
cd $S/fst1-wt
for a in riscv64:riscv64-dev2 aarch64:aarch64-dev1 x86_64:x86_64-dev1; do
  arch=${a%%:*}; name=${a##*:}
  env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$S/fst1-dev/W/$name scripts/qemu-futex-sync-witness-smoke.sh $arch 2 > $S/fst1-dev/W/$name.out 2>&1
  echo "$name rc=$?" >> $S/fst1-dev/W/summary.txt
done
echo ALLDONE >> $S/fst1-dev/W/summary.txt
