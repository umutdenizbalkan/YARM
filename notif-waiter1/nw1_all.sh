#!/usr/bin/env bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
$S/nw1_nc_run.sh
rm -rf $S/nc-target
$S/nw1_matrix.sh > $S/nw1_matrix.out 2>&1
echo ALL_DONE > $S/nw1_all.done
