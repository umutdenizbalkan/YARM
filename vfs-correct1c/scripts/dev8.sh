#!/usr/bin/env bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
cd $S/vc1c-wt
for t in aarch64 x86_64 riscv64; do
  mkdir -p $S/vc1c-dev/dev8-$t
  env -u CARGO_TARGET_DIR TIMEOUT_SECS=1500 LOGDIR=$S/vc1c-dev/dev8-$t scripts/qemu-vfs-fd-witness-smoke.sh $t 1 wide > $S/vc1c-dev/dev8-$t/run.log 2>&1
  echo "rc=$?" >> $S/vc1c-dev/dev8-$t/run.log
  echo "$t $(tail -1 $S/vc1c-dev/dev8-$t/run.log) $(tail -2 $S/vc1c-dev/dev8-$t/run.log | head -1)" >> $S/vc1c-dev/dev8.done
done
echo DONE >> $S/vc1c-dev/dev8.done
