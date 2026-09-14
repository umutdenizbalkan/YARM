#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# U9-SEND-FINAL §3 — the NR 1 SOURCE-FAULT witness, per architecture.
#
# Usage: scripts/qemu-ipc-send-final-fault-witness-smoke.sh [x86_64|aarch64|riscv64]
#
# What it proves, in ONE clean boot, over DISPOSABLE authority (the endpoint
# `provision_init_shared_region_oracle` hands init) and from a DISPOSABLE child task:
#
#   * a real NR 1 whose SOURCE BUFFER IS UNREADABLE is answered by the canonical user-fault
#     path — `record_user_fault(.., FaultAccess::Read)` and a `PageFault` frame, with the
#     SYSCALL itself succeeding — and not by `InvalidArgs` and not by a fall-through;
#   * the kernel's own fault line names the right ADDRESS, the right ACCESS direction and the
#     right CALLER, correlated against the child's tid and the address it actually passed;
#   * the caller RESUMES: the very next send from the same task, with a buffer it owns,
#     succeeds — a task that was not settled correctly could not issue it;
#   * the failed send DELIVERED NOTHING and WOKE NOBODY: the endpoint holds exactly the good
#     messages, in order, and no envelope, queue entry or wake survives the fault;
#   * and NO `IpcSend` trap reached the terminal broad acquisition on this boot.
#
# Fails on: a missing or duplicated witness line, result=fail, a fault answered with the wrong
# error, a fault-address/caller mismatch, any message the failed sends produced, any NR 1 broad
# entry, or any fatal trap/panic/timeout.
set -uo pipefail
cd "$(dirname "$0")/.."

ARCH=${1:-x86_64}
case "$ARCH" in
  x86_64)
    FEATURE=x86-shared-region-direct-oracle
    KTARGET=targets/x86_64-yarm-none.json
    KPROFILE=x86-none
    KELF=target/x86_64-yarm-none/${KPROFILE}/kernel_boot
    BUILD_SCRIPT=scripts/build-qemu-x86_64-artifacts.sh
    SMOKE=scripts/qemu-x86_64-core-smoke.sh
    INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio
    DEST=build-x86_64/kernel_boot.elf
    NEEDS_OBJCOPY=0
    ;;
  aarch64)
    FEATURE=aarch64-shared-region-direct-oracle
    KTARGET=targets/aarch64-yarm-none.json
    KPROFILE=aarch64-none
    KELF=target/aarch64-yarm-none/${KPROFILE}/kernel_boot
    BUILD_SCRIPT=scripts/build-qemu-aarch64-artifacts.sh
    SMOKE=scripts/qemu-aarch64-core-smoke.sh
    INITRAMFS_IMAGE=build-aarch64/initramfs-core.cpio
    DEST=build-aarch64/yarm-aarch64.bin
    NEEDS_OBJCOPY=1
    ;;
  riscv64)
    FEATURE=riscv-shared-region-direct-oracle
    KTARGET=riscv64gc-unknown-none-elf
    KPROFILE=release
    KELF=target/riscv64gc-unknown-none-elf/${KPROFILE}/kernel_boot
    BUILD_SCRIPT=scripts/build-qemu-riscv64-artifacts.sh
    SMOKE=scripts/qemu-riscv64-core-smoke.sh
    INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio
    DEST=build-riscv64/yarm-riscv64.bin
    NEEDS_OBJCOPY=1
    ;;
  *) echo "[ipc-send-fault-witness][fail] unknown arch: $ARCH"; exit 1 ;;
esac

BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ipc-send-final-fault-$ARCH}
TIMEOUT_SECS=${TIMEOUT_SECS:-180}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[ipc-send-fault-witness] $*"; }
die()  { echo "[ipc-send-fault-witness][fail] $*"; fail=1; }

note "building base $ARCH artifacts"
BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
  "$BUILD_SCRIPT" >"$LOGDIR/build.log" 2>&1 || die "base artifact build failed"

note "rebuilding kernel_boot with $FEATURE"
JSON_SPEC_ARG=()
[[ "$KTARGET" == *.json ]] && JSON_SPEC_ARG=(-Z json-target-spec)
cargo build --no-default-features --features "$FEATURE" \
  --target "$KTARGET" --profile "$KPROFILE" \
  -Z build-std="$BUILD_STD" "${JSON_SPEC_ARG[@]}" \
  -p yarm --bin kernel_boot \
  >"$LOGDIR/kbuild.log" 2>&1 || die "feature kernel build failed"

if [[ ! -f "$KELF" ]]; then
  die "feature kernel ELF missing"
