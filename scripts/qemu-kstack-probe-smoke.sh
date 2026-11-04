#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# KTHREAD-CLOSURE1 §C — one live kernel-thread stack overflow probe, fatal by design.
#
#   ARCH=x86_64|aarch64|riscv64 KIND=access|recursion|frame [SMP=2] [LOGDIR=...] \
#     scripts/qemu-kstack-probe-smoke.sh
#
# Builds the port's KT1 witness kernel with `kstack-probe-$KIND` (through the AP-scheduling
# runner, so BUILD-GATES1 binds the boot to its artifacts), boots it until the first
# KTHREAD_STACK_OVERFLOW, KTHREAD_FAULT or KSTACK_PROBE_SURVIVED line, and passes only when:
#   * the probe announced itself (KSTACK_PROBE_START: kind, kthread, slot, stack bounds, gap);
#   * exactly one KTHREAD_STACK_OVERFLOW was reported, naming that kthread and slot, with its
#     guard address inside the probe's own gap [gap_low, low), result=fatal;
#   * no KTHREAD_FAULT and no KSTACK_PROBE_SURVIVED line.
set -u
ARCH=${ARCH:?ARCH}; KIND=${KIND:?KIND}; SMP=${SMP:-2}
LOGDIR=${LOGDIR:-/tmp/qemu-kstack-probe-$ARCH-$KIND}
case "$ARCH" in
  x86_64) BASE=x86-kthread1-witness ;;
  aarch64) BASE=aarch64-kthread1-witness ;;
  riscv64) BASE=riscv64-kthread1-witness ;;
  *) echo "KSTACK_PROBE_SEAL arch=$ARCH result=fail reason=unsupported_arch"; exit 2 ;;
esac
case "$KIND" in access|recursion|frame) ;; *) echo "KSTACK_PROBE_SEAL result=fail reason=unknown_kind"; exit 2 ;; esac
mkdir -p "$(dirname "$LOGDIR")"
APSW_FEATURE="$BASE,kstack-probe-$KIND" APSW_STOP_ON="KTHREAD_STACK_OVERFLOW|KTHREAD_FAULT|KSTACK_PROBE_SURVIVED" \
  TIMEOUT_SECS=${TIMEOUT_SECS:-600} LOGDIR="$LOGDIR" SMP=$SMP QEMU_SMP=$SMP \
  "scripts/qemu-$ARCH-apsched-witness-smoke.sh" >"$LOGDIR.boot.txt" 2>&1
grep -a "ARTIFACT_BINDING \|\[apsched-witness\] boot" "$LOGDIR.boot.txt" | tail -2
python3 -I - "$LOGDIR/boot.log" "$ARCH" "$KIND" <<'PY'
import re, sys
text = open(sys.argv[1], "rb").read().decode("utf-8", "replace")
arch, kind = sys.argv[2], sys.argv[3]
fails = []
start = re.findall(r"KSTACK_PROBE_START kind=(\w+) kthread=(0x[0-9a-f]+) slot=(\d+) low=(0x[0-9a-f]+) high=(0x[0-9a-f]+) gap_low=(0x[0-9a-f]+)", text)
over = re.findall(r"KTHREAD_STACK_OVERFLOW arch=(\w+) cpu=(\d+) kthread=(0x[0-9a-f]+) slot=(\d+) guard=(0x[0-9a-f]+) sp=(0x[0-9a-f]+) pc=(0x[0-9a-f]+) cause=(0x[0-9a-f]+) result=(\w+)", text)
if len(start) != 1:
    fails.append("probe start lines: %d" % len(start))
if len(over) != 1:
    fails.append("overflow reports: %d" % len(over))
for bad in ("KTHREAD_FAULT", "KSTACK_PROBE_SURVIVED"):
    if bad in text:
        fails.append("%s reported: %s" % (bad, re.search(bad + r"[^\n]*", text).group(0)[:200]))
if len(start) == 1 and len(over) == 1:
    k, kid, slot, low, high, gap_low = start[0]
    a, cpu, okid, oslot, guard, sp, pc, cause, res = over[0]
    if k != kind: fails.append("probe kind %s, want %s" % (k, kind))
    if a != arch: fails.append("report arch %s" % a)
    if okid != kid or oslot != slot: fails.append("report names %s slot %s, the probe is %s slot %s" % (okid, oslot, kid, slot))
    if not int(gap_low, 16) <= int(guard, 16) < int(low, 16):
        fails.append("guard %s outside the probe's gap [%s, %s)" % (guard, gap_low, low))
    if res != "fatal": fails.append("result=%s" % res)
for f in fails:
    print("[kstack-probe][fail] " + f)
summary = "arch=%s kind=%s" % (arch, kind)
if len(over) == 1:
    summary += " kthread=%s guard=%s sp=%s pc=%s" % (over[0][2], over[0][4], over[0][5], over[0][6])
print("KSTACK_PROBE_SEAL %s result=%s" % (summary, "fail" if fails else "pass"))
sys.exit(1 if fails else 0)
PY
