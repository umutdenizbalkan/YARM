#!/usr/bin/env bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
until grep -q NC_DONE $S/fra-evidence/nc/summary.txt && grep -q LIVE_DONE $S/fra-evidence/live/summary.txt; do sleep 20; done
rm -rf $S/fra-nc-target
cd $S && F=5939c705 ./fra_matrix.sh > $S/fra_matrix2.out 2>&1
