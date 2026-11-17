#!/usr/bin/env bash
# RT-SCHED1A addendum B: the documentation commit 5360efdd under the doc-reading checks.
set -u
O=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/rt1a-evidence/qual; W=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/rt1a-wt; SUM=$O/summary.txt
cd $W
[[ "$(git rev-parse --short=8 HEAD)" == "5360efdd" ]] || { echo "addB: HEAD not 5360efdd" >> $SUM; exit 1; }
echo "addB start $(date -u +%FT%TZ) head=5360efdd sha=$(sha256sum $0 | cut -c1-16)" >> $SUM
bash scripts/check-current-contracts.sh > $O/addB_contract_gate.log 2>&1; echo "addB_contract_gate rc=$?" >> $SUM
bash scripts/check-contract-doc-enforcement.sh > $O/addB_doc_enforcement.log 2>&1; echo "addB_doc_enforcement rc=$?" >> $SUM
env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 > $O/addB_hosted_lib.log 2>&1; echo "addB_hosted_lib rc=$? $(grep '^test result' $O/addB_hosted_lib.log)" >> $SUM
echo "addB porcelain=$(git status --porcelain | wc -l)" >> $SUM
echo RT1A_ADDB_DONE >> $SUM
