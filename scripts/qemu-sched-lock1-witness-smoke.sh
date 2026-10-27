#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# SCHED-LOCK1 — the independence and wake witness, on one port, default-off.
#
# Builds the port's production AP-scheduling witness workload with `<port>-sched-lock1-witness`
# (through that port's own apsched runner and the BUILD-GATES1 checked build path), boots it, and
# grades TWO things separately:
#
#   * the AP-scheduling workload itself (`scripts/grade-apsched-witness.py`, unchanged) — the
#     witness must not have broken the scheduling it observes;
#   * the SCHED-LOCK1 records (`scripts/grade-sched-lock1-witness.py`): SYNCHRONIZED holds (CPU A
#     inside its own per-CPU scheduler lock while CPU B commits and its task progresses, for every
#     ordered pair of the schedule) and the NATURAL membership trace (replayed SNAP → CENSUS: no
#     lost or duplicated wake, one placement per task, claims and notifications matched).
#
#   ARCH=x86_64|aarch64|riscv64   SMP=N (2..8)   LOGDIR=...   SKIP_BUILD=1
#   HOST_LOAD=idle|loaded         a LABEL of the host condition the caller arranged; recorded only
#   TIMEOUT_SECS=...              boot budget passed to the port's runner; default 2400 from 4
#                                 CPUs (QEMU TCG with several vCPUs and masked-interrupt holds is
#                                 slow: an x86 4-CPU run needed 1041 s, over its runner's 900 s),
#                                 else the runner's own default
set -uo pipefail
cd "$(dirname "$0")/.."
ARCH=${ARCH:?ARCH=x86_64|aarch64|riscv64}
SMP=${SMP:-2}
HOST_LOAD=${HOST_LOAD:-idle}
if [[ -z "${TIMEOUT_SECS:-}" ]] && (( SMP >= 4 )); then export TIMEOUT_SECS=2400; fi
LOGDIR=${LOGDIR:-/tmp/qemu-sched-lock1-witness-$ARCH-smp$SMP}
case "$ARCH" in
  x86_64) FEATURE=x86-sched-lock1-witness ;;
  aarch64) FEATURE=aarch64-sched-lock1-witness ;;
  riscv64) FEATURE=riscv64-sched-lock1-witness ;;
  *) echo "SL1W_SEAL arch=$ARCH result=fail reason=unsupported_arch"; exit 2 ;;
esac
mkdir -p "$(dirname "$LOGDIR")"
seal() { echo "SL1W_SEAL arch=$ARCH smp=$SMP host=$HOST_LOAD $*"; }
python3 -I scripts/grade-sched-lock1-witness.py --self-test >"$LOGDIR.selftest.txt" 2>&1 \
  || { cat "$LOGDIR.selftest.txt"; seal "result=fail reason=grader_self_test"; exit 1; }
tail -1 "$LOGDIR.selftest.txt"

APSW_FEATURE=$FEATURE LOGDIR="$LOGDIR" SMP=$SMP QEMU_SMP=$SMP \
  "scripts/qemu-$ARCH-apsched-witness-smoke.sh" >"$LOGDIR.apsched.txt" 2>&1
apsw_rc=$?
grep -a "ARTIFACT_BINDING \|APSW_WITNESS_SEAL\|\[apsched-witness\] boot" "$LOGDIR.apsched.txt" | tail -4
NORM="$LOGDIR/boot.norm.log"
[[ -s "$NORM" ]] || { seal "result=fail reason=no_boot_log apsched_rc=$apsw_rc"; exit 1; }
grade="$(python3 -I scripts/grade-sched-lock1-witness.py --expect-cpus "$SMP" "$NORM")"
grade_rc=$?
echo "$grade"
if (( apsw_rc != 0 )); then
  seal "result=fail reason=workload apsched_rc=$apsw_rc sl1_rc=$grade_rc"
  exit 1
fi
if (( grade_rc != 0 )); then
  seal "result=fail reason=grade"
  exit 1
fi
seal "result=pass"
