#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# BUILD-GATES1 §4 — AArch64 U9-RX4 reply-record REUSE witness (labeled; not production policy).
#
# The strict AArch64 core smoke grades tid 2's PM lifecycle query (U9-RX4). In every ordinary boot
# the query takes the reply-record slot first, at generation 1, so a grader that pinned generation
# 1 was never contradicted live. This witness builds the default profile plus the default-off
# `u9rx4-reuse-witness` feature, under which the supervisor yields before its query; init's PM
# calls then take and release the shared slot first and the query runs on a REUSED record. The
# unmodified strict core smoke then grades that boot with the production grader
# (`scripts/lib/u9rx4-terminal-eval.sh`).
#
# Two separate verdicts, never folded together:
#   * PREMISE — did this boot actually reuse a slot? tid 2's armed record must have generation > 1
#     and the slot must have been used by another caller first. A boot that did not reuse proves
#     nothing about reuse and FAILS here (`reason=premise`), whatever the grader said;
#   * GRADE — the core smoke's own verdict on that boot, including the U9-RX4 terminal line naming
#     the reused record.
#
#   LOGDIR=...   build + boot logs (default /tmp/qemu-aarch64-u9rx4-reuse-witness)
#   SKIP_BUILD=1 reuse $LOGDIR/build (still refused before boot unless provenance binds it)
#   QEMU_SMP=N   CPUs (default 1)
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=u9rx4-reuse-witness
LOGDIR=${LOGDIR:-/tmp/qemu-aarch64-u9rx4-reuse-witness}
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
SMP=${QEMU_SMP:-1}
mkdir -p "$LOGDIR"
seal() { echo "U9RX4_REUSE_WITNESS_SEAL arch=aarch64 smp=$SMP $*"; }

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "[u9rx4-reuse] building aarch64 artifacts with --features $FEATURE into $BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $FEATURE" \
    scripts/build-qemu-aarch64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || { seal "result=fail reason=build"; exit 1; }
fi

# The core smoke refuses (before QEMU) any pair that is not exactly this witness build.
rm -f "$BOOT_LOG"
KERNEL_IMAGE="$BUILD_DIR/yarm-aarch64.bin" INITRAMFS_IMAGE="$BUILD_DIR/initramfs-core.cpio" \
  ARTIFACT_REQUIRE="kernel=$FEATURE initramfs=$FEATURE" \
  LOGFILE="$BOOT_LOG" QEMU_SMP="$SMP" QEMU_SMOKE_STRICT=1 TIMEOUT_SECS="${TIMEOUT_SECS:-90}" \
  scripts/qemu-aarch64-core-smoke.sh >"$LOGDIR/core-smoke.log" 2>&1
grade_rc=$?
grep -a "ARTIFACT_BINDING " "$LOGDIR/core-smoke.log" | head -1
if ! grep -aq "ARTIFACT_BINDING .* result=ok" "$LOGDIR/core-smoke.log"; then
  seal "result=fail reason=artifact-binding"
  exit 1
fi

# ── PREMISE: the boot reused the slot ──
norm="$(tr '\r' '\n' <"$BOOT_LOG")"
arm="$(grep -a -m1 -E '^IPC_REPLY_TERMINAL_ARMED_SPLIT caller_tid=2 caller_asid=2 record_index=[0-9]+ record_generation=[0-9]+ .*finite_deadline=1' <<<"$norm")"
rec="$(grep -o -E 'record_index=[0-9]+' <<<"$arm" | cut -d= -f2)"
gen="$(grep -o -E 'record_generation=[0-9]+' <<<"$arm" | cut -d= -f2)"
yielded="$(grep -a -o -m1 -E 'U9RX4_REUSE_WITNESS_YIELDED yields=[0-9]+' <<<"$norm")"
earlier=0 own_earlier=0
if [[ -n "$rec" && -n "$gen" ]] && (( gen > 1 )); then
  # Earlier generations of the same slot: armed by OTHER callers, never by tid 2 itself.
  read -r earlier own_earlier < <(awk -v r="$rec" -v g="$gen" '
    /^IPC_REPLY_TERMINAL_ARMED_SPLIT / {
      c=""; i=""; n="";
      for (k = 2; k <= NF; k++) { split($k, kv, "="); if (kv[1]=="caller_tid") c=kv[2]; if (kv[1]=="record_index") i=kv[2]; if (kv[1]=="record_generation") n=kv[2] }
      if (i == r && n + 0 < g + 0) { if (c == "2") own++; else other++ }
    }
    END { print other + 0, own + 0 }' <<<"$norm")
fi
echo "[u9rx4-reuse] ${yielded:-U9RX4_REUSE_WITNESS_YIELDED missing} tid2_record=(${rec:-?}, ${gen:-?}) earlier_arms_on_slot_by_other_callers=${earlier} by_tid2=${own_earlier}"
if [[ -z "$yielded" ]]; then
  seal "result=fail reason=premise detail=witness-not-armed"
  exit 1
fi
if [[ -z "$gen" ]] || (( gen <= 1 )) || (( earlier < 1 )) || (( own_earlier != 0 )); then
  seal "result=fail reason=premise detail=no-reuse record=(${rec:-?},${gen:-?}) earlier=${earlier}"
  exit 1
fi

# ── GRADE: the strict core smoke's verdict, and its U9-RX4 line on the reused record ──
terminal="$(grep -a -m1 -E "^\[ok\] U9-RX4: (REPLY|TIMEOUT) won the terminal of record_index=${rec} record_generation=${gen} " "$LOGDIR/core-smoke.log")"
echo "[u9rx4-reuse] ${terminal:-no U9-RX4 terminal verdict on the reused record}"
grep -a "U9-RX4: terminal-outcome evaluator self-test" "$LOGDIR/core-smoke.log" | head -1
if (( grade_rc != 0 )) || [[ -z "$terminal" ]]; then
  grep -a "^\[error\]" "$LOGDIR/core-smoke.log" | head -20
  seal "result=fail reason=grade record=(${rec},${gen}) core_rc=${grade_rc}"
  exit 1
fi
route="$(grep -o -E '\((queued|direct) route' <<<"$terminal" | tr -d '(')"
seal "result=pass record=(${rec},${gen}) ${route} earlier=${earlier}"
