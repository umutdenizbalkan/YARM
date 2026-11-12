#!/usr/bin/env bash
# FUTEX-WAIT-ATOMIC1 hosted negative controls: each control on a clean checkout of F, filter
# `futex`; a patch that does not apply is recorded as PATCH_FAILED, never counted.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=${OUT:-$S/fwa-evidence/nc}; F=${F:?}
mkdir -p $OUT; [ "${APPEND:-0}" = 1 ] || : > $OUT/summary.txt; echo "--- run $(date -u +%FT%TZ) ncs=${NCS:-all}" >> $OUT/summary.txt
WT=$S/fwanc-wt
[ -d $WT ] || git -C $S/fwa-wt worktree add -q --detach $WT $F
echo "candidate=$F nc_script_sha=$(sha256sum $S/fwa_nc.py | cut -c1-16)" >> $OUT/summary.txt
run() { (cd $WT && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/fwa-nc-target cargo test --lib futex -- --test-threads=1); }
summ() {
  local tag=$1 rc=$2
  res=$(grep -aE '^test result' $OUT/$tag.log | tail -1)
  failed=$(awk '/^failures:$/{f++} f==2 && /^    /{print $1}' $OUT/$tag.log | sed 's/.*tests:://' | sort -u | paste -sd',' -)
  build=$(grep -c '^error\[' $OUT/$tag.log)
  echo "$tag rc=$rc build_errors=$build $res FAILED=[$failed]" >> $OUT/summary.txt
}
for nc in ${NCS:-nc1 nc2 nc3 nc4 nc5 nc6 nc7 nc8 nc9 nc10 nc11}; do
  git -C $WT checkout -q -f $F && git -C $WT clean -qfd
  if ! python3 -I $S/fwa_nc.py $WT $nc > $OUT/$nc.patch.txt 2>&1; then
    echo "$nc PATCH_FAILED (not a control result)" >> $OUT/summary.txt; continue
  fi
  git -C $WT diff >> $OUT/$nc.patch.txt
  run > $OUT/$nc.log 2>&1; summ $nc $?
done
git -C $WT checkout -q -f $F && git -C $WT clean -qfd
run > $OUT/F_control.log 2>&1; summ F_control $?
echo NC_DONE >> $OUT/summary.txt
