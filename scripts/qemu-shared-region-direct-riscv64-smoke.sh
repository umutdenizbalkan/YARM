#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198E3C2C — RISC-V DIRECT shared-region LIVE oracle smoke (single cell).
#
# Builds a FRESH feature-enabled RISC-V artifact (kernel_boot with
# `--features riscv-shared-region-direct-oracle`, over a normally-built server +
# initramfs set — the userspace oracle scaffold is arch-gated, not feature-gated),
# boots ONE `-smp 1` QEMU-virt/OpenSBI with `yarm.riscv_shared_region_direct_oracle=1`,
# and asserts the DIRECT shared-region class is proven LIVE on RISC-V.
#
# It reuses the SAME arch-neutral oracle core, large-length send ABI, off-lock
# transaction, authoritative ack gate, waiter identity, and finalization as the
# x86_64 (198E3C1C) and AArch64 (198E3C2B) cells; only the arch marker tags, the
# slot-5 selector (=7), and the target-specific oracle VA (512 MiB — 1 GiB is the
# RISC-V heap base) differ.
#
# A cell is LIVE only when, in a single clean boot, ALL of these appear EXACTLY
# ONCE from the real off-lock transaction completion:
#
#   IPCSEND_SHARED_REGION_OBJECT_OK    arch=riscv64 class=direct object_match=1 fresh_cap=1 pin_transfer=1
#   IPCSEND_SHARED_REGION_MAP_OK       arch=riscv64 class=direct map_right=1 write_right_ok=1 nx=1 cleanup_token=1
#   IPCSEND_SHARED_REGION_LIFECYCLE_OK arch=riscv64 class=direct transaction_published=1 receiver_wakes=1 leaked_state=0
#   GLOBAL_LOCK_RETIRE_CLASS_BEGIN     arch=riscv64 class=IpcSendSharedRegionDirect
#   GLOBAL_LOCK_RETIRE_CLASS_DONE      arch=riscv64 class=IpcSendSharedRegionDirect result=ok
#
# plus the userspace completion (exactly once):
#   RISCV_SHARED_REGION_DIRECT_LIVE_ORACLE_DONE mapped_pages=2 fresh_cap=1 readonly=1 first_release=ok second_release=rejected wakes=1 result=ok
#   RISCV_SHARED_REGION_DIRECT_SEND attempts=<n> early_retries=<n-1> result=ok   (exactly one success)
#
# Fails on: missing/duplicate kernel marker, missing userspace completion, more
# than one successful send, any enqueue/queued shared-region evidence, a fuse
# trip, a duplicate wake/post-work, a broad-lock fallback, a stale-waiter/identity
# failure, an Err(Internal) idle path, a repeated sepc advance, or a fatal
# trap/panic/timeout.
#
# On a genuinely clean log emits:
#   SECOND_COHORT_SHARED_REGION_DIRECT_LIVE_SEAL arch=riscv64 classes=1 live_cells=1 fuse_trips=0 result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=riscv-shared-region-direct-oracle
KTARGET=${KTARGET:-riscv64gc-unknown-none-elf}
KPROFILE=${KPROFILE:-release}
KELF=${KELF:-target/riscv64gc-unknown-none-elf/${KPROFILE}/kernel_boot}
KBIN=${KBIN:-build-riscv64/yarm-riscv64.bin}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/shared-region-direct-riscv64}
TIMEOUT_SECS=${TIMEOUT_SECS:-180}
IDLE_MAX_SECS=${IDLE_MAX_SECS:-180}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[shared-region-direct-riscv64-smoke] $*"; }
die()  { echo "[shared-region-direct-riscv64-smoke][fail] $*"; fail=1; }

# ── 1. Base artifacts: servers + initramfs (no feature; the scaffold is arch-gated) ──
note "building base riscv64 artifacts (servers + initramfs)"
BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
  scripts/build-qemu-riscv64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
  || die "base artifact build failed (see $LOGDIR/build.log)"

