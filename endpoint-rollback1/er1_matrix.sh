#!/usr/bin/env bash
# ENDPOINT-ROLLBACK1 frozen-candidate qualification (schedule: er1_schedule.md).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=b9a29032
WT=$S/er1-wt BWT=$S/sta1-wt
OUT=$S/er1-evidence/qual
mkdir -p "$OUT"
SUMMARY=$OUT/summary.txt BIND=$OUT/bindings.txt

step() {  # step <id> <dir> <cmd...>
  local id=$1 dir=$2; shift 2
  local t0; t0=$(date +%s)
  echo "=== $id [$(git -C "$dir" rev-parse --short=12 HEAD) porcelain=$(git -C "$dir" status --porcelain | wc -l)]: $* (start $(date -u +%T))" >> "$SUMMARY"
  ( cd "$dir" && "$@" ) > "$OUT/$id.log" 2>&1
  local rc=$?
  echo "$id rc=$rc secs=$(( $(date +%s) - t0 ))" >> "$SUMMARY"
  grep -ah 'ARTIFACT_BINDING' "$OUT/$id.log" 2>/dev/null | sed "s/^/$id /" >> "$BIND" || true
  return 0
}
hosted() { CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/er1q-target "$@"; }
hosted_b() { CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/sta1q-target "$@"; }
oracle_snap() { ls -d /tmp/yarm-oracle-* 2>/dev/null | sort; }
keep_oracle() {
  local tag=$1 before=$2 d
  for d in $(comm -13 <(echo "$before") <(oracle_snap)); do cp -a "$d" "$OUT/oracle-$tag-$(basename "$d")"; done
}

ONLY=${ONLY:-all}
on() { [[ $ONLY == all || " $ONLY " == *" $1 "* ]]; }
echo "matrix start $(date -u +%FT%TZ) only=$ONLY schedule_sha=$(sha256sum $S/er1_schedule.md | cut -c1-16)" >> "$SUMMARY"

if on prov; then
  step P0_provenance "$WT" bash -c "git rev-parse HEAD; test \$(git rev-parse --short=8 HEAD) = $F && echo HEAD_IS_F; git status --porcelain | wc -l; rustc -V; cargo -V; qemu-system-x86_64 --version | head -1; qemu-system-aarch64 --version | head -1; qemu-system-riscv64 --version | head -1"
  step P1_source "$WT" bash -c "git diff --stat 3148a754 HEAD; sed -n '/pub(crate) fn remove_unpublished_endpoint_locked(/,/^}/p' src/kernel/boot/spawn_ipc_cap_txn.rs; grep -rn 'remove_unpublished_endpoint_locked\|irq_routes' src/kernel/boot/spawn_ipc_cap_txn.rs src/kernel/syscall/spawn_txn.rs src/kernel/syscall/spawn_image_txn.rs"
fi
if on hosted; then
  step H1_fmt "$WT" cargo fmt --all -- --check
  step H2_clippy "$WT" hosted cargo clippy --lib --features hosted-dev
  step H3_hosted_lib "$WT" hosted cargo test --lib -- --test-threads=1
  step H4_integration "$WT" hosted cargo test --test '*' --no-fail-fast -- --test-threads=1
  for f in endpoint_rollback1 u9spawnic1_endpoint_cap_txn u9spawn1_sp3_spawn_ledger u9spawntxn3_provisional_rollback u9spawnvm2_provision_failure_injection spawn fork endpoint generation capability exit reap send_timeout_arb1 ipc_sender_id1 ipc_displace1 sender_wake transfer shared_region notif_lifetime1 notif_waiter1 irq_generation1 irq_auth1 irq_delivery1; do
    step "H5_$f" "$WT" hosted cargo test --lib "$f" -- --test-threads=1
  done
  step H2b_clippy_classes_B "$BWT" bash -c "$S/er1_clipcmp.sh"
  step H6_lockorder_attr "$WT" bash -c "$S/er1_lockattr.sh"
