#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# VM-INVALIDATION1 — the RISC-V COW / fork invalidation witness, on QEMU virt / rv64 / 512M /
# -smp 2 with the PINNED OpenSBI (`scripts/firmware/riscv64-opensbi.pin`, booted from this run's own
# copy) and `yarm.ap_user_dispatch=1`.
#
# Usage: scripts/qemu-riscv64-vmi-witness-smoke.sh
#   LOGDIR=...     where the build, the artifact identity and the boot log land
#   SKIP_BUILD=1   reuse the artifacts already in $LOGDIR/build
#   TIMEOUT_SECS   boot budget (default 300)
#
# Two kernel-built tasks share one address space: the requester R (boot hart, CPU 0) and the
# observer O (secondary hart, CPU 1).
#
#   COW   O caches the read-only translation of a copy-on-write page C and spins on it with no
#         kernel entry; R stores into C, so the production split COW recovery on CPU 0 replaces a
#         page whose translation is live on CPU 1.
#   fork  O stores continuously into F through a cached WRITABLE translation with no kernel entry;
#         R forks, so the fork's write-protection must retire that translation before the child
#         is published. The child reads F twice across a delay.
#
# Graded SEPARATELY:
#   * OBSERVED BYTES — R reads its own byte back (R_COW_OK); O, with no supervisor entry since its
#     priming, reads R's byte (O_COW_NEW, credited); the child sees F frozen (two equal non-zero
#     reads), O having taken no supervisor entry between its priming and the fork's shootdown, and
#     O's next store faulting afterwards.
#   * KERNEL COMPLETION — for C and for F, the production shootdown's requester is CPU 0, its own
#     target set INCLUDES CPU 1 and the firmware fence succeeded; the production owners' markers say
#     the COW committed with its invalidation acknowledged (`VM_COW_SPLIT_COMMITTED ... acked=1`)
#     and the fork's write-protection completed (`FORK_COW_SHOOTDOWN_SPLIT ... result=ok`) before
#     the fork committed.
#   * NOTHING ELSE: no incomplete invalidation, no refused fork, no fail-closed COW, no panic.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-riscv64-vmi-witness}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
QEMU_BIN=${QEMU_BIN:-qemu-system-riscv64}
FEATURE=riscv64-vmi-witness
PIN=scripts/firmware/riscv64-opensbi.pin
pin() { sed -n "s/^$1=//p" "$PIN"; }
FIRMWARE=$(pin path)
BOOT_FW="$BUILD_DIR/opensbi-fw_dynamic.bin"
CMDLINE="console=ttyS0 rdinit=/init yarm.ap_user_dispatch=1"

seal() { echo "VMIW_WITNESS_SEAL arch=riscv64 $*"; }

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "[vmi-witness] building riscv64 artifacts, then the kernel with $FEATURE, into $BUILD_DIR"
  rm -rf "$BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-riscv64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || { seal "result=fail reason=base_build"; exit 1; }
  # The kernel with the witness feature, published as ELF + raw image with its no-FP check and
  # provenance (BUILD-GATES1).
  source scripts/lib/artifact-binding.sh
  artifact_build_kernel riscv64 "--no-default-features --features "$FEATURE"" \
    "$BUILD_DIR/yarm-riscv64-vmiw.elf" "$BUILD_DIR/yarm-riscv64-vmiw.bin" \
    >"$LOGDIR/kbuild.log" 2>&1 || { seal "result=fail reason=kernel_build"; exit 1; }
  [[ -f "$FIRMWARE" ]] || { seal "result=fail reason=firmware_missing"; exit 1; }
  [[ "$(sha256sum "$FIRMWARE" | cut -d' ' -f1)" == "$(pin sha256)" ]] \
    || { seal "result=fail reason=firmware_substituted"; exit 1; }
  cp "$FIRMWARE" "$BOOT_FW"
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l) kernel_features=--no-default-features --features $FEATURE userspace_features=--no-default-features"
    echo "qemu=$($QEMU_BIN --version | head -n1) machine=virt cpu=rv64 smp=2 cmdline=$CMDLINE"
    echo "firmware_path=$FIRMWARE firmware_banner=$(pin banner)"
    sha256sum "$BUILD_DIR/yarm-riscv64-vmiw.bin" "$BUILD_DIR/yarm-riscv64-vmiw.elf" "$BUILD_DIR/initramfs-core.cpio" "$BOOT_FW"
  } >"$LOGDIR/artifact-identity.txt"
fi

