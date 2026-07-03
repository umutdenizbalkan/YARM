#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# x86_64 uniprocessor (-smp 1) core smoke test.
# Greps for hard boot blockers (exits nonzero), then checks service entry
# counts and IPC sequence markers.

set -euo pipefail
source "$(dirname "$0")/qemu-smoke-common.sh"

SMOKE_LOG=${SMOKE_LOG:-smoke.log}
: >"$SMOKE_LOG"
exec 19>>"$SMOKE_LOG"
export BASH_XTRACEFD=19
export PS4='+ ${BASH_SOURCE}:${LINENO}: '
set -x

KERNEL_IMAGE=${KERNEL_IMAGE:-build-x86_64/kernel_boot.elf}
KERNEL_DEBUG_ELF=${KERNEL_DEBUG_ELF:-build-x86_64/kernel_boot.elf}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-build-x86_64/initramfs-core.cpio}
TIMEOUT_SECS=${TIMEOUT_SECS:-60}
QEMU_SMOKE_STRICT=${QEMU_SMOKE_STRICT:-0}
QEMU_MACHINE=${QEMU_MACHINE:-q35}
QEMU_CPU=${QEMU_CPU:-qemu64}
QEMU_MEMORY=${QEMU_MEMORY:-512M}
# SMP defaults to 1 (x86_64 SMP is out of scope for the normal smoke). The Stage 177
# opt-in SMP_READY profile is the ONLY thing that raises this, below, after mode
# isolation (see the SMP_READY_CPUS override).
# Stage 183 (SMP-LIVE): honor a caller-provided QEMU_SMP (the runner's smp2/smp4
# profiles pass QEMU_SMP=2/4) so x86_64 -smp >1 boots can be driven; default -smp 1.
# The SMP_READY profile still overrides this below. NOT a production fallback knob —
# it only selects the QEMU CPU topology.
QEMU_SMP=${QEMU_SMP:-1}
DEFAULT_KERNEL_CMDLINE="console=ttyS0 rdinit=/init"
KERNEL_CMDLINE=${KERNEL_CMDLINE:-"$DEFAULT_KERNEL_CMDLINE"}
# Stage 168B (mode isolation): D6_SWITCH_PROOF, D6_SWITCH_A, and the
# D6_GENUINE/D2_RECV_GENUINE family are MUTUALLY-EXCLUSIVE kernel modes.
# Inherited/exported env from a prior run must never contaminate the current
# one — a clean D6_SWITCH_PROOF=1 regression must not append the genuine cmdline
# knobs nor check/require D6_GENUINE markers. Normalize here with a fixed
# precedence before any cmdline knob is appended or any check block runs:
#   D6_SWITCH_PROOF > D6_SWITCH_A > {D6_GENUINE, D2_RECV_GENUINE}
# The genuine pair may run together; a higher-precedence mode forces the lower
# ones off (with a warning). Set YARM_MODE_ISOLATION=0 to opt out.
D6_SWITCH_PROOF=${D6_SWITCH_PROOF:-0}
D6_SWITCH_A=${D6_SWITCH_A:-0}
D6_GENUINE=${D6_GENUINE:-0}
D2_RECV_GENUINE=${D2_RECV_GENUINE:-0}
D2_SEND_GENUINE=${D2_SEND_GENUINE:-0}
# Stage 171/172 (SCHED-TIMEOUT / VM-COW): pure DIAGNOSTIC overlays (no behavior
# change). Orthogonal to the genuine modes but forced off under the pure
# D6_SWITCH_PROOF / D6_SWITCH_A regressions so those runs stay uncontaminated.
SCHED_TIMEOUT=${SCHED_TIMEOUT:-0}
VM_COW=${VM_COW:-0}
CAP_CNODE=${CAP_CNODE:-0}
FAULT_DELIVERY=${FAULT_DELIVERY:-0}
SPAWN_LIFECYCLE=${SPAWN_LIFECYCLE:-0}
GLOBAL_STATE=${GLOBAL_STATE:-0}
SMP_READY=${SMP_READY:-0}
CROSS_ARCH_D6=${CROSS_ARCH_D6:-0}
D3_FULL=${D3_FULL:-0}
# Stage 181: UNLOCK_GRADUATED is tri-state: "" (kernel default: graduated on -smp1),
# "1" (explicit graduate), "0" (emergency opt-out / conservative). Under the D6 proof
# / switch-a isolation it is forced to "0" so those proof modes own the switch path.
UNLOCK_GRADUATED=${UNLOCK_GRADUATED:-}
YARM_MODE_ISOLATION=${YARM_MODE_ISOLATION:-1}
if [[ "$YARM_MODE_ISOLATION" == "1" ]]; then
  if [[ "$D6_SWITCH_PROOF" == "1" ]]; then
    for _mode in D6_SWITCH_A D6_GENUINE D2_RECV_GENUINE D2_SEND_GENUINE SCHED_TIMEOUT VM_COW CAP_CNODE FAULT_DELIVERY SPAWN_LIFECYCLE GLOBAL_STATE SMP_READY CROSS_ARCH_D6 D3_FULL UNLOCK_GRADUATED; do
      if [[ "${!_mode}" == "1" ]]; then
        echo "[warn] mode isolation: D6_SWITCH_PROOF=1 active; forcing $_mode=0 (was 1)"
      fi
      printf -v "$_mode" '%s' 0
    done
  elif [[ "$D6_SWITCH_A" == "1" ]]; then
    for _mode in D6_GENUINE D2_RECV_GENUINE D2_SEND_GENUINE SCHED_TIMEOUT VM_COW CAP_CNODE FAULT_DELIVERY SPAWN_LIFECYCLE GLOBAL_STATE SMP_READY CROSS_ARCH_D6 D3_FULL UNLOCK_GRADUATED; do
      if [[ "${!_mode}" == "1" ]]; then
        echo "[warn] mode isolation: D6_SWITCH_A=1 active; forcing $_mode=0 (was 1)"
      fi
      printf -v "$_mode" '%s' 0
    done
  fi
fi
echo "[info] mode isolation: D6_SWITCH_PROOF=$D6_SWITCH_PROOF D6_SWITCH_A=$D6_SWITCH_A D6_GENUINE=$D6_GENUINE D2_RECV_GENUINE=$D2_RECV_GENUINE D2_SEND_GENUINE=$D2_SEND_GENUINE SCHED_TIMEOUT=$SCHED_TIMEOUT VM_COW=$VM_COW CAP_CNODE=$CAP_CNODE FAULT_DELIVERY=$FAULT_DELIVERY SPAWN_LIFECYCLE=$SPAWN_LIFECYCLE GLOBAL_STATE=$GLOBAL_STATE SMP_READY=$SMP_READY CROSS_ARCH_D6=$CROSS_ARCH_D6 D3_FULL=$D3_FULL UNLOCK_GRADUATED=${UNLOCK_GRADUATED:-<default>}"
# Stage 177 (SMP-READY): the normal x86_64 core smoke stays -smp 1. Only the opt-in
# SMP_READY profile (after mode isolation, so a forced-off SMP_READY keeps -smp 1)
# raises QEMU_SMP to SMP_READY_CPUS (default 2). This is the single place x86_64 SMP
# is allowed above 1, and only for the audit profile.
SMP_READY_CPUS=${SMP_READY_CPUS:-2}
if [[ "$SMP_READY" == "1" ]]; then
  QEMU_SMP="$SMP_READY_CPUS"
  echo "[info] SMP-READY profile: raising QEMU_SMP to $QEMU_SMP (SMP_READY_CPUS=$SMP_READY_CPUS)"
fi
if [[ "$D6_SWITCH_PROOF" == "1" && "$KERNEL_CMDLINE" != *"yarm.d6_switch_proof="* ]]; then
  KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.d6_switch_proof=1"
fi
# Stage 166 (D6-SWITCH-A): D6_SWITCH_A=1 appends yarm.d6_switch_a=1 to opt the
# first narrow production unlocked switch in (default-off; x86_64-only).
D6_SWITCH_A=${D6_SWITCH_A:-0}
if [[ "$D6_SWITCH_A" == "1" && "$KERNEL_CMDLINE" != *"yarm.d6_switch_a="* ]]; then
  KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.d6_switch_a=1"
fi
# Stage 182 (REMOVE-FALLBACKS): the D6/D2-RECV/D2-SEND graduated seams are now the
# only x86_64 -smp1 production path (compile-time gate in the kernel — no runtime
# toggle). The old `yarm.d6_genuine` / `yarm.d2_recv_genuine` / `yarm.d2_send_genuine`
# SELECTOR knobs are DELETED and no longer appended to the cmdline. The D6_GENUINE /
# D2_RECV_GENUINE / D2_SEND_GENUINE env vars are retained ONLY as diagnostic marker-check
# selectors (the seam markers appear on every normal boot now that the seam is always
# active); they must not append an obsolete kernel knob or select any fallback path.
D6_GENUINE=${D6_GENUINE:-0}
D2_RECV_GENUINE=${D2_RECV_GENUINE:-0}
# A blocking send only happens when a sender must wait; the Stage 163P sender-wake proof
# workload deterministically creates exactly that, so the D2_SEND_GENUINE marker-check
# also turns on the sender-wake workload (which exercises the blocking send + regression-
# checks the Stage 163P oracle). Override by pre-setting IPC_RECV_PROOF*.
D2_SEND_GENUINE=${D2_SEND_GENUINE:-0}
if [[ "$D2_SEND_GENUINE" == "1" ]]; then
  IPC_RECV_PROOF=${IPC_RECV_PROOF:-1}
  IPC_RECV_PROOF_SENDER_WAKE=${IPC_RECV_PROOF_SENDER_WAKE:-1}
fi
# Stage 171 (SCHED-TIMEOUT): SCHED_TIMEOUT=1 appends yarm.sched_timeout=1 to emit
# the scheduler timeout/deadline diagnostic markers (arch-neutral; no behavior
# change). Also auto-enables the sender-wake proof workload so blocking IPC
# recv/send with deadlines is exercised (a superset of the idle-safety markers).
SCHED_TIMEOUT=${SCHED_TIMEOUT:-0}
if [[ "$SCHED_TIMEOUT" == "1" ]]; then
  if [[ "$KERNEL_CMDLINE" != *"yarm.sched_timeout="* ]]; then
    KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.sched_timeout=1"
  fi
  IPC_RECV_PROOF=${IPC_RECV_PROOF:-1}
  IPC_RECV_PROOF_SENDER_WAKE=${IPC_RECV_PROOF_SENDER_WAKE:-1}
fi
# Stage 172 (VM-COW): VM_COW=1 appends yarm.vm_cow=1 to emit the VM/COW/page-table/
# fork phase-boundary diagnostic markers (arch-neutral; no behavior change). Also
# auto-enables the sender-wake proof workload, which forks (COW clone) and writes
# to shared pages (COW write faults) — deterministically exercising the COW path.
VM_COW=${VM_COW:-0}
if [[ "$VM_COW" == "1" ]]; then
  if [[ "$KERNEL_CMDLINE" != *"yarm.vm_cow="* ]]; then
    KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.vm_cow=1"
  fi
  IPC_RECV_PROOF=${IPC_RECV_PROOF:-1}
  IPC_RECV_PROOF_SENDER_WAKE=${IPC_RECV_PROOF_SENDER_WAKE:-1}
fi
# Stage 173 (CAP-CNODE): CAP_CNODE=1 appends yarm.cap_cnode=1 to emit the
# capability/CNode phase-boundary diagnostic markers + run the one-shot cap/CNode
# lifecycle proof (arch-neutral; no behavior change). Standalone — it does NOT
# enable any D6/D2 mode and is NOT auto-enabled by the IPC proof workloads. The
# reply/transfer markers fire naturally from the boot's spawn IPC (reply caps +
# cap transfer already occur every boot); the one-shot proof provides the
# deterministic reserve/materialize/lookup/release/invariant markers.
CAP_CNODE=${CAP_CNODE:-0}
if [[ "$CAP_CNODE" == "1" && "$KERNEL_CMDLINE" != *"yarm.cap_cnode="* ]]; then
  KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.cap_cnode=1"
fi
# Stage 174 (FAULT-DELIVERY): FAULT_DELIVERY=1 appends yarm.fault_delivery=1 to
# emit the kernel-fault → supervisor delivery / fault-channel lifecycle diagnostic
# markers + run the one-shot self-contained fault-delivery proof (arch-neutral; no
# behavior change). Standalone — it does NOT enable any D6/D2 mode and is NOT
# auto-enabled by the IPC proof workloads. The classify markers fire naturally
# from the boot's handled COW faults; the one-shot proof provides the
# deterministic classify/msg-build/endpoint/queue/dequeue/invariant markers.
FAULT_DELIVERY=${FAULT_DELIVERY:-0}
if [[ "$FAULT_DELIVERY" == "1" && "$KERNEL_CMDLINE" != *"yarm.fault_delivery="* ]]; then
  KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.fault_delivery=1"
fi
# Stage 175 (SPAWN-LIFECYCLE): SPAWN_LIFECYCLE=1 appends yarm.spawn_lifecycle=1 to
# emit the spawn / image-loading / lifecycle-metadata phase markers + run the
# one-shot self-contained spawn-rollback proof (arch-neutral; no behavior change).
# Standalone — it does NOT enable any D6/D2 mode and is NOT auto-enabled by the IPC
# proof workloads. The phase markers fire naturally from the boot's service spawns
# (every SpawnFromInitramfsFile); the one-shot proof provides the deterministic
# rollback/invariant markers.
SPAWN_LIFECYCLE=${SPAWN_LIFECYCLE:-0}
if [[ "$SPAWN_LIFECYCLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.spawn_lifecycle="* ]]; then
  KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.spawn_lifecycle=1"
fi
# Stage 176 (GLOBAL-STATE): GLOBAL_STATE=1 appends yarm.global_state=1 to emit the
# remaining direct global-KernelState mutation audit + lock-rank discipline markers
# + run the one-shot read-only global-state audit (arch-neutral; no behavior change).
# Standalone — it does NOT enable any D6/D2 mode and is NOT auto-enabled by the IPC
# proof workloads.
GLOBAL_STATE=${GLOBAL_STATE:-0}
if [[ "$GLOBAL_STATE" == "1" && "$KERNEL_CMDLINE" != *"yarm.global_state="* ]]; then
  KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.global_state=1"
fi
# Stage 177 (SMP-READY): SMP_READY=1 appends yarm.smp_ready=1 to emit the x86_64
# SMP-readiness audit markers (AP bring-up mirror + one-shot per-CPU/scheduler/
# remote-wake/IPI audit; arch-neutral, no behavior change — APs stay parked/BSP-only).
# Standalone — it does NOT enable any D6/D2 mode.
SMP_READY=${SMP_READY:-0}
if [[ "$SMP_READY" == "1" && "$KERNEL_CMDLINE" != *"yarm.smp_ready="* ]]; then
  KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.smp_ready=1"
