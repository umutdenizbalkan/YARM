#!/usr/bin/env bash
# The appended run at F2 (declared in fwa_schedule.md): provenance, the hosted group with G1-G4,
# the wait witness x3 and the live NC1 (fwa_live2.sh), and artifact equivalence (fwa_equiv.sh).
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
cd $S
export F=ae65c57e OUT=$S/fwa-evidence/qual-ae65c57e
mkdir -p $OUT
ONLY="prov hosted" ./fwa_matrix.sh
OUT=$S/fwa-evidence/live-ae65c57e ./fwa_live2.sh > $S/fwa_live2.out 2>&1
( cd fwa-wt; t0=$(date +%s); echo "=== E_equiv [$(git rev-parse --short=12 HEAD) porcelain=$(git status --porcelain | wc -l)] (start $(date -u +%T))" >> $OUT/summary.txt; $S/fwa_equiv.sh > $OUT/E_equiv.log 2>&1; echo "E_equiv rc=$? secs=$(( $(date +%s) - t0 ))" >> $OUT/summary.txt )
echo RERUN_DONE >> $OUT/summary.txt
