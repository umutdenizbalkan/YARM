#!/usr/bin/env bash
cd /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/fs1-p7wt
for t in x86_64 riscv64; do env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh > /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/fs1-evidence/p7_build_$t.log 2>&1; echo "$t rc=$?" >> /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/fs1-evidence/p7_build.summary; done
echo P7_DONE >> /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/fs1-evidence/p7_build.summary
