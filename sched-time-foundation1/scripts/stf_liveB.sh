#!/usr/bin/env bash
# SCHED-TIME-FOUNDATION1 B focused live clock smoke (not the qualification matrix): one FUTEX-TIME1
# witness boot per ISA at B, SMP 2; graded on the TIMEKEEP_SOURCE boot marker and the witness seal.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
WT=$S/stflive-wt; OUT=$S/stf-evidence/B_live; mkdir -p $OUT; touch $OUT/summary.txt
echo "liveB start $(date -u +%FT%TZ) head=$(git -C $WT rev-parse HEAD) porcelain=$(git -C $WT status --porcelain | wc -l)" >> $OUT/summary.txt
for a in riscv64 aarch64 x86_64; do
  (cd $WT && env -u CARGO_TARGET_DIR LOGDIR=$OUT/LB-$a timeout 2400 scripts/qemu-futex-time-witness-smoke.sh $a 2) > $OUT/LB-$a.txt 2>&1
  rc=$?
  src=$(grep -ahr 'TIMEKEEP_SOURCE' $OUT/LB-$a.txt $OUT/LB-$a 2>/dev/null | head -1 | tr -d '\r' | sed 's/.*TIMEKEEP_SOURCE/TIMEKEEP_SOURCE/')
  cal=$(grep -ahr 'TIMEKEEP_X86_TSC_CALIBRATION' $OUT/LB-$a.txt $OUT/LB-$a 2>/dev/null | head -1 | tr -d '\r' | sed 's/.*TIMEKEEP_X86/TIMEKEEP_X86/')
  seal=$(grep -a 'FUTEX_TIME_WITNESS_SEAL' $OUT/LB-$a.txt | tail -1)
  echo "LB-$a rc=$rc | $src | $cal | $seal" >> $OUT/summary.txt
done
echo "liveB end $(date -u +%FT%TZ)" >> $OUT/summary.txt
echo LIVEB_DONE >> $OUT/summary.txt
