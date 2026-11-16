#!/usr/bin/env bash
# PRE-SCHEDULER-CLEANUP1 qualification Addendum B (declared after W3_sched_time_riscv64 failed, before
# any addendum-B step ran): on the frozen candidate F, W3_sched_time_riscv64 failed its premise guard —
# saturation completed at 153.6 s, after W's case-8 deadline (149.6 s; dev runs saturated at 56–80 s);
# the kernel-side proofs held (W full_proving=4, R ipc_proving=4 with one settlement). W3b reruns that
# one step once, unchanged, alone on the host. W3 stays recorded.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
WT=$S/psc-q-wt OUT=$S/psc-evidence/qual SUMMARY=$OUT/summary.txt BIND=$OUT/bindings.txt
step() {
  local id=$1 dir=$2; shift 2
  grep -q "^$id rc=" "$SUMMARY" 2>/dev/null && return 0
  [ -e "$OUT/$id.log" ] && mv "$OUT/$id.log" "$OUT/$id.log.interrupted.$(date +%s)"
  local t0; t0=$(date +%s)
  echo "=== $id [$(git -C "$dir" rev-parse --short=12 HEAD) porcelain=$(git -C "$dir" status --porcelain --untracked-files=no | wc -l)]: $* (start $(date -u +%T))" >> "$SUMMARY"
  ( cd "$dir" && "$@" ) > "$OUT/$id.log" 2>&1
  echo "$id rc=$? secs=$(( $(date +%s) - t0 ))" >> "$SUMMARY"
  grep -ah 'ARTIFACT_BINDING' "$OUT/$id.log" 2>/dev/null | sed "s/^/$id /" >> "$BIND" || true
}
echo "addendum B start $(date -u +%FT%TZ) sha=$(sha256sum $0 | cut -c1-16)" >> "$SUMMARY"
step W3b_sched_time_riscv64 "$WT" env -u CARGO_TARGET_DIR SMP=2 LOGDIR="$OUT/W3b_riscv64" timeout 3600 scripts/qemu-sched-time-witness-smoke.sh riscv64
echo ADDENDUM_B_DONE >> "$SUMMARY"
