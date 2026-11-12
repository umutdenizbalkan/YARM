#!/usr/bin/env bash
# Live, RISC-V SMP 2: the ecall witness at B+W x2 (must fail on a3/a7 only), at F x3, the live
# controls (F+NC1, F+NC11), and the FUTEX-WAIT-ATOMIC1 wait witness at F x1 per ISA.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=${F:?}; BW=${BW:?}; OUT=${OUT:-$S/ea7-evidence/live}; mkdir -p $OUT; : > $OUT/summary.txt
WT=$S/ea7-wt; LWT=$S/ea7live-wt
[ -d $LWT ] || git -C $WT worktree add -q --detach $LWT $F
ew() { # ew <label> <dir>
  (cd $2 && LOGDIR=$OUT/$1 timeout 2400 scripts/qemu-riscv-ecall-witness-smoke.sh 2) > $OUT/$1.txt 2>&1
  local rc=$?
  echo "$1 [$(git -C $2 rev-parse --short=8 HEAD) porcelain=$(git -C $2 status --porcelain | wc -l)] rc=$rc $(grep -a 'RISCV_ECALL_WITNESS_SEAL' $OUT/$1.txt | tail -1)" >> $OUT/summary.txt
  grep -a '\[riscv-ecall-grade\]\[fail\]' $OUT/$1.txt | sed -E 's/cycle [0-9]+/cycle N/; s/0x[0-9a-f]+/X/g' | sort | uniq -c | sort -rn | head -8 | sed 's/^/    /' >> $OUT/summary.txt
}
echo "candidate=$(git -C $WT rev-parse --short=8 HEAD) porcelain=$(git -C $WT status --porcelain | wc -l) baseline_witness=$BW" >> $OUT/summary.txt
git -C $LWT checkout -q -f $BW && git -C $LWT clean -qfd
for n in 1 2; do ew LB$n $LWT; done
for n in 1 2 3; do ew LF$n $WT; done
for c in nc1 nc11; do
  git -C $LWT checkout -q -f $F && git -C $LWT clean -qfd
  python3 -I $S/ea7_nc.py $LWT $c > $OUT/L-$c.patch.txt 2>&1; git -C $LWT diff >> $OUT/L-$c.patch.txt
  git -C $LWT -c user.name=nc -c user.email=nc@localhost commit -qam "$c (live control, never pushed)"
  ew L-$c $LWT
done
git -C $LWT checkout -q -f $F; rm -rf $LWT/build-*
for a in riscv64 aarch64 x86_64; do
  (cd $WT && LOGDIR=$OUT/LW-$a timeout 2400 scripts/qemu-futex-wait-witness-smoke.sh $a 2) > $OUT/LW-$a.txt 2>&1
  rc=$?
  echo "LW-$a rc=$rc $(grep -a 'FUTEX_WAIT_WITNESS_SEAL' $OUT/LW-$a.txt | tail -1)" >> $OUT/summary.txt
  grep -a '\[futex-wait-grade\]\[fail\]' $OUT/LW-$a.txt | head -4 | sed 's/^/    /' >> $OUT/summary.txt
done
echo LIVE_DONE >> $OUT/summary.txt