elif (( NEEDS_OBJCOPY )); then
  if command -v llvm-objcopy >/dev/null 2>&1; then OBJCOPY=llvm-objcopy
  elif command -v rust-objcopy >/dev/null 2>&1; then OBJCOPY=rust-objcopy
  else OBJCOPY=""; die "no objcopy available to produce the raw kernel image"; fi
  [[ -n "$OBJCOPY" ]] && { "$OBJCOPY" -O binary "$KELF" "$DEST" >"$LOGDIR/objcopy.log" 2>&1 \
    || die "objcopy of the feature kernel failed"; }
else
  cp "$KELF" "$DEST" || die "feature kernel copy failed"
fi
KERNEL_IMAGE="$DEST"

if (( fail )); then
  echo "IPC_SEND_FAULT_SEAL arch=$ARCH result=fail reason=build"
  exit 1
fi

note "booting QEMU -smp 1 with yarm.ipc_send_final_fault_witness=1"
env \
  KERNEL_IMAGE="$KERNEL_IMAGE" \
  INITRAMFS_IMAGE="$INITRAMFS_IMAGE" \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.ipc_send_final_fault_witness=1" \
  QEMU_SMP=1 \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  "$SMOKE" >"$LOGDIR/core-smoke.log" 2>&1 || true

[[ -s "$BOOT_LOG" ]] || { echo "IPC_SEND_FAULT_SEAL arch=$ARCH result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"

count() { grep -a -F -- "$1" "$NORM" 2>/dev/null | wc -l | tr -d ' '; }
have()  { grep -a -q -F -- "$1" "$NORM"; }

# ── The witness itself ──
[[ "$(count 'IPC_SEND_FAULT_WITNESS_BEGIN')" == "1" ]] || die "witness did not start exactly once"
line=$(grep -a -m1 -- 'IPC_SEND_FAULT_WITNESS faults=' "$NORM" || true)
[[ -n "$line" ]] || die "the witness produced no verdict line"
# Every property is named, so a partial success cannot read as a pass.
for prop in "wrong_err=0" "succeeded=0" "good_failed=0" "bad_payload=0" "out_of_order=0" \
            "intact=1" "result=1"; do
  case "$line" in
    *" $prop"*) ;;
    *) die "witness missing $prop  [$line]" ;;
  esac
done
[[ "$(count 'IPC_SEND_FAULT_WITNESS_DONE')" == "1" ]] || die "witness completion missing"
grep -a -q 'IPC_SEND_FAULT_WITNESS_DONE .* result=ok' "$NORM" || die "witness result is not ok"

rounds=$(printf '%s' "$line" | grep -oE 'faults=[0-9]+' | head -1 | sed 's/.*=//')
(( rounds >= 1 )) || die "the witness issued no faulting sends"
# THE CALLER'S ANSWER: every faulting send was answered PageFault, none InvalidArgs, none Ok.
pagefault=$(printf '%s' "$line" | grep -oE 'pagefault=[0-9]+' | head -1 | sed 's/.*=//')
[[ "$pagefault" == "$rounds" ]] \
  || die "only $pagefault of $rounds faulting sends answered PageFault"
# THE CONTINUATION: the same task issued a good send after every fault, and all of them landed.
good_ok=$(printf '%s' "$line" | grep -oE 'good_ok=[0-9]+' | head -1 | sed 's/.*=//')
[[ "$good_ok" == "$rounds" ]] \
  || die "only $good_ok of $rounds recoveries succeeded — the caller did not resume"
# NO DELIVERY FROM THE FAILED SENDS: the endpoint holds exactly the good messages.
drained=$(printf '%s' "$line" | grep -oE 'drained=[0-9]+' | head -1 | sed 's/.*=//')
[[ "$drained" == "$good_ok" ]] \
  || die "drained=$drained but only $good_ok good sends were issued — a failed send delivered"

child=$(grep -a -m1 -- 'IPC_SEND_FAULT_WITNESS_CHILD ' "$NORM" || true)
[[ -n "$child" ]] || die "the faulting child did not report"

# ── THE FAULT, from the kernel's own line ──
#
# The child's view is that it got `PageFault` back. That alone does not say the kernel RECORDED
# a fault, at the right address, for the right access, on behalf of the right task — a handler
# that simply encoded the error would look identical from userspace. This is that evidence.
child_tid=$(grep -a -m1 -oE 'IPC_SEND_FAULT_WITNESS_BEGIN .* child_tid=[0-9]+' "$NORM" \
            | sed 's/.*child_tid=//')
