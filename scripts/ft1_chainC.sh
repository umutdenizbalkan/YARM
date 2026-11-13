#!/usr/bin/env bash
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
FPP=$(cat $S/ft1_fprime)
echo "chainC start $(date -u +%FT%TZ) F''=$FPP" >> $S/ft1_chainC.log
$S/ft1_liveC.sh >> $S/ft1_chainC.log 2>&1; echo "liveC rc=$? $(date -u +%FT%TZ)" >> $S/ft1_chainC.log
OUT=$S/ft1-evidence/nc-Fpp; mkdir -p $OUT; touch $OUT/summary.txt
if ! grep -q NC_DONE $OUT/summary.txt; then
  todo=""; for i in $(seq 1 29); do grep -q "^nc$i rc=" $OUT/summary.txt || todo="$todo $i"; done
  for i in $todo; do [ -e $OUT/nc$i.log ] && mv $OUT/nc$i.log $OUT/nc$i.log.interrupted.$(date +%s); done
  F=$FPP NCS="$todo" OUT=$OUT $S/ft1_nc_run.sh >> $S/ft1_chainC.log 2>&1
fi
echo "nc rc=$? $(date -u +%FT%TZ)" >> $S/ft1_chainC.log
touch $S/ft1_matrix.go; echo "gate released $(date -u +%FT%TZ)" >> $S/ft1_chainC.log
