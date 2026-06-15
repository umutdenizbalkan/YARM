#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail
source "$(dirname "$0")/qemu-smoke-common.sh"

# Official RISC-V64 core smoke. The kernel image and initramfs paths default to
# the artifacts emitted by scripts/build-qemu-riscv64-artifacts.sh.
KERNEL_IMAGE=${KERNEL_IMAGE:-build-riscv64/yarm-riscv64.bin}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-build-riscv64/initramfs-core.cpio}
TIMEOUT_SECS=${TIMEOUT_SECS:-30}
QEMU_SMOKE_STRICT=${QEMU_SMOKE_STRICT:-0}
QEMU_MACHINE=${QEMU_MACHINE:-virt}
QEMU_CPU=${QEMU_CPU:-rv64}
QEMU_MEMORY=${QEMU_MEMORY:-512M}
QEMU_SMP=${QEMU_SMP:-1}
QEMU_BIOS=${QEMU_BIOS:-default}
KERNEL_CMDLINE=${KERNEL_CMDLINE:-"console=ttyS0 rdinit=/init"}

# CLI: --smp 2  → enable smp=2 secondary-park assertion.
while (( $# > 0 )); do
  case "$1" in
    --smp)
      QEMU_SMP="$2"
      shift 2
      ;;
    --smp=*)
      QEMU_SMP="${1#--smp=}"
      shift
      ;;
    *)
      echo "[warn] unknown arg: $1" >&2
      shift
      ;;
  esac
done

require_file_or_warn "$KERNEL_IMAGE" "$QEMU_SMOKE_STRICT" "kernel image"
require_file_or_warn "$INITRAMFS_IMAGE" "$QEMU_SMOKE_STRICT" "initramfs image"

QEMU_BIN=${QEMU_BIN:-qemu-system-riscv64-hwe}
if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  QEMU_BIN=qemu-system-riscv64
fi
require_qemu_or_warn "$QEMU_BIN" "$QEMU_SMOKE_STRICT"

LOGFILE=${LOGFILE:-qemu-riscv64-core.log}
rm -f "$LOGFILE"

echo "[info] qemu command: $QEMU_BIN -machine $QEMU_MACHINE -cpu $QEMU_CPU -m $QEMU_MEMORY -smp $QEMU_SMP -nographic -monitor none -serial stdio -bios $QEMU_BIOS -kernel $KERNEL_IMAGE -initrd $INITRAMFS_IMAGE -append '$KERNEL_CMDLINE'"

if run_qemu_timeout_to_log "$TIMEOUT_SECS" "$LOGFILE" "$QEMU_BIN" \
  -machine "$QEMU_MACHINE" \
  -cpu "$QEMU_CPU" \
  -m "$QEMU_MEMORY" \
  -smp "$QEMU_SMP" \
  -nographic \
  -monitor none \
  -serial stdio \
  -bios "$QEMU_BIOS" \
  -kernel "$KERNEL_IMAGE" \
  -initrd "$INITRAMFS_IMAGE" \
  -append "$KERNEL_CMDLINE" \
; then
  QEMU_STATUS=0
else
  QEMU_STATUS=$?
fi

REQUIRED_PATTERNS=(
  "YARM_BOOT_OK"
  "RISCV_KERNEL_BOOT_OK"
  "RISCV_BOOT_HART_SELECTED hart="
  "RISCV_HART_TOPOLOGY present_cpus="
  "RISCV_LIVEEEEEEE"
  "RISCV_SYSCALL_ROUNDTRIP_OK"
  "RISCV_USER_RESUMED"
  "INITRAMFS_SRV_ENTRY"
  "DEVFS_SRV_ENTRY"
  "VFS_SRV_ENTRY"
  "VFS_MOUNT_TABLE_READY"
  "RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked"
)

# RAMFS/EXT4 mount markers are emitted when those servers register their
# mount points; treated as required for the core smoke once the chain runs.
OPTIONAL_FS_PATTERNS=(
  "RAMFS_MOUNT_READY"
  "VFS_MOUNT_REGISTER_RAMFS_OK"
  "EXT4_SRV_READY"
  "VFS_MOUNT_REGISTER_EXT4_OK"
)

# Timer / PLIC / external-IRQ acceptance: either the live marker OR the
# explicit deferral with reason. The kernel must emit exactly one of each
# pair so partial bring-up is detectable.
TIMER_ACCEPT_REGEX='RISCV_TIMER_SMOKE_OK ticks=|RISCV_TIMER_DEFERRED reason='
PLIC_ACCEPT_REGEX='RISCV_PLIC_INIT_DONE|RISCV_PLIC_DEFERRED reason='
EXTIRQ_ACCEPT_REGEX='RISCV_EXTIRQ_SMOKE_OK source=|RISCV_EXTIRQ_DEFERRED reason='

# Patterns that must NOT appear in a healthy boot.
REJECT_PATTERNS=(
  'RISCV_EARLY_TRAP'
  '\bPANIC\b'
  '\bFATAL\b'
  '\bASSERT\b'
  'PAGE_FAULT_UNHANDLED'
  'TRAP_HANDLE failed'
  'Vm\(Full\)'
  '\boom\b'
  '\bcapacity\b'
)

failures=0

for pat in "${REQUIRED_PATTERNS[@]}"; do
  if ! rg -n -F "$pat" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] required marker missing: $pat"
    failures=$((failures + 1))
  fi
done

