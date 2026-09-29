#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-SMP3-ACCEPTANCE §2 — the AArch64 overtaken-deferral witness: one strict two-CPU boot of a
# kernel built with `aarch64-overtaken-witness` and `yarm.ap_user_dispatch=1`, graded by
# `grade-overtaken-witness.py` from the kernel's sealed summary.
#
# Usage: LOGDIR=... scripts/qemu-aarch64-overtaken-witness-smoke.sh
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-aarch64-overtaken-witness}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
QEMU_BIN=${QEMU_BIN:-qemu-system-aarch64}
CMDLINE="yarm.ap_user_dispatch=1"

if [[ "${REGRADE:-0}" != "1" ]]; then
  echo "[overtaken-witness] building aarch64 artifacts with aarch64-overtaken-witness into $BUILD_DIR"
  rm -rf "$BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features --features aarch64-overtaken-witness" \
    scripts/build-qemu-aarch64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || { echo "OVT_WITNESS_SEAL arch=aarch64 result=fail reason=build"; exit 1; }
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l) features=aarch64-overtaken-witness cmdline=$CMDLINE"
    echo "qemu=$($QEMU_BIN --version | head -1) machine=virt cpu=cortex-a72 smp=2"
    sha256sum "$BUILD_DIR/yarm-aarch64.bin" "$BUILD_DIR/initramfs-core.cpio"
  } >"$LOGDIR/artifact-identity.txt"

  echo "[overtaken-witness] booting -smp 2 $CMDLINE"
  python3 - "$QEMU_BIN" "$BUILD_DIR" "$BOOT_LOG" "${TIMEOUT_SECS:-240}" "$CMDLINE" <<'PY'
import subprocess, sys, time, os, select, re
qemu, build, log, budget, cmdline = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4]), sys.argv[5]
args = [qemu, "-machine", "virt", "-cpu", "cortex-a72", "-m", "1024M", "-smp", "2",
        "-nographic", "-monitor", "none", "-serial", "stdio", "-no-reboot", "-no-shutdown",
        "-kernel", os.path.join(build, "yarm-aarch64.bin"),
        "-initrd", os.path.join(build, "initramfs-core.cpio"),
        "-append", cmdline]
p = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
start, seen, buf, passes = time.time(), None, b"", 0
with open(log, "wb") as out:
    while True:
        r, _, _ = select.select([p.stdout], [], [], 0.5)
        if r:
            chunk = os.read(p.stdout.fileno(), 65536)
            if not chunk:
                break
            out.write(chunk); out.flush(); buf = (buf + chunk)[-8192:]
            if seen is None and re.search(rb"OVT_SUM part=(\d+)/\1 [^\n]* pass=2", buf):
                seen = time.time()
        now = time.time()
        if (seen is not None and now - seen > 2) or now - start > budget:
            break
p.kill(); p.wait()
print("[overtaken-witness] boot %s after %.1fs" % ("sealed" if seen else "TIMED OUT", time.time() - start))
PY
fi

python3 scripts/grade-overtaken-witness.py "$BOOT_LOG" aarch64
