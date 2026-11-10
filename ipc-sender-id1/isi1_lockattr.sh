#!/usr/bin/env bash
# Per-test attribution of lock-order pairs, F vs B, on the common test set.
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/isi1-evidence/qual/H6_attribution; mkdir -p $OUT
attr() { awk '
  { if (match($0, /^test [^ ]+ \.\.\. /)) { cur=$2 }
    while (match($0, /YARM_LOCK_ORDER_WARN current=[a-z_]+ previous=[0-9]+/)) {
      w=substr($0, RSTART, RLENGTH); split(w, a, " "); c[cur" "a[2]" "a[3]]++; $0=substr($0, RSTART+RLENGTH) } }
  END { for (k in c) print k, c[k] }' | sort; }
(cd $S/isi1-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/isi1q-target cargo test --lib -- --test-threads=1 --nocapture --skip ipc_sender_id1 --skip a_task_not_blocked_in_the_named_send_cycle_is_never_woken 2>&1) | tee $OUT/F.raw.log | attr > $OUT/F.txt
(cd $S/id1-wt && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/id1q-target cargo test --lib -- --test-threads=1 --nocapture 2>&1) | tee $OUT/B.raw.log | attr > $OUT/B.txt
join -a1 -a2 -e0 -o 0,1.2,2.2 <(awk '{print $1"|"$2"|"$3, $4}' $OUT/F.txt | sort) <(awk '{print $1"|"$2"|"$3, $4}' $OUT/B.txt | sort) | awk '$2!=$3 {print $1, "F="$2, "B="$3, "d="$2-$3}' > $OUT/diff.txt
# For (c): every test in which B logged a refused sender publication.
grep -aE '^test [^ ]+ \.\.\. |U6_SEND_COMPLETION_REFUSED_STALE' $OUT/B.raw.log | awk '/^test /{cur=$2} /U6_SEND_COMPLETION_REFUSED_STALE/{print cur}' | sort -u > $OUT/B_refused_publication_tests.txt
rm -f $OUT/F.raw.log $OUT/B.raw.log.gz; gzip -f $OUT/B.raw.log
echo ATTR_DONE
