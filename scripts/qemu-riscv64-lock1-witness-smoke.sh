#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-LOCK1 — real subdomain-lock contention and interrupt progress on the production VM
# address-space lock (`vm_state_lock`), driven by the SMP3 two-hart VM/IPI workload, on QEMU virt /
# rv64 / 512M / -smp 2 with the PINNED OpenSBI and `yarm.ap_user_dispatch=1`.
#
# One boot of a kernel built with `riscv64-lock1-witness` (which co-enables `riscv64-smp3-witness`).
# The grader re-derives every credited contended round from the raw sealed `LOCK1_REC` lines and
# additionally requires the SMP3 seal to report `result=ok` (progress + correct results + context).
#
# Usage: scripts/qemu-riscv64-lock1-witness-smoke.sh
#   LOGDIR=...     where the build, artifact identity and boot log land (default per run)
#   SKIP_BUILD=1   reuse the artifacts already in $LOGDIR/build
#   REGRADE=1      grade the existing $LOGDIR/boot.log again without booting (implies SKIP_BUILD)
#   TIMEOUT_SECS   boot budget (default 300)
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-riscv64-lock1-witness}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
QEMU_BIN=${QEMU_BIN:-qemu-system-riscv64}
KELF=target/riscv64gc-unknown-none-elf/release/kernel_boot
PIN=scripts/firmware/riscv64-opensbi.pin
pin() { sed -n "s/^$1=//p" "$PIN"; }
FIRMWARE=${FIRMWARE:-$(pin path)}
BOOT_FW="$BUILD_DIR/opensbi-fw_dynamic.bin"

if [[ "${SKIP_BUILD:-0}" != "1" && "${REGRADE:-0}" != "1" ]]; then
  echo "[lock1-witness] building riscv64 artifacts, then the kernel with riscv64-lock1-witness, into $BUILD_DIR"
  rm -rf "$BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-riscv64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || { echo "LOCK1_WITNESS_SEAL result=fail reason=base_build"; exit 1; }
  cargo build -Z build-std=core,alloc,compiler_builtins,panic_abort \
    --target riscv64gc-unknown-none-elf --profile release \
    --no-default-features --features riscv64-lock1-witness -p yarm --bin kernel_boot \
    >"$LOGDIR/kbuild.log" 2>&1 || { echo "LOCK1_WITNESS_SEAL result=fail reason=kernel_build"; exit 1; }
  [[ -f "$FIRMWARE" ]] || { echo "LOCK1_WITNESS_SEAL result=fail reason=firmware_missing path=$FIRMWARE"; exit 1; }
  FW_SHA=$(sha256sum "$FIRMWARE" | cut -d' ' -f1)
  [[ "$FW_SHA" == "$(pin sha256)" ]] \
    || { echo "LOCK1_WITNESS_SEAL result=fail reason=firmware_substituted sha256=$FW_SHA pinned=$(pin sha256)"; exit 1; }
  cp "$FIRMWARE" "$BOOT_FW"
  cp "$KELF" "$BUILD_DIR/yarm-riscv64-lock1.elf"
  llvm-objcopy -O binary "$KELF" "$BUILD_DIR/yarm-riscv64-lock1.bin" \
    || { echo "LOCK1_WITNESS_SEAL result=fail reason=objcopy"; exit 1; }
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l) features=riscv64-lock1-witness"
    echo "qemu=$($QEMU_BIN --version | head -n1) machine=virt cpu=rv64 smp=2 cmdline=console=ttyS0 rdinit=/init yarm.ap_user_dispatch=1"
    echo "firmware_path=$FIRMWARE"
    echo "firmware_provenance=$(pin provenance) installed=$(dpkg-query -W -f='${Package} ${Version}' qemu-system-data 2>/dev/null)"
    echo "firmware_sha256=$(sha256sum "$BOOT_FW" | cut -d' ' -f1)"
    echo "firmware_pinned_banner=$(pin banner) runtime_sbi=$(pin runtime_sbi) impl_id=$(pin impl_id) impl_version=$(pin impl_version) spec=$(pin spec)"
    sha256sum "$BUILD_DIR/yarm-riscv64-lock1.bin" "$BUILD_DIR/yarm-riscv64-lock1.elf" "$BUILD_DIR/initramfs-core.cpio" "$BOOT_FW"
  } >"$LOGDIR/artifact-identity.txt"
fi

if [[ "${REGRADE:-0}" != "1" ]]; then
  echo "[lock1-witness] booting -smp 2 yarm.ap_user_dispatch=1"
  python3 - "$QEMU_BIN" "$BUILD_DIR" "$BOOT_LOG" "${TIMEOUT_SECS:-300}" <<'PY'
import subprocess, sys, time, os, select
qemu, build, log, budget = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])
args = [qemu, "-machine", "virt", "-cpu", "rv64", "-m", "512M", "-smp", "2",
        "-nographic", "-monitor", "none", "-serial", "stdio", "-bios", os.path.join(build, "opensbi-fw_dynamic.bin"),
        "-no-reboot",
        "-kernel", os.path.join(build, "yarm-riscv64-lock1.bin"),
        "-initrd", os.path.join(build, "initramfs-core.cpio"),
        "-append", "console=ttyS0 rdinit=/init yarm.ap_user_dispatch=1"]
p = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
start, seen, buf, done = time.time(), None, b"", 0
with open(log, "wb") as out:
    while True:
        r, _, _ = select.select([p.stdout], [], [], 0.5)
        if r:
            chunk = os.read(p.stdout.fileno(), 65536)
            if not chunk:
                break
            out.write(chunk); out.flush()
            buf += chunk
            done += buf.count(b"LOCK1_DUMP_DONE")
            buf = buf[-(len(b"LOCK1_DUMP_DONE") - 1):] if not buf.endswith(b"LOCK1_DUMP_DONE") else b""
            if seen is None and done >= 2:
                seen = time.time()
        now = time.time()
        if (seen is not None and now - seen > 3) or now - start > budget:
            break
p.kill(); p.wait()
print("[lock1-witness] boot %s after %.1fs" % ("sealed" if seen else "TIMED OUT", time.time() - start))
PY
fi

python3 scripts/grade-riscv64-lock1-witness.py "$BOOT_LOG" "$LOGDIR/artifact-identity.txt"
