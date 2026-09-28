#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-SMP2-ACCEPTANCE §3 — the POSITIVE witness for the RISC-V core census's NR 0 (`Yield`) term.
#
# Usage: scripts/qemu-riscv64-late-reply-yield-smoke.sh
#   LOGDIR=...        build, artifact identity and boot log (default /tmp/qemu-riscv64-late-reply-yield)
#   TIMEOUT_SECS=...  boot budget (default 90)
#
# A default core boot issues no Yield at all. The users are init's reply-timeout oracle lanes, which
# exist only in a `riscv64-ipc-reply-timeout-oracle` kernel: with
# `yarm.riscv_ipc_reply_timeout_oracle=timeout-wins` the oracle server yields until the client has
# timed out, then issues its late NR 7 (rejected: `..._SERVER_LATE_REPLY rejected=1`), and the client
# yields until it sees that verdict (`init/service.rs`). This runner builds fresh, identified
# artifacts into $LOGDIR (servers + initramfs as the core smoke uses them, and the feature kernel
# exactly as `qemu-ipc-reply-timeout-riscv64-retirement-smoke.sh` builds it), boots the STRICT core
# smoke on them ONCE, and requires, besides the smoke's own verdict, that this boot took the
# late-reply path AND dispatched at least one NR 0 — which the census must then account for.
#
# One boot, never retried. A boot that does not reach the lane reports `result=not_reached`: it is
# a regression smoke, not the witness.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-riscv64-late-reply-yield}
TIMEOUT_SECS=${TIMEOUT_SECS:-90}
FEATURE=riscv64-ipc-reply-timeout-oracle
BUILD="$LOGDIR/build"
KELF=target/riscv64gc-unknown-none-elf/release/kernel_boot
KBIN="$BUILD/yarm-riscv64-oracle.bin"
BOOT_LOG="$LOGDIR/boot.log"
mkdir -p "$LOGDIR"
seal() { echo "RISCV_LATE_REPLY_YIELD_SEAL $*"; }

rm -rf "$BUILD"
OUT_DIR="$BUILD" BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
  scripts/build-qemu-riscv64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
  || { seal "result=fail reason=base_build"; exit 1; }
cargo build -Z build-std=core,alloc,compiler_builtins,panic_abort \
  --target riscv64gc-unknown-none-elf --profile release \
  --no-default-features --features "$FEATURE" -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 \
  || { seal "result=fail reason=kernel_build"; exit 1; }
llvm-objcopy -O binary "$KELF" "$KBIN" || { seal "result=fail reason=objcopy"; exit 1; }
{
  echo "tree=$(git rev-parse HEAD^{tree}) head=$(git rev-parse HEAD) dirty=$(git status --porcelain | wc -l) feature=$FEATURE"
  sha256sum "$KBIN" "$BUILD/initramfs-core.cpio"
} >"$LOGDIR/artifact-identity.txt"

KERNEL_IMAGE="$KBIN" INITRAMFS_IMAGE="$BUILD/initramfs-core.cpio" \
KERNEL_CMDLINE="yarm.riscv_ipc_reply_timeout_oracle=timeout-wins" QEMU_SINGLE_BOOT=1 \
TIMEOUT_SECS="$TIMEOUT_SECS" QEMU_SMOKE_STRICT=1 LOGFILE="$BOOT_LOG" \
  scripts/qemu-riscv64-core-smoke.sh >"$LOGDIR/core-smoke.txt" 2>&1
core=$?
grep -a '^\[fail\]\|^\[info\] RISC-V NR 0\|core-smoke passed\|check(s) failed' "$LOGDIR/core-smoke.txt"

late=$(rg -a -c "IPC_REPLY_TIMEOUT_ORACLE_SERVER_LATE_REPLY(_REJECTED)? rejected=1" "$BOOT_LOG" 2>/dev/null || echo 0)
nr0=$(rg -a -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=0 " "$BOOT_LOG" 2>/dev/null || echo 0)
accounted=$(rg -a -c "RISC-V NR 0 \(Yield\) split dispatches: ${nr0}, each paired" "$LOGDIR/core-smoke.txt" 2>/dev/null || echo 0)
summary="late_reply_rejected=${late:-0} nr0=${nr0:-0} census_accounted=${accounted:-0} core_smoke_exit=$core"
if (( core != 0 )); then
  seal "$summary result=fail"; exit 1
fi
if (( ${late:-0} == 0 || ${nr0:-0} == 0 )); then
  seal "$summary result=not_reached"; exit 2
fi
if (( ${accounted:-0} != 1 )); then
  seal "$summary result=fail reason=census_did_not_account_nr0"; exit 1
fi
seal "$summary result=ok"
