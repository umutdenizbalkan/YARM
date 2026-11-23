#!/usr/bin/env bash
# FUTEX-SEAL1 focused qualification (run 3): candidate C vs baseline c4cac895 (B, the delivered
# FUTEX-SYNC-TIME1 head). Steps in order, every log kept, nothing rerun in place. Run 1 (bc5194a5)
# and run 2 (acbf79c1) are retained as qual-1-FAILED and qual-2-FAILED; the steps are unchanged.
# Reused, not rerun: the hosted lock-order baseline is the FUTEX-SYNC-TIME1 run 3 (f88db55a;
# c4cac895 differs from it only in doc/). Negative-control mutations: $S/fs1-nc/nc.py (hashed
# with this runner, the seal grader and the smoke script).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C=${C:?candidate}
B=c4cac895
O=$S/fs1-evidence/qual
W=$S/fst1-wt
BW=$S/fab1-wt
SUM=$O/summary.txt
NC="python3 -I $S/fs1-nc/nc.py"
CELLS="os nr17 lh pb rb"
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
wto() { case $1 in riscv64) echo 360;; *) echo 900;; esac; }
# The reused FUTEX-ABS-BITSET1 witness on RISC-V: its saturation sequence E logs ~2M lines under TCG
# and outran 900 s of wall time in a development run (time-dev-riscv64); it gets 1800 s.
tto() { case $1 in riscv64) echo 1800;; *) echo 900;; esac; }
echo "fs1 qual start $(date -u +%FT%TZ) C=$C B=$B sha=$(sha256sum $0 | cut -c1-16) nc=$(sha256sum $S/fs1-nc/nc.py | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $C
[[ "$(git -C $BW rev-parse --short=8 HEAD)" == "$B" ]] || { echo "baseline worktree not at $B" >> $SUM; exit 1; }
[[ "$(git -C $BW status --porcelain | wc -l)" == 0 ]] || { echo "baseline worktree not clean" >> $SUM; exit 1; }

# ── hosted ──────────────────────────────────────────────────────────────────────────────────
step H1_fmt cargo fmt --all -- --check
step H2_hosted_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
step H3_integration env -u CARGO_TARGET_DIR cargo test -q --tests --no-fail-fast -- --test-threads=1
for f in futex_requeue1 sync_time1 r30_the_contended_state abs_bitset1 time1 futex rt1b_ sched_budget rt1a_ stage45 syscall_abi lock_order census; do
  step H4_subset_$f env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 $f
