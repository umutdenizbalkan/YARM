#!/usr/bin/env bash
# IPC-D2-RULE1 qualification: candidate C (committed) against dev 4b7b71e0 (B). Steps run in order;
# every log is kept and nothing is rerun in place. A failed or interrupted run is kept under its own
# directory with its disposition. Every live step is STRICT (QEMU_SMOKE_STRICT=1) and SMP2.
# This runner, the checker, its fixtures, the graders and the smoke scripts are hashed into
# predeclared.sha first.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C=${C:?candidate}
RUN=${RUN:?run name}
FROM=${FROM:-H1}
B=4b7b71e0
O=$S/d2r1-evidence/$RUN
W=$S/d2p1-wt
BW=$S/vc1c-wt
SUM=$O/summary.txt
EV=35267905
mkdir -p $O/kernels $O/initramfs $O/replay
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
kpath() { grep -a "ARTIFACT_CARGO_OUTPUT bin=kernel_boot " "$1" | tail -1 | sed 's/.* path=\([^ ]*\) .*/\1/'; }
kimg() { case $1 in x86_64) echo build-x86_64/kernel_boot.elf;; aarch64) echo build-aarch64/yarm-aarch64.bin;; riscv64) echo build-riscv64/yarm-riscv64.bin;; esac; }
XCHK="env -u CARGO_TARGET_DIR cargo check -q -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features"
CT="env -u CARGO_TARGET_DIR cargo test -q"
STRICT="env -u CARGO_TARGET_DIR QEMU_SMOKE_STRICT=1"
echo "d2r1 qual $RUN start $(date -u +%FT%TZ) C=$C B=$B FROM=$FROM runner=$(sha256sum $0 | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $C || { echo "cannot check out $C" >> $SUM; exit 1; }
[[ "$(git -C $W status --porcelain | grep -v '^?? doc/YARM_' | wc -l)" == 0 ]] || { echo "candidate worktree not clean" >> $SUM; exit 1; }
[[ "$(git -C $BW rev-parse --short=8 HEAD)" == "$B" ]] || { echo "baseline worktree not at $B" >> $SUM; exit 1; }
echo "candidate tree $(git -C $W rev-parse HEAD^{tree})" >> $SUM

# ── hosted ──
step H1_fmt cargo fmt --all -- --check
step H2_hosted_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
step H3_integration env -u CARGO_TARGET_DIR cargo test -q --tests --no-fail-fast -- --test-threads=1
for p in yarm-ipc-abi yarm-user-rt yarm-fs-servers yarm-srv-common yarm-kernel yarm-control-plane-servers; do
  step H4_crate_$p $CT -p $p --lib -- --test-threads=1
done
step H4B_baseline_control_plane bash -c "cd $BW && env -u CARGO_TARGET_DIR cargo test -q -p yarm-control-plane-servers --lib -- --test-threads=1"
for f in ipc_displace1 u9rx3 recv d2 direct timeout teardown sched lock_order stage114 stage115 stage116 d2rule1 smoke; do
  step H5_subset_root_$f $CT --lib -- --test-threads=1 $f
done
step H5_test_d2_checker $CT --test d2_publish_race_checker
step H5_test_riscv_rejection $CT --test riscv_core_smoke_capacity_rejection
step H6_checker_self_test bash -c "python3 -I scripts/check-d2-publish-race.py --self-test scripts/fixtures/d2-publish-race/*.txt"
step H6_grader_self_test_fd python3 -I scripts/grade-vfs-fd-witness.py --self-test
step H6_grader_self_test_mount python3 -I scripts/grade-vfs-mount-witness.py --self-test
step H6_grader_self_test_correct python3 -I scripts/grade-vfs-correct-witness.py --self-test
step G1_contract_gate bash scripts/check-current-contracts.sh
step G2_doc_enforcement bash scripts/check-contract-doc-enforcement.sh
step G3_proc_vfs_codec_freeze bash scripts/check-proc-vfs-codec-freeze.sh
step G4_service_arch_boundary bash scripts/check-service-arch-boundary.sh
step G5_service_domain_ownership bash scripts/check-service-domain-ownership.sh
step G6_shell_syntax bash -c 'for f in scripts/qemu-x86_64-core-smoke.sh scripts/qemu-aarch64-core-smoke.sh scripts/qemu-riscv64-core-smoke.sh scripts/qemu-x86_64-optional-fs-smoke.sh scripts/qemu-aarch64-optional-fs-smoke.sh scripts/qemu-vfs-fd-witness-smoke.sh; do bash -n $f && echo "ok $f" || { echo "BAD $f"; exit 1; }; done'
for a in x86_64 aarch64 riscv64; do
  T="--target targets/$a-yarm-none.json"
  U="--target targets/$a-yarm-user-none.json"
  step X_check_default_kernel_$a $XCHK $T -p yarm --bin kernel_boot
  step X_check_default_servers_$a $XCHK $U -p yarm-control-plane-servers -p yarm-fs-servers --bins
