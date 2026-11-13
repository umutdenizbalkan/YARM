#!/usr/bin/env bash
# FUTEX-TIME1 frozen-candidate qualification (schedule: ft1_schedule.md).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=${F:?}
WT=$S/ft1-wt BWT=$S/ea7-wt
OUT=${OUT:-$S/ft1-evidence/qual}
mkdir -p "$OUT"
SUMMARY=$OUT/summary.txt BIND=$OUT/bindings.txt
# Addendum B: wait for the strengthened candidate F' to be checked out in $WT.
until [ -e $S/ft1_matrix.go ]; do sleep 30; done
F=$(cat $S/ft1_fprime)

step() {  # step <id> <dir> <cmd...>
  local id=$1 dir=$2; shift 2
  grep -q "^$id rc=" "$SUMMARY" 2>/dev/null && return 0
  [ -e "$OUT/$id.log" ] && mv "$OUT/$id.log" "$OUT/$id.log.interrupted.$(date +%s)"
  local t0; t0=$(date +%s)
  echo "=== $id [$(git -C "$dir" rev-parse --short=12 HEAD) porcelain=$(git -C "$dir" status --porcelain | wc -l)]: $* (start $(date -u +%T))" >> "$SUMMARY"
  ( cd "$dir" && "$@" ) > "$OUT/$id.log" 2>&1
  local rc=$?
  echo "$id rc=$rc secs=$(( $(date +%s) - t0 ))" >> "$SUMMARY"
  grep -ah 'ARTIFACT_BINDING' "$OUT/$id.log" 2>/dev/null | sed "s/^/$id /" >> "$BIND" || true
  return 0
}
hosted() { CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/ft1q-target "$@"; }
hosted_b() { CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/ea7-target "$@"; }
oracle_snap() { ls -d /tmp/yarm-oracle-* 2>/dev/null | sort; }
keep_oracle() {
  local tag=$1 before=$2 d
  for d in $(comm -13 <(echo "$before") <(oracle_snap)); do cp -a "$d" "$OUT/oracle-$tag-$(basename "$d")"; done
}

ONLY=${ONLY:-all}
on() { [[ $ONLY == all || " $ONLY " == *" $1 "* ]]; }
echo "matrix start $(date -u +%FT%TZ) only=$ONLY schedule_sha=$(sha256sum $S/ft1_schedule.md | cut -c1-16)" >> "$SUMMARY"

if on prov; then
  step P0_provenance "$WT" bash -c "git rev-parse HEAD; test \$(git rev-parse --short=8 HEAD) = $F && echo HEAD_IS_F; git status --porcelain | wc -l; rustc -V; cargo -V; qemu-system-x86_64 --version | head -1; qemu-system-aarch64 --version | head -1; qemu-system-riscv64 --version | head -1"
  step P1_source "$WT" bash -c "git diff --stat 6ba03af9 HEAD; git diff --stat 6ba03af9 HEAD -- src crates ':!src/kernel/boot/tests.rs'; git log --oneline 6ba03af9..HEAD; grep -n 'FUTEX_FLAG_TIMED\\|FUTEX_WAIT_FLAGS_DEFINED' src/kernel/boot/futex.rs | head; grep -n 'commit_x86_syscall_returns' src/arch/x86_64/trap.rs src/kernel/trapframe.rs; true"