done
step H4_crate_yarm_user_rt env -u CARGO_TARGET_DIR cargo test -q -p yarm-user-rt --lib -- --test-threads=1
step H4_crate_yarm_kernel env -u CARGO_TARGET_DIR cargo test -q -p yarm-kernel --lib -- --test-threads=1
step H4_crate_yarm_ipc_abi env -u CARGO_TARGET_DIR cargo test -q -p yarm-ipc-abi --lib -- --test-threads=1
step H4_grader_self_test_seal python3 -I scripts/grade-futex-seal-witness.py --self-test
step H4_grader_self_test_sync python3 -I scripts/grade-futex-sync-witness.py --self-test
step H4_grader_self_test_time python3 -I scripts/grade-futex-time-witness.py --self-test
step H5_lockorder_C bash -c "env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 --nocapture > $O/H5_C.raw.log 2>&1; echo rc=\$?"
pairs < $O/H5_C.raw.log > $O/H5_C.pairs.txt; cp $S/keep/fst1_H5_C.pairs.txt $O/H5_B_reused_from_f88db55a.pairs.txt
diff <(awk '{print $1, $2}' $O/H5_C.pairs.txt) <(awk '{print $1, $2}' $O/H5_B_reused_from_f88db55a.pairs.txt) > $O/H5_pairtypes.diff; echo "H5_pairtypes C=$(wc -l < $O/H5_C.pairs.txt) B=$(wc -l < $O/H5_B_reused_from_f88db55a.pairs.txt) diff_lines=$(wc -l < $O/H5_pairtypes.diff)" >> $SUM
gzip -f $O/H5_C.raw.log
step G1_contract_gate bash scripts/check-current-contracts.sh
step G2_doc_enforcement bash scripts/check-contract-doc-enforcement.sh
for a in x86_64:x86 aarch64:aarch64 riscv64:riscv; do
  t=${a%:*}; f=${a#*:}
  step X_check_default_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features -p yarm --bin kernel_boot
  step X_check_seal_kernel_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features --features $f-shared-region-direct-oracle,futex-seal-witness -p yarm --bin kernel_boot
  step X_check_seal_init_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-user-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features --features futex-requeue-witness,futex-time-witness,futex-seal-witness,futex-seal-os,futex-seal-nr17,futex-seal-lh,futex-seal-pb,futex-seal-rb -p yarm-control-plane-servers --bin init_server
  step X_check_sync_init_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-user-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features --features futex-requeue-witness,futex-time-witness,futex-sync-witness -p yarm-control-plane-servers --bin init_server
  step X_check_time_init_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-user-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features --features futex-requeue-witness,futex-time-witness -p yarm-control-plane-servers --bin init_server
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

# ── live, SMP2 on each port: the seal cells (GP and RT+budget), and the reused pass-6 / pass-7 witnesses
for t in riscv64 aarch64 x86_64; do
  step W_seal_gp_$t env -u CARGO_TARGET_DIR TIMEOUT_SECS=$(wto $t) SEAL_CELLS="$CELLS" SEAL_PROFILE=gp LOGDIR=$O/W_seal_gp_$t scripts/qemu-futex-seal-witness-smoke.sh $t 2
  step W_seal_rtb_$t env -u CARGO_TARGET_DIR TIMEOUT_SECS=$(wto $t) SEAL_CELLS="$CELLS" SEAL_PROFILE=rtb LOGDIR=$O/W_seal_rtb_$t scripts/qemu-futex-seal-witness-smoke.sh $t 2
  step W_time_$t env -u CARGO_TARGET_DIR TIMEOUT_SECS=$(tto $t) LOGDIR=$O/W_time_$t scripts/qemu-futex-time-witness-smoke.sh $t 2
  step W_sync_$t env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/W_sync_$t scripts/qemu-futex-sync-witness-smoke.sh $t 2
done

# ── negative controls (expected rc != 0; the mutated file restored after each) ───────────────
P=$($NC ncC_duplicate_settlement --path)
step NC_hosted_ncC_duplicate_settlement bash -c "$NC ncC_duplicate_settlement && git diff --stat && env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 futex_requeue1::; rc=\$?; git checkout -- $P; exit \$rc"
P=$($NC ncB_condvar_no_reacquire --path)
step NC_hosted_ncB_condvar_no_reacquire bash -c "$NC ncB_condvar_no_reacquire && git diff --stat && env -u CARGO_TARGET_DIR cargo test -q -p yarm-user-rt --lib -- --test-threads=1 futex_sync_tests; rc=\$?; git checkout -- $P; exit \$rc"
# live, each a full build of its mutated tree, RISC-V SMP2
P=$($NC ncA_wakeall_in_rq --path)
step NC_live_ncA_wakeall_in_rq_riscv64 bash -c "$NC ncA_wakeall_in_rq && git diff --stat && env -u CARGO_TARGET_DIR TIMEOUT_SECS=360 SEAL_CELLS=\"$CELLS\" SEAL_PROFILE=gp LOGDIR=$O/NCLA scripts/qemu-futex-seal-witness-smoke.sh riscv64 2; rc=\$?; git checkout -- $P; exit \$rc"
P=$($NC ncB_condvar_no_reacquire --path)
step NC_live_ncB_condvar_no_reacquire_riscv64 bash -c "$NC ncB_condvar_no_reacquire && git diff --stat && env -u CARGO_TARGET_DIR TIMEOUT_SECS=360 SEAL_CELLS=\"$CELLS\" SEAL_PROFILE=rtb LOGDIR=$O/NCLB scripts/qemu-futex-seal-witness-smoke.sh riscv64 2; rc=\$?; git checkout -- $P; exit \$rc"
echo "post-NC porcelain=$(git -C $W status --porcelain | wc -l) base-porcelain=$(git -C $BW status --porcelain | wc -l)" >> $SUM
echo "fs1 qual end $(date -u +%FT%TZ)" >> $SUM
echo FS1_QUAL_DONE >> $SUM
