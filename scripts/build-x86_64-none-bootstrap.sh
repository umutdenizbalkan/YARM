#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

TARGET_SPEC=${TARGET_SPEC:-targets/x86_64-yarm-none.json}
PROFILE=${PROFILE:-x86-none}
TOOLCHAIN=${TOOLCHAIN:-nightly}
RUSTUP_DISABLED=${RUSTUP_DISABLED:-0}

RUSTUP_TOOLCHAIN=${RUSTUP_TOOLCHAIN:-$TOOLCHAIN}
BUILD_STD_COMPONENTS=${BUILD_STD_COMPONENTS:-core,alloc,compiler_builtins,panic_abort}
BOOTSTRAP_FEATURE_ARGS=${BOOTSTRAP_FEATURE_ARGS:---no-default-features}

if [[ ! -f "$TARGET_SPEC" ]]; then
  echo "[error] missing target spec: $TARGET_SPEC"
  exit 1
fi

if [[ "$RUSTUP_DISABLED" == "0" ]] && ! command -v rustup >/dev/null 2>&1; then
  echo "[warn] rustup not found; switching to host toolchain mode (RUSTUP_DISABLED=1)"
  RUSTUP_DISABLED=1
fi

if [[ "$RUSTUP_DISABLED" == "1" ]]; then
  if ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
    echo "[error] cargo/rustc must be available in PATH when RUSTUP_DISABLED=1"
    exit 2
  fi
  RUST_SYSROOT=${RUST_SYSROOT:-$(rustc --print sysroot 2>/dev/null || true)}
  CARGO_CMD=(cargo)
  TOOLCHAIN_LABEL="host"
  RUSTC_FOR_CHECK=${RUSTC_FOR_CHECK:-$(command -v rustc)}
else
  if ! rustup toolchain list | rg -q "^${TOOLCHAIN}"; then
    echo "[warn] toolchain '${TOOLCHAIN}' is not installed"
    echo "[hint] run: rustup toolchain install ${TOOLCHAIN}"
    exit 2
  fi
  RUST_SYSROOT=${RUST_SYSROOT:-$(rustup run "${RUSTUP_TOOLCHAIN}" rustc --print sysroot 2>/dev/null || true)}
  CARGO_CMD=(cargo +"${TOOLCHAIN}")
  TOOLCHAIN_LABEL="${TOOLCHAIN}"
  RUSTC_FOR_CHECK=${RUSTC_FOR_CHECK:-$(rustup which --toolchain "${RUSTUP_TOOLCHAIN}" rustc 2>/dev/null || true)}
fi

if [[ -z "$RUST_SYSROOT" ]]; then
  echo "[warn] unable to resolve sysroot for toolchain: ${RUSTUP_TOOLCHAIN}"
  echo "[hint] if rustup is unavailable, export RUSTUP_DISABLED=1 and ensure rustc is in PATH"
  exit 2
fi

RUST_SRC_DIR=${RUST_SRC_DIR:-${RUST_SYSROOT}/lib/rustlib/src/rust}
if [[ ! -d "$RUST_SRC_DIR" ]]; then
  echo "[warn] rust-src is not installed for selected toolchain"
  if [[ "$RUSTUP_DISABLED" == "0" ]]; then
    echo "[hint] run: rustup component add rust-src --toolchain ${RUSTUP_TOOLCHAIN}"
  else
    echo "[hint] install rust-src for your host toolchain/package manager"
  fi
  echo "[debug] looked for rust-src under: ${RUST_SRC_DIR}"
  exit 2
fi

echo "[info] building kernel_boot + init_server for ${TARGET_SPEC} with toolchain=${TOOLCHAIN_LABEL}, build-std=${BUILD_STD_COMPONENTS}"
"${CARGO_CMD[@]}" build \
  -Z build-std=${BUILD_STD_COMPONENTS} \
  -Z json-target-spec \
  --target "$TARGET_SPEC" \
  --profile "$PROFILE" \
  ${BOOTSTRAP_FEATURE_ARGS} \
  --bin kernel_boot \
  --bin init_server

KERNEL_ELF_PATH="target/x86_64-yarm-none/${PROFILE}/kernel_boot"
echo "[info] x86_64 bootstrap invariant script removed; skipping legacy invariant check stage"

echo "[ok] x86_64-none build completed"
echo "[next] stage qemu artifacts: scripts/build-qemu-x86_64-artifacts.sh"
echo "[next] run smoke boot (core markers): scripts/qemu-x86_64-core-smoke.sh"
