#!/usr/bin/env bash
# VFS-CORRECT1B qualification, CONTINUATION of an interrupted run (only its unfinished steps): candidate C (committed) vs baseline cfc14b33 (B). Steps run in order,
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

echo "continuation of qual-1: the live negative controls and the epilogue only" >> $SUM
ncstep NC_live_ncT_vfs_truncates_aarch64_cont ncT_vfs_truncates env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/NCL_T scripts/qemu-vfs-mount-witness-smoke.sh aarch64 2 wide
ncstep NC_live_ncS_sender_slot_aarch64_cont ncS_sender_slot env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/NCL_S scripts/qemu-vfs-mount-witness-smoke.sh aarch64 2 wide
echo "post-NC porcelain=$(git -C $W status --porcelain | wc -l) base-porcelain=$(git -C $BW status --porcelain | wc -l) candidate=$(git -C $W rev-parse --short=12 HEAD)" >> $SUM
echo "vc1b qual $RUN end $(date -u +%FT%TZ)" >> $SUM
echo VC1B_QUAL_DONE >> $SUM
