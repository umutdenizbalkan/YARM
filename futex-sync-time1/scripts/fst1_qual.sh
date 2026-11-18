#!/usr/bin/env bash
# FUTEX-SYNC-TIME1 focused qualification: candidate C vs baseline 253ca580 (B, the delivered
# FUTEX-ABS-BITSET1 head). Steps in order, every log kept, nothing rerun in place.
# Reused, not rerun: the hosted lock-order baseline is the FUTEX-ABS-BITSET1 candidate run
# (a8d7952d; 253ca580 differs from it only in doc/), and the kernel-level timed/bitset futex
# witness (pass 6 W1-W3), because this pass changes no kernel source outside cfg(test).
# Negative-control patches: $S/fst1-nc/nc.py (hashed with this runner and the grader).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C=${C:?candidate}
B=253ca580
O=$S/fst1-evidence/qual
W=$S/fst1-wt
BW=$S/fab1-wt
P6=$S/fab1-evidence/qual
SUM=$O/summary.txt
NC="python3 -I $S/fst1-nc/nc.py"
RT=crates/yarm-user-rt/src/lib.rs
mkdir -p $O/kernels $O/initramfs
step() {
  local name=$1; shift
  local por; por=$(git -C $W status --porcelain | wc -l)
  echo "=== $name [$(git -C $W rev-parse --short=12 HEAD) porcelain=$por]: $* (start $(date -u +%H:%M:%S))" >> $SUM
  local t0=$SECONDS
  ( cd $W && "$@" ) > $O/$name.log 2>&1
  local rc=$?
  echo "$name rc=$rc secs=$((SECONDS - t0))" >> $SUM
}
pairs() { awk '{ while (match($0, /YARM_LOCK_ORDER_WARN current=[a-z_]+ previous=[0-9]+/)) { w=substr($0, RSTART, RLENGTH); split(w, a, " "); c[a[2]" "a[3]]++; $0=substr($0, RSTART+RLENGTH) } } END { for (k in c) print k, c[k] }' | sort; }
kpath() { grep -a "ARTIFACT_CARGO_OUTPUT bin=kernel_boot " "$1" | tail -1 | sed 's/.* path=\([^ ]*\) .*/\1/'; }
kimg() { case $1 in x86_64) echo build-x86_64/kernel_boot.elf;; aarch64) echo build-aarch64/yarm-aarch64.bin;; riscv64) echo build-riscv64/yarm-riscv64.bin;; esac; }
echo "fst1 qual start $(date -u +%FT%TZ) C=$C B=$B sha=$(sha256sum $0 | cut -c1-16) nc=$(sha256sum $S/fst1-nc/nc.py | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $C
[[ "$(git -C $BW rev-parse --short=8 HEAD)" == "$B" ]] || { echo "baseline worktree not at $B" >> $SUM; exit 1; }
[[ "$(git -C $BW status --porcelain | wc -l)" == 0 ]] || { echo "baseline worktree not clean" >> $SUM; exit 1; }

# ── hosted ──────────────────────────────────────────────────────────────────────────────────
step H1_fmt cargo fmt --all -- --check
step H2_hosted_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
step H3_integration env -u CARGO_TARGET_DIR cargo test -q --tests --no-fail-fast -- --test-threads=1
for f in sync_time1 r30_the_contended_state futex_requeue1 abs_bitset1 time1 futex rt1b_ sched_budget rt1a_ stage45 syscall_abi lock_order; do
  step H4_subset_$f env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 $f
