#!/usr/bin/env bash
set -euo pipefail

# 1) concrete FS/service types must not leak into the kernel VFS layer or the control-plane VFS shim
if rg -n "Ext4|RamFs|DevFs|Initramfs|Fat|BlkCache" src/kernel/vfs.rs src/services/control_plane/vfs/service.rs >/dev/null; then
  echo "[fail] concrete service names found in kernel VFS layer/control-plane shim"
  rg -n "Ext4|RamFs|DevFs|Initramfs|Fat|BlkCache" src/kernel/vfs.rs src/services/control_plane/vfs/service.rs
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

# 3) thin *_srv.rs binaries must delegate directly to yarm::services::*::run
bad=0
for f in src/bin/*_srv.rs; do
  [[ -e "$f" ]] || continue
  lines=$(wc -l < "$f" | tr -d ' ')
  if [[ "$lines" -gt 8 ]]; then
    echo "[fail] $f is not thin (>$lines lines)"
    bad=1
  fi
  if ! rg -n "yarm::services::[a-z0-9_:]+::run\(\);" "$f" >/dev/null; then
    echo "[fail] $f does not delegate to services::<name>::run()"
    bad=1
  fi
 done

# 4) prevent boundary creep for high-risk kernel-only types.
#    Existing compatibility/control-plane shims are temporarily allow-listed.
deny_re='kernel::(trapframe::TrapFrame|boot::KernelState)'
while IFS=: read -r path line rest; do
  [[ -z "${path:-}" ]] && continue
  case "$path" in
    src/services/compatibility/linux_compat/*|src/services/control_plane/vfs/service.rs)
      ;;
    *)
      echo "[fail] kernel-only boundary type imported outside allow-list: $path:$line:$rest"
      bad=1
      ;;
  esac
done < <(rg -n "$deny_re" src/services src/bin/*_srv.rs || true)

if [[ "$bad" -ne 0 ]]; then
  exit 1
fi

echo "[ok] service/kernel architecture boundary checks passed"
