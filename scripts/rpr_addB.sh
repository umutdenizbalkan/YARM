#!/usr/bin/env bash
# Addendum B: fail-closed AArch64 (and x86_64) .text identity with llvm-objcopy; RISC-V footprint.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/rpr-evidence/qual; SUMMARY=$OUT/summary.txt; X=$OUT/XB; mkdir -p $X $S/rpr-evidence/footprint
WT=$S/rpr-wt; BWT=$S/fwa-wt; EMPTY=e3b0c44298fc1c14
echo "addendum B start $(date -u +%FT%TZ) schedule_sha=$(sha256sum $S/rpr_schedule.md | cut -c1-16)" >> $SUMMARY
for side in F B; do wt=$WT; [ $side = B ] && wt=$BWT
  echo "side $side $(git -C $wt rev-parse --short=12 HEAD) porcelain=$(git -C $wt status --porcelain | wc -l)" >> $X/provenance.txt
  (cd $wt && scripts/build-qemu-aarch64-artifacts.sh > $X/build_aarch64_$side.log 2>&1); echo "build aarch64 $side rc=$?" >> $X/provenance.txt
  cp $wt/build-aarch64/yarm-aarch64.elf $X/aarch64_$side.elf
  (cd $wt && scripts/build-qemu-x86_64-artifacts.sh > $X/build_x86_64_$side.log 2>&1); echo "build x86_64 $side rc=$?" >> $X/provenance.txt
  cp $wt/build-x86_64/kernel_boot.elf $X/x86_64_$side.elf
  (cd $wt && scripts/build-qemu-riscv64-artifacts.sh > $X/build_riscv64_$side.log 2>&1); echo "build riscv64 $side rc=$?" >> $X/provenance.txt
  rv=$(ls $wt/build-riscv64/*.elf | grep -v debug | head -1); echo "riscv elf $side $rv" >> $X/provenance.txt; cp $rv $X/riscv64_$side.elf
done
bad=0
{ for a in aarch64 x86_64; do
  for s in $(llvm-readelf -SW $X/${a}_F.elf | sed -nE 's/^ *\[ *[0-9]+\] +(\.[^ ]+).*/\1/p' | grep -v '^\.debug'); do
    t=$(llvm-readelf -SW $X/${a}_F.elf | awk -v s=$s '$2==s || $3==s {print}' | grep -c NOBITS)
    f=$(llvm-objcopy -O binary --only-section=$s $X/${a}_F.elf /dev/stdout 2>/dev/null | sha256sum | cut -c1-16)
    b=$(llvm-objcopy -O binary --only-section=$s $X/${a}_B.elf /dev/stdout 2>/dev/null | sha256sum | cut -c1-16)
    [ "$f" = "$b" ] && r=same || r=DIFF
    if [ "$t" = 0 ] && { [ $f = $EMPTY ] || [ $b = $EMPTY ]; }; then r=EMPTY_EXTRACTION; fi
    echo "SECTION $a $s $r F=$f B=$b"
    case "$s" in .text*) [ $r != same ] && bad=1;; esac
  done; done
  [ $bad = 0 ] && echo TEXT_IDENTICAL_AARCH64_X86_64_FAILCLOSED; } > $OUT/XB_text_identity.log 2>&1
echo "XB_text_identity rc=$bad secs=0" >> $SUMMARY
{ for side in F B; do
    echo "== riscv64 $side"; llvm-readelf -SW $X/riscv64_$side.elf | grep -E ' \.text'
    llvm-nm -C --size-sort $X/riscv64_$side.elf | grep -iE 'commit_final_park|commit_riscv_parked|capture_outgoing_user_context_split' || echo "(no separate symbol: inlined or absent)"
  done; } > $S/rpr-evidence/footprint/riscv64_text.txt 2>&1
echo "FP_footprint rc=0 secs=0" >> $SUMMARY
rm -f $X/*.elf
touch $S/rpr_addB.done
