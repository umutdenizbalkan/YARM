#!/usr/bin/env bash
# PRE-SCHEDULER-CLEANUP1 — the one targeted qualification of the frozen candidate F against
# B = 2866be83. Every step runs once; a step already in summary.txt is skipped (resumable); a
# step's log is never overwritten (an interrupted one is renamed). Baseline (B) steps run only
# where they resolve attribution: clippy classes, lock-order attribution, footprint.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=${F:?}
WT=$S/psc-q-wt BWT=$S/psc-base-wt
OUT=${OUT:-$S/psc-evidence/qual}
mkdir -p "$OUT"
SUMMARY=$OUT/summary.txt BIND=$OUT/bindings.txt

step() {  # step <id> <dir> <cmd...>
  local id=$1 dir=$2; shift 2
  grep -q "^$id rc=" "$SUMMARY" 2>/dev/null && return 0
  [ -e "$OUT/$id.log" ] && mv "$OUT/$id.log" "$OUT/$id.log.interrupted.$(date +%s)"
  local t0; t0=$(date +%s)
  echo "=== $id [$(git -C "$dir" rev-parse --short=12 HEAD) porcelain=$(git -C "$dir" status --porcelain --untracked-files=no | wc -l)]: $* (start $(date -u +%T))" >> "$SUMMARY"
  ( cd "$dir" && "$@" ) > "$OUT/$id.log" 2>&1
  local rc=$?
  echo "$id rc=$rc secs=$(( $(date +%s) - t0 ))" >> "$SUMMARY"
  grep -ah 'ARTIFACT_BINDING' "$OUT/$id.log" 2>/dev/null | sed "s/^/$id /" >> "$BIND" || true
  return 0
}
hosted() { CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/psc-q-target "$@"; }
std() { env -u CARGO_TARGET_DIR "$@"; }   # QEMU steps: the standard ./target path

ONLY=${ONLY:-all}
on() { [[ $ONLY == all || " $ONLY " == *" $1 "* ]]; }
echo "matrix start $(date -u +%FT%TZ) only=$ONLY runner_sha=$(sha256sum $0 | cut -c1-16)" >> "$SUMMARY"

if on prov; then
  step P0_provenance "$WT" bash -c "git rev-parse HEAD; test \$(git rev-parse --short=8 HEAD) = $F && echo HEAD_IS_F; git status --porcelain --untracked-files=no | wc -l; rustc -V; cargo -V; qemu-system-x86_64 --version | head -1; qemu-system-aarch64 --version | head -1; qemu-system-riscv64 --version | head -1"
  step P1_source "$WT" bash -c "git log --oneline 2866be83..HEAD; git diff --stat 2866be83 HEAD; git diff 2866be83 HEAD -- src/kernel/syscall.rs | grep -c '^[+-].*SYSCALL_' ; true"
fi
if on hosted; then
  step H1_fmt "$WT" cargo fmt --all -- --check
  step H2_clippy "$WT" hosted cargo clippy --lib --features hosted-dev
  step H2b_clippy_classes "$BWT" bash -c "OUT=$OUT $S/psc_clipcmp.sh"
  step H3_hosted_lib "$WT" hosted cargo test --lib -- --test-threads=1
  step H4_integration "$WT" hosted cargo test --test '*' --no-fail-fast -- --test-threads=1
  for f in sr_place1 stage199d stage198e3b2b shared_region sat_witness schedlock1 printk sched_full_rot1 sched_time_foundation1 sleep1 futex_requeue1 futex_wake_cap1 ipc_timeout census lock_order broad syscall_abi abi; do
    step "H5_$f" "$WT" hosted cargo test --lib "$f" -- --test-threads=1
  done
  step H6_lockorder_attr "$WT" bash -c "OUT=$OUT $S/psc_lockattr.sh"
  step G1_contract_gate "$WT" env CARGO_TARGET_DIR=$S/psc-q-target bash scripts/check-contract-doc-enforcement.sh
  step G2_grader_self_tests "$WT" bash -c "python3 -I scripts/grade-sched-time-witness.py --self-test && python3 -I scripts/grade-futex-time-witness.py --self-test && python3 -I scripts/grade-sched-lock1-witness.py --self-test && python3 -I scripts/grade-futex-wait-witness.py --self-test && python3 -I scripts/grade-futex-requeue-witness.py --self-test"
  step G3_artifact_cargo_binding "$WT" env -u CARGO_TARGET_DIR bash scripts/test-artifact-cargo-binding.sh
  step G4_zscan_self_test "$WT" bash -c "$S/fwa_zscan.sh --self-test"
fi
if on default; then
  step D1_build_x86_64 "$WT" std scripts/build-qemu-x86_64-artifacts.sh
  step D1s_core_x86_64_smp2 "$WT" std env QEMU_SMP=2 LOGFILE="$OUT/D1s.boot.log" scripts/qemu-x86_64-core-smoke.sh
  step D2_build_aarch64 "$WT" std scripts/build-qemu-aarch64-artifacts.sh
  step D2s_core_aarch64_smp2 "$WT" std env QEMU_SMP=2 LOGFILE="$OUT/D2s.boot.log" scripts/qemu-aarch64-core-smoke.sh
  step D3_build_riscv64 "$WT" std scripts/build-qemu-riscv64-artifacts.sh
  step D3s_core_riscv64_smp2 "$WT" std env QEMU_SMP=2 LOGFILE="$OUT/D3s.boot.log" scripts/qemu-riscv64-core-smoke.sh
  step X_footprint "$WT" bash -c "$S/psc_fp.sh $BWT $WT $OUT/X_footprint.txt && mkdir -p $OUT/X_elf && cp build-x86_64/kernel_boot.elf $OUT/X_elf/F_x86_64.elf && cp build-aarch64/yarm-aarch64.elf build-aarch64/yarm-aarch64.bin build-riscv64/yarm-riscv64.elf build-riscv64/yarm-riscv64.bin $OUT/X_elf/ && cp build-x86_64/kernel_boot.elf.provenance $OUT/X_elf/F_x86_64.elf.provenance && cp build-aarch64/yarm-aarch64.elf.provenance build-riscv64/yarm-riscv64.elf.provenance $OUT/X_elf/"
