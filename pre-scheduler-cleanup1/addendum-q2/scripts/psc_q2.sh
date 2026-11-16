#!/usr/bin/env bash
# PRE-SCHEDULER-CLEANUP1 qualification addendum Q2: candidate C (code) vs the original F.
# Steps run in order; every log kept; nothing rerun in place.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C=${C:?candidate}
O=$S/psc-evidence/q2
QA=$S/psc-q-wt      # AArch64 default build + core smoke, AArch64 sched-time
QR=$S/psc-dev-wt    # RISC-V and x86_64 sched-time
SUM=$O/summary.txt
mkdir -p $O
step() {
  local name=$1 dir=$2; shift 2
  local por; por=$(git -C $dir status --porcelain | wc -l)
  echo "=== $name [$(git -C $dir rev-parse --short=12 HEAD) porcelain=$por]: $* (start $(date -u +%H:%M:%S))" >> $SUM
  local t0=$SECONDS
  ( cd $dir && "$@" ) > $O/$name.log 2>&1
  local rc=$?
  echo "$name rc=$rc secs=$((SECONDS - t0))" >> $SUM
}
echo "q2 start $(date -u +%FT%TZ) C=$C sha=$(sha256sum $0 | cut -c1-16)" >> $SUM
for d in $QA $QR; do git -C $d checkout -q --detach $C; done
# cheap checks (in QA's clean tree)
step C1_syntax $QA bash -c 'set -e; bash -n scripts/qemu-aarch64-core-smoke.sh; bash -n scripts/qemu-smoke-common.sh; python3 -I -m py_compile scripts/check-aarch64-boot-completion.py scripts/grade-sched-time-witness.py; rustfmt --edition 2024 --check crates/yarm-control-plane-servers/src/control_plane/init/service.rs; echo SYNTAX_FMT_OK'
step C2_completion_self_test $QA python3 -I scripts/check-aarch64-boot-completion.py self-test
step C3_completion_replay $QA python3 -I scripts/check-aarch64-boot-completion.py replay $S/psc-evidence/qual/D2s.boot.log $S/psc-evidence/qual/S4.boot.log
step C4_grader_self_tests $QA bash -c 'set -e; for g in scripts/grade-sched-time-witness.py scripts/grade-futex-time-witness.py scripts/grade-sched-lock1-witness.py; do echo "== $g"; python3 -I $g --self-test; done'
step C5_replay_W3 $QA bash -c "d=$S/psc-evidence/qual/W3_riscv64; python3 -I scripts/grade-sched-time-witness.py --arch riscv64 --smp 2 --core-rc \$(cat \$d/core-smoke.rc) --core-log \$d/core-smoke.log \$d/boot.log; rc=\$?; echo grader_rc=\$rc; test \$rc -ne 0"
step C6_replay_W3b $QA bash -c "d=$S/psc-evidence/qual/W3b_riscv64; python3 -I scripts/grade-sched-time-witness.py --arch riscv64 --smp 2 --core-rc \$(cat \$d/core-smoke.rc) --core-log \$d/core-smoke.log \$d/boot.log; rc=\$?; echo grader_rc=\$rc; test \$rc -ne 0"
# live 1: AArch64 SMP2 default profile, artifact binding enforced (the core smoke checks it in every mode)
step L1_build_aarch64_default $QA env -u CARGO_TARGET_DIR scripts/build-qemu-aarch64-artifacts.sh
step L1_core_aarch64_smp2 $QA env -u CARGO_TARGET_DIR QEMU_SMP=2 LOGFILE=$O/L1.boot.log scripts/qemu-aarch64-core-smoke.sh
step L1_ordered_markers $QA python3 -I scripts/check-aarch64-boot-completion.py check $O/L1.boot.log
# live 2: RISC-V SMP2 sched-time witness
step L2_sched_time_riscv64 $QR env -u CARGO_TARGET_DIR SMP=2 LOGDIR=$O/L2_riscv64 timeout 3600 scripts/qemu-sched-time-witness-smoke.sh riscv64
# the cell is shared: its sched-time profile on the other two ports
step L3_sched_time_x86_64 $QR env -u CARGO_TARGET_DIR SMP=2 LOGDIR=$O/L3_x86_64 timeout 3600 scripts/qemu-sched-time-witness-smoke.sh x86_64
step L4_sched_time_aarch64 $QA env -u CARGO_TARGET_DIR SMP=2 LOGDIR=$O/L4_aarch64 timeout 3600 scripts/qemu-sched-time-witness-smoke.sh aarch64
echo "q2 end $(date -u +%FT%TZ)" >> $SUM
echo Q2_DONE >> $SUM
