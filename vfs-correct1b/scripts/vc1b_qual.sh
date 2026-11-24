#!/usr/bin/env bash
# VFS-CORRECT1B qualification: candidate C (committed) vs baseline cfc14b33 (B). Steps run in order,
# every log is kept, nothing is rerun in place; a failed run is kept under its own directory.
# Negative controls run in fresh worktrees checked out at C (one per control) and mutated there by
# $S/vc1b-nc/nc.py; the candidate worktree is never mutated. This runner, nc.py, both graders and
# the smoke script are hashed into predeclared.sha before the run.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C=${C:?candidate}
RUN=${RUN:?run name}
B=cfc14b33
O=$S/vc1b-evidence/$RUN
W=$S/vc1b-wt
BW=$S/vc1a-wt
NCW=$S/vc1b-nc-wt
SUM=$O/summary.txt
NC="python3 -I $S/vc1b-nc/nc.py"
mkdir -p $O/kernels $O/initramfs
step() {
  local name=$1; shift
  local por; por=$(git -C $W status --porcelain | wc -l)
  echo "=== $name [$(git -C $W rev-parse --short=12 HEAD) porcelain=$por]: $* (start $(date -u +%H:%M:%S))" >> $SUM
  local t0=$SECONDS
  ( cd $W && "$@" ) > $O/$name.log 2>&1
  local rc=$?
  echo "$name rc=$rc secs=$((SECONDS - t0)) free=$(df -h /tmp | awk 'NR==2{print $4}')" >> $SUM
}
# Build caches only (never evidence): cross-target artifacts are rebuilt by every later build.
freecross() { rm -rf $1/target/x86_64-yarm-none $1/target/aarch64-yarm-none $1/target/riscv64-yarm-none $1/target/x86_64-yarm-user-none $1/target/aarch64-yarm-user-none $1/target/riscv64-yarm-user-none $1/target/riscv64gc-unknown-none-elf; }
# A negative control in its own worktree at C: mutate, run, record, remove the build directory.
ncstep() {
  local name=$1 mut=$2; shift 2
  local wt=$NCW/$name
  git -C $W worktree add -q --detach $wt $C >> $O/$name.log 2>&1
  echo "=== $name [worktree $wt at $(git -C $wt rev-parse --short=12 HEAD)]: $mut; $* (start $(date -u +%H:%M:%S))" >> $SUM
  local t0=$SECONDS
  ( cd $wt && $NC $mut && git diff && "$@" ) >> $O/$name.log 2>&1
  local rc=$?
  echo "$name rc=$rc secs=$((SECONDS - t0)) mutated_files=$(git -C $wt diff --name-only | tr '\n' ' ')" >> $SUM
  rm -rf $wt/target $wt/build-x86_64 $wt/build-aarch64 $wt/build-riscv64
}
kpath() { grep -a "ARTIFACT_CARGO_OUTPUT bin=kernel_boot " "$1" | tail -1 | sed 's/.* path=\([^ ]*\) .*/\1/'; }
kimg() { case $1 in x86_64) echo build-x86_64/kernel_boot.elf;; aarch64) echo build-aarch64/yarm-aarch64.bin;; riscv64) echo build-riscv64/yarm-riscv64.bin;; esac; }
XCHK="env -u CARGO_TARGET_DIR cargo check -q -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features"
echo "vc1b qual $RUN start $(date -u +%FT%TZ) C=$C B=$B runner=$(sha256sum $0 | cut -c1-16) nc=$(sha256sum $S/vc1b-nc/nc.py | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $C || { echo "cannot check out $C" >> $SUM; exit 1; }
[[ "$(git -C $W status --porcelain | wc -l)" == 0 ]] || { echo "candidate worktree not clean" >> $SUM; exit 1; }
[[ "$(git -C $BW rev-parse --short=8 HEAD)" == "$B" ]] || { echo "baseline worktree not at $B" >> $SUM; exit 1; }
[[ "$(git -C $BW status --porcelain | wc -l)" == 0 ]] || { echo "baseline worktree not clean" >> $SUM; exit 1; }
echo "candidate tree $(git -C $W rev-parse HEAD^{tree})" >> $SUM

# ── hosted ──
step H1_fmt cargo fmt --all -- --check
step H2_hosted_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
step H3_integration env -u CARGO_TARGET_DIR cargo test -q --tests --no-fail-fast -- --test-threads=1
for p in yarm-ipc-abi yarm-user-rt yarm-fs-servers yarm-srv-common yarm-kernel yarm-control-plane-servers; do
  step H4_crate_$p env -u CARGO_TARGET_DIR cargo test -q -p $p --lib -- --test-threads=1
