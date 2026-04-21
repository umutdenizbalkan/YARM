#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

export RUST_MIN_STACK=${RUST_MIN_STACK:-33554432}

if [[ "${1:-}" == "--fs-runtime-entrypoint" ]]; then
  cargo test -q -p yarm-fs-servers fs_server_bin_parity_guard_covers_expected_entrypoints
  cargo check -q -p yarm-fs-servers --bins
  echo "[ok] fs runtime-entrypoint boundary checks passed"
  exit 0
fi

python3 scripts/check-crate-graph-boundary.py
bash scripts/check-service-arch-boundary.sh
bash scripts/check-boundary-milestone-freeze.sh
bash scripts/check-tid-allocation-policy.sh

# Structural compile checks for extracted server packages.
for pkg in \
  yarm-control-plane-servers \
  yarm-fs-servers \
  yarm-network-servers \
  yarm-driver-servers \
  yarm-ui-servers
  do
  cargo check -q -p "$pkg"
done

cargo check -q -p yarm-compat-servers --features posix-compat

echo "[ok] phase5 boundary gates passed"
