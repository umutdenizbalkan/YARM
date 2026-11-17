#!/usr/bin/env bash
# RT-SCHED1B focused qualification: candidate C vs baseline 5360efdd (B, the RT-SCHED1A head).
# Steps in order, every log kept, nothing rerun in place. Unaffected coverage reuses the accepted
# RT-SCHED1A evidence; no historical matrix. Negative-control patches: $S/rt1b-nc/*.py (hashed).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C=${C:?candidate}
B=5360efdd
O=$S/rt1b-evidence/qual
W=$S/rt1b-wt
BW=$S/sl2b-base-wt
SUM=$O/summary.txt
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
echo "rt1b qual start $(date -u +%FT%TZ) C=$C B=$B sha=$(sha256sum $0 | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $C
git -C $BW checkout -q --detach $B
[[ "$(git -C $BW rev-parse --short=8 HEAD)" == "$B" ]] || { echo "baseline worktree not at $B" >> $SUM; exit 1; }

# ── hosted ──────────────────────────────────────────────────────────────────────────────────
step H1_fmt cargo fmt --all -- --check
step H2_hosted_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
step H3_integration env -u CARGO_TARGET_DIR cargo test -q --tests --no-fail-fast -- --test-threads=1
for f in rt1b_ sched_budget rt1a_ kthread sched_full_rot sched_lock1 sl1_ fair1_ lock_order census abi stage199d u3_ed u9timer ap_sched1 sl2b_ sl2a_ task_enqueue overtaken reap futex ipc_timeout sr_place1 run_queue boot_command_line; do
  step H4_subset_$f env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 $f
