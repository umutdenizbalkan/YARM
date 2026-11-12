#!/usr/bin/env bash
# Addendum C: fail-closed per-section identity via regular-file extraction.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/rpr-evidence/qual; SUMMARY=$OUT/summary.txt; X=$OUT/XC; mkdir -p $X/sec
WT=$S/rpr-wt; BWT=$S/fwa-wt
echo "addendum C start $(date -u +%FT%TZ) schedule_sha=$(sha256sum $S/rpr_schedule.md | cut -c1-16)" >> $SUMMARY
for side in F B; do wt=$WT; [ $side = B ] && wt=$BWT
  echo "side $side $(git -C $wt rev-parse --short=12 HEAD) porcelain=$(git -C $wt status --porcelain | wc -l)" >> $X/provenance.txt
  cp $wt/build-aarch64/yarm-aarch64.elf $X/aarch64_$side.elf; cp $wt/build-x86_64/kernel_boot.elf $X/x86_64_$side.elf
  sha256sum $X/aarch64_$side.elf $X/x86_64_$side.elf >> $X/provenance.txt
  (cd $wt && BOOTSTRAP_FEATURE_ARGS="--no-default-features --features futex-requeue-witness" scripts/build-qemu-x86_64-artifacts.sh > $X/build_rqw_base_$side.log 2>&1
   echo "rqw base $side rc=$?" >> $X/provenance.txt
   source scripts/lib/artifact-binding.sh
   artifact_build_kernel x86_64 "--no-default-features --features x86-shared-region-direct-oracle" build-x86_64/kernel_boot.elf > $X/build_rqw_kernel_$side.log 2>&1
   echo "rqw kernel $side rc=$?" >> $X/provenance.txt)
  cp $wt/build-x86_64/kernel_boot.elf $X/x86rqw_$side.elf; sha256sum $X/x86rqw_$side.elf >> $X/provenance.txt
done
bad=0
{ for a in aarch64 x86_64 x86rqw; do
  llvm-readelf -SW $X/${a}_F.elf | sed -nE 's/^ *\[ *[0-9]+\] +(\.[^ ]+) +([A-Z_]+) +[0-9a-f]+ +[0-9a-f]+ +([0-9a-f]+).*/\1 \2 \3/p' | grep -v '^\.debug' |
  while read s typ hsz; do
    r=same; line=""
    for side in F B; do
      o=$X/sec/${a}_${side}_${s#.}; rm -f "$o"
      llvm-objcopy -O binary --only-section=$s $X/${a}_$side.elf "$o" 2>>$X/objcopy.err
      sz=$(stat -c %s "$o" 2>/dev/null || echo missing)
      h=$(sha256sum "$o" 2>/dev/null | cut -c1-16)
      line="$line $side=$h/$sz"
      eval "h_$side=\$h"
      if [ "$typ" != NOBITS ] && [ "$sz" != "$((16#$hsz))" ]; then r=EXTRACTION_SIZE_MISMATCH; fi
    done
    [ "$r" = same ] && [ "$h_F" != "$h_B" ] && r=DIFF
    echo "SECTION $a $s $typ hdr=$((16#$hsz)) $r$line"
  done; done; } > $OUT/XC_text_identity.log 2>&1
grep -E '^SECTION \S+ \.text' $OUT/XC_text_identity.log | grep -qv ' same ' && bad=1
[ $(grep -cE '^SECTION \S+ \.text' $OUT/XC_text_identity.log) -ge 6 ] || bad=1
[ $bad = 0 ] && echo TEXT_IDENTICAL_AARCH64_X86_64_X86RQW_FAILCLOSED >> $OUT/XC_text_identity.log
echo "XC_text_identity rc=$bad secs=0" >> $SUMMARY
rm -f $X/*.elf; rm -rf $X/sec
touch $S/rpr_addC.done
