#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

mapfile -t server_bin_files < <(
  rg --files crates src -g '*_srv.rs' 2>/dev/null \
    | rg '(^|/)src/bin/[^/]+_srv\.rs$' \
    | sort -u
)

bad=0

# 1) concrete FS/service types must not leak into the kernel VFS layer or control-plane production shim code.
concrete_re="Ext4|RamFs|DevFs|Initramfs|Fat|BlkCache"
kernel_vfs_paths=(
  src/kernel/vfs.rs
  crates/yarm-kernel/src/kernel/vfs.rs
)
for kernel_vfs in "${kernel_vfs_paths[@]}"; do
  [[ -e "$kernel_vfs" ]] || continue
  if rg -n "$concrete_re" "$kernel_vfs" >/dev/null; then
    echo "[fail] concrete service names found in kernel VFS layer: $kernel_vfs"
    rg -n "$concrete_re" "$kernel_vfs"
    bad=1
  fi
done

control_plane_vfs_paths=(
  src/services/control_plane/vfs/service.rs
  crates/yarm-control-plane-servers/src/control_plane/vfs/service.rs
)
for control_plane_vfs in "${control_plane_vfs_paths[@]}"; do
  [[ -e "$control_plane_vfs" ]] || continue
  # allow explicit backend names in control-plane test scaffolding, but not in production path.
  if awk '/#\[cfg\(test\)\]/{exit} {print}' "$control_plane_vfs" | rg -n "$concrete_re" >/dev/null; then
    echo "[fail] concrete service names found in control-plane VFS production shim: $control_plane_vfs"
    awk '/#\[cfg\(test\)\]/{exit} {print}' "$control_plane_vfs" | rg -n "$concrete_re"
    bad=1
  fi
done