done
step H4_crate_yarm_user_rt env -u CARGO_TARGET_DIR cargo test -q -p yarm-user-rt --lib -- --test-threads=1
step H4_crate_yarm_user_rt_futex_sync env -u CARGO_TARGET_DIR cargo test -q -p yarm-user-rt --lib -- --test-threads=1 futex_sync_tests
step H4_crate_yarm_kernel env -u CARGO_TARGET_DIR cargo test -q -p yarm-kernel --lib -- --test-threads=1
step H4_crate_yarm_ipc_abi env -u CARGO_TARGET_DIR cargo test -q -p yarm-ipc-abi --lib -- --test-threads=1
step H4_grader_self_test_sync python3 -I scripts/grade-futex-sync-witness.py --self-test
step H4_grader_self_test_time python3 -I scripts/grade-futex-time-witness.py --self-test
# lock-order: descending sequential pair TYPES of the whole serial lib suite, C vs the reused
# FUTEX-ABS-BITSET1 candidate run (code-identical to B)
step H5_lockorder_C bash -c "env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 --nocapture > $O/H5_C.raw.log 2>&1; echo rc=\$?"
pairs < $O/H5_C.raw.log > $O/H5_C.pairs.txt; cp $P6/H5_C.pairs.txt $O/H5_B_reused_from_a8d7952d.pairs.txt
diff <(awk '{print $1, $2}' $O/H5_C.pairs.txt) <(awk '{print $1, $2}' $O/H5_B_reused_from_a8d7952d.pairs.txt) > $O/H5_pairtypes.diff; echo "H5_pairtypes C=$(wc -l < $O/H5_C.pairs.txt) B=$(wc -l < $O/H5_B_reused_from_a8d7952d.pairs.txt) diff_lines=$(wc -l < $O/H5_pairtypes.diff)" >> $SUM
gzip -f $O/H5_C.raw.log
step G1_contract_gate bash scripts/check-current-contracts.sh
step G2_doc_enforcement bash scripts/check-contract-doc-enforcement.sh
# cross-target checks: default kernel, witness kernel, the sync witness init and the pass-6 time
# witness init (the cell dispatch changed) on each target
for a in x86_64:x86 aarch64:aarch64 riscv64:riscv; do
  t=${a%:*}; f=${a#*:}
  step X_check_default_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features -p yarm --bin kernel_boot
  step X_check_witness_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features --features $f-shared-region-direct-oracle,futex-time-witness,futex-requeue-witness -p yarm --bin kernel_boot
  step X_check_sync_witness_init_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-user-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features --features futex-requeue-witness,futex-time-witness,futex-sync-witness -p yarm-control-plane-servers --bin init_server
  step X_check_time_witness_init_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-user-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features --features futex-requeue-witness,futex-time-witness -p yarm-control-plane-servers --bin init_server
done

# ── default builds of C, the GP / RT / RT+budget core smokes on them, then B ────────────────
for t in x86_64 aarch64 riscv64; do
  step B_build_C_$t env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh
  k=$(kpath $O/B_build_C_$t.log); [[ -n "$k" ]] && cp "$k" $O/kernels/C-$t.elf
  cp $W/build-$t/initramfs-core.cpio $O/initramfs/C-$t-initramfs-core.cpio
  to=600; [[ $t == riscv64 ]] && to=300
  step S_gp_core_smp2_$t env KERNEL_IMAGE=$(kimg $t) INITRAMFS_IMAGE=build-$t/initramfs-core.cpio QEMU_SMP=2 LOGFILE=$O/S_gp_$t.boot.log SMOKE_LOG=$O/S_gp_$t.smoke.log TIMEOUT_SECS=$to scripts/qemu-$t-core-smoke.sh
  step S_rt_core_smp2_$t env KERNEL_IMAGE=$(kimg $t) INITRAMFS_IMAGE=build-$t/initramfs-core.cpio QEMU_SMP=2 "KERNEL_CMDLINE=console=ttyS0 rdinit=/init yarm.sched_profile=rt" LOGFILE=$O/S_rt_$t.boot.log SMOKE_LOG=$O/S_rt_$t.smoke.log TIMEOUT_SECS=$to scripts/qemu-$t-core-smoke.sh
  step S_rtb_core_smp2_$t env KERNEL_IMAGE=$(kimg $t) INITRAMFS_IMAGE=build-$t/initramfs-core.cpio QEMU_SMP=2 "KERNEL_CMDLINE=console=ttyS0 rdinit=/init yarm.sched_profile=rt yarm.sched_budget=on" LOGFILE=$O/S_rtb_$t.boot.log SMOKE_LOG=$O/S_rtb_$t.smoke.log TIMEOUT_SECS=$to scripts/qemu-$t-core-smoke.sh
  step B_build_B_$t bash -c "cd $BW && env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh"
  k=$(kpath $O/B_build_B_$t.log); [[ -n "$k" ]] && cp "$k" $O/kernels/B-$t.elf
  cp $BW/build-$t/initramfs-core.cpio $O/initramfs/B-$t-initramfs-core.cpio
done
for t in x86_64 aarch64 riscv64; do
  { echo "== $t"; for v in gp rt rtb; do echo "-- $v"; grep -a "YARM_SCHED_PROFILE\|YARM_SCHED_BUDGET\|TIMEKEEP_SOURCE" $O/S_${v}_$t.boot.log | tr -d '\r' | head -3; done; } >> $O/S_profile_markers.txt 2>&1
done
( cd $O/kernels && sha256sum *.elf > SHA256SUMS
  for t in x86_64 aarch64 riscv64; do
    for v in B C; do echo "== $v-$t"; llvm-size -A $v-$t.elf | grep -E '^\.(text|rodata|data|bss) '; done
  done ) > $O/Z2_sizes.txt 2>&1
( cd $O/initramfs && sha256sum *.cpio > SHA256SUMS )
echo "Z2_sizes written" >> $SUM

# ── live: the timed mutex / condvar witness, SMP2 on each port (GP) ──────────────────────────
step W1_x86_64_smp2 env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/W1 scripts/qemu-futex-sync-witness-smoke.sh x86_64 2
step W2_aarch64_smp2 env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/W2 scripts/qemu-futex-sync-witness-smoke.sh aarch64 2
step W3_riscv64_smp2 env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/W3 scripts/qemu-futex-sync-witness-smoke.sh riscv64 2

# ── negative controls (expected rc != 0; tree restored after each) ───────────────────────────
for nc in nc1_reset_deadline_on_retry nc2_timeout_without_reacquire nc3_drop_contended nc4_no_predicate_recheck; do
  step NC_hosted_rt_$nc bash -c "$NC $nc && git diff --stat && env -u CARGO_TARGET_DIR cargo test -q -p yarm-user-rt --lib -- --test-threads=1 futex_sync_tests; rc=\$?; git checkout -- $RT; exit \$rc"
  step NC_hosted_k_$nc bash -c "$NC $nc && git diff --stat && env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 sync_time1; rc=\$?; git checkout -- $RT; exit \$rc"
done
# live: condvar timeout without reacquiring, a full build (kernel and init), RISC-V SMP2
step NC_live_nc2_timeout_without_reacquire_riscv64 bash -c "$NC nc2_timeout_without_reacquire && git diff --stat && env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/NCL2 scripts/qemu-futex-sync-witness-smoke.sh riscv64 2; rc=\$?; git checkout -- $RT; exit \$rc"
echo "post-NC porcelain=$(git -C $W status --porcelain | wc -l) base-porcelain=$(git -C $BW status --porcelain | wc -l)" >> $SUM
echo "fst1 qual end $(date -u +%FT%TZ)" >> $SUM
echo FST1_QUAL_DONE >> $SUM
