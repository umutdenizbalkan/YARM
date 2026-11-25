#!/usr/bin/env bash
# RISCV-STRICT1 qualification: candidate C (committed, from 6d6ae30d) against the IPC-D2-RULE1
# tested candidate P=ccf582fe (evidence E=3b209df1). Steps run in order; every log is kept and
# nothing is rerun in place. Every live RISC-V step is STRICT (QEMU_SMOKE_STRICT=1) and SMP2, as in
# IPC-D2-RULE1 run 2, except the IPC-DISPLACE1 witness and its gate controls (non-strict there too).
# This runner, the helper, its fixtures, the graders and the smoke scripts are hashed into
# predeclared.sha first.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C=${C:?candidate}
RUN=${RUN:?run name}
FROM=${FROM:-H1}
P=ccf582feb8fef6f129195adcff75a5076851be19
E=3b209df1a01a84a03efe2e6c5aa6bde360cb7d87
EQ=ipc-d2-rule1/evidence/qual-2
O=$S/rv1-evidence/$RUN
W=$S/rv1-wt
SUM=$O/summary.txt
mkdir -p $O/archive $O/identity
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
CT="env -u CARGO_TARGET_DIR cargo test -q"
STRICT="env -u CARGO_TARGET_DIR QEMU_SMOKE_STRICT=1"
FD=cap-gen-witness,vfs-correct-witness,vfs-fd-witness
MT=cap-gen-witness,vfs-correct-witness,vfs-mount-witness
echo "rv1 qual $RUN start $(date -u +%FT%TZ) C=$C P=$P E=$E FROM=$FROM runner=$(sha256sum $0 | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $C || { echo "cannot check out $C" >> $SUM; exit 1; }
[[ "$(git -C $W status --porcelain | grep -v '^?? doc/YARM_' | wc -l)" == 0 ]] || { echo "candidate worktree not clean" >> $SUM; exit 1; }
echo "candidate tree $(git -C $W rev-parse HEAD^{tree})" >> $SUM

# ── scope: what changed since the tested candidate P ──
step X_scope bash -c "git diff --name-only $P $C | tee /dev/stderr | grep -vxE 'doc/KERNEL_UNLOCKING.md|scripts/qemu-riscv64-core-smoke.sh|scripts/riscv64-workload-obligations.py|scripts/fixtures/README.md|scripts/fixtures/riscv-workload/(core|fd-witness|mount-witness).txt|src/kernel/boot/tests.rs|tests/artifact_binding_scope.rs|tests/riscv_split_census.rs' && exit 1 || exit 0"

# ── hosted ──
step H1_fmt cargo fmt --all -- --check
step H2_hosted_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
step H3_integration env -u CARGO_TARGET_DIR cargo test -q --tests --no-fail-fast -- --test-threads=1
step H5_test_riscv_census $CT --test riscv_split_census
step H5_test_artifact_binding $CT --test artifact_binding_scope
step H5_test_riscv_rejection $CT --test riscv_core_smoke_capacity_rejection
step H5_test_riscv_regular_scope $CT --test riscv64_regular_smoke_scope
step H5_subset_root_riscv $CT --lib -- --test-threads=1 riscv
step H6_helper_self_test python3 -I scripts/riscv64-workload-obligations.py --self-test
step H6_checker_self_test bash -c "python3 -I scripts/check-d2-publish-race.py --self-test scripts/fixtures/d2-publish-race/*.txt"
step H6_grader_self_test_fd python3 -I scripts/grade-vfs-fd-witness.py --self-test
step H6_grader_self_test_mount python3 -I scripts/grade-vfs-mount-witness.py --self-test
step H6_grader_self_test_correct python3 -I scripts/grade-vfs-correct-witness.py --self-test
step G1_contract_gate bash scripts/check-current-contracts.sh
step G2_doc_enforcement bash scripts/check-contract-doc-enforcement.sh
step G3_proc_vfs_codec_freeze bash scripts/check-proc-vfs-codec-freeze.sh
step G4_service_arch_boundary bash scripts/check-service-arch-boundary.sh
step G5_service_domain_ownership bash scripts/check-service-domain-ownership.sh
step G6_shell_syntax bash -c 'for f in scripts/qemu-riscv64-core-smoke.sh scripts/qemu-vfs-fd-witness-smoke.sh scripts/qemu-vfs-mount-witness-smoke.sh scripts/ipc-displace1-gate-controls.sh; do bash -n $f && echo "ok $f" || { echo "BAD $f"; exit 1; }; done'