[[ -n "$child_tid" ]] || die "the witness did not report its child tid"
unreadable=$(grep -a -m1 -oE 'IPC_SEND_FAULT_WITNESS_BEGIN .* unreadable=0x[0-9a-f]+' "$NORM" \
             | sed 's/.*unreadable=//')
[[ -n "$unreadable" ]] || die "the witness did not report the address it faults on"

faultlines=$(grep -a -c -- "IPC_SEND_SPLIT_SOURCE_FAULT " "$NORM")
(( faultlines >= 1 )) || die "the kernel recorded no NR 1 source fault at all"
# IDENTITY: every recorded source fault names the CHILD, not init and not a stranger.
mine=$(grep -a -c -- "IPC_SEND_SPLIT_SOURCE_FAULT cpu=0 tid=$child_tid " "$NORM")
[[ "$mine" == "$faultlines" ]] \
  || die "$faultlines source faults recorded but only $mine belong to the child (tid=$child_tid)"
[[ "$mine" == "$rounds" ]] \
  || die "the child issued $rounds faulting sends but the kernel recorded $mine"
# ADDRESS and ACCESS: the address the child named, read direction, and nothing produced.
for prop in "user_ptr=$unreadable" "access=read" "envelopes=0" "enqueues=0" "deliveries=0" "wakes=0"; do
  n=$(grep -a -- "IPC_SEND_SPLIT_SOURCE_FAULT cpu=0 tid=$child_tid " "$NORM" | grep -c -- "$prop")
  [[ "$n" == "$rounds" ]] || die "a recorded source fault is missing $prop ($n of $rounds)"
done
# The substitution §2 forbids must be absent for this caller: a source fault answered
# InvalidArgs would have taken the refusal marker instead of the fault one.
have "IPC_SEND_SPLIT_REFUSED cpu=0 tid=$child_tid reason=payload_fault" \
  && die "a source fault took the old fall-through marker"

# ── NO NR 1 REACHED THE TERMINAL BROAD ACQUISITION ──
#
# `handle_ipc_send` emits this on entry and has exactly one caller, the broad dispatcher's
# `Syscall::IpcSend` arm — so this counts arrivals at the terminal acquisition, not doors the
# split route chose to walk past.
broad=$(count 'IPC_SEND_BROAD_ENTRY nr=1')
[[ "$broad" == "0" ]] || die "$broad IpcSend traps entered the terminal broad acquisition"

# ── The rest of the IPC family stayed closed, and the good sends really were sends ──
(( $(count 'IPC_SEND_SPLIT_DONE') >= 1 )) || die "the NR 1 split route did not run at all"
for d in nr6 nr7; do
  q=$(grep -a -m1 -- "IPC_DIRECT_PRODUCTION_QUIESCENT dir=$d " "$NORM" || true)
  [[ -n "$q" ]] || { die "no quiescent census for $d"; continue; }
  case "$q" in
    *" broad_entries=0"*) ;;
    *) die "$d entered the terminal broad acquisition: [$q]" ;;
  esac
done

# ── The COW workload must NOT be in this profile ──
# U9-IPC-RESIDUAL3 §3 gates the fork proof on slot 5 being EMPTY, and this profile sets it to 15.
have 'IPC_RECV_PROOF_SENDER_WAKE_FORK_SYSCALL_BEGIN' \
  && die "the unrelated COW fork workload ran in the NR 1 witness profile"
have 'VM_COW_SPLIT_FAILED_CLOSED' && die "a copy-on-write remap failed closed"
# And no OTHER slot-5 cell may have run: the selector is mutually exclusive.
have 'IPC_RESIDUAL2_PARK_WITNESS_BEGIN' && die "the park witness ran in the NR 1 witness profile"
have 'IPC_RESIDUAL1_QUEUED_CAP_WITNESS_BEGIN' && die "the queued-cap witness ran in this profile"
have 'XFER2_GRANT_WITNESS_BEGIN' && die "the grant witness ran in this profile"

# ── Nothing may have gone wrong on the way ──
for bad in \
  'YARM_SPLIT_DISPATCH_FALLBACK' \
  'KERNEL PANIC' 'panicked at' 'UNHANDLED'; do
  have "$bad" && die "fatal or unexpected condition in boot log: $bad"
done

if (( fail )); then
  echo "IPC_SEND_FAULT_SEAL arch=$ARCH result=fail"
  exit 1
fi
note "an unreadable NR 1 source faulted, the caller resumed, and the failed sends delivered nothing"
echo "IPC_SEND_FAULT_SEAL arch=$ARCH rounds=$rounds pagefault=$pagefault recovered=$good_ok drained=$drained broad_entries=0 result=ok"
