#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
TARGET_SPEC=${TARGET_SPEC:-"$ROOT_DIR/targets/x86_64-yarm-none.json"}
BOOT_RS=${BOOT_RS:-"$ROOT_DIR/src/arch/x86_64/boot.rs"}
KERNEL_ELF=${1:-}

RUSTC_BIN=${RUSTC_BIN:-rustc}

if ! command -v "$RUSTC_BIN" >/dev/null 2>&1; then
  echo "[error] rust compiler not found: $RUSTC_BIN"
  exit 1
fi

if [[ ! -f "$TARGET_SPEC" ]]; then
  echo "[error] missing target spec: $TARGET_SPEC"
  exit 1
fi

if [[ ! -f "$BOOT_RS" ]]; then
  echo "[error] missing boot source: $BOOT_RS"
  exit 1
fi

echo "[info] validating rust target spec parses: $TARGET_SPEC"
if ! target_parse_err=$("$RUSTC_BIN" --print cfg --target "$TARGET_SPEC" >/dev/null 2>&1); then
  echo "[error] rustc failed to parse target spec: $TARGET_SPEC"
  if [[ -n "$target_parse_err" ]]; then
    echo "$target_parse_err"
  fi
  exit 1
fi

echo "[info] validating x86_64 bootstrap mapping invariants in boot.rs"
rg -q "boot_pd_hi" "$BOOT_RS"
rg -q "0xFEC00000" "$BOOT_RS"
rg -q "0xFEE00000" "$BOOT_RS"
rg -q "boot_pdpt \\+ 28" "$BOOT_RS"

if [[ -n "$KERNEL_ELF" ]]; then
  if [[ ! -f "$KERNEL_ELF" ]]; then
    echo "[error] kernel ELF for PVH checks not found: $KERNEL_ELF"
    exit 1
  fi
  if ! command -v readelf >/dev/null 2>&1; then
    echo "[error] readelf is required for PVH note checks"
    exit 1
  fi
  echo "[info] validating PVH note presence in: $KERNEL_ELF"
  if ! readelf -l "$KERNEL_ELF" | rg -q "NOTE"; then
    echo "[error] kernel ELF missing PT_NOTE program header"
    exit 1
  fi
  if ! readelf -n "$KERNEL_ELF" | rg -qi "Xen|PVH"; then
    echo "[error] kernel ELF missing Xen/PVH note metadata"
    exit 1
  fi
fi

echo "[ok] x86_64 bootstrap invariants validated"