fi
if on hosted; then
  step H1_fmt "$WT" cargo fmt --all -- --check
  step H2_clippy "$WT" hosted cargo clippy --lib --features hosted-dev
  step H3_hosted_lib "$WT" hosted cargo test --lib -- --test-threads=1
  step H4_integration "$WT" hosted cargo test --test '*' --no-fail-fast -- --test-threads=1
  for f in time1 ecall_contract riscv_park_ret1 riscv64_async_preemption stage184 stage199d futex_requeue1::wait_atomic1 futex_requeue1::atomic1 futex_requeue1 futex_wake_cap1 futex_found1 futex trapframe async riscv u6 blocked recv send scheduler sched_ ap_sched1 syscall kthread exit spawn thread fork ipc_displace1 sender_wake; do
    step "H5_$f" "$WT" hosted cargo test --lib "$f" -- --test-threads=1
  done
  step H2b_clippy_classes_B "$BWT" bash -c "$S/ft1_clipcmp.sh"
  step H6_lockorder_attr "$WT" bash -c "$S/ft1_lockattr.sh"
  step G1_contract_gate_F "$WT" env CARGO_TARGET_DIR=$S/ft1q-target bash scripts/check-contract-doc-enforcement.sh
  step G2_contract_gate_B "$BWT" env CARGO_TARGET_DIR=$S/ea7-target bash scripts/check-contract-doc-enforcement.sh
  step G3_zscan_self_test "$WT" bash -c "$S/fwa_zscan.sh --self-test"
  step G4_grader_self_tests "$WT" bash -c "python3 -I scripts/grade-futex-wait-witness.py --self-test && python3 -I scripts/grade-futex-requeue-witness.py --self-test && python3 -I scripts/grade-riscv-ecall-witness.py --self-test && python3 -I scripts/grade-futex-time-witness.py --self-test"
fi
if on default; then
  step D1_build_x86_64 "$WT" scripts/build-qemu-x86_64-artifacts.sh
  step D2_build_aarch64 "$WT" scripts/build-qemu-aarch64-artifacts.sh
  step D3_build_riscv64 "$WT" scripts/build-qemu-riscv64-artifacts.sh
  step O1_oracle_x86_64 "$WT" scripts/qemu-ipc-recv-v2-oracle-smoke.sh x86_64
  step O2_oracle_aarch64 "$WT" scripts/qemu-ipc-recv-v2-oracle-smoke.sh aarch64
  step O3_oracle_riscv64 "$WT" scripts/qemu-ipc-recv-v2-oracle-smoke.sh riscv64
fi
if on uartnc; then
  step UN0a_record_x86_64_smp2 "$WT" bash -c "scripts/build-qemu-x86_64-artifacts.sh >/dev/null 2>&1 && env QEMU_SMP=2 LOGFILE=$OUT/UN0a_x86_64_smp2.boot.log scripts/qemu-x86_64-core-smoke.sh"
  step UN0b_record_aarch64_smp2 "$WT" bash -c "scripts/build-qemu-aarch64-artifacts.sh >/dev/null 2>&1 && env QEMU_SMP=2 LOGFILE=$OUT/UN0b_aarch64_smp2.boot.log scripts/qemu-aarch64-core-smoke.sh"
  step UN_damaged_marker_control "$WT" env UNC_OUT=$OUT/UN UNC_X86_LOG=$OUT/UN0a_x86_64_smp2.boot.log UNC_A64_LOG=$OUT/UN0b_aarch64_smp2.boot.log bash -c "$S/ft1_uart_nc.sh"
fi
if on strict; then
  step S1_build_x86_64_strict "$WT" env ARTIFACTS_STRICT=1 BOOTSTRAP_FEATURE_ARGS="--no-default-features --features timer-contract-witness" scripts/build-qemu-x86_64-artifacts.sh
  step S2_core_x86_64_strict "$WT" env QEMU_SMOKE_STRICT=1 LOGFILE="$OUT/S2_qemu-x86_64-core.log" scripts/qemu-x86_64-core-smoke.sh
  step S3_build_aarch64_strict "$WT" env ARTIFACTS_STRICT=1 scripts/build-qemu-aarch64-artifacts.sh
  step S4_core_aarch64_strict "$WT" env QEMU_SMOKE_STRICT=1 scripts/qemu-aarch64-core-smoke.sh
  step S5_build_riscv64_strict "$WT" env ARTIFACTS_STRICT=1 scripts/build-qemu-riscv64-artifacts.sh
  step S6_core_riscv64_strict "$WT" env QEMU_SMOKE_STRICT=1 scripts/qemu-riscv64-core-smoke.sh
  step FX1_first_cohort_seal "$WT" env LOGDIR="$OUT/FX1" scripts/qemu-first-cohort-retirement-seal.sh
  step S7_ci_timer_grep "$WT" bash -c "for m in YARM_TIMER_IRQ_DELIVERED YARM_TIMER_EOI_DONE YARM_SCHED_TICK; do echo \"\$m \$(grep -ac \$m $OUT/S2_qemu-x86_64-core.log)\"; done; grep -aq YARM_TIMER_IRQ_DELIVERED $OUT/S2_qemu-x86_64-core.log && grep -aq YARM_TIMER_EOI_DONE $OUT/S2_qemu-x86_64-core.log && grep -aq YARM_SCHED_TICK $OUT/S2_qemu-x86_64-core.log"
