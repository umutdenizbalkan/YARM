#!/usr/bin/env bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
cd $S
F=9ce9845f ./fwa_nc_run.sh > $S/fwa_nc.out 2>&1
rm -rf $S/fwa-nc-target
F=9ce9845f ./fwa_baseline.sh > $S/fwa_baseline.out 2>&1
F=9ce9845f ./fwa_live.sh > $S/fwa_live.out 2>&1
F=9ce9845f ./fwa_matrix.sh > $S/fwa_matrix.out 2>&1
echo CHAIN_DONE > $S/fwa_chain.done