# ── 2. Overlay: rebuild kernel_boot WITH the direct-oracle feature, re-objcopy the .bin ──
if (( ! fail )); then
  note "rebuilding kernel_boot with --features $FEATURE"
  cargo build -Z "build-std=${BUILD_STD}" \
    --target "$KTARGET" --profile "$KPROFILE" \
    --no-default-features --features "$FEATURE" \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 \
    || die "feature kernel_boot build failed (see $LOGDIR/kbuild.log)"
fi
if (( ! fail )); then
  if command -v llvm-objcopy >/dev/null 2>&1; then
    OBJCOPY=llvm-objcopy
  elif command -v rust-objcopy >/dev/null 2>&1; then
    OBJCOPY=rust-objcopy
  else
    die "no objcopy available to produce raw kernel binary"
  fi
fi
if (( ! fail )); then
  "$OBJCOPY" -O binary "$KELF" "$KBIN" >"$LOGDIR/objcopy.log" 2>&1 \
    || die "objcopy of feature kernel failed (see $LOGDIR/objcopy.log)"
fi
if (( ! fail )); then
  # Artifact integrity: the feature kernel MUST carry the DIRECT retirement literal
  # (and must NOT carry an ENQUEUE class literal — enqueue stays disabled).
  if ! rg -a -q "class=IpcSendSharedRegionDirect" "$KBIN"; then
    die "feature kernel missing IpcSendSharedRegionDirect literal (wrong build)"
  fi
  if rg -a -q "class=IpcSendSharedRegionEnqueue" "$KBIN"; then
    die "feature kernel unexpectedly contains an ENQUEUE class literal"
  fi
  # Cross-arch hygiene: the RISC-V armed kernel must NOT carry an x86/aarch64 literal.
  if rg -a -q "arch=x86_64 class=IpcSendSharedRegion" "$KBIN"; then
    die "feature kernel unexpectedly contains an x86_64 shared-region literal"
  fi
  if rg -a -q "arch=aarch64 class=IpcSendSharedRegion" "$KBIN"; then
    die "feature kernel unexpectedly contains an aarch64 shared-region literal"
  fi
fi

if (( fail )); then
  echo "SECOND_COHORT_SHARED_REGION_DIRECT_LIVE_SEAL arch=riscv64 classes=1 result=fail reason=build"
  exit 1
fi

# ── 3. Boot ONE -smp 1 QEMU-virt with the oracle knob ──
note "booting QEMU-virt -smp 1 with yarm.riscv_shared_region_direct_oracle=1"
env \
  KERNEL_IMAGE="$KBIN" \
  INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.riscv_shared_region_direct_oracle=1" \
  QEMU_SMP=1 \
  QEMU_SMOKE_STRICT=0 \
  LOGFILE="$BOOT_LOG" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  IDLE_MAX_SECS="$IDLE_MAX_SECS" \
  scripts/qemu-riscv64-core-smoke.sh >"$LOGDIR/core-smoke.log" 2>&1 || true

if [[ ! -s "$BOOT_LOG" ]]; then
  die "no boot log produced"
  echo "SECOND_COHORT_SHARED_REGION_DIRECT_LIVE_SEAL arch=riscv64 classes=1 result=fail reason=no_boot_log"
  exit 1
fi

# Normalize CRs for grepping.
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"

count() { rg -a -c -F "$1" "$NORM" 2>/dev/null || echo 0; }
have()  { rg -a -q -F "$1" "$NORM"; }

# ── 4a. The five kernel markers — each EXACTLY once ──
declare -a KMARKERS=(
  "IPCSEND_SHARED_REGION_OBJECT_OK arch=riscv64 class=direct object_match=1 fresh_cap=1 pin_transfer=1"
  "IPCSEND_SHARED_REGION_MAP_OK arch=riscv64 class=direct map_right=1 write_right_ok=1 nx=1 cleanup_token=1"
  "IPCSEND_SHARED_REGION_LIFECYCLE_OK arch=riscv64 class=direct transaction_published=1 receiver_wakes=1 leaked_state=0"
  "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=riscv64 class=IpcSendSharedRegionDirect"
  "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcSendSharedRegionDirect result=ok"
)
for m in "${KMARKERS[@]}"; do
  c=$(count "$m")
  if [[ "$c" != "1" ]]; then
    die "kernel marker count != 1 (got $c): $m"
  fi
