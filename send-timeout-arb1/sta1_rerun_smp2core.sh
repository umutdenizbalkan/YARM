#!/usr/bin/env bash
# Appended rerun of C2's smp2-core profile on F (the C2 run failed on a serial-interleaved marker).
# The C2 log is kept; nothing is substituted.
Q=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/sta1-evidence/qual; cd /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/sta1-wt
{ echo "=== R_rebuild_default_x86_64_F [$(git rev-parse --short=12 HEAD) porcelain=$(git status --porcelain | wc -l)] (start $(date -u +%H:%M:%S))"; } >> $Q/summary.txt
scripts/build-qemu-x86_64-artifacts.sh > $Q/R_rebuild_default_x86_64_F.log 2>&1; echo "R_rebuild_default_x86_64_F rc=$? kernel_sha256=$(sha256sum build-x86_64/kernel_boot.elf | cut -c1-64)" >> $Q/summary.txt
for i in 1 2 3; do
  echo "=== R${i}_smp2_core_F [$(git rev-parse --short=12 HEAD) porcelain=$(git status --porcelain | wc -l)] (start $(date -u +%H:%M:%S))" >> $Q/summary.txt
  t0=$(date +%s); scripts/run-ci-profiles.sh smp2-core --keep-going --logs-dir $Q/R${i}_ci-profiles > $Q/R${i}_smp2_core_F.log 2>&1; rc=$?
  echo "R${i}_smp2_core_F rc=$rc secs=$(($(date +%s)-t0))" >> $Q/summary.txt
done
echo RERUN_DONE >> $Q/summary.txt
