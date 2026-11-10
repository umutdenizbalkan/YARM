#!/usr/bin/env bash
# Per-test attribution of lock-order pairs (analysis appended to H6; not a retry of it).
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/id1-evidence/qual/H6_attribution; mkdir -p $OUT
attr() { awk '
  { if (match($0, /^test [^ ]+ \.\.\. /)) { cur=$2 }
    while (match($0, /YARM_LOCK_ORDER_WARN current=[a-z_]+ previous=[0-9]+/)) {
      w=substr($0, RSTART, RLENGTH); split(w, a, " "); c[cur" "a[2]" "a[3]]++; $0=substr($0, RSTART+RLENGTH) } }
  END { for (k in c) print k, c[k] }' | sort; }
(cd $S/id1-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/id1q-target cargo test --lib -- --test-threads=1 --nocapture --skip irq_delivery1 2>&1) | attr > $OUT/F.txt
(cd $S/ia1-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/ia1q-target cargo test --lib -- --test-threads=1 --nocapture --skip irq_delivery1 2>&1) | attr > $OUT/B.txt
join -a1 -a2 -e0 -o 0,1.2,2.2 <(awk '{print $1"|"$2"|"$3, $4}' $OUT/F.txt | sort) <(awk '{print $1"|"$2"|"$3, $4}' $OUT/B.txt | sort) | awk '$2!=$3 {print $1, "F="$2, "B="$3, "d="$2-$3}' > $OUT/diff.txt
echo ATTR_DONE
