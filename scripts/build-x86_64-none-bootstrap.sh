#!/usr/bin/env bash
set -euo pipefail

TARGET_SPEC=${TARGET_SPEC:-targets/x86_64-yarm-none.json}
PROFILE=${PROFILE:-x86-none}
TOOLCHAIN=${TOOLCHAIN:-nightly}

RUSTUP_TOOLCHAIN=${RUSTUP_TOOLCHAIN:-$TOOLCHAIN}
RUST_SRC_DIR=${RUST_SRC_DIR:-$HOME/.rustup/toolchains/${RUSTUP_TOOLCHAIN}/lib/rustlib/src/rust}

if [[ ! -f "$TARGET_SPEC" ]]; then
  echo "[error] missing target spec: $TARGET_SPEC"
  exit 1
fi

if ! rustup toolchain list | rg -q "^${TOOLCHAIN}"; then
  echo "[warn] toolchain '${TOOLCHAIN}' is not installed"
  echo "[hint] run: rustup toolchain install ${TOOLCHAIN}"
  exit 2
fi

if [[ ! -d "$RUST_SRC_DIR" ]]; then
  echo "[warn] rust-src is not installed for toolchain: ${RUSTUP_TOOLCHAIN}"
  echo "[hint] run: rustup component add rust-src --toolchain ${RUSTUP_TOOLCHAIN}"
  echo "[hint] then re-run this script to build std/core for custom target"
  exit 2
fi

echo "[info] building kernel_boot + init_server for ${TARGET_SPEC} with build-std + json target spec"
cargo +"${TOOLCHAIN}" build \
  -Z build-std=core,alloc,std,panic_abort \
  -Z json-target-spec \
  --target "$TARGET_SPEC" \
  --profile "$PROFILE" \
  --bin kernel_boot \
  --bin init_server

echo "[ok] x86_64-none build completed"
