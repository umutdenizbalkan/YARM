#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

RUST_TARGET=${RUST_TARGET:-aarch64-unknown-none}
PROFILE=${PROFILE:-aarch64-none}
TOOLCHAIN=${TOOLCHAIN:-nightly}
RUSTUP_DISABLED=${RUSTUP_DISABLED:-0}
BUILD_STD_COMPONENTS=${BUILD_STD_COMPONENTS:-core,alloc,compiler_builtins,panic_abort}
BOOTSTRAP_FEATURE_ARGS=${BOOTSTRAP_FEATURE_ARGS:---no-default-features}

if [[ "$RUSTUP_DISABLED" == "0" ]] && ! command -v rustup >/dev/null 2>&1; then
  echo "[warn] rustup not found; switching to host toolchain mode (RUSTUP_DISABLED=1)"
  RUSTUP_DISABLED=1
fi

if [[ "$RUSTUP_DISABLED" == "1" ]]; then
  CARGO_CMD=(cargo)
  TOOLCHAIN_LABEL="host"
else
  if ! rustup toolchain list | rg -q "^${TOOLCHAIN}"; then
    echo "[warn] toolchain '${TOOLCHAIN}' is not installed"
    echo "[hint] run: rustup toolchain install ${TOOLCHAIN}"
    exit 2
  fi
  rustup target add "$RUST_TARGET" --toolchain "$TOOLCHAIN" >/dev/null 2>&1 || true
  CARGO_CMD=(cargo +"${TOOLCHAIN}")
  TOOLCHAIN_LABEL="$TOOLCHAIN"
fi

echo "[info] building kernel_boot for ${RUST_TARGET} profile=${PROFILE} toolchain=${TOOLCHAIN_LABEL}"
"${CARGO_CMD[@]}" build \
  -Z build-std=${BUILD_STD_COMPONENTS} \
  --target "$RUST_TARGET" \
  --profile "$PROFILE" \
  ${BOOTSTRAP_FEATURE_ARGS} \
  -p yarm \
  --bin kernel_boot

echo "[info] building init_server for ${RUST_TARGET} profile=${PROFILE}"
"${CARGO_CMD[@]}" build \
  -Z build-std=${BUILD_STD_COMPONENTS} \
  --target "$RUST_TARGET" \
  --profile "$PROFILE" \
  ${BOOTSTRAP_FEATURE_ARGS} \
  -p yarm-control-plane-servers \
  --bin init_server

echo "[ok] aarch64-none bootstrap build completed"
echo "[next] stage qemu artifacts: scripts/build-qemu-aarch64-artifacts.sh"
echo "[next] run smoke boot: scripts/qemu-aarch64-core-smoke.sh"
