#!/usr/bin/env bash
# Per-test attribution of lock-order pairs, F vs B, on the common test set, plus the mechanical
# lists for the predeclared change classes.
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/ff1-evidence/qual/H6_attribution; mkdir -p $OUT
attr() { awk '
  { if (match($0, /^test [^ ]+ \.\.\. /)) { cur=$2 }
    while (match($0, /YARM_LOCK_ORDER_WARN current=[a-z_]+ previous=[0-9]+/)) {
      w=substr($0, RSTART, RLENGTH); split(w, a, " "); c[cur" "a[2]" "a[3]]++; $0=substr($0, RSTART+RLENGTH) } }
  END { for (k in c) print k, c[k] }' | sort; }
marks() { grep -aE '^test [^ ]+ \.\.\. |'"$2" "$1" | awk '/^test /{cur=$2} !/^test /{print cur}' | sort -u; }
(cd $S/ff1-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/ff1q-target cargo test --lib -- --test-threads=1 --nocapture --skip futex_found1 2>&1) > $OUT/F.raw.log
(cd $S/er1-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/ff1qb-target cargo test --lib -- --test-threads=1 --nocapture 2>&1) > $OUT/B.raw.log
attr < $OUT/F.raw.log > $OUT/F.txt; attr < $OUT/B.raw.log > $OUT/B.txt
join -a1 -a2 -e0 -o 0,1.2,2.2 <(awk '{print $1"|"$2"|"$3, $4}' $OUT/F.txt | sort) <(awk '{print $1"|"$2"|"$3, $4}' $OUT/B.txt | sort) | awk '$2!=$3 {print $1, "F="$2, "B="$3, "d="$2-$3}' > $OUT/diff.txt
marks $OUT/F.raw.log 'FUTEX_WAKE_ENQUEUE_DEFERRED' > $OUT/f_enqueue_deferred_tests.txt
marks $OUT/F.raw.log 'FUTEX_WAIT_PARK_WOKEN_DURING_PUBLISH' > $OUT/f_woken_during_publish_tests.txt
gzip -f $OUT/F.raw.log $OUT/B.raw.log
echo ATTR_DONE
