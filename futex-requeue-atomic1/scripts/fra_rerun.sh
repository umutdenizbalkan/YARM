#!/usr/bin/env bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
cd $S
export F=16c31398 OUT=$S/fra-evidence/qual-16c31398
mkdir -p $OUT
ONLY="prov hosted requeue" ./fra_matrix.sh
( cd fra-wt; t0=$(date +%s); echo "=== E_equiv [$(git rev-parse --short=12 HEAD) porcelain=$(git status --porcelain | wc -l)] (start $(date -u +%T))" >> $OUT/summary.txt; $S/fra_equiv.sh > $OUT/E_equiv.log 2>&1; echo "E_equiv rc=$? secs=$(( $(date +%s) - t0 ))" >> $OUT/summary.txt )
echo RERUN_DONE >> $OUT/summary.txt
