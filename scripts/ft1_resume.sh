#!/usr/bin/env bash
# FUTEX-TIME1 resumable chain (schedule: ft1_schedule.md, Addendum A).
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
export F=04a09b2b B=6ba03af9
unset CARGO_TARGET_DIR
N=$S/ft1-evidence/nc
echo "resume $(date -u +%FT%TZ) schedule_sha=$(sha256sum $S/ft1_schedule.md | cut -c1-16)" >> $S/ft1_chain.log
if ! grep -q NC_DONE $N/summary.txt 2>/dev/null; then
  first=1; while grep -q "^nc$first rc=" $N/summary.txt 2>/dev/null; do first=$((first+1)); done
  for f in $N/nc$first.log $N/nc$first.patch.txt; do [ -e $f ] && mv $f $f.interrupted.$(date +%s); done
  echo "nc resume from nc$first" >> $S/ft1_chain.log
  if [ $first -le 28 ]; then NCS="$(seq $first 28)" $S/ft1_nc_run.sh >> $S/ft1_chain.log 2>&1
  else NCS="" $S/ft1_nc_run.sh >> $S/ft1_chain.log 2>&1; fi
  echo "nc done $(date -u +%T)" >> $S/ft1_chain.log
fi
grep -q LIVE_DONE $S/ft1-evidence/live/summary.txt 2>/dev/null || { $S/ft1_live.sh >> $S/ft1_chain.log 2>&1; echo "live done $(date -u +%T)" >> $S/ft1_chain.log; }
grep -q MATRIX_DONE $S/ft1-evidence/qual/summary.txt 2>/dev/null || { $S/ft1_matrix.sh >> $S/ft1_chain.log 2>&1; echo "matrix done $(date -u +%T)" >> $S/ft1_chain.log; }
touch $S/ft1_chain.done
