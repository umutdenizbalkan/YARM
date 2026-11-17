#!/usr/bin/env bash
# FUTEX-ABS-BITSET1 focused qualification: candidate C vs baseline e1aee62d (B, the RT-SCHED1B
# head). Steps in order, every log kept, nothing rerun in place. Unaffected coverage reuses the
# accepted RT-SCHED1B evidence; no historical matrix. Negative-control patches: $S/fab1-nc/nc.py
# (hashed with this runner).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C=${C:?candidate}
B=e1aee62d
O=$S/fab1-evidence/qual
W=$S/fab1-wt
BW=$S/sl2b-base-wt
SUM=$O/summary.txt
NC="python3 -I $S/fab1-nc/nc.py"
mkdir -p $O/kernels
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
echo "fab1 qual start $(date -u +%FT%TZ) C=$C B=$B sha=$(sha256sum $0 | cut -c1-16) nc=$(sha256sum $S/fab1-nc/nc.py | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $C
git -C $BW checkout -q --detach $B
[[ "$(git -C $BW rev-parse --short=8 HEAD)" == "$B" ]] || { echo "baseline worktree not at $B" >> $SUM; exit 1; }

# ── hosted ──────────────────────────────────────────────────────────────────────────────────
step H1_fmt cargo fmt --all -- --check
step H2_hosted_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
step H3_integration env -u CARGO_TARGET_DIR cargo test -q --tests --no-fail-fast -- --test-threads=1
for f in abs_bitset1 time1 sleep1 futex_requeue1 futex riscv_park_ret1 stage191d u9qa stage199d stage45 census lock_order syscall_abi abi timekeeping sched_time clock rt1b_ sched_budget rt1a_ ipc_timeout reap task_enqueue; do
  step H4_subset_$f env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 $f
