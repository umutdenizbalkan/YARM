#!/usr/bin/env bash
set -euo pipefail

TARGET_SPEC=${TARGET_SPEC:-targets/x86_64-yarm-none.json}
PROFILE=${PROFILE:-x86-none}
TOOLCHAIN=${TOOLCHAIN:-nightly}

RUSTUP_TOOLCHAIN=${RUSTUP_TOOLCHAIN:-$TOOLCHAIN}
BUILD_STD_COMPONENTS=${BUILD_STD_COMPONENTS:-core,alloc,compiler_builtins,panic_abort}
BOOTSTRAP_FEATURE_ARGS=${BOOTSTRAP_FEATURE_ARGS:---no-default-features}

RUST_SYSROOT=${RUST_SYSROOT:-$(rustup run "${RUSTUP_TOOLCHAIN}" rustc --print sysroot 2>/dev/null || true)}
RUST_SRC_DIR=${RUST_SRC_DIR:-${RUST_SYSROOT}/lib/rustlib/src/rust}

if [[ ! -f "$TARGET_SPEC" ]]; then
  echo "[error] missing target spec: $TARGET_SPEC"
  exit 1
fi

if ! rustup toolchain list | rg -q "^${TOOLCHAIN}"; then
  echo "[warn] toolchain '${TOOLCHAIN}' is not installed"
  echo "[hint] run: rustup toolchain install ${TOOLCHAIN}"
  exit 2
fi

if [[ -z "$RUST_SYSROOT" ]]; then
  echo "[warn] unable to resolve sysroot for toolchain: ${RUSTUP_TOOLCHAIN}"
  echo "[hint] run: rustup toolchain install ${RUSTUP_TOOLCHAIN}"
  exit 2
fi

if [[ ! -d "$RUST_SRC_DIR" ]]; then
  echo "[warn] rust-src is not installed for toolchain: ${RUSTUP_TOOLCHAIN}"
  echo "[hint] run: rustup component add rust-src --toolchain ${RUSTUP_TOOLCHAIN}"
  echo "[hint] then re-run this script to build core/alloc for custom target"
  echo "[debug] looked for rust-src under: ${RUST_SRC_DIR}"
  exit 2
fi

echo "[info] building kernel_boot + init_server for ${TARGET_SPEC} with build-std=${BUILD_STD_COMPONENTS} + json target spec"
cargo +"${TOOLCHAIN}" build \
  -Z build-std=${BUILD_STD_COMPONENTS} \
  -Z json-target-spec \
  --target "$TARGET_SPEC" \
  --profile "$PROFILE" \
  ${BOOTSTRAP_FEATURE_ARGS} \
  --bin kernel_boot \
  --bin init_server

echo "[ok] x86_64-none build completed"