fi
if on witness; then
  for a in x86_64 aarch64 riscv64; do
    step "W_${a}_smp1" "$WT" env LOGDIR="$OUT/W_${a}_smp1" timeout 2400 scripts/qemu-ipc-displace1-witness-smoke.sh "$a" 1
  done
fi
if on futex; then
  step FX2_overtaken_x86_64 "$WT" env LOGDIR="$OUT/FX2" scripts/qemu-x86_64-overtaken-witness-smoke.sh
  step FX3_overtaken_aarch64 "$WT" env LOGDIR="$OUT/FX3" scripts/qemu-aarch64-overtaken-witness-smoke.sh
  for a in x86_64 aarch64 riscv64; do
    step "FX4_apsched_${a}" "$WT" env SMP=2 LOGDIR="$OUT/FX4_${a}" scripts/qemu-${a}-apsched-witness-smoke.sh
    step "FX5_kthread1_${a}" "$WT" env ARCH=$a SMP=2 LOGDIR="$OUT/FX5_${a}" scripts/qemu-kthread1-witness-smoke.sh
    step "FX6_schedlock1_${a}" "$WT" env ARCH=$a SMP=2 LOGDIR="$OUT/FX6_${a}" scripts/qemu-sched-lock1-witness-smoke.sh
  done
fi
if on requeue; then
  for a in x86_64 aarch64 riscv64; do
    step "FX7_requeue_${a}" "$WT" env LOGDIR="$OUT/FX7_${a}" timeout 3000 scripts/qemu-futex-requeue-witness-smoke.sh "$a" 2
    step "FX8_race_${a}" "$WT" env RACE=1 REQUIRE_CROSS_CPU=1 LOGDIR="$OUT/FX8_${a}" timeout 3000 scripts/qemu-futex-requeue-witness-smoke.sh "$a" 2
  done
fi
if on rsend; then
  step R1_d2_settle_riscv64_F "$WT" env LOGDIR="$OUT/R1_F" timeout 2400 scripts/qemu-riscv64-d2-settle-witness-smoke.sh
  step R1_d2_settle_riscv64_B "$BWT" env LOGDIR="$OUT/R1_B" timeout 2400 scripts/qemu-riscv64-d2-settle-witness-smoke.sh
  for a in riscv64 x86_64 aarch64; do
    step "R2_send_fault_${a}_F" "$WT" env LOGDIR="$OUT/R2_${a}_F" timeout 2400 scripts/qemu-ipc-send-final-fault-witness-smoke.sh "$a"
    step "R3_queued_cap_${a}_F" "$WT" env LOGDIR="$OUT/R3_${a}_F" timeout 2400 scripts/qemu-ipc-residual1-queued-cap-witness-smoke.sh "$a"
  done
  step R2_send_fault_riscv64_B "$BWT" env LOGDIR="$OUT/R2_riscv64_B" timeout 2400 scripts/qemu-ipc-send-final-fault-witness-smoke.sh riscv64
  step R3_queued_cap_riscv64_B "$BWT" env LOGDIR="$OUT/R3_riscv64_B" timeout 2400 scripts/qemu-ipc-residual1-queued-cap-witness-smoke.sh riscv64
