#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# U9-IPC-RESIDUAL2 §4 — the FULL-ENDPOINT PARK witness, per architecture.
#
# Usage: scripts/qemu-ipc-residual2-park-witness-smoke.sh [x86_64|aarch64|riscv64]
#
# What it proves, in ONE clean boot, over DISPOSABLE authority (the endpoint
# `provision_init_shared_region_oracle` hands init — nothing the running system depends on):
#
#   * NR 6 onto an endpoint whose queue is FULL PARKS the sender rather than answering it. That
#     arm had no owner before this package: U9-IPC-RESIDUAL1 called the full queue "the BLOCKING
#     origin ... out of scope" and handed the trap to the terminal broad dispatcher.
#   * the park is PUBLISHED through the existing U6 blocking-send transaction
#     (`IPCCALL_PARK_SPLIT_PUBLISHED`, then `U6_BLOCKING_SEND_COMMITTED ... result=ok`);
#   * the RECEIVER MAKES PROGRESS while the sender is parked — the child drains, which is only
#     possible because the queue-advancing dispatch ran after the caller left the run queue;
#   * and the SENDER RESUMES CORRECTLY: every one of its calls returns Ok (`calls_failed=0`),
#     and its endpoint capability still resolves afterwards (`intact=1`).
#
# The child polls with deadline 0 rather than blocking, so it never publishes a receiver waiter —
# a blocked receiver would send the request down the DELIVERY arm and the queue would never fill.
#
# And, from the kernel's own per-trap census, that NR 6 never entered the terminal broad
# acquisition on this boot: `broad_entries=0`, with the production seal green.
#
# Fails on: a missing or duplicated witness line, result=fail, a call that failed, no drain, no
# committed park, any terminal-broad entry, or any fatal trap/panic/timeout.
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
  *) echo "[ipc-residual1-witness][fail] unknown arch: $ARCH"; exit 1 ;;
esac

BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ipc-residual1-queued-cap-$ARCH}
TIMEOUT_SECS=${TIMEOUT_SECS:-180}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[ipc-residual1-witness] $*"; }
die()  { echo "[ipc-residual1-witness][fail] $*"; fail=1; }

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
  echo "IPC_RESIDUAL2_PARK_SEAL arch=$ARCH result=fail reason=build"
  exit 1
fi

note "booting QEMU -smp 1 with yarm.ipc_residual2_park_witness=1"
env \
  KERNEL_IMAGE="$KERNEL_IMAGE" \
  INITRAMFS_IMAGE="$INITRAMFS_IMAGE" \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.ipc_residual2_park_witness=1" \
  QEMU_SMP=1 \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  "$SMOKE" >"$LOGDIR/core-smoke.log" 2>&1 || true

[[ -s "$BOOT_LOG" ]] || { echo "IPC_RESIDUAL2_PARK_SEAL arch=$ARCH result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"

count() { grep -a -F -- "$1" "$NORM" 2>/dev/null | wc -l | tr -d ' '; }
have()  { grep -a -q -F -- "$1" "$NORM"; }

# ── The witness itself ──
[[ "$(count 'IPC_RESIDUAL2_PARK_WITNESS_BEGIN')" == "1" ]] || die "witness did not start exactly once"
line=$(grep -a -m1 -- 'IPC_RESIDUAL2_PARK_WITNESS calls_ok=' "$NORM" || true)
[[ -n "$line" ]] || die "the witness produced no verdict line"
for prop in "calls_failed=0" "intact=1" "stream_ok=1" "result=1"; do
  case "$line" in
    *" $prop"*) ;;
    *) die "witness missing $prop  [$line]" ;;
  esac
done
[[ "$(count 'IPC_RESIDUAL2_PARK_WITNESS_DONE')" == "1" ]] || die "witness completion missing"
grep -a -q 'IPC_RESIDUAL2_PARK_WITNESS_DONE .* result=ok' "$NORM" || die "witness result is not ok"

# The receiver's view of the STREAM, not its size: ordered, un-duplicated, every request
# carrying a materialized one-shot. A park that resumed a sender out of turn, delivered twice,
# or lost its envelope shows up here and in no count.
child=$(grep -a -m1 -- 'IPC_RESIDUAL2_PARK_WITNESS_CHILD ' "$NORM" || true)
[[ -n "$child" ]] || die "the drainer did not report"
for prop in "out_of_order=0" "duplicates=0" "malformed=0"; do
  case "$child" in
    *" $prop"*) ;;
    *) die "drainer reports $prop violated  [$child]" ;;
  esac
done

# ── THE PARK, from the kernel's own identity-correlated chain ──
#
# `drained > 0` is deliberately NOT used as park evidence: timer preemption and Yield can both
# run the child, so a drain proves only that the child ran. What proves a park is the chain
# below, in which each step names the caller and the last two are paired by `send_generation`.
caller=$(grep -a -m1 -oE 'IPCCALL_PARK_SPLIT_PUBLISHED tid=[0-9]+' "$NORM" | head -1 | sed 's/.*tid=//')
[[ -n "$caller" ]] || die "no NR 6 park proposal was published"

# (1) the PROPOSAL stashed — distinct from a commit, and counted separately on purpose.
proposed=$(grep -a -c -- "IPCCALL_PARK_SPLIT_PUBLISHED tid=$caller " "$NORM")
(( proposed >= 1 )) || die "no park proposal for the caller"
# (2) the park COMMITTED by the rank-ordered transaction. A stashed proposal that never
#     committed would leave this at zero, which is exactly the distinction being drawn.
committed=$(grep -a -c -- "U6_BLOCKING_SEND_COMMITTED tid=$caller " "$NORM")
(( committed >= 1 )) || die "a proposal was stashed but no park was COMMITTED"
[[ "$proposed" == "$committed" ]] \
  || die "proposed=$proposed but committed=$committed — a proposal did not become a park"