fi
# Stage 178 (CROSS-ARCH-D6): CROSS_ARCH_D6=1 appends yarm.cross_arch_d6=1 to emit the
# per-arch D6 restore-path audit markers (arch-neutral; no behavior change / no
# cross-arch D6 live-wire). On x86_64 the audit records model=switch_frames and defers
# to the ALREADY-ACCEPTED D6 path (observe-only; it does NOT touch D6_SWITCH_A/GENUINE).
# Standalone — it does NOT enable any D6/D2 mode.
CROSS_ARCH_D6=${CROSS_ARCH_D6:-0}
if [[ "$CROSS_ARCH_D6" == "1" && "$KERNEL_CMDLINE" != *"yarm.cross_arch_d6="* ]]; then
  KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.cross_arch_d6=1"
fi
# Stage 179 (D3-FULL): D3_FULL=1 appends yarm.d3_full=1 to emit the D3 VM
# anon-map/unmap two-phase markers + run the one-shot self-contained D3 proof (drives
# the real VM primitives on a scratch ASID; local flush live, remote shootdown
# deferred; no production VM ABI change). Standalone — does NOT enable any D6/D2 mode.
D3_FULL=${D3_FULL:-0}
if [[ "$D3_FULL" == "1" && "$KERNEL_CMDLINE" != *"yarm.d3_full="* ]]; then
  KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.d3_full=1"
fi
# Stage 182 (REMOVE-FALLBACKS): the graduated seams are the only x86_64 -smp1 production
# path; the `yarm.unlock_graduated` umbrella knob (incl. its `=0` emergency opt-out) is
# DELETED. The normal smoke exercises the graduated path with NO cmdline knob. The
# UNLOCK_GRADUATED env is retained only so an OBSOLETE-knob passthrough can be verified
# to NOT re-enable the old fallback (the acceptance block below asserts that). When it is
# explicitly set we deliberately still pass the now-obsolete kernel token so the kernel's
# UNLOCK_FALLBACK_KNOB_OBSOLETE "ignored" path is exercised — it must NOT change behavior.
UNLOCK_GRADUATED=${UNLOCK_GRADUATED:-}
if [[ -n "$UNLOCK_GRADUATED" && "$KERNEL_CMDLINE" != *"yarm.unlock_graduated="* ]]; then
  KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.unlock_graduated=$UNLOCK_GRADUATED"
fi
# Stage 159BC/D: the IPC recv-v2 oracle proof workload only runs when the kernel
# is booted with yarm.ipc_recv_proof=1. The oracle script sets IPC_RECV_PROOF=1
# whenever any proof requirement env var is enabled, so honor it here.
IPC_RECV_PROOF=${IPC_RECV_PROOF:-0}
if [[ "$IPC_RECV_PROOF" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_recv_proof="* ]]; then
  KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.ipc_recv_proof=1"
fi
# Stage 163: the sender-wake proof additionally needs the sub-knob
# yarm.ipc_recv_proof_sender_wake=1 (gates the coordination hook + workload).
IPC_RECV_PROOF_SENDER_WAKE=${IPC_RECV_PROOF_SENDER_WAKE:-0}
if [[ "$IPC_RECV_PROOF_SENDER_WAKE" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_recv_proof_sender_wake="* ]]; then
  KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.ipc_recv_proof_sender_wake=1"
fi

if [[ "$KERNEL_CMDLINE" != *"console="* ]] || [[ "${#KERNEL_CMDLINE}" -lt 12 ]]; then
  echo "[warn] suspicious KERNEL_CMDLINE override detected: '$KERNEL_CMDLINE'"
  echo "[hint] resetting to default kernel cmdline: '$DEFAULT_KERNEL_CMDLINE'"
  KERNEL_CMDLINE="$DEFAULT_KERNEL_CMDLINE"
fi

# ---------------------------------------------------------------------------
# Pre-flight: verify required files and qemu binary are present.
# ---------------------------------------------------------------------------

if [[ ! -f "$KERNEL_IMAGE" ]]; then
  echo "[warn] kernel image missing: $KERNEL_IMAGE"
  echo "[hint] run: scripts/build-qemu-x86_64-artifacts.sh"
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

if [[ ! -f "$INITRAMFS_IMAGE" ]]; then
  echo "[warn] initramfs image missing: $INITRAMFS_IMAGE"
  echo "[hint] run: scripts/build-qemu-x86_64-artifacts.sh"
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
  echo "[warn] qemu-system-x86_64 not installed"
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

# ---------------------------------------------------------------------------
# Verify the kernel ELF has a PVH note so QEMU can direct-boot it.
# ---------------------------------------------------------------------------
check_x86_kernel_bootability() {
  local kernel="$1"
  if [[ ! -f "$kernel" ]]; then
    return 1
  fi
  if command -v readelf >/dev/null 2>&1; then
    # Presence of a PT_NOTE segment is necessary for PVH direct-boot.
    if ! readelf -l "$kernel" 2>/dev/null | rg -q "NOTE"; then
      echo "[warn] kernel ELF lacks a PT_NOTE program header; PVH entry note will be ignored by qemu"
      return 1
    fi
    # Check for Xen/PVH note by name.
    if readelf -n "$kernel" 2>/dev/null | rg -qi "(PVH|Xen)"; then
      return 0
    fi
    if readelf -S "$kernel" 2>/dev/null | rg -q "\.note\.Xen"; then
      return 0
    fi
    echo "[warn] kernel ELF has no verified PVH/Xen direct-boot note"
    return 1
  fi
  # readelf not available — assume bootable and let QEMU decide.
  echo "[warn] readelf not found; skipping PVH note check"
  return 0
}

if ! check_x86_kernel_bootability "$KERNEL_IMAGE"; then
  echo "[warn] kernel image may not be PVH direct-bootable: $KERNEL_IMAGE"
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

LOGFILE=${LOGFILE:-qemu-x86_64-core.log}
rm -f "$LOGFILE"

# ---------------------------------------------------------------------------
# QEMU command — exactly as specified: q35, 512M, -smp 1, kernel_boot.elf,
# initramfs-core.cpio, "console=ttyS0 rdinit=/init".
# ---------------------------------------------------------------------------
QEMU_CMD=(
  qemu-system-x86_64
  -machine "$QEMU_MACHINE"
  -cpu "$QEMU_CPU"
  -m "$QEMU_MEMORY"
  -smp "$QEMU_SMP"
  -nographic
  -monitor none
  -serial stdio
  -no-reboot
  -no-shutdown
  -kernel "$KERNEL_IMAGE"
  -initrd "$INITRAMFS_IMAGE"
  -append "$KERNEL_CMDLINE"
)

echo "[info] qemu command: ${QEMU_CMD[*]}"
echo "[info] waiting up to ${TIMEOUT_SECS}s for boot markers..."

# ---------------------------------------------------------------------------
# Run QEMU with timeout, capture output to LOGFILE.
# ---------------------------------------------------------------------------
set +e
if command -v timeout >/dev/null 2>&1; then
  timeout --foreground "${TIMEOUT_SECS}s" stdbuf -oL -eL "${QEMU_CMD[@]}" 2>&1 | tee "$LOGFILE"
  QEMU_STATUS=${PIPESTATUS[0]}
else
  echo "[warn] 'timeout' command unavailable; qemu run may not auto-terminate"
  stdbuf -oL -eL "${QEMU_CMD[@]}" 2>&1 | tee "$LOGFILE"
  QEMU_STATUS=${PIPESTATUS[0]}
fi
set -e

log_has_pattern() {
  local pattern="$1"
  [[ -f "$LOGFILE" ]] || return 1
  tr '\r' '\n' <"$LOGFILE" | rg -a -n "$pattern" >/dev/null 2>&1
}

log_count_pattern() {
  local pattern="$1"
  [[ -f "$LOGFILE" ]] || { echo 0; return; }
  # Use word boundaries so e.g. VFS_SRV_ENTRY does not match DEVFS_SRV_ENTRY.
  tr '\r' '\n' <"$LOGFILE" | rg -a -c "\b${pattern}\b" 2>/dev/null || echo 0
}

# ---------------------------------------------------------------------------
# Hard blocker check — exit nonzero immediately if the log shows a crash or
# missing critical ELF that means the boot never reached userspace.
# ---------------------------------------------------------------------------
HARD_BLOCKER_PATTERNS=(
  "YARM_SUPERVISOR_ELF_MISSING"
  "YARM_PM_ELF_MISSING"
  "BOOTSTRAP_ERROR"
  "PM_PANIC"
  "INIT_PANIC"
  "^PANIC "
  "D2_PUBLISH_RACE_UNWIND"
)

hard_blocker_found=0
for blocker in "${HARD_BLOCKER_PATTERNS[@]}"; do
  if log_has_pattern "$blocker"; then
    echo "[error] hard boot blocker detected in log: $blocker"
    hard_blocker_found=1
  fi
done

if [[ "$hard_blocker_found" -eq 1 ]]; then
  echo "[error] hard boot blockers present — x86_64 smoke FAILED"
  if [[ -f "$LOGFILE" ]]; then
    echo "[info] last 40 log lines from $LOGFILE:"
    tail -n 40 "$LOGFILE" || true
  fi
  exit 1
fi

# ---------------------------------------------------------------------------
# Kernel boot sequence check (required markers in order).
# ---------------------------------------------------------------------------
KERNEL_BOOT_SEQUENCE=(
  "YARM_BOOT_PVH_START_INFO"
  "YARM_BOOT_OK"
  "YARM_SUPERVISOR_TID2_SPAWNED"
  "YARM_PM_TID3_SPAWNED"
)

FIRMWARE_FALLBACK_REGEX="SeaBIOS|iPXE|Booting from ROM"

if log_has_pattern "$FIRMWARE_FALLBACK_REGEX" && ! log_has_pattern "YARM_BOOT_PVH_START_INFO"; then
  echo "[warn] firmware fallback detected — QEMU did not accept the kernel as a PVH direct-boot image"
  echo "[hint] serial shows SeaBIOS/iPXE without any YARM_BOOT_PVH_START_INFO marker"
  if [[ -f "$LOGFILE" ]]; then
    echo "[info] last 20 log lines from $LOGFILE:"
    tail -n 20 "$LOGFILE" || true
  fi
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

if ! log_has_pattern "YARM_BOOT_PVH_START_INFO"; then
  echo "[warn] PVH boot marker not found — kernel may not have reached C entry"
  if [[ "$QEMU_STATUS" -eq 124 ]]; then
    echo "[warn] timeout reached (${TIMEOUT_SECS}s)"
  fi
  if [[ -f "$LOGFILE" ]]; then
    echo "[info] last 20 log lines from $LOGFILE:"
    tail -n 20 "$LOGFILE" || true
  fi
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

if ! check_log_sequence "$LOGFILE" "${KERNEL_BOOT_SEQUENCE[@]}"; then
  echo "[warn] kernel boot marker sequence missing or out of order"
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
fi

# ---------------------------------------------------------------------------
# IPC sequence (user_log! output; no-op in pure no_std builds but checked
# as a warn-only signal when present).
# ---------------------------------------------------------------------------
SPAWN_IPC_SEQUENCE=(
  "YARM_PM_RECV_LOOP_START"
  "INIT_SPAWN_V5_CALL_BEGIN"
  "INIT_SPAWN_V5_REPLY_RECV_OK"
)
if ! check_log_sequence "$LOGFILE" "${SPAWN_IPC_SEQUENCE[@]}"; then
  echo "[warn] PM/init IPC sequence absent (user_log! is a no-op in no_std; expected in hosted-dev)"
fi

# ---------------------------------------------------------------------------
# SharedKernel-primary trap ownership proof markers (Stage 2N / x86_64 -smp 1).
# Installed and first-shared-trap markers must appear once; fallback must be absent.
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  STAGE2N_INSTALLED=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "YARM_LOCK_SPLIT_STAGE2N_INSTALLED arch=x86_64 shared=1 raw=0" 2>/dev/null || echo 0)
  STAGE2N_FIRST=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "YARM_LOCK_SPLIT_STAGE2N_FIRST_SHARED_TRAP arch=x86_64" 2>/dev/null || echo 0)
  STAGE2N_FALLBACK=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "YARM_LOCK_SPLIT_STAGE2N_FALLBACK arch=x86_64" 2>/dev/null || echo 0)
  if [[ "$STAGE2N_INSTALLED" -eq 1 ]]; then
    echo "[ok] Stage2N: x86_64 installed shared trap state count=1"
  else
    echo "[warn] Stage2N: x86_64 installed marker count=${STAGE2N_INSTALLED} (expected 1)"
    stage2n_fail=1
  fi
  if [[ "$STAGE2N_FIRST" -eq 1 ]]; then
    echo "[ok] Stage2N: x86_64 first shared trap count=1"
  else
    echo "[warn] Stage2N: x86_64 first shared trap count=${STAGE2N_FIRST} (expected 1)"
    stage2n_fail=1
  fi
  if [[ "$STAGE2N_FALLBACK" -eq 0 ]]; then
    echo "[ok] Stage2N: x86_64 fallback count=0"
  else
    echo "[warn] Stage2N: x86_64 fallback count=${STAGE2N_FALLBACK} (expected 0)"
    stage2n_fail=1
  fi
  TID_MISMATCH=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "YARM_LOCK_SPLIT_CURRENT_TID_MISMATCH" 2>/dev/null || echo 0)
  if [[ "$TID_MISMATCH" -eq 0 ]]; then
    echo "[ok] L5B: x86_64 current-TID split-read mismatch count=0"
  else
    echo "[warn] L5B: x86_64 current-TID split-read mismatch count=${TID_MISMATCH} (expected 0 in normal build)"
    stage2n_fail=1
  fi
  if [[ "${stage2n_fail:-0}" -eq 1 && "$QEMU_SMOKE_STRICT" == "1" ]]; then
    echo "[error] strict x86_64 smoke: Stage2N SharedKernel-primary marker check failed"
    exit 1
  fi
fi

# ---------------------------------------------------------------------------
# Service entry count check.
# Each of the six services must appear EXACTLY ONCE in the log.
# ---------------------------------------------------------------------------
declare -A REQUIRED_SERVICE_ENTRIES
REQUIRED_SERVICE_ENTRIES=(
  [INITRAMFS_SRV_ENTRY]=1
  [DEVFS_SRV_ENTRY]=1
  [VFS_SRV_ENTRY]=1
  [DRIVER_MANAGER_ENTRY]=1
  [BLKCACHE_SRV_ENTRY]=1
  [VIRTIO_BLK_SRV_ENTRY]=1
  [DRIVER_MANAGER_READY]=1
  [BLKCACHE_SRV_READY]=1
  [VIRTIO_BLK_SRV_READY]=1
)

