#!/usr/bin/env bash
# FUTEX-TIME1 qualification chain (schedule: ft1_schedule.md). Sequential, no retries.
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
export F=04a09b2b B=6ba03af9
unset CARGO_TARGET_DIR
echo "chain start $(date -u +%FT%TZ) F=$F B=$B schedule_sha=$(sha256sum $S/ft1_schedule.md | cut -c1-16) nc_sha=$(sha256sum $S/ft1_nc.py | cut -c1-16)" > $S/ft1_chain.log
$S/ft1_nc_run.sh >> $S/ft1_chain.log 2>&1; echo "nc done $(date -u +%T)" >> $S/ft1_chain.log
$S/ft1_live.sh >> $S/ft1_chain.log 2>&1; echo "live done $(date -u +%T)" >> $S/ft1_chain.log
$S/ft1_matrix.sh >> $S/ft1_chain.log 2>&1; echo "matrix done $(date -u +%T)" >> $S/ft1_chain.log
touch $S/ft1_chain.done