# (3) the caller DESCHEDULED.
blocked=$(grep -a -c -- "U6_BLOCKING_SEND_WRAPPER_BLOCKED arch=$ARCH cpu=0 tid=$caller " "$NORM")
(( blocked >= committed )) || die "a committed park did not deschedule the caller"
# (4) the EXACT sender completion, paired to its commit by send_generation.
completed=$(grep -a -c -- "U6_SEND_COMPLETION_PUBLISHED tid=$caller " "$NORM")
[[ "$completed" == "$committed" ]] \
  || die "committed=$committed parks but completed=$completed — a parked sender was not resumed"
for g in $(grep -a -oE "U6_BLOCKING_SEND_COMMITTED tid=$caller send_generation=[0-9]+" "$NORM" \
           | sed 's/.*send_generation=//'); do
  grep -a -q -- "U6_SEND_COMPLETION_PUBLISHED tid=$caller asid=[0-9]* send_generation=$g result=0" "$NORM" \
    || grep -a -qE "U6_SEND_COMPLETION_PUBLISHED tid=$caller asid=[0-9]+ send_generation=$g result=0" "$NORM" \
    || die "park send_generation=$g was committed but never completed with result=0"
done
# (5) every park carries its OWN one-shot: distinct reply capability and distinct record, so no
#     two parked requests share reply authority and none was delivered twice. This is where
#     reply-capability validity is checked — the child's deadline-0 probe surfaces the payload
#     only, and the kernel's own marker is the authoritative place for the authority.
caps=$(grep -a -oE "IPCCALL_PARK_SPLIT_PUBLISHED tid=$caller .* reply_cap=[0-9]+" "$NORM" | sed 's/.*reply_cap=//')
ncaps=$(printf '%s\n' "$caps" | grep -c .)
ucaps=$(printf '%s\n' "$caps" | sort -u | grep -c .)
[[ "$ncaps" == "$ucaps" ]] || die "two parks shared a reply capability ($ncaps parks, $ucaps distinct caps)"
[[ "$ncaps" == "$proposed" ]] || die "a park published no reply capability"
printf '%s\n' "$caps" | grep -qx 0 && die "a park published reply_cap=0"
recs=$(grep -a -oE "IPCCALL_PARK_SPLIT_PUBLISHED tid=$caller .* record_index=[0-9]+" "$NORM" | sed 's/.*record_index=//')
urecs=$(printf '%s\n' "$recs" | sort -u | grep -c .)
[[ "$urecs" == "$proposed" ]] || die "two parks shared a reply record ($proposed parks, $urecs distinct records)"

# (6) and no park was refused, so no compensation was owed.
have 'U6_SEND_COMPLETION_REFUSED_STALE' && die "a sender completion was refused as stale"
have 'IPCCALL_PARK_SPLIT_REFUSED' && die "a park proposal was refused"
have 'U6_BLOCKING_SEND_REPLY_AUTHORITY_RECLAIMED' && die "a park commit was refused"
have 'U6_BLOCKING_SEND_ENVELOPE_RECLAIMED' && die "a park commit reclaimed an envelope"
# The buffered lane fills the queue on the way there, so it must have run too.
(( $(count 'IPCCALL_QUEUED_SPLIT_OK') >= 1 )) || die "the NR 6 buffered lane did not run"
# The BROAD producer's publication marker must be absent: this park came from the split route.
have 'U6_BLOCKING_SEND_PUBLISHED' && die "the broad producer published this park, not the split route"

# ── NR 6 never entered the terminal broad acquisition ──
for d in nr6 nr7; do
  q=$(grep -a -m1 -- "IPC_DIRECT_PRODUCTION_QUIESCENT dir=$d " "$NORM" || true)
  [[ -n "$q" ]] || { die "no quiescent census for $d"; continue; }
  case "$q" in
    *" broad_entries=0"*) ;;
    *) die "$d entered the terminal broad acquisition: [$q]" ;;
  esac
done
[[ "$(count 'IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL nr6_ok=1 nr7_ok=1 census_ok=1 result=ok')" == "1" ]] \
  || die "the direct-IPC production seal is not green"

# ── The COW workload must NOT be in this profile ──
# U9-IPC-RESIDUAL3 §3: the fork proof is gated on slot 5 being empty, and this profile sets it.
have 'IPC_RECV_PROOF_SENDER_WAKE_FORK_SYSCALL_BEGIN' \
  && die "the unrelated COW fork workload ran in the IPC witness profile"
have 'VM_COW_SPLIT_FAILED_CLOSED' && die "a copy-on-write remap failed closed"

# ── Nothing may have gone wrong on the way ──
for bad in \
  'YARM_SPLIT_DISPATCH_FALLBACK' \
  'KERNEL PANIC' 'panicked at' 'UNHANDLED'; do
  have "$bad" && die "fatal or unexpected condition in boot log: $bad"
done

if (( fail )); then
  echo "IPC_RESIDUAL2_PARK_SEAL arch=$ARCH result=fail"
  exit 1
fi
note "a full endpoint parked the sender, the receiver drained, and the sender resumed"
echo "IPC_RESIDUAL2_PARK_SEAL arch=$ARCH proposed=$proposed committed=$committed resumed=$completed stream_ok=1 broad_entries=0 result=ok"
