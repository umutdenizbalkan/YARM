#!/usr/bin/env bash
# RISCV-ECALL-A7-RET1 qualification chain (schedule: ea7_schedule.md). Sequential, no retries.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
export F=3091f143 B=3ac4f578 BW=1a570728
echo "chain start $(date -u +%FT%TZ) F=$F B=$B BW=$BW schedule_sha=$(sha256sum $S/ea7_schedule.md | cut -c1-16)" > $S/ea7_chain.log
$S/ea7_baseline.sh >> $S/ea7_chain.log 2>&1; echo "baseline done $(date -u +%T)" >> $S/ea7_chain.log
$S/ea7_nc_run.sh >> $S/ea7_chain.log 2>&1; echo "nc done $(date -u +%T)" >> $S/ea7_chain.log
$S/ea7_live.sh >> $S/ea7_chain.log 2>&1; echo "live done $(date -u +%T)" >> $S/ea7_chain.log
$S/ea7_matrix.sh >> $S/ea7_chain.log 2>&1; echo "matrix done $(date -u +%T)" >> $S/ea7_chain.log
touch $S/ea7_chain.done
