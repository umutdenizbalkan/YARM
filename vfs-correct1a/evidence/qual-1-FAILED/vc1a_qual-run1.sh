#!/usr/bin/env bash
# VFS-CORRECT1A qualification (run 1): candidate C vs baseline 1c2b8dfc (B). Steps in order, every
# log kept, nothing rerun in place. NC mutations: $S/vc1a-nc/nc.py (hashed with this runner, the
# grader and the smoke script in predeclared.sha).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C=${C:?candidate}
B=1c2b8dfc
O=$S/vc1a-evidence/qual
W=$S/vc1a-wt
BW=$S/vc1a-basewt
SUM=$O/summary.txt
NC="python3 -I $S/vc1a-nc/nc.py"
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
kpath() { grep -a "ARTIFACT_CARGO_OUTPUT bin=kernel_boot " "$1" | tail -1 | sed 's/.* path=\([^ ]*\) .*/\1/'; }
kimg() { case $1 in x86_64) echo build-x86_64/kernel_boot.elf;; aarch64) echo build-aarch64/yarm-aarch64.bin;; riscv64) echo build-riscv64/yarm-riscv64.bin;; esac; }
echo "vc1a qual start $(date -u +%FT%TZ) C=$C B=$B sha=$(sha256sum $0 | cut -c1-16) nc=$(sha256sum $S/vc1a-nc/nc.py | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $C
[[ "$(git -C $BW rev-parse --short=8 HEAD)" == "$B" ]] || { echo "baseline worktree not at $B" >> $SUM; exit 1; }
[[ "$(git -C $BW status --porcelain | wc -l)" == 0 ]] || { echo "baseline worktree not clean" >> $SUM; exit 1; }

# ── hosted ──
step H1_fmt cargo fmt --all -- --check
step H2_hosted_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
step H3_integration env -u CARGO_TARGET_DIR cargo test -q --tests --no-fail-fast -- --test-threads=1
for p in yarm-ipc-abi yarm-user-rt yarm-fs-servers yarm-srv-common yarm-kernel; do
  step H4_crate_$p env -u CARGO_TARGET_DIR cargo test -q -p $p --lib -- --test-threads=1
done
step H4_subset_ipc_abi_vc1a env -u CARGO_TARGET_DIR cargo test -q -p yarm-ipc-abi --lib -- --test-threads=1 vc1a
step H4_subset_user_rt_vfs_client env -u CARGO_TARGET_DIR cargo test -q -p yarm-user-rt --lib -- --test-threads=1 vfs_client
step H4_subset_fs_servers_vc1a env -u CARGO_TARGET_DIR cargo test -q -p yarm-fs-servers --lib -- --test-threads=1 vc1a
step H4_subset_fs_servers_stage92 env -u CARGO_TARGET_DIR cargo test -q -p yarm-fs-servers --lib -- --test-threads=1 stage92
step H4_subset_root_syscall_abi env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 syscall_abi
step H4_subset_root_grant env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 grant_ro
step H4_grader_self_test python3 -I scripts/grade-vfs-correct-witness.py --self-test
step G1_contract_gate bash scripts/check-current-contracts.sh
step G2_doc_enforcement bash scripts/check-contract-doc-enforcement.sh
step G3_proc_vfs_codec_freeze bash scripts/check-proc-vfs-codec-freeze.sh
for a in x86_64 aarch64 riscv64; do
  step X_check_default_kernel_$a env -u CARGO_TARGET_DIR cargo check -q --target targets/$a-yarm-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features -p yarm --bin kernel_boot
  step X_check_default_init_$a env -u CARGO_TARGET_DIR cargo check -q --target targets/$a-yarm-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features -p yarm-control-plane-servers --bin init_server
  step X_check_witness_init_$a env -u CARGO_TARGET_DIR cargo check -q --target targets/$a-yarm-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features --features vfs-correct-witness -p yarm-control-plane-servers --bin init_server
done

# ── builds and core smokes (default artifacts) ──
for t in x86_64 aarch64 riscv64; do
  to=900
  step B_build_C_$t env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh
  k=$(kpath $O/B_build_C_$t.log); [[ -n "$k" ]] && cp "$k" $O/kernels/C-$t.elf
  cp $W/build-$t/initramfs-core.cpio $O/initramfs/C-$t-initramfs-core.cpio
  step S_gp_core_smp2_$t env KERNEL_IMAGE=$(kimg $t) INITRAMFS_IMAGE=build-$t/initramfs-core.cpio QEMU_SMP=2 LOGFILE=$O/S_gp_$t.boot.log SMOKE_LOG=$O/S_gp_$t.smoke.log TIMEOUT_SECS=$to scripts/qemu-$t-core-smoke.sh
  step B_build_B_$t bash -c "cd $BW && env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh"
  k=$(kpath $O/B_build_B_$t.log); [[ -n "$k" ]] && cp "$k" $O/kernels/B-$t.elf
  cp $BW/build-$t/initramfs-core.cpio $O/initramfs/B-$t-initramfs-core.cpio
done
( cd $O/kernels && sha256sum *.elf > SHA256SUMS ); ( cd $O/initramfs && sha256sum *.cpio > SHA256SUMS )

# ── live witness, every port ──
for t in x86_64 aarch64 riscv64; do
  step W_vfs_$t env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/W_vfs_$t scripts/qemu-vfs-correct-witness-smoke.sh $t 2
done

# ── negative controls ──
P=crates/yarm-user-rt/src/vfs_client.rs
step NC_hosted_ncA_count_without_copy bash -c "$NC ncA_count_without_copy && git diff --stat && env -u CARGO_TARGET_DIR cargo test -q -p yarm-user-rt --lib -- --test-threads=1 vfs_client; rc=\$?; git checkout -- $P; exit \$rc"
step NC_hosted_ncB_receive_after_failed_send bash -c "$NC ncB_receive_after_failed_send && git diff --stat && env -u CARGO_TARGET_DIR cargo test -q -p yarm-user-rt --lib -- --test-threads=1 vfs_client; rc=\$?; git checkout -- $P; exit \$rc"
Q=crates/yarm-fs-servers/src/fs/common/vfs_service.rs
step NC_hosted_ncC_uncapped_backend_read bash -c "$NC ncC_uncapped_backend_read && git diff --stat && env -u CARGO_TARGET_DIR cargo test -q -p yarm-fs-servers --lib -- --test-threads=1 vc1a; rc=\$?; git checkout -- $Q; exit \$rc"
step NC_live_ncA_count_without_copy_aarch64 bash -c "$NC ncA_count_without_copy && git diff --stat && env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/NCLA scripts/qemu-vfs-correct-witness-smoke.sh aarch64 2; rc=\$?; git checkout -- $P; exit \$rc"
echo "post-NC porcelain=$(git -C $W status --porcelain | wc -l) base-porcelain=$(git -C $BW status --porcelain | wc -l)" >> $SUM
echo "vc1a qual end $(date -u +%FT%TZ)" >> $SUM
echo VC1A_QUAL_DONE >> $SUM
