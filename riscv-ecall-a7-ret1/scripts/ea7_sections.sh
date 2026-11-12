#!/usr/bin/env bash
# X / FP: the three default kernels at F and at B — every allocated section's size and sha256
# (extracted to a regular file and checked against its header size: fail-closed), and the RISC-V
# write-back's instruction counts. Reported, not graded.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=${OUT:-$S/ea7-evidence/qual}/X; mkdir -p $OUT/sec
bad=0
for side in F B; do wt=$S/ea7-wt; [ $side = B ] && wt=$S/rpr-wt
  echo "side $side $(git -C $wt rev-parse --short=12 HEAD) porcelain=$(git -C $wt status --porcelain | wc -l)" >> $OUT/provenance.txt
  for a in x86_64 aarch64 riscv64; do
    (cd $wt && scripts/build-qemu-$a-artifacts.sh > $OUT/build_${a}_$side.log 2>&1); echo "build $a $side rc=$?" >> $OUT/provenance.txt
  done
  cp $wt/build-x86_64/kernel_boot.elf $OUT/x86_64_$side.elf
  cp $wt/build-aarch64/yarm-aarch64.elf $OUT/aarch64_$side.elf
  cp $wt/build-riscv64/yarm-riscv64.elf $OUT/riscv64_$side.elf
  sha256sum $OUT/*_$side.elf >> $OUT/provenance.txt
done
for a in x86_64 aarch64 riscv64; do
  llvm-readelf -SW $OUT/${a}_F.elf | sed -nE 's/^ *\[ *[0-9]+\] +(\.[^ ]+) +([A-Z_]+) +[0-9a-f]+ +[0-9a-f]+ +([0-9a-f]+) +[0-9a-f]+ +([A-Z]*).*/\1 \2 \3 \4/p' | grep -v '^\.debug' |
  while read s typ hsz flg; do
    case "$flg" in *A*) ;; *) continue;; esac
    line=""; hf=""; hb=""
    for side in F B; do
      o=$OUT/sec/${a}_${side}_${s#.}; rm -f "$o"
      bsz=$(llvm-readelf -SW $OUT/${a}_$side.elf | awk -v s=$s '{for(i=1;i<=NF;i++) if($i==s){print $(i+4); exit}}')
      if [ "$typ" = NOBITS ]; then h=nobits; sz=$((16#${bsz:-0}));
      else llvm-objcopy -O binary --only-section=$s $OUT/${a}_$side.elf "$o" 2>/dev/null; sz=$(stat -c %s "$o" 2>/dev/null || echo missing); h=$(sha256sum "$o" 2>/dev/null | cut -c1-16)
           [ "$sz" = "$((16#${bsz:-0}))" ] || h=EXTRACTION_SIZE_MISMATCH; fi
      line="$line $side=$h/$sz"; [ $side = F ] && hf=$h || hb=$h
    done
    r=same; [ "$hf" = "$hb" ] || r=DIFF
    echo "SECTION $a $s $typ$line $r"
  done
done > $S/ea7-evidence/qual/X_sections.log 2>&1
grep -q EXTRACTION_SIZE_MISMATCH $S/ea7-evidence/qual/X_sections.log && bad=1
{ for side in F B; do
    echo "== riscv64 $side: write-back instruction counts"
    llvm-objdump -d --no-show-raw-insn $OUT/riscv64_$side.elf > $OUT/riscv64_$side.dis
    awk '/<yarm_riscv64_trap_bridge>:/{f=1;n=0;next} f&&/^[0-9a-f]+ </{f=0} f&&/^ +[0-9a-f]+:/{n++} END{print "yarm_riscv64_trap_bridge instructions:", n}' $OUT/riscv64_$side.dis
    llvm-nm --size-sort -C $OUT/riscv64_$side.elf | grep -E ' yarm_riscv64_trap_bridge$| yarm_riscv64_s_mode_trap|riscv64_s_mode_idle' | head -4
    rm -f $OUT/riscv64_$side.dis
  done
  for a in x86_64 aarch64 riscv64; do for side in F B; do
    echo "== $a $side sizes"; llvm-size -A $OUT/${a}_$side.elf | grep -E '^\.(text|rodata|data|bss)\b' ; done; done
} > $S/ea7-evidence/qual/FP_footprint.log 2>&1
rm -rf $OUT/sec $OUT/*.elf
exit $bad
