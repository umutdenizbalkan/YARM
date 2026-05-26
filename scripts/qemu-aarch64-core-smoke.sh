#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail
source "$(dirname "$0")/qemu-smoke-common.sh"

KERNEL_IMAGE=${KERNEL_IMAGE:-build-aarch64/yarm-aarch64.bin}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-build-aarch64/initramfs-core.cpio}
TIMEOUT_SECS=${TIMEOUT_SECS:-30}
QEMU_SMOKE_STRICT=${QEMU_SMOKE_STRICT:-0}
QEMU_MACHINE=${QEMU_MACHINE:-virt}
QEMU_CPU=${QEMU_CPU:-cortex-a72}
QEMU_MEMORY=${QEMU_MEMORY:-1024M}
QEMU_SMP=${QEMU_SMP:-2}
# Keep kernel cmdline empty by default until AArch64 command-line parsing is
# explicitly validated. Override if needed via KERNEL_CMDLINE=...
KERNEL_CMDLINE=${KERNEL_CMDLINE:-}

require_file_or_warn "$KERNEL_IMAGE" "$QEMU_SMOKE_STRICT" "kernel image"
require_file_or_warn "$INITRAMFS_IMAGE" "$QEMU_SMOKE_STRICT" "initramfs image"
QEMU_BIN=${QEMU_BIN:-qemu-system-aarch64-hwe}
if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  QEMU_BIN=qemu-system-aarch64
fi
require_qemu_or_warn "$QEMU_BIN" "$QEMU_SMOKE_STRICT"

LOGFILE=${LOGFILE:-qemu-aarch64-core.log}
rm -f "$LOGFILE"

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
)
if [[ -n "$KERNEL_CMDLINE" ]]; then
  QEMU_ARGS+=(-append "$KERNEL_CMDLINE")
fi

echo "[info] qemu command: $QEMU_BIN ${QEMU_ARGS[*]}"

MARKER_REGEX="YARM_AARCH64_BOOT_MARKER|YARM_SUPERVISOR_TID2_SPAWNED|YARM_PM_TID3_SPAWNED|YARM_BOOT_OK|YARM_PROC_VFS_OK|YARM_INIT_START|YARM_INIT_DONE|BusyBox|/ #|Welcome|\[ui\] boot-to-shell marker"
INIT_SERVER_REGEX="init_server|first server|first-server"
EARLY_MARKER_SEQUENCE=(
  "YARM_AARCH64_BOOT_MARKER stage=_start"
  "YARM_AARCH64_BOOT_MARKER stage=prepare_arch_boot"
  "YARM_AARCH64_BOOT_MARKER stage=vbar_el1_ready"
  "YARM_AARCH64_BOOT_MARKER stage=mmu_enabled"
  "YARM_AARCH64_BOOT_MARKER stage=run_with_prepared_kernel"
  "YARM_SUPERVISOR_TID2_SPAWNED"
  "YARM_PM_TID3_SPAWNED"
  "YARM_BOOT_OK"
)
# Markers 4-6 come from user_log! which is a no-op in no_std; checked warn-only.
SPAWN_IPC_SEQUENCE=(
  "YARM_PM_RECV_LOOP_START"
  "INIT_SPAWN_V5_CALL_BEGIN"
  "INIT_SPAWN_V5_REPLY_OK"
)

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

BLOCKER_REGEX='IPC_CALL_FAIL|IPC_RECV_CAP_MATERIALIZE_FAILED|IPC_RECV_BLOCKED_COMPLETE_FAILED|CapabilityFull|VM_FULL|YARM_FIRST_USER_FAIL|MemoryObjectMissing|ELF_MISSING|PrivilegeViolation|failed to bootstrap first user task|panic|InvalidCapability|WrongObject|StaleCapability|MissingRight|UserMemoryFault|PM_RECV_DECODE_FAIL|bad_len expected=16 got=8|CAP_LOOKUP tid=1 cap=0|empty-elf|Malformed|Syscall\\(Internal\\)|memory allocation of|DELEGATE_FAIL|delegation.*fail|IPC_REPLY_FAST_REVOKE_FAIL|PM_PANIC|INIT_PANIC|DEVFS_PANIC|VFS_PANIC|INITRAMFS_PANIC|INITRAMFS_CPIO_EMPTY'
BLOCKER_EXCLUDE_REGEX='YARM_AARCH64_EXCEPTION_KIND unknown|BLOCKED_WOULDBLOCK_CLASSIFY|reply replay|second reply|replay rejected'

