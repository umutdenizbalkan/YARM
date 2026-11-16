#!/usr/bin/env bash
# SCHED-LOCK2A addendum B (declared after A): attribute A1's L2 outcome (oracle armed, seal
# result=blocked, no IPCCALL_DIRECT_SMP_SERVER_BLOCKED) by running the SAME repaired script on the
# baseline a1b06b2c, with ONLY cabe3ccc's script change applied as an uncommitted scratch patch.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
O=$S/sl2-evidence/qual; SUM=$O/summary.txt; QB=$S/psc-dev-wt
echo "addendum B start $(date -u +%FT%TZ) sha=$(sha256sum $0 | cut -c1-16)" >> $SUM
git -C $QB checkout -q --detach a1b06b2c
git -C $S/sl2-wt show cabe3ccc -- scripts/qemu-ipccall-direct-x86_64-smp-request-smoke.sh > $O/B1_scratch_script_fix.patch
git -C $QB apply $O/B1_scratch_script_fix.patch
echo "=== B1_L2_on_baseline_with_script_fix [$(git -C $QB rev-parse --short=12 HEAD) porcelain=$(git -C $QB status --porcelain | wc -l) (the script patch only)] (start $(date -u +%H:%M:%S))" >> $SUM
t0=$SECONDS
( cd $QB && env -u CARGO_TARGET_DIR LOGDIR=$O/B1_L2_base scripts/qemu-ipccall-direct-x86_64-smp-request-smoke.sh ) > $O/B1_L2_on_baseline_with_script_fix.log 2>&1
echo "B1_L2_on_baseline_with_script_fix rc=$? secs=$((SECONDS - t0))" >> $SUM
git -C $QB checkout -q -- scripts/qemu-ipccall-direct-x86_64-smp-request-smoke.sh
echo "B2 compare: C server_blocked=$(grep -ac IPCCALL_DIRECT_SMP_SERVER_BLOCKED $O/A1_L2/boot.log) base server_blocked=$(grep -ac IPCCALL_DIRECT_SMP_SERVER_BLOCKED $O/B1_L2_base/boot.log)" >> $SUM
echo ADDENDUM_B_DONE >> $SUM