service_count_fail=0
for marker in "${!REQUIRED_SERVICE_ENTRIES[@]}"; do
  expected="${REQUIRED_SERVICE_ENTRIES[$marker]}"
  actual=$(log_count_pattern "$marker")
  if [[ "$actual" -eq "$expected" ]]; then
    echo "[ok] service entry count: ${marker}=${actual}"
  elif [[ "$actual" -eq 0 ]]; then
    echo "[warn] service entry MISSING: ${marker} (expected=${expected} got=0)"
    service_count_fail=1
  else
    echo "[warn] service entry count wrong: ${marker} expected=${expected} got=${actual}"
    service_count_fail=1
  fi
done

if [[ "$service_count_fail" -eq 1 ]]; then
  echo "[warn] one or more service entry counts wrong"
  if [[ "$QEMU_SMOKE_STRICT" == "1" ]]; then
    echo "[error] strict x86_64 smoke: service entry count check failed"
    exit 1
  fi
fi

# ---------------------------------------------------------------------------
# Phase 3B: zero-copy ELF loading baseline.
# All three late services (image_id 7/8/9) must use the ZC grant path with
# zc_pages > 0. Phase 2B bulk-read and Phase 2A bridge must be absent.
# x86_64 SMP remains out of scope; this smoke is always -smp 1.
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  phase3b_fail=0

  # PM_ELF_ZC_DONE must appear exactly once per image_id, with zc_pages > 0.
  for img_id in 7 8 9; do
    zc_count=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_ELF_ZC_DONE image_id=${img_id}\\b" 2>/dev/null || echo 0)
    zc_nonzero=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_ELF_ZC_DONE image_id=${img_id}\\b.*zc_pages=[1-9]" 2>/dev/null || echo 0)
    if [[ "$zc_count" -eq 1 && "$zc_nonzero" -eq 1 ]]; then
      echo "[ok] Phase 3B: PM_ELF_ZC_DONE image_id=${img_id} count=1 zc_pages>0"
    elif [[ "$zc_count" -eq 1 && "$zc_nonzero" -eq 0 ]]; then
      echo "[warn] Phase 3B: PM_ELF_ZC_DONE image_id=${img_id} count=1 but zc_pages=0 (CPIO or ELF alignment regression)"
      phase3b_fail=1
    else
      echo "[warn] Phase 3B: PM_ELF_ZC_DONE image_id=${img_id} expected=1 got=${zc_count}"
      phase3b_fail=1
    fi
  done

  # PM_ELF_ZC_FAIL must be 0.
  ZC_FAIL_TOTAL=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_ELF_ZC_FAIL" 2>/dev/null || echo 0)
  if [[ "$ZC_FAIL_TOTAL" -eq 0 ]]; then
    echo "[ok] Phase 3B: PM_ELF_ZC_FAIL count=0"
  else
    echo "[warn] Phase 3B: PM_ELF_ZC_FAIL count=${ZC_FAIL_TOTAL} (ZC loader errors)"
    phase3b_fail=1
  fi

  # PM_VFS_READ_BULK_DONE image_id=7/8/9 must be 0 (Phase 2B path must not activate).
  for img_id in 7 8 9; do
    bulk_done=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_VFS_READ_BULK_DONE image_id=${img_id}\\b" 2>/dev/null || echo 0)
    if [[ "$bulk_done" -eq 0 ]]; then
      echo "[ok] Phase 3B: PM_VFS_READ_BULK_DONE image_id=${img_id} count=0 (ZC path active)"
    else
      echo "[warn] Phase 3B: PM_VFS_READ_BULK_DONE image_id=${img_id} count=${bulk_done} (Phase 2B fallback active)"
      phase3b_fail=1
    fi
  done

  # PM_VFS_READ_BULK_PHASE2A_BEGIN must be 0 (Phase 2A bridge must not activate).
  PHASE2A_COUNT=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_VFS_READ_BULK_PHASE2A_BEGIN" 2>/dev/null || echo 0)
  if [[ "$PHASE2A_COUNT" -eq 0 ]]; then
    echo "[ok] Phase 3B: PM_VFS_READ_BULK_PHASE2A_BEGIN count=0"
  else
    echo "[warn] Phase 3B: PM_VFS_READ_BULK_PHASE2A_BEGIN count=${PHASE2A_COUNT} (Phase 2A bridge active)"
    phase3b_fail=1
  fi

  # Phase 3B summary.
  ZC_DONE_TOTAL=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_ELF_ZC_DONE" 2>/dev/null || echo 0)
  ZC_NONZERO_TOTAL=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_ELF_ZC_DONE.*zc_pages=[1-9]" 2>/dev/null || echo 0)
  echo "[ok] Phase 3B summary: PM_ELF_ZC_DONE total=${ZC_DONE_TOTAL} zc_pages>0 count=${ZC_NONZERO_TOTAL}"

  if [[ "$phase3b_fail" -eq 1 ]]; then
    echo "[warn] Phase 3B x86_64 (-smp 1) checks did not all pass"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  fi
fi

# ---------------------------------------------------------------------------
# Timer / scheduler progression (strict mode only).
# ---------------------------------------------------------------------------
if [[ "$QEMU_SMOKE_STRICT" == "1" ]]; then
  strict_fail=0

  for required_timer in "YARM_TIMER_IRQ_DELIVERED" "YARM_TIMER_EOI_DONE" "YARM_SCHED_TICK"; do
    if ! log_has_pattern "$required_timer"; then
      echo "[warn] strict smoke: missing timer/scheduler marker: $required_timer"
      strict_fail=1
    fi
  done

  tick_lines=$(tr '\r' '\n' <"$LOGFILE" | rg -a -o "YARM_SCHED_TICK cpu=[0-9]+ tick=[0-9]+" || true)
  tick_count=$(printf '%s\n' "$tick_lines" | rg -c "YARM_SCHED_TICK" 2>/dev/null || echo 0)
  first_tick=$(printf '%s\n' "$tick_lines" | head -n1 | awk -F'tick=' '{print $2}' | awk '{print $1}')
  last_tick=$(printf '%s\n' "$tick_lines" | tail -n1 | awk -F'tick=' '{print $2}' | awk '{print $1}')

  if [[ -z "$first_tick" || -z "$last_tick" || "$tick_count" -lt 2 ]]; then
    echo "[warn] strict smoke: need at least two scheduler tick markers (got ${tick_count:-0})"
    strict_fail=1
  elif (( last_tick <= first_tick )); then
    echo "[warn] strict smoke: scheduler tick did not progress (first=$first_tick last=$last_tick)"
    strict_fail=1
  fi

  if [[ "$strict_fail" -eq 1 ]]; then
    echo "[error] strict x86_64 smoke: timer/scheduler checks failed"
    exit 1
  fi
  echo "[ok] strict x86_64 smoke: timer IRQ + EOI + scheduler tick progression verified"
fi

# ---------------------------------------------------------------------------
# Optional FAT userspace mount/config smoke markers.
# Do not fail default core smoke profiles without a real FAT block image; set
# FAT_SMOKE_EXPECTED=1 when the profile is expected to spawn and mount FAT.
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  FAT_SMOKE_EXPECTED=${FAT_SMOKE_EXPECTED:-0}
  FAT_MARKERS=(
    INIT_FAT_SPAWN_BEGIN
    INIT_FAT_SPAWN_SKIPPED
    INIT_FAT_SPAWN_OK
    PM_IMAGE_ID_10_FAT_SRV
    FAT_CONFIG_FOUND
    FAT_BLOCK_BACKEND_STARTUP_CAP
    FAT_MOUNT_READY
    FAT_MOUNT_FAILED
    VFS_MOUNT_REGISTER_FAT_OK
  )
  fat_seen=0
  for marker in "${FAT_MARKERS[@]}"; do
    count=$(log_count_pattern "$marker")
    if [[ "$count" -gt 0 ]]; then
      fat_seen=1
    fi
    echo "[info] FAT smoke marker count: ${marker}=${count}"
  done
  if [[ "$FAT_SMOKE_EXPECTED" == "1" && "$fat_seen" -eq 0 ]]; then
    echo "[error] FAT smoke expected but no FAT markers were observed"
    exit 1
  fi
fi

# ---------------------------------------------------------------------------
# Optional RAMFS userspace mount/config smoke markers.
# Do not fail default core smoke profiles; set RAMFS_SMOKE_EXPECTED=1 when the
# profile is expected to spawn and mount RAMFS.
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  RAMFS_SMOKE_EXPECTED=${RAMFS_SMOKE_EXPECTED:-0}
  RAMFS_MARKERS=(
    INIT_RAMFS_SPAWN_BEGIN
    INIT_RAMFS_SPAWN_SKIPPED
    INIT_RAMFS_SPAWN_OK
    PM_IMAGE_ID_11_RAMFS_SRV
    RAMFS_CONFIG_FOUND
    RAMFS_CONFIG_DEFAULT
    RAMFS_MOUNT_READY
    RAMFS_MOUNT_FAILED
    VFS_MOUNT_REGISTER_RAMFS_OK
  )
  ramfs_seen=0
  for marker in "${RAMFS_MARKERS[@]}"; do
    count=$(log_count_pattern "$marker")
    if [[ "$count" -gt 0 ]]; then
      ramfs_seen=1
    fi
    echo "[info] RAMFS smoke marker count: ${marker}=${count}"
  done
  if [[ "$RAMFS_SMOKE_EXPECTED" == "1" ]]; then
    if [[ "$ramfs_seen" -eq 0 ]]; then
      echo "[error] RAMFS smoke expected but no RAMFS markers were observed"
      exit 1
    fi
    RAMFS_REQUIRED_MARKERS=(
      INIT_RAMFS_SPAWN_BEGIN
      INIT_RAMFS_SPAWN_OK
      PM_IMAGE_ID_11_RAMFS_SRV
      RAMFS_MOUNT_READY
      VFS_MOUNT_REGISTER_RAMFS_OK
    )
    for marker in "${RAMFS_REQUIRED_MARKERS[@]}"; do
      if [[ "$(log_count_pattern "$marker")" -eq 0 ]]; then
        echo "[error] RAMFS smoke expected marker missing: ${marker}"
        exit 1
      fi
    done
    if [[ "$(log_count_pattern RAMFS_CONFIG_FOUND)" -eq 0 && "$(log_count_pattern RAMFS_CONFIG_DEFAULT)" -eq 0 ]]; then
      echo "[error] RAMFS smoke expected config marker missing"
      exit 1
    fi
  fi
fi

# ---------------------------------------------------------------------------
# Optional EXT4 userspace spawn markers (profile-gated; informational only).
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  EXT4_MARKERS=(
    INIT_EXT4_SPAWN_BEGIN
    INIT_EXT4_SPAWN_SKIPPED
    INIT_EXT4_SPAWN_OK
    PM_IMAGE_ID_12_EXT4_SRV
    EXT4_SRV_READY
  )
  for marker in "${EXT4_MARKERS[@]}"; do
    count=$(log_count_pattern "$marker")
    echo "[info] EXT4 smoke marker count: ${marker}=${count}"
  done
fi

# ---------------------------------------------------------------------------
# Summary.
# ---------------------------------------------------------------------------
if [[ "$service_count_fail" -eq 0 ]]; then
  echo "[ok] x86_64 core smoke: all 6 service entries present exactly once"
else
  echo "[warn] x86_64 core smoke: completed with service entry warnings (status=$QEMU_STATUS)"
fi