done
step H5_subset_user_rt_vc1b env -u CARGO_TARGET_DIR cargo test -q -p yarm-user-rt --lib -- --test-threads=1 vc1b
step H5_subset_fs_servers_vc1b env -u CARGO_TARGET_DIR cargo test -q -p yarm-fs-servers --lib -- --test-threads=1 vc1b
step H5_subset_fs_servers_vc1a env -u CARGO_TARGET_DIR cargo test -q -p yarm-fs-servers --lib -- --test-threads=1 vc1a
step H5_subset_root_vc1b env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 vc1b
step H5_subset_root_spawn_ledger env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 u9spawn
step H5_subset_root_lock_order env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 lock_order
step H5_subset_root_syscall_abi env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 syscall_abi
step H5_subset_root_grant env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 grant_ro
step H6_grader_self_test_mount python3 -I scripts/grade-vfs-mount-witness.py --self-test
step H6_grader_self_test_correct python3 -I scripts/grade-vfs-correct-witness.py --self-test
step G1_contract_gate bash scripts/check-current-contracts.sh
step G2_doc_enforcement bash scripts/check-contract-doc-enforcement.sh
step G3_proc_vfs_codec_freeze bash scripts/check-proc-vfs-codec-freeze.sh
for a in x86_64 aarch64 riscv64; do
  T="--target targets/$a-yarm-none.json"
  U="--target targets/$a-yarm-user-none.json"
  step X_check_default_kernel_$a $XCHK $T -p yarm --bin kernel_boot
  step X_check_genwitness_kernel_$a $XCHK $T --features cap-gen-witness -p yarm --bin kernel_boot
  step X_check_default_servers_$a $XCHK $U -p yarm-control-plane-servers -p yarm-fs-servers --bins
  step X_check_witness_servers_$a $XCHK $U --features vfs-mount-witness,vfs-correct-witness -p yarm-control-plane-servers -p yarm-fs-servers --bins
done

freecross $W
# ── builds and core smokes (default artifacts) ──
for t in x86_64 aarch64 riscv64; do
  step B_build_C_$t env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh
  k=$(kpath $O/B_build_C_$t.log); [[ -n "$k" ]] && cp "$k" $O/kernels/C-$t.elf
  cp $W/build-$t/initramfs-core.cpio $O/initramfs/C-$t-initramfs-core.cpio
  step S_gp_core_smp2_$t env KERNEL_IMAGE=$(kimg $t) INITRAMFS_IMAGE=build-$t/initramfs-core.cpio QEMU_SMP=2 LOGFILE=$O/S_gp_$t.boot.log SMOKE_LOG=$O/S_gp_$t.smoke.log TIMEOUT_SECS=900 scripts/qemu-$t-core-smoke.sh
  step B_build_B_$t bash -c "cd $BW && env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh"
  k=$(kpath $O/B_build_B_$t.log); [[ -n "$k" ]] && cp "$k" $O/kernels/B-$t.elf
  cp $BW/build-$t/initramfs-core.cpio $O/initramfs/B-$t-initramfs-core.cpio
  rm -rf $BW/target; freecross $W
done
( cd $O/kernels && sha256sum *.elf > SHA256SUMS ); ( cd $O/initramfs && sha256sum *.cpio > SHA256SUMS )
rm -rf $BW/target; freecross $W

# ── live witness, every port, both profiles ──
for t in x86_64 aarch64 riscv64; do
  for prof in wide narrow; do
    step W_${prof}_$t env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/W_${prof}_$t scripts/qemu-vfs-mount-witness-smoke.sh $t 2 $prof
  done
  freecross $W
done

# ── negative controls (each in its own worktree at C) ──
CT="env -u CARGO_TARGET_DIR cargo test -q"
ncstep NC_hosted_ncT_recv_truncates ncT_recv_truncates $CT -p yarm-user-rt --lib -- --test-threads=1 vc1b
ncstep NC_hosted_ncT_vfs_truncates ncT_vfs_truncates $CT -p yarm-fs-servers --lib -- --test-threads=1 vc1b
ncstep NC_hosted_ncS_sender_slot ncS_sender_slot $CT -p yarm-fs-servers --lib -- --test-threads=1 vc1b
ncstep NC_hosted_ncK_no_slot17 ncK_no_slot17 $CT --lib -- --test-threads=1 vc1b_spawn_admin
ncstep NC_live_ncT_vfs_truncates_aarch64 ncT_vfs_truncates env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/NCL_T scripts/qemu-vfs-mount-witness-smoke.sh aarch64 2 wide
ncstep NC_live_ncS_sender_slot_aarch64 ncS_sender_slot env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/NCL_S scripts/qemu-vfs-mount-witness-smoke.sh aarch64 2 wide
echo "post-NC porcelain=$(git -C $W status --porcelain | wc -l) base-porcelain=$(git -C $BW status --porcelain | wc -l) candidate=$(git -C $W rev-parse --short=12 HEAD)" >> $SUM
echo "vc1b qual $RUN end $(date -u +%FT%TZ)" >> $SUM
echo VC1B_QUAL_DONE >> $SUM