fi
if on futexb; then
  step FB0a_build_x86_64_strict_B "$BWT" env ARTIFACTS_STRICT=1 BOOTSTRAP_FEATURE_ARGS="--no-default-features --features timer-contract-witness" scripts/build-qemu-x86_64-artifacts.sh
  step FB0b_build_aarch64_strict_B "$BWT" env ARTIFACTS_STRICT=1 scripts/build-qemu-aarch64-artifacts.sh
  step FB0c_build_riscv64_strict_B "$BWT" env ARTIFACTS_STRICT=1 scripts/build-qemu-riscv64-artifacts.sh
  step FB1_first_cohort_seal_B "$BWT" env LOGDIR="$OUT/FB1" scripts/qemu-first-cohort-retirement-seal.sh
  step FB2_overtaken_x86_64_B "$BWT" env LOGDIR="$OUT/FB2" scripts/qemu-x86_64-overtaken-witness-smoke.sh
  step FB3_overtaken_aarch64_B "$BWT" env LOGDIR="$OUT/FB3" scripts/qemu-aarch64-overtaken-witness-smoke.sh
fi
if on uart; then
  step U1_uart_irq_x86_64 "$WT" env LOGDIR="$OUT/U1" scripts/qemu-x86_64-uart-irq-witness-smoke.sh
  step U2_pl011_irq_aarch64 "$WT" env LOGDIR="$OUT/U2" scripts/qemu-aarch64-pl011-irq-witness-smoke.sh
  step U3_uart_irq_riscv64 "$WT" env LOGDIR="$OUT/U3" scripts/qemu-riscv64-uart-irq-witness-smoke.sh
fi
if on ipc; then
  step M1_direct_matrix "$WT" scripts/qemu-ipccall-reply-direct-matrix-seal.sh
  step M2_server_dies_x86_64 "$WT" scripts/qemu-x86_64-server-dies-smoke.sh
  step M3_server_dies_aarch64 "$WT" scripts/qemu-aarch64-server-dies-smoke.sh
  step M4_server_dies_riscv64 "$WT" scripts/qemu-riscv64-server-dies-smoke.sh
fi
PROFILES="sender-wake d2-send smp2-core smp4-core smp2-sender-wake smp4-sender-wake sched-timeout"
if on ci; then
  step C1_build_default_x86_64_F "$WT" scripts/build-qemu-x86_64-artifacts.sh
  before=$(oracle_snap)
  step C2_ci_profiles_F "$WT" scripts/run-ci-profiles.sh $PROFILES --keep-going --logs-dir "$OUT/C2_ci-profiles"
  keep_oracle C2 "$before"
  step C3_build_default_x86_64_B "$BWT" scripts/build-qemu-x86_64-artifacts.sh
  before=$(oracle_snap)
  step C4_ci_profiles_B "$BWT" scripts/run-ci-profiles.sh $PROFILES --keep-going --logs-dir "$OUT/C4_ci-profiles"
  keep_oracle C4 "$before"
  step C5a_reply_timeout_matrix_F "$WT" scripts/qemu-ipc-reply-timeout-matrix-smoke.sh
  step C5b_reply_timeout_matrix_B "$BWT" scripts/qemu-ipc-reply-timeout-matrix-smoke.sh
  step C5c_smp_request_F "$WT" scripts/qemu-ipccall-direct-x86_64-smp-request-smoke.sh
  step C5d_smp_request_B "$BWT" scripts/qemu-ipccall-direct-x86_64-smp-request-smoke.sh
fi
if on xsect; then
  step AP_marker_integrity "$WT" bash -c "$S/ft1_apscan.sh"
  step X_sections_and_FP "$WT" bash -c "$S/ft1_sections.sh"
fi
step Z_markers "$WT" bash -c "$S/fwa_zscan.sh \$(find $OUT -name '*.log' ! -name 'H*' ! -name 'Z_*' ! -name 'P*' ! -name 'G*' ! -path '*/C4_*' ! -path '*/FB*' ! -name 'C3_*' ! -name 'C4_*' ! -name 'C5b_*' ! -name 'C5d_*' ! -name '*_B.log' ! -path '*_B/*' ! -path '*/X_*' ! -path '*/X/*' ! -name 'FB*' ! -path '*/UN/*' ! -name 'UN0*' ! -name 'AP_*')"
echo "matrix end $(date -u +%FT%TZ) F_porcelain=$(git -C "$WT" status --porcelain | wc -l) B_porcelain=$(git -C "$BWT" status --porcelain | wc -l)" >> "$SUMMARY"
echo MATRIX_DONE >> "$SUMMARY"
