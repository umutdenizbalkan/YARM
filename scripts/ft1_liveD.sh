#!/usr/bin/env bash
# FUTEX-TIME1 Addendum D (ft1_schedule.md): LS-<isa>-D at F'' + kernel identity vs LS-<isa> at F.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/ft1-evidence/live; WT=$S/ft1-wt
FPP=$(cat $S/ft1_fprime); [ "$(git -C $WT rev-parse HEAD)" = "$(git -C $WT rev-parse $FPP)" ] || { echo "ft1-wt is not at F''"; exit 2; }
stamp() { echo "[$(git -C $1 rev-parse --short=8 HEAD) porcelain=$(git -C $1 status --porcelain | wc -l)]"; }
done_l() { grep -q "^$1 \[" $OUT/summary.txt; }
interrupted() { [ -e $OUT/$1 ] && mv $OUT/$1 $OUT/$1.interrupted.$(date +%s); [ -e $OUT/$1.txt ] && mv $OUT/$1.txt $OUT/$1.txt.interrupted.$(date +%s); true; }
ksha() { grep -a -m1 'ARTIFACT_BINDING' $1 | grep -oE 'kernel_sha256=[0-9a-f]+' | cut -d= -f2; }
grep -q '^liveD start' $OUT/summary.txt || echo "liveD start $(date -u +%FT%TZ) candidate=$FPP $(stamp $WT)" >> $OUT/summary.txt
for a in x86_64 aarch64 riscv64; do
  L=LS-$a-D; done_l $L && continue; interrupted $L
  (cd $WT && env -u CARGO_TARGET_DIR ARCH=$a LOGDIR=$OUT/$L timeout 2400 scripts/qemu-irq-saturation-witness-smoke.sh) > $OUT/$L.txt 2>&1
  rc=$?; r=$(ksha $OUT/LS-$a.txt); g=$(ksha $OUT/$L.txt)
  [ -n "$r" ] && [ "$r" = "$g" ] && v=identical || v=DIFFERENT
  echo "$L $(stamp $WT) rc=$rc kernel $v vs LS-$a ($g) $(grep -aE 'SEAL|result=' $OUT/$L.txt | tail -1 | cut -c1-200)" >> $OUT/summary.txt
done
echo "liveD end $(date -u +%FT%TZ)" >> $OUT/summary.txt; echo LIVED_DONE >> $OUT/summary.txt