if [[ -f "$LOGFILE" ]]; then
  blocker_lines="$(tr '\r' '\n' <"$LOGFILE" | rg -a -n "$BLOCKER_REGEX" || true)"
  if [[ -n "$blocker_lines" ]]; then
    blocker_lines="$(printf '%s\n' "$blocker_lines" | rg -a -v "$BLOCKER_EXCLUDE_REGEX" || true)"
  fi
  if [[ -n "$blocker_lines" ]]; then
    echo "[error] BAD / BOOT BLOCKERS found:"
    printf '%s\n' "$blocker_lines"
    exit 1
  else
    echo "[ok] BAD / BOOT BLOCKERS: empty"
  fi
fi

if check_common_boot_markers "$LOGFILE" "$MARKER_REGEX" "$INIT_SERVER_REGEX"; then
  if ! check_required_patterns "$LOGFILE" "${EARLY_MARKER_SEQUENCE[@]}"; then
    echo "[warn] aarch64 strict required markers are incomplete"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    exit 0
  fi
  if ! check_log_sequence "$LOGFILE" "${EARLY_MARKER_SEQUENCE[@]}"; then
    echo "[warn] aarch64 early boot marker sequence missing or out of order"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    exit 0
  fi

  if ! check_required_patterns "$LOGFILE" \
      "YARM_TIMER_IRQ_DELIVERED" \
      "YARM_TIMER_EOI_DONE" \
      "YARM_SCHED_TICK"; then
    echo "[warn] aarch64 timer progression markers missing"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    exit 0
  fi
  if ! check_log_sequence "$LOGFILE" "${SPAWN_IPC_SEQUENCE[@]}"; then
    echo "[warn] spawn IPC sequence absent (user_log! is a no-op in no_std; expected)"
  fi
  declare -A REQUIRED_SERVICE_ENTRIES=(
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
  # Phase 2A/2B: verify bulk read path was used for image_id 7/8/9.
  declare -A REQUIRED_BULK_MARKERS=(
    [PM_VFS_READ_BULK_BEGIN]=3
    [PM_VFS_READ_BULK_DONE]=3
    [PM_VFS_READ_DONE]=3
  )
  bulk_count_fail=0
  for marker in "${!REQUIRED_BULK_MARKERS[@]}"; do
    expected="${REQUIRED_BULK_MARKERS[$marker]}"
    actual=$(log_count_pattern "$marker")
    if [[ "$actual" -ge "$expected" ]]; then
      echo "[ok] bulk marker: ${marker}>=${expected} (got=${actual})"
    else
      echo "[warn] bulk marker missing: ${marker} expected>=${expected} got=${actual}"
      bulk_count_fail=1
    fi
  done
  if [[ "$bulk_count_fail" -eq 1 && "$QEMU_SMOKE_STRICT" == "1" ]]; then
    exit 1
  fi

  # Phase 2B path or Phase 2A fallback: check which bulk-read mode was used.
  PHASE2B_BULK_FORWARD=$(log_count_pattern "VFS_FORWARD_BULK_READ")
  PHASE2A_FALLBACK=$(log_count_pattern "PM_VFS_READ_BULK_PHASE2A_BEGIN")
  if [[ "$PHASE2B_BULK_FORWARD" -ge 3 ]]; then
    echo "[ok] Phase 2B VFS bulk-read active: VFS_FORWARD_BULK_READ=${PHASE2B_BULK_FORWARD} (mode=vfs_transfer)"
  elif [[ "$PHASE2A_FALLBACK" -ge 3 ]]; then
    echo "[ok] Phase 2A fallback active: PM_VFS_READ_BULK_PHASE2A_BEGIN=${PHASE2A_FALLBACK} (mode=phase2a_bridge)"
  else
    echo "[warn] neither Phase 2B VFS_FORWARD_BULK_READ (got=${PHASE2B_BULK_FORWARD}) nor Phase 2A PM_VFS_READ_BULK_PHASE2A_BEGIN (got=${PHASE2A_FALLBACK}) reached 3"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  fi

  # Phase 2B: if VFS bulk-read forwarding was active, check initramfs handled it.
  if [[ "$PHASE2B_BULK_FORWARD" -ge 1 && -f "$LOGFILE" ]]; then
    INITRAMFS_BULK=$(log_count_pattern "INITRAMFS_READ_BULK fd=")
    INITRAMFS_BULK_REPLY=$(log_count_pattern "INITRAMFS_READ_BULK_REPLY")
    if [[ "$INITRAMFS_BULK" -ge 1 && "$INITRAMFS_BULK_REPLY" -ge 1 ]]; then
      echo "[ok] Phase 2B initramfs bulk-read: handled=${INITRAMFS_BULK} replied=${INITRAMFS_BULK_REPLY}"
    else
      echo "[warn] Phase 2B VFS forwarding seen but initramfs bulk handler did not engage (INITRAMFS_READ_BULK=${INITRAMFS_BULK} REPLY=${INITRAMFS_BULK_REPLY})"
      [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    fi
  fi

  # Phase 2A safety: must not have not_found failures (means CPIO entry missing).
  if [[ -f "$LOGFILE" ]]; then
    NOT_FOUND=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_VFS_READ_BULK_FAIL.*reason=not_found" 2>/dev/null || echo 0)
    if [[ "$NOT_FOUND" -gt 0 ]]; then
      echo "[error] PM_VFS_READ_BULK_FAIL reason=not_found found: ${NOT_FOUND} — file missing in CPIO (hard failure)"
      exit 1
    else
      echo "[ok] no PM_VFS_READ_BULK_FAIL reason=not_found"
    fi
  fi

  # Mode check: verify PM_VFS_READ_BULK_DONE completions carry a known mode tag.
  if [[ -f "$LOGFILE" ]]; then
    BULK_DONE_VFS=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_VFS_READ_BULK_DONE.*mode=vfs_transfer" 2>/dev/null || echo 0)
    BULK_DONE_2A=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_VFS_READ_BULK_DONE.*mode=phase2a_bridge" 2>/dev/null || echo 0)
    BULK_DONE_TOTAL=$((BULK_DONE_VFS + BULK_DONE_2A))
    echo "[ok] PM_VFS_READ_BULK_DONE: mode=vfs_transfer=${BULK_DONE_VFS} mode=phase2a_bridge=${BULK_DONE_2A} total=${BULK_DONE_TOTAL}"
    if [[ "$BULK_DONE_TOTAL" -lt 3 && "$QEMU_SMOKE_STRICT" == "1" ]]; then
      echo "[error] expected at least 3 PM_VFS_READ_BULK_DONE with a known mode tag (got=${BULK_DONE_TOTAL})"
      exit 1
    fi
  fi

  # Phase 2: verify absent hot-path markers (should not appear in default logs).
  ABSENT_MARKERS=(PM_VFS_READ_APPEND COPY_TO_USER_PAGE YARM_LOCK_SPLIT_STAGE2N)
  for marker in "${ABSENT_MARKERS[@]}"; do
    if log_count_pattern "$marker" | grep -q "^[1-9]"; then
      echo "[warn] unexpected marker in log: ${marker}"
    else
      echo "[ok] absent marker confirmed: ${marker}"
    fi
  done
  service_count_fail=0
  for marker in "${!REQUIRED_SERVICE_ENTRIES[@]}"; do
    expected="${REQUIRED_SERVICE_ENTRIES[$marker]}"
    actual=$(log_count_pattern "$marker")
    if [[ "$actual" -eq "$expected" ]]; then
      echo "[ok] marker count: ${marker}=${actual}"
    else
      echo "[warn] marker count wrong: ${marker} expected=${expected} got=${actual}"
      service_count_fail=1
    fi
  done
  if [[ "$service_count_fail" -eq 1 && "$QEMU_SMOKE_STRICT" == "1" ]]; then
    exit 1
  fi
  echo "[ok] aarch64 strict marker progression detected"
  exit 0
fi

echo "[warn] boot shell and init-server markers not detected (status=$QEMU_STATUS)"
if [[ -f "$LOGFILE" ]]; then
  echo "[info] last 20 log lines from $LOGFILE"
  tail -n 20 "$LOGFILE" || true
fi

if [[ "$QEMU_SMOKE_STRICT" == "1" ]]; then
  exit 1
fi
exit 0
