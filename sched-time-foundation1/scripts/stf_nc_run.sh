#!/usr/bin/env bash
# SCHED-TIME-FOUNDATION1 negative-control runner: full hosted lib per control; resumable.
# Usage: stf_nc_run.sh <candidate-sha> <outdir> <control>...
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=$1; OUT=$2; shift 2; mkdir -p $OUT; touch $OUT/summary.txt
W=$S/stfnc-wt
[ -d $W ] || git -C $S/stf-wt worktree add -q --detach $W $F
echo "--- run $(date -u +%FT%TZ) candidate=$F nc_script_sha=$(sha256sum $S/stf_nc.py | cut -c1-16)" >> $OUT/summary.txt
run() { # run <label> [control]
  local L=$1 c=${2:-}
  grep -q "^$L " $OUT/summary.txt && return 0
  git -C $W checkout -q -f $F && git -C $W clean -qfd
  if [ -n "$c" ]; then python3 -I $S/stf_nc.py $W $c > $OUT/$L.patch.txt 2>&1; git -C $W diff >> $OUT/$L.patch.txt; fi
  (cd $W && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/stfnc-target timeout 1200 cargo test --lib -- --test-threads=1) > $OUT/$L.log 2>&1
  local rc=$?
  local failed=$(grep -aE '^test .* FAILED$' $OUT/$L.log | sed -E 's/^test (kernel::boot::tests::|kernel::scheduler::)?//; s/ \.\.\. FAILED$//' | tr '\n' ',')
  local cerr=$(grep -acE '^error(\[E[0-9]+\])?: ' $OUT/$L.log); local tf=$(grep -ac '^error: test failed' $OUT/$L.log)
  echo "$L rc=$rc compile_errors=$((cerr - tf)) $(grep -a 'test result' $OUT/$L.log | tail -1) FAILED=[$failed]" >> $OUT/summary.txt
}
for c in "$@"; do run $c $c; done
run F_control
echo NC_DONE >> $OUT/summary.txt