# ── archive: the three open IPC-D2-RULE1 verdicts and its default RISC-V core boot, from E ──
if ! skip A_fetch; then
  git -C $W show $E:ipc-d2-rule1/MANIFEST.sha256 > $O/archive/MANIFEST.sha256
  for d in W_fd_strict_smp2_riscv64_a W_fd_strict_smp2_riscv64_b W_1b_strict_smp2_riscv64; do
    mkdir -p $O/archive/$d
    for f in boot.log.gz core-smoke.log.gz core-smoke.rc grade-fd.txt grade-mount.txt grade-correct.txt sha256.txt artifact-binding.txt; do
      git -C $W cat-file -e $E:$EQ/$d/$f 2>/dev/null && git -C $W show $E:$EQ/$d/$f > $O/archive/$d/$f
    done
  done
  mkdir -p $O/archive/S_core_riscv64
  git -C $W show $E:$EQ/S_core_riscv64.boot.log.gz > $O/archive/S_core_riscv64/boot.log.gz
  git -C $W show $E:$EQ/S_core_strict_smp2_riscv64.log.gz > $O/archive/S_core_riscv64/core-smoke.log.gz
  for f in kernels/SHA256SUMS initramfs/SHA256SUMS; do mkdir -p $O/archive/$(dirname $f); git -C $W show $E:$EQ/$f > $O/archive/$f; done
  for d in W_fd_strict_smp2_x86_64_a W_1b_strict_smp2_x86_64 W_fd_strict_smp2_aarch64_a W_1b_strict_smp2_aarch64; do
    mkdir -p $O/archive/$d; git -C $W show $E:$EQ/$d/sha256.txt > $O/archive/$d/sha256.txt
  done
  echo "A_fetch rc=0 (archived files copied verbatim from $E)" >> $SUM
