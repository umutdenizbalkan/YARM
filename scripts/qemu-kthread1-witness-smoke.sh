#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# KTHREAD1 — the kernel-thread witness, on one port, default-off.
#
# Builds the port's production AP-scheduling witness workload with `<port>-kthread1-witness`
# (through that port's own apsched runner and the BUILD-GATES1 checked build path), boots it until
# BOTH dumps are out, and grades two things separately:
#
#   * the AP-scheduling workload itself (`scripts/grade-apsched-witness.py`, unchanged) — user tasks
#     on every admitted CPU must still make the progress that witness requires while kernel threads
#     run beside them;
#   * the KT1 records (`scripts/grade-kthread1-witness.py`): kernel threads on every CPU of the
#     declared population, kernel-origin timer and reschedule interrupts on them with every
#     register intact, cooperative alternation with private stacks, remote wake through the
#     reschedule IPI to the intended thread, user progress, switch-safety refusals, the lost-wakeup
#     and stale-id probes, and create/exit/reuse with reclamation only after the switch away.
#
#   ARCH=x86_64|aarch64|riscv64   SMP=N (1..8)   LOGDIR=...   SKIP_BUILD=1
#   At SMP=1 the AP-scheduling grade (which grades secondary CPUs) is reported, not required; the
#   KT1 grade still requires user progress on the one CPU.
#   HOST_LOAD=idle|loaded         a LABEL of the host condition the caller arranged; recorded only
#   TIMEOUT_SECS=...              boot budget passed to the port's runner (default 2400 from 4 CPUs)
set -uo pipefail
cd "$(dirname "$0")/.."
ARCH=${ARCH:?ARCH=x86_64|aarch64|riscv64}
SMP=${SMP:-2}
HOST_LOAD=${HOST_LOAD:-idle}
if [[ -z "${TIMEOUT_SECS:-}" ]] && (( SMP >= 4 )); then export TIMEOUT_SECS=2400; fi
LOGDIR=${LOGDIR:-/tmp/qemu-kthread1-witness-$ARCH-smp$SMP}
case "$ARCH" in
  x86_64) FEATURE=x86-kthread1-witness ;;
  aarch64) FEATURE=aarch64-kthread1-witness ;;
  riscv64) FEATURE=riscv64-kthread1-witness ;;
  *) echo "KT1W_SEAL arch=$ARCH result=fail reason=unsupported_arch"; exit 2 ;;
esac
mkdir -p "$(dirname "$LOGDIR")"
seal() { echo "KT1W_SEAL arch=$ARCH smp=$SMP host=$HOST_LOAD $*"; }
python3 -I scripts/grade-kthread1-witness.py --self-test >"$LOGDIR.selftest.txt" 2>&1 \
  || { cat "$LOGDIR.selftest.txt"; seal "result=fail reason=grader_self_test"; exit 1; }
tail -1 "$LOGDIR.selftest.txt"

APSW_FEATURE=$FEATURE APSW_ALSO_STOP=KT1_DUMP_END LOGDIR="$LOGDIR" SMP=$SMP QEMU_SMP=$SMP \
  "scripts/qemu-$ARCH-apsched-witness-smoke.sh" >"$LOGDIR.apsched.txt" 2>&1
apsw_rc=$?
grep -a "ARTIFACT_BINDING \|APSW_WITNESS_SEAL\|\[apsched-witness\] boot" "$LOGDIR.apsched.txt" | tail -4
NORM="$LOGDIR/boot.norm.log"
[[ -s "$NORM" ]] || { seal "result=fail reason=no_boot_log apsched_rc=$apsw_rc"; exit 1; }
grade="$(python3 -I scripts/grade-kthread1-witness.py --expect-cpus "$SMP" "$NORM")"
grade_rc=$?
echo "$grade"
if (( apsw_rc != 0 && SMP > 1 )); then
  seal "result=fail reason=workload apsched_rc=$apsw_rc kt1_rc=$grade_rc"
  exit 1
fi
if (( grade_rc != 0 )); then
  seal "result=fail reason=grade"
  exit 1
fi
seal "result=pass apsched_rc=$apsw_rc"
