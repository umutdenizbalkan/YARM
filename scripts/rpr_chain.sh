#!/usr/bin/env bash
# The predeclared order (rpr_schedule.md): controls, baseline reproduction, live, matrix.
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
cd $S; export F=194842bc
rm -f $S/rpr_chain.done
./rpr_nc_run.sh > $S/rpr_nc_run.out 2>&1
./rpr_baseline.sh > $S/rpr_baseline.out 2>&1
./rpr_live.sh > $S/rpr_live.out 2>&1
./rpr_matrix.sh > $S/rpr_matrix.out 2>&1
touch $S/rpr_chain.done
