#!/usr/bin/env bash
# FUTEX-TIME1 Addendum C live runs at F'' (ft1_schedule.md). Appended to live/summary.txt; resumable
# by label (Addendum A); no retries.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OUT=$S/ft1-evidence/live; WT=$S/ft1-wt
FPP=$(cat $S/ft1_fprime); [ "$(git -C $WT rev-parse HEAD)" = "$(git -C $WT rev-parse $FPP)" ] || { echo "ft1-wt is not at F''"; exit 2; }
stamp() { echo "[$(git -C $1 rev-parse --short=8 HEAD) porcelain=$(git -C $1 status --porcelain | wc -l)]"; }
done_l() { grep -q "^$1 \[" $OUT/summary.txt; }
interrupted() { [ -e $OUT/$1 ] && mv $OUT/$1 $OUT/$1.interrupted.$(date +%s); [ -e $OUT/$1.txt ] && mv $OUT/$1.txt $OUT/$1.txt.interrupted.$(date +%s); true; }
grep -q '^liveC start' $OUT/summary.txt || echo "liveC start $(date -u +%FT%TZ) candidate=$FPP $(stamp $WT)" >> $OUT/summary.txt
# (1) binary identity of the time-witness kernel + initramfs, F'' vs the LT-<isa>-1 run at F
for a in riscv64 aarch64 x86_64; do
  L=ID-$a; done_l $L && continue; interrupted $L; mkdir -p $OUT/$L
  case $a in x86_64) F0=x86-shared-region-direct-oracle; BS=scripts/build-qemu-x86_64-artifacts.sh; I=build-x86_64/initramfs-core.cpio; D=build-x86_64/kernel_boot.elf; OBJ=0;;
    aarch64) F0=aarch64-shared-region-direct-oracle; BS=scripts/build-qemu-aarch64-artifacts.sh; I=build-aarch64/initramfs-core.cpio; D=build-aarch64/yarm-aarch64.bin; OBJ=1;;
    riscv64) F0=riscv-shared-region-direct-oracle; BS=scripts/build-qemu-riscv64-artifacts.sh; I=build-riscv64/initramfs-core.cpio; D=build-riscv64/yarm-riscv64.bin; OBJ=1;; esac
  ( cd $WT && env -u CARGO_TARGET_DIR BOOTSTRAP_FEATURE_ARGS="--no-default-features --features futex-requeue-witness,futex-time-witness" $BS > $OUT/$L/build.log 2>&1 \
    && source scripts/lib/artifact-binding.sh \
    && if (( OBJ )); then artifact_build_kernel $a "--no-default-features --features $F0,futex-time-witness" "${D%.bin}.elf" "$D"; else artifact_build_kernel $a "--no-default-features --features $F0,futex-time-witness" "$D"; fi > $OUT/$L/kbuild.log 2>&1 \
    && sha256sum $D $I > $OUT/$L/sha256.txt ); rc=$?
  ref=$(grep -v boot.log $OUT/LT-$a-1/sha256.txt | awk '{print $1"  "$2}'); got=$(awk '{print $1"  "$2}' $OUT/$L/sha256.txt 2>/dev/null)
  [ $rc = 0 ] && [ "$ref" = "$got" ] && v=identical || v=DIFFERENT
  echo "$L $(stamp $WT) rc=$rc $v vs LT-$a-1" >> $OUT/summary.txt
done
lt() { done_l $1 && return 0; interrupted $1
  (cd $WT && env -u CARGO_TARGET_DIR LOGDIR=$OUT/$1 timeout 2400 scripts/qemu-futex-time-witness-smoke.sh $2 2) > $OUT/$1.txt 2>&1; local rc=$?
  echo "$1 $(stamp $WT) rc=$rc $(grep -a 'FUTEX_TIME_WITNESS_SEAL' $OUT/$1.txt | tail -1)" >> $OUT/summary.txt
  grep -a '\[futex-time-witness\]\[fail\]' $OUT/$1.txt | sed -E 's/call [0-9]+/call N/; s/gen [0-9]+/gen N/' | sort | uniq -c | sort -rn | head -6 | sed 's/^/    /' >> $OUT/summary.txt; }
for a in riscv64 aarch64 x86_64; do
  L=LW-$a-C; done_l $L && continue; interrupted $L
  (cd $WT && env -u CARGO_TARGET_DIR LOGDIR=$OUT/$L timeout 2400 scripts/qemu-futex-wait-witness-smoke.sh $a 2) > $OUT/$L.txt 2>&1
  rc=$?; echo "$L $(stamp $WT) rc=$rc $(grep -a 'FUTEX_WAIT_WITNESS_SEAL' $OUT/$L.txt | tail -1)" >> $OUT/summary.txt
done
done_l LE-C || { interrupted LE-C; (cd $WT && env -u CARGO_TARGET_DIR LOGDIR=$OUT/LE-C timeout 2400 scripts/qemu-riscv-ecall-witness-smoke.sh 2) > $OUT/LE-C.txt 2>&1
  rc=$?; echo "LE-C $(stamp $WT) rc=$rc $(grep -a 'RISCV_ECALL_WITNESS_SEAL' $OUT/LE-C.txt | tail -1)" >> $OUT/summary.txt; }
for n in 4 5 6; do lt LT-x86_64-$n x86_64; done
echo "liveC end $(date -u +%FT%TZ)" >> $OUT/summary.txt; echo LIVEC_DONE >> $OUT/summary.txt
