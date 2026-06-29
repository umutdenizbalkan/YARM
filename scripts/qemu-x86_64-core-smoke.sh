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
# SMP must always be 1; x86_64 SMP is out of scope for this smoke.
QEMU_SMP=1
DEFAULT_KERNEL_CMDLINE="console=ttyS0 rdinit=/init"
KERNEL_CMDLINE=${KERNEL_CMDLINE:-"$DEFAULT_KERNEL_CMDLINE"}
D6_SWITCH_PROOF=${D6_SWITCH_PROOF:-0}
if [[ "$D6_SWITCH_PROOF" == "1" && "$KERNEL_CMDLINE" != *"yarm.d6_switch_proof="* ]]; then
  KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.d6_switch_proof=1"
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
  if [[ "$proof_fail" -eq 1 || "$fatal_after_proof" -eq 1 ]]; then
    echo "[error] D6 switch proof mode FAILED"
    exit 1
  fi
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
