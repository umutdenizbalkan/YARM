#!/usr/bin/env bash
set -euo pipefail

# 1) concrete FS/service types must not be in kernel vfs modules
if rg -n "Ext4|RamFs|DevFs|Initramfs|Fat|BlkCache" src/kernel/vfs.rs src/kernel/vfs_lite.rs >/dev/null; then
  echo "[fail] concrete service names found in kernel vfs modules"
  rg -n "Ext4|RamFs|DevFs|Initramfs|Fat|BlkCache" src/kernel/vfs.rs src/kernel/vfs_lite.rs
  exit 1
fi

# 2) thin *_srv.rs binaries must delegate directly to yarm::services::*::run
bad=0
for f in src/bin/*_srv.rs; do
  [[ -e "$f" ]] || continue
  lines=$(wc -l < "$f" | tr -d ' ')
  if [[ "$lines" -gt 8 ]]; then
    echo "[fail] $f is not thin (>$lines lines)"
    bad=1
  fi
  if ! rg -n "yarm::services::[a-z0-9_]+::run\(\);" "$f" >/dev/null; then
    echo "[fail] $f does not delegate to services::<name>::run()"
    bad=1
  fi
 done

if [[ "$bad" -ne 0 ]]; then
  exit 1
fi

echo "[ok] service/kernel architecture boundary checks passed"
