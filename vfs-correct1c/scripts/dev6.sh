#!/usr/bin/env bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
cd $S/vc1c-wt
for a in x86_64 riscv64 aarch64; do
  mkdir -p $S/vc1c-dev/dev6-$a
  env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$S/vc1c-dev/dev6-$a scripts/qemu-vfs-fd-witness-smoke.sh $a 2 wide > $S/vc1c-dev/dev6-$a/run.log 2>&1
  echo "rc=$?" >> $S/vc1c-dev/dev6-$a/run.log
done
echo DEV6_DONE > $S/vc1c-dev/dev6.done
