#!/usr/bin/env bash
# SCHED-LOCK2B focused qualification: candidate C vs baseline c9fbc7ef. Steps in order, every log
# kept, nothing rerun in place. Unrelated suites reuse the accepted baseline; no historical matrix.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C=${C:?candidate}
O=$S/sl2b-evidence/qual
W=$S/sl2b-wt
BW=$S/sl2b-base-wt
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
pairs() { awk '{ while (match($0, /YARM_LOCK_ORDER_WARN current=[a-z_]+ previous=[0-9]+/)) { w=substr($0, RSTART, RLENGTH); split(w, a, " "); c[a[2]" "a[3]]++; $0=substr($0, RSTART+RLENGTH) } } END { for (k in c) print k, c[k] }' | sort; }
echo "sl2b qual start $(date -u +%FT%TZ) C=$C sha=$(sha256sum $0 | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $C
step H1_fmt cargo fmt --all -- --check
step H2_hosted_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
step H3_integration env -u CARGO_TARGET_DIR cargo test -q --tests --no-fail-fast -- --test-threads=1
for f in sl2b_ sl2a_ task_enqueue sl1_ sched_lock1 lock_order abi syscall_abi u9exit reap stage199d stage199a2b2d stage199a2b3 u9pf1 u9pagefault3 u9recvblock u9_recv_block overtaken census futex ap_sched1 ipc_timeout sr_place1; do
  step H4_subset_$f env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 $f
done
# lock-order: descending sequential pair TYPES, candidate vs baseline, whole serial lib suite
step H5_lockorder_C bash -c "env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 --nocapture > $O/H5_C.raw.log 2>&1; echo rc=\$?"
step H5_lockorder_B bash -c "cd $BW && env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 --nocapture > $O/H5_B.raw.log 2>&1; echo rc=\$?"
pairs < $O/H5_C.raw.log > $O/H5_C.pairs.txt; pairs < $O/H5_B.raw.log > $O/H5_B.pairs.txt
diff <(awk '{print $1, $2}' $O/H5_C.pairs.txt) <(awk '{print $1, $2}' $O/H5_B.pairs.txt) > $O/H5_pairtypes.diff; echo "H5_pairtypes C=$(wc -l < $O/H5_C.pairs.txt) B=$(wc -l < $O/H5_B.pairs.txt) diff_lines=$(wc -l < $O/H5_pairtypes.diff)" >> $SUM
gzip -f $O/H5_C.raw.log $O/H5_B.raw.log
step G1_contract_gate bash scripts/check-current-contracts.sh
step G2_doc_enforcement bash scripts/check-contract-doc-enforcement.sh
# builds (default profile) and the live cells
step B1_build_x86_64 env -u CARGO_TARGET_DIR scripts/build-qemu-x86_64-artifacts.sh
step L1_x86_64_direct_request_reply env -u CARGO_TARGET_DIR LOGDIR=$O/L1 scripts/qemu-ipccall-reply-direct-x86_64-smoke.sh
step B2_build_aarch64 env -u CARGO_TARGET_DIR scripts/build-qemu-aarch64-artifacts.sh
step L2_aarch64_direct_request_reply env -u CARGO_TARGET_DIR LOGDIR=$O/L2 scripts/qemu-ipccall-reply-direct-aarch64-smoke.sh
step L3_aarch64_exit_current_task env -u CARGO_TARGET_DIR LOGDIR=$O/L3 scripts/qemu-aarch64-exit-current-task-smoke.sh
step B3_build_riscv64 env -u CARGO_TARGET_DIR scripts/build-qemu-riscv64-artifacts.sh
step L4_riscv64_direct_request_reply env -u CARGO_TARGET_DIR LOGDIR=$O/L4 scripts/qemu-ipccall-reply-direct-riscv64-smoke.sh
step L5_riscv64_exit_current_task env -u CARGO_TARGET_DIR LOGDIR=$O/L5 scripts/qemu-riscv-exit-current-task-smoke.sh
step Z_home_refusal_scan bash -c "grep -rac --include='*.log' 'SCHED_HOME_REFUSED' $O/L1 $O/L2 $O/L3 $O/L4 $O/L5 | grep -v ':0\$' && exit 1 || echo NO_HOME_REFUSAL_IN_ANY_LIVE_LOG"
# negative controls (tree restored after each; expected rc != 0)
step NC1_home_lookup_ignores_stamp bash -c "sed -i 's/^    if stamp != 0 \&\& tcb.incarnation != stamp {\$/    if false \&\& stamp != 0 \&\& tcb.incarnation != stamp {/' src/kernel/task_enqueue.rs && git diff --stat && env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 sl2b_; rc=\$?; git checkout -- src/kernel/task_enqueue.rs; exit \$rc"
step NC2_request_stale_read_as_absent bash -c "python3 -c \"
p='src/kernel/ipccall_direct_txn.rs'; s=open(p).read()
a='''            crate::runtime::HomeMembership::Stale => {
                self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
                return Err(IpcCallDirectError::WouldBlock);
            }'''
assert s.count(a)==1; open(p,'w').write(s.replace(a,'            crate::runtime::HomeMembership::Stale => {}'))
\" && git diff --stat && env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 sl2b_a_request; rc=\$?; git checkout -- src/kernel/ipccall_direct_txn.rs; exit \$rc"
echo "post-NC porcelain=$(git -C $W status --porcelain | wc -l)" >> $SUM
echo "sl2b qual end $(date -u +%FT%TZ)" >> $SUM
echo SL2B_QUAL_DONE >> $SUM