fi
step A_hash bash -c "cd $O/archive && fail=0; for f in \$(find . -type f ! -name MANIFEST.sha256 | sed 's|^\./||' | sort); do h=\$(sha256sum \$f | cut -c1-64); grep -q \"^\$h  evidence/qual-2/\$f\$\" MANIFEST.sha256 && echo \"ok \$f\" || { [[ \$f == S_core_riscv64/* ]] && grep -q \"^\$h  evidence/qual-2/\" MANIFEST.sha256 && echo \"ok \$f\" || { echo \"MISMATCH \$f\"; fail=1; }; }; done; exit \$fail"
if ! skip A_unpack; then
  for d in $O/archive/*/; do [[ -f $d/boot.log.gz ]] && gunzip -kf $d/boot.log.gz; done
  echo "A_unpack rc=0" >> $SUM
fi
# The original verdicts, as recorded (never rewritten): core rc and its printed failures.
step A_original_verdicts bash -c "cd $O/archive && for d in W_fd_strict_smp2_riscv64_a W_fd_strict_smp2_riscv64_b W_1b_strict_smp2_riscv64; do echo \"== \$d core_rc=\$(cat \$d/core-smoke.rc)\"; zcat \$d/core-smoke.log.gz | grep -a '^\[fail\]'; grep -h 'SEAL' \$d/grade-*.txt; done"

# Replay one archived boot log through the REAL RISC-V core smoke of C, against the artifacts now
# in build-riscv64/ (a PATH-free QEMU_BIN shim cats the log), then grade it with the witness graders.
replay() { # name log require marker graders
  local name=$1 log=$2 req=$3 marker=$4 graders=$5 d=$O/$1
  mkdir -p $d/shim
  printf '#!/usr/bin/env bash\ncat %q\nexit 0\n' "$log" > $d/shim/qemu-system-riscv64; chmod +x $d/shim/qemu-system-riscv64
  step $name bash -c "env -u CARGO_TARGET_DIR QEMU_SMOKE_STRICT=1 QEMU_BIN=$d/shim/qemu-system-riscv64 KERNEL_IMAGE=build-riscv64/yarm-riscv64.bin INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio ${req:+ARTIFACT_REQUIRE=\"$req\"} QEMU_SMP=2 LOGFILE=$d/boot.log SMOKE_LOG=$d/smoke.log TIMEOUT_SECS=60 ${marker:+TERMINAL_IDLE_MARKER=$marker} scripts/qemu-riscv64-core-smoke.sh > $d/core-smoke.log 2>&1; rc=\$?; echo \$rc > $d/core-smoke.rc; cmp -s $log $d/boot.log && echo replayed_exact=1 || echo replayed_exact=0; grep -a -m1 ARTIFACT_BINDING $d/core-smoke.log; grep -a -E '^(\[fail\]|RISCV_WORKLOAD|D2_PUBLISH_RACE_CHECK |\[info\] RISCV-STRICT1)' $d/core-smoke.log; tr '\r' '\n' < $d/boot.log > $d/boot.norm.log; for g in $graders; do case \$g in fd) python3 -I scripts/grade-vfs-fd-witness.py --arch riscv64 --smp 2 --core-rc \$rc --wide $d/boot.norm.log > $d/grade-fd.txt;; mount) python3 -I scripts/grade-vfs-mount-witness.py --arch riscv64 --smp 2 --core-rc \$rc --wide $d/boot.norm.log > $d/grade-mount.txt;; correct) python3 -I scripts/grade-vfs-correct-witness.py --arch riscv64 --smp 2 --core-rc \$rc --fixture scripts/fixtures/vfs-fixture.bin --init-rootfs build-riscv64/rootfs/init $d/boot.norm.log > $d/grade-correct.txt;; esac; done; grep -h SEAL $d/grade-*.txt 2>/dev/null; rm -f $d/boot.norm.log; echo core_rc=\$rc; exit \$rc"
}
# identity NAME DIR... : the images in build-riscv64/ (or build-<arch>/) against an archived sha256.txt
ident() { # name arch archived-sha256.txt
  step $1 bash -c "for f in $(kimg $2) build-$2/initramfs-core.cpio build-$2/rootfs/init; do want=\$(grep \"  \$f\$\" $3 | cut -c1-64); have=\$(sha256sum \$f | cut -c1-64); [[ -n \"\$want\" && \"\$want\" == \"\$have\" ]] && echo \"identical \$f \$have\" || { echo \"DIFFERENT \$f have=\$have archived=\$want\"; exit 1; }; done"
}

# ── x86_64 and AArch64: carried forward iff every image they boot is byte-identical to P's ──
for t in x86_64 aarch64; do
  step B_build_C_$t env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh
  step ID_core_$t bash -c "k=\$(grep -a 'ARTIFACT_CARGO_OUTPUT bin=kernel_boot ' $O/B_build_C_$t.log | tail -1 | sed 's/.* path=\([^ ]*\) .*/\1/'); kh=\$(sha256sum \$k | cut -c1-64); ih=\$(sha256sum build-$t/initramfs-core.cpio | cut -c1-64); grep -q \"^\$kh  C-$t.elf\$\" $O/archive/kernels/SHA256SUMS && grep -q \"^\$ih  C-$t-initramfs-core.cpio\$\" $O/archive/initramfs/SHA256SUMS && echo \"identical kernel \$kh initramfs \$ih\""
  xf=""; [[ $t == x86_64 ]] && xf=",timer-contract-witness"
  step B_build_fd_$t env -u CARGO_TARGET_DIR BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $FD$xf" scripts/build-qemu-$t-artifacts.sh
  ident ID_fd_$t $t $O/archive/W_fd_strict_smp2_${t}_a/sha256.txt
  step B_build_1b_$t env -u CARGO_TARGET_DIR BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $MT$xf" scripts/build-qemu-$t-artifacts.sh
  ident ID_1b_$t $t $O/archive/W_1b_strict_smp2_$t/sha256.txt
  freecross $W
done

