#!/usr/bin/env bash
# SCHED-TIME-FOUNDATION1 C focused live run (predeclared, sequential, no retries):
#   LC-<isa>      the sched-time (sleep/clock/rotation) witness at C, x1 per ISA, SMP 2
#   LCF-riscv64   the FUTEX-TIME1 witness at C on RISC-V: the printk-ring remedy re-check
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
WT=$S/stflive-wt; OUT=$S/stf-evidence/C_live; mkdir -p $OUT; touch $OUT/summary.txt
echo "liveC start $(date -u +%FT%TZ) head=$(git -C $WT rev-parse HEAD) porcelain=$(git -C $WT status --porcelain | wc -l)" >> $OUT/summary.txt
for a in riscv64 aarch64 x86_64; do
  l=LC-$a; grep -q "^$l rc=" $OUT/summary.txt && continue
  (cd $WT && env -u CARGO_TARGET_DIR LOGDIR=$OUT/$l timeout 3000 scripts/qemu-sched-time-witness-smoke.sh $a 2) > $OUT/$l.txt 2>&1
  rc=$?
  echo "$l rc=$rc $(grep -a 'SCHED_TIME_WITNESS_SEAL' $OUT/$l.txt | tail -1) printk_lost=$(grep -ac PRINTK_RECORDS_LOST $OUT/$l/boot.log 2>/dev/null)" >> $OUT/summary.txt
  grep -a '\]\[fail\]' $OUT/$l.txt | sort | uniq -c | head -8 | sed 's/^/    /' >> $OUT/summary.txt
done
l=LCF-riscv64
if ! grep -q "^$l rc=" $OUT/summary.txt; then
  (cd $WT && env -u CARGO_TARGET_DIR LOGDIR=$OUT/$l timeout 2400 scripts/qemu-futex-time-witness-smoke.sh riscv64 2) > $OUT/$l.txt 2>&1
  rc=$?
  echo "$l rc=$rc $(grep -a 'FUTEX_TIME_WITNESS_SEAL' $OUT/$l.txt | tail -1) printk_lost=$(grep -ac PRINTK_RECORDS_LOST $OUT/$l/boot.log 2>/dev/null) commits=$(grep -ac 'YIELD_SPLIT_COMMITTED cpu=' $OUT/$l/boot.log 2>/dev/null)" >> $OUT/summary.txt
  grep -a '\]\[fail\]' $OUT/$l.txt | sort | uniq -c | head -8 | sed 's/^/    /' >> $OUT/summary.txt
fi
echo "liveC end $(date -u +%FT%TZ)" >> $OUT/summary.txt
echo LIVEC_DONE >> $OUT/summary.txt
