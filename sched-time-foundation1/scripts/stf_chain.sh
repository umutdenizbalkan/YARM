#!/usr/bin/env bash
# SCHED-TIME-FOUNDATION1 E chain (schedule: stf_schedule.md): NC -> live -> matrix. Resumable.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=$(cat $S/stf_F)
nice -n 5 $S/stf_nc_run.sh $F $S/stf-evidence/qual-nc ncA1 ncA2 ncA3 ncA4 ncA5 ncA6 ncA7 ncA8 ncA9 ncA10 ncB1 ncB2 ncB3 ncB4 ncB5 ncB6 ncB7 ncC1 ncC2 ncC3 ncC4 ncC5 ncC6 ncC7 ncC8 ncC9 ncC10
F=$F $S/stf_live.sh
F=$F $S/stf_matrix.sh
echo CHAIN_DONE > $S/stf_chain.done
