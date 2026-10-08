#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-LOCK2 — real contention on the production VM address-space lock (`vm_state_lock`, rank 5,
# `SpinLockIrq`) in both directions on AArch64, and the reschedule SGI deferred through the holder's
# masked hold, driven by the SMP2 two-CPU VM/SGI workload on QEMU virt / cortex-a72 / 1024M /
# -smp 2 with the existing GICv2 and `yarm.ap_user_dispatch=1`.
#
# One boot of a kernel (and image set) built with `aarch64-lock2-witness`, which co-enables
# `aarch64-smp2-witness`. The grader re-derives every round from the raw sealed `LOCK2_REC` lines
# and additionally requires the SMP2 seal on the same boot (progress, results, context, and the SGI
# send/claim/completion population).
#
# Usage: scripts/qemu-aarch64-lock2-witness-smoke.sh
#   LOGDIR=...     where the build, artifact identity and boot log land (default per run)
#   SKIP_BUILD=1   reuse the artifacts already in $LOGDIR/build
#   REGRADE=1      grade the existing $LOGDIR/boot.log again without booting (implies SKIP_BUILD)
#   TIMEOUT_SECS   boot budget (default 240)
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-aarch64-lock2-witness}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
QEMU_BIN=${QEMU_BIN:-qemu-system-aarch64}
FEATURES=aarch64-lock2-witness
CMDLINE="yarm.ap_user_dispatch=1"

if [[ "${SKIP_BUILD:-0}" != "1" && "${REGRADE:-0}" != "1" ]]; then
  echo "[lock2-witness] building aarch64 artifacts with $FEATURES into $BUILD_DIR"
  rm -rf "$BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $FEATURES" \
    scripts/build-qemu-aarch64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || { echo "LOCK2_WITNESS_SEAL result=fail reason=build"; exit 1; }
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l) features=$FEATURES"
    echo "qemu=$($QEMU_BIN --version | head -n1) machine=virt cpu=cortex-a72 m=1024M smp=2 gic=v2 cmdline=$CMDLINE"
    sha256sum "$BUILD_DIR/yarm-aarch64.bin" "$BUILD_DIR/yarm-aarch64.elf" "$BUILD_DIR/initramfs-core.cpio"
  } >"$LOGDIR/artifact-identity.txt"
fi

if [[ "${REGRADE:-0}" != "1" ]]; then
  echo "[lock2-witness] booting -smp 2 yarm.ap_user_dispatch=1"
  python3 - "$QEMU_BIN" "$BUILD_DIR" "$BOOT_LOG" "${TIMEOUT_SECS:-240}" "$CMDLINE" <<'PY'
import subprocess, sys, time, os, select
qemu, build, log, budget, cmdline = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4]), sys.argv[5]
args = [qemu, "-machine", "virt", "-cpu", "cortex-a72", "-m", "1024M", "-smp", "2",
        "-nographic", "-monitor", "none", "-serial", "stdio", "-no-reboot", "-no-shutdown",
        "-kernel", os.path.join(build, "yarm-aarch64.bin"),
        "-initrd", os.path.join(build, "initramfs-core.cpio"),
        "-append", cmdline]
p = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
start, seen, buf, done = time.time(), None, b"", 0
tag = b"LOCK2_DUMP_DONE"
with open(log, "wb") as out:
    while True:
        r, _, _ = select.select([p.stdout], [], [], 0.5)
        if r:
            chunk = os.read(p.stdout.fileno(), 65536)
            if not chunk:
                break
            out.write(chunk); out.flush()
            buf += chunk
            done += buf.count(tag)
            buf = buf[-(len(tag) - 1):] if not buf.endswith(tag) else b""
            if seen is None and done >= 2:
                seen = time.time()
        now = time.time()
        if (seen is not None and now - seen > 3) or now - start > budget:
            break
p.kill(); p.wait()
print("[lock2-witness] boot %s after %.1fs" % ("sealed" if seen else "TIMED OUT", time.time() - start))
PY
fi

python3 scripts/grade-aarch64-lock2-witness.py "$BOOT_LOG" "$LOGDIR/artifact-identity.txt"
