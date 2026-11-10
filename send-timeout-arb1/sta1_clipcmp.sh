#!/usr/bin/env bash
# Clippy warning classes, F (H2's log) vs a fresh clippy of B.
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/sta1-evidence/qual
rm -rf $S/sta1clipB-target
(cd $S/isi1-wt && CARGO_TARGET_DIR=$S/sta1clipB-target cargo clippy --lib --features hosted-dev) > $OUT/H2b_B_clippy.log 2>&1
for x in F B; do
  f=$OUT/H2_clippy.log; [ $x = B ] && f=$OUT/H2b_B_clippy.log
  grep -E "^warning: " $f | grep -v "generated [0-9]* warnings" | sed -E 's/`[^`]*`/`_`/g' | sort | uniq -c | sort -k2 > $OUT/H2_$x.classes
  echo "$x: $(grep 'yarm` (lib) generated' $f)"
done
rm -rf $S/sta1clipB-target
diff $OUT/H2_F.classes $OUT/H2_B.classes && echo CLASSES_IDENTICAL
