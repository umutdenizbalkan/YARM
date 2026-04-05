#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

server_crates=(
  crates/yarm-control-plane-servers
  crates/yarm-fs-servers
  crates/yarm-network-servers
  crates/yarm-driver-servers
  crates/yarm-ui-servers
  crates/yarm-compat-servers
)

bad=0
for crate_dir in "${server_crates[@]}"; do
  manifest="$crate_dir/Cargo.toml"
  if ! rg -n '^yarm-server-runtime\s*=\s*\{' "$manifest" >/dev/null; then
    echo "[fail] missing yarm-server-runtime dependency: $manifest"
    bad=1
  fi

  if rg -n '^yarm\s*=\s*\{' "$manifest" >/dev/null; then
    echo "[fail] direct yarm dependency found in extracted server crate: $manifest"
    rg -n '^yarm\s*=\s*\{' "$manifest"
    bad=1
  fi
done

runtime_manifest="crates/yarm-server-runtime/Cargo.toml"
if ! rg -n '^yarm\s*=\s*\{' "$runtime_manifest" >/dev/null; then
  echo "[fail] yarm-server-runtime must depend on root yarm runtime facade"
  bad=1
fi

if [[ "$bad" -ne 0 ]]; then
  exit 1
fi

echo "[ok] extracted server crate dependency wiring checks passed"
