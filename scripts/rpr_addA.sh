#!/usr/bin/env bash
# RISCV-PARK-RET1 schedule Addendum A: appended FX7 x86_64 reruns at F and B, and X7 (.text identity
# of the futex-requeue-witness x86_64 kernel, F vs B). Nothing in qual/ is replaced.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/rpr-evidence/qual; SUMMARY=$OUT/summary.txt; BIND=$OUT/artifact_bindings.txt
WT=$S/rpr-wt; BWT=$S/fwa-wt
echo "addendum A start $(date -u +%FT%TZ) schedule_sha=$(sha256sum $S/rpr_schedule.md | cut -c1-16)" >> $SUMMARY
step() { local id=$1 dir=$2; shift 2; local t0; t0=$(date +%s)
  echo "=== $id [$(git -C "$dir" rev-parse --short=12 HEAD) porcelain=$(git -C "$dir" status --porcelain | wc -l)]: $* (start $(date -u +%T))" >> "$SUMMARY"
  ( cd "$dir" && "$@" ) > "$OUT/$id.log" 2>&1; local rc=$?
  echo "$id rc=$rc secs=$(( $(date +%s) - t0 ))" >> "$SUMMARY"
  grep -ah 'ARTIFACT_BINDING' "$OUT/$id.log" 2>/dev/null | sed "s/^/$id /" >> "$BIND" || true; }
mkdir -p $OUT/X7
for i in 1 2; do
  step FX7r${i}_requeue_x86_64_F "$WT" env LOGDIR="$OUT/FX7r${i}_x86_64" timeout 3000 scripts/qemu-futex-requeue-witness-smoke.sh x86_64 2
done
cp $WT/build-x86_64/kernel_boot.elf $OUT/X7/F.elf
for i in 1 2; do
  step FX7B${i}_requeue_x86_64_B "$BWT" env LOGDIR="$OUT/FX7B${i}_x86_64" timeout 3000 scripts/qemu-futex-requeue-witness-smoke.sh x86_64 2
done
cp $BWT/build-x86_64/kernel_boot.elf $OUT/X7/B.elf
{
  bad=0
  for s in $(readelf -SW $OUT/X7/F.elf | sed -nE 's/^ *\[ *[0-9]+\] +(\.[^ ]+).*/\1/p' | grep -v '^\.debug'); do
    f=$(objcopy -O binary --only-section=$s $OUT/X7/F.elf /dev/stdout 2>/dev/null | sha256sum | cut -c1-16)
    b=$(objcopy -O binary --only-section=$s $OUT/X7/B.elf /dev/stdout 2>/dev/null | sha256sum | cut -c1-16)
    [ "$f" = "$b" ] && r=same || r=DIFF; echo "SECTION x86_64-requeue-witness $s $r F=$f B=$b"
    [ "$s" = ".text" ] && [ $r = DIFF ] && bad=1
  done
  [ $bad = 0 ] && echo TEXT_IDENTICAL_X86_64_REQUEUE_WITNESS
} > $OUT/X7_text_identity.log 2>&1
echo "X7_text_identity rc=$(grep -q TEXT_IDENTICAL $OUT/X7_text_identity.log; echo $?) secs=0" >> $SUMMARY
rm -f $OUT/X7/*.elf
touch $S/rpr_addA.done
