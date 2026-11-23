#!/usr/bin/env bash
cd /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/fst1-wt
for a in riscv64:1800 aarch64:900 x86_64:900; do
  t=${a%:*}; to=${a#*:}
  env -u CARGO_TARGET_DIR TIMEOUT_SECS=$to LOGDIR=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/fs1-dev3/time-fix3-$t scripts/qemu-futex-time-witness-smoke.sh $t 2 > /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/fs1-dev3/time-fix3-$t.out 2>&1
  echo "$t rc=$?" >> /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/fs1-dev3/fix3.summary
done
echo FIX3_DONE >> /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/fs1-dev3/fix3.summary
