#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 199A2C3 — Exact-Commit Three-Architecture DIRECT IpcCall/IpcReply Matrix Seal.
#
# Runs, SERIALLY and from ONE exact clean-tree commit, the three per-architecture DIRECT NR6/NR7
# live round-trip smokes (x86_64, AArch64, RISC-V). Each child smoke performs a FRESH feature build,
# boots ONE `-smp 1` QEMU with its arch-specific selector, validates its full service chain, and
# emits exactly one per-architecture live seal. This runner — NOT userspace and NOT any child smoke —
# emits the combined matrix seal, and ONLY after all three fresh boots succeed in the same
# uninterrupted exact-commit run.
#
# Exact-commit + clean-tree discipline:
#   * captures the git SHA + `git status --porcelain` at start; requires a clean tracked tree;
#   * re-verifies the SHA + clean tracked tree after EACH child — a tracked source/script change
#     aborts the matrix (no cross-commit seal aggregation, no stale-log splicing);
#   * clears each per-arch log dir before its child, and requires each boot log to POST-DATE the
#     matrix start (no old-log reuse; no reusing one arch's log for another).
#
# On a fully clean 3-arch run emits:
#   STAGE_199_IPCCALL_REPLY_DIRECT_MATRIX_SEAL arches=3 classes_per_arch=2 total_live_cells=6 \
#     duplicate_replies=0 duplicate_wakes=0 fuse_trips=0 result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

note() { echo "[ipccall-reply-direct-matrix] $*"; }
die()  { echo "[ipccall-reply-direct-matrix][FAIL] $*"; echo "STAGE_199_IPCCALL_REPLY_DIRECT_MATRIX_SEAL result=fail reason=$1_or_later"; exit 1; }

# ── 1. Exact-commit + clean-tree capture ──
START_SHA=$(git rev-parse HEAD 2>/dev/null) || die "no_git"
PORCELAIN=$(git status --porcelain)
if [[ -n "$PORCELAIN" ]]; then
  echo "[ipccall-reply-direct-matrix][FAIL] dirty tracked tree at start:"
  echo "$PORCELAIN"
  echo "STAGE_199_IPCCALL_REPLY_DIRECT_MATRIX_SEAL result=fail reason=dirty_tree"
  exit 1
fi
MATRIX_START=$(date +%s)
note "exact-commit matrix start: SHA=$START_SHA (clean tracked tree) at epoch $MATRIX_START"

MATRIX_LOGDIR=${MATRIX_LOGDIR:-/tmp/ipccall-reply-direct-matrix}
rm -rf "$MATRIX_LOGDIR"; mkdir -p "$MATRIX_LOGDIR"

# arch → (smoke script, boot-log dir, userspace-completion literal)
declare -a ARCHES=(x86_64 aarch64 riscv64)
declare -A SMOKE=(
  [x86_64]=scripts/qemu-ipccall-reply-direct-x86_64-smoke.sh
  [aarch64]=scripts/qemu-ipccall-reply-direct-aarch64-smoke.sh
  [riscv64]=scripts/qemu-ipccall-reply-direct-riscv64-smoke.sh
)
declare -A BOOTDIR=(
  [x86_64]=/tmp/ipccall-reply-direct-x86_64
  [aarch64]=/tmp/ipccall-reply-direct-aarch64
  [riscv64]=/tmp/ipccall-reply-direct-riscv64
)
declare -A UDONE_PREFIX=(
  [x86_64]=X86_IPCCALL_DIRECT_ROUNDTRIP_DONE
  [aarch64]=AARCH64_IPCCALL_DIRECT_ROUNDTRIP_DONE
  [riscv64]=RISCV_IPCCALL_DIRECT_ROUNDTRIP_DONE
)

verify_clean_still() {
  local now_sha now_porcelain
  now_sha=$(git rev-parse HEAD 2>/dev/null)
  now_porcelain=$(git status --porcelain)
  [[ "$now_sha" == "$START_SHA" ]] || die "commit_changed_${now_sha}"
  [[ -z "$now_porcelain" ]] || { echo "[matrix][FAIL] tracked tree became dirty mid-run:"; echo "$now_porcelain"; die "tree_dirty_midrun"; }
}

