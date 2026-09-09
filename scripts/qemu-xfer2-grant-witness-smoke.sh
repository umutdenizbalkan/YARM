#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# U9-XFER2 §4 — the NR 30 + NR 4 end-to-end grant witness, per architecture.
#
# Usage: scripts/qemu-xfer2-grant-witness-smoke.sh [x86_64|aarch64|riscv64]
#
# What it proves, in ONE clean boot, over DISPOSABLE authority (the two-page MemoryObject and the
# endpoint capability `provision_init_shared_region_oracle` hands init — nothing the running
# system depends on):
#
#   * NR 30 `RecvSharedV3` RECEIVES and MAPS a grant, with the output record's metadata, rights and
#     backing all checked: `meta=1 perm=1 page0=1 page1=1`. `perm=1` is READ-ONLY, which is what the
#     grant's `READ | MAP` (no WRITE) rights entitle it to; `page0`/`page1` validate every byte of
#     BOTH pages independently against the deterministic pattern the kernel wrote, so a page swap or
#     a single-page mapping is detected rather than averaged away.
#   * NR 4 `TransferRelease` SUCCESSFULLY releases it: `release=1`.
#   * BOTH of NR 4's request shapes are covered, over SEPARATE grants, so neither can be satisfied
#     by state the other left behind: grant A `shape=registered_range` (`cap,0,0`), grant B
#     `shape=explicit_range` (`cap,base,len`).
#   * The mapping is really REMOVED (`unmapped=1`: a fresh anonymous mapping over the window
#     succeeds, which it could not if the pages were still there) and the capability is really
#     REVOKED (`revoked=1`: a duplicate release is refused).
#   * The accounting and final backing disposition are visible kernel-side as
#     `XFER_RELEASE_OK route=split pages=2 len=8192`, twice — once per grant.
#   * Init's ESSENTIAL capabilities survive: `caps_intact=1`, and the boot reaches its normal
#     terminal idle.
#
# Fails on: a missing or duplicated witness line, any grant reporting result=0, a broad-route
# release (the family is closed, so every release must be `route=split`), a lost commit race, a
# writeback rollback, or any fatal trap/panic/timeout.
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
    # x86_64 boots the ELF directly (multiboot), so the feature build is copied straight in.
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
    # AArch64 and RISC-V boot the RAW image, not the ELF — the same `objcopy` overlay the
    # established `qemu-shared-region-direct-<arch>-smoke.sh` cells perform.
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
  *) echo "[xfer2-witness][fail] unknown arch: $ARCH"; exit 1 ;;
esac

BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/xfer2-grant-witness-$ARCH}
TIMEOUT_SECS=${TIMEOUT_SECS:-120}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[xfer2-witness] $*"; }
die()  { echo "[xfer2-witness][fail] $*"; fail=1; }

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
  echo "XFER2_GRANT_WITNESS_SEAL arch=$ARCH result=fail reason=build"
  exit 1
fi

note "booting QEMU -smp 1 with yarm.xfer2_grant_witness=1"
env \
  KERNEL_IMAGE="$KERNEL_IMAGE" \
  INITRAMFS_IMAGE="$INITRAMFS_IMAGE" \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.xfer2_grant_witness=1" \
  QEMU_SMP=1 \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  "$SMOKE" >"$LOGDIR/core-smoke.log" 2>&1 || true

[[ -s "$BOOT_LOG" ]] || { echo "XFER2_GRANT_WITNESS_SEAL arch=$ARCH result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"

# `grep -c` exits 1 on zero matches, so the `|| echo 0` arm would otherwise APPEND a second line
# to grep's own "0" and every comparison below would see a two-line string. Count with `wc -l` on
# the match stream instead, which is single-valued and exits 0 either way.
count() { grep -a -F -- "$1" "$NORM" 2>/dev/null | wc -l | tr -d ' '; }
have()  { grep -a -q -F -- "$1" "$NORM"; }

# ── The witness itself ──
[[ "$(count 'XFER2_GRANT_WITNESS_BEGIN')" == "1" ]] || die "witness did not start exactly once"

# Both grants must report every property true. The full line is matched, so a partial success
# cannot pass by carrying the right prefix.
for shape in registered_range explicit_range; do
  # The VERDICT line only — `XFER2_GRANT_DETAIL` carries the observed values on its own line and
  # also names the shape, so an unqualified `shape=` match would count two lines per grant.
  m="XFER2_GRANT_WITNESS shape=$shape"
  n=$(count "$m")
  [[ "$n" == "1" ]] || die "expected exactly one $shape grant (got $n)"
  line=$(grep -a -m1 -- "$m" "$NORM" || true)
  for prop in meta=1 perm=1 page0=1 page1=1 release=1 unmapped=1 revoked=1 result=1; do
    case "$line" in
      *" $prop"*) ;;
      *) die "$shape grant missing $prop  [$line]" ;;
    esac
  done
done

[[ "$(count 'XFER2_GRANT_WITNESS_DONE grant_a=1 grant_b=1 caps_intact=1 result=ok')" == "1" ]] \
  || die "witness completion missing (grants, or essential caps, did not all pass)"

# ── Kernel side: two successful releases, both through the SPLIT route ──
rel=$(count 'XFER_RELEASE_OK route=split pages=2 len=8192')
[[ "$rel" == "2" ]] || die "expected two split-route releases of the two-page grant (got $rel)"
have 'XFER_RELEASE_OK route=broad' && die "a release fell to the broad route: the family is not closed"

# ── NR 30 must have delivered through the split route too, twice, mapped ──
v3=$(count 'RECV_V3_LIVE_MAPPED route=split')
[[ "$v3" == "2" ]] || die "expected two split-route mapped NR 30 deliveries (got $v3)"
have 'RECV_V3_LIVE_MAPPED route=broad' && die "an NR 30 delivery fell to the broad route"

# ── Nothing may have gone wrong on the way ──
for bad in \
  'RECV_V3_COMMIT_LOST_RACE' \
  'RECV_V3_WRITEBACK_FAIL_ROLLBACK' \
  'XFER_RELEASE_SHOOTDOWN_INCOMPLETE' \
  'XFER_RELEASE_RACED_REVOKE' \
  'XFER_REVOKE_LINKS_MOVED' \
  'VM_DISPLACED_SETTLE_IDENTITY_MISMATCH' \
  'YARM_SPLIT_DISPATCH_FALLBACK' \
  'KERNEL PANIC' 'panicked at' 'UNHANDLED'; do
  have "$bad" && die "fatal or unexpected condition in boot log: $bad"
done

if (( fail )); then
  echo "XFER2_GRANT_WITNESS_SEAL arch=$ARCH result=fail"
  exit 1
fi
note "NR 30 received+mapped and NR 4 released two disposable grants, both shapes, split route only"
echo "XFER2_GRANT_WITNESS_SEAL arch=$ARCH grants=2 shapes=registered_range,explicit_range releases=2 route=split result=ok"
