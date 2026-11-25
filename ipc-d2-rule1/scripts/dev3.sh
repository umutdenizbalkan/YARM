#!/usr/bin/env bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
cd $S/d2r1-dev-wt
env -u CARGO_TARGET_DIR QEMU_SMOKE_STRICT=1 TIMEOUT_SECS=1500 LOGDIR=$S/d2r1-dev/dev3-x86_64 scripts/qemu-vfs-fd-witness-smoke.sh x86_64 2 wide > $S/d2r1-dev/dev3-x86_64/run.log 2>&1
echo "rc=$?" >> $S/d2r1-dev/dev3-x86_64/run.log
echo DONE > $S/d2r1-dev/dev3.done
