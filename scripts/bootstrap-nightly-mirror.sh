#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Bootstraps nightly + rust-src for build-std workflows, then delegates to
# scripts/build-x86_64-none-bootstrap.sh.
#
# In environments where rustup is unavailable (e.g. some Android/Termux setups),
# this script can fall back to host toolchain mode by exporting RUSTUP_DISABLED=1
# and requiring cargo/rustc/rust-src to already be present on PATH.

DIST_SERVER_DEFAULT="https://static.rust-lang.org"
UPDATE_ROOT_DEFAULT="${DIST_SERVER_DEFAULT}/rustup"
BOOTSTRAP_SCRIPT_DEFAULT="scripts/build-x86_64-none-bootstrap.sh"

DIST_SERVER="${RUSTUP_DIST_SERVER:-$DIST_SERVER_DEFAULT}"
UPDATE_ROOT="${RUSTUP_UPDATE_ROOT:-$UPDATE_ROOT_DEFAULT}"
BOOTSTRAP_SCRIPT="${BOOTSTRAP_SCRIPT:-$BOOTSTRAP_SCRIPT_DEFAULT}"
TOOLCHAIN="${TOOLCHAIN:-nightly}"
SKIP_NET_CHECK="${SKIP_NET_CHECK:-0}"
RUSTUP_DISABLED="${RUSTUP_DISABLED:-0}"

usage() {
  cat <<USAGE
Usage: $(basename "$0") [--dist-server URL] [--update-root URL] [--toolchain nightly] [--skip-net-check] [--no-rustup]

Environment overrides:
  RUSTUP_DIST_SERVER   Rust distribution endpoint (default: ${DIST_SERVER_DEFAULT})
  RUSTUP_UPDATE_ROOT   Rustup metadata endpoint (default: ${UPDATE_ROOT_DEFAULT})
  TOOLCHAIN            Toolchain name (default: nightly)
  BOOTSTRAP_SCRIPT     Delegate script path (default: ${BOOTSTRAP_SCRIPT_DEFAULT})
  SKIP_NET_CHECK       Set to 1 to skip endpoint reachability checks
  RUSTUP_DISABLED      Set to 1 to skip rustup install/component steps
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dist-server)
      DIST_SERVER="$2"
      shift 2
      ;;
    --update-root)
      UPDATE_ROOT="$2"
      shift 2
      ;;
    --toolchain)
      TOOLCHAIN="$2"
      shift 2
      ;;
    --skip-net-check)
      SKIP_NET_CHECK=1
      shift
      ;;
    --no-rustup)
      RUSTUP_DISABLED=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "[error] unknown argument: $1"
      usage
      exit 1
      ;;
  esac
done

if [[ ! -x "$BOOTSTRAP_SCRIPT" ]]; then
  echo "[error] bootstrap script not found or not executable: $BOOTSTRAP_SCRIPT"
  exit 1
fi

check_url() {
  local url="$1"
  if [[ "$SKIP_NET_CHECK" == "1" ]]; then
    return 0
  fi
  if command -v curl >/dev/null 2>&1; then
    curl -fsSLI --connect-timeout 10 "$url" >/dev/null
  elif command -v wget >/dev/null 2>&1; then
    wget -q --spider --timeout=10 "$url"
  else
    echo "[warn] neither curl nor wget is available; skipping network endpoint checks"
    return 0
  fi
}

echo "[info] rustup dist server: ${DIST_SERVER}"
echo "[info] rustup update root: ${UPDATE_ROOT}"

if [[ "$RUSTUP_DISABLED" == "0" ]] && ! command -v rustup >/dev/null 2>&1; then
  echo "[warn] rustup not found; falling back to host toolchain mode"
  echo "[hint] install rustup for managed nightly installs, or continue with preinstalled cargo/rustc"
  RUSTUP_DISABLED=1
fi

if [[ "$RUSTUP_DISABLED" == "0" ]]; then
  check_url "${DIST_SERVER}/dist/channel-rust-nightly.toml"
  check_url "${UPDATE_ROOT}/release-stable.toml"

  echo "[info] installing toolchain '${TOOLCHAIN}'"
  RUSTUP_DIST_SERVER="$DIST_SERVER" RUSTUP_UPDATE_ROOT="$UPDATE_ROOT" \
    rustup toolchain install "$TOOLCHAIN"

  echo "[info] installing rust-src for '${TOOLCHAIN}'"
  RUSTUP_DIST_SERVER="$DIST_SERVER" RUSTUP_UPDATE_ROOT="$UPDATE_ROOT" \
    rustup component add rust-src --toolchain "$TOOLCHAIN"
else
  echo "[info] rustup-disabled mode: using cargo/rustc from PATH"
fi

echo "[info] delegating to ${BOOTSTRAP_SCRIPT}"
RUSTUP_DIST_SERVER="$DIST_SERVER" \
RUSTUP_UPDATE_ROOT="$UPDATE_ROOT" \
RUSTUP_TOOLCHAIN="$TOOLCHAIN" \
TOOLCHAIN="$TOOLCHAIN" \
RUSTUP_DISABLED="$RUSTUP_DISABLED" \
"$BOOTSTRAP_SCRIPT"