if [[ "$D6_SWITCH_PROOF" == "1" ]]; then
  proof_fail=0
  for proof_marker in \
    "D6_CONTROLLED_SWITCH_PROOF_DONE" \
    "D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH" \
    "D6_SWITCH_FRAMES_ENTER_UNLOCKED" \
    "D6_FIRST_RESUME_ENTER" \
    "D6_SWITCH_FRAMES_RETURNED_UNLOCKED"; do
    if log_has_pattern "$proof_marker"; then
      echo "[ok] D6 switch proof marker present: $proof_marker"
    else
      echo "[error] D6 switch proof marker missing: $proof_marker"
      proof_fail=1
    fi
  done
  # Stage 165B: early proof markers alone do NOT prove success.  A healthy D6
  # proof boot must not emit ANY raw fatal breadcrumb AFTER the proof begins.
  # The Stage 165 crash printed `[ok]` for every early marker and then faulted
  # (#PF `!Fv…`/`!BNv…`) in the post-proof trap path, which this gate now rejects.
  fatal_after_proof=0
  if [[ -f "$LOGFILE" ]]; then
    proof_tail="$(tr '\r' '\n' <"$LOGFILE" \
      | awk '/D6_CONTROLLED_SWITCH_PROOF_BEGIN/{seen=1} seen{print}')"
    for fatal_pat in '!Fv' '!BNv' 'PAGE_FAULT' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$proof_tail" | rg -a -F -q -- "$fatal_pat"; then
        echo "[error] D6 switch proof: fatal breadcrumb after proof start: $fatal_pat"
        fatal_after_proof=1
      fi
    done
  fi
  if [[ "$fatal_after_proof" -eq 0 ]]; then
    echo "[ok] D6 switch proof: no fatal breadcrumb after proof start"
  fi
  # Stage 165C/165D: hard proof-setup/cleanup failure markers (unconditional).
  # These indicate a D6 stack-mapping step aborted even if no raw fatal
  # breadcrumb was printed.
  map_fail=0
  for map_fail_marker in \
    "D6_PROOF_LIVE_RSP_STACK_MAP_FAILED" \
    "D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_FAILED" \
    "D6_POST_CLEANUP_STACK_MAP_FAILED" \
    "D6_POST_CLEANUP_STACK_MAP_SKIP" \
    "D6_FIRST_RESUME_STASH_MISSING"; do
    if log_has_pattern "$map_fail_marker"; then
      echo "[error] D6 switch proof: failure marker present: $map_fail_marker"
      map_fail=1
    fi
  done
  # Stage 165D: the post-cleanup shared-stack mapping must not report any failure
  # (per-root result=failed, or a DONE line with a nonzero failure count).
  if [[ -f "$LOGFILE" ]]; then
    if tr '\r' '\n' <"$LOGFILE" | rg -a -q -- 'D6_POST_CLEANUP_STACK_MAP_ROOT .*result=failed'; then
      echo "[error] D6 switch proof: post-cleanup stack map root result=failed"
      map_fail=1
    fi
    if tr '\r' '\n' <"$LOGFILE" \
        | rg -a -- 'D6_POST_CLEANUP_STACK_MAP_DONE' \
        | rg -av -- 'failures=0' \
        | rg -aq -- 'failures='; then
      echo "[error] D6 switch proof: post-cleanup stack map reported failures>0"
      map_fail=1
    fi
    # Stage 165F: a schedulable task's guard-adjacent page must be included.
    if tr '\r' '\n' <"$LOGFILE" | rg -a -q -- 'D6_POST_CLEANUP_STACK_MAP_GUARD_PAGE .*included=0'; then
      echo "[error] D6 switch proof: post-cleanup guard-adjacent page not included"
      map_fail=1
    fi
    # Stage 165G: a no-owner (idle/trap-capable, e.g. tid=0) stack must be mapped,
    # not left as an "ignorable" NOTE — its kernel stack can still take a trap.
    if tr '\r' '\n' <"$LOGFILE" | rg -a -q -- 'D6_POST_CLEANUP_STACK_MAP_NOTE .*reason=no_owner_asid_unmapped_not_schedulable'; then
      echo "[error] D6 switch proof: no-owner kernel stack left unmapped (NOTE)"
      map_fail=1
    fi
  fi
  # Stage 165D / 166B: D6_KERNEL_SWITCH_STACK_CHECK_FAILED is a stack-mapping
  # *retry* breadcrumb (early `target_asid_unavailable` before the target ASID is
  # bound).  The Stage 165D heuristic — "fail unless a later CHECK_OK exists for
  # that tid" — is a STALE false negative: once the proof actually completes via
  # the accepted path (D6-SWITCH-A or a successful switch), the mapping succeeds
  # through a different code path that need not emit a matching CHECK_OK marker.
  # So suppress this heuristic when the proof completed cleanly: PROOF_DONE +
  # CLEANUP_DONE present, POST_CLEANUP failures=0, and no fatal breadcrumb after
  # proof start.  All hard runtime gates above (fatal breadcrumbs, SKIP, ROOT
  # result=failed, DONE failures>0, GUARD_PAGE included=0, no-owner NOTE,
  # MAP_ACTIVE_FAILED, LIVE_RSP_STACK_MAP_FAILED, FIRST_RESUME_STASH_MISSING)
  # remain unconditional, so runtime safety is unchanged.
  proof_completed_clean=0
  if [[ -f "$LOGFILE" ]] \
     && [[ "$fatal_after_proof" -eq 0 ]] \
     && log_has_pattern "D6_CONTROLLED_SWITCH_PROOF_DONE" \
     && log_has_pattern "D6_CONTROLLED_SWITCH_PROOF_CLEANUP_DONE" \
     && tr '\r' '\n' <"$LOGFILE" \
          | rg -a -- 'D6_POST_CLEANUP_STACK_MAP_DONE' \
          | rg -aq -- 'failures=0\b'; then
    proof_completed_clean=1
  fi
  if [[ "$proof_completed_clean" -eq 1 ]]; then
    echo "[ok] D6 switch proof: completed clean (PROOF_DONE/CLEANUP_DONE/failures=0, no fatal); skipping stale CHECK_FAILED-without-CHECK_OK heuristic"
  elif [[ -f "$LOGFILE" ]]; then
    check_failed_tids="$(tr '\r' '\n' <"$LOGFILE" \
      | rg -a -o -- 'D6_KERNEL_SWITCH_STACK_CHECK_FAILED tid=[0-9]+' \
      | rg -a -o -- 'tid=[0-9]+' | sort -u)"
    for ft in $check_failed_tids; do
      if tr '\r' '\n' <"$LOGFILE" | rg -a -q -- "D6_KERNEL_SWITCH_STACK_CHECK_OK ${ft}\b"; then
        echo "[ok] D6 switch proof: ${ft} CHECK_FAILED was retried and later CHECK_OK"
      else
        echo "[error] D6 switch proof: ${ft} CHECK_FAILED with no later CHECK_OK"
        map_fail=1
      fi
    done
  fi
  if [[ "$map_fail" -eq 0 ]]; then
    echo "[ok] D6 switch proof: no unresolved stack-mapping failures"
  fi
  if [[ "$proof_fail" -eq 1 || "$fatal_after_proof" -eq 1 || "$map_fail" -eq 1 ]]; then
    echo "[error] D6 switch proof mode FAILED"
    exit 1
  fi
fi

# Stage 166 (D6-SWITCH-A): when booted with yarm.d6_switch_a=1, require evidence
# of at least one real production unlocked switch, and reject any fatal
# breadcrumb after the switch begins.
if [[ "$D6_SWITCH_A" == "1" ]]; then
  switch_a_fail=0
  echo "[ok] D6_SWITCH_A enabled marker:" $(log_has_pattern "D6_SWITCH_A_ENABLED" && echo present || echo MISSING)
  for sa_marker in \
    "D6_SWITCH_A_CANDIDATE" \
    "D6_SWITCH_A_LOCK_DROPPED" \
    "D6_SWITCH_A_SWITCH_ENTER" \
    "D6_SWITCH_A_RETURNED" \
    "D6_SWITCH_A_DONE"; do
    if log_has_pattern "$sa_marker"; then
      echo "[ok] D6-SWITCH-A marker present: $sa_marker"
    else
      echo "[error] D6-SWITCH-A marker missing: $sa_marker"
      switch_a_fail=1
    fi
  done
  if [[ -f "$LOGFILE" ]]; then
    sa_tail="$(tr '\r' '\n' <"$LOGFILE" | awk '/D6_SWITCH_A_CANDIDATE/{seen=1} seen{print}')"
    for fatal_pat in '!Fv' '!BNv' 'PAGE_FAULT' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$sa_tail" | rg -a -F -q -- "$fatal_pat"; then
        echo "[error] D6-SWITCH-A: fatal breadcrumb after switch start: $fatal_pat"
        switch_a_fail=1
      fi
    done
  fi
  if [[ "$switch_a_fail" -eq 1 ]]; then
    echo "[error] D6-SWITCH-A mode FAILED"
    exit 1
  fi
  echo "[ok] D6-SWITCH-A: real production unlocked switch observed"
fi

# Stage 167 (D6-GENUINE-A): when booted with yarm.d6_genuine=1, require evidence
# of at least one genuine scheduler-seam dispatch observation run outside the
# global lock, and reject any fatal breadcrumb after the seam wire begins.
if [[ "$D6_GENUINE" == "1" ]]; then
  genuine_fail=0
  echo "[ok] D6_GENUINE enabled marker:" $(log_has_pattern "D6_GENUINE_ENABLED" && echo present || echo MISSING)
  for g_marker in \
    "D6_LOCAL_DISPATCH_SEAM_CANDIDATE" \
    "D6_LOCAL_DISPATCH_SEAM_ENTER" \
    "D6_LOCAL_DISPATCH_SEAM_LOCK_SCOPE_DROPPED" \
    "D6_LOCAL_DISPATCH_STEP_SPLIT" \
    "D6_LOCAL_DISPATCH_SEAM_COUNT" \
    "D6_LOCAL_DISPATCH_SEAM_DONE"; do
    if log_has_pattern "$g_marker"; then
      echo "[ok] D6-GENUINE marker present: $g_marker"
    else
      echo "[error] D6-GENUINE marker missing: $g_marker"
      genuine_fail=1
    fi
  done
  if [[ -f "$LOGFILE" ]]; then
    g_tail="$(tr '\r' '\n' <"$LOGFILE" | awk '/D6_LOCAL_DISPATCH_SEAM_CANDIDATE/{seen=1} seen{print}')"
    for fatal_pat in '!Fv' '!BNv' 'PAGE_FAULT' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$g_tail" | rg -a -F -q -- "$fatal_pat"; then
        echo "[error] D6-GENUINE: fatal breadcrumb after seam wire start: $fatal_pat"
        genuine_fail=1
      fi
    done
  fi
  # Stage 168 (D6-GENUINE-B): require evidence that the AUTHORITATIVE mutating
  # dispatch ran through the scheduler seam AFTER the global lock was dropped.
  for m_marker in \
    "D6_GENUINE_MUT_DISPATCH_GLOBAL_DROPPED" \
    "D6_GENUINE_MUT_DISPATCH_ENTER" \
    "D6_GENUINE_MUT_DISPATCH_STEP_SPLIT" \
    "D6_GENUINE_MUT_DISPATCH_DONE" \
    "D6_GENUINE_MUT_DISPATCH_COUNT"; do
    if log_has_pattern "$m_marker"; then
      echo "[ok] D6-GENUINE-B mutating-dispatch marker present: $m_marker"
    else
      echo "[error] D6-GENUINE-B mutating-dispatch marker missing: $m_marker"
      genuine_fail=1
    fi
  done
  if [[ "$genuine_fail" -eq 1 ]]; then
    echo "[error] D6-GENUINE mode FAILED"
    exit 1
  fi
  echo "[ok] D6-GENUINE: authoritative mutating dispatch ran outside the global lock"
fi

# Stage 168 (D2-GENUINE-RECV): when booted with yarm.d2_recv_genuine=1, require
# evidence of the rank-clean recv phase markers and reject any fatal breadcrumb
# after the recv wire begins.
if [[ "$D2_RECV_GENUINE" == "1" ]]; then
  d2_recv_fail=0
  echo "[ok] D2_RECV_GENUINE enabled marker:" $(log_has_pattern "D2_RECV_GENUINE_ENABLED" && echo present || echo MISSING)
  # Core rank-clean phase markers that must appear for any blocking recv.
  for d2_marker in \
    "D2_RECV_GENUINE_CANDIDATE" \
    "D2_RECV_GENUINE_PHASE_CAP_OK" \
    "D2_RECV_GENUINE_PHASE_IPC_LOCK" \
    "D2_RECV_GENUINE_DONE"; do
    if log_has_pattern "$d2_marker"; then
      echo "[ok] D2-RECV-GENUINE marker present: $d2_marker"
    else
      echo "[error] D2-RECV-GENUINE marker missing: $d2_marker"
      d2_recv_fail=1
    fi
  done
  # At least one of the block/immediate outcome markers must appear.
  if log_has_pattern "D2_RECV_GENUINE_BLOCKED_OK" \
     || log_has_pattern "D2_RECV_GENUINE_IMMEDIATE_OK" \
     || log_has_pattern "D2_RECV_GENUINE_TIMEOUT_OK" \
     || log_has_pattern "D2_RECV_GENUINE_NOWAIT_OK"; then
    echo "[ok] D2-RECV-GENUINE outcome marker present (block/immediate/timeout/nowait)"
  else
    echo "[error] D2-RECV-GENUINE: no recv outcome marker observed"
    d2_recv_fail=1
  fi
  # Stage 168B: require evidence of at least one real BLOCKING recv whose
  # queue-advancing dispatch was deferred and run OUTSIDE the global lock.
  for d2b_marker in \
    "D2_RECV_GENUINE_PHASE_TASK_BLOCK" \
    "D2_RECV_GENUINE_PHASE_DISPATCH" \
    "D2_RECV_GENUINE_DISPATCH_DEFERRED" \
    "D2_RECV_GENUINE_GLOBAL_DROPPED" \
    "D2_RECV_GENUINE_DISPATCH_ENTER" \
    "D2_RECV_GENUINE_DISPATCH_STEP_SPLIT" \
    "D2_RECV_GENUINE_DISPATCH_DONE"; do
    if log_has_pattern "$d2b_marker"; then
      echo "[ok] D2-RECV-GENUINE blocking-dispatch marker present: $d2b_marker"
    else
      echo "[error] D2-RECV-GENUINE blocking-dispatch marker missing: $d2b_marker"
      d2_recv_fail=1
    fi
  done
  # HARD requirement (Stage 168B): every blocking recv PHASE_DISPATCH must be
  # followed by DISPATCH_DEFERRED (queue-advancing dispatch relocated OUT of the
  # global lock) — NOT a D6 switch_required in-lock fallback or a D2 in-lock
  # fallback. This is recv-path-specific: unrelated non-recv preemption may
  # legitimately still emit D6_GENUINE_MUT_DISPATCH_FALLBACK reason=switch_required.
  if [[ -f "$LOGFILE" ]]; then
    bad_recv_fallback="$(tr '\r' '\n' <"$LOGFILE" | awk '
      /D2_RECV_GENUINE_PHASE_DISPATCH/ { pending=1; next }
      pending && /D2_RECV_GENUINE_DISPATCH_DEFERRED/ { pending=0; next }
      pending && /D6_GENUINE_MUT_DISPATCH_FALLBACK reason=switch_required/ { print "BAD"; pending=0; next }
      pending && /D2_RECV_GENUINE_FALLBACK reason=/ { print "BAD"; pending=0; next }
    ')"
    if [[ -n "$bad_recv_fallback" ]]; then
      echo "[error] D2-RECV-GENUINE: blocking recv dispatch fell back in-lock (switch_required) instead of deferring out of lock (Stage 168B incomplete)"
      d2_recv_fail=1
    fi
  fi
  if [[ -f "$LOGFILE" ]]; then
    d2_tail="$(tr '\r' '\n' <"$LOGFILE" | awk '/D2_RECV_GENUINE_CANDIDATE/{seen=1} seen{print}')"
    for fatal_pat in '!Fv' '!BNv' 'PAGE_FAULT' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$d2_tail" | rg -a -F -q -- "$fatal_pat"; then
        echo "[error] D2-RECV-GENUINE: fatal breadcrumb after recv wire start: $fatal_pat"
        d2_recv_fail=1
      fi
    done
  fi
  if [[ "$d2_recv_fail" -eq 1 ]]; then
    echo "[error] D2-RECV-GENUINE mode FAILED"
    exit 1
  fi
  echo "[ok] D2-RECV-GENUINE: rank-clean blocking-recv phases observed"
fi

