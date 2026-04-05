#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

mapfile -t server_bin_files < <(
  {
    rg --files src/bin -g '*_srv.rs' 2>/dev/null || true
    rg --files crates -g '*/src/bin/*_srv.rs' 2>/dev/null || true
  } | sort -u
)

# 1) concrete FS/service types must not leak into the kernel VFS layer or control-plane production shim code.
concrete_re="Ext4|RamFs|DevFs|Initramfs|Fat|BlkCache"
if rg -n "$concrete_re" src/kernel/vfs.rs >/dev/null; then
  echo "[fail] concrete service names found in kernel VFS layer"
  rg -n "$concrete_re" src/kernel/vfs.rs
  exit 1
fi

# allow explicit backend names in control-plane test scaffolding, but not in production path.
if awk '/#\[cfg\(test\)\]/{exit} {print}' src/services/control_plane/vfs/service.rs | rg -n "$concrete_re" >/dev/null; then
  echo "[fail] concrete service names found in control-plane VFS production shim"
  awk '/#\[cfg\(test\)\]/{exit} {print}' src/services/control_plane/vfs/service.rs | rg -n "$concrete_re"
  exit 1
fi

# 2) enforce service domain layout (no legacy flat service directories)
allowed='^(common|compatibility|control_plane|drivers|fs|init|network|ui)$'
for d in src/services/*; do
  [[ -d "$d" ]] || continue
  base=$(basename "$d")
  if ! [[ "$base" =~ $allowed ]]; then
    echo "[fail] legacy/non-domain service directory found: $d"
    exit 1
  fi
done

# 3) thin *_srv.rs binaries must delegate directly to yarm-server-runtime wrappers.
bad=0
for f in "${server_bin_files[@]}"; do
  [[ -e "$f" ]] || continue
  lines=$(wc -l < "$f" | tr -d ' ')
  if [[ "$lines" -gt 8 ]]; then
    echo "[fail] $f is not thin (>$lines lines)"
    bad=1
  fi
  if ! rg -n "yarm_server_runtime::[a-z0-9_:]+\(\);" "$f" >/dev/null; then
    echo "[fail] $f does not delegate to yarm-server-runtime entry wrapper"
    bad=1
  fi
 done

# 3b) root package should own only kernel bootstrap binaries.
if rg -n 'name\s*=\s*"(.*_srv|driver_manager|console_driver|core_profile_smoke)"' Cargo.toml >/dev/null; then
  echo "[fail] root Cargo.toml still owns non-kernel server/runtime bins"
  rg -n 'name\s*=\s*"(.*_srv|driver_manager|console_driver|core_profile_smoke)"' Cargo.toml
  bad=1
fi

# 4) prevent boundary creep for high-risk kernel-only types.
#    Existing compatibility/control-plane shims are temporarily allow-listed.
deny_re='kernel::(trapframe::TrapFrame|boot::KernelState)'
while IFS=: read -r path line rest; do
  [[ -z "${path:-}" ]] && continue
  case "$path" in
    src/services/compatibility/posix_compat/*|src/services/control_plane/vfs/service.rs)
      ;;
    *)
      echo "[fail] kernel-only boundary type imported outside allow-list: $path:$line:$rest"
      bad=1
      ;;
  esac
done < <(rg -n "$deny_re" src/services "${server_bin_files[@]}" || true)

if [[ "$bad" -ne 0 ]]; then
  exit 1
fi

echo "[ok] service/kernel architecture boundary checks passed"
