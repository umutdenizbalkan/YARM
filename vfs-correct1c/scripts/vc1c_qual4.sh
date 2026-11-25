#!/usr/bin/env bash
# VFS-CORRECT1C qualification run 4: the test-only commit C4 (76c74efb) after run 3 on C3 (d9becda1).
# C4 changes only src/kernel/boot/tests.rs (cfg(test)). This run: (1) the hosted steps that compile
# that file, and the ncK control that run 3 found uncaught; (2) proof that every artifact the run-3
# live steps booted is byte-identical when built from C4: the default kernel and core initramfs per
# port (run 3 B_build_C), and the wide fd-witness kernel, initramfs and init per port (run 3
# W_fd_wide_smp1_<port>, which R_race_smp2_<port> and NCL share at C3). If every hash matches,
# run 3's live results stand for C4 unchanged.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C=76c74efbaf7b275a86d2c6dcdddd445da894e333
O=$S/vc1c-evidence/qual-4
Q3=$S/vc1c-evidence/qual-3
W=$S/vc1c-wt
NCW=$S/vc1c-nc-wt
SUM=$O/summary.txt
mkdir -p $O
echo "vc1c qual qual-4 start $(date -u +%FT%TZ) C=$C runner=$(sha256sum $0 | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $C || { echo "cannot check out $C" >> $SUM; exit 1; }
[[ "$(git -C $W status --porcelain | grep -v '^?? doc/YARM_' | wc -l)" == 0 ]] || { echo "candidate worktree not clean" >> $SUM; exit 1; }
echo "candidate tree $(git -C $W rev-parse HEAD^{tree}); diff vs run-3 C3: $(git -C $W diff --name-only d9becda16831ea0b58d70ce924a4c28855440f67 $C | tr '\n' ' ')" >> $SUM
step() {
  local name=$1; shift
  echo "=== $name [$(git -C $W rev-parse --short=12 HEAD) porcelain=$(git -C $W status --porcelain | grep -v '^?? doc/YARM_' | wc -l)]: $* (start $(date -u +%H:%M:%S))" >> $SUM
  local t0=$SECONDS
  ( cd $W && "$@" ) > $O/$name.log 2>&1
  echo "$name rc=$? secs=$((SECONDS - t0)) free=$(df -h /tmp | awk 'NR==2{print $4}')" >> $SUM
}
step H1_fmt cargo fmt --all -- --check
step H2_hosted_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
step H3_integration env -u CARGO_TARGET_DIR cargo test -q --tests --no-fail-fast -- --test-threads=1
step H5_subset_root_vc1c_nr21 env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 vc1c_nr21
# ncK in its own worktree at C4
wt=$NCW/NC_hosted_ncK_split_names_tid
rm -rf $wt; git -C $W worktree prune
git -C $W worktree add -q --detach $wt $C >> $O/NC_hosted_ncK_split_names_tid.log 2>&1
echo "=== NC_hosted_ncK_split_names_tid [worktree $wt at $(git -C $wt rev-parse --short=12 HEAD)] (start $(date -u +%H:%M:%S))" >> $SUM
t0=$SECONDS
( cd $wt && python3 -I $S/vc1c-nc/nc.py ncK_split_names_tid && git diff && env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 vc1c_nr21 ) >> $O/NC_hosted_ncK_split_names_tid.log 2>&1
echo "NC_hosted_ncK_split_names_tid rc=$? secs=$((SECONDS - t0)) mutated_files=$(git -C $wt diff --name-only | tr '\n' ' ')" >> $SUM
rm -rf $wt/target
# artifact identity
kpath() { grep -a "ARTIFACT_CARGO_OUTPUT bin=kernel_boot " "$1" | tail -1 | sed 's/.* path=\([^ ]*\) .*/\1/'; }
img() { case $1 in x86_64) echo build-x86_64/kernel_boot.elf;; aarch64) echo build-aarch64/yarm-aarch64.bin;; riscv64) echo build-riscv64/yarm-riscv64.bin;; esac; }
for t in x86_64 aarch64 riscv64; do
  step A_build_default_$t env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh
  k=$(kpath $O/A_build_default_$t.log)
  a=$(sha256sum "$k" | cut -c1-64); b=$(grep " C-$t.elf" $Q3/kernels/SHA256SUMS | cut -c1-64)
  ai=$(sha256sum $W/build-$t/initramfs-core.cpio | cut -c1-64); bi=$(grep " C-$t-initramfs-core.cpio" $Q3/initramfs/SHA256SUMS | cut -c1-64)
  echo "IDENTITY default $t kernel c4=$a run3=$b same=$([[ $a == $b ]] && echo 1 || echo 0) initramfs c4=$ai run3=$bi same=$([[ $ai == $bi ]] && echo 1 || echo 0)" >> $SUM
  step A_build_fdwide_$t env -u CARGO_TARGET_DIR BOOTSTRAP_FEATURE_ARGS="--no-default-features --features cap-gen-witness,vfs-correct-witness,vfs-fd-witness" scripts/build-qemu-$t-artifacts.sh
  for f in $(img $t) build-$t/initramfs-core.cpio build-$t/rootfs/init; do
    a=$(sha256sum $W/$f | cut -c1-64); b=$(grep " $f\$" $Q3/W_fd_wide_smp1_$t/sha256.txt | cut -c1-64)
    echo "IDENTITY fdwide $t $f c4=$a run3=$b same=$([[ -n $b && $a == $b ]] && echo 1 || echo 0)" >> $SUM
  done
  rm -rf $W/target/x86_64-yarm-none $W/target/aarch64-yarm-none $W/target/riscv64-yarm-none $W/target/*-yarm-user-none $W/target/riscv64gc-unknown-none-elf
done
echo "vc1c qual qual-4 end $(date -u +%FT%TZ)" >> $SUM
echo VC1C_QUAL_DONE >> $SUM
