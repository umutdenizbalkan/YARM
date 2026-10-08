#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-LOCK3 — real contention on the production VM address-space lock (`vm_state_lock`, rank 5,
# `SpinLockIrq`) in both directions on x86_64, and the reschedule IPI deferred through the holder's
# masked hold, driven by the SMP1 two-CPU VM/IPI workload on QEMU q35 / qemu64 / 512M / -smp 2 with
# the cross-CPU reply profile and `yarm.x86_64_smp1_witness=1`.
#
# One boot of a kernel built with `x86_64-lock3-witness` (which co-enables `x86-smp1-witness`) over
# the plain base initramfs. Two graders read the same boot log: the SMP1 witness grader (wakes,
# resident and twelve mutual rounds, every shootdown ACKed at its generation, settles after ACK,
# context, both tasks blocked again) and the LOCK3 grader, which re-derives every round from the raw
# sealed `LOCK3_REC` lines.
#
# Usage: scripts/qemu-x86_64-lock3-witness-smoke.sh
#   LOGDIR=...     where the build, artifact identity and boot log land
#   SKIP_BUILD=1   reuse the artifacts already in $LOGDIR/build
#   REGRADE=1      grade the existing $LOGDIR/boot.log again without booting (implies SKIP_BUILD)
#   TIMEOUT_SECS   boot ceiling (default 300)
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-x86_64-lock3-witness}
TIMEOUT_SECS=${TIMEOUT_SECS:-300}
KTARGET=targets/x86_64-yarm-none.json
KPROFILE=x86-none
BUILD_STD=core,alloc,compiler_builtins,panic_abort
FEATURES=x86_64-lock3-witness
CMDLINE="console=ttyS0 rdinit=/init yarm.x86_64_ipccall_direct_smp_oracle=1 yarm.x86_64_ipccall_direct_smp_recv_v2_server=1 yarm.x86_64_ipccall_direct_smp_request=1 yarm.x86_64_ipccall_direct_smp_reply=1 yarm.ap_user_dispatch=1 yarm.x86_64_smp1_witness=1"
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
seal_fail() { echo "LOCK3_WITNESS_SEAL result=fail reason=$1"; exit 1; }

if [[ "${SKIP_BUILD:-0}" != "1" && "${REGRADE:-0}" != "1" ]]; then
  echo "[lock3-witness] building base artifacts and kernel_boot --features $FEATURES into $BUILD_DIR"
  rm -rf "$BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/build.log" 2>&1 || seal_fail build
  cargo build -Z "build-std=$BUILD_STD" -Z json-target-spec --target "$KTARGET" \
    --profile "$KPROFILE" --no-default-features --features "$FEATURES" \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 || seal_fail kernel_build
  cp "target/x86_64-yarm-none/$KPROFILE/kernel_boot" "$BUILD_DIR/kernel_boot.elf" || seal_fail copy
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l) features=$FEATURES"
    echo "qemu=$(qemu-system-x86_64 --version | head -n1) machine=q35 cpu=qemu64 m=512M smp=2 cmdline=$CMDLINE"
    sha256sum "$BUILD_DIR/kernel_boot.elf" "$BUILD_DIR/initramfs-core.cpio"
  } >"$LOGDIR/artifact-identity.txt"
fi
[[ -s "$BUILD_DIR/kernel_boot.elf" && -s "$BUILD_DIR/initramfs-core.cpio" ]] || seal_fail no_artifacts

if [[ "${REGRADE:-0}" != "1" ]]; then
  source scripts/lib/qemu-x86-deterministic.sh
  QEMU_ARGV=(
    qemu-system-x86_64 -machine q35 -cpu qemu64 -m 512M -smp 2
    -nographic -monitor none -serial stdio -no-reboot -no-shutdown
    -kernel "$BUILD_DIR/kernel_boot.elf" -initrd "$BUILD_DIR/initramfs-core.cpio"
    -append "$CMDLINE"
  )
  # The dumping CPU prints both passes before its task blocks again, so both blocks imply a
  # complete dump.
  QEMU_TERMINAL_MARKERS=(
    "LOCK3_DUMP_DONE"
    "X86_SMP_ORACLE_BLOCKED cpu=1 tid=20205 endpoint=6 wait_gen=2"
    "X86_SMP_ORACLE_BLOCKED cpu=0 tid=21205 endpoint=7 wait_gen=2"
  )
  FATAL_RE="KERNEL PANIC|RUST PANIC|panicked at|DOUBLE FAULT|Unhandled|BOOTSTRAP_ERROR|X86_TLB_SHOOTDOWN_FAIL|X86_USER_FPU_HOME_UNAUTHENTICATED|X86_AP_SAVED_RESUME_REFUSED|SMP1_[A-Z0-9_]*(FAIL|STALE_AFTER_ACK|RESIDENCY_LOST|UNEXPECTED|RETURNED|EXHAUSTED)"
  echo "#HOST_QEMU ${QEMU_ARGV[*]} $(qemu-system-x86_64 --version | head -1)" >"$BOOT_LOG.host"
  echo "[lock3-witness] booting -smp 2 (ceiling ${TIMEOUT_SECS}s)"
  start=$(date +%s)
  qemu_run_deterministic "$BOOT_LOG" "$FATAL_RE" "$TIMEOUT_SECS" "${QEMU_ARGV[@]}"
  rc=$?
  echo "[lock3-witness] qemu lifecycle: ${QEMU_LIFECYCLE_RESULT} (rc=$rc) after $(( $(date +%s) - start ))s"
fi
[[ -s "$BOOT_LOG" ]] || seal_fail no_boot_log

# The SMP1 grader on the same boot: twelve mutual rounds, and the six reschedule IPIs the waiter
# publishes in each direction on top of the two wakes.
LOGDIR="$LOGDIR" REGRADE=1 SMP1_MUTUAL_ROUNDS=12 SMP1_EXTRA_WAKES_01=6 SMP1_EXTRA_WAKES_10=6 \
  scripts/qemu-x86_64-smp1-witness-smoke.sh >"$LOGDIR/smp1-grade.txt" 2>&1
tail -n 1 "$LOGDIR/smp1-grade.txt" >"$LOGDIR/smp1-seal.txt"
python3 scripts/grade-x86_64-lock3-witness.py "$BOOT_LOG" "$LOGDIR/smp1-seal.txt" "$LOGDIR/artifact-identity.txt"
