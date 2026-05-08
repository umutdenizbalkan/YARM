#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOG="${ROOT}/target/initramfs-ready-boot.log"
KERNEL_IMG="${KERNEL_IMG:-$ROOT/out/x86_64/kernel.elf}"
INITRD_IMG="${INITRD_IMG:-$ROOT/out/x86_64/initramfs.cpio}"
QEMU_BIN="${QEMU_BIN:-qemu-system-x86_64}"
BUILD_CMD="${BUILD_CMD:-scripts/build-qemu-x86_64-artifacts.sh}"
CHECK_ONLY_LOG="${CHECK_ONLY_LOG:-}"

usage() {
  cat <<'EOF'
Usage:
  scripts/check-initramfs-ready-boot.sh
  scripts/check-initramfs-ready-boot.sh --check-log <path>

Modes:
  default         Run x86_64 QEMU boot (20s timeout), then validate marker order.
  --check-log     Validate marker order from an existing log file without QEMU/artifact checks.

Env overrides:
  KERNEL_IMG, INITRD_IMG, QEMU_BIN, BUILD_CMD, CHECK_ONLY_LOG
EOF
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

if [[ "${1:-}" == "--check-log" ]]; then
  if [[ -z "${2:-}" ]]; then
    echo "[fail] --check-log requires a log path"
    usage
    exit 1
  fi
  CHECK_ONLY_LOG="$2"
fi

mkdir -p "$(dirname "$LOG")"
if [[ -z "$CHECK_ONLY_LOG" ]]; then
  rm -f "$LOG"
else
  LOG="$CHECK_ONLY_LOG"
fi

if [[ -z "$CHECK_ONLY_LOG" ]]; then
  if [[ ! -x "$QEMU_BIN" ]] && ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
    echo "[fail] missing QEMU binary: $QEMU_BIN"
    echo "install/provide qemu-system-x86_64, then rerun."
    exit 1
  fi

  if [[ ! -f "$KERNEL_IMG" || ! -f "$INITRD_IMG" ]]; then
    echo "[fail] missing boot artifacts."
    echo "expected:"
    echo "  kernel: $KERNEL_IMG"
    echo "  initrd: $INITRD_IMG"
    echo "build prerequisite: $BUILD_CMD"
    echo "log: $LOG"
    exit 1
  fi

  set +e
  timeout 20s "$QEMU_BIN" \
    -M q35 -m 512M -nographic -serial mon:stdio \
    -kernel "$KERNEL_IMG" -initrd "$INITRD_IMG" \
    >"$LOG" 2>&1
  rc=$?
  set -e

  if [[ $rc -eq 124 ]]; then
    echo "[ok] QEMU run reached timeout window (20s); validating markers from log."
  elif [[ $rc -ne 0 ]]; then
    echo "[warn] QEMU exited with status $rc; validating markers from log anyway."
  fi
else
  if [[ ! -f "$LOG" ]]; then
    echo "[fail] check-log mode: log file does not exist: $LOG"
    exit 1
  fi
  echo "[ok] check-log mode using: $LOG"
fi

markers=(
  INIT_ORCH_CAPS_INSTALLED
  INIT_SPAWN_V5_SEND
  INIT_SPAWN_V5_REPLY_OK
  INITRAMFS_READY_SEND
  INITRAMFS_READY_RECV_OK
  INITRAMFS_SERVICE_READY
)

last=0
for m in "${markers[@]}"; do
  line=$(grep -n "$m" "$LOG" | head -n1 | cut -d: -f1 || true)
  if [[ -z "$line" ]]; then
    echo "[fail] missing marker: $m"
    echo "log: $LOG"
    exit 1
  fi
  if (( line < last )); then
    echo "[fail] out-of-order marker: $m"
    echo "log: $LOG"
    exit 1
  fi
  last=$line
  echo "[ok] $m @ line $line"
done

echo "[ok] marker order validated"
echo "log: $LOG"
