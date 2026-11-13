#!/usr/bin/env bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
until grep -q MATRIX_DONE $S/ft1-evidence/qual/summary.txt; do sleep 60; done
echo "liveD launch $(date -u +%FT%TZ)" >> $S/ft1_chainD.log
$S/ft1_liveD.sh >> $S/ft1_chainD.log 2>&1; echo "liveD rc=$? $(date -u +%FT%TZ)" >> $S/ft1_chainD.log
