#!/usr/bin/env bash
cd /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/vc1a-wt
for a in x86_64:900 riscv64:900; do t=${a%:*}; to=${a#*:}
  env -u CARGO_TARGET_DIR TIMEOUT_SECS=$to LOGDIR=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/vc1a-dev/w1-$t scripts/qemu-vfs-correct-witness-smoke.sh $t 2 > /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/vc1a-dev/w1-$t.out 2>&1; echo "$t rc=$?" >> /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/vc1a-dev/dev2.summary
done
python3 -I /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/vc1a-nc/nc.py ncA_count_without_copy >> /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/vc1a-dev/dev2.summary 2>&1
env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/vc1a-dev/ncA-aarch64 scripts/qemu-vfs-correct-witness-smoke.sh aarch64 2 > /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/vc1a-dev/ncA-aarch64.out 2>&1; echo "ncA aarch64 rc=$?" >> /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/vc1a-dev/dev2.summary
git checkout -- crates/yarm-user-rt/src/vfs_client.rs; echo "restored porcelain=$(git status --porcelain crates/yarm-user-rt/src/vfs_client.rs | wc -l)" >> /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/vc1a-dev/dev2.summary
echo DEV2_DONE >> /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/vc1a-dev/dev2.summary
