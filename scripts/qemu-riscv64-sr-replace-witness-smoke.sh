#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# SHARED-REGION-LIFETIME2 — the RISC-V shared-region replacement witness, on QEMU virt / rv64 /
# 512M / -smp 2 with the PINNED OpenSBI (`scripts/firmware/riscv64-opensbi.pin`, booted from this
# run's own copy) and `yarm.ap_user_dispatch=1`.
#
# Usage: scripts/qemu-riscv64-sr-replace-witness-smoke.sh
#   LOGDIR=...     where the build, the artifact identity and the boot log land
#   SKIP_BUILD=1   reuse the artifacts already in $LOGDIR/build
#   TIMEOUT_SECS   boot budget (default 300)
#
# Two kernel-built tasks share one address space: the observer O (secondary hart, CPU 1) caches
# the translation of the window W and spins on it with no kernel entry, and the receiver R (boot
# hart, CPU 0) receives a one-page shared region with NR 2 mapped AT W — so the receive's install
# REPLACES a page whose translation is live on the other hart.
#
# Graded SEPARATELY:
#   * OBSERVED BYTES — R reads the granted bytes through its own mapping (R_OK); O, which took NO
#     supervisor entry between priming W and its observation, sees the granted byte at both ends
#     of the page (O_NEW, credited) — so its cached translation was retired by the remote fence.
#   * KERNEL COMPLETION — the production install at W: requester CPU 0, the production shootdown's
#     own target set INCLUDES CPU 1 (`targets=0x2`), the firmware fence succeeded, the outcome is
#     `complete`, and only then the displaced page's hold settled and reclaimed exactly its own
#     object (`SRR_KERNEL_SETTLE ... reclaimed=1`, object = the provisioned one).
#   * ORDER BY CAUSE, NOT BY CONSOLE (AP-SCHED1) — every ordering obligation is checked on the
#     witness's `seq=` stamps (scripts/grade-riscv64-sr-replace-witness.py): O primed before the
#     invalidation was planned, the target set preceded the fence request, the fence completed
#     before the outcome, O's observation follows the fence request, R's read and the settlement
#     follow the outcome. O's line may be PRINTED before the install line: that line is printed
#     only once the outcome is known, after O's hart has already been fenced.
#   * NOTHING ELSE: no incomplete invalidation, no retained hold, no refused displacement, no
#     panic.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-riscv64-sr-replace-witness}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
QEMU_BIN=${QEMU_BIN:-qemu-system-riscv64}
KELF=target/riscv64gc-unknown-none-elf/release/kernel_boot
FEATURE=riscv64-sr-replace-witness
PIN=scripts/firmware/riscv64-opensbi.pin
pin() { sed -n "s/^$1=//p" "$PIN"; }
FIRMWARE=$(pin path)
BOOT_FW="$BUILD_DIR/opensbi-fw_dynamic.bin"
CMDLINE="console=ttyS0 rdinit=/init yarm.ap_user_dispatch=1"

seal() { echo "SRR_WITNESS_SEAL arch=riscv64 $*"; }

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "[srr-witness] building riscv64 artifacts, then the kernel with $FEATURE, into $BUILD_DIR"
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
  cp "$KELF" "$BUILD_DIR/yarm-riscv64-srr.elf"
  llvm-objcopy -O binary "$KELF" "$BUILD_DIR/yarm-riscv64-srr.bin" \
    || { seal "result=fail reason=objcopy"; exit 1; }
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l) kernel_features=--no-default-features --features $FEATURE userspace_features=--no-default-features"
    echo "qemu=$($QEMU_BIN --version | head -n1) machine=virt cpu=rv64 smp=2 cmdline=$CMDLINE"
    echo "firmware_path=$FIRMWARE firmware_banner=$(pin banner)"
    sha256sum "$BUILD_DIR/yarm-riscv64-srr.bin" "$BUILD_DIR/yarm-riscv64-srr.elf" "$BUILD_DIR/initramfs-core.cpio" "$BOOT_FW"
  } >"$LOGDIR/artifact-identity.txt"
fi

echo "[srr-witness] booting -smp 2 yarm.ap_user_dispatch=1"
python3 - "$QEMU_BIN" "$BUILD_DIR" "$BOOT_LOG" "${TIMEOUT_SECS:-300}" "$CMDLINE" <<'PY'
import subprocess, sys, time, os, select
qemu, build, log, budget, cmdline = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4]), sys.argv[5]
args = [qemu, "-machine", "virt", "-cpu", "rv64", "-m", "512M", "-smp", "2",
        "-nographic", "-monitor", "none", "-serial", "stdio",
        "-bios", os.path.join(build, "opensbi-fw_dynamic.bin"), "-no-reboot",
        "-kernel", os.path.join(build, "yarm-riscv64-srr.bin"),
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
            if seen is None and b"SRR_VERDICT" in tail:
                seen = time.time()
        now = time.time()
        if (seen is not None and now - seen > 3) or now - start > budget:
            break
p.kill(); p.wait()
print("[srr-witness] boot %s after %.1fs" % ("sealed" if seen else "TIMED OUT", time.time() - start))
PY

[[ -s "$BOOT_LOG" ]] || { seal "result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
# AP-SCHED1: the grader orders by the witness's causal stamps, not console positions; its own
# fixture controls run first, so a grader that stopped rejecting a known-bad boot cannot pass one.
python3 -I scripts/grade-riscv64-sr-replace-witness.py --self-test >"$LOGDIR/grader-self-test.txt" \
  || { cat "$LOGDIR/grader-self-test.txt"; seal "result=fail reason=grader_self_test"; exit 1; }
python3 -I scripts/grade-riscv64-sr-replace-witness.py "$NORM"
