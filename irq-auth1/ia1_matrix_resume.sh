#!/usr/bin/env bash
# IRQ-AUTH1 frozen-candidate qualification — CONTINUATION after the interrupted run (M2 onward) (schedule: ia1_schedule.md).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=578daf4
WT=$S/ia1-wt BWT=$S/nw1-wt
OUT=$S/ia1-evidence/qual
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
hosted() { CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/ia1q-target "$@"; }
hosted_b() { CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/nw1q-target "$@"; }
oracle_snap() { ls -d /tmp/yarm-oracle-* 2>/dev/null | sort; }
keep_oracle() {
  local tag=$1 before=$2 d
  for d in $(comm -13 <(echo "$before") <(oracle_snap)); do cp -a "$d" "$OUT/oracle-$tag-$(basename "$d")"; done
}

ONLY=all
on() { true; }
echo "matrix resume $(date -u +%FT%TZ) frozen_matrix_sha=$(sha256sum $S/ia1_matrix.sh | cut -c1-16)" >> "$SUMMARY"
if on ipc; then
  step M2_server_dies_x86_64 "$WT" scripts/qemu-x86_64-server-dies-smoke.sh
  step M3_server_dies_aarch64 "$WT" scripts/qemu-aarch64-server-dies-smoke.sh
  step M4_server_dies_riscv64 "$WT" scripts/qemu-riscv64-server-dies-smoke.sh
fi
PROFILES="smp2-core smp4-core smp2-sender-wake smp4-sender-wake sched-timeout"
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
step Z_notif_markers "$WT" bash -c "grep -arlE 'NOTIF_REF_OVER_RELEASE|NOTIF_REF_RELEASE_FULL|IRQ_ROUTE_RETRACTED|IRQ_ROUTE_STALE_RECLAIMED|IRQ_ROUTE_RELEASED' $OUT --include='*.log' --exclude='H*' --exclude='Z_*' --exclude='P*' | wc -l"
echo "matrix end $(date -u +%FT%TZ) F_porcelain=$(git -C "$WT" status --porcelain | wc -l) B_porcelain=$(git -C "$BWT" status --porcelain | wc -l)" >> "$SUMMARY"
echo MATRIX_DONE >> "$SUMMARY"
