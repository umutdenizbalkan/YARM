#!/usr/bin/env bash
# Addendum C / XW2: symmetric F vs F' default-artifact comparison.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
O=$S/ea7-evidence/addC/XW2; SUM=$O/summary.txt; : > $SUM
WF=$S/ea7-wt WP=$S/ea7fix-wt
for side in F FP; do wt=$WF; [ $side = FP ] && wt=$WP
  echo "$side HEAD=$(git -C $wt rev-parse --short=12 HEAD) porcelain=$(git -C $wt status --porcelain | wc -l)" >> $SUM
  for a in x86_64 aarch64 riscv64; do (cd $wt && scripts/build-qemu-$a-artifacts.sh > $O/build_${a}_$side.log 2>&1); echo "build $a $side rc=$?" >> $SUM; done
  cp $wt/build-x86_64/kernel_boot.elf $O/x86_64_$side.elf; cp $wt/build-aarch64/yarm-aarch64.elf $O/aarch64_$side.elf; cp $wt/build-riscv64/yarm-riscv64.elf $O/riscv64_$side.elf
  for a in x86_64 aarch64 riscv64; do cp $wt/build-$a/initramfs-core.cpio $O/initramfs_${a}_$side.cpio; done
done
bad=0
h() { llvm-objcopy -O binary --only-section="$2" "$1" $O/sx 2>/dev/null || { echo ERR; return; }; echo "$(stat -c %s $O/sx):$(sha256sum $O/sx | cut -c1-16)"; }
for a in x86_64 aarch64 riscv64; do
  shf=$(llvm-readelf -SW $O/${a}_F.elf | sed -nE 's/^ *\[ *[0-9]+\] +//p' | grep -v '\.debug' | awk '{print $1,$2,$3,$5}'); shp=$(llvm-readelf -SW $O/${a}_FP.elf | sed -nE 's/^ *\[ *[0-9]+\] +//p' | grep -v '\.debug' | awk '{print $1,$2,$3,$5}')
  [ "$shf" = "$shp" ] && echo "XW2 $a section_headers(name,type,addr,size) same" >> $SUM || { echo "XW2 $a section_headers DIFF" >> $SUM; diff <(echo "$shf") <(echo "$shp") >> $SUM; bad=1; }
  for s in $(echo "$shf" | awk '$2=="PROGBITS"||$2=="NOTE"{print $1}'); do
    x=$(h $O/${a}_F.elf $s); y=$(h $O/${a}_FP.elf $s); r=same; [ "$x" = "$y" ] && [ "$x" != ERR ] || { r=DIFF; bad=1; }
    echo "XW2 kernel $a $s F=$x F'=$y $r" >> $SUM
  done
  c1=$(sha256sum $O/initramfs_${a}_F.cpio | cut -c1-16); c2=$(sha256sum $O/initramfs_${a}_FP.cpio | cut -c1-16); r=same; [ "$c1" = "$c2" ] || { r=DIFF; bad=1; }
  echo "XW2 initramfs $a F=$c1 F'=$c2 $r" >> $SUM
done
rm -f $O/*.elf $O/*.cpio $O/sx
echo "XW2 identical=$([ $bad = 0 ] && echo yes || echo NO)" >> $SUM
