#!/usr/bin/env bash
# SCHED-LOCK2A addendum A (declared after the main run; main-run logs kept as recorded):
#  A1  L2 (the SMP oracle gap cell) on C' = cabe3ccc — its script had a bash syntax error on the
#      baseline and on C (15114160), repaired in cabe3ccc; kernel source identical to C.
#  A2  L1 on the baseline a1b06b2c — attributes L1's `result=blocked` seal and the
#      IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL line to pre-existing behaviour or to C.
#  A3  the home-refusal scan over text logs only (the main scan also matched kernel images).
#  A4  lock-order pair types, hosted lib --nocapture, C vs a1b06b2c.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
O=$S/sl2-evidence/qual
SUM=$O/summary.txt
step() {
  local name=$1 dir=$2; shift 2
  local por; por=$(git -C $dir status --porcelain | wc -l)
  echo "=== $name [$(git -C $dir rev-parse --short=12 HEAD) porcelain=$por]: $* (start $(date -u +%H:%M:%S))" >> $SUM
  local t0=$SECONDS
  ( cd $dir && "$@" ) > $O/$name.log 2>&1
  echo "$name rc=$? secs=$((SECONDS - t0))" >> $SUM
}
echo "addendum A start $(date -u +%FT%TZ) sha=$(sha256sum $0 | cut -c1-16)" >> $SUM
QC=$S/psc-q-wt; QB=$S/psc-dev-wt
git -C $QC checkout -q --detach cabe3ccc
git -C $QB checkout -q --detach a1b06b2c
step A1_L2_x86_64_smp2_direct_request_oracle_armed $QC env -u CARGO_TARGET_DIR LOGDIR=$O/A1_L2 scripts/qemu-ipccall-direct-x86_64-smp-request-smoke.sh
step A2_L1_on_baseline $QB env -u CARGO_TARGET_DIR LOGDIR=$O/A2_L1_base scripts/qemu-ipccall-reply-direct-x86_64-smp-smoke.sh
step A3_home_refusal_scan_logs $QC bash -c "n=\$(grep -arc 'SCHED_HOME_REFUSED' --include='*.log' $O/L1 $O/L3 $O/L4 $O/A1_L2 $O/L5.boot.log | grep -v ':0\$' | wc -l); echo files_with_refusals=\$n; test \$n -eq 0 && echo NO_HOME_REFUSAL_IN_ANY_LIVE_LOG"
attr='{ if (match($0, /^test [^ ]+ \.\.\. /)) { cur=$2 } while (match($0, /YARM_LOCK_ORDER_WARN current=[a-z_]+ previous=[0-9]+/)) { w=substr($0, RSTART, RLENGTH); split(w, a, " "); c[a[2]" "a[3]]++; $0=substr($0, RSTART+RLENGTH) } } END { for (k in c) print k, c[k] }'
step A4_lockorder_C $QC bash -c "env -u CARGO_TARGET_DIR cargo test --lib -- --test-threads=1 --nocapture 2>&1 | awk '$attr' | sort > $O/A4_pairs_C.txt; wc -l < $O/A4_pairs_C.txt"
step A4_lockorder_B $QB bash -c "env -u CARGO_TARGET_DIR cargo test --lib -- --test-threads=1 --nocapture 2>&1 | awk '$attr' | sort > $O/A4_pairs_B.txt; wc -l < $O/A4_pairs_B.txt"
step A4_pair_types_compare $QC bash -c "cut -d' ' -f1,2 $O/A4_pairs_C.txt > $O/A4_types_C.txt; cut -d' ' -f1,2 $O/A4_pairs_B.txt > $O/A4_types_B.txt; echo C_types=\$(wc -l < $O/A4_types_C.txt) B_types=\$(wc -l < $O/A4_types_B.txt); echo 'only in C:'; comm -23 $O/A4_types_C.txt $O/A4_types_B.txt; echo 'only in B:'; comm -13 $O/A4_types_C.txt $O/A4_types_B.txt; test -z \"\$(comm -23 $O/A4_types_C.txt $O/A4_types_B.txt)\" && echo NO_NEW_DESCENDING_PAIR_TYPE"
echo "addendum A end $(date -u +%FT%TZ)" >> $SUM
echo ADDENDUM_A_DONE >> $SUM