fi
if on default; then
  step D1_build_x86_64 "$WT" scripts/build-qemu-x86_64-artifacts.sh
  step D2_build_aarch64 "$WT" scripts/build-qemu-aarch64-artifacts.sh
  step D3_build_riscv64 "$WT" scripts/build-qemu-riscv64-artifacts.sh
  step O1_oracle_x86_64 "$WT" scripts/qemu-ipc-recv-v2-oracle-smoke.sh x86_64
  step O2_oracle_aarch64 "$WT" scripts/qemu-ipc-recv-v2-oracle-smoke.sh aarch64
  step O3_oracle_riscv64 "$WT" scripts/qemu-ipc-recv-v2-oracle-smoke.sh riscv64
fi
if on strict; then
  step S1_build_x86_64_strict "$WT" env ARTIFACTS_STRICT=1 BOOTSTRAP_FEATURE_ARGS="--no-default-features --features timer-contract-witness" scripts/build-qemu-x86_64-artifacts.sh
  step S2_core_x86_64_strict "$WT" env QEMU_SMOKE_STRICT=1 LOGFILE="$OUT/S2_qemu-x86_64-core.log" scripts/qemu-x86_64-core-smoke.sh
  step S3_build_aarch64_strict "$WT" env ARTIFACTS_STRICT=1 scripts/build-qemu-aarch64-artifacts.sh
  step S4_core_aarch64_strict "$WT" env QEMU_SMOKE_STRICT=1 scripts/qemu-aarch64-core-smoke.sh
  step S5_build_riscv64_strict "$WT" env ARTIFACTS_STRICT=1 scripts/build-qemu-riscv64-artifacts.sh
  step S6_core_riscv64_strict "$WT" env QEMU_SMOKE_STRICT=1 scripts/qemu-riscv64-core-smoke.sh
  step S7_ci_timer_grep "$WT" bash -c "for m in YARM_TIMER_IRQ_DELIVERED YARM_TIMER_EOI_DONE YARM_SCHED_TICK; do echo \"\$m \$(grep -ac \$m $OUT/S2_qemu-x86_64-core.log)\"; done; grep -aq YARM_TIMER_IRQ_DELIVERED $OUT/S2_qemu-x86_64-core.log && grep -aq YARM_TIMER_EOI_DONE $OUT/S2_qemu-x86_64-core.log && grep -aq YARM_SCHED_TICK $OUT/S2_qemu-x86_64-core.log"
fi
if on witness; then
  for a in x86_64 aarch64 riscv64; do
    step "W_${a}_smp1" "$WT" env LOGDIR="$OUT/W_${a}_smp1" timeout 2400 scripts/qemu-ipc-displace1-witness-smoke.sh "$a" 1
  done
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
step Z_markers "$WT" bash -c "for f in \$(find $OUT -name '*.log' ! -name 'H*' ! -name 'Z_*' ! -name 'P*' ! -path '*/C4_*' ! -name 'C3_*' ! -name 'C4_*' ! -name 'C5b_*' ! -name 'C5d_*'); do n=\$(grep -acE 'SCHED_WAKE_FAIL|SCHED_TIMEOUT_STRANDED_WAITER|IPC_RECV_BLOCKED_COMPLETE_FAILED|x86 shared trap dispatch failed|YARM_AARCH64_TRAP_HANDLE halting|RISCV_TRAP_HANDLE_FAILED|U6_SEND_COMPLETION_REFUSED_STALE|U7_SEND_TIMEOUT_CLAIM_AFTER_WITHDRAW_REFUSED|SEND_ARB_PUBLICATION_REFUSED_UNWIND|endpoint=Published|outcome=Published|SPAWN_LEDGER_OVERFLOW' \$f); [ \$n -gt 0 ] && echo \"\$n \$f\"; done; true"
echo "matrix end $(date -u +%FT%TZ) F_porcelain=$(git -C "$WT" status --porcelain | wc -l) B_porcelain=$(git -C "$BWT" status --porcelain | wc -l)" >> "$SUMMARY"
echo MATRIX_DONE >> "$SUMMARY"