done
step H4_crate_yarm_kernel env -u CARGO_TARGET_DIR cargo test -q -p yarm-kernel --lib -- --test-threads=1
step H4_grader_self_test python3 -I scripts/grade-rt-sched-witness.py --self-test
# lock-order: descending sequential pair TYPES, candidate vs baseline, whole serial lib suite
step H5_lockorder_C bash -c "env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 --nocapture > $O/H5_C.raw.log 2>&1; echo rc=\$?"
step H5_lockorder_B bash -c "cd $BW && env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 --nocapture > $O/H5_B.raw.log 2>&1; echo rc=\$?"
pairs < $O/H5_C.raw.log > $O/H5_C.pairs.txt; pairs < $O/H5_B.raw.log > $O/H5_B.pairs.txt
diff <(awk '{print $1, $2}' $O/H5_C.pairs.txt) <(awk '{print $1, $2}' $O/H5_B.pairs.txt) > $O/H5_pairtypes.diff; echo "H5_pairtypes C=$(wc -l < $O/H5_C.pairs.txt) B=$(wc -l < $O/H5_B.pairs.txt) diff_lines=$(wc -l < $O/H5_pairtypes.diff)" >> $SUM
gzip -f $O/H5_C.raw.log $O/H5_B.raw.log
step G1_contract_gate bash scripts/check-current-contracts.sh
step G2_doc_enforcement bash scripts/check-contract-doc-enforcement.sh
# cross-target checks: the default kernel and the witness feature
for a in x86_64:x86 aarch64:aarch64 riscv64:riscv64; do
  t=${a%:*}; f=${a#*:}
  step X_check_default_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features -p yarm --bin kernel_boot
  step X_check_witness_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features --features $f-rt-sched-witness -p yarm --bin kernel_boot
done

# ── scheduler-state sizes (hosted layout; same 64-bit layout on every port), C and B ────────
PROBE='
#[cfg(test)]
mod rt1a_size_probe {
    #[test]
    fn rt1a_size_probe() {
        std::println!(
            "RT1B_SIZE CpuSched={} CpuCell={} PriorityScheduler={} Admission={} SchedDomain={} align_CpuCell={}",
            core::mem::size_of::<super::CpuSched>(),
            core::mem::size_of::<super::CpuCell>(),
            core::mem::size_of::<super::PriorityScheduler>(),
            core::mem::size_of::<super::Admission>(),
            core::mem::size_of::<super::SchedDomain>(),
            core::mem::align_of::<super::CpuCell>()
        );
    }
}'
PROBE_RQ='
#[cfg(test)]
mod rt1a_size_probe_rq {
    #[test]
    fn rt1a_size_probe_rq() {
        std::println!(
            "RT1B_SIZE RunQueues={} BudgetLedger={} SchedContext={} OptionSchedContext={}",
            core::mem::size_of::<super::RunQueues>(),
            core::mem::size_of::<crate::kernel::sched_budget::BudgetLedger>(),
            core::mem::size_of::<crate::kernel::sched_budget::SchedContext>(),
            core::mem::size_of::<Option<crate::kernel::sched_budget::SchedContext>>()
        );
    }
}'
step Z1_size_probe_C bash -c "printf '%s\n%s\n' \"\$1\" \"\$2\" >> src/kernel/scheduler.rs && env -u CARGO_TARGET_DIR cargo test -q --lib rt1a_size_probe -- --nocapture --test-threads=1; rc=\$?; git checkout -- src/kernel/scheduler.rs; exit \$rc" _ "$PROBE" "$PROBE_RQ"
step Z1_size_probe_B bash -c "cd $BW && printf '%s\n' \"\$1\" >> src/kernel/scheduler.rs && env -u CARGO_TARGET_DIR cargo test -q --lib rt1a_size_probe -- --nocapture --test-threads=1; rc=\$?; git checkout -- src/kernel/scheduler.rs; exit \$rc" _ "$PROBE"

# ── default builds of C, the default (GP) and RT-profile core smokes on them, then B ─────────
for t in x86_64 aarch64 riscv64; do
  step B_build_C_$t env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh
  k=$(kpath $O/B_build_C_$t.log); [[ -n "$k" ]] && cp "$k" $O/kernels/C-$t.elf
  to=600; [[ $t == riscv64 ]] && to=300
  step S_gp_core_smp2_$t env KERNEL_IMAGE=$(kimg $t) INITRAMFS_IMAGE=build-$t/initramfs-core.cpio QEMU_SMP=2 LOGFILE=$O/S_gp_$t.boot.log SMOKE_LOG=$O/S_gp_$t.smoke.log TIMEOUT_SECS=$to scripts/qemu-$t-core-smoke.sh
  step S_rt_core_smp2_$t env KERNEL_IMAGE=$(kimg $t) INITRAMFS_IMAGE=build-$t/initramfs-core.cpio QEMU_SMP=2 "KERNEL_CMDLINE=console=ttyS0 rdinit=/init yarm.sched_profile=rt" LOGFILE=$O/S_rt_$t.boot.log SMOKE_LOG=$O/S_rt_$t.smoke.log TIMEOUT_SECS=$to scripts/qemu-$t-core-smoke.sh
  step S_rtb_core_smp2_$t env KERNEL_IMAGE=$(kimg $t) INITRAMFS_IMAGE=build-$t/initramfs-core.cpio QEMU_SMP=2 "KERNEL_CMDLINE=console=ttyS0 rdinit=/init yarm.sched_profile=rt yarm.sched_budget=on" LOGFILE=$O/S_rtb_$t.boot.log SMOKE_LOG=$O/S_rtb_$t.smoke.log TIMEOUT_SECS=$to scripts/qemu-$t-core-smoke.sh
  [[ $t == riscv64 ]] && step S_gpb_refused_core_smp2_$t env KERNEL_IMAGE=$(kimg $t) INITRAMFS_IMAGE=build-$t/initramfs-core.cpio QEMU_SMP=2 "KERNEL_CMDLINE=console=ttyS0 rdinit=/init yarm.sched_budget=on" LOGFILE=$O/S_gpb_$t.boot.log SMOKE_LOG=$O/S_gpb_$t.smoke.log TIMEOUT_SECS=$to scripts/qemu-$t-core-smoke.sh
  step B_build_B_$t bash -c "cd $BW && env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh"
  k=$(kpath $O/B_build_B_$t.log); [[ -n "$k" ]] && cp "$k" $O/kernels/B-$t.elf
done
for t in x86_64 aarch64 riscv64; do
  { echo "== GP $t"; grep -a -c "YARM_SCHED_PROFILE profile=gp" $O/S_gp_$t.boot.log; grep -a -c "RT_PREEMPT_COMMITTED\|RT_RESCHED_DECLINED" $O/S_gp_$t.boot.log
    echo "== RT $t"; grep -a -c "YARM_SCHED_PROFILE profile=rt" $O/S_rt_$t.boot.log; grep -a -c "YARM_SCHED_BUDGET" $O/S_rt_$t.boot.log; grep -a -o "RT_PREEMPT_COMMITTED cpu=[0-9] cause=[0-9]\|RT_RESCHED_DECLINED cpu=[0-9] cause=[0-9]\|RT_PREEMPT_DEFERRED cpu=[0-9] cause=[0-9]" $O/S_rt_$t.boot.log | sort | uniq -c
    echo "== RT+budget $t"; grep -a "YARM_SCHED_PROFILE\|YARM_SCHED_BUDGET" $O/S_rtb_$t.boot.log; grep -a -o "RT_PREEMPT_COMMITTED cpu=[0-9] cause=[0-9]\|RT_RESCHED_DECLINED cpu=[0-9] cause=[0-9]\|RT_PREEMPT_DEFERRED cpu=[0-9] cause=[0-9]" $O/S_rtb_$t.boot.log | sort | uniq -c; } >> $O/S_profile_markers.txt 2>&1
done
( cd $O/kernels && sha256sum *.elf > SHA256SUMS
  for t in x86_64 aarch64 riscv64; do
    for v in B C; do echo "== $v-$t"; llvm-size -A $v-$t.elf | grep -E '^\.(text|rodata|data|bss) '; done
  done ) > $O/Z2_sizes.txt 2>&1
{ echo "== GP + yarm.sched_budget=on riscv64 (must be refused)"; grep -a "YARM_SCHED_PROFILE\|YARM_SCHED_BUDGET" $O/S_gpb_riscv64.boot.log; grep -a -c "RT_PREEMPT_COMMITTED\|RT_RESCHED_DECLINED" $O/S_gpb_riscv64.boot.log; } >> $O/S_profile_markers.txt 2>&1
echo "Z2_sizes written" >> $SUM

# ── live: the RT witness with budgets (phases A–F), SMP2 on each port; and once without ──────
step W1_x86_64_smp2 env -u CARGO_TARGET_DIR SCHED_BUDGET=on LOGDIR=$O/W1 scripts/qemu-rt-sched-witness-smoke.sh x86_64 2
step W2_aarch64_smp2 env -u CARGO_TARGET_DIR SCHED_BUDGET=on LOGDIR=$O/W2 scripts/qemu-rt-sched-witness-smoke.sh aarch64 2
step W3_riscv64_smp2 env -u CARGO_TARGET_DIR SCHED_BUDGET=on LOGDIR=$O/W3 scripts/qemu-rt-sched-witness-smoke.sh riscv64 2
step W4_riscv64_smp2_budget_off env -u CARGO_TARGET_DIR SCHED_BUDGET=off SKIP_BUILD=1 LOGDIR=$O/W4 scripts/qemu-rt-sched-witness-smoke.sh riscv64 2

# ── negative controls (expected rc != 0; tree restored after each) ───────────────────────────
FORGIVE="python3 -I $S/rt1b-nc/forgive.py"
RUNS="python3 -I $S/rt1b-nc/runs.py"
step NC1a_forgive_debt_hosted bash -c "$FORGIVE && git diff --stat && env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 rt1b_; rc=\$?; git checkout -- src/kernel/sched_budget.rs; exit \$rc"
step NC2a_exhausted_runs_hosted bash -c "$RUNS && git diff --stat && env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 rt1b_; rc=\$?; git checkout -- src/kernel/scheduler.rs; exit \$rc"
step NC2b_exhausted_runs_live_riscv64 bash -c "$RUNS && git diff --stat && env -u CARGO_TARGET_DIR SCHED_BUDGET=on SKIP_BASE=1 LOGDIR=$O/NC2b scripts/qemu-rt-sched-witness-smoke.sh riscv64 2; rc=\$?; git checkout -- src/kernel/scheduler.rs; exit \$rc"
echo "post-NC porcelain=$(git -C $W status --porcelain | wc -l) base-porcelain=$(git -C $BW status --porcelain | wc -l)" >> $SUM
echo "rt1b qual end $(date -u +%FT%TZ)" >> $SUM
echo RT1B_QUAL_DONE >> $SUM
