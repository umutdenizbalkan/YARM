#!/usr/bin/env bash
# FUTEX-WAIT-ATOMIC1 qualification marker scan (the matrix's Z step).
#   fwa_zscan.sh --self-test        positive/negative cases for the pattern; rc 0 when all hold
#   fwa_zscan.sh <file>...          prints "<count> <file>" for every file with a hit
#
# The pattern is FUTEX-REQUEUE-ATOMIC1's list with one correction: its bare `race: ` alternative
# also matched the SCHED-LOCK1 grader's innocent "NATURAL trace: ..." summary. A race diagnostic
# is now matched only as a word — `\brace: ` — which still catches every grader `race:` failure
# (`[futex-requeue-grade][fail] race: ...`) and a bare `race:` line, and no longer `trace:`.
set -u
PATTERN='SCHED_WAKE_FAIL|SCHED_TIMEOUT_STRANDED_WAITER|IPC_RECV_BLOCKED_COMPLETE_FAILED|x86 shared trap dispatch failed|YARM_AARCH64_TRAP_HANDLE halting|RISCV_TRAP_HANDLE_FAILED|U6_SEND_COMPLETION_REFUSED_STALE|U7_SEND_TIMEOUT_CLAIM_AFTER_WITHDRAW_REFUSED|SEND_ARB_PUBLICATION_REFUSED_UNWIND|endpoint=Published|outcome=Published|SPAWN_LEDGER_OVERFLOW|FUTEX_WAKE_RESERVED_COMMIT_REFUSED|FUTEX_WAKE_STOPPED|FUTEX_WAIT_PARK_REFUSED|reason=capacity_exhausted|OVT_.*result=fail|SL1W.*result=fail|FUTEX_REQUEUE_SPLIT_REFUSED|FUTEX_REQUEUE_RACE_IN_WINDOW_WAKE.*woke=[1-9]|\brace: |CRQ_WITNESS.*result=fail|FUTEX_REQUEUE_WITNESS_SEAL.*result=fail|FUTEX_WAIT_WITNESS.*result=fail|FWAW_.*result=fail'

if [[ "${1:-}" == "--self-test" ]]; then
  bad=0
  hit() { printf '%s\n' "$1" | grep -qaE "$PATTERN"; }
  for pos in \
    "[futex-requeue-grade][fail] race: window 2 never closed" \
    "[futex-requeue-grade][fail] race: a wake INSIDE the window took a waiter the commit had decided: {'woke': '1'}" \
    "race: 3 held windows for 2 broadcasts" \
    "FUTEX_REQUEUE_RACE_IN_WINDOW_WAKE cpu=1 tid=10018 window=1 woke=1" \
    "FUTEX_WAIT_WITNESS_SEAL arch=x86_64 smp=2 result=fail" \
    "SL1W_SEAL arch=x86_64 smp=2 host=idle result=fail"; do
    if hit "$pos"; then echo "[zscan-self-test] ok   hit:  $pos"; else echo "[zscan-self-test] FAIL miss: $pos"; bad=1; fi
  done
  for neg in \
    "SL1W_GRADE result=pass ncpu=2 | NATURAL trace: 145 sections (20 remote adds) replayed SNAP->CENSUS exactly" \
    "embrace: nothing" \
    "FUTEX_REQUEUE_RACE_IN_WINDOW_WAKE cpu=1 tid=10018 window=1 woke=0" \
    "FUTEX_WAIT_WITNESS_SEAL arch=x86_64 smp=2 result=ok"; do
    if hit "$neg"; then echo "[zscan-self-test] FAIL hit:  $neg"; bad=1; else echo "[zscan-self-test] ok   miss: $neg"; fi
  done
  [[ $bad == 0 ]] && echo "[zscan-self-test] all cases hold"
  exit $bad
fi

for f in "$@"; do
  n=$(grep -acE "$PATTERN" "$f")
  [[ $n -gt 0 ]] && echo "$n $f"
done
exit 0
