#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# AP-SCHED3 — the AArch64 PRODUCTION SMP-scheduling witness, on QEMU virt / cortex-a72 / GICv2 /
# 1024M / -smp N, WITHOUT `yarm.ap_user_dispatch`: every secondary is admitted by the production
# path (there is no other admission on this port any more).
#
# Usage: scripts/qemu-aarch64-apsched-witness-smoke.sh
#   SMP=N          CPUs (default 2, at most 8: GICv2's CPU interfaces); the grader expects
#                  secondaries 1..N-1, from THIS number
#   LOGDIR=...     where the build, the artifact identity and the boot log land
#   SKIP_BUILD=1   reuse the artifacts already in $LOGDIR/build
#   TIMEOUT_SECS   boot budget (default 600)
#
# One App-class process P; its threads and fork children are placed by the production balanced
# placement. The grader (`scripts/grade-apsched-witness.py --arch aarch64`) orders every witness
# record by its causal stamp, never by console position, and requires on EVERY expected secondary:
# the production admission; ordinary user work; timer preemption of non-yielding spinners with the
# whole user context intact (x0..x30 but x18, SP, q0..q31, FPCR, FPSR, NZCV, TPIDR_EL0); a deadline
# whose global-tick advance equals CPU 0's timer interrupts alone while that CPU took its own; sleeper
# wakes delivered to that IDLE CPU by the reschedule SGI and dispatched from it; IPC completing in
# both directions with CPU 0; churn children run, exited and reaped with their address spaces
# destroyed and none quarantined, the CPU having left each child's root first; and progress to the
# end. A CPU that announced readiness but ran nothing fails.
set -uo pipefail
cd "$(dirname "$0")/.."

SMP=${SMP:-2}
LOGDIR=${LOGDIR:-/tmp/qemu-aarch64-apsched-witness}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
QEMU_BIN=${QEMU_BIN:-qemu-system-aarch64}
# SCHED-LOCK1: `APSW_FEATURE=aarch64-sched-lock1-witness` runs the same workload with the
# independence/wake witness co-enabled (`scripts/qemu-sched-lock1-witness-smoke.sh`); nothing else.
FEATURE=${APSW_FEATURE:-aarch64-apsched-witness}
case "$FEATURE" in
  aarch64-apsched-witness|aarch64-sched-lock1-witness) ;;
  *) echo "APSW_WITNESS_SEAL arch=aarch64 result=fail reason=unsupported_feature feature=$FEATURE"; exit 2 ;;
esac

seal() { echo "APSW_WITNESS_SEAL arch=aarch64 $*"; }

python3 -I scripts/grade-apsched-witness.py --arch aarch64 --self-test || { seal "result=fail reason=grader_self_test"; exit 1; }

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "[apsched-witness] building aarch64 artifacts, then the kernel with $FEATURE, into $BUILD_DIR"
  rm -rf "$BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-aarch64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || { seal "result=fail reason=base_build"; exit 1; }
  # The feature is the kernel's alone (the servers carry no witness code): rebuild only the kernel,
  # published as ELF + raw image with its no-FP check and provenance (BUILD-GATES1).
  source scripts/lib/artifact-binding.sh
  artifact_build_kernel aarch64 "--no-default-features --features $FEATURE" \
    "$BUILD_DIR/yarm-aarch64.elf" "$BUILD_DIR/yarm-aarch64.bin" \
    >"$LOGDIR/kbuild.log" 2>&1 || { seal "result=fail reason=kernel_build"; exit 1; }
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l) kernel_features=--no-default-features --features $FEATURE"
    echo "qemu=$($QEMU_BIN --version | head -n1) machine=virt,gic-version=2 cpu=cortex-a72 append=(none)"
    sha256sum "$BUILD_DIR/yarm-aarch64.bin" "$BUILD_DIR/yarm-aarch64.elf" "$BUILD_DIR/initramfs-core.cpio"
  } >"$LOGDIR/artifact-identity.txt"
fi

# BUILD-GATES1: boot only the recorded, no-FP-checked artifacts this run expects (refuses before
# QEMU on a missing record, a replaced or cached image from other source, failed check or features).
source scripts/lib/artifact-binding.sh
artifact_boot_guard aarch64 "$BUILD_DIR/yarm-aarch64.bin" "$BUILD_DIR/initramfs-core.cpio" "kernel=$FEATURE" "initramfs=none" >"$LOGDIR/artifact-binding.txt" 2>&1
_ab_rc=$?
cat "$LOGDIR/artifact-binding.txt"
(( _ab_rc == 0 )) || { seal "result=fail reason=artifact_binding"; exit 1; }
echo "boot smp=$SMP" >>"$LOGDIR/artifact-identity.txt"
echo "[apsched-witness] booting -smp $SMP with no diagnostic knob"
python3 - "$QEMU_BIN" "$BUILD_DIR" "$BOOT_LOG" "${TIMEOUT_SECS:-600}" "$SMP" <<'PY'
import subprocess, sys, time, os, select
qemu, build, log, budget, smp = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4]), sys.argv[5]
args = [qemu, "-machine", "virt,gic-version=2", "-cpu", "cortex-a72", "-m", "1024M", "-smp", smp,
        "-nographic", "-monitor", "none", "-serial", "stdio", "-no-reboot", "-no-shutdown",
        "-kernel", os.path.join(build, "yarm-aarch64.bin"),
        "-initrd", os.path.join(build, "initramfs-core.cpio")]
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
python3 -I scripts/grade-apsched-witness.py --arch aarch64 --expect-cpus "$SMP" "$NORM"
