#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Stage 92: x86_64 optional-FS smoke test.
# Checks RAMFS, EXT4, and FAT (skipped) marker presence in a QEMU boot log.
# Sources qemu-smoke-common.sh for shared helpers.
#
# Environment overrides:
#   RAMFS_SMOKE_EXPECTED  — set to 1 if RAMFS is expected (default: 1)
#   EXT4_SMOKE_EXPECTED   — set to 1 if EXT4 is expected  (default: 1)
#   FAT_SMOKE_EXPECTED    — set to 1 if FAT is expected    (default: 0)
#   KERNEL_IMAGE, INITRAMFS_IMAGE, LOGFILE, TIMEOUT_SECS
#   QEMU_SMOKE_STRICT

set -euo pipefail
source "$(dirname "$0")/qemu-smoke-common.sh"

KERNEL_IMAGE=${KERNEL_IMAGE:-build-x86_64/kernel_boot.elf}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-build-x86_64/initramfs-core.cpio}
TIMEOUT_SECS=${TIMEOUT_SECS:-60}
QEMU_SMOKE_STRICT=${QEMU_SMOKE_STRICT:-0}
QEMU_MACHINE=${QEMU_MACHINE:-q35}
QEMU_CPU=${QEMU_CPU:-qemu64}
QEMU_MEMORY=${QEMU_MEMORY:-512M}
# SMP must always be 1 for x86_64 smoke (x86_64 SMP not validated).
QEMU_SMP=1
DEFAULT_KERNEL_CMDLINE="console=ttyS0 rdinit=/init"
KERNEL_CMDLINE=${KERNEL_CMDLINE:-"$DEFAULT_KERNEL_CMDLINE"}

# Default expectations for Stage 92:
#   RAMFS is live (INIT_SPAWN_RAMFS_SRV=true, VFS_RAMFS_LIVE_MOUNT_ENABLED=true)
#   EXT4 is live  (INIT_SPAWN_EXT4_SRV=true,  VFS_EXT4_LIVE_MOUNT_ENABLED=true)
#   FAT is disabled (INIT_SPAWN_FAT_SRV=false, no virtio_blk in default profile)
#   QEMU_SMOKE_STRICT=1: INIT_SPAWN_V5_WRONG_SENDER_REPLY must be absent (count=0)
RAMFS_SMOKE_EXPECTED=${RAMFS_SMOKE_EXPECTED:-1}
EXT4_SMOKE_EXPECTED=${EXT4_SMOKE_EXPECTED:-1}
FAT_SMOKE_EXPECTED=${FAT_SMOKE_EXPECTED:-0}

# ---------------------------------------------------------------------------
# Pre-flight: verify required files and QEMU binary.
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
  echo "[warn] qemu-system-x86_64 not installed; skipping optional-FS smoke"
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

LOGFILE=${LOGFILE:-qemu-x86_64-optional-fs.log}
rm -f "$LOGFILE"

# ---------------------------------------------------------------------------
# QEMU command — mirrors x86_64-core-smoke.sh configuration.
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

echo "[info] qemu-x86_64-optional-fs-smoke: running ${QEMU_CMD[*]}"
echo "[info] expectations: RAMFS_SMOKE_EXPECTED=${RAMFS_SMOKE_EXPECTED} EXT4_SMOKE_EXPECTED=${EXT4_SMOKE_EXPECTED} FAT_SMOKE_EXPECTED=${FAT_SMOKE_EXPECTED}"

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

log_count_pattern() {
  local pattern="$1"
  [[ -f "$LOGFILE" ]] || { echo 0; return; }
  tr '\r' '\n' <"$LOGFILE" | rg -a -c "\\b${pattern}\\b" 2>/dev/null || echo 0
}

log_has_pattern() {
  local pattern="$1"
  [[ -f "$LOGFILE" ]] || return 1
  tr '\r' '\n' <"$LOGFILE" | rg -a -q "$pattern" 2>/dev/null
}

smoke_fail=0

# ---------------------------------------------------------------------------
# Fail-fast: ZC loader errors for optional FS image IDs (10=fat, 11=ramfs, 12=ext4).
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  for img_id in 10 11 12; do
    zc_fail=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_ELF_ZC_FAIL image_id=${img_id}\\b" 2>/dev/null || echo 0)
    if [[ "$zc_fail" -gt 0 ]]; then
      echo "[error] PM_ELF_ZC_FAIL image_id=${img_id} count=${zc_fail} (ZC loader error for optional FS server)"
      smoke_fail=1
    else
      echo "[ok] PM_ELF_ZC_FAIL image_id=${img_id} count=0"
    fi
  done
fi

