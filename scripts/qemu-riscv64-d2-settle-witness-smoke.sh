#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# KTHREAD1-ACCEPTANCE §3 — the RISC-V D2 empty-current settlement witness (finding F1), on QEMU
# virt / rv64 / 512M / -smp 1 with the PINNED OpenSBI.
#
# The kernel carries the default-off fixture `riscv64-d2-settle-witness`
# (`src/arch/riscv64/d2_settle_witness.rs`): the first time a D2 send or receive drain finds
# nothing runnable, it queues one phantom id no owner accepts, so the production drain meets a
# queue whose every entry is refused. Everything else is the default boot.
#
# Graded from the boot log, in order, for every class the fixture injected (the receive class is
# required; the send class is graded when the boot reached a blocking send):
#   D2SW_INJECTED -> D2_<CLASS>_GENUINE_DISPATCH_SETTLED reason=refused_scheduler_untouched
#   (typed kernel idle, never ReturnToCurrent) -> D2SW_SETTLED with nothing current, the outgoing
#   task still blocked in its class and the phantom withdrawn;
# then no trap-handling failure or panic anywhere, and the timer still ticking afterwards. A run
# that never settles is reported as a timeout, not as a named detection.
#
# Usage: scripts/qemu-riscv64-d2-settle-witness-smoke.sh   (LOGDIR, SKIP_BUILD=1, TIMEOUT_SECS)
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-riscv64-d2-settle-witness}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
QEMU_BIN=${QEMU_BIN:-qemu-system-riscv64}
FEATURE=riscv64-d2-settle-witness
PIN=scripts/firmware/riscv64-opensbi.pin
pin() { sed -n "s/^$1=//p" "$PIN"; }
FIRMWARE=$(pin path)
BOOT_FW="$BUILD_DIR/opensbi-fw_dynamic.bin"
CMDLINE="console=ttyS0 rdinit=/init"

seal() { echo "D2SW_SEAL arch=riscv64 $*"; }

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "[d2-settle-witness] building riscv64 artifacts, then the kernel with $FEATURE, into $BUILD_DIR"
  rm -rf "$BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-riscv64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || { seal "result=fail reason=base_build"; exit 1; }
  source scripts/lib/artifact-binding.sh
  artifact_build_kernel riscv64 "--no-default-features --features $FEATURE" \
    "$BUILD_DIR/yarm-riscv64-d2sw.elf" "$BUILD_DIR/yarm-riscv64-d2sw.bin" \
    >"$LOGDIR/kbuild.log" 2>&1 || { seal "result=fail reason=kernel_build"; exit 1; }
  [[ -f "$FIRMWARE" ]] || { seal "result=fail reason=firmware_missing"; exit 1; }
  [[ "$(sha256sum "$FIRMWARE" | cut -d' ' -f1)" == "$(pin sha256)" ]] \
    || { seal "result=fail reason=firmware_substituted"; exit 1; }
  cp "$FIRMWARE" "$BOOT_FW"
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l) kernel_features=--no-default-features --features $FEATURE userspace_features=--no-default-features"
    echo "qemu=$($QEMU_BIN --version | head -n1) machine=virt cpu=rv64 cmdline=$CMDLINE"
    sha256sum "$BUILD_DIR/yarm-riscv64-d2sw.bin" "$BUILD_DIR/yarm-riscv64-d2sw.elf" "$BUILD_DIR/initramfs-core.cpio" "$BOOT_FW"
  } >"$LOGDIR/artifact-identity.txt"
fi

source scripts/lib/artifact-binding.sh
artifact_boot_guard riscv64 "$BUILD_DIR/yarm-riscv64-d2sw.bin" "$BUILD_DIR/initramfs-core.cpio" "kernel=$FEATURE" "initramfs=none" >"$LOGDIR/artifact-binding.txt" 2>&1
_ab_rc=$?
cat "$LOGDIR/artifact-binding.txt"
(( _ab_rc == 0 )) || { seal "result=fail reason=artifact_binding"; exit 1; }

python3 - "$QEMU_BIN" "$BUILD_DIR" "$BOOT_LOG" "${TIMEOUT_SECS:-120}" "$CMDLINE" <<'PY'
import subprocess, sys, time, os, select
qemu, build, log, budget, cmdline = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4]), sys.argv[5]
args = [qemu, "-machine", "virt", "-cpu", "rv64", "-m", "512M", "-smp", "1",
        "-nographic", "-monitor", "none", "-serial", "stdio",
        "-bios", os.path.join(build, "opensbi-fw_dynamic.bin"), "-no-reboot",
        "-kernel", os.path.join(build, "yarm-riscv64-d2sw.bin"),
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
            window = tail + chunk
            if seen is None and b"D2SW_SETTLED" in window:
                seen = time.time()
            tail = window[-4096:]
        now = time.time()
        # After the settlement, keep running long enough to see the timer and the boot continue.
        if (seen is not None and now - seen > 10) or now - start > budget:
            break
p.kill(); p.wait()
print("[d2-settle-witness] boot %s after %.1fs" % ("settled" if seen else "TIMED OUT", time.time() - start))
PY

[[ -s "$BOOT_LOG" ]] || { seal "result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
python3 -I - "$NORM" <<'PY'
import re, sys
lines = open(sys.argv[1], errors="replace").read().splitlines()
def find(pat, start=0):
    for i in range(start, len(lines)):
        if re.search(pat, lines[i]):
            return i
    return None
fails, cases = [], []
bad = [i for i, l in enumerate(lines) if re.search(r"RISCV_TRAP_HANDLE_FAILED|panicked|PANIC|UnownedEnteringFrame|KTHREAD_FAULT", l)]
inj = {}
for i, l in enumerate(lines):
    m = re.search(r"D2SW_INJECTED class=(\w+) cpu=(\d+) outgoing=(\d+)", l)
    if m:
        inj[m.group(1)] = (i, int(m.group(3)))
if "recv" not in inj:
    fails.append("recv_case_never_injected")
last = 0
for cls, (i, out) in sorted(inj.items()):
    up = cls.upper()
    s = find(r"D2_%s_GENUINE_DISPATCH_SETTLED cpu=\d+ incoming=none reason=refused_scheduler_untouched settlement=kernel_idle" % up, i)
    if s is None:
        # What the drain did instead, if anything: the defect's signature is the tail's return.
        fails.append("%s_not_settled_as_kernel_idle" % cls)
        continue
    w = find(r"D2SW_SETTLED class=%s " % cls, s)
    if w is None:
        fails.append("%s_settlement_unrecorded" % cls)
        continue
    m = re.search(r"outgoing=(\d+) current=(\d+) outgoing_blocked_in_class=(\d) phantom_withdrawn=(\d)", lines[w])
    if not m or int(m.group(1)) != out or m.group(2) != "0" or m.group(3) != "1" or m.group(4) != "1":
        fails.append("%s_settled_state_wrong %s" % (cls, lines[w].strip()[:160]))
    cases.append(cls)
    last = max(last, w)
if bad:
    fails.append("trap_failure_or_panic first=%r" % lines[bad[0]].strip()[:160])
ticks = sum(1 for l in lines[last:] if "RISCV_TIMER_TICK" in l or "TIMER_IDLE_ADVANCE" in l)
if cases and ticks < 5:
    fails.append("timer_stopped_after_settlement ticks=%d" % ticks)
for f in fails:
    print("[fail] " + f)
print("D2SW_SEAL arch=riscv64 cases=%s injected=%s result=%s" % (",".join(cases) or "none", ",".join(sorted(inj)) or "none", "pass" if not fails else "fail"))
sys.exit(0 if not fails else 1)
PY
