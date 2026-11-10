#!/usr/bin/env bash
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/id1-evidence/nc; F=${F:?}; B=c703226
mkdir -p $OUT; : > $OUT/summary.txt
T="irq_delivery1 irq_auth1 u9irq1 u9irqfinal irq notification notif_waiter1 notif_lifetime1 stage21 stage22 stage23 selected_arch_trap_entry"
run() { (cd $S/nc-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/nc-target cargo test --lib -- $T --test-threads=1); }
summ() {
  local tag=$1 rc=$2
  res=$(grep -E '^test result' $OUT/$tag.log | tail -1)
  failed=$(grep -E '^    kernel::' $OUT/$tag.log | sort -u | sed 's/.*tests:://' | paste -sd',' -)
  echo "$tag rc=$rc $res FAILED=[$failed]" >> $OUT/summary.txt
}
# Baseline reproducers: c703226 + only the test-only completion counter + the tests (no fix).
git -C $S/nc-wt checkout -q -f $B && git -C $S/nc-wt clean -qfd
git -C $S/nc-wt show $F:src/arch/hal_adapters.rs > $S/nc-wt/src/arch/hal_adapters.rs
python3 - "$S" <<'PY'
import sys; S=sys.argv[1]
p=S+"/nc-wt/src/kernel/boot/tests.rs"; s=open(p).read().rstrip("\n")+"\n"+open(S+"/id1_tests.rs").read()
open(p,"w").write(s)
PY
(git -C $S/nc-wt rev-parse HEAD; git -C $S/nc-wt diff --stat) > $OUT/baseline.patch.txt
(cd $S/nc-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/nc-target cargo test --lib -- irq_delivery1 --test-threads=1) > $OUT/baseline.log 2>&1; summ baseline $?
for nc in nc1 nc2 nc3 nc4 nc5 nc6 nc7 nc8 nc9 nc10; do
  git -C $S/nc-wt checkout -q -f $F && git -C $S/nc-wt clean -qfd
  python3 -I $S/id1_nc.py $S/nc-wt $nc > $OUT/$nc.patch.txt 2>&1
  git -C $S/nc-wt diff >> $OUT/$nc.patch.txt
  run > $OUT/$nc.log 2>&1; summ $nc $?
done
git -C $S/nc-wt checkout -q -f $F && git -C $S/nc-wt clean -qfd
run > $OUT/F_control.log 2>&1; summ F_control $?
echo NC_DONE >> $OUT/summary.txt