# BUILD-GATES1: boot only the recorded, no-FP-checked artifacts this run expects (refuses before
# QEMU on a missing record, a replaced or cached image from other source, failed check or features).
source scripts/lib/artifact-binding.sh
artifact_boot_guard riscv64 "$BUILD_DIR/yarm-riscv64-vmiw.bin" "$BUILD_DIR/initramfs-core.cpio" "kernel=$FEATURE" "initramfs=none" >"$LOGDIR/artifact-binding.txt" 2>&1
_ab_rc=$?
cat "$LOGDIR/artifact-binding.txt"
(( _ab_rc == 0 )) || { seal "result=fail reason=artifact_binding"; exit 1; }
echo "[vmi-witness] booting -smp 2 yarm.ap_user_dispatch=1"
python3 - "$QEMU_BIN" "$BUILD_DIR" "$BOOT_LOG" "${TIMEOUT_SECS:-300}" "$CMDLINE" <<'PY'
import subprocess, sys, time, os, select
qemu, build, log, budget, cmdline = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4]), sys.argv[5]
args = [qemu, "-machine", "virt", "-cpu", "rv64", "-m", "512M", "-smp", "2",
        "-nographic", "-monitor", "none", "-serial", "stdio",
        "-bios", os.path.join(build, "opensbi-fw_dynamic.bin"), "-no-reboot",
        "-kernel", os.path.join(build, "yarm-riscv64-vmiw.bin"),
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
            if seen is None and b"VMIW_VERDICT" in tail:
                seen = time.time()
        now = time.time()
        if (seen is not None and now - seen > 3) or now - start > budget:
            break
p.kill(); p.wait()
print("[vmi-witness] boot %s after %.1fs" % ("sealed" if seen else "TIMED OUT", time.time() - start))
PY

[[ -s "$BOOT_LOG" ]] || { seal "result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
python3 -I - "$NORM" <<'PY'
import re, sys
lines = open(sys.argv[1], errors="replace").read().split("\n")
fails = []
def find(pat):
    rx = re.compile(pat)
    return [(i, l) for i, l in enumerate(lines) if rx.search(l)]
def one(pat, why):
    hits = find(pat)
    if len(hits) != 1:
        fails.append(f"{why} (found {len(hits)})")
        return None
    return hits[0]
prov = one(r"VMIW_WITNESS_PROVISIONED ", "provisioned once")
asid = None
if prov:
    m = re.search(r"asid=(\d+)", prov[1]); asid = m and m.group(1)
placed = one(r"VMIW_WITNESS_SECONDARY_PLACED cpu=1 tid=9501 kick=1", "observer placed on CPU 1 with a kick")
# COW
o_cow_ready = one(r"VMIW_USER cpu=1 tid=9501 step=O_COW_READY ", "O primed C on CPU 1")
c_shoot = one(r"VMIW_KERNEL_SHOOT page=C requester_cpu=0 .* targets=0x2 ", "C: requester 0, remote target CPU 1")
cow_commit = one(r"VM_COW_SPLIT_COMMITTED cpu=0 tid=9500 asid=%s va=0x20080000 .* acked=1" % (asid or r"\d+"),
                 "the production COW committed on CPU 0 with its invalidation acknowledged")
r_cow = one(r"VMIW_USER cpu=0 tid=9500 step=R_COW_OK ", "R read its own byte back")
o_cow = one(r"VMIW_USER cpu=1 tid=9501 step=O_COW_NEW round=0x0 aux=0x5a ", "O read R's byte on CPU 1")
# fork
o_fork_ready = one(r"VMIW_USER cpu=1 tid=9501 step=O_FORK_READY ", "O primed F on CPU 1")
f_shoot = one(r"VMIW_KERNEL_SHOOT page=F requester_cpu=0 .* targets=0x2 ", "F: requester 0, remote target CPU 1")
first = one(r"VMIW_KERNEL_FORK_FIRST_SHOOT requester_cpu=0 .* targets=0x2 ", "the fork's first shootdown: requester 0, remote target CPU 1")
wp = one(r"FORK_COW_SHOOTDOWN_SPLIT asid=%s .* result=ok" % (asid or r"\d+"), "the fork's write-protection invalidation completed")
committed = one(r"FORK_TXN_COMMITTED parent_tid=9500 ", "the fork committed once")
forked = one(r"VMIW_USER cpu=0 tid=9500 step=R_FORKED ", "the parent saw its child")
child = one(r"VMIW_USER cpu=0 tid=\d+ step=C_F ", "the child reported F")
one(r"VMIW_DETAIL_COW cow_bytes=1 cow_credited=1 cow_kernel=1 cow_requester=0 cow_targets=0x2 cow_fence=1$",
    "COW detail: observed bytes credited, requester 0, target CPU 1, fence ok")
one(r"VMIW_DETAIL_FORK fork_bytes=1 child_f=(0x[0-9a-f]+)/\1 fork_credited=1 o_faulted=1 fork_kernel=1 fork_requester=0 fork_targets=0x2 fork_fence=1$",
    "fork detail: child saw F frozen, credited, requester 0, target CPU 1, fence ok")
verdict = one(r"VMIW_VERDICT cow=1 fork=1 o_faulted=1 result=ok$", "verdict ok")
def before(a, b, why):
    if a and b and a[0] > b[0]:
        fails.append(why)
before(o_cow_ready, c_shoot, "O was not resident before the COW")
before(c_shoot, cow_commit, "the COW committed before its shootdown")
before(cow_commit, o_cow, "O's observation reported before the COW committed")
before(o_fork_ready, f_shoot, "O was not resident before the fork's write-protection")
before(o_fork_ready, first, "O was not resident before the fork's first shootdown")
before(first, f_shoot, "F's shootdown preceded the fork's first one")
before(f_shoot, wp, "the write-protection completed before its shootdown")
before(wp, committed, "the fork committed before its write-protection completed")
before(committed, child, "the child ran before the fork committed")
for bad in ["VM_COW_INVALIDATION_INCOMPLETE", "VM_COW_SPLIT_FAILED_CLOSED", "FORK_TXN_SHOOTDOWN_INCOMPLETE",
            "X86_TLB_SHOOTDOWN_FAIL", "VM_DISPLACED_HOLD_RETAINED", "KERNEL PANIC", "panicked at",
            "VMIW_WITNESS_PLACEMENT_FAIL", "step=O_COW_STALE", "step=O_PRIME_FAIL", "step=R_COW_FAIL",
            "step=R_NO_OBSERVER", "step=R_FORK_FAIL"]:
    if any(bad in l for l in lines):
        fails.append(f"forbidden: {bad}")
for f in fails:
    print(f"[vmi-witness][fail] {f}")
if fails:
    print("VMIW_WITNESS_SEAL arch=riscv64 result=fail")
    sys.exit(1)
print("VMIW_WITNESS_SEAL arch=riscv64 cow=committed_acked cow_observer=credited fork=write_protect_complete child_f=frozen requester_cpu=0 remote_target=cpu1 fence=ok result=ok")
PY
