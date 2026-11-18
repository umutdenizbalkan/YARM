#!/bin/bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
cd $S/fst1-wt
cmp crates/yarm-user-rt/src/lib.rs $S/fst1-backup/lib.rs && cmp crates/yarm-control-plane-servers/src/control_plane/init/service.rs $S/fst1-backup/service.rs || { echo "lib.rs differs from backup; abort" >> $S/fst1-dev/W/summary.txt; exit 1; }
for a in aarch64:aarch64-dev4 x86_64:x86_64-dev2 riscv64:riscv64-dev3; do
  arch=${a%%:*}; name=${a##*:}
  env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$S/fst1-dev/W/$name scripts/qemu-futex-sync-witness-smoke.sh $arch 2 > $S/fst1-dev/W/$name.out 2>&1
  echo "$name rc=$?" >> $S/fst1-dev/W/summary.txt
done
echo ALLDONE >> $S/fst1-dev/W/summary.txt