# Stage 169 (D2-GENUINE-SEND): when booted with yarm.d2_send_genuine=1, require
# the rank-clean blocking-send phase markers AND evidence that the send's
# queue-advancing dispatch ran OUTSIDE the global lock.
if [[ "$D2_SEND_GENUINE" == "1" ]]; then
  d2_send_fail=0
  echo "[ok] D2_SEND_GENUINE enabled marker:" $(log_has_pattern "D2_SEND_GENUINE_ENABLED" && echo present || echo MISSING)
  for d2s_marker in \
    "D2_SEND_GENUINE_CANDIDATE" \
    "D2_SEND_GENUINE_PHASE_CAP_OK" \
    "D2_SEND_GENUINE_PHASE_IPC_LOCK" \
    "D2_SEND_GENUINE_PHASE_TASK_BLOCK" \
    "D2_SEND_GENUINE_PHASE_DISPATCH" \
    "D2_SEND_GENUINE_DISPATCH_DEFERRED" \
    "D2_SEND_GENUINE_NO_INLOCK_DISPATCH" \
    "D2_SEND_GENUINE_GLOBAL_DROPPED" \
    "D2_SEND_GENUINE_DISPATCH_REVERIFY_OK" \
    "D2_SEND_GENUINE_DISPATCH_ENTER" \
    "D2_SEND_GENUINE_DISPATCH_STEP_SPLIT" \
    "D2_SEND_GENUINE_DISPATCH_DONE"; do
    if log_has_pattern "$d2s_marker"; then
      echo "[ok] D2-SEND-GENUINE marker present: $d2s_marker"
    else
      echo "[error] D2-SEND-GENUINE marker missing: $d2s_marker"
      d2_send_fail=1
    fi
  done
  # HARD (Stage 169): every blocking send PHASE_DISPATCH must be followed by
  # DISPATCH_DEFERRED (queue-advancing dispatch relocated OUT of the global lock)
  # — NOT a D6 switch_required in-lock fallback or a D2 send in-lock fallback.
  if [[ -f "$LOGFILE" ]]; then
    bad_send_fallback="$(tr '\r' '\n' <"$LOGFILE" | awk '
      /D2_SEND_GENUINE_PHASE_DISPATCH/ { pending=1; next }
      pending && /D2_SEND_GENUINE_DISPATCH_DEFERRED/ { pending=0; next }
      pending && /D6_GENUINE_MUT_DISPATCH_FALLBACK reason=switch_required/ { print "BAD"; pending=0; next }
      pending && /D2_SEND_GENUINE_FALLBACK reason=/ { print "BAD"; pending=0; next }
    ')"
    if [[ -n "$bad_send_fallback" ]]; then
      echo "[error] D2-SEND-GENUINE: blocking send dispatch fell back in-lock instead of deferring out of lock (Stage 169 incomplete)"
      d2_send_fail=1
    fi
  fi
  # Stage 163P sender-wake oracle must remain intact under D2_SEND_GENUINE=1
  # (the workload is auto-enabled above to exercise a blocking send).
  for sw_marker in \
    "IPC_RECV_PROOF_SENDER_WAKE_BLOCKED_OK" \
    "IPC_RECV_V2_SENDER_WAKE_ORDER_OK" \
    "IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE"; do
    if log_has_pattern "$sw_marker"; then
      echo "[ok] D2-SEND-GENUINE: Stage 163P marker preserved: $sw_marker"
    else
      echo "[error] D2-SEND-GENUINE: Stage 163P sender-wake marker missing: $sw_marker"
      d2_send_fail=1
    fi
  done
  if [[ -f "$LOGFILE" ]]; then
    d2s_tail="$(tr '\r' '\n' <"$LOGFILE" | awk '/D2_SEND_GENUINE_CANDIDATE/{seen=1} seen{print}')"
    # Stage 173B: narrow the fatal gate. The sender-wake workload forks and emits
    # handled COW fault groups (PAGE_FAULT_ENTRY … PAGE_FAULT_HANDLED_COW) whose
    # benign PAGE_FAULT_* diagnostics (ENTRY / HW_REGS / FRAME_WORDS / FRAME_DECODE
    # / HW_PTE_WALK / RAW / X86_ERROR / CR3_COMPARE) must NOT trip the fatal gate.
    # Only line-anchored crash breadcrumbs are fatal here; generic PAGE_FAULT is
    # NOT a fatal token — the explicit unhandled/fatal page-fault markers below are.
    for fatal_pat in '^!Fv' '^!BNv' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$d2s_tail" | rg -a -q -- "$fatal_pat"; then
        echo "[error] D2-SEND-GENUINE: fatal breadcrumb after send wire start: $fatal_pat"
        d2_send_fail=1
      fi
    done
    # Explicit unhandled/fatal page-fault markers only (handled COW/DEMAND are OK).
    for pf_fatal in 'PAGE_FAULT_UNHANDLED' 'PAGE_FAULT_FATAL' 'PAGE_FAULT_NOT_HANDLED'; do
      if printf '%s\n' "$d2s_tail" | rg -a -F -q -- "$pf_fatal"; then
        echo "[error] D2-SEND-GENUINE: explicit unhandled/fatal page-fault marker: $pf_fatal"
        d2_send_fail=1
      fi
    done
  fi
  if [[ "$d2_send_fail" -eq 1 ]]; then
    echo "[error] D2-SEND-GENUINE mode FAILED"
    exit 1
  fi
  echo "[ok] D2-SEND-GENUINE: rank-clean blocking-send dispatch ran outside the global lock"
fi

# Stage 171 (SCHED-TIMEOUT): when booted with yarm.sched_timeout=1, require the
# scheduler timeout/deadline diagnostics and reject stranded/duplicate/blocked
# timeout regressions. The idle-safety markers are deterministic (idle occurs
# during boot); the expiry markers are checked only when a timeout actually fires.
if [[ "$SCHED_TIMEOUT" == "1" ]]; then
  sched_to_fail=0
  echo "[ok] SCHED_TIMEOUT enabled marker:" $(log_has_pattern "SCHED_TIMEOUT_ENABLED" && echo present || echo MISSING)
  if ! log_has_pattern "SCHED_TIMEOUT_ENABLED"; then
    echo "[error] SCHED-TIMEOUT: SCHED_TIMEOUT_ENABLED missing (knob not applied)"
    sched_to_fail=1
  fi
  # Idle-with-pending-timeout safety (Task E): idle occurs during boot, so at
  # least one idle marker must appear; and every PENDING idle must be SAFE.
  if log_has_pattern "SCHED_IDLE_PENDING_TIMEOUT" \
     || log_has_pattern "SCHED_IDLE_NO_PENDING_TIMEOUT"; then
    echo "[ok] SCHED-TIMEOUT: idle-entry timeout diagnostics present"
  else
    echo "[error] SCHED-TIMEOUT: no idle-entry timeout marker observed"
    sched_to_fail=1
  fi
  if log_has_pattern "SCHED_IDLE_PENDING_TIMEOUT" && ! log_has_pattern "SCHED_IDLE_TIMEOUT_SAFE"; then
    echo "[error] SCHED-TIMEOUT: idle entered with pending timeout but not marked SAFE (timer progress at risk)"
    sched_to_fail=1
  fi
  # Never-stranded invariant (Task D): the defensive re-check must never fire.
  if log_has_pattern "SCHED_TIMEOUT_STRANDED_WAITER"; then
    echo "[error] SCHED-TIMEOUT: stranded timed-out waiter detected"
    sched_to_fail=1
  fi
  # If any timeout actually fired, require the full rank-clean phase sequence and
  # exactly-once wake (EXPIRED count == RUNQUEUE_ENQUEUE count within the batch
  # markers; each expired task clears its deadline so it cannot be woken twice).
  if [[ -f "$LOGFILE" ]] && log_has_pattern "SCHED_TIMEOUT_EXPIRED"; then
    echo "[info] SCHED-TIMEOUT: timeout expiry observed — checking full phase sequence"
    for m in \
      "SCHED_TIMEOUT_SCAN_BEGIN" \
      "SCHED_TIMEOUT_TASK_WAKE_BEGIN" \
      "SCHED_TIMEOUT_RUNQUEUE_ENQUEUE" \
      "SCHED_TIMEOUT_TASK_WAKE_DONE" \
      "SCHED_TIMEOUT_NO_STRANDED_WAITERS" \
      "SCHED_TIMEOUT_SCAN_DONE"; do
      if log_has_pattern "$m"; then
        echo "[ok] SCHED-TIMEOUT phase marker present: $m"
      else
        echo "[error] SCHED-TIMEOUT: expiry occurred but phase marker missing: $m"
        sched_to_fail=1
      fi
    done
    exp_n="$(tr '\r' '\n' <"$LOGFILE" | rg -c -a '^SCHED_TIMEOUT_EXPIRED ' || true)"
    enq_n="$(tr '\r' '\n' <"$LOGFILE" | rg -c -a '^SCHED_TIMEOUT_RUNQUEUE_ENQUEUE ' || true)"
    exp_n="${exp_n:-0}"; enq_n="${enq_n:-0}"
    if [[ "$exp_n" != "$enq_n" ]]; then
      echo "[error] SCHED-TIMEOUT: expired ($exp_n) != runqueue-enqueue ($enq_n) — wake without enqueue or duplicate wake"
      sched_to_fail=1
    else
      echo "[ok] SCHED-TIMEOUT: exactly-once wake (expired=$exp_n enqueue=$enq_n)"
    fi
  fi
  # Failure gates.
  for f in BLOCKED_WOULDBLOCK_FATAL CapabilityFull TaskTableFull; do
    if log_has_pattern "$f"; then
      echo "[error] SCHED-TIMEOUT: fatal marker present: $f"
      sched_to_fail=1
    fi
  done
  if [[ -f "$LOGFILE" ]]; then
    st_tail="$(tr '\r' '\n' <"$LOGFILE" | awk '/SCHED_TIMEOUT_ENABLED/{seen=1} seen{print}')"
    # Raw fatal breadcrumbs: `!Fv` / `!BNv` are line-start anchored (per the
    # accepted convention); the fault-escalation tokens are matched anywhere.
    for fatal_pat in '^!Fv' '^!BNv' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$st_tail" | rg -a -q -- "$fatal_pat"; then
        echo "[error] SCHED-TIMEOUT: fatal breadcrumb after sched-timeout wire start: $fatal_pat"
        sched_to_fail=1
      fi
    done
    # Stage 171B: page-fault gate — fail ONLY on the EXPLICIT unhandled/fatal
    # page-fault markers, never on benign PAGE_FAULT_* diagnostic lines. A HANDLED
    # fault emits many PAGE_FAULT_* diagnostics (ENTRY / HW_REGS / FRAME_WORDS /
    # FRAME_DECODE / HW_PTE_WALK / RAW / X86_ERROR / CR3_COMPARE) BEFORE the final
    # PAGE_FAULT_HANDLED_COW (or PAGE_FAULT_HANDLED_DEMAND); those are expected and
    # NOT fatal. The kernel emits `PAGE_FAULT_UNHANDLED tid=… addr=…` for a genuine
    # unhandled fault (fault_state.rs); PAGE_FAULT_FATAL / PAGE_FAULT_NOT_HANDLED
    # are accepted defensively in case future markers use those names.
    for pf_fatal in 'PAGE_FAULT_UNHANDLED' 'PAGE_FAULT_FATAL' 'PAGE_FAULT_NOT_HANDLED'; do
      if printf '%s\n' "$st_tail" | rg -a -F -q -- "$pf_fatal"; then
        echo "[error] SCHED-TIMEOUT: explicit unhandled/fatal page-fault marker: $pf_fatal"
        sched_to_fail=1
      fi
    done
  fi
  if [[ "$sched_to_fail" -eq 1 ]]; then
    echo "[error] SCHED-TIMEOUT mode FAILED"
    exit 1
  fi
  echo "[ok] SCHED-TIMEOUT: timeout/deadline hardening diagnostics clean"
fi

# Stage 172 (VM-COW): when booted with yarm.vm_cow=1, require the VM/COW phase
# diagnostics and reject VM/COW correctness regressions. VM_COW_ENABLED is
# deterministic; the COW/fork/map phase markers are checked when they occur.
if [[ "$VM_COW" == "1" ]]; then
  vm_cow_fail=0
  echo "[ok] VM_COW enabled marker:" $(log_has_pattern "VM_COW_ENABLED" && echo present || echo MISSING)
  if ! log_has_pattern "VM_COW_ENABLED"; then
    echo "[error] VM-COW: VM_COW_ENABLED missing (knob not applied)"
    vm_cow_fail=1
  fi
  # If a COW write fault occurred, its full phase sequence must complete.
  if log_has_pattern "VM_COW_FAULT_BEGIN"; then
    echo "[info] VM-COW: COW write fault observed — checking phase sequence"
    for m in "VM_COW_PHASE_METADATA" "VM_COW_PHASE_TLB_FLUSH" "VM_COW_DONE"; do
      if log_has_pattern "$m"; then
        echo "[ok] VM-COW phase marker present: $m"
      else
        echo "[error] VM-COW: COW fault occurred but phase marker missing: $m"
        vm_cow_fail=1
      fi
    done
  fi
  # If a fork COW clone occurred, it must reach DONE (or a clean ROLLBACK_OK).
  if log_has_pattern "VM_COW_FORK_BEGIN" \
     && ! log_has_pattern "VM_COW_FORK_DONE" \
     && ! log_has_pattern "VM_COW_FORK_ROLLBACK_OK"; then
    echo "[error] VM-COW: fork COW clone began but neither DONE nor ROLLBACK_OK observed"
    vm_cow_fail=1
  fi
  # TLB-shootdown prep markers must accompany a COW/unmap that changed a mapping.
  if log_has_pattern "VM_TLB_LOCAL_FLUSH" && ! log_has_pattern "VM_TLB_SHOOTDOWN_DEFERRED"; then
    echo "[error] VM-COW: local TLB flush without shootdown-deferred prep marker"
    vm_cow_fail=1
  fi
  # Hard failure markers (must never appear).
  for f in \
    "VM_COW_FAIL" \
    "VM_MAP_ROLLBACK_FAIL" \
    "VM_UNMAP_ROLLBACK_FAIL" \
    "VM_COW_REFCOUNT_UNDERFLOW" \
    "VM_COW_WRITABLE_SHARED_ALIAS" \
    "VM_COW_CHILD_ASID_LEAK" \
    "BLOCKED_WOULDBLOCK_FATAL" \
    "CapabilityFull" \
    "TaskTableFull"; do
    if log_has_pattern "$f"; then
      echo "[error] VM-COW: fatal marker present: $f"
      vm_cow_fail=1
    fi
  done
  if [[ -f "$LOGFILE" ]]; then
    vc_tail="$(tr '\r' '\n' <"$LOGFILE" | awk '/VM_COW_ENABLED/{seen=1} seen{print}')"
    for fatal_pat in '^!Fv' '^!BNv' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$vc_tail" | rg -a -q -- "$fatal_pat"; then
        echo "[error] VM-COW: fatal breadcrumb after vm-cow wire start: $fatal_pat"
        vm_cow_fail=1
      fi
    done
    # Explicit unhandled/fatal page-fault markers only (handled COW/DEMAND are OK).
    for pf_fatal in 'PAGE_FAULT_UNHANDLED' 'PAGE_FAULT_FATAL' 'PAGE_FAULT_NOT_HANDLED'; do
      if printf '%s\n' "$vc_tail" | rg -a -F -q -- "$pf_fatal"; then
        echo "[error] VM-COW: explicit unhandled/fatal page-fault marker: $pf_fatal"
        vm_cow_fail=1
      fi
    done
  fi
  if [[ "$vm_cow_fail" -eq 1 ]]; then
    echo "[error] VM-COW mode FAILED"
    exit 1
  fi
  echo "[ok] VM-COW: VM/COW/page-table/fork phase diagnostics clean"