fi
if on strict; then
  step S1_build_x86_64_strict "$WT" std env ARTIFACTS_STRICT=1 BOOTSTRAP_FEATURE_ARGS="--no-default-features --features timer-contract-witness" scripts/build-qemu-x86_64-artifacts.sh
  step S2_core_x86_64_strict "$WT" std env QEMU_SMOKE_STRICT=1 LOGFILE="$OUT/S2.boot.log" scripts/qemu-x86_64-core-smoke.sh
  step S3_build_aarch64_strict "$WT" std env ARTIFACTS_STRICT=1 scripts/build-qemu-aarch64-artifacts.sh
  step S4_core_aarch64_strict "$WT" std env QEMU_SMOKE_STRICT=1 LOGFILE="$OUT/S4.boot.log" scripts/qemu-aarch64-core-smoke.sh
  step S5_build_riscv64_strict "$WT" std env ARTIFACTS_STRICT=1 scripts/build-qemu-riscv64-artifacts.sh
  step S6_core_riscv64_strict "$WT" std env QEMU_SMOKE_STRICT=1 LOGFILE="$OUT/S6.boot.log" scripts/qemu-riscv64-core-smoke.sh
fi
if on witness; then
  for a in x86_64 aarch64 riscv64; do
    step "W1_shared_region_${a}" "$WT" std env LOGDIR="$OUT/W1_${a}" timeout 2400 scripts/qemu-shared-region-direct-${a}-smoke.sh
    step "W2_schedlock1_${a}" "$WT" std env ARCH=$a SMP=2 LOGDIR="$OUT/W2_${a}" timeout 2400 scripts/qemu-sched-lock1-witness-smoke.sh
    step "W3_sched_time_${a}" "$WT" std env SMP=2 LOGDIR="$OUT/W3_${a}" timeout 3600 scripts/qemu-sched-time-witness-smoke.sh "$a"
    step "W4_futex_time_${a}" "$WT" std env LOGDIR="$OUT/W4_${a}" timeout 3600 scripts/qemu-futex-time-witness-smoke.sh "$a" 2
  done
fi
if on altdir; then
  # ARTIFACT-CARGO-BIND1 live: an alternate CARGO_TARGET_DIR while ./target holds the strict build
  # just published (stale for this configuration); the default build must publish and boot the
  # alternate directory's output. Runs after the witnesses, so ./target holds the last witness's
  # feature kernel (W4 riscv64): different bytes at the old path.
  step A1_build_riscv64_altdir "$WT" env CARGO_TARGET_DIR=$S/psc-q-alt-target scripts/build-qemu-riscv64-artifacts.sh
  step A2_core_riscv64_altdir "$WT" env CARGO_TARGET_DIR=$S/psc-q-alt-target LOGFILE="$OUT/A2.boot.log" scripts/qemu-riscv64-core-smoke.sh
  step A3_altdir_binding "$WT" bash -c "set -e; grep -a ARTIFACT_CARGO_OUTPUT $OUT/A1_build_riscv64_altdir.log | head -20; P='python3 -I scripts/artifact-provenance.py show'; \$P build-riscv64/yarm-riscv64.bin | grep -E '^(cargo_|elf_sha256|features|yarm_provenance)'; \$P build-riscv64/initramfs-core.cpio | grep '^cargo_inputs=' | tr ',' '\\n'; pub=\$(\$P build-riscv64/yarm-riscv64.bin | sed -n 's/^elf_sha256=//p'); alt=\$(sha256sum $S/psc-q-alt-target/riscv64gc-unknown-none-elf/release/kernel_boot | cut -d' ' -f1); old=\$(sha256sum target/riscv64gc-unknown-none-elf/release/kernel_boot | cut -d' ' -f1); echo published=\$pub alt=\$alt old_path=\$old; test \$pub = \$alt; test \$alt != \$old; \$P build-riscv64/yarm-riscv64.bin | grep -q '^cargo_executable=$S/psc-q-alt-target/'; echo PUBLISHED_IS_ALT_DIR_OUTPUT_NOT_OLD_PATH"
fi
step Z_markers "$WT" bash -c "$S/fwa_zscan.sh \$(find $OUT -name '*.log' ! -name 'H*' ! -name 'Z_*' ! -name 'P*' ! -name 'G*'); echo; echo PRINTK_RECORDS_LOST:; grep -arc 'PRINTK_RECORDS_LOST' $OUT --include='*.log' | grep -v ':0$' || echo none"
echo "matrix end $(date -u +%FT%TZ) F_porcelain=$(git -C "$WT" status --porcelain --untracked-files=no | wc -l)" >> "$SUMMARY"
echo MATRIX_DONE >> "$SUMMARY"
