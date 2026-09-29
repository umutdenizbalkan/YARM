#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-SMP3 — the RISC-V two-hart IPI / remote-fence / context witness, on QEMU virt / rv64 /
# 512M / -smp 2 with the PINNED OpenSBI (`scripts/firmware/riscv64-opensbi.pin`, booted from this
# run's own copy with an explicit `-bios`) and `yarm.ap_user_dispatch=1`.
#
# Usage: scripts/qemu-riscv64-smp3-witness-smoke.sh
#   LOGDIR=...     where the build, the artifact identity and the boot log land
#                  (default /tmp/qemu-riscv64-smp3-witness); every run should get its own
#   SKIP_BUILD=1   reuse the artifacts already in $LOGDIR/build
#   REGRADE=1      grade the existing $LOGDIR/boot.log again without booting (implies SKIP_BUILD)
#   TIMEOUT_SECS   boot budget (default 300)
#
# One boot of a kernel built with `riscv64-smp3-witness`. The grader NEVER retries, and it does
# not take the kernel's own verdict on trust: it re-derives every graded edge from the raw sealed
# record (`SMP3_REC` lines, printed synchronously after both witness tasks finished) and
# additionally requires the kernel's verifier to agree.
#
# Graded, each separately:
#   * FIRMWARE AND BRING-UP: OpenSBI's banner names the runtime SBI version; the secondary parked
#     with SSIE as a wake source only (SIE 0), was released by the boot hart's knob-gated IPI,
#     admitted in one scheduler transition and published ready; its two tasks placed and one
#     start-up kick taken.
#   * IPI POPULATION: every consumed source is explained by an earlier publication from that CPU to
#     that CPU (recorded after the mailbox publication and BEFORE the firmware call); no
#     publication is left unconsumed; merged publications and empty arrivals are counted, never
#     taken as loss or duplication; no CPU consumed more supervisor software interrupts than the
#     firmware was successfully asked to raise on it; every firmware request succeeded; no
#     completion (EOI) is looked for, because none exists on this port.
#   * PARKED TARGETS (16): C->S and S->C eight times each — the call/reply step, the publication,
#     the consuming arrival, the idle advance resuming exactly the woken task, and that task's own
#     context-checked resume step on the target hart. Every wake of S (CPU 1 has no timer) must be
#     the IPI's at the idle boundary; CPU 0's periodic idle advance may win a race it is entitled
#     to (accepted only when it resumed exactly the woken task and the IPI then arrived in that
#     task), or CPU 0 may be busy; neither counts toward the >= 2 IPI-driven wakes CPU 0 needs.
#   * USER TARGETS (2): the arrival from U-mode, in the resident task, with `sepc` inside its
#     register-checked window, followed by that window's own passing check; FS = VS = Off in every
#     interrupted user frame.
#   * REMOTE FENCE (8 serial rounds): target primed on hart T after its last kernel entry; the
#     requester's production NR 3 (vm_op_begin..vm_op_end, Ok, W) displaces the page, asks the
#     firmware to fence a hart mask that names T's HART (never its CPU index) for exactly that
#     asid/va, gets the same request's success back, has its shootdown acknowledged and only then
#     settles the displaced page; the target observes the new W after that. A round is CREDITED
#     only when hart T took no supervisor entry between its PRE and OBSERVED steps; at least two
#     credited rounds per direction are required.
#   * MUTUAL PROGRESS (4 rounds): both production operations, each complete as above with a fence
#     to the OTHER hart, both entered before either completed, each task's observation after the
#     other's completion. Contended lock acquisitions inside the rounds are reported, not graded.
#   * CONTEXT: every context-checked step is present (a failed check reports a FAIL step instead).
#   * NOTHING FATAL, no user failure step, no broad-entry or unrouted marker.
#   * THE ARTIFACT'S ORDERING (`scripts/check-riscv64-smp3-sequence.sh`): in the built image, the
#     remote fence's `fence rw,rw` precedes its RFENCE `ecall`; the wake's publication and fence
#     precede its IPI `ecall`; the arrival clears `sip.SSIP` before it swaps the mailbox. QEMU
#     cannot distinguish a missing fence.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-riscv64-smp3-witness}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
QEMU_BIN=${QEMU_BIN:-qemu-system-riscv64}
KELF=target/riscv64gc-unknown-none-elf/release/kernel_boot
# QEMU-SMP3-ACCEPTANCE §3: the qualified firmware is PINNED. `FIRMWARE` may name another file (the
# negative controls do); the pin itself is never overridden, so anything else fails.
PIN=scripts/firmware/riscv64-opensbi.pin
pin() { sed -n "s/^$1=//p" "$PIN"; }
FIRMWARE=${FIRMWARE:-$(pin path)}
BOOT_FW="$BUILD_DIR/opensbi-fw_dynamic.bin"