done
freecross $W

# ── replay: the six archived VFS-CORRECT1C SMP2 events (evidence 35267905) ──
if ! skip R_fetch; then
  for t in x86_64 aarch64 riscv64; do
    p=vfs-correct1c/evidence/qual-3/R_race_smp2_$t
    mkdir -p $O/replay/$t
    for f in boot.log.gz grade-fd.txt grade-correct.txt core-smoke.rc; do git -C $W show $EV:$p/$f > $O/replay/$t/$f; done
    git -C $W show $EV:vfs-correct1c/MANIFEST.sha256 | grep "qual-3/R_race_smp2_$t/boot.log.gz" > $O/replay/$t/manifest_line.txt
    gunzip -c $O/replay/$t/boot.log.gz | tr '\r' '\n' > $O/replay/$t/boot.norm.log
  done
  echo "R_fetch rc=0 (archived 1C verdicts copied verbatim: grade-fd.txt, grade-correct.txt, core-smoke.rc)" >> $SUM
fi
for t in x86_64 aarch64 riscv64; do
  step R_hash_$t bash -c "cd $O/replay/$t && h=\$(sha256sum boot.log.gz | cut -c1-64) && grep -q \"^\$h \" manifest_line.txt && echo \"hash \$h matches the 1C manifest\""
  step R_checker_$t python3 -I scripts/check-d2-publish-race.py $O/replay/$t/boot.norm.log
  step R_fdgrade_original_core_$t bash -c "python3 -I scripts/grade-vfs-fd-witness.py --arch $t --smp 2 --core-rc \$(cat $O/replay/$t/core-smoke.rc) --wide $O/replay/$t/boot.norm.log"
  step R_fdgrade_rules_$t python3 -I scripts/grade-vfs-fd-witness.py --arch $t --smp 2 --core-rc 0 --wide $O/replay/$t/boot.norm.log
done

# ── live, strict, SMP2, per port ──
for t in aarch64 x86_64 riscv64; do
  step B_build_C_$t env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh
  if ! skip B_copy_C_$t; then
    k=$(kpath $O/B_build_C_$t.log); [[ -n "$k" ]] && sha256sum "$k" | sed "s#  .*#  C-$t.elf#" >> $O/kernels/SHA256SUMS
    sha256sum $W/build-$t/initramfs-core.cpio | sed "s#  .*#  C-$t-initramfs-core.cpio#" >> $O/initramfs/SHA256SUMS
  fi
  # The strict x86_64 core smoke grades the timer-contract witness: build it CI-exact
  # (.github/workflows/core-qemu-smoke.yml, x86_64-core-smoke-strict).
  if [[ $t == x86_64 ]]; then
    step B_build_C_strict_$t env -u CARGO_TARGET_DIR ARTIFACTS_STRICT=1 BOOTSTRAP_FEATURE_ARGS="--no-default-features --features timer-contract-witness" scripts/build-qemu-$t-artifacts.sh
  fi
  step S_core_strict_smp2_$t $STRICT KERNEL_IMAGE=$(kimg $t) INITRAMFS_IMAGE=build-$t/initramfs-core.cpio QEMU_SMP=2 LOGFILE=$O/S_core_$t.boot.log SMOKE_LOG=$O/S_core_$t.smoke.log TIMEOUT_SECS=900 scripts/qemu-$t-core-smoke.sh
  step W_fd_strict_smp2_${t}_a $STRICT TIMEOUT_SECS=1500 LOGDIR=$O/W_fd_strict_smp2_${t}_a scripts/qemu-vfs-fd-witness-smoke.sh $t 2 wide
  step W_fd_strict_smp2_${t}_b $STRICT TIMEOUT_SECS=1500 LOGDIR=$O/W_fd_strict_smp2_${t}_b scripts/qemu-vfs-fd-witness-smoke.sh $t 2 wide
  step W_1b_strict_smp2_$t $STRICT TIMEOUT_SECS=1200 LOGDIR=$O/W_1b_strict_smp2_$t scripts/qemu-vfs-mount-witness-smoke.sh $t 2 wide
  # Extra to the brief: the IPC-DISPLACE1 witness and its gate controls, non-strict on every port
  # (their artifacts carry no timer contract), replaying fixture mutants through the real smokes.
  step I_ipcd1_smp2_$t env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/I_ipcd1_smp2_$t scripts/qemu-ipc-displace1-witness-smoke.sh $t 2
  step GC_gate_controls_$t bash scripts/ipc-displace1-gate-controls.sh $t 2 $O/I_ipcd1_smp2_$t/boot.log $O/GC_gate_controls_$t
  freecross $W
done
echo "d2r1 qual $RUN end $(date -u +%FT%TZ)" >> $SUM
echo D2R1_QUAL_DONE >> $SUM
