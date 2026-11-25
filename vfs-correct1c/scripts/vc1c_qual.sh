#!/usr/bin/env bash
# VFS-CORRECT1C qualification: candidate C (committed) vs baseline fbc8ff49 (B). Steps run in order,
# every log is kept, nothing is rerun in place; a failed or interrupted run is kept under its own
# directory with its disposition. Negative controls run in fresh worktrees checked out at C (one
# per control) and mutated there by $S/vc1c-nc/nc.py; the candidate worktree is never mutated.
# This runner, nc.py, the graders and the smoke scripts are hashed into predeclared.sha first.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C=${C:?candidate}
RUN=${RUN:?run name}
FROM=${FROM:-H1}
B=fbc8ff49
O=$S/vc1c-evidence/$RUN
W=$S/vc1c-wt
BW=$S/vc1b-wt
NCW=$S/vc1c-nc-wt
SUM=$O/summary.txt
NC="python3 -I $S/vc1c-nc/nc.py"
mkdir -p $O/kernels $O/initramfs
started=0
skip() { [[ $started == 1 ]] && return 1; [[ "$1" == "$FROM"* ]] && { started=1; return 1; }; return 0; }
step() {
  local name=$1; shift
  skip $name && return 0
  local por; por=$(git -C $W status --porcelain | grep -v '^?? doc/YARM_' | wc -l)
  echo "=== $name [$(git -C $W rev-parse --short=12 HEAD) porcelain=$por]: $* (start $(date -u +%H:%M:%S))" >> $SUM
  local t0=$SECONDS
  ( cd $W && "$@" ) > $O/$name.log 2>&1
  local rc=$?
  echo "$name rc=$rc secs=$((SECONDS - t0)) free=$(df -h /tmp | awk 'NR==2{print $4}')" >> $SUM
}
freecross() { rm -rf $1/target/x86_64-yarm-none $1/target/aarch64-yarm-none $1/target/riscv64-yarm-none $1/target/x86_64-yarm-user-none $1/target/aarch64-yarm-user-none $1/target/riscv64-yarm-user-none $1/target/riscv64gc-unknown-none-elf; }
ncstep() {
  local name=$1 mut=$2; shift 2
  skip $name && return 0
  local wt=$NCW/$name
  rm -rf $wt; git -C $W worktree prune
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
CT="env -u CARGO_TARGET_DIR cargo test -q"
echo "vc1c qual $RUN start $(date -u +%FT%TZ) C=$C B=$B FROM=$FROM runner=$(sha256sum $0 | cut -c1-16) nc=$(sha256sum $S/vc1c-nc/nc.py | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $C || { echo "cannot check out $C" >> $SUM; exit 1; }
[[ "$(git -C $W status --porcelain | grep -v '^?? doc/YARM_' | wc -l)" == 0 ]] || { echo "candidate worktree not clean" >> $SUM; exit 1; }
[[ "$(git -C $BW rev-parse --short=8 HEAD)" == "$B" ]] || { echo "baseline worktree not at $B" >> $SUM; exit 1; }
[[ "$(git -C $BW status --porcelain | wc -l)" == 0 ]] || { echo "baseline worktree not clean" >> $SUM; exit 1; }
echo "candidate tree $(git -C $W rev-parse HEAD^{tree})" >> $SUM

# ── hosted ──
step H1_fmt cargo fmt --all -- --check
step H2_hosted_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
step H3_integration env -u CARGO_TARGET_DIR cargo test -q --tests --no-fail-fast -- --test-threads=1
for p in yarm-ipc-abi yarm-user-rt yarm-fs-servers yarm-srv-common yarm-kernel yarm-control-plane-servers; do
  step H4_crate_$p $CT -p $p --lib -- --test-threads=1
done
step H5_subset_cp_vc1c $CT -p yarm-control-plane-servers --lib -- --test-threads=1 vc1c
step H5_subset_fs_vc1c $CT -p yarm-fs-servers --lib -- --test-threads=1 vc1c
step H5_subset_root_vc1c_nr21 $CT --lib -- --test-threads=1 vc1c_nr21
step H5_subset_root_syscall_abi $CT --lib -- --test-threads=1 syscall_abi
step H5_subset_root_variant_count $CT --lib -- --test-threads=1 variant_count
step H5_subset_root_spawn_ledger $CT --lib -- --test-threads=1 u9spawn
step H5_subset_root_fork $CT --lib -- --test-threads=1 fork
step H5_subset_root_lock_order $CT --lib -- --test-threads=1 lock_order
step H5_subset_fs_vc1b $CT -p yarm-fs-servers --lib -- --test-threads=1 vc1b
step H5_subset_fs_vc1a $CT -p yarm-fs-servers --lib -- --test-threads=1 vc1a
step H6_grader_self_test_fd python3 -I scripts/grade-vfs-fd-witness.py --self-test
step H6_grader_self_test_mount python3 -I scripts/grade-vfs-mount-witness.py --self-test
step H6_grader_self_test_correct python3 -I scripts/grade-vfs-correct-witness.py --self-test
step G1_contract_gate bash scripts/check-current-contracts.sh
step G2_doc_enforcement bash scripts/check-contract-doc-enforcement.sh
step G3_proc_vfs_codec_freeze bash scripts/check-proc-vfs-codec-freeze.sh
step G4_service_arch_boundary bash scripts/check-service-arch-boundary.sh
step G5_service_domain_ownership bash scripts/check-service-domain-ownership.sh
step G6_arch_boundary_baseline_scan_width bash -c "cd $BW && awk '/#\[cfg\(test\)\]/{exit} {print}' crates/yarm-control-plane-servers/src/control_plane/vfs/service.rs | wc -l; cd $W && awk 'prev ~ /#\[cfg\((all\()?test/ && /^mod [a-z_]*tests/ {exit} {print; prev=\$0}' crates/yarm-control-plane-servers/src/control_plane/vfs/service.rs | wc -l"
for a in x86_64 aarch64 riscv64; do
  T="--target targets/$a-yarm-none.json"
  U="--target targets/$a-yarm-user-none.json"
  step X_check_default_kernel_$a $XCHK $T -p yarm --bin kernel_boot
  step X_check_genwitness_kernel_$a $XCHK $T --features cap-gen-witness -p yarm --bin kernel_boot
  step X_check_default_servers_$a $XCHK $U -p yarm-control-plane-servers -p yarm-fs-servers --bins
  step X_check_fd_witness_servers_$a $XCHK $U --features vfs-fd-witness,vfs-correct-witness -p yarm-control-plane-servers -p yarm-fs-servers --bins
  step X_check_mount_witness_servers_$a $XCHK $U --features vfs-mount-witness,vfs-correct-witness -p yarm-control-plane-servers -p yarm-fs-servers --bins
done
freecross $W

# ── builds and core smokes (default artifacts), candidate and baseline ──
for t in x86_64 aarch64 riscv64; do
  step B_build_C_$t env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh
  if ! skip B_copy_C_$t; then
    k=$(kpath $O/B_build_C_$t.log); [[ -n "$k" ]] && cp "$k" $O/kernels/C-$t.elf
    cp $W/build-$t/initramfs-core.cpio $O/initramfs/C-$t-initramfs-core.cpio 2>/dev/null
  fi
  step S_gp_core_smp2_$t env KERNEL_IMAGE=$(kimg $t) INITRAMFS_IMAGE=build-$t/initramfs-core.cpio QEMU_SMP=2 LOGFILE=$O/S_gp_$t.boot.log SMOKE_LOG=$O/S_gp_$t.smoke.log TIMEOUT_SECS=900 scripts/qemu-$t-core-smoke.sh
  step B_build_B_$t bash -c "cd $BW && env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh"
  if ! skip B_copy_B_$t; then
    k=$(kpath $O/B_build_B_$t.log); [[ -n "$k" ]] && cp "$k" $O/kernels/B-$t.elf
    cp $BW/build-$t/initramfs-core.cpio $O/initramfs/B-$t-initramfs-core.cpio 2>/dev/null
  fi
  rm -rf $BW/target; freecross $W
done
( cd $O/kernels && sha256sum *.elf > SHA256SUMS ); ( cd $O/initramfs && sha256sum *.cpio > SHA256SUMS )

# ── live: the fd witness (wide, SMP1) and the VFS-CORRECT1B wide rerun (1A, 1B, NR 29, SMP2) ──
# The fd witness qualifies at SMP1: its second thread and fork child then share VFS's CPU, and the
# unmodified core smoke gates apply in full. At SMP2 they can run on CPU 1 while VFS blocks on
# CPU 0 and reach the split receive route's queue-publication race, whose D2_PUBLISH_RACE_UNWIND
# is a stop-ship marker (doc/AI_AGENT_RULES.md §14.3) that no gate here excuses. R_race_smp2_<port>
# records that boot per port as a FINDING step: either rc is recorded as it falls, rc=1 is
# expected whenever the race occurs, and it is never counted as a pass of the fd witness.
for t in aarch64 x86_64 riscv64; do
  step W_fd_wide_smp1_$t env -u CARGO_TARGET_DIR TIMEOUT_SECS=1500 LOGDIR=$O/W_fd_wide_smp1_$t scripts/qemu-vfs-fd-witness-smoke.sh $t 1 wide
  step W_1b_wide_$t env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/W_1b_wide_$t scripts/qemu-vfs-mount-witness-smoke.sh $t 2 wide
  freecross $W
done
step W_fd_narrow_smp1_aarch64 env -u CARGO_TARGET_DIR TIMEOUT_SECS=1500 LOGDIR=$O/W_fd_narrow_smp1_aarch64 scripts/qemu-vfs-fd-witness-smoke.sh aarch64 1 narrow
freecross $W
for t in aarch64 x86_64 riscv64; do
  step R_race_smp2_$t env -u CARGO_TARGET_DIR TIMEOUT_SECS=1500 LOGDIR=$O/R_race_smp2_$t scripts/qemu-vfs-fd-witness-smoke.sh $t 2 wide
  freecross $W
done

# ── negative controls (each in its own worktree at C) ──
ncstep NC_hosted_ncH_handle_as_fd ncH_handle_as_fd $CT -p yarm-control-plane-servers --lib -- --test-threads=1 vc1c
ncstep NC_hosted_ncP_original_spelling ncP_original_spelling $CT -p yarm-control-plane-servers --lib -- --test-threads=1 vc1c
ncstep NC_hosted_ncR_no_rollback_close ncR_no_rollback_close $CT -p yarm-control-plane-servers --lib -- --test-threads=1 vc1c
ncstep NC_hosted_ncK_split_names_tid ncK_split_names_tid $CT --lib -- --test-threads=1 vc1c_nr21
ncstep NC_live_ncH_handle_as_fd_aarch64 ncH_handle_as_fd env -u CARGO_TARGET_DIR TIMEOUT_SECS=1500 LOGDIR=$O/NCL_H scripts/qemu-vfs-fd-witness-smoke.sh aarch64 1 wide
ncstep NC_live_ncP_original_spelling_aarch64 ncP_original_spelling env -u CARGO_TARGET_DIR TIMEOUT_SECS=1500 LOGDIR=$O/NCL_P scripts/qemu-vfs-fd-witness-smoke.sh aarch64 1 wide
ncstep NC_live_ncS_thread_session_aarch64 ncS_thread_session env -u CARGO_TARGET_DIR TIMEOUT_SECS=1500 LOGDIR=$O/NCL_S scripts/qemu-vfs-fd-witness-smoke.sh aarch64 1 wide
echo "post-NC porcelain=$(git -C $W status --porcelain | grep -v '^?? doc/YARM_' | wc -l) base-porcelain=$(git -C $BW status --porcelain | wc -l) candidate=$(git -C $W rev-parse --short=12 HEAD)" >> $SUM

# ── feature sweep (kernel and servers), last: it is the longest step ──
step F_feature_sweep bash $S/vc1c-evidence/feature_sweep.sh $W $O/F_feature_sweep
freecross $W
echo "vc1c qual $RUN end $(date -u +%FT%TZ)" >> $SUM
echo VC1C_QUAL_DONE >> $SUM
