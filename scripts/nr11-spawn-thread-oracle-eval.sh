#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# A64-DEPTH §3 — the ONE evaluator for the NR 11 (`SpawnThread`) witness inside an
# ExitCurrentTask oracle serial log.
#
# WHY THIS EXISTS AS A COMMITTED SCRIPT
#
# The first version of this counter lived in a scratch file and required the marker name and
# `disposable_tid=` to be ADJACENT:
#
#     EXIT_TASK_ORACLE_SPAWNED disposable_tid=([0-9]+)
#
# x86_64 and AArch64 happen to match that. RISC-V does not — its marker carries an architecture
# field in between:
#
#     EXIT_TASK_ORACLE_SPAWNED arch=riscv64 disposable_tid=10008
#
# so a perfectly healthy RISC-V boot was scored `user_spawn_ok=0 broad=-1`. The kernel was fine;
# the counter was wrong, and nothing would have caught it, because the counter and the markers
# it reads had no relationship other than a hand-copied literal.
#
# Two things close that hole:
#
#   1. the pattern below tolerates an OPTIONAL architecture field, and
#   2. `--self-test` re-derives every marker spelling FROM the userspace source that emits them
#      and proves this evaluator matches all of them. Adding an architecture field to a marker
#      that lacks one — or a fourth architecture — makes the self-test exercise the new spelling
#      automatically instead of silently zeroing its count.
#
# USAGE
#   nr11-spawn-thread-oracle-eval.sh <serial-log> [<serial-log> ...]
#   nr11-spawn-thread-oracle-eval.sh --self-test
#
# OUTPUT (one line per log, plus a verdict)
#   NR11_EVAL log=<path> split_ok=N broad=N split_fail=N bad_args=N undone=N tid_match=N result=ok|fail
set -uo pipefail
cd "$(dirname "$0")/.."

# The kernel-side witness that NR 11 was served by the split route.
SPLIT_OK='SPAWN_THREAD_SPLIT_OK'
SPLIT_FAIL='SPAWN_THREAD_SPLIT_FAIL'
# The broad route's only compensation marker; a live boot must never emit it.
UNDONE='SPAWN_THREAD_ENQUEUE_FAILED'
# The USERSPACE witness that a spawn was observed to succeed. The architecture field is optional
# BY DESIGN — see the header.
USER_SPAWNED='EXIT_TASK_ORACLE_SPAWNED( arch=[A-Za-z0-9_]+)? disposable_tid=([0-9]+)'

# Strip carriage returns; QEMU serial logs are CRLF and every downstream match depends on it.
norm() { tr -d '\r' < "$1"; }

# `broad` is derived, not observed: userspace saw a spawn succeed, so exactly one of the two
# routes served it. Every success the split route did NOT claim was served broad.
eval_log() {
  local log="$1" rc=0
  local body; body=$(norm "$log")

  local split_ok split_fail undone user_ok
  split_ok=$(grep -acE "${SPLIT_OK} " <<<"$body" || true)
  split_fail=$(grep -acE "${SPLIT_FAIL} " <<<"$body" || true)
  undone=$(grep -acE "${UNDONE} " <<<"$body" || true)
  user_ok=$(grep -acE "${USER_SPAWNED}" <<<"$body" || true)

  local broad=$(( user_ok - split_ok ))

  # An argument-bearing success must carry a real entry, stack and TLS base.
  local bad_args
  bad_args=$(grep -aE "${SPLIT_OK} " <<<"$body" \
    | grep -acE 'entry=0x0( |$)|stack=0x0( |$)|tls=0x0( |$)' || true)

  # The TID the kernel says it created must be the TID userspace says it got.
  local ktid utid tid_match=0
  ktid=$(grep -aoE "${SPLIT_OK} parent_tid=[0-9]+ tid=[0-9]+" <<<"$body" | grep -oE 'tid=[0-9]+$' | cut -d= -f2 | head -1)
  utid=$(grep -aoE "${USER_SPAWNED}" <<<"$body" | grep -oE 'disposable_tid=[0-9]+' | cut -d= -f2 | head -1)
  if [[ -n "$ktid" && "$ktid" == "$utid" ]]; then tid_match=1; fi

  [[ "$split_ok"   == 1 ]] || rc=1
  [[ "$broad"      == 0 ]] || rc=1
  [[ "$split_fail" == 0 ]] || rc=1
  [[ "$bad_args"   == 0 ]] || rc=1
  [[ "$undone"     == 0 ]] || rc=1
  [[ "$tid_match"  == 1 ]] || rc=1

  local verdict=ok; [[ $rc == 0 ]] || verdict=fail
  echo "NR11_EVAL log=${log} split_ok=${split_ok} broad=${broad} split_fail=${split_fail} bad_args=${bad_args} undone=${undone} tid_match=${tid_match} result=${verdict}"
  return $rc
}

# ── self-test ────────────────────────────────────────────────────────────────────────────
INIT_SRC=crates/yarm-control-plane-servers/src/control_plane/init/service.rs