done
step H4_crate_yarm_kernel env -u CARGO_TARGET_DIR cargo test -q -p yarm-kernel --lib -- --test-threads=1
step H4_crate_yarm_user_rt env -u CARGO_TARGET_DIR cargo test -q -p yarm-user-rt --lib -- --test-threads=1
step H4_crate_yarm_ipc_abi env -u CARGO_TARGET_DIR cargo test -q -p yarm-ipc-abi --lib -- --test-threads=1
step H4_grader_self_test python3 -I scripts/grade-futex-time-witness.py --self-test
# lock-order: descending sequential pair TYPES, candidate vs baseline, whole serial lib suite
step H5_lockorder_C bash -c "env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 --nocapture > $O/H5_C.raw.log 2>&1; echo rc=\$?"
step H5_lockorder_B bash -c "cd $BW && env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 --nocapture > $O/H5_B.raw.log 2>&1; echo rc=\$?"
pairs < $O/H5_C.raw.log > $O/H5_C.pairs.txt; pairs < $O/H5_B.raw.log > $O/H5_B.pairs.txt
diff <(awk '{print $1, $2}' $O/H5_C.pairs.txt) <(awk '{print $1, $2}' $O/H5_B.pairs.txt) > $O/H5_pairtypes.diff; echo "H5_pairtypes C=$(wc -l < $O/H5_C.pairs.txt) B=$(wc -l < $O/H5_B.pairs.txt) diff_lines=$(wc -l < $O/H5_pairtypes.diff)" >> $SUM
gzip -f $O/H5_C.raw.log $O/H5_B.raw.log
step G1_contract_gate bash scripts/check-current-contracts.sh
step G2_doc_enforcement bash scripts/check-contract-doc-enforcement.sh
# the gate's new v13 block must catch a runtime/kernel flag disagreement (expected rc != 0)
step G3_gate_control_flag_drift bash -c "sed -i 's/^    pub const FUTEX_FLAG_BITSET: usize = 1 << 2;\$/    pub const FUTEX_FLAG_BITSET: usize = 1 << 3;/' crates/yarm-user-rt/src/lib.rs && git diff --stat && bash scripts/check-contract-doc-enforcement.sh; rc=\$?; git checkout -- crates/yarm-user-rt/src/lib.rs; exit \$rc"
# cross-target checks: the default kernel and the witness kernel; the witness init on each target
for a in x86_64:x86 aarch64:aarch64 riscv64:riscv; do
  t=${a%:*}; f=${a#*:}
  step X_check_default_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features -p yarm --bin kernel_boot
  step X_check_witness_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features --features $f-shared-region-direct-oracle,futex-time-witness,futex-requeue-witness -p yarm --bin kernel_boot
  step X_check_witness_init_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-user-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features --features futex-requeue-witness,futex-time-witness -p yarm-control-plane-servers --bin init_server
done

# ── default builds of C, the GP / RT / RT+budget core smokes on them, then B ────────────────
for t in x86_64 aarch64 riscv64; do
  step B_build_C_$t env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh
  k=$(kpath $O/B_build_C_$t.log); [[ -n "$k" ]] && cp "$k" $O/kernels/C-$t.elf
  to=600; [[ $t == riscv64 ]] && to=300
  step S_gp_core_smp2_$t env KERNEL_IMAGE=$(kimg $t) INITRAMFS_IMAGE=build-$t/initramfs-core.cpio QEMU_SMP=2 LOGFILE=$O/S_gp_$t.boot.log SMOKE_LOG=$O/S_gp_$t.smoke.log TIMEOUT_SECS=$to scripts/qemu-$t-core-smoke.sh
  step S_rt_core_smp2_$t env KERNEL_IMAGE=$(kimg $t) INITRAMFS_IMAGE=build-$t/initramfs-core.cpio QEMU_SMP=2 "KERNEL_CMDLINE=console=ttyS0 rdinit=/init yarm.sched_profile=rt" LOGFILE=$O/S_rt_$t.boot.log SMOKE_LOG=$O/S_rt_$t.smoke.log TIMEOUT_SECS=$to scripts/qemu-$t-core-smoke.sh
  step S_rtb_core_smp2_$t env KERNEL_IMAGE=$(kimg $t) INITRAMFS_IMAGE=build-$t/initramfs-core.cpio QEMU_SMP=2 "KERNEL_CMDLINE=console=ttyS0 rdinit=/init yarm.sched_profile=rt yarm.sched_budget=on" LOGFILE=$O/S_rtb_$t.boot.log SMOKE_LOG=$O/S_rtb_$t.smoke.log TIMEOUT_SECS=$to scripts/qemu-$t-core-smoke.sh
  step B_build_B_$t bash -c "cd $BW && env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh"
  k=$(kpath $O/B_build_B_$t.log); [[ -n "$k" ]] && cp "$k" $O/kernels/B-$t.elf
done
for t in x86_64 aarch64 riscv64; do
  { echo "== $t"; for v in gp rt rtb; do echo "-- $v"; grep -a "YARM_SCHED_PROFILE\|YARM_SCHED_BUDGET\|TIMEKEEP_SOURCE" $O/S_${v}_$t.boot.log | tr -d '\r' | head -3; done; } >> $O/S_profile_markers.txt 2>&1
done
( cd $O/kernels && sha256sum *.elf > SHA256SUMS
  for t in x86_64 aarch64 riscv64; do
    for v in B C; do echo "== $v-$t"; llvm-size -A $v-$t.elf | grep -E '^\.(text|rodata|data|bss) '; done
  done ) > $O/Z2_sizes.txt 2>&1
echo "Z2_sizes written" >> $SUM

# ── live: the timed/bitset futex witness, SMP2 on each port (GP) ─────────────────────────────
step W1_x86_64_smp2 env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/W1 scripts/qemu-futex-time-witness-smoke.sh x86_64 2
step W2_aarch64_smp2 env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/W2 scripts/qemu-futex-time-witness-smoke.sh aarch64 2
step W3_riscv64_smp2 env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/W3 scripts/qemu-futex-time-witness-smoke.sh riscv64 2

# ── negative controls (expected rc != 0; tree restored after each) ───────────────────────────
for nc in nc1_wrong_clock_after_rekey nc2_ignored_wake_mask nc3a_reset_mask_on_requeue nc3b_reset_deadline_on_requeue nc4_duplicate_settlement; do
  step NC_hosted_$nc bash -c "$NC $nc && git diff --stat && env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 futex_requeue1::; rc=\$?; git checkout -- src/kernel/boot/futex.rs; exit \$rc"
done
# live: the ignored wake mask, a full build (kernel and init), RISC-V SMP2
step NC_live_nc2_ignored_wake_mask_riscv64 bash -c "$NC nc2_ignored_wake_mask && git diff --stat && env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/NCL2 scripts/qemu-futex-time-witness-smoke.sh riscv64 2; rc=\$?; git checkout -- src/kernel/boot/futex.rs; exit \$rc"
echo "post-NC porcelain=$(git -C $W status --porcelain | wc -l) base-porcelain=$(git -C $BW status --porcelain | wc -l)" >> $SUM
echo "fab1 qual end $(date -u +%FT%TZ)" >> $SUM
echo FAB1_QUAL_DONE >> $SUM
