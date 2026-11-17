#!/usr/bin/env bash
# SCHED-LOCK2B addendum A (declared after the main run): the same three direct request/reply smokes
# on baseline c9fbc7ef, to attribute `IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL ... nr7_ok=0 result=fail`
# seen in the candidate's L1/L2/L4 (a diagnostic the scripts do not gate on).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
O=$S/sl2b-evidence/addA
BW=$S/sl2b-base-wt
SUM=$O/summary.txt
mkdir -p $O
step() {
  local name=$1; shift
  echo "=== $name [$(git -C $BW rev-parse --short=12 HEAD) porcelain=$(git -C $BW status --porcelain | wc -l)]: $* (start $(date -u +%H:%M:%S))" >> $SUM
  local t0=$SECONDS
  ( cd $BW && "$@" ) > $O/$name.log 2>&1
  echo "$name rc=$? secs=$((SECONDS - t0))" >> $SUM
}
echo "sl2b addA start $(date -u +%FT%TZ) sha=$(sha256sum $0 | cut -c1-16)" >> $SUM
step A1_x86_64_direct_base env -u CARGO_TARGET_DIR LOGDIR=$O/A1 scripts/qemu-ipccall-reply-direct-x86_64-smoke.sh
step A2_aarch64_direct_base env -u CARGO_TARGET_DIR LOGDIR=$O/A2 scripts/qemu-ipccall-reply-direct-aarch64-smoke.sh
step A3_riscv64_direct_base env -u CARGO_TARGET_DIR LOGDIR=$O/A3 scripts/qemu-ipccall-reply-direct-riscv64-smoke.sh
for d in A1 A2 A3; do
  echo "$d $(grep -rah --include='*.norm.log' -o 'IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL nr6_ok=[01] nr7_ok=[01] census_ok=[01] result=[a-z]*' $O/$d | sort | uniq -c | tr '\n' ';') $(grep -rah --include='*.log' -o 'STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL .*' $O/$d | sort -u | tr '\n' ';')" >> $SUM
done
echo SL2B_ADDA_DONE >> $SUM
