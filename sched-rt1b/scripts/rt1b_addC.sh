#!/usr/bin/env bash
# RT-SCHED1B addendum C: the documentation commit D = e1aee62d (docs only on top of 1e6cfd40).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
D=e1aee62d
O=$S/rt1b-evidence/addC
W=$S/rt1b-wt
SUM=$O/summary.txt
mkdir -p $O
step() {
  local name=$1; shift
  local por; por=$(git -C $W status --porcelain | wc -l)
  echo "=== $name [$(git -C $W rev-parse --short=12 HEAD) porcelain=$por]: $* (start $(date -u +%H:%M:%S))" >> $SUM
  local t0=$SECONDS
  ( cd $W && "$@" ) > $O/$name.log 2>&1
  local rc=$?
  echo "$name rc=$rc secs=$((SECONDS - t0))" >> $SUM
}
echo "rt1b addC start $(date -u +%FT%TZ) D=$D sha=$(sha256sum $0 | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $D
step C1_contract_gate bash scripts/check-current-contracts.sh
step C2_doc_enforcement bash scripts/check-contract-doc-enforcement.sh
step C3_hosted_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
step C4_diff_from_C2 git diff --stat 1e6cfd40 $D
echo "rt1b addC end $(date -u +%FT%TZ)" >> $SUM
echo RT1B_ADDC_DONE >> $SUM