for pat in "${OPTIONAL_FS_PATTERNS[@]}"; do
  if ! rg -n -F "$pat" "$LOGFILE" >/dev/null 2>&1; then
    echo "[warn] optional-FS marker missing: $pat"
  fi
done

if ! rg -nE "$TIMER_ACCEPT_REGEX" "$LOGFILE" >/dev/null 2>&1; then
  echo "[fail] neither RISCV_TIMER_SMOKE_OK nor RISCV_TIMER_DEFERRED present"
  failures=$((failures + 1))
fi

if ! rg -nE "$PLIC_ACCEPT_REGEX" "$LOGFILE" >/dev/null 2>&1; then
  echo "[fail] neither RISCV_PLIC_INIT_DONE nor RISCV_PLIC_DEFERRED present"
  failures=$((failures + 1))
fi

if ! rg -nE "$EXTIRQ_ACCEPT_REGEX" "$LOGFILE" >/dev/null 2>&1; then
  echo "[fail] neither RISCV_EXTIRQ_SMOKE_OK nor RISCV_EXTIRQ_DEFERRED present"
  failures=$((failures + 1))
fi

# Repeated missing-DTB loop: more than one occurrence of source=missing_dtb in
# YARM_BOOT_CMDLINE_CAPTURE is the failure mode the kernel guards against.
missing_dtb_count=$(rg -cF "source=missing_dtb" "$LOGFILE" 2>/dev/null || true)
missing_dtb_count=${missing_dtb_count:-0}
if (( missing_dtb_count > 1 )); then
  echo "[fail] repeated source=missing_dtb loop (count=$missing_dtb_count)"
  failures=$((failures + 1))
fi

for pat in "${REJECT_PATTERNS[@]}"; do
  if rg -nE "$pat" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] rejected pattern present: $pat"
    failures=$((failures + 1))
  fi
done

# SpawnV5 wrong-sender reply guard: the kernel marks any SpawnV5 reply that
# arrives on the wrong endpoint.
if rg -n "SPAWN_V5_WRONG_SENDER|YARM_SPAWN_V5_REPLY_WRONG_SENDER" "$LOGFILE" >/dev/null 2>&1; then
  echo "[fail] SpawnV5 wrong-sender reply observed"
  failures=$((failures + 1))
fi

# Unexpected early halt: any RISCV_TRAP_HALTED reason that is not the
# expected idle terminal state is an unexpected halt.
if rg -n "RISCV_TRAP_HALTED reason=" "$LOGFILE" >/dev/null 2>&1; then
  if rg -nv "RISCV_TRAP_HALTED reason=kernel_idle_awaiting_io" "$LOGFILE" 2>/dev/null \
      | rg -n "RISCV_TRAP_HALTED reason=" >/dev/null 2>&1; then
    echo "[fail] unexpected RISCV_TRAP_HALTED reason"
    failures=$((failures + 1))
  fi
fi

if (( QEMU_SMP >= 2 )); then
  if ! rg -nE "RISCV_SECONDARY_HART_PARK hart=" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] --smp ${QEMU_SMP} requires RISCV_SECONDARY_HART_PARK hart=N"
    failures=$((failures + 1))
  fi
fi

# Topology assertions: YARM must report present_cpus matching --smp N, and the
# present_bitmap must be the contiguous 0..N-1 mask for QEMU virt. online_cpus
# remains 1 until RISC-V SMP scheduling is implemented.
expected_bitmap_hex=""
case "$QEMU_SMP" in
  1) expected_bitmap_hex="0x1" ;;
  2) expected_bitmap_hex="0x3" ;;
  3) expected_bitmap_hex="0x7" ;;
  4) expected_bitmap_hex="0xf" ;;
esac
if [[ -n "$expected_bitmap_hex" ]]; then
  if ! rg -n "YARM_BOOT_OK present_cpus=${QEMU_SMP} present_bitmap=${expected_bitmap_hex} online_cpus=1" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] YARM_BOOT_OK must report present_cpus=${QEMU_SMP} present_bitmap=${expected_bitmap_hex} online_cpus=1"
    failures=$((failures + 1))
  fi
fi

# Boot hart must not be parked: the RISCV_BOOT_HART_SELECTED hart=N and the
# RISCV_SECONDARY_HART_PARK hart=N lines must NOT share the same hart-id.
boot_hart=$(rg -n "RISCV_BOOT_HART_SELECTED hart=" "$LOGFILE" 2>/dev/null \
  | head -n1 | sed -E 's/.*hart=([0-9]+).*/\1/')
if [[ -n "$boot_hart" ]]; then
  if rg -n "RISCV_SECONDARY_HART_PARK hart=${boot_hart}\b" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] boot hart ${boot_hart} appears in RISCV_SECONDARY_HART_PARK list"
    failures=$((failures + 1))
  fi
fi

# Scheduler-online breadcrumb: always required (RISC-V SMP scheduling is off).
if ! rg -n "RISCV_SCHEDULER_BSP_ONLY online_cpus=1 reason=riscv_smp_scheduler_not_enabled" "$LOGFILE" >/dev/null 2>&1; then
  echo "[fail] RISCV_SCHEDULER_BSP_ONLY breadcrumb missing"
  failures=$((failures + 1))
fi

if (( failures > 0 )); then
  echo "[fail] qemu-riscv64-core-smoke: ${failures} check(s) failed (qemu_status=${QEMU_STATUS})"
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

echo "[ok] qemu-riscv64-core-smoke passed (smp=${QEMU_SMP}, qemu_status=${QEMU_STATUS})"
exit 0
