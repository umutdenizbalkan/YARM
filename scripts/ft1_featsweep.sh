#!/usr/bin/env bash
# FUTEX-TIME1 Addendum C: check-compile every kernel feature (kernel_boot) per ISA at a revision.
# usage: ft1_featsweep.sh <rev> <label>   (detached throwaway worktree, private target dir)
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
REV=$1; LBL=$2; W=$S/ft1fs-$LBL-wt; OUT=$S/ft1-evidence/featsweep-$LBL; mkdir -p $OUT
[ -d $W ] || git -C $S/ft1-wt worktree add -q --detach $W $REV
cd $W; touch $OUT/summary.txt; echo "rev=$(git rev-parse HEAD) porcelain=$(git status --porcelain|wc -l) start=$(date -u +%FT%TZ)" >> $OUT/summary.txt
TD=$S/ft1fs-target-$LBL
feats=$(sed -n '/^\[features\]/,/^\[/p' Cargo.toml | grep -oE '^[a-z0-9-]+ *=' | tr -d ' =' | grep -vE '^(default|hosted-dev|legacy-tests|rpi5-stage1|rpi5-highhalf)$')
tgt() { case $1 in riscv64) echo riscv64gc-unknown-none-elf;; aarch64) echo targets/aarch64-yarm-none.json;; x86_64) echo targets/x86_64-yarm-none.json;; esac; }
for f in $feats; do
  case $f in x86*) isas=x86_64;; aarch64*) isas=aarch64;; riscv*) isas=riscv64;; *) isas="riscv64 aarch64 x86_64";; esac
  for a in $isas; do
    grep -q "^$f $a rc=" $OUT/summary.txt && continue
    CARGO_TARGET_DIR=$TD timeout 900 cargo +nightly check -q -Z build-std=core,alloc,compiler_builtins,panic_abort -Z json-target-spec \
      --target $(tgt $a) --no-default-features --features $f -p yarm --bin kernel_boot > $OUT/$f.$a.log 2>&1
    rc=$?; echo "$f $a rc=$rc $(grep -c '^error' $OUT/$f.$a.log)" >> $OUT/summary.txt
    rm -rf $TD/*/debug/incremental/yarm-* ; find $TD -path '*/debug/deps/*' \( -name 'libyarm-*' -o -name 'yarm-*' -o -name 'kernel_boot-*' \) -delete 2>/dev/null; rm -rf $TD/*/debug/.fingerprint/yarm-* 
  done
done
echo SWEEP_DONE >> $OUT/summary.txt