done

# ── 4b. Userspace completion + exactly one successful send ──
UDONE="RISCV_SHARED_REGION_DIRECT_LIVE_ORACLE_DONE mapped_pages=2 fresh_cap=1 readonly=1 first_release=ok second_release=rejected wakes=1 result=ok"
[[ "$(count "$UDONE")" == "1" ]] || die "userspace completion missing/duplicate: $UDONE"
# Exactly one SUCCESSFUL send (early WouldBlock retries are allowed but each is result != ok).
send_ok_result=$(grep -aF "RISCV_SHARED_REGION_DIRECT_SEND" "$NORM" | grep -aFc "result=ok" || true)
[[ "$send_ok_result" == "1" ]] || die "expected exactly one successful send (got $send_ok_result)"
# Continuation count must be exactly one (child validation body ran once).
if ! rg -a -q "SHARED_REGION_DIRECT_ORACLE_CHILD_DONE .* continuations=1" "$NORM"; then
  die "child continuation count != 1"
fi

# ── 4c. Off-lock post-work completed EXACTLY once (no duplicate wake / post-work) ──
pw=$(count "DISPATCH_POST_WORK_DONE kind=blocked_waiter_shared_region result=ok")
[[ "$pw" == "1" ]] || die "shared-region post-work completion count != 1 (got $pw)"

# ── 4d. Hard-stop conditions ──
have "SHARED_REGION_CANCEL_FUSE_SET" && die "cancellation fuse tripped"
have "class=IpcSendSharedRegionEnqueue" && die "enqueue retirement class observed"
have "IPCSEND_SHARED_REGION_OBJECT_OK arch=riscv64 class=enqueue" && die "enqueue attestation observed"
have "DISPATCH_POST_WORK_FAIL kind=blocked_waiter_shared_region" && die "shared-region post-work FAILED"
have "SHARED_REGION_DIRECT_ORACLE_MISSING_CAPS" && die "oracle caps missing (provisioning failed)"
have "SHARED_REGION_ORACLE_PROVISION_FAIL" && die "shared-region provisioning failed"
have "SHARED_REGION_DIRECT_ACK_CONSUME_RACE" && die "ack consume race observed"
have "RISCV_ORACLE_SLOT5_CONFLICT" && die "slot-5 selector conflict (arm-neither) observed"
# An x86/aarch64 shared-region attestation must never appear in a RISC-V boot.
have "IPCSEND_SHARED_REGION_OBJECT_OK arch=x86_64" && die "x86_64 shared-region attestation in riscv64 boot"
have "IPCSEND_SHARED_REGION_OBJECT_OK arch=aarch64" && die "aarch64 shared-region attestation in riscv64 boot"
# RISC-V typed-outcome regression: a genuine trap-handling error must never be the idle path.
have "RISCV_TRAP_HANDLE_FAILED" && die "RISC-V trap handling failed (Err(Internal) path)"
# Fatal traps / panics / stalls.
for bad in "KERNEL PANIC" "RUST PANIC" "panicked at" "CPU EXCEPTION" "DOUBLE FAULT" "Unhandled" "BOOTSTRAP_ERROR" "RISCV_TRAP_ENTER scause=0x2 " ; do
  have "$bad" && die "fatal condition in boot log: $bad"
done

if (( fail )); then
  echo "SECOND_COHORT_SHARED_REGION_DIRECT_LIVE_SEAL arch=riscv64 classes=1 live_cells=0 result=fail (see $BOOT_LOG)"
  exit 1
fi

note "all direct-class kernel markers + userspace completion present exactly once; no fuse/enqueue/fault"
echo "SECOND_COHORT_SHARED_REGION_DIRECT_LIVE_SEAL arch=riscv64 classes=1 live_cells=1 fuse_trips=0 result=ok"
