#!/usr/bin/env bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
until grep -q LIVED_DONE $S/ft1-evidence/live/summary.txt; do sleep 60; done
echo "addE launch $(date -u +%FT%TZ)" >> $S/ft1_chainE.log
$S/ft1_addE.sh >> $S/ft1_chainE.log 2>&1; echo "addE rc=$? $(date -u +%FT%TZ)" >> $S/ft1_chainE.log