# 2) enforce legacy in-tree service domain layout when it exists (extracted service crates are checked below).
allowed='^(common|compatibility|control_plane|drivers|fs|init|network|ui)$'
if [[ -d src/services ]]; then
  for d in src/services/*; do
    [[ -d "$d" ]] || continue
    base=$(basename "$d")
    if ! [[ "$base" =~ $allowed ]]; then
      echo "[fail] legacy/non-domain service directory found: $d"
      bad=1
    fi
  done
fi

# 3) service binary boundary: bins may contain runtime/entry glue, but not direct arch/kernel/syscall ABI definitions.
if [[ "${#server_bin_files[@]}" -eq 0 ]]; then
  echo "[fail] no service binaries found under */src/bin/*_srv.rs"
  bad=1
else
  if ! python3 - "${server_bin_files[@]}" <<'PY'
import re
import sys
from pathlib import Path

files = [Path(arg) for arg in sys.argv[1:]]
violations: list[tuple[str, int, str, str]] = []

def strip_comments_and_literals(src: str) -> str:
    out: list[str] = []
    i = 0
    n = len(src)
    state = "code"
    block_depth = 0
    raw_hashes = 0
    while i < n:
        ch = src[i]
        nxt = src[i + 1] if i + 1 < n else ""
        if state == "code":
            if ch == "/" and nxt == "/":
                state = "line_comment"
                out.extend("  ")
                i += 2
                continue
            if ch == "/" and nxt == "*":
                state = "block_comment"
                block_depth = 1
                out.extend("  ")
                i += 2
                continue
            raw = re.match(r'br(#+)?"|r(#+)?"', src[i:])
            if raw:
                raw_hashes = len(raw.group(1) or raw.group(2) or "")
                out.extend(" " * len(raw.group(0)))
                i += len(raw.group(0))
                state = "raw_string"
                continue
            if (ch == "b" and nxt == '"') or ch == '"':
                width = 2 if ch == "b" and nxt == '"' else 1
                out.extend(" " * width)
                i += width
                state = "string"
                continue
            if (ch == "b" and nxt == "'") or ch == "'":
                width = 2 if ch == "b" and nxt == "'" else 1
                out.extend(" " * width)
                i += width
                state = "char"
                continue
            out.append(ch)
            i += 1
            continue
        if state == "line_comment":
            if ch == "\n":
                out.append("\n")
                state = "code"
            else:
                out.append(" ")
            i += 1
            continue
        if state == "block_comment":
            if ch == "/" and nxt == "*":
                block_depth += 1
                out.extend("  ")
                i += 2
                continue
            if ch == "*" and nxt == "/":
                block_depth -= 1
                out.extend("  ")
                i += 2
                if block_depth == 0:
                    state = "code"
                continue
            out.append("\n" if ch == "\n" else " ")
            i += 1
            continue
        if state == "string":
            if ch == "\\":
                out.append(" ")
                if i + 1 < n:
                    out.append("\n" if src[i + 1] == "\n" else " ")
                    i += 2
                else:
                    i += 1
                continue
            out.append("\n" if ch == "\n" else " ")
            i += 1
            if ch == '"':
                state = "code"
            continue
        if state == "char":
            if ch == "\\":
                out.append(" ")
                if i + 1 < n:
                    out.append("\n" if src[i + 1] == "\n" else " ")
                    i += 2
                else:
                    i += 1
                continue
            out.append("\n" if ch == "\n" else " ")
            i += 1
            if ch == "'":
                state = "code"
            continue
        if state == "raw_string":
            out.append("\n" if ch == "\n" else " ")
            if ch == '"' and src.startswith("#" * raw_hashes, i + 1):
                out.extend(" " * raw_hashes)
                i += 1 + raw_hashes
                state = "code"
            else:
                i += 1
            continue
    return "".join(out)

checks: list[tuple[re.Pattern[str], str]] = [
    (re.compile(r"\bcrate\s*::\s*arch\b"), "direct crate::arch reference"),
    (re.compile(r"\bsrc/arch\b"), "direct src/arch reference"),
    (re.compile(r"\byarm_kernel\b"), "direct yarm_kernel dependency/reference"),
    (re.compile(r"\b(?:crate|super|self|src|yarm)\s*::\s*kernel\b"), "kernel-internal import/reference"),
    (re.compile(r"(?<![A-Za-z0-9_])(?:aarch64|x86_64|riscv64)(?=\s*::|\b)"), "architecture-specific reference"),
    (re.compile(r"^\s*(?:pub\s+)?(?:const|static)\s+(?:SYS_|SYSCALL_|[A-Z0-9_]*_SYSCALL(?:_[A-Z0-9_]*)?|[A-Z0-9_]*_SYS(?:_[A-Z0-9_]*)?)\b", re.M), "direct syscall ABI constant redefinition"),
    (re.compile(r"^\s*(?:pub\s+)?(?:enum|struct|union|trait)\s+Syscall\b|^\s*(?:pub\s+)?mod\s+syscall_abi\b", re.M), "direct syscall ABI type/module redefinition"),
    (re.compile(r"^\s*(?:pub\s+)?(?:struct|enum|union|trait)\s+(?!PanicInfo\b)\w+", re.M), "service-domain type definition in bin instead of library"),
    (re.compile(r"^\s*impl(?:\s*<[^>]+>)?\s+(?!core::panic::PanicInfo\b)\w", re.M), "service-domain impl block in bin instead of library"),
]

delegates_to_runtime_or_service = re.compile(
    r"\b(?:yarm_[a-z0-9_]+_servers|yarm_server_runtime)\s*::\s*[A-Za-z0-9_:]*(?:run_|main_|enter_user_entrypoint)"
)

for path in files:
    src = path.read_text(encoding="utf-8")
    clean = strip_comments_and_literals(src)
    for pattern, reason in checks:
        for match in pattern.finditer(clean):
            line_no = clean.count("\n", 0, match.start()) + 1
            line = src.splitlines()[line_no - 1].strip()
            violations.append((str(path), line_no, reason, line))
    if not delegates_to_runtime_or_service.search(clean):
        violations.append((str(path), 1, "missing delegation to service/runtime entrypoint", ""))

if violations:
    for path, line_no, reason, line in violations:
        suffix = f": {line}" if line else ""
        print(f"[fail] {path}:{line_no}: {reason}{suffix}")
    sys.exit(1)

print(f"[ok] checked {len(files)} service binaries for arch/kernel/syscall boundary violations")
PY
  then
    bad=1
  fi
fi

# 3b) root package should own only kernel bootstrap binaries.
if rg -n 'name\s*=\s*"(.*_srv|driver_manager|console_driver|core_profile_smoke)"' Cargo.toml >/dev/null; then
  echo "[fail] root Cargo.toml still owns non-kernel server/runtime bins"
  rg -n 'name\s*=\s*"(.*_srv|driver_manager|console_driver|core_profile_smoke)"' Cargo.toml
  bad=1
fi

# 4) prevent boundary creep for high-risk kernel-only types.
#    Existing compatibility/control-plane shims are temporarily allow-listed.
deny_re='kernel::(trapframe::TrapFrame|boot::KernelState)'
service_search_roots=()
# Keep this legacy in-tree service sweep scoped to src/services when present.
# Extracted service crates are covered for the requested binary boundary above;
# their broader control-plane/kernel shims need a separate migration plan before
# this deny-list can be applied repository-wide without false positives.
[[ -d src/services ]] && service_search_roots+=(src/services)

if [[ "${#service_search_roots[@]}" -gt 0 || "${#server_bin_files[@]}" -gt 0 ]]; then
  while IFS=: read -r path line rest; do
    [[ -z "${path:-}" ]] && continue
    case "$path" in
      src/services/compatibility/posix_compat/*|src/services/control_plane/vfs/service.rs|\
      crates/yarm-compat-servers/src/posix_compat/*|crates/yarm-control-plane-servers/src/control_plane/vfs/service.rs)
        ;;
      *)
        echo "[fail] kernel-only boundary type imported outside allow-list: $path:$line:$rest"
        bad=1
        ;;
    esac
  done < <(rg -n "$deny_re" "${service_search_roots[@]}" "${server_bin_files[@]}" || true)
fi

if [[ "$bad" -ne 0 ]]; then
  exit 1
fi

echo "[ok] service/kernel architecture boundary checks passed"