fi

# Stage 173 (CAP-CNODE): when booted with yarm.cap_cnode=1, require the cap/CNode
# lifecycle diagnostics and reject cap/CNode correctness regressions.
if [[ "$CAP_CNODE" == "1" ]]; then
  cap_cnode_fail=0
  echo "[ok] CAP_CNODE enabled marker:" $(log_has_pattern "CAP_CNODE_ENABLED" && echo present || echo MISSING)
  if ! log_has_pattern "CAP_CNODE_ENABLED"; then
    echo "[error] CAP-CNODE: CAP_CNODE_ENABLED missing (knob not applied)"
    cap_cnode_fail=1
  fi
  # Deterministic one-shot proof markers (must appear).
  for m in "CAP_CNODE_LOOKUP_OK" "CAP_CNODE_RESERVE_OK"; do
    if log_has_pattern "$m"; then
      echo "[ok] CAP-CNODE marker present: $m"
    else
      echo "[error] CAP-CNODE: required marker missing: $m"
      cap_cnode_fail=1
    fi
  done
  # At least one materialize (proof mint or a transferred cap).
  if log_has_pattern "CAP_CNODE_MATERIALIZE_OK" || log_has_pattern "CAP_CNODE_TRANSFER_MATERIALIZE_OK"; then
    echo "[ok] CAP-CNODE materialize marker present"
  else
    echo "[error] CAP-CNODE: no materialize marker observed"
    cap_cnode_fail=1
  fi
  # At least one release (proof revoke or on-exit revoke).
  if log_has_pattern "CAP_CNODE_RELEASE_OK" || log_has_pattern "CAP_CNODE_REVOKE_ON_EXIT_OK"; then
    echo "[ok] CAP-CNODE release marker present"
  else
    echo "[error] CAP-CNODE: no release marker observed"
    cap_cnode_fail=1
  fi
  # If the proof emitted an invariant result, it must be OK.
  if log_has_pattern "CAP_CNODE_INVARIANT_OK"; then
    echo "[ok] CAP-CNODE invariant OK"
  fi
  # Hard invariant-violation markers (must never appear).
  for f in \
    "CAP_CNODE_REFCOUNT_UNDERFLOW" \
    "CAP_CNODE_SLOT_LEAK" \
    "CAP_CNODE_STALE_CAP_ACCEPTED" \
    "CAP_CNODE_RIGHTS_ESCALATION" \
    "CAP_CNODE_ROLLBACK_LEAK" \
    "CAP_CNODE_MATERIALIZE_FAIL" \
    "CapabilityFull" \
    "TaskTableFull" \
    "BLOCKED_WOULDBLOCK_FATAL"; do
    if log_has_pattern "$f"; then
      echo "[error] CAP-CNODE: fatal marker present: $f"
      cap_cnode_fail=1
    fi
  done
  # A committed transfer that FAILs is only acceptable if it rolled back cleanly.
  if log_has_pattern "CAP_CNODE_TRANSFER_FAIL" && ! log_has_pattern "CAP_CNODE_TRANSFER_ROLLBACK_OK"; then
    echo "[error] CAP-CNODE: transfer failed without a clean rollback"
    cap_cnode_fail=1
  fi
  if [[ -f "$LOGFILE" ]]; then
    cc_tail="$(tr '\r' '\n' <"$LOGFILE" | awk '/CAP_CNODE_ENABLED/{seen=1} seen{print}')"
    for fatal_pat in '^!Fv' '^!BNv' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$cc_tail" | rg -a -q -- "$fatal_pat"; then
        echo "[error] CAP-CNODE: fatal breadcrumb after cap-cnode wire start: $fatal_pat"
        cap_cnode_fail=1
      fi
    done
    # Explicit unhandled/fatal page-fault markers only (handled COW/DEMAND are OK).
    for pf_fatal in 'PAGE_FAULT_UNHANDLED' 'PAGE_FAULT_FATAL' 'PAGE_FAULT_NOT_HANDLED'; do
      if printf '%s\n' "$cc_tail" | rg -a -F -q -- "$pf_fatal"; then
        echo "[error] CAP-CNODE: explicit unhandled/fatal page-fault marker: $pf_fatal"
        cap_cnode_fail=1
      fi
    done
  fi
  if [[ "$cap_cnode_fail" -eq 1 ]]; then
    echo "[error] CAP-CNODE mode FAILED"
    exit 1
  fi
  echo "[ok] CAP-CNODE: capability/CNode lifecycle diagnostics clean"
fi

# Stage 174 (FAULT-DELIVERY): when booted with yarm.fault_delivery=1, require the
# kernel-fault → supervisor delivery / fault-channel lifecycle diagnostics and
# reject delivery/queue/channel regressions. The one-shot self-contained proof
# provides the deterministic classify/msg-build/endpoint/queue/dequeue/invariant
# markers; the live classify markers additionally fire on the boot's handled COW
# faults. Handled COW/DEMAND page faults remain accepted (Stage 171B/173B).
if [[ "$FAULT_DELIVERY" == "1" ]]; then
  fault_delivery_fail=0
  echo "[ok] FAULT_DELIVERY enabled marker:" $(log_has_pattern "FAULT_DELIVERY_ENABLED" && echo present || echo MISSING)
  if ! log_has_pattern "FAULT_DELIVERY_ENABLED"; then
    echo "[error] FAULT-DELIVERY: FAULT_DELIVERY_ENABLED missing (knob not applied)"
    fault_delivery_fail=1
  fi
  # Deterministic proof / live-path required markers (must appear).
  for m in \
    "FAULT_DELIVERY_CLASSIFY_USER_UNHANDLED" \
    "FAULT_DELIVERY_MSG_BUILD_OK" \
    "FAULT_DELIVERY_INVARIANT_OK"; do
    if log_has_pattern "$m"; then
      echo "[ok] FAULT-DELIVERY marker present: $m"
    else
      echo "[error] FAULT-DELIVERY: required marker missing: $m"
      fault_delivery_fail=1
    fi
  done
  # At least one delivery completion — direct blocked-recv OR queued dequeue.
  if log_has_pattern "FAULT_DELIVERY_DIRECT_RECV_DONE" || log_has_pattern "FAULT_DELIVERY_DEQUEUE_OK"; then
    echo "[ok] FAULT-DELIVERY delivery-completion marker present"
  else
    echo "[error] FAULT-DELIVERY: no direct-recv/dequeue completion observed"
    fault_delivery_fail=1
  fi
  # If the current policy stopped a faulting task, the stop must have completed.
  if log_has_pattern "FAULT_DELIVERY_TASK_STOP_BEGIN" && ! log_has_pattern "FAULT_DELIVERY_TASK_STOP_OK"; then
    echo "[error] FAULT-DELIVERY: task-stop began but did not complete"
    fault_delivery_fail=1
  fi
  # Hard invariant-violation markers (must never appear).
  for f in \
    "FAULT_DELIVERY_STRANDED_QUEUE" \
    "FAULT_DELIVERY_DUPLICATE_MSG" \
    "FAULT_DELIVERY_ORPHANED_WAITER" \
    "FAULT_DELIVERY_STALE_SUPERVISOR" \
    "FAULT_DELIVERY_BAD_SENDER" \
    "FAULT_DELIVERY_WRITEBACK_FAIL" \
    "FAULT_DELIVERY_QUEUE_LEAK" \
    "CapabilityFull" \
    "TaskTableFull" \
    "BLOCKED_WOULDBLOCK_FATAL"; do
    if log_has_pattern "$f"; then
      echo "[error] FAULT-DELIVERY: fatal marker present: $f"
      fault_delivery_fail=1
    fi
  done
  if [[ -f "$LOGFILE" ]]; then
    fd_tail="$(tr '\r' '\n' <"$LOGFILE" | awk '/FAULT_DELIVERY_ENABLED/{seen=1} seen{print}')"
    for fatal_pat in '^!Fv' '^!BNv' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$fd_tail" | rg -a -q -- "$fatal_pat"; then
        echo "[error] FAULT-DELIVERY: fatal breadcrumb after fault-delivery wire start: $fatal_pat"
        fault_delivery_fail=1
      fi
    done
    # Explicit unhandled/fatal page-fault markers only. A generic PAGE_FAULT is
    # NOT fatal here (handled COW/DEMAND emit many benign PAGE_FAULT_* diagnostics);
    # an unhandled fault that escapes WITHOUT a fault-delivery success is fatal, but
    # PAGE_FAULT_UNHANDLED routed into a FAULT_DELIVERY_* success is the expected
    # user-fault delivery path.
    for pf_fatal in 'PAGE_FAULT_FATAL' 'PAGE_FAULT_NOT_HANDLED'; do
      if printf '%s\n' "$fd_tail" | rg -a -F -q -- "$pf_fatal"; then
        echo "[error] FAULT-DELIVERY: explicit fatal page-fault marker: $pf_fatal"
        fault_delivery_fail=1
      fi
    done
    # PAGE_FAULT_UNHANDLED is only fatal if it did NOT route to a fault-delivery
    # classification/build (i.e. it escaped the supervisor delivery path).
    if printf '%s\n' "$fd_tail" | rg -a -F -q -- 'PAGE_FAULT_UNHANDLED' \
       && ! log_has_pattern "FAULT_DELIVERY_CLASSIFY_USER_UNHANDLED"; then
      echo "[error] FAULT-DELIVERY: unhandled page fault escaped without supervisor delivery"
      fault_delivery_fail=1
    fi
  fi
  if [[ "$fault_delivery_fail" -eq 1 ]]; then
    echo "[error] FAULT-DELIVERY mode FAILED"
    exit 1
  fi
  echo "[ok] FAULT-DELIVERY: kernel-fault → supervisor delivery diagnostics clean"
fi

# Stage 175 (SPAWN-LIFECYCLE): when booted with yarm.spawn_lifecycle=1, require the
# spawn / image-loading / lifecycle-metadata diagnostics and reject spawn/rollback
# regressions. The phase markers fire naturally from the boot's service spawns; the
# one-shot proof provides the deterministic rollback/invariant markers. Handled
# COW/DEMAND page faults remain accepted (Stage 171B/173B).
if [[ "$SPAWN_LIFECYCLE" == "1" ]]; then
  spawn_lc_fail=0
  echo "[ok] SPAWN_LIFECYCLE enabled marker:" $(log_has_pattern "SPAWN_LIFECYCLE_ENABLED" && echo present || echo MISSING)
  if ! log_has_pattern "SPAWN_LIFECYCLE_ENABLED"; then
    echo "[error] SPAWN-LIFECYCLE: SPAWN_LIFECYCLE_ENABLED missing (knob not applied)"
    spawn_lc_fail=1
  fi
  # At least one successful spawn path must have run to completion.
  if log_has_pattern "SPAWN_LIFECYCLE_PROCESS_READY" || log_has_pattern "SPAWN_LIFECYCLE_SERVICE_READY"; then
    echo "[ok] SPAWN-LIFECYCLE: successful spawn path observed"
  else
    echo "[error] SPAWN-LIFECYCLE: no successful spawn path observed"
    spawn_lc_fail=1
  fi
  # The deterministic one-shot rollback proof invariant (must appear).
  if log_has_pattern "SPAWN_LIFECYCLE_INVARIANT_OK"; then
    echo "[ok] SPAWN-LIFECYCLE invariant OK"
  else
    echo "[error] SPAWN-LIFECYCLE: SPAWN_LIFECYCLE_INVARIANT_OK missing"
    spawn_lc_fail=1
  fi
  # If a rollback began it must have completed cleanly.
  if log_has_pattern "SPAWN_LIFECYCLE_ROLLBACK_BEGIN" && ! log_has_pattern "SPAWN_LIFECYCLE_ROLLBACK_OK"; then
    echo "[error] SPAWN-LIFECYCLE: rollback began but did not complete"
    spawn_lc_fail=1
  fi
  # Service baseline must still be reached (services came up).
  if ! log_has_pattern "YARM_SERVICE_BASELINE" && ! log_has_pattern "SERVICE_BASELINE_READY" && ! log_has_pattern "YARM_BOOT_OK"; then
    echo "[warn] SPAWN-LIFECYCLE: service baseline marker not observed"
  fi
  # Hard invariant-violation markers (must never appear).
  for f in \
    "SPAWN_LIFECYCLE_ROLLBACK_LEAK" \
    "SPAWN_LIFECYCLE_ZOMBIE_LEAK" \
    "SPAWN_LIFECYCLE_CAP_LEAK" \
    "SPAWN_LIFECYCLE_ASPACE_LEAK" \
    "SPAWN_LIFECYCLE_TCB_LEAK" \
    "SPAWN_LIFECYCLE_DUPLICATE_TID" \
    "SPAWN_LIFECYCLE_BAD_IMAGE_ID" \
    "SPAWN_LIFECYCLE_SERVICE_ORDER_VIOLATION" \
    "CapabilityFull" \
    "TaskTableFull" \
    "BLOCKED_WOULDBLOCK_FATAL"; do
    if log_has_pattern "$f"; then
      echo "[error] SPAWN-LIFECYCLE: fatal marker present: $f"
      spawn_lc_fail=1
    fi
  done
  if [[ -f "$LOGFILE" ]]; then
    sl_tail="$(tr '\r' '\n' <"$LOGFILE" | awk '/SPAWN_LIFECYCLE_ENABLED/{seen=1} seen{print}')"
    for fatal_pat in '^!Fv' '^!BNv' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$sl_tail" | rg -a -q -- "$fatal_pat"; then
        echo "[error] SPAWN-LIFECYCLE: fatal breadcrumb after spawn-lifecycle wire start: $fatal_pat"
        spawn_lc_fail=1
      fi
    done
    # Explicit unhandled/fatal page-fault markers only (handled COW/DEMAND are OK).
    for pf_fatal in 'PAGE_FAULT_UNHANDLED' 'PAGE_FAULT_FATAL' 'PAGE_FAULT_NOT_HANDLED'; do
      if printf '%s\n' "$sl_tail" | rg -a -F -q -- "$pf_fatal"; then
        echo "[error] SPAWN-LIFECYCLE: explicit unhandled/fatal page-fault marker: $pf_fatal"
        spawn_lc_fail=1
      fi
    done
  fi
  if [[ "$spawn_lc_fail" -eq 1 ]]; then
    echo "[error] SPAWN-LIFECYCLE mode FAILED"
    exit 1
  fi
  echo "[ok] SPAWN-LIFECYCLE: spawn / image-loading / lifecycle-metadata diagnostics clean"
fi

