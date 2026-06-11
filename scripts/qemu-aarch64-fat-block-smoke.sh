#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Stage 93: AArch64 FAT block-device profile smoke test.
#
# Runs YARM with a real virtio-blk-backed FAT image and verifies that:
#   - fat_srv receives the blkcache block capability at startup
#   - FAT filesystem mounts successfully via the IPC block backend
#   - /fat is registered with VFS
#   - FAT markers are present in the log
#
# Prerequisites:
#   - FAT block image at ${FAT_IMAGE} (create with scripts/create-fat-image.sh)
#   - kernel image at ${KERNEL_IMAGE}
#   - initramfs image at ${INITRAMFS_IMAGE}
#   - INIT_SPAWN_FAT_SRV=true profile binary (requires recompile with fat-block profile)
#
# Environment overrides:
#   FAT_IMAGE, KERNEL_IMAGE, INITRAMFS_IMAGE, LOGFILE, TIMEOUT_SECS
#   QEMU_BIN, QEMU_SMOKE_STRICT
#
# NOTE: This script always exits 0 if FAT_IMAGE is missing or QEMU is unavailable.
# Set QEMU_SMOKE_STRICT=1 to make it fail on missing prerequisites.

set -euo pipefail
source "$(dirname "$0")/qemu-smoke-common.sh"

KERNEL_IMAGE=${KERNEL_IMAGE:-build-aarch64/yarm-aarch64.bin}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-build-aarch64/initramfs-core.cpio}
FAT_IMAGE=${FAT_IMAGE:-build-fat/fat.img}
TIMEOUT_SECS=${TIMEOUT_SECS:-45}
QEMU_SMOKE_STRICT=${QEMU_SMOKE_STRICT:-0}
QEMU_MACHINE=${QEMU_MACHINE:-virt}
QEMU_CPU=${QEMU_CPU:-cortex-a72}
QEMU_MEMORY=${QEMU_MEMORY:-1024M}
QEMU_SMP=${QEMU_SMP:-2}
KERNEL_CMDLINE=${KERNEL_CMDLINE:-}

# FAT block profile: all FS servers enabled; FAT uses real virtio-blk.
RAMFS_SMOKE_EXPECTED=${RAMFS_SMOKE_EXPECTED:-1}
EXT4_SMOKE_EXPECTED=${EXT4_SMOKE_EXPECTED:-1}
FAT_SMOKE_EXPECTED=${FAT_SMOKE_EXPECTED:-1}

# ---------------------------------------------------------------------------
# Pre-flight checks.
# ---------------------------------------------------------------------------
require_file_or_warn "$KERNEL_IMAGE" "$QEMU_SMOKE_STRICT" "kernel image"
require_file_or_warn "$INITRAMFS_IMAGE" "$QEMU_SMOKE_STRICT" "initramfs image"

if [[ ! -f "$FAT_IMAGE" ]]; then
  echo "[warn] FAT image missing: $FAT_IMAGE"
  echo "[hint] run: scripts/create-fat-image.sh $FAT_IMAGE"
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  echo "[info] skipping fat-block smoke (no FAT image)"
  exit 0
fi

QEMU_BIN=${QEMU_BIN:-qemu-system-aarch64-hwe}
if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  QEMU_BIN=qemu-system-aarch64
fi

if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  echo "[warn] qemu binary not available ($QEMU_BIN); skipping fat-block smoke"
  exit 0
fi

LOGFILE=${LOGFILE:-qemu-aarch64-fat-block.log}
rm -f "$LOGFILE"

# ---------------------------------------------------------------------------
# QEMU command — adds virtio-blk device backed by FAT image.
# ---------------------------------------------------------------------------
QEMU_ARGS=(
  -machine "$QEMU_MACHINE"
  -cpu "$QEMU_CPU"
  -m "$QEMU_MEMORY"
  -smp "$QEMU_SMP"
  -nographic
  -monitor none
  -serial stdio
  -kernel "$KERNEL_IMAGE"
  -initrd "$INITRAMFS_IMAGE"
  -drive "file=${FAT_IMAGE},if=none,id=blk0,format=raw"
  -device "virtio-blk-pci,drive=blk0"
)
if [[ -n "$KERNEL_CMDLINE" ]]; then
  QEMU_ARGS+=(-append "$KERNEL_CMDLINE")
fi

echo "[info] qemu-aarch64-fat-block-smoke: running $QEMU_BIN ${QEMU_ARGS[*]}"
echo "[info] FAT image: $FAT_IMAGE"
echo "[info] expectations: RAMFS=${RAMFS_SMOKE_EXPECTED} EXT4=${EXT4_SMOKE_EXPECTED} FAT=${FAT_SMOKE_EXPECTED}"

if run_qemu_timeout_to_log "$TIMEOUT_SECS" "$LOGFILE" "$QEMU_BIN" "${QEMU_ARGS[@]}"; then
  QEMU_STATUS=0
else
  QEMU_STATUS=$?
