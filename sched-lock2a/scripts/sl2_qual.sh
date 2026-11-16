#!/usr/bin/env bash
# SCHED-LOCK2A focused qualification: candidate C vs baseline a1b06b2c. Steps in order, every log
# kept, nothing rerun in place.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C=${C:?candidate}
O=$S/sl2-evidence/qual
W=$S/psc-q-wt
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
echo "sl2a qual start $(date -u +%FT%TZ) C=$C sha=$(sha256sum $0 | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $C
step H1_fmt cargo fmt --all -- --check
step H2_hosted_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
step H3_integration env -u CARGO_TARGET_DIR cargo test -q --tests --no-fail-fast -- --test-threads=1
for f in sl2a_ task_enqueue sl1_ sched_lock1 lock_order abi syscall_abi u9exit reap futex_wake_cap1 ipc_timeout sr_place1 census stage199d overtaken qemu_context1 ap_sched1 u9spawn1; do
  step H4_subset_$f env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 $f
done
step G1_contract_gate bash scripts/check-current-contracts.sh
step G2_doc_enforcement bash scripts/check-contract-doc-enforcement.sh
# builds (default profile) and the live cells
step B1_build_x86_64 env -u CARGO_TARGET_DIR scripts/build-qemu-x86_64-artifacts.sh
step L5_x86_64_smp2_ordinary_oracle_off env -u CARGO_TARGET_DIR QEMU_SMP=2 LOGFILE=$O/L5.boot.log scripts/qemu-x86_64-core-smoke.sh
step L1_x86_64_smp2_direct_request_reply env -u CARGO_TARGET_DIR LOGDIR=$O/L1 scripts/qemu-ipccall-reply-direct-x86_64-smp-smoke.sh
step L2_x86_64_smp2_direct_request_oracle_armed env -u CARGO_TARGET_DIR LOGDIR=$O/L2 scripts/qemu-ipccall-direct-x86_64-smp-request-smoke.sh
step B2_build_aarch64 env -u CARGO_TARGET_DIR scripts/build-qemu-aarch64-artifacts.sh
step L3_aarch64_direct_ipc env -u CARGO_TARGET_DIR LOGDIR=$O/L3 scripts/qemu-ipccall-reply-direct-aarch64-smoke.sh
step B3_build_riscv64 env -u CARGO_TARGET_DIR scripts/build-qemu-riscv64-artifacts.sh
step L4_riscv64_direct_ipc env -u CARGO_TARGET_DIR LOGDIR=$O/L4 scripts/qemu-ipccall-reply-direct-riscv64-smoke.sh
step Z_home_refusal_scan bash -c "grep -arc 'SCHED_HOME_REFUSED' $O/L5.boot.log $O/L1 $O/L2 $O/L3 $O/L4 2>/dev/null | grep -v ':0\$' && exit 1 || echo NO_HOME_REFUSAL_IN_ANY_LIVE_LOG"
echo "sl2a qual end $(date -u +%FT%TZ)" >> $SUM
echo SL2A_QUAL_DONE >> $SUM
