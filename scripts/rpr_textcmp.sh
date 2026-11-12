#!/usr/bin/env bash
# X: default x86_64 and AArch64 kernels built at F and at B, compared section by section.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=${OUT:-$S/rpr-evidence/qual}/X; mkdir -p $OUT
bad=0
for side in F B; do
  wt=$S/rpr-wt; [ $side = B ] && wt=$S/fwa-wt
  (cd $wt && scripts/build-qemu-x86_64-artifacts.sh > $OUT/build_x86_64_$side.log 2>&1 && cp build-x86_64/kernel_boot.elf $OUT/x86_64_$side.elf)
  (cd $wt && scripts/build-qemu-aarch64-artifacts.sh > $OUT/build_aarch64_$side.log 2>&1 && cp build-aarch64/yarm-aarch64.elf $OUT/aarch64_$side.elf)
done
for a in x86_64 aarch64; do
  for s in $(readelf -SW $OUT/${a}_F.elf | sed -nE 's/^ *\[ *[0-9]+\] +(\.[^ ]+).*/\1/p' | grep -v '^\.debug'); do
    f=$(objcopy -O binary --only-section=$s $OUT/${a}_F.elf /dev/stdout 2>/dev/null | sha256sum | cut -c1-16)
    b=$(objcopy -O binary --only-section=$s $OUT/${a}_B.elf /dev/stdout 2>/dev/null | sha256sum | cut -c1-16)
    [ "$f" = "$b" ] && r=same || r=DIFF
    echo "SECTION $a $s $r F=$f B=$b"
    [ "$s" = ".text" ] && [ $r = DIFF ] && bad=1
  done
done
rm -f $OUT/*.elf
[ $bad = 0 ] && echo TEXT_IDENTICAL_X86_64_AARCH64
exit $bad
