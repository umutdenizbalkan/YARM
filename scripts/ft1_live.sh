#!/usr/bin/env bash
# FUTEX-TIME1 live qualification (schedule: ft1_schedule.md). Sequential; no retries; a rerun is an
# appended, predeclared addendum, never a substitute.
#   LT-<isa>-<n>  the FUTEX-TIME1 timed-wait witness (A-G) at F, x3 per ISA, SMP 2
#   L-nc2         F + nc2 (timeout publishes Woken), RISC-V: must FAIL the grader
#   L-nc28        F + nc28 (x86 completion into the fields only), x86_64: must FAIL the grader
#   LW-<isa>      FUTEX-WAIT-ATOMIC1 / RISCV-PARK-RET1 wait witness at F, x1 per ISA
#   LE            RISCV-ECALL-A7-RET1 ecall register witness at F (RISC-V)
#   LS-<isa>      the IRQ saturation witness at F, x1 per ISA
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=${F:?}; OUT=${OUT:-$S/ft1-evidence/live}; mkdir -p $OUT; touch $OUT/summary.txt
WT=$S/ft1-wt; LWT=$S/ft1live-wt
[ -d $LWT ] || git -C $WT worktree add -q --detach $LWT $F
stamp() { echo "[$(git -C $1 rev-parse --short=8 HEAD) porcelain=$(git -C $1 status --porcelain | wc -l)]"; }
done_l() { grep -q "^$1 \[" $OUT/summary.txt; }
interrupted() { [ -e $OUT/$1 ] && mv $OUT/$1 $OUT/$1.interrupted.$(date +%s); [ -e $OUT/$1.txt ] && mv $OUT/$1.txt $OUT/$1.txt.interrupted.$(date +%s); true; }
lt() { # lt <label> <dir> <arch>
  done_l $1 && return 0; interrupted $1
  (cd $2 && env -u CARGO_TARGET_DIR LOGDIR=$OUT/$1 timeout 2400 scripts/qemu-futex-time-witness-smoke.sh $3 2) > $OUT/$1.txt 2>&1
  local rc=$?
  echo "$1 $(stamp $2) rc=$rc $(grep -a 'FUTEX_TIME_WITNESS_SEAL' $OUT/$1.txt | tail -1)" >> $OUT/summary.txt
  grep -a '\[futex-time-witness\]\[fail\]' $OUT/$1.txt | sed -E 's/call [0-9]+/call N/; s/gen [0-9]+/gen N/' | sort | uniq -c | sort -rn | head -6 | sed 's/^/    /' >> $OUT/summary.txt
}
echo "live start $(date -u +%FT%TZ) candidate=$F $(stamp $WT)" >> $OUT/summary.txt
for n in 1 2 3; do
  for a in riscv64 aarch64 x86_64; do lt LT-$a-$n $WT $a; done
done
for c in "nc2 riscv64" "nc28 x86_64"; do
  set -- $c
  done_l L-$1 && continue
  git -C $LWT checkout -q -f $F && git -C $LWT clean -qfd
  python3 -I $S/ft1_nc.py $LWT $1 > $OUT/L-$1.patch.txt 2>&1; git -C $LWT diff >> $OUT/L-$1.patch.txt
  git -C $LWT -c user.name=nc -c user.email=nc@localhost commit -qam "$1 (live control, never pushed)"
  lt L-$1 $LWT $2
done
git -C $LWT checkout -q -f $F; rm -rf $LWT/build-*
for a in riscv64 aarch64 x86_64; do
  done_l LW-$a && continue; interrupted LW-$a
  (cd $WT && env -u CARGO_TARGET_DIR LOGDIR=$OUT/LW-$a timeout 2400 scripts/qemu-futex-wait-witness-smoke.sh $a 2) > $OUT/LW-$a.txt 2>&1
  rc=$?; echo "LW-$a $(stamp $WT) rc=$rc $(grep -a 'FUTEX_WAIT_WITNESS_SEAL' $OUT/LW-$a.txt | tail -1)" >> $OUT/summary.txt
done
done_l LE || { interrupted LE; (cd $WT && env -u CARGO_TARGET_DIR LOGDIR=$OUT/LE timeout 2400 scripts/qemu-riscv-ecall-witness-smoke.sh 2) > $OUT/LE.txt 2>&1
rc=$?; echo "LE $(stamp $WT) rc=$rc $(grep -a 'RISCV_ECALL_WITNESS_SEAL' $OUT/LE.txt | tail -1)" >> $OUT/summary.txt; }
for a in x86_64 aarch64 riscv64; do
  done_l LS-$a && continue; interrupted LS-$a
  (cd $WT && env -u CARGO_TARGET_DIR ARCH=$a LOGDIR=$OUT/LS-$a timeout 2400 scripts/qemu-irq-saturation-witness-smoke.sh) > $OUT/LS-$a.txt 2>&1
  rc=$?; echo "LS-$a $(stamp $WT) rc=$rc $(grep -aE 'SEAL|result=' $OUT/LS-$a.txt | tail -1 | cut -c1-200)" >> $OUT/summary.txt
done
echo "live end $(date -u +%FT%TZ)" >> $OUT/summary.txt
echo LIVE_DONE >> $OUT/summary.txt
