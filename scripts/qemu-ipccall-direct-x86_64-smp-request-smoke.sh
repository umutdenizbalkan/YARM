#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 199A2D2A — x86_64 SMP=2 cross-CPU DIRECT IpcCall (NR6 request-only) smoke.
#
# GOAL (Part 10): boot the SMP request oracle under QEMU_SMP=2 and attempt to observe a GENUINE
# cross-CPU NR6 request: a userspace IPC server BLOCKED in recv-v2 on CPU 1, one NR6 direct request
# from a CPU 0 client, a remote wake, and the server RESUMED on CPU 1 — with strictly cross-CPU
# markers:
#
#   IPCCALL_DIRECT_SMP_SERVER_BLOCKED arch=x86_64 server_cpu=1 ... result=ok
#   IPCCALL_DIRECT_SMP_REQUEST_OK     arch=x86_64 sender_cpu=0 receiver_cpu=1 cross_cpu=1 ... result=ok
#
# with sender_cpu != receiver_cpu.
#
# HONEST STATUS (Stage 199A2D2B): the GENERIC AP dispatch-on-wake MECHANISM now exists and is
# proven by hosted tests — the per-CPU reschedule-pending coalescing flag, the lost-wakeup-safe idle
# decision, and the owned scheduler-selected dispatch plan that distinguishes a FreshUserEntry from a
# BlockedUserResume (`src/arch/x86_64/ap_sched.rs`; `arch::x86_64::ap_sched::tests`). The CPU-targeted
# remote enqueue (via the accepted NR6 transaction's captured affinity), single-slot ack, and
# one-pair fuse were proven in Stage 199A2D2A (`stage199a2d1`/`stage199a2d2a_smp_request`). The
# remaining LIVE piece is the ARCH ASM half: the AP context-restore `iretq` that installs per-CPU
# CR3/TSS-RSP0/GS/FS and resumes a blocked task's saved continuation, plus the real recv-v2 server
# provisioned on CPU 1. Until that asm is wired + proven under QEMU, a server cannot actually block
# in recv-v2 on CPU 1 and be woken + RESUMED there. Presenting same-CPU execution as cross-CPU is a
# HARD-STOP, so this script NEVER emits `result=ok` without the genuine distinct-CPU markers.
#
# What this script proves honestly: the SMP request oracle knob boots CLEAN under QEMU_SMP=2 (no
# panic/fault/stall), both CPUs online, no ack overwrite, no service regression — i.e. arming the
# request-only selector introduces no SMP regression. Absent the genuine cross-CPU markers it emits
# `result=blocked` with the precise blocker reason.
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=x86-ipccall-direct-smp-oracle
KTARGET=${KTARGET:-targets/x86_64-yarm-none.json}
KPROFILE=${KPROFILE:-x86-none}
KELF=${KELF:-target/x86_64-yarm-none/${KPROFILE}/kernel_boot}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ipccall-direct-x86_64-smp-request}
TIMEOUT_SECS=${TIMEOUT_SECS:-150}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

SEAL_BLOCKED="STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL arch=x86_64 smp=2 pairs=1 sender_cpu=0 receiver_cpu=1 cross_cpu=0 duplicate_deliveries=0 duplicate_wakes=0 wrong_waiter_mutations=0 result=blocked reason=ap_saved_frame_restore_and_recv_v2_server_not_wired"

fail=0
note() { echo "[ipccall-direct-smp-request] $*"; }
die()  { echo "[ipccall-direct-smp-request][fail] $*"; fail=1; }

# ── 1. Base artifacts: servers + initramfs (no feature; the scaffold is arch-gated) ──
note "building base x86_64 artifacts (servers + initramfs)"
BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
  scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
  || die "base artifact build failed (see $LOGDIR/build.log)"

# ── 2. Overlay: rebuild kernel_boot WITH the SMP request oracle feature ──
if (( ! fail )); then
  note "rebuilding kernel_boot with --features $FEATURE"
  cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" \
    --no-default-features --features "$FEATURE" \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 \
    || die "feature kernel_boot build failed (see $LOGDIR/kbuild.log)"
fi
if (( ! fail )); then
  cp "$KELF" build-x86_64/kernel_boot.elf
fi

if (( fail )); then
  echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL arch=x86_64 smp=2 result=fail reason=build"
  exit 1
fi

