#!/usr/bin/env bash
# SCHED-LOCK-FAIR1 addendum B: the documentation commit D (contract §22 and status lines, stage
# record) on top of the qualified candidate b7263d21. D must change only the two documents; then
# the contract gate, doc enforcement, fmt and the serial lib run on D.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
W=$S/fair1-wt
D=${D:?docs commit}
O=$S/fair1-evidence/qual/addB
mkdir -p $O
SUM=$O/summary.txt
echo "addB start $(date -u +%FT%TZ) D=$D sha=$(sha256sum $0 | cut -c1-16)" > $SUM
git -C $W checkout -q --detach $D
echo "files changed from b7263d21: $(git -C $W diff --name-only b7263d21 $D | tr '\n' ' ')" >> $SUM
echo "parent: $(git -C $W rev-parse --short=8 $D^)" >> $SUM
step() { local n=$1; shift; ( cd $W && "$@" ) > $O/$n.log 2>&1; echo "$n rc=$? porcelain=$(git -C $W status --porcelain | wc -l)" >> $SUM; }
step B1_contract_gate bash scripts/check-current-contracts.sh
step B2_doc_enforcement bash scripts/check-contract-doc-enforcement.sh
step B3_fmt cargo fmt --all -- --check
step B4_serial_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
grep -a "^test result" $O/B4_serial_lib.log >> $SUM
echo ADDB_DONE >> $SUM
