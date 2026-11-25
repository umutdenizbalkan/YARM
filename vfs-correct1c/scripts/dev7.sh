#!/usr/bin/env bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
cd $S/vc1c-wt
mkdir -p $S/vc1c-dev/dev7-x86_64
env -u CARGO_TARGET_DIR TIMEOUT_SECS=1500 LOGDIR=$S/vc1c-dev/dev7-x86_64 scripts/qemu-vfs-fd-witness-smoke.sh x86_64 2 wide > $S/vc1c-dev/dev7-x86_64/run.log 2>&1
echo "rc=$?" >> $S/vc1c-dev/dev7-x86_64/run.log
echo DONE > $S/vc1c-dev/dev7.done