# ── 3. Boot ONE -smp 2 QEMU with the SMP request oracle knob ──
note "booting QEMU -smp 2 with yarm.x86_64_ipccall_direct_smp_oracle=1"
env \
  KERNEL_IMAGE=build-x86_64/kernel_boot.elf \
  INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.x86_64_ipccall_direct_smp_oracle=1" \
  QEMU_SMP=2 \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  scripts/qemu-x86_64-core-smoke.sh >"$LOGDIR/core-smoke.log" 2>&1 || true

if [[ ! -s "$BOOT_LOG" ]]; then
  die "no boot log produced"
  echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL arch=x86_64 smp=2 result=fail reason=no_boot_log"
  exit 1
fi

NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
count() { rg -a -c -F "$1" "$NORM" 2>/dev/null || echo 0; }
have()  { rg -a -q -F "$1" "$NORM"; }

# ── 4. No SMP regression: clean boot, both CPUs online ──
have "X86_AP_ONLINE cpu=1" || die "CPU 1 did not come online under SMP=2"
have "YARM_X86_64_IPCCALL_DIRECT_SMP_ORACLE_SET enabled=true" || die "SMP request selector not set"

# ── 5. Hard-stops: no panic/fault/stall/overwrite/regression under SMP=2 ──
for bad in "KERNEL PANIC" "RUST PANIC" "panicked at" "CPU EXCEPTION" "DOUBLE FAULT" "Unhandled" "BOOTSTRAP_ERROR" \
           "IPCCALL_DIRECT_ACK_OVERWRITE_FUSE" "IPCREPLY_DIRECT_ACK_OVERWRITE_FUSE" ; do
  have "$bad" && die "fatal/overwrite condition under SMP=2: $bad"
done
# This is the REQUEST-only stage: a cross-CPU NR7 reply marker must NOT appear.
have "IPCREPLY_DIRECT_SMP_REPLY_OK" && die "unexpected NR7 SMP reply marker in a request-only stage"

# ── 6. Cross-CPU proof gate: require STRICTLY cross-CPU request markers with DISTINCT CPU IDs ──
req_ok=0
if have "IPCCALL_DIRECT_SMP_SERVER_BLOCKED arch=x86_64 server_cpu=1" \
   && have "IPCCALL_DIRECT_SMP_REQUEST_OK arch=x86_64"; then
  line=$(grep -aF "IPCCALL_DIRECT_SMP_REQUEST_OK arch=x86_64" "$NORM" | head -1)
  scpu=$(sed -n 's/.*sender_cpu=\([0-9]\+\).*/\1/p' <<<"$line")
  rcpu=$(sed -n 's/.*receiver_cpu=\([0-9]\+\).*/\1/p' <<<"$line")
  if [[ -n "$scpu" && -n "$rcpu" && "$scpu" != "$rcpu" ]]; then req_ok=1; fi
fi

if (( fail )); then
  echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL arch=x86_64 smp=2 result=fail (see $BOOT_LOG)"
  exit 1
fi

# Stage 199A2D2C2: the request-only cross-CPU seal requires the FULL live continuation sequence — a
# real CPU-1 recv-v2 block AND a real saved-continuation resume — not merely the cross-CPU request
# markers. Require the authoritative block marker + the userspace recv-v2 continuation.
blocked_ok=$(count "IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1")
cont_ok=$(count "X86_AP_RECV_V2_CONTINUED cpu=1")
if (( req_ok )) && [[ "$blocked_ok" == "1" && "$cont_ok" == "1" ]]; then
  # A GENUINE cross-CPU NR6 request with a real CPU-1 recv-v2 blocked continuation was observed.
  dup_deliv=$(count "IPCCALL_DIRECT_SMP_DUP_DELIVERY")
  dup_wake=$(count "IPCCALL_DIRECT_SMP_DUP_WAKE")
  [[ "$dup_deliv" == "0" && "$dup_wake" == "0" ]] || { echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL arch=x86_64 smp=2 result=fail reason=duplicate"; exit 1; }
  note "GENUINE cross-CPU NR6 request + CPU-1 recv-v2 continuation observed under SMP=2"
  echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL arch=x86_64 smp=2 pairs=1 sender_cpu=0 receiver_cpu=1 cross_cpu=1 request_copies=1 server_wakes=1 server_continuations=1 duplicate_deliveries=0 duplicate_wakes=0 wrong_cpu_continuations=0 result=ok"
  exit 0
fi

# No cross-CPU markers: the kernel is SMP=2-clean, but the AP dispatch-on-wake + context-restore
# path that would host + resume a blocked recv-v2 server on CPU 1 is not wired. Report the blocker.
note "SMP=2 boot clean; both CPUs online; NO cross-CPU NR6 markers (AP resume path not wired)"
echo "$SEAL_BLOCKED"
exit 0
