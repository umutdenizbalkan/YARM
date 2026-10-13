#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# AP-SCHED1 — the RISC-V PRODUCTION AP-scheduling witness, on QEMU virt / rv64 / 512M / -smp 2
# with the PINNED OpenSBI (`scripts/firmware/riscv64-opensbi.pin`, booted from this run's own copy)
# and WITHOUT `yarm.ap_user_dispatch`: logical CPU 1 is admitted by the production path, never the
# diagnostic pinned one.
#
# Usage: scripts/qemu-riscv64-apsched-witness-smoke.sh
#   LOGDIR=...     where the build, the artifact identity and the boot log land
#   SKIP_BUILD=1   reuse the artifacts already in $LOGDIR/build
#   TIMEOUT_SECS   boot budget (default 600)
#
# One App-class process P; its threads and fork children are placed by the production balanced
# placement. The grader (`scripts/grade-riscv64-apsched-witness.py`) orders every witness record by
# its causal stamp, never by console position, and requires: ordinary work on both CPUs; timer
# preemption of non-yielding spinners on CPU 1 (and CPU 0) with every patterned register intact; a
# deadline whose global-tick advance equals CPU 0's timer interrupts alone while CPU 1 took its
# own; every sleeper wake delivered to IDLE CPU 1 by an IPI from CPU 0 and dispatched from it; IPC
# completing in both directions across CPUs; every churn child run, exited and reaped with its
# address space destroyed and none quarantined; and progress to the end.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-riscv64-apsched-witness}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
QEMU_BIN=${QEMU_BIN:-qemu-system-riscv64}
KELF=target/riscv64gc-unknown-none-elf/release/kernel_boot
FEATURE=riscv64-apsched-witness
PIN=scripts/firmware/riscv64-opensbi.pin
pin() { sed -n "s/^$1=//p" "$PIN"; }
FIRMWARE=$(pin path)
BOOT_FW="$BUILD_DIR/opensbi-fw_dynamic.bin"
CMDLINE="console=ttyS0 rdinit=/init"

seal() { echo "APSW_WITNESS_SEAL arch=riscv64 $*"; }

python3 -I scripts/grade-riscv64-apsched-witness.py --self-test || { seal "result=fail reason=grader_self_test"; exit 1; }

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "[apsched-witness] building riscv64 artifacts, then the kernel with $FEATURE, into $BUILD_DIR"
  rm -rf "$BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-riscv64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || { seal "result=fail reason=base_build"; exit 1; }
  cargo build -Z build-std=core,alloc,compiler_builtins,panic_abort \
    --target riscv64gc-unknown-none-elf --profile release \
    --no-default-features --features "$FEATURE" -p yarm --bin kernel_boot \
    >"$LOGDIR/kbuild.log" 2>&1 || { seal "result=fail reason=kernel_build"; exit 1; }
  [[ -f "$FIRMWARE" ]] || { seal "result=fail reason=firmware_missing"; exit 1; }
  [[ "$(sha256sum "$FIRMWARE" | cut -d' ' -f1)" == "$(pin sha256)" ]] \
    || { seal "result=fail reason=firmware_substituted"; exit 1; }
  cp "$FIRMWARE" "$BOOT_FW"
  cp "$KELF" "$BUILD_DIR/yarm-riscv64-apsw.elf"
  llvm-objcopy -O binary "$KELF" "$BUILD_DIR/yarm-riscv64-apsw.bin" \
    || { seal "result=fail reason=objcopy"; exit 1; }
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l) kernel_features=--no-default-features --features $FEATURE userspace_features=--no-default-features"
    echo "qemu=$($QEMU_BIN --version | head -n1) machine=virt cpu=rv64 smp=2 cmdline=$CMDLINE"
    echo "firmware_path=$FIRMWARE firmware_banner=$(pin banner)"
    sha256sum "$BUILD_DIR/yarm-riscv64-apsw.bin" "$BUILD_DIR/yarm-riscv64-apsw.elf" "$BUILD_DIR/initramfs-core.cpio" "$BOOT_FW"
  } >"$LOGDIR/artifact-identity.txt"
fi

echo "[apsched-witness] booting -smp 2 with no diagnostic knob"
python3 - "$QEMU_BIN" "$BUILD_DIR" "$BOOT_LOG" "${TIMEOUT_SECS:-600}" "$CMDLINE" <<'PY'
import subprocess, sys, time, os, select
qemu, build, log, budget, cmdline = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4]), sys.argv[5]
args = [qemu, "-machine", "virt", "-cpu", "rv64", "-m", "512M", "-smp", "2",
        "-nographic", "-monitor", "none", "-serial", "stdio",
        "-bios", os.path.join(build, "opensbi-fw_dynamic.bin"), "-no-reboot",
        "-kernel", os.path.join(build, "yarm-riscv64-apsw.bin"),
        "-initrd", os.path.join(build, "initramfs-core.cpio"),
        "-append", cmdline]
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
python3 -I scripts/grade-riscv64-apsched-witness.py "$NORM"
