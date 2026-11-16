#!/usr/bin/env bash
# PRINTK-FOOTPRINT1 ring-size measurement: default and strict SMP core smokes at the tree's
# PRB_SLOTS; records smoke rc and the PRINTK_RECORDS_LOST count per run. Usage: <wt> <outdir>
set -u
W=$1; OUT=$2; mkdir -p $OUT; touch $OUT/summary.txt
cd $W
echo "--- $(date -u +%FT%TZ) head=$(git rev-parse --short=12 HEAD) porcelain=$(git status --porcelain --untracked-files=no | wc -l) slots=$(grep -E '^pub const PRB_SLOTS_DEFAULT' src/kernel/printk.rs)" >> $OUT/summary.txt
run() { local L=$1; shift; grep -q "^$L " $OUT/summary.txt && return 0
  ( env -u CARGO_TARGET_DIR "$@" ) > $OUT/$L.log 2>&1; local rc=$?
  local lost=$(grep -ac PRINTK_RECORDS_LOST $OUT/$L.log)
  echo "$L rc=$rc printk_lost=$lost" >> $OUT/summary.txt; }
for a in x86_64 aarch64 riscv64; do run build_default_$a scripts/build-qemu-$a-artifacts.sh; done
run x86_smp1 env QEMU_SMP=1 scripts/qemu-x86_64-core-smoke.sh
run x86_smp2 env QEMU_SMP=2 scripts/qemu-x86_64-core-smoke.sh
run x86_smp4 env QEMU_SMP=4 scripts/qemu-x86_64-core-smoke.sh
run a64_smp2 scripts/qemu-aarch64-core-smoke.sh
run rv_smp1 scripts/qemu-riscv64-core-smoke.sh
run rv_smp2 scripts/qemu-riscv64-core-smoke.sh --smp=2
run rv_smp4 scripts/qemu-riscv64-core-smoke.sh --smp=4
run build_strict_x86_64 env ARTIFACTS_STRICT=1 BOOTSTRAP_FEATURE_ARGS="--no-default-features --features timer-contract-witness" scripts/build-qemu-x86_64-artifacts.sh
run x86_strict env QEMU_SMOKE_STRICT=1 scripts/qemu-x86_64-core-smoke.sh
run build_strict_aarch64 env ARTIFACTS_STRICT=1 scripts/build-qemu-aarch64-artifacts.sh
run a64_strict env QEMU_SMOKE_STRICT=1 scripts/qemu-aarch64-core-smoke.sh
run build_strict_riscv64 env ARTIFACTS_STRICT=1 scripts/build-qemu-riscv64-artifacts.sh
run rv_strict env QEMU_SMOKE_STRICT=1 scripts/qemu-riscv64-core-smoke.sh
run rv_strict_smp2 env QEMU_SMOKE_STRICT=1 scripts/qemu-riscv64-core-smoke.sh --smp=2
echo DONE >> $OUT/summary.txt
