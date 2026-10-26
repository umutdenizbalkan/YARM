#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# VM-INVALIDATION2 — the x86_64 dispatch-gap TLB witness (ACCEPTANCE revision), on QEMU q35 /
# qemu64 / -smp N.
#
#   LOGDIR=...        build + boot logs (default /tmp/qemu-x86_64-vmi2-witness)
#   SMP=N             CPUs (default 2); the observer thread lands on a secondary, the mutator on CPU 0
#   SKIP_BUILD=1      reuse $LOGDIR/build
#   TIMEOUT_SECS=...  boot ceiling (default 420)
#
# Built with `x86-vmi2-witness`, armed by `yarm.x86_64_vmi2_witness=1`. The verdict is the standalone
# grader's (`scripts/grade-vmi2-witness.py`): against its PINNED contract (8 complete rounds, >= 4
# credited gaps — never the log's own declaration) it validates every round from the COMPLETE
# per-round records — the recorded observer identity, the shootdown owner's OWN target set naming
# that observer CPU whenever residency obliges it, a genuine acknowledged replacement, and the
# recorded event order — before deciding gap credit. This smoke claims only that the production
# target-set decision is correct at the real window; the witness observes the final ACK and hold
# disposition, while reclaim-after-ACK ordering rests on source and hosted tests, and the
# architectural retention of a stale entry is QEMU's model, not silicon's.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-x86_64-vmi2-witness}
SMP=${SMP:-2}
TIMEOUT_SECS=${TIMEOUT_SECS:-420}
KTARGET=targets/x86_64-yarm-none.json
KPROFILE=x86-none
BUILD_STD=core,alloc,compiler_builtins,panic_abort
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
NORM="$LOGDIR/boot.norm.log"
seal_fail() { echo "VMI2_WITNESS_SMOKE arch=x86_64 smp=$SMP result=fail reason=$1"; exit 1; }

python3 -I scripts/grade-vmi2-witness.py --self-test || seal_fail grader_self_test

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "[vmi2-witness] building base artifacts into $BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/build.log" 2>&1 || seal_fail base_build
  echo "[vmi2-witness] building kernel_boot with --features x86-vmi2-witness"
  source scripts/lib/artifact-binding.sh
  artifact_build_kernel x86_64 "--no-default-features --features x86-vmi2-witness" \
    "$BUILD_DIR/kernel_boot.elf" >"$LOGDIR/kbuild.log" 2>&1 || seal_fail kernel_build
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l) kernel_features=--no-default-features --features x86-vmi2-witness"
    echo "qemu=$(qemu-system-x86_64 --version | head -n1) machine=q35 cpu=qemu64 m=512M smp=$SMP"
    sha256sum "$BUILD_DIR/kernel_boot.elf" "$BUILD_DIR/initramfs-core.cpio"
  } >"$LOGDIR/artifact-identity.txt"
fi
[[ -s "$BUILD_DIR/kernel_boot.elf" && -s "$BUILD_DIR/initramfs-core.cpio" ]] || seal_fail no_artifacts
# BUILD-GATES1: boot only the recorded, no-FP-checked artifacts this run expects (refuses before
# QEMU on a missing record, a replaced or cached image from other source, failed check or features).
source scripts/lib/artifact-binding.sh
artifact_boot_guard x86_64 "$BUILD_DIR/kernel_boot.elf" "$BUILD_DIR/initramfs-core.cpio" "kernel=x86-vmi2-witness" "initramfs=none" >"$LOGDIR/artifact-binding.txt" 2>&1
_ab_rc=$?
cat "$LOGDIR/artifact-binding.txt"
(( _ab_rc == 0 )) || seal_fail artifact_binding

APPEND="console=ttyS0 rdinit=/init yarm.x86_64_vmi2_witness=1${APPEND:+ $APPEND}"
echo "[vmi2-witness] booting -smp $SMP (ceiling ${TIMEOUT_SECS}s)"
python3 - "$BUILD_DIR" "$BOOT_LOG" "$TIMEOUT_SECS" "$SMP" "$APPEND" <<'PY'
import subprocess, sys, time, os, select
build, log, budget, smp, append = sys.argv[1:6]
budget = float(budget)
args = ["qemu-system-x86_64", "-machine", "q35", "-cpu", "qemu64", "-m", "512M", "-smp", smp,
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
            if seen is None and b"VMI2_DUMP_END" in tail:
                seen = time.time()
        now = time.time()
        if (seen is not None and now - seen > 3) or now - start > budget:
            break
p.kill(); p.wait()
print("[vmi2-witness] boot %s after %.1fs" % ("dumped" if seen else "TIMED OUT", time.time() - start))
PY

[[ -s "$BOOT_LOG" ]] || seal_fail no_boot_log
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
if grep -a -q "KERNEL PANIC\|RUST PANIC\|panicked at\|DOUBLE FAULT\|BOOTSTRAP_ERROR\|VMI2_PROVISION_FAIL\|VMI2_SPAWN_FAIL" "$NORM"; then
  seal_fail fatal_boot_marker
fi
python3 -I scripts/grade-vmi2-witness.py "$NORM"
rc=$?
if [[ $rc -eq 0 ]]; then
  echo "VMI2_WITNESS_SMOKE arch=x86_64 smp=$SMP result=ok"
else
  echo "VMI2_WITNESS_SMOKE arch=x86_64 smp=$SMP result=fail reason=graded_checks"
  exit 1
fi
