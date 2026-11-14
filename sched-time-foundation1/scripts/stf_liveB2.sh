#!/usr/bin/env bash
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
WT=$S/stflive-wt; OUT=$S/stf-evidence/B_live
for lab in LB-riscv64-r2:riscv64 LB-riscv64-r3:riscv64 LB-x86_64:x86_64; do
  l=${lab%%:*}; a=${lab##*:}
  grep -q "^$l rc=" $OUT/summary.txt && continue
  (cd $WT && env -u CARGO_TARGET_DIR LOGDIR=$OUT/$l timeout 2400 scripts/qemu-futex-time-witness-smoke.sh $a 2) > $OUT/$l.txt 2>&1
  rc=$?
  src=$(grep -ahr 'TIMEKEEP_SOURCE' $OUT/$l/boot.log 2>/dev/null | head -1 | tr -d '\r' | sed 's/.*TIMEKEEP_SOURCE/TIMEKEEP_SOURCE/')
  cal=$(grep -ahr 'TIMEKEEP_X86_TSC_CALIBRATION' $OUT/$l/boot.log 2>/dev/null | head -1 | tr -d '\r' | sed 's/.*TIMEKEEP_X86/TIMEKEEP_X86/')
  seal=$(grep -a 'FUTEX_TIME_WITNESS_SEAL' $OUT/$l.txt | tail -1)
  fails=$(grep -a '\]\[fail\]' $OUT/$l.txt | sort -u | head -4 | tr '\n' ';')
  lost=$(grep -ac PRINTK_RECORDS_LOST $OUT/$l/boot.log 2>/dev/null)
  echo "$l rc=$rc | $src | $cal | $seal | printk_lost_lines=$lost | $fails" >> $OUT/summary.txt
done
echo "addendum-1 end $(date -u +%FT%TZ)" >> $OUT/summary.txt
