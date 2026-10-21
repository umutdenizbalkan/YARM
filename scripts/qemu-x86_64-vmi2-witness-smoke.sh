#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# VM-INVALIDATION2 — the x86_64 dispatch-gap TLB witness, on QEMU q35 / qemu64 / -smp N.
#
#   LOGDIR=...        build + boot logs (default /tmp/qemu-x86_64-vmi2-witness)
#   SMP=N             CPUs (default 2); the observer pins to CPU 1, the mutator to CPU 0
#   SKIP_BUILD=1      reuse $LOGDIR/build
#   TIMEOUT_SECS=...  boot ceiling (default 420)
#
# The witness builds with `x86-vmi2-witness` and arms with `yarm.x86_64_vmi2_witness=1`. It reaches
# the REAL empty-current/root-still-loaded window (the observer on CPU 1 yields) and records, from
# the production owner `live_cpu_bitmap_for_asid_split`, the shootdown target set for the shared
# address space at that gap. A mutator on CPU 0 replaces a page of it each round.
#
# Positive evidence this regression REQUIRES: at least one gap window occurred (gaps_observed >= 1)
# and the production target set NAMED the root-resident CPU at every observed gap (gaps_named ==
# gaps_observed). Zero gaps cannot qualify. The observer's post-resume bytes are recorded and
# cross-checked separately; this smoke does NOT claim a QEMU pass proves the architectural
# retention of a stale TLB entry — a QEMU TLB model need not keep one — only that the production
# target-set decision is correct at the real window, with the ACK and reclamation ordering from
# the owners.
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
fail=0
note() { echo "[vmi2-witness] $*"; }
die()  { echo "[vmi2-witness][fail] $*"; fail=1; }
seal_fail() { echo "VMI2_WITNESS_SEAL arch=x86_64 smp=$SMP result=fail reason=$1"; exit 1; }

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  note "building base artifacts into $BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/build.log" 2>&1 || seal_fail base_build
  note "building kernel_boot with --features x86-vmi2-witness"
  cargo build -Z "build-std=$BUILD_STD" -Z json-target-spec --target "$KTARGET" \
    --profile "$KPROFILE" --no-default-features --features x86-vmi2-witness \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 || seal_fail kernel_build
  cp "target/x86_64-yarm-none/$KPROFILE/kernel_boot" "$BUILD_DIR/kernel_boot.elf" || seal_fail copy
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l) kernel_features=--no-default-features --features x86-vmi2-witness"
    echo "qemu=$(qemu-system-x86_64 --version | head -n1) machine=q35 cpu=qemu64 m=512M smp=$SMP"
    sha256sum "$BUILD_DIR/kernel_boot.elf" "$BUILD_DIR/initramfs-core.cpio"
  } >"$LOGDIR/artifact-identity.txt"
fi
[[ -s "$BUILD_DIR/kernel_boot.elf" && -s "$BUILD_DIR/initramfs-core.cpio" ]] || seal_fail no_artifacts

APPEND="console=ttyS0 rdinit=/init yarm.x86_64_vmi2_witness=1${APPEND:+ $APPEND}"
note "booting -smp $SMP (ceiling ${TIMEOUT_SECS}s)"
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
rg -a -q "KERNEL PANIC|RUST PANIC|panicked at|DOUBLE FAULT|BOOTSTRAP_ERROR|VMI2_PROVISION_FAIL" "$NORM" \
  && die "a fatal boot marker appeared"

count() { rg -a -c -F -- "$1" "$NORM" 2>/dev/null || echo 0; }
SEAL="$(rg -a -o 'VMI2_WITNESS_SEAL .*' "$NORM" | head -1)"
[[ -n "$SEAL" ]] || seal_fail no_seal
echo "  $SEAL"
field() { sed -n "s/.*[ ]$1=\([0-9]*\).*/\1/p" <<<"$SEAL"; }
GAPS=$(field gaps_observed); NAMED=$(field gaps_named); COH=$(field coherent); POSTR=$(field post_rounds)
PROV=$(count "VMI2_WITNESS_PROVISIONED ")
[[ "$PROV" == 1 ]] || die "the witness did not provision"
# POSITIVE WINDOW EVIDENCE: the intended gap must have occurred at least once.
[[ "${GAPS:-0}" -ge 1 ]] || die "zero dispatch-gap windows occurred — cannot qualify this regression"
# Every observed gap must have NAMED the root-resident CPU in the production target set.
[[ "${NAMED:-0}" == "${GAPS:-0}" ]] || die "a gap window left the root-resident CPU out of the target set ($NAMED/$GAPS)"
# Every gap record names the CPU in its target set (belt and braces against the seal arithmetic).
UNNAMED=$(rg -a -c -- 'VMI2_GAP .* named=0' "$NORM" 2>/dev/null || echo 0)
[[ "${UNNAMED:-0}" == 0 ]] || die "$UNNAMED gap records had named=0"
# The observer's post-resume bytes are coherent with the replace (corroborating, not architectural).
echo "  observer bytes: coherent=$COH of post_rounds=$POSTR (corroborating; QEMU need not retain a stale entry)"
[[ "${COH:-0}" == "${POSTR:-0}" && "${POSTR:-0}" -ge 1 ]] || die "observer read an incoherent byte after a replace ($COH/$POSTR)"

if [[ "$fail" == 0 ]]; then
  echo "VMI2_WITNESS_SMOKE arch=x86_64 smp=$SMP gaps=$GAPS named=$NAMED coherent=$COH result=ok"
else
  seal_fail graded_checks
fi