# Stage 176 (GLOBAL-STATE): when booted with yarm.global_state=1, require the
# remaining direct global-KernelState mutation audit + lock-rank discipline
# diagnostics and reject rank inversions / leaked global guards / unclassified
# mutation sites. The one-shot read-only audit provides the deterministic markers.
# Handled COW/DEMAND page faults remain accepted (Stage 171B/173B).
if [[ "$GLOBAL_STATE" == "1" ]]; then
  global_state_fail=0
  echo "[ok] GLOBAL_STATE enabled marker:" $(log_has_pattern "GLOBAL_STATE_ENABLED" && echo present || echo MISSING)
  if ! log_has_pattern "GLOBAL_STATE_ENABLED"; then
    echo "[error] GLOBAL-STATE: GLOBAL_STATE_ENABLED missing (knob not applied)"
    global_state_fail=1
  fi
  # Deterministic audit required markers (must appear).
  for m in \
    "GLOBAL_STATE_OWNER_HELPER_OK" \
    "GLOBAL_STATE_DIRECT_SITE_ALLOWED" \
    "GLOBAL_STATE_RANK_ORDER_OK" \
    "GLOBAL_STATE_NO_LEAKED_GLOBAL_GUARD" \
    "GLOBAL_STATE_INVARIANT_OK" \
    "GLOBAL_STATE_PROOF_DONE"; do
    if log_has_pattern "$m"; then
      echo "[ok] GLOBAL-STATE marker present: $m"
    else
      echo "[error] GLOBAL-STATE: required marker missing: $m"
      global_state_fail=1
    fi
  done
  # Hard invariant-violation markers (must never appear).
  for f in \
    "GLOBAL_STATE_DIRECT_MUTATION_LEAK" \
    "GLOBAL_STATE_RANK_INVERSION" \
    "GLOBAL_STATE_RANK_ORDER_FAIL" \
    "GLOBAL_STATE_GUARD_HELD_ACROSS_USER_COPY" \
    "GLOBAL_STATE_GUARD_HELD_ACROSS_SWITCH" \
    "GLOBAL_STATE_GUARD_HELD_ACROSS_IPC_WRITEBACK" \
    "GLOBAL_STATE_OWNER_HELPER_BYPASS" \
    "GLOBAL_STATE_UNCLASSIFIED_SITE" \
    "GLOBAL_STATE_DIRECT_SITE_REJECTED" \
    "GLOBAL_STATE_INVARIANT_FAIL" \
    "CapabilityFull" \
    "TaskTableFull" \
    "BLOCKED_WOULDBLOCK_FATAL"; do
    if log_has_pattern "$f"; then
      echo "[error] GLOBAL-STATE: fatal marker present: $f"
      global_state_fail=1
    fi
  done
  if [[ -f "$LOGFILE" ]]; then
    gs_tail="$(tr '\r' '\n' <"$LOGFILE" | awk '/GLOBAL_STATE_ENABLED/{seen=1} seen{print}')"
    for fatal_pat in '^!Fv' '^!BNv' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$gs_tail" | rg -a -q -- "$fatal_pat"; then
        echo "[error] GLOBAL-STATE: fatal breadcrumb after global-state wire start: $fatal_pat"
        global_state_fail=1
      fi
    done
    # Explicit unhandled/fatal page-fault markers only (handled COW/DEMAND are OK).
    for pf_fatal in 'PAGE_FAULT_UNHANDLED' 'PAGE_FAULT_FATAL' 'PAGE_FAULT_NOT_HANDLED'; do
      if printf '%s\n' "$gs_tail" | rg -a -F -q -- "$pf_fatal"; then
        echo "[error] GLOBAL-STATE: explicit unhandled/fatal page-fault marker: $pf_fatal"
        global_state_fail=1
      fi
    done
  fi
  if [[ "$global_state_fail" -eq 1 ]]; then
    echo "[error] GLOBAL-STATE mode FAILED"
    exit 1
  fi
  echo "[ok] GLOBAL-STATE: global-KernelState mutation audit + lock-rank diagnostics clean"
fi

# Stage 177 (SMP-READY): when booted with yarm.smp_ready=1, require the x86_64
# SMP-readiness audit diagnostics. Acceptance is honest per Option A/B: either an AP
# reaches online/idle OR an explicit AP fallback reason is recorded; the per-CPU /
# scheduler / rank invariants + PROOF_DONE must be clean. Remote-wake/IPI are
# DEFERRED (APs stay parked, BSP-only) — not a failure. Handled COW/DEMAND accepted.
if [[ "$SMP_READY" == "1" ]]; then
  smp_ready_fail=0
  echo "[ok] SMP_READY enabled marker:" $(log_has_pattern "SMP_READY_ENABLED" && echo present || echo MISSING)
  if ! log_has_pattern "SMP_READY_ENABLED"; then
    echo "[error] SMP-READY: SMP_READY_ENABLED missing (knob not applied)"
    smp_ready_fail=1
  fi
  # Boot CPU + invariant + proof required.
  for m in \
    "SMP_READY_BOOT_CPU_OK" \
    "SMP_READY_RANK_ORDER_OK" \
    "SMP_READY_GLOBAL_STATE_OK" \
    "SMP_READY_INVARIANT_OK" \
    "SMP_READY_PROOF_DONE"; do
    if log_has_pattern "$m"; then
      echo "[ok] SMP-READY marker present: $m"
    else
      echo "[error] SMP-READY: required marker missing: $m"
      smp_ready_fail=1
    fi
  done
  # Either an AP came online/idle OR an explicit AP fallback reason was recorded.
  if log_has_pattern "SMP_READY_AP_ONLINE" || log_has_pattern "SMP_READY_AP_IDLE_OK" || log_has_pattern "SMP_READY_AP_FALLBACK"; then
    echo "[ok] SMP-READY: AP online/idle or explicit fallback recorded"
  else
    echo "[error] SMP-READY: no AP online/idle and no AP fallback reason recorded"
    smp_ready_fail=1
  fi
  # Hard invariant-violation markers (must never appear).
  for f in \
    "SMP_READY_AP_BOOT_FAIL" \
    "SMP_READY_AP_STACK_ALIAS" \
    "SMP_READY_AP_TSS_BAD" \
    "SMP_READY_PERCPU_CLOBBER" \
    "SMP_READY_CURRENT_TID_MISMATCH" \
    "SMP_READY_ASID_MISMATCH" \
    "SMP_READY_REMOTE_WAKE_LOST" \
    "SMP_READY_IPI_LOST" \
    "SMP_READY_RUNQUEUE_CORRUPT" \
    "SMP_READY_GLOBAL_GUARD_LEAK" \
    "SMP_READY_RANK_INVERSION" \
    "SMP_READY_INVARIANT_FAIL" \
    "CapabilityFull" \
    "TaskTableFull" \
    "BLOCKED_WOULDBLOCK_FATAL"; do
    if log_has_pattern "$f"; then
      echo "[error] SMP-READY: fatal marker present: $f"
      smp_ready_fail=1
    fi
  done
  if [[ -f "$LOGFILE" ]]; then
    sr_tail="$(tr '\r' '\n' <"$LOGFILE" | awk '/SMP_READY_ENABLED/{seen=1} seen{print}')"
    for fatal_pat in '^!Fv' '^!BNv' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$sr_tail" | rg -a -q -- "$fatal_pat"; then
        echo "[error] SMP-READY: fatal breadcrumb after smp-ready wire start: $fatal_pat"
        smp_ready_fail=1
      fi
    done
    for pf_fatal in 'PAGE_FAULT_UNHANDLED' 'PAGE_FAULT_FATAL' 'PAGE_FAULT_NOT_HANDLED'; do
      if printf '%s\n' "$sr_tail" | rg -a -F -q -- "$pf_fatal"; then
        echo "[error] SMP-READY: explicit unhandled/fatal page-fault marker: $pf_fatal"
        smp_ready_fail=1
      fi
    done
  fi
  if [[ "$smp_ready_fail" -eq 1 ]]; then
    echo "[error] SMP-READY mode FAILED"
    exit 1
  fi
  echo "[ok] SMP-READY: x86_64 SMP-readiness audit diagnostics clean (APs parked / BSP-only)"
fi

# Stage 178 (CROSS-ARCH-D6): when booted with yarm.cross_arch_d6=1, require the
# per-arch D6 restore-path audit diagnostics. Acceptance is honest: either a live
# RESTORE_DONE OR an explicit FALLBACK/DEFERRED reason, plus INVARIANT_OK + PROOF_DONE.
# On x86_64 the audit records model=switch_frames and the accepted-D6 observe-only
# fallback. Handled COW/DEMAND page faults remain accepted.
if [[ "$CROSS_ARCH_D6" == "1" ]]; then
  cross_arch_d6_fail=0
  echo "[ok] CROSS_ARCH_D6 enabled marker:" $(log_has_pattern "CROSS_ARCH_D6_ENABLED" && echo present || echo MISSING)
  if ! log_has_pattern "CROSS_ARCH_D6_ENABLED"; then
    echo "[error] CROSS-ARCH-D6: CROSS_ARCH_D6_ENABLED missing (knob not applied)"
    cross_arch_d6_fail=1
  fi
  for m in "CROSS_ARCH_D6_INVARIANT_OK" "CROSS_ARCH_D6_PROOF_DONE"; do
    if log_has_pattern "$m"; then
      echo "[ok] CROSS-ARCH-D6 marker present: $m"
    else
      echo "[error] CROSS-ARCH-D6: required marker missing: $m"
      cross_arch_d6_fail=1
    fi
  done
  # Either a live restore completed OR an explicit fallback/deferred reason recorded.
  if log_has_pattern "CROSS_ARCH_D6_RESTORE_DONE" || log_has_pattern "CROSS_ARCH_D6_FALLBACK"; then
    echo "[ok] CROSS-ARCH-D6: live restore-done or explicit fallback/deferred recorded"
  else
    echo "[error] CROSS-ARCH-D6: neither RESTORE_DONE nor an explicit fallback reason recorded"
    cross_arch_d6_fail=1
  fi
  # Hard invariant-violation markers (must never appear).
  for f in \
    "CROSS_ARCH_D6_GLOBAL_GUARD_HELD" \
    "CROSS_ARCH_D6_BAD_TRAPFRAME" \
    "CROSS_ARCH_D6_BAD_ASID" \
    "CROSS_ARCH_D6_CURRENT_TID_MISMATCH" \
    "CROSS_ARCH_D6_DOUBLE_DISPATCH" \
    "CROSS_ARCH_D6_RESTORE_FAIL" \
    "CROSS_ARCH_D6_UNSUPPORTED_MODEL" \
    "CROSS_ARCH_D6_INVARIANT_FAIL" \
    "CapabilityFull" \
    "TaskTableFull" \
    "BLOCKED_WOULDBLOCK_FATAL"; do
    if log_has_pattern "$f"; then
      echo "[error] CROSS-ARCH-D6: fatal marker present: $f"
      cross_arch_d6_fail=1
    fi
  done
  if [[ -f "$LOGFILE" ]]; then
    cad_tail="$(tr '\r' '\n' <"$LOGFILE" | awk '/CROSS_ARCH_D6_ENABLED/{seen=1} seen{print}')"
    for fatal_pat in '^!Fv' '^!BNv' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$cad_tail" | rg -a -q -- "$fatal_pat"; then
        echo "[error] CROSS-ARCH-D6: fatal breadcrumb after cross-arch-d6 wire start: $fatal_pat"
        cross_arch_d6_fail=1
      fi
    done
    for pf_fatal in 'PAGE_FAULT_UNHANDLED' 'PAGE_FAULT_FATAL' 'PAGE_FAULT_NOT_HANDLED'; do
      if printf '%s\n' "$cad_tail" | rg -a -F -q -- "$pf_fatal"; then
        echo "[error] CROSS-ARCH-D6: explicit unhandled/fatal page-fault marker: $pf_fatal"
        cross_arch_d6_fail=1
      fi
    done
  fi
  if [[ "$cross_arch_d6_fail" -eq 1 ]]; then
    echo "[error] CROSS-ARCH-D6 mode FAILED"
    exit 1
  fi
  echo "[ok] CROSS-ARCH-D6: x86_64 D6 restore-path audit diagnostics clean (accepted-D6 observe-only)"
fi

# Stage 179 (D3-FULL): when booted with yarm.d3_full=1, require the D3 VM
# anon-map/unmap two-phase diagnostics + the self-contained proof. Local TLB flush is
# live; remote shootdown is prepped/deferred (no fake SMP shootdown). Handled
# COW/DEMAND page faults remain accepted.
if [[ "$D3_FULL" == "1" ]]; then
  d3_full_fail=0
  echo "[ok] D3_FULL enabled marker:" $(log_has_pattern "D3_FULL_ENABLED" && echo present || echo MISSING)
  if ! log_has_pattern "D3_FULL_ENABLED"; then
    echo "[error] D3-FULL: D3_FULL_ENABLED missing (knob not applied)"
    d3_full_fail=1
  fi
  # Required successful two-phase map + unmap + flush + invariant + proof markers.
  for m in \
    "D3_VM_ANON_VALIDATE_OK" \
    "D3_VM_ANON_PT_UPDATE_OK" \
    "D3_VM_ANON_COMMIT_OK" \
    "D3_VM_ANON_DONE" \
    "D3_VM_UNMAP_PT_REMOVE_OK" \
    "D3_VM_UNMAP_DONE" \
    "D3_TLB_LOCAL_FLUSH_OK" \
    "D3_VM_INVARIANT_OK"; do
    if log_has_pattern "$m"; then
      echo "[ok] D3-FULL marker present: $m"
    else
      echo "[error] D3-FULL: required marker missing: $m"
      d3_full_fail=1
    fi
  done
  # Proof must complete OK.
  if log_has_pattern "D3_VM_PROOF_DONE result=ok"; then
    echo "[ok] D3-FULL: proof done result=ok"
  else
    echo "[error] D3-FULL: D3_VM_PROOF_DONE result=ok missing"
    d3_full_fail=1
  fi
  # Remote shootdown must be explicitly prepped or deferred (never a fake claim).
  if log_has_pattern "D3_TLB_SHOOTDOWN_PREP_OK" || log_has_pattern "D3_TLB_SHOOTDOWN_DEFERRED"; then
    echo "[ok] D3-FULL: remote shootdown prepped/deferred"
  else
    echo "[error] D3-FULL: no shootdown prep/deferred marker"
    d3_full_fail=1
  fi
  # Hard invariant-violation markers (must never appear).
  for f in \
    "D3_VM_FRAME_LEAK" \
    "D3_VM_CAP_LEAK" \
    "D3_VM_METADATA_LEAK" \
    "D3_VM_STALE_PTE" \
    "D3_VM_COW_UNDERFLOW" \
    "D3_VM_WRITABLE_SHARED_ALIAS" \
    "D3_VM_RANK_INVERSION" \
    "D3_VM_ROLLBACK_FAIL" \
    "D3_TLB_LOCAL_FLUSH_FAIL" \
    "D3_TLB_SHOOTDOWN_UNSAFE_WAIT" \
    "D3_VM_INVARIANT_FAIL" \
    "CapabilityFull" \
    "TaskTableFull" \
    "BLOCKED_WOULDBLOCK_FATAL"; do
    if log_has_pattern "$f"; then
      echo "[error] D3-FULL: fatal marker present: $f"
      d3_full_fail=1
    fi
  done
  if [[ -f "$LOGFILE" ]]; then
    d3_tail="$(tr '\r' '\n' <"$LOGFILE" | awk '/D3_FULL_ENABLED/{seen=1} seen{print}')"
    for fatal_pat in '^!Fv' '^!BNv' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$d3_tail" | rg -a -q -- "$fatal_pat"; then
        echo "[error] D3-FULL: fatal breadcrumb after d3-full wire start: $fatal_pat"
        d3_full_fail=1
      fi
    done
    for pf_fatal in 'PAGE_FAULT_UNHANDLED' 'PAGE_FAULT_FATAL' 'PAGE_FAULT_NOT_HANDLED'; do
      if printf '%s\n' "$d3_tail" | rg -a -F -q -- "$pf_fatal"; then
        echo "[error] D3-FULL: explicit unhandled/fatal page-fault marker: $pf_fatal"
        d3_full_fail=1
      fi
    done
  fi
  if [[ "$d3_full_fail" -eq 1 ]]; then
    echo "[error] D3-FULL mode FAILED"
    exit 1
  fi
  echo "[ok] D3-FULL: VM anon map/unmap two-phase diagnostics clean (local flush live, remote deferred)"