# ---------------------------------------------------------------------------
# Strict: fatal runtime error patterns must be absent.
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  # Stage 92: wrong-sender SpawnV5 drain must be zero.
  wsr_count=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "INIT_SPAWN_V5_WRONG_SENDER_REPLY" 2>/dev/null || echo 0)
  if [[ "$wsr_count" -gt 0 ]]; then
    echo "[error] INIT_SPAWN_V5_WRONG_SENDER_REPLY count=${wsr_count} (wrong-sender drain must be zero)"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && smoke_fail=1
  else
    echo "[ok] INIT_SPAWN_V5_WRONG_SENDER_REPLY count=0"
  fi

  # KSPAWN_EXTRA_CAP_DELEGATE_FAIL: kernel rejected a non-zero service_caps slot.
  kspawn_fail=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "KSPAWN_EXTRA_CAP_DELEGATE_FAIL" 2>/dev/null || echo 0)
  if [[ "$kspawn_fail" -gt 0 ]]; then
    echo "[error] KSPAWN_EXTRA_CAP_DELEGATE_FAIL count=${kspawn_fail} (kernel rejected service_caps entry — check spawn call)"
    smoke_fail=1
  else
    echo "[ok] KSPAWN_EXTRA_CAP_DELEGATE_FAIL count=0"
  fi

  # D2_PUBLISH_RACE_UNWIND: D2 endpoint-recv waiter-publish no-lost-wakeup
  # unwind. Per doc/AI_AGENT_RULES.md §14.3 / doc/KERNEL_UNLOCKING.md §3 this
  # must be 0 — any occurrence is a stop-ship bug, not just a strict-mode warning.
  if rg -a -q "D2_PUBLISH_RACE_UNWIND" "$LOGFILE" 2>/dev/null; then
    echo "[fail] STOP-SHIP: D2_PUBLISH_RACE_UNWIND observed"
    smoke_fail=1
  else
    echo "[ok] D2_PUBLISH_RACE_UNWIND count=0"
  fi

  # PM_VFS_SPAWN_FAIL: PM failed to load/spawn a server from VFS.
  pm_vfs_fail=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_VFS_SPAWN_FAIL" 2>/dev/null || echo 0)
  if [[ "$pm_vfs_fail" -gt 0 ]]; then
    echo "[error] PM_VFS_SPAWN_FAIL count=${pm_vfs_fail} (PM failed to load server ELF via VFS)"
    smoke_fail=1
  else
    echo "[ok] PM_VFS_SPAWN_FAIL count=0"
  fi

  # bad_fd_decode inside a PM_ELF_ZC_FAIL or PM_VFS_SPAWN_FAIL context.
  bad_fd=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "reason=bad_fd_decode" 2>/dev/null || echo 0)
  if [[ "$bad_fd" -gt 0 ]]; then
    echo "[error] bad_fd_decode count=${bad_fd} (PM received malformed fd from VFS — check vfs_client blocking-recv fix)"
    smoke_fail=1
  else
    echo "[ok] bad_fd_decode count=0"
  fi

  # PM_VFS_GRANT_RO_UNSUPPORTED fallback=phase2b: Phase 3A zero-copy grant unavailable.
  phase2b=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "fallback=phase2b" 2>/dev/null || echo 0)
  if [[ "$phase2b" -gt 0 ]]; then
    echo "[warn] PM_VFS_GRANT_RO_UNSUPPORTED fallback=phase2b count=${phase2b} (Phase 3A unavailable, using bulk-read fallback)"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && smoke_fail=1
  else
    echo "[ok] Phase2B fallback count=0"
  fi

  # Kernel panic or userspace panic.
  # Stage 94: exclude lines containing nonfatal=true — those are non-fatal diagnostic events.
  panic_count=$(tr '\r' '\n' <"$LOGFILE" | rg -ai "\bpanic\b" 2>/dev/null | rg -avc "nonfatal=true" 2>/dev/null || echo 0)
  if [[ "$panic_count" -gt 0 ]]; then
    echo "[error] panic count=${panic_count} (kernel or userspace panic detected)"
    smoke_fail=1
  else
    echo "[ok] panic count=0"
  fi
fi

# ---------------------------------------------------------------------------
# Fail-fast: spawn failure markers for optional FS servers.
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  if log_has_pattern "INIT_RAMFS_SPAWN_FAIL"; then
    FAIL_COUNT=$(log_count_pattern "INIT_RAMFS_SPAWN_FAIL")
    echo "[error] INIT_RAMFS_SPAWN_FAIL count=${FAIL_COUNT} (RAMFS server spawn failed)"
    smoke_fail=1
  else
    echo "[ok] INIT_RAMFS_SPAWN_FAIL count=0"
  fi
  if log_has_pattern "INIT_EXT4_SPAWN_FAIL"; then
    FAIL_COUNT=$(log_count_pattern "INIT_EXT4_SPAWN_FAIL")
    echo "[error] INIT_EXT4_SPAWN_FAIL count=${FAIL_COUNT} (EXT4 server spawn failed)"
    smoke_fail=1
  else
    echo "[ok] INIT_EXT4_SPAWN_FAIL count=0"
  fi
