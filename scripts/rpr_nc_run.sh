#!/usr/bin/env bash
# Run every RISCV-PARK-RET1 negative control against the candidate's full hosted lib.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=${F:?}; OUT=${OUT:-$S/rpr-evidence/nc}; NCS=${NCS:-$(seq 1 10)}
W=$S/rprnc-wt; mkdir -p $OUT
[ -d $W ] || git -C $S/rpr-wt worktree add -q --detach $W $F
echo "--- run $(date -u +%FT%TZ) candidate=$F nc_script_sha=$(sha256sum $S/rpr_nc.py | cut -c1-16)" >> $OUT/summary.txt
for i in $NCS; do
  git -C $W checkout -q -f $F && git -C $W clean -qfd
  python3 -I $S/rpr_nc.py $W nc$i > $OUT/nc$i.patch.txt 2>&1; git -C $W diff >> $OUT/nc$i.patch.txt
  (cd $W && CARGO_TARGET_DIR=$S/rprnc-target cargo test --lib --features hosted-dev -- --test-threads=1) > $OUT/nc$i.log 2>&1
  rc=$?
  failed=$(grep -aE '^test .* FAILED$' $OUT/nc$i.log | sed -E 's/^test (kernel::boot::tests::)?//; s/ \.\.\. FAILED$//' | tr '\n' ',')
  errs=$(grep -ac '^error' $OUT/nc$i.log)
  echo "nc$i rc=$rc build_errors=$errs $(grep -a 'test result' $OUT/nc$i.log | tail -1) FAILED=[$failed]" >> $OUT/summary.txt
done
git -C $W checkout -q -f $F
(cd $W && CARGO_TARGET_DIR=$S/rprnc-target cargo test --lib --features hosted-dev -- --test-threads=1) > $OUT/F_control.log 2>&1
echo "F_control rc=$? $(grep -a 'test result' $OUT/F_control.log | tail -1)" >> $OUT/summary.txt
echo NC_DONE >> $OUT/summary.txt
