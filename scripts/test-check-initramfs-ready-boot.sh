#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPT="$ROOT/scripts/check-initramfs-ready-boot.sh"

"$SCRIPT" --check-log "$ROOT/scripts/testdata/initramfs-ready-pass.log"

if "$SCRIPT" --check-log "$ROOT/scripts/testdata/initramfs-ready-missing-ready-send.log"; then
  echo "[fail] expected missing-ready-send case to fail"
  exit 1
fi

if "$SCRIPT" --check-log "$ROOT/scripts/testdata/initramfs-ready-out-of-order.log"; then
  echo "[fail] expected out-of-order case to fail"
  exit 1
fi

echo "[ok] check-initramfs-ready-boot self-test passed"
