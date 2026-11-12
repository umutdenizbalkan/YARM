#!/usr/bin/env bash
# Schedule Addendum A: F' = F + W' (witness-only). XW, hosted at F', LF' x3, L-NC1'/L-NC11', LB'.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=3091f143 FP=83be1921 BW=1a570728
OUT=$S/ea7-evidence/addA; mkdir -p $OUT/X; SUM=$OUT/summary.txt
WF=$S/ea7-wt WP=$S/ea7fix-wt LWT=$S/ea7live-wt
echo "addendum A start $(date -u +%FT%TZ) schedule_sha=$(sha256sum $S/ea7_schedule.md | cut -c1-16) F=$(git -C $WF rev-parse --short=8 HEAD) F'=$(git -C $WP rev-parse --short=8 HEAD)" > $SUM
# XW — default artifacts at F and F'.
for side in F FP; do wt=$WF; [ $side = FP ] && wt=$WP
  for a in x86_64 aarch64 riscv64; do (cd $wt && scripts/build-qemu-$a-artifacts.sh > $OUT/X/build_${a}_$side.log 2>&1); echo "build $a $side rc=$?" >> $SUM; done
  cp $wt/build-x86_64/kernel_boot.elf $OUT/X/x86_64_$side.elf; cp $wt/build-aarch64/yarm-aarch64.elf $OUT/X/aarch64_$side.elf; cp $wt/build-riscv64/yarm-riscv64.elf $OUT/X/riscv64_$side.elf
  for a in x86_64 aarch64 riscv64; do cp $wt/build-$a/initramfs-core.cpio $OUT/X/initramfs_${a}_$side.cpio; done
done
xbad=0
for a in x86_64 aarch64 riscv64; do
  for s in $(llvm-readelf -SW $OUT/X/${a}_F.elf | sed -nE 's/^ *\[ *[0-9]+\] +(\.[^ ]+) +(PROGBITS|NOTE) .*/\1/p' | grep -v '^\.debug'); do
    hf=$(llvm-objcopy -O binary --only-section=$s $OUT/X/${a}_F.elf $OUT/X/s1 2>/dev/null; sha256sum $OUT/X/s1 | cut -c1-16)
    hp=$(llvm-objcopy -O binary --only-section=$s $OUT/X/${a}_FP.elf $OUT/X/s2 2>/dev/null; sha256sum $OUT/X/s2 | cut -c1-16)
    [ -s $OUT/X/s1 ] || hf=EMPTY; r=same; [ "$hf" = "$hp" ] || { r=DIFF; xbad=1; }; echo "XW kernel $a $s F=$hf F'=$hp $r" >> $SUM
  done
  c1=$(sha256sum $OUT/X/initramfs_${a}_F.cpio | cut -c1-16); c2=$(sha256sum $OUT/X/initramfs_${a}_FP.cpio | cut -c1-16)
  r=same; [ "$c1" = "$c2" ] || r=DIFF; echo "XW initramfs $a F=$c1 F'=$c2 $r" >> $SUM
done
rm -f $OUT/X/*.elf $OUT/X/s1 $OUT/X/s2
echo "XW kernels_identical=$([ $xbad = 0 ] && echo yes || echo NO)" >> $SUM
# Hosted at F'.
(cd $WP && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/ea7nc-target cargo test --lib -- --test-threads=1) > $OUT/hosted_lib.log 2>&1; echo "hosted_lib rc=$? $(grep -a 'test result' $OUT/hosted_lib.log | tail -1)" >> $SUM
(cd $WP && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/ea7nc-target cargo test --test '*' --no-fail-fast -- --test-threads=1) > $OUT/integration.log 2>&1; echo "integration rc=$? $(grep -a 'test result' $OUT/integration.log | awk '{p+=$4; f+=$6} END {print "passed",p,"failed",f,NR,"binaries"}')" >> $SUM
(cd $WP && python3 -I scripts/grade-riscv-ecall-witness.py --self-test) > $OUT/grader_self_test.log 2>&1; echo "grader_self_test rc=$?" >> $SUM
ew() { (cd $2 && LOGDIR=$OUT/$1 timeout 2400 scripts/qemu-riscv-ecall-witness-smoke.sh 2) > $OUT/$1.txt 2>&1; local rc=$?
  echo "$1 [$(git -C $2 rev-parse --short=8 HEAD) porcelain=$(git -C $2 status --porcelain | wc -l)] rc=$rc $(grep -a 'RISCV_ECALL_WITNESS_SEAL' $OUT/$1.txt | tail -1)" >> $SUM
  grep -a '\[riscv-ecall-grade\]\[fail\]' $OUT/$1.txt | sed -E 's/cycle [0-9]+/cycle N/; s/0x[0-9a-f]+/X/g' | sort | uniq -c | sort -rn | head -8 | sed 's/^/    /' >> $SUM; }
for n in 1 2 3; do ew LFp$n $WP; done
for c in nc1 nc11; do
  git -C $LWT checkout -q -f $FP && git -C $LWT clean -qfd
  python3 -I $S/ea7_nc.py $LWT $c > $OUT/L-${c}p.patch.txt 2>&1; git -C $LWT diff >> $OUT/L-${c}p.patch.txt
  git -C $LWT -c user.name=nc -c user.email=nc@localhost commit -qam "$c (live control, never pushed)"
  ew L-${c}p $LWT
done
git -C $LWT checkout -q -f $BW && git -C $LWT clean -qfd
git -C $LWT -c user.name=nc -c user.email=nc@localhost cherry-pick $FP > $OUT/LBp.cherry.txt 2>&1; echo "LBp cherry-pick rc=$? onto $BW" >> $SUM
git -C $LWT diff --stat $BW HEAD >> $OUT/LBp.cherry.txt
ew LBp1 $LWT
git -C $LWT checkout -q -f $FP; rm -rf $LWT/build-*
echo ADDA_DONE >> $SUM
touch $S/ea7_addA.done