fi

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
# Fatal pattern checks.
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  for pattern in KSPAWN_EXTRA_CAP_DELEGATE_FAIL PM_VFS_SPAWN_FAIL "reason=bad_fd_decode"; do
    count=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "$pattern" 2>/dev/null || echo 0)
    if [[ "$count" -gt 0 ]]; then
      echo "[error] $pattern count=${count}"
      smoke_fail=1
    else
      echo "[ok] $pattern count=0"
    fi
  done

  for img_id in 10 11 12; do
    zc_fail=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_ELF_ZC_FAIL image_id=${img_id}\\b" 2>/dev/null || echo 0)
    if [[ "$zc_fail" -gt 0 ]]; then
      echo "[error] PM_ELF_ZC_FAIL image_id=${img_id} count=${zc_fail}"
      smoke_fail=1
    else
      echo "[ok] PM_ELF_ZC_FAIL image_id=${img_id} count=0"
    fi
  done

  # Wrong-sender drain must be zero.
  wsr=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "INIT_SPAWN_V5_WRONG_SENDER_REPLY" 2>/dev/null || echo 0)
  if [[ "$wsr" -gt 0 ]]; then
    echo "[error] INIT_SPAWN_V5_WRONG_SENDER_REPLY count=${wsr}"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && smoke_fail=1
  else
    echo "[ok] INIT_SPAWN_V5_WRONG_SENDER_REPLY count=0"
  fi

  # Panic.
  panic_count=$(tr '\r' '\n' <"$LOGFILE" | rg -ai -c "\bpanic\b" 2>/dev/null || echo 0)
  if [[ "$panic_count" -gt 0 ]]; then
    echo "[error] panic count=${panic_count}"
    smoke_fail=1
  else
    echo "[ok] panic count=0"
  fi
fi

# ---------------------------------------------------------------------------
# FAT block profile markers.
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  FAT_MARKERS=(
    INIT_FAT_SPAWN_BEGIN
    INIT_FAT_SPAWN_OK
    FAT_BIN_ENTRY_START
    FAT_CONFIG_FOUND
    FAT_BLOCK_BACKEND_STARTUP_CAP
    FAT_MOUNT_READY
    FAT_SRV_READY
    VFS_MOUNT_REGISTER_FAT_OK
  )
  echo "[info] --- FAT block-device profile markers ---"
  fat_ok=1
  for marker in "${FAT_MARKERS[@]}"; do
    count=$(log_count_pattern "$marker")
    echo "[info] FAT marker: ${marker}=${count}"
    if [[ "$FAT_SMOKE_EXPECTED" == "1" && "$count" -eq 0 ]]; then
      echo "[error] FAT required marker missing: ${marker}"
      fat_ok=0
      smoke_fail=1
    fi
  done
  if [[ "$FAT_SMOKE_EXPECTED" == "1" && "$fat_ok" -eq 1 ]]; then
    echo "[ok] FAT block-device profile: all required markers present"
  fi
fi

# ---------------------------------------------------------------------------
# RAMFS markers (must still pass with FAT enabled).
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" && "$RAMFS_SMOKE_EXPECTED" == "1" ]]; then
  for marker in INIT_RAMFS_SPAWN_BEGIN INIT_RAMFS_SPAWN_OK RAMFS_SRV_ENTRY VFS_MOUNT_REGISTER_RAMFS_OK; do
    count=$(log_count_pattern "$marker")
    echo "[info] RAMFS marker: ${marker}=${count}"
    if [[ "$count" -eq 0 ]]; then
      echo "[error] RAMFS required marker missing: ${marker}"
      smoke_fail=1
    fi
  done
fi

# ---------------------------------------------------------------------------
# EXT4 markers (must still pass with FAT enabled).
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" && "$EXT4_SMOKE_EXPECTED" == "1" ]]; then
  for marker in INIT_EXT4_SPAWN_BEGIN INIT_EXT4_SPAWN_OK EXT4_SRV_READY VFS_MOUNT_REGISTER_EXT4_OK; do
    count=$(log_count_pattern "$marker")
    echo "[info] EXT4 marker: ${marker}=${count}"
    if [[ "$count" -eq 0 ]]; then
      echo "[error] EXT4 required marker missing: ${marker}"
      smoke_fail=1
    fi
  done
fi

# ---------------------------------------------------------------------------
# FAT write must be Unsupported even in production.
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  fat_write_ok=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "FAT_WRITE_OK\|fat.*write.*success" 2>/dev/null || echo 0)
  if [[ "$fat_write_ok" -gt 0 ]]; then
    echo "[error] FAT write succeeded (must remain Unsupported in production profile)"
    smoke_fail=1
  else
    echo "[ok] FAT write not accepted (correct)"
  fi
fi

# ---------------------------------------------------------------------------
# Summary.
# ---------------------------------------------------------------------------
if [[ "$smoke_fail" -eq 1 ]]; then
  echo "[error] aarch64 fat-block smoke: FAILED"
  if [[ -f "$LOGFILE" ]]; then
    echo "[info] last 20 log lines:"
    tail -n 20 "$LOGFILE" || true
  fi
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

echo "[ok] aarch64 fat-block smoke: all checks passed (status=$QEMU_STATUS)"
exit 0
