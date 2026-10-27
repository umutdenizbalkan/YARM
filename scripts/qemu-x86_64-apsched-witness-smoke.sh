#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# AP-SCHED4 — the x86_64 PRODUCTION SMP-scheduling witness, on QEMU q35 / qemu64 / -smp N, WITHOUT
# `yarm.ap_user_dispatch`: every secondary the firmware lists is planned from the MADT, bound by
# APIC ID, and admitted by the production path.
#
# Usage: scripts/qemu-x86_64-apsched-witness-smoke.sh
#   SMP=N          CPUs (default 2); the grader expects logical CPUs 0..min(N,8)-1, from THIS number
#   TOPO=...       extra `-smp` topology, e.g. `sockets=2,cores=3,threads=1` (QEMU then gives
#                  sparse APIC IDs: each field is a power-of-two width); the grader re-derives the
#                  APIC IDs from it and grades the firmware plan against them
#   LOGDIR=...     where the build, the artifact identity and the boot log land
#   SKIP_BUILD=1   reuse the artifacts already in $LOGDIR/build
#   TIMEOUT_SECS   boot budget (default 900)
#   APPEND=...     extra kernel command line (controls only)
#
# One App-class process P (`src/arch/x86_64/apsched_witness.S`); its threads and fork children are
# placed by the production balanced placement. The grader (`scripts/grade-apsched-witness.py --arch
# x86_64`) orders every witness record by its causal stamp, never by console position, and requires
# on EVERY expected CPU: the production admission under the APIC ID the firmware plan gave it;
# ordinary user work; timer preemption of non-yielding spinners with the whole user context intact
# (13 GPRs, RSP, the user RFLAGS bits, x87 ST0..7 and FCW, MXCSR, XMM0..15, the FS-based TLS); a
# deadline whose global-tick advance equals CPU 0's timer interrupts alone; sleeper wakes delivered
# to that IDLE CPU by the reschedule IPI and dispatched from it (at least one per CPU, and no wake
# credited to its own timer); IPC in both directions with CPU 0; churn children run, faulted
# copy-on-write, exited and reaped with their address spaces destroyed and none quarantined, the CPU
# having left each child's root first; and progress to the end.
set -uo pipefail
cd "$(dirname "$0")/.."

SMP=${SMP:-2}
TOPO=${TOPO:-}
LOGDIR=${LOGDIR:-/tmp/qemu-x86_64-apsched-witness}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
QEMU_BIN=${QEMU_BIN:-qemu-system-x86_64}
# SCHED-LOCK1: `APSW_FEATURE=x86-sched-lock1-witness` runs the same workload with the
# independence/wake witness co-enabled (`scripts/qemu-sched-lock1-witness-smoke.sh`); nothing else.
FEATURE=${APSW_FEATURE:-x86-apsched-witness}
case "$FEATURE" in
  x86-apsched-witness|x86-sched-lock1-witness) ;;
  *) echo "APSW_WITNESS_SEAL arch=x86_64 result=fail reason=unsupported_feature feature=$FEATURE"; exit 2 ;;
esac
KTARGET=targets/x86_64-yarm-none.json
KPROFILE=x86-none
SMP_ARG="$SMP${TOPO:+,$TOPO}"
APPEND="console=ttyS0 rdinit=/init${APPEND:+ $APPEND}"

seal() { echo "APSW_WITNESS_SEAL arch=x86_64 $*"; }

python3 -I scripts/grade-apsched-witness.py --arch x86_64 --self-test || { seal "result=fail reason=grader_self_test"; exit 1; }

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "[apsched-witness] building x86_64 artifacts, then the kernel with $FEATURE, into $BUILD_DIR"
  rm -rf "$BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || { seal "result=fail reason=base_build"; exit 1; }
  # The feature is the kernel's alone (the servers carry no witness code): rebuild only the kernel,
  # published with its no-FP check and provenance (BUILD-GATES1).
  source scripts/lib/artifact-binding.sh
  artifact_build_kernel x86_64 "--no-default-features --features $FEATURE" "$BUILD_DIR/kernel_boot.elf" \
    >"$LOGDIR/kbuild.log" 2>&1 || { seal "result=fail reason=kernel_build"; exit 1; }
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l) kernel_features=--no-default-features --features $FEATURE"
    echo "qemu=$($QEMU_BIN --version | head -n1) machine=q35 cpu=qemu64 m=512M"
    sha256sum "$BUILD_DIR/kernel_boot.elf" "$BUILD_DIR/initramfs-core.cpio"
  } >"$LOGDIR/artifact-identity.txt"
fi
[[ -s "$BUILD_DIR/kernel_boot.elf" && -s "$BUILD_DIR/initramfs-core.cpio" ]] || { seal "result=fail reason=no_artifacts"; exit 1; }
# BUILD-GATES1: boot only the recorded, no-FP-checked artifacts this run expects (refuses before
# QEMU on a missing record, a replaced or cached image from other source, failed check or features).
source scripts/lib/artifact-binding.sh
artifact_boot_guard x86_64 "$BUILD_DIR/kernel_boot.elf" "$BUILD_DIR/initramfs-core.cpio" "kernel=$FEATURE" "initramfs=none" >"$LOGDIR/artifact-binding.txt" 2>&1
_ab_rc=$?
cat "$LOGDIR/artifact-binding.txt"
(( _ab_rc == 0 )) || { seal "result=fail reason=artifact_binding"; exit 1; }

echo "boot smp=$SMP_ARG append=$APPEND" >>"$LOGDIR/artifact-identity.txt"
echo "[apsched-witness] booting -smp $SMP_ARG with no diagnostic knob"
python3 - "$QEMU_BIN" "$BUILD_DIR" "$BOOT_LOG" "${TIMEOUT_SECS:-900}" "$SMP_ARG" "$APPEND" <<'PY'
import subprocess, sys, time, os, select
qemu, build, log, budget, smp, append = sys.argv[1:7]
budget = float(budget)
args = [qemu, "-machine", "q35", "-cpu", "qemu64", "-m", "512M", "-smp", smp,
        "-nographic", "-monitor", "none", "-serial", "stdio", "-no-reboot", "-no-shutdown",
        "-kernel", os.path.join(build, "kernel_boot.elf"),
        "-initrd", os.path.join(build, "initramfs-core.cpio"), "-append", append]
p = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
start, seen, tail = time.time(), None, b""
with open(log, "wb") as out:
    while True:
        r, _, _ = select.select([p.stdout], [], [], 0.5)
        if r:
            chunk = os.read(p.stdout.fileno(), 65536)
            if not chunk:
                break
            out.write(chunk); out.flush()
            tail = (tail + chunk)[-4096:]
            if seen is None and b"APSW_DUMP_END" in tail:
                seen = time.time()
        now = time.time()
        if (seen is not None and now - seen > 3) or now - start > budget:
            break
p.kill(); p.wait()
print("[apsched-witness] boot %s after %.1fs" % ("dumped" if seen else "TIMED OUT", time.time() - start))
PY

[[ -s "$BOOT_LOG" ]] || { seal "result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
python3 -I scripts/grade-apsched-witness.py --arch x86_64 ${TOPO:+--topo "$TOPO"} --expect-cpus "$SMP" "$NORM"