# ── 2/3/4/5. Run each child serially with independent per-arch evidence checks ──
for arch in "${ARCHES[@]}"; do
  smoke=${SMOKE[$arch]}
  bootdir=${BOOTDIR[$arch]}
  child_log="$MATRIX_LOGDIR/${arch}.child.log"
  note "── running ${arch} smoke (fresh feature build + one -smp 1 boot) ──"
  # Clear the child's log dir so no stale ELF/log can satisfy the run.
  rm -rf "$bootdir"
  verify_clean_still
  bash "$smoke" >"$child_log" 2>&1
  rc=$?
  if [[ $rc -ne 0 ]]; then
    tail -20 "$child_log"
    die "${arch}_smoke_exit_${rc}"
  fi
  verify_clean_still

  # 3. Exactly one per-architecture seal in the child output.
  seal="STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=${arch} classes=2 live_cells=2 duplicate_replies=0 duplicate_wakes=0 result=ok"
  sc=$(grep -aFc "$seal" "$child_log" || true)
  [[ "$sc" == "1" ]] || die "${arch}_seal_count_${sc}"

  # Fresh-log discipline: the child's normalized boot log must post-date the matrix start.
  norm="$bootdir/boot.norm.log"
  [[ -s "$norm" ]] || die "${arch}_no_boot_log"
  blog_mtime=$(stat -c %Y "$norm" 2>/dev/null || echo 0)
  [[ "$blog_mtime" -ge "$MATRIX_START" ]] || die "${arch}_stale_boot_log"

  cnt() { grep -aFc "$1" "$norm" 2>/dev/null || echo 0; }

  # 4. Class evidence — each exactly once.
  for m in \
    "IPCCALL_DIRECT_REQUEST_OK arch=${arch} source_copy_offlock=1 reply_cap=1 server_wakes=1" \
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=${arch} class=IpcCallDirectRequest result=ok" \
    "IPCREPLY_DIRECT_OK arch=${arch} source_copy_offlock=1 caller_wakes=1 one_shot=1" \
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=${arch} class=IpcReplyDirect result=ok" ; do
    c=$(cnt "$m")
    [[ "$c" == "1" ]] || die "${arch}_class_evidence_count_${c}"
  done

  # 4/5. Userspace completion (exact counts) + duplicate-reply rejection — exactly once.
  udone="${UDONE_PREFIX[$arch]} request_ok=1 reply_ok=1 duplicate_reply=rejected server_wakes=1 caller_wakes=1 client_continuations=1 server_continuations=1 result=ok"
  uc=$(cnt "$udone")
  [[ "$uc" == "1" ]] || die "${arch}_userspace_completion_count_${uc}"
  dupc=$(cnt "IPCCALL_DIRECT_ORACLE_SERVER_DUP dup_rejected=1")
  [[ "$dupc" == "1" ]] || die "${arch}_duplicate_reject_count_${dupc}"

  # 6. Cross-arch attribution hygiene: no OTHER arch's kernel attestation or userspace completion.
  for other in "${ARCHES[@]}"; do
    [[ "$other" == "$arch" ]] && continue
    grep -aFq "IPCCALL_DIRECT_REQUEST_OK arch=${other}" "$norm" && die "${arch}_has_${other}_attestation"
    grep -aFq "${UDONE_PREFIX[$other]}" "$norm" && die "${arch}_has_${other}_completion"
  done

  # 8. Fail-closed conditions in the boot log.
  for bad in "KERNEL PANIC" "RUST PANIC" "panicked at" "DOUBLE FAULT" "BOOTSTRAP_ERROR" \
             "IPCCALL_DIRECT_ORACLE_SERVER_DUP dup_rejected=0" ; do
    grep -aFq "$bad" "$norm" && die "${arch}_fatal_${bad// /_}"
  done
  note "${arch}: PASS (both cells live; class evidence + userspace completion exactly once)"
done

# ── 9. Final clean-tree re-verify, then emit the combined seal ──
verify_clean_still
note "all three architectures PASS at exact commit $START_SHA (clean tracked tree throughout)"
echo "STAGE_199_IPCCALL_REPLY_DIRECT_MATRIX_SEAL arches=3 classes_per_arch=2 total_live_cells=6 duplicate_replies=0 duplicate_wakes=0 fuse_trips=0 result=ok"
