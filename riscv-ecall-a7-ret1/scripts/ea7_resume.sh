#!/usr/bin/env bash
# Addendum B: resume the matrix after the container restart, then Addendum A.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
export F=3091f143 B=3ac4f578 BW=1a570728
echo "resume start $(date -u +%FT%TZ) schedule_sha=$(sha256sum $S/ea7_schedule.md | cut -c1-16)" >> $S/ea7_chain.log
ONLY="ci xsect" $S/ea7_matrix.sh >> $S/ea7_chain.log 2>&1; echo "matrix (resumed) done $(date -u +%T)" >> $S/ea7_chain.log
touch $S/ea7_chain.done
$S/ea7_addA.sh > $S/ea7_addA.out 2>&1
