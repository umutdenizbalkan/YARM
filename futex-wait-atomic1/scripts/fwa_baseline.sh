#!/usr/bin/env bash
# The frozen candidate's wait_atomic1 suite, verbatim, against the baseline c2b9893e, with the
# test-only hooks (fwa_hook.py) and nothing else changed.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=${F:?}; OUT=$S/fwa-evidence; WT=$S/fwab-wt
git -C $WT checkout -q -f c2b9893e && git -C $WT clean -qfd
git -C $S/fwa-wt show $F:src/kernel/boot/tests.rs > $S/fwa_F_tests.rs
python3 -I $S/fwa_hook.py $WT $S/fwa_F_tests.rs > $OUT/baseline_harness_apply.txt 2>&1
git -C $WT diff > $OUT/baseline_harness.patch
(cd $WT && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/fwab-target cargo test --lib wait_atomic1 -- --test-threads=1) > $OUT/baseline_repro_c2b9893e.log 2>&1
echo "baseline rc=$? suite_from=$F head=$(git -C $WT rev-parse --short=8 HEAD)" >> $OUT/baseline_harness_apply.txt
git -C $WT checkout -q -f c2b9893e && git -C $WT clean -qfd
rm -rf $S/fwab-target
echo BASELINE_DONE >> $OUT/baseline_harness_apply.txt