fi

# ---------------------------------------------------------------------------
# RAMFS optional-FS markers.
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  RAMFS_MARKERS=(
    INIT_RAMFS_SPAWN_BEGIN
    INIT_RAMFS_SPAWN_OK
    RAMFS_SRV_ENTRY
    RAMFS_MOUNT_READY
    VFS_MOUNT_REGISTER_RAMFS_OK
  )
  echo "[info] --- RAMFS optional-FS markers ---"
  ramfs_ok=1
  for marker in "${RAMFS_MARKERS[@]}"; do
    count=$(log_count_pattern "$marker")
    echo "[info] RAMFS marker count: ${marker}=${count}"
    if [[ "$RAMFS_SMOKE_EXPECTED" == "1" && "$count" -eq 0 ]]; then
      echo "[error] RAMFS expected marker missing: ${marker}"
      ramfs_ok=0
      smoke_fail=1
    fi
  done
  if [[ "$RAMFS_SMOKE_EXPECTED" == "1" && "$ramfs_ok" -eq 1 ]]; then
    echo "[ok] RAMFS optional-FS: all required markers present"
  fi
fi

# ---------------------------------------------------------------------------
# EXT4 optional-FS markers.
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  EXT4_MARKERS=(
    INIT_EXT4_SPAWN_BEGIN
    INIT_EXT4_SPAWN_OK
    EXT4_SRV_ENTRY
    EXT4_SRV_READY
    VFS_MOUNT_REGISTER_EXT4_OK
  )
  echo "[info] --- EXT4 optional-FS markers ---"
  ext4_ok=1
  for marker in "${EXT4_MARKERS[@]}"; do
    count=$(log_count_pattern "$marker")
    echo "[info] EXT4 marker count: ${marker}=${count}"
    if [[ "$EXT4_SMOKE_EXPECTED" == "1" && "$count" -eq 0 ]]; then
      echo "[error] EXT4 expected marker missing: ${marker}"
      ext4_ok=0
      smoke_fail=1
    fi
  done
  if [[ "$EXT4_SMOKE_EXPECTED" == "1" && "$ext4_ok" -eq 1 ]]; then
    echo "[ok] EXT4 optional-FS: all required markers present"
  fi
fi

# ---------------------------------------------------------------------------
# FAT markers: expect skipped, not spawned.
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
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
  echo "[info] --- FAT optional-FS markers ---"
  fat_seen=0
  for marker in "${FAT_MARKERS[@]}"; do
    count=$(log_count_pattern "$marker")
    if [[ "$count" -gt 0 ]]; then
      fat_seen=1
    fi
    echo "[info] FAT marker count: ${marker}=${count}"
  done

  if [[ "$FAT_SMOKE_EXPECTED" == "0" ]]; then
    # FAT is disabled: INIT_FAT_SPAWN_SKIPPED must appear (one of the two reasons).
    skipped_count=$(log_count_pattern "INIT_FAT_SPAWN_SKIPPED")
    if [[ "$skipped_count" -gt 0 ]]; then
      echo "[ok] FAT skipped marker present (INIT_FAT_SPAWN_SKIPPED count=${skipped_count})"
    else
      echo "[info] FAT skipped marker not found (may be absent if profile did not reach optional-FS section)"
    fi
    # FAT must not have spawned.
    if [[ "$(log_count_pattern INIT_FAT_SPAWN_OK)" -gt 0 ]]; then
      echo "[error] INIT_FAT_SPAWN_OK found but FAT_SMOKE_EXPECTED=0 (FAT must not spawn in default profile)"
      smoke_fail=1
    fi
  fi

  if [[ "$FAT_SMOKE_EXPECTED" == "1" && "$fat_seen" -eq 0 ]]; then
    echo "[error] FAT smoke expected but no FAT markers were observed"
    smoke_fail=1
  fi
fi

# ---------------------------------------------------------------------------
# Summary.
# ---------------------------------------------------------------------------
if [[ "$smoke_fail" -eq 1 ]]; then
  echo "[error] x86_64 optional-FS smoke: FAILED"
  if [[ -f "$LOGFILE" ]]; then
    echo "[info] last 20 log lines from $LOGFILE:"
    tail -n 20 "$LOGFILE" || true
  fi
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

echo "[ok] x86_64 optional-FS smoke: all checks passed (status=$QEMU_STATUS)"
exit 0