self_test() {
  local tmp; tmp=$(mktemp -d); local fails=0 checks=0
  check() { # name expected_rc file
    checks=$((checks+1))
    local out; out=$(eval_log "$3" 2>&1); local got=$?
    if [[ "$got" != "$2" ]]; then
      echo "  [FAIL] $1: expected rc=$2 got rc=$got"; echo "         $out"; fails=$((fails+1))
    else
      echo "  [ok]   $1"
    fi
  }

  # (1) Every spelling the userspace oracle actually emits must be MATCHED. These are derived
  #     from the source, not typed here, so a new or changed marker is exercised automatically.
  local spellings; spellings=$(grep -aoE '"EXIT_TASK_ORACLE_SPAWNED[^"]*"' "$INIT_SRC" | tr -d '"')
  if [[ -z "$spellings" ]]; then
    echo "  [FAIL] no EXIT_TASK_ORACLE_SPAWNED format strings found in $INIT_SRC"
    fails=$((fails+1)); checks=$((checks+1))
  fi
  local n=0
  while IFS= read -r fmt; do
    [[ -n "$fmt" ]] || continue
    n=$((n+1))
    # Render the format string as the log would carry it: `{}` becomes the TID.
    local rendered=${fmt//\{\}/10008}
    printf 'SPAWN_THREAD_SPLIT_OK parent_tid=1 tid=10008 entry=0x405754 stack=0x46b620 tls=0x46b624 result=ok\nUSER_LOG tid=1 msg=%s\n' \
      "$rendered" > "$tmp/spelling$n.log"
    check "source spelling $n matches: ${rendered}" 0 "$tmp/spelling$n.log"
  done <<<"$spellings"

  # (2) The exact defect that motivated this script: an architecture field between the marker
  #     name and the TID must not zero the count.
  printf 'SPAWN_THREAD_SPLIT_OK parent_tid=1 tid=10008 entry=0x405754 stack=0x46b620 tls=0x46b624 result=ok\nUSER_LOG tid=1 msg=EXIT_TASK_ORACLE_SPAWNED arch=riscv64 disposable_tid=10008\n' > "$tmp/riscv.log"
  check "architecture field does not invalidate the count" 0 "$tmp/riscv.log"

  # (3) A userspace success the split route did not claim IS a broad execution.
  printf 'USER_LOG tid=1 msg=EXIT_TASK_ORACLE_SPAWNED arch=aarch64 disposable_tid=10008\n' > "$tmp/broad.log"
  check "unclaimed userspace success counts as broad" 1 "$tmp/broad.log"

  # (4) Two split successes are not one.
  printf 'SPAWN_THREAD_SPLIT_OK parent_tid=1 tid=10008 entry=0x1 stack=0x2 tls=0x3 result=ok\nSPAWN_THREAD_SPLIT_OK parent_tid=1 tid=10009 entry=0x1 stack=0x2 tls=0x3 result=ok\nUSER_LOG tid=1 msg=EXIT_TASK_ORACLE_SPAWNED disposable_tid=10008\n' > "$tmp/dup.log"
  check "a duplicate creation is refused" 1 "$tmp/dup.log"

  # (5) A success with no entry point is not a success.
  printf 'SPAWN_THREAD_SPLIT_OK parent_tid=1 tid=10008 entry=0x0 stack=0x46b620 tls=0x46b624 result=ok\nUSER_LOG tid=1 msg=EXIT_TASK_ORACLE_SPAWNED disposable_tid=10008\n' > "$tmp/badargs.log"
  check "a zero entry point is refused" 1 "$tmp/badargs.log"

  # (6) The kernel's TID and userspace's TID must be the same task.
  printf 'SPAWN_THREAD_SPLIT_OK parent_tid=1 tid=10008 entry=0x1 stack=0x2 tls=0x3 result=ok\nUSER_LOG tid=1 msg=EXIT_TASK_ORACLE_SPAWNED disposable_tid=99999\n' > "$tmp/mismatch.log"
  check "a TID mismatch is refused" 1 "$tmp/mismatch.log"

  # (7) A compensated broad enqueue failure must not pass.
  printf 'SPAWN_THREAD_SPLIT_OK parent_tid=1 tid=10008 entry=0x1 stack=0x2 tls=0x3 result=ok\nSPAWN_THREAD_ENQUEUE_FAILED tid=10008 err=Full result=compensated\nUSER_LOG tid=1 msg=EXIT_TASK_ORACLE_SPAWNED disposable_tid=10008\n' > "$tmp/undone.log"
  check "a compensated enqueue failure is refused" 1 "$tmp/undone.log"

  # (8) A split failure is refused even when userspace saw nothing.
  printf 'SPAWN_THREAD_SPLIT_FAIL parent_tid=1 tid=10008 err=TaskTableFull tasks=0 result=compensated\n' > "$tmp/fail.log"
  check "a split failure is refused" 1 "$tmp/fail.log"

  rm -rf "$tmp"
  echo "NR11_EVAL_SELFTEST checks=${checks} failures=${fails} result=$([[ $fails == 0 ]] && echo ok || echo fail)"
  [[ $fails == 0 ]]
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test; exit $?
fi

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <serial-log> [...] | --self-test" >&2
  exit 2
fi

rc=0
for log in "$@"; do
  if [[ ! -f "$log" ]]; then
    echo "NR11_EVAL log=${log} result=fail reason=missing_log"; rc=1; continue
  fi
  eval_log "$log" || rc=1
done
exit $rc
