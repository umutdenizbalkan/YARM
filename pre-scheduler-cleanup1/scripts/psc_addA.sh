#!/usr/bin/env bash
# PRE-SCHEDULER-CLEANUP1 qualification Addendum A (declared after W1_shared_region_x86_64 failed, before
# any addendum step ran): the shared-region direct smokes require `initramfs:shared-region-offset-witness`
# unconditionally (ARTIFACT_REQUIRE in all three scripts, since 15114160), so their default
# OFFSET_SELECTION=0 is refused by the boot guard before QEMU on every commit. The matrix's W1 steps
# invoked them with the default: a runner configuration error. W1b runs each with OFFSET_SELECTION=1,
# the configuration the scripts require. The W1 results stay recorded as they are.
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
echo "addendum A start $(date -u +%FT%TZ) sha=$(sha256sum $0 | cut -c1-16)" >> "$SUMMARY"
for a in x86_64 aarch64 riscv64; do
  step "W1b_shared_region_${a}" "$WT" env -u CARGO_TARGET_DIR OFFSET_SELECTION=1 LOGDIR="$OUT/W1b_${a}" timeout 2400 scripts/qemu-shared-region-direct-${a}-smoke.sh
done
echo ADDENDUM_A_DONE >> "$SUMMARY"