# ── RISC-V: default core image ──
step B_build_C_riscv64 env -u CARGO_TARGET_DIR scripts/build-qemu-riscv64-artifacts.sh
step ID_core_riscv64 bash -c "k=\$(grep -a 'ARTIFACT_CARGO_OUTPUT bin=kernel_boot ' $O/B_build_C_riscv64.log | tail -1 | sed 's/.* path=\([^ ]*\) .*/\1/'); kh=\$(sha256sum \$k | cut -c1-64); ih=\$(sha256sum build-riscv64/initramfs-core.cpio | cut -c1-64); grep -q \"^\$kh  C-riscv64.elf\$\" $O/archive/kernels/SHA256SUMS && grep -q \"^\$ih  C-riscv64-initramfs-core.cpio\$\" $O/archive/initramfs/SHA256SUMS && echo \"identical kernel \$kh initramfs \$ih\""
step S_core_strict_smp2_riscv64 $STRICT KERNEL_IMAGE=build-riscv64/yarm-riscv64.bin INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio QEMU_SMP=2 LOGFILE=$O/S_core_riscv64.boot.log SMOKE_LOG=$O/S_core_riscv64.smoke.log TIMEOUT_SECS=900 scripts/qemu-riscv64-core-smoke.sh
replay R_core_on_core $O/archive/S_core_riscv64/boot.log "" "" ""
# Wrong image: the fd witness's boot graded against the bound DEFAULT image must fail as before.
replay R_fd_a_on_core $O/archive/W_fd_strict_smp2_riscv64_a/boot.log "" "" ""

# ── RISC-V: fd witness image ──
step W_fd_strict_smp2_riscv64_a $STRICT TIMEOUT_SECS=1500 LOGDIR=$O/W_fd_strict_smp2_riscv64_a scripts/qemu-vfs-fd-witness-smoke.sh riscv64 2 wide
ident ID_fd_riscv64 riscv64 $O/archive/W_fd_strict_smp2_riscv64_a/sha256.txt
replay R_fd_a $O/archive/W_fd_strict_smp2_riscv64_a/boot.log "kernel=$FD initramfs=$FD" VC1C_FD_WITNESS_SEAL "fd correct"
replay R_fd_b $O/archive/W_fd_strict_smp2_riscv64_b/boot.log "kernel=$FD initramfs=$FD" VC1C_FD_WITNESS_SEAL "fd correct"
# Wrong image: the default boot graded against the bound fd-witness image.
replay R_core_on_fd $O/archive/S_core_riscv64/boot.log "kernel=$FD initramfs=$FD" "" ""
step W_fd_strict_smp2_riscv64_b $STRICT TIMEOUT_SECS=1500 LOGDIR=$O/W_fd_strict_smp2_riscv64_b scripts/qemu-vfs-fd-witness-smoke.sh riscv64 2 wide

# ── RISC-V: mount (1B) witness image ──
step W_1b_strict_smp2_riscv64 $STRICT TIMEOUT_SECS=1200 LOGDIR=$O/W_1b_strict_smp2_riscv64 scripts/qemu-vfs-mount-witness-smoke.sh riscv64 2 wide
ident ID_1b_riscv64 riscv64 $O/archive/W_1b_strict_smp2_riscv64/sha256.txt
replay R_1b $O/archive/W_1b_strict_smp2_riscv64/boot.log "kernel=$MT initramfs=$MT" VC1B_END "mount correct"
# Wrong images: the default boot and the fd boot graded against the bound mount-witness image.
replay R_core_on_1b $O/archive/S_core_riscv64/boot.log "kernel=$MT initramfs=$MT" "" ""
replay R_fd_a_on_1b $O/archive/W_fd_strict_smp2_riscv64_a/boot.log "kernel=$MT initramfs=$MT" "" ""

# ── RISC-V: the IPC-DISPLACE1 witness and its gate controls also run through this smoke ──
step I_ipcd1_smp2_riscv64 env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/I_ipcd1_smp2_riscv64 scripts/qemu-ipc-displace1-witness-smoke.sh riscv64 2
step GC_gate_controls_riscv64 bash scripts/ipc-displace1-gate-controls.sh riscv64 2 $O/I_ipcd1_smp2_riscv64/boot.log $O/GC_gate_controls_riscv64
freecross $W
echo "rv1 qual $RUN end $(date -u +%FT%TZ)" >> $SUM
echo RV1_QUAL_DONE >> $SUM