fi

# Stage 181 (GRADUATE-KNOBS): the accepted x86_64 -smp1 unlock seams graduate to
# default-on. Three cases:
#   UNLOCK_GRADUATED=0  -> emergency opt-out: expect DEFERRED reason=emergency_optout,
#                          NO graduated ENABLED marker; just prove the conservative
#                          fallback still boots.
#   UNLOCK_GRADUATED=1  -> explicit graduated profile: STRICT gate on the graduated
#                          marker set + no unexpected fallback.
#   (empty / default)   -> normal boot: graduated by default; SOFT-observe the markers
#                          (do not fail the plain smoke on proof-timing), but any
#                          UNLOCK_GRADUATED_* failure marker is still fatal.
unlock_graduated_fatal_scan() {
  local label="$1"
  local rc=0
  for f in \
    "UNLOCK_GRADUATED_UNEXPECTED_INLOCK_DISPATCH" \
    "UNLOCK_GRADUATED_DOUBLE_DISPATCH" \
    "UNLOCK_GRADUATED_RESTORE_FAIL" \
    "UNLOCK_GRADUATED_D3_ROLLBACK_FAIL" \
    "UNLOCK_GRADUATED_D3_LEAK" \
    "UNLOCK_GRADUATED_INVARIANT_FAIL"; do
    if log_has_pattern "$f"; then
      echo "[error] $label: fatal graduated marker present: $f"
      rc=1
    fi
  done
  # An unexpected fallback on the committed normal path fails the graduated profile.
  if log_has_pattern "UNLOCK_GRADUATED_FALLBACK path="; then
    echo "[error] $label: unexpected UNLOCK_GRADUATED_FALLBACK on the committed path"
    rc=1
  fi
  return $rc
}

# Stage 182 (REMOVE-FALLBACKS): the old opt-out (emergency_optout) is GONE. The old
# emergency opt-out and any UNLOCK_GRADUATED_FALLBACK are now impossible — assert their
# ABSENCE unconditionally as a negative test that the fallback was removed, not disabled.
if log_has_pattern "UNLOCK_GRADUATED_DEFERRED reason=emergency_optout"; then
  echo "[error] REMOVE-FALLBACKS: obsolete emergency opt-out fallback fired (must be removed)"
  exit 1
fi
if log_has_pattern "UNLOCK_GRADUATED_UNEXPECTED_INLOCK_DISPATCH"; then
  echo "[error] REMOVE-FALLBACKS: unexpected in-lock fallback dispatch on the graduated path"
  exit 1
fi
if ! unlock_graduated_fatal_scan "REMOVE-FALLBACKS"; then
  echo "[error] REMOVE-FALLBACKS: fatal graduated marker present"
  exit 1
fi

if [[ -n "$UNLOCK_GRADUATED" ]]; then
  # An explicit (now-OBSOLETE) yarm.unlock_graduated=$UNLOCK_GRADUATED was passed. It must
  # be reported obsolete + ignored, and must NOT re-enable any fallback: graduation still
  # runs regardless of the value (including the old =0 opt-out).
  echo "[info] REMOVE-FALLBACKS: obsolete UNLOCK_GRADUATED=$UNLOCK_GRADUATED passed (must be ignored)"
  if log_has_pattern "UNLOCK_FALLBACK_KNOB_OBSOLETE knob=yarm.unlock_graduated"; then
    echo "[ok] REMOVE-FALLBACKS: kernel reported yarm.unlock_graduated obsolete + ignored"
  else
    echo "[error] REMOVE-FALLBACKS: kernel did not report the obsolete unlock_graduated knob"
    exit 1
  fi
fi

# The graduated path is the production path on every boot. Require the graduated verdict
# (result=ok) and the accepted seam OK markers; a missing/failed verdict is fatal now
# (no longer a soft-observe — there is no other path to fall back to).
ug_fail=0
for m in \
  "UNLOCK_GRADUATED_D2_RECV_OK" \
  "UNLOCK_GRADUATED_D2_SEND_OK" \
  "UNLOCK_GRADUATED_D6_OK" \
  "UNLOCK_GRADUATED_D3_OK" \
  "UNLOCK_GRADUATED_INVARIANT_OK"; do
  if log_has_pattern "$m"; then
    echo "[ok] REMOVE-FALLBACKS marker present: $m"
  else
    echo "[error] REMOVE-FALLBACKS: required graduated marker missing: $m"
    ug_fail=1
  fi
done
if ! log_has_pattern "UNLOCK_GRADUATED_DONE result=ok"; then
  echo "[error] REMOVE-FALLBACKS: UNLOCK_GRADUATED_DONE result=ok missing (graduated path must run)"
  ug_fail=1
fi
if [[ "$ug_fail" -eq 1 ]]; then
  echo "[error] REMOVE-FALLBACKS: graduated production path verification FAILED"
  exit 1
fi
echo "[ok] REMOVE-FALLBACKS: x86_64 -smp1 graduated seams are the only production path (no fallback)"

# Stage 183 (SMP-LIVE): under x86_64 -smp >1, verify the SMP-liveness audit ran and no
# fallback / fatal path fired. The graduated seams remain the ONLY x86_64 path; until AP
# scheduler admission lands the APs park (online==1) and the audit reports the blocker
# honestly (result=deferred reason=aps_not_admitted). Either the deferred verdict or a
# future aps_live verdict is acceptable here; a FALLBACK/UNEXPECTED_INLOCK is fatal.
if [[ "$QEMU_SMP" -gt 1 ]]; then
  echo "[info] SMP-LIVE: x86_64 -smp $QEMU_SMP acceptance checks"
  if ! log_has_pattern "X86_SMP_UNLOCK_DONE"; then
    echo "[error] SMP-LIVE: X86_SMP_UNLOCK_DONE audit verdict missing under -smp $QEMU_SMP"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  else
    echo "[ok] SMP-LIVE: X86_SMP_UNLOCK audit verdict present"
  fi
  for f in \
    "UNLOCK_GRADUATED_DEFERRED reason=emergency_optout" \
    "UNLOCK_GRADUATED_FALLBACK path=" \
    "UNLOCK_GRADUATED_UNEXPECTED_INLOCK_DISPATCH" \
    "X86_SMP_ONLINE_ACCOUNTING_BAD" \
    "X86_AP_GS_BAD" \
    "X86_AP_IDLE_FAIL" \
    "X86_AP_SCHED_ADMIT_FAIL" \
    "X86_AP_KERNEL_CR3_FAIL" \
    "X86_AP_TSS_BAD" \
    "X86_AP_LAPIC_BAD" \
    "X86_AP_IDLE_CONTEXT_BAD" \
    "X86_AP_SCHED_PREREQ_INCOMPLETE" \
    "X86_AP_CR4_SYNC_FAIL" \
    "X86_AP_IDT_BAD" \
    "X86_AP_IDT_VECTOR_BAD" \
    "X86_AP_IST_BAD" \
    "X86_AP_LAPIC_INTERRUPT_BAD" \
    "X86_IPI_FIXED_SEND_FAIL" \
    "X86_AP_INTERRUPT_SMOKE_FAIL" \
    "X86_AP_IDLE_TASK_BAD" \
    "X86_AP_SCHED_ONLINE_FAIL" \
    "X86_AP_SCHED_IDLE_BAD" \
    "D6_SMP_LOST_WAKE_FAIL" \
    "D6_SMP_DUP_WAKE_FAIL" \
    "SCHED_ENQUEUE_DENIED_WAKE_ONLY"; do
    if log_has_pattern "$f"; then
      echo "[error] SMP-LIVE: forbidden marker under SMP: $f"
      exit 1
    fi
  done
  # Stage 183 increment 2: AP idle admission. The APs must leave the bare park loop and
  # reach the GS-initialized, interrupt-masked idle loop. Require the admission markers +
  # the GS-verified idle-live verdict (interrupt-masked, NOT scheduler-runnable yet).
  # Stage 183 increment 3: scheduler-admission PREREQUISITES — the APs must additionally
  # prove kernel CR3 live (.bss canary), per-AP GDT/TSS loaded (busy-bit readback), LAPIC
  # access (ID readback match), timer explicitly deferred (no scheduler tick yet), and
  # the idle task metadata/context (recorded + live-rsp validated; nothing enqueued).
  # Stage 183 increment 4: INTERRUPT-SAFE IDLE — CR4 synced with the BSP, the AP-safe
  # IDT loaded (catch-all park stubs + smoke handler; ist=0 policy validated), and the
  # controlled interrupt smoke: exactly one BSP->AP fixed IPI handled (gs: count+vector,
  # LAPIC EOI, iretq) with the AP returning to its interrupt-masked idle loop.
  for m in \
    "X86_AP_SCHED_ADMIT_BEGIN" \
    "X86_AP_GS_OK" \
    "X86_AP_KERNEL_CR3_BEGIN" \
    "X86_AP_KERNEL_CR3_OK" \
    "X86_AP_GDT_LOCAL_OK" \
    "X86_AP_TSS_OK" \
    "X86_AP_LAPIC_OK" \
    "X86_AP_LAPIC_TIMER_DEFERRED" \
    "X86_AP_IDLE_TASK_READY" \
    "X86_AP_IDLE_CONTEXT_OK" \
    "X86_AP_SCHED_PREREQ_OK" \
    "X86_AP_CR4_SYNC_OK" \
    "X86_AP_IDT_BEGIN" \
    "X86_AP_IDT_OK" \
    "X86_AP_IDT_VECTOR_OK" \
    "X86_AP_IST_OK" \
    "X86_AP_LAPIC_ENABLE_BEGIN" \
    "X86_AP_LAPIC_SVR_OK" \
    "X86_AP_LAPIC_TPR_OK" \
    "X86_AP_LAPIC_ESR_OK" \
    "X86_AP_LAPIC_INTERRUPT_READY" \
    "X86_AP_INTERRUPT_SMOKE_BEGIN" \
    "X86_IPI_FIXED_SEND_BEGIN" \
    "X86_IPI_FIXED_ICR_WRITTEN" \
    "X86_IPI_FIXED_SEND_DONE" \
    "X86_IPI_REMOTE_WAKE_SEND" \
    "X86_IPI_REMOTE_WAKE_RECV" \
    "X86_IPI_REMOTE_WAKE_ACK" \
    "X86_AP_INTERRUPT_SMOKE_OK" \
    "X86_AP_IDLE_ENTER" \
    "X86_AP_SCHED_ADMIT_DONE" \
    "X86_SMP_AP_ENV_READY" \
    "X86_SMP_AP_INTERRUPT_READY" \
    "X86_AP_IDLE_TASK_CREATE_BEGIN" \
    "X86_AP_IDLE_TASK_READY" \
    "X86_AP_IDLE_TASK_ACTIVE" \
    "X86_AP_SCHED_ONLINE_BEGIN" \
    "X86_AP_SCHED_ONLINE_OK" \
    "X86_AP_SCHED_IDLE_ENTER" \
    "X86_AP_SCHED_IDLE_REENTER" \
    "D6_SMP_REMOTE_WAKE_OK" \
    "X86_SMP_AP_SCHED_ONLINE" \
    "X86_SMP_PLACEMENT_GATED"; do
    if ! log_has_pattern "$m"; then
      echo "[error] SMP-LIVE: AP admission/prereq marker missing: $m"
      exit 1
    fi
    echo "[ok] SMP-LIVE: AP admission marker present: $m"
  done
  # Stage 183.5: AP scheduler-online admission + remote-wake proof. All present APs
  # must be scheduler-online (wake-only: placement gated until the AP dispatcher in
  # 183.6) and the exactly-one-wake proof must pass per AP. The graduated one-shot
  # proof runs BEFORE the admission (online==1 at proof time), so the unconditional
  # UNLOCK_GRADUATED_DONE result=ok gate above still holds.
  if ! log_has_pattern "X86_SMP_ONLINE_READY present=${QEMU_SMP} online=${QEMU_SMP}"; then
    echo "[error] SMP-LIVE: expected X86_SMP_ONLINE_READY present=${QEMU_SMP} online=${QEMU_SMP}"
    exit 1
  fi
  if ! log_has_pattern "X86_SMP_UNLOCK_DONE result=aps_online"; then
    echo "[error] SMP-LIVE: expected X86_SMP_UNLOCK_DONE result=aps_online (183.5 verdict)"
    exit 1
  fi
  # Tripwire: the legacy premature-admission marker must never reappear.
  if log_has_pattern "X86_SMP_APS_ADMITTED"; then
    echo "[error] SMP-LIVE: legacy X86_SMP_APS_ADMITTED marker must not be emitted"
    exit 1
  fi
  echo "[ok] SMP-LIVE: APs scheduler-online (wake-only) + remote-wake proven under -smp $QEMU_SMP"
fi

if log_has_pattern "YARM_BOOT_OK"; then
  echo "[ok] x86_64 boot markers detected"
  exit 0
fi

echo "[warn] boot markers not detected (status=$QEMU_STATUS)"
if [[ -f "$LOGFILE" ]]; then
  echo "[info] last 20 log lines from $LOGFILE:"
  tail -n 20 "$LOGFILE" || true
fi
[[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
exit 0