if [[ "${SKIP_BUILD:-0}" != "1" && "${REGRADE:-0}" != "1" ]]; then
  echo "[smp3-witness] building riscv64 artifacts, then the kernel with riscv64-smp3-witness, into $BUILD_DIR"
  rm -rf "$BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-riscv64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || { echo "SMP3_WITNESS_SEAL result=fail reason=base_build"; exit 1; }
  cargo build -Z build-std=core,alloc,compiler_builtins,panic_abort \
    --target riscv64gc-unknown-none-elf --profile release \
    --no-default-features --features riscv64-smp3-witness -p yarm --bin kernel_boot \
    >"$LOGDIR/kbuild.log" 2>&1 || { echo "SMP3_WITNESS_SEAL result=fail reason=kernel_build"; exit 1; }
  [[ -f "$FIRMWARE" ]] || { echo "SMP3_WITNESS_SEAL result=fail reason=firmware_missing path=$FIRMWARE"; exit 1; }
  FW_SHA=$(sha256sum "$FIRMWARE" | cut -d' ' -f1)
  [[ "$FW_SHA" == "$(pin sha256)" ]] \
    || { echo "SMP3_WITNESS_SEAL result=fail reason=firmware_substituted sha256=$FW_SHA pinned=$(pin sha256)"; exit 1; }
  cp "$FIRMWARE" "$BOOT_FW"
  cp "$KELF" "$BUILD_DIR/yarm-riscv64-smp3.elf"
  llvm-objcopy -O binary "$KELF" "$BUILD_DIR/yarm-riscv64-smp3.bin" \
    || { echo "SMP3_WITNESS_SEAL result=fail reason=objcopy"; exit 1; }
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l) features=riscv64-smp3-witness"
    echo "qemu=$($QEMU_BIN --version | head -n1) machine=virt cpu=rv64 smp=2 cmdline=console=ttyS0 rdinit=/init yarm.ap_user_dispatch=1"
    echo "firmware_path=$FIRMWARE"
    echo "firmware_provenance=$(pin provenance) installed=$(dpkg-query -W -f='${Package} ${Version}' qemu-system-data 2>/dev/null)"
    echo "firmware_sha256=$(sha256sum "$BOOT_FW" | cut -d' ' -f1)"
    echo "firmware_pinned_banner=$(pin banner) runtime_sbi=$(pin runtime_sbi) impl_id=$(pin impl_id) impl_version=$(pin impl_version) spec=$(pin spec)"
    sha256sum "$BUILD_DIR/yarm-riscv64-smp3.bin" "$BUILD_DIR/yarm-riscv64-smp3.elf" "$BUILD_DIR/initramfs-core.cpio" "$BOOT_FW"
  } >"$LOGDIR/artifact-identity.txt"
fi

if [[ "${REGRADE:-0}" != "1" ]]; then
  echo "[smp3-witness] booting -smp 2 yarm.ap_user_dispatch=1"
  python3 - "$QEMU_BIN" "$BUILD_DIR" "$BOOT_LOG" "${TIMEOUT_SECS:-300}" <<'PY'
import subprocess, sys, time, os, select
qemu, build, log, budget = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])
args = [qemu, "-machine", "virt", "-cpu", "rv64", "-m", "512M", "-smp", "2",
        "-nographic", "-monitor", "none", "-serial", "stdio", "-bios", os.path.join(build, "opensbi-fw_dynamic.bin"),
        "-no-reboot",
        "-kernel", os.path.join(build, "yarm-riscv64-smp3.bin"),
        "-initrd", os.path.join(build, "initramfs-core.cpio"),
        "-append", "console=ttyS0 rdinit=/init yarm.ap_user_dispatch=1"]
p = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
start, seen, buf, verdicts = time.time(), None, b"", 0
with open(log, "wb") as out:
    while True:
        r, _, _ = select.select([p.stdout], [], [], 0.5)
        if r:
            chunk = os.read(p.stdout.fileno(), 65536)
            if not chunk:
                break
            out.write(chunk); out.flush()
            # Count across chunk boundaries: the tail keeps the last partial marker only.
            buf += chunk
            verdicts += buf.count(b"SMP3_VERDICT")
            buf = buf[-(len(b"SMP3_VERDICT") - 1):] if not buf.endswith(b"SMP3_VERDICT") else b""
            if seen is None and verdicts >= 2:
                seen = time.time()
        now = time.time()
        if (seen is not None and now - seen > 3) or now - start > budget:
            break
p.kill(); p.wait()
print("[smp3-witness] boot %s after %.1fs" % ("sealed" if seen else "TIMED OUT", time.time() - start))
PY
fi

# The ordering of THIS artifact. QEMU cannot distinguish a missing fence.
scripts/check-riscv64-smp3-sequence.sh "$BUILD_DIR/yarm-riscv64-smp3.elf" > "$LOGDIR/sequence.txt" 2>&1
SEQ_STATUS=$?
sed 's/^/[smp3-witness] /' "$LOGDIR/sequence.txt"

python3 scripts/grade-riscv64-smp3-witness.py "$BOOT_LOG" "$SEQ_STATUS" "$PIN" "$LOGDIR/artifact-identity.txt"
