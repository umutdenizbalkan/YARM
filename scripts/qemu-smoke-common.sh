#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

require_file_or_warn() {
  local path="$1"
  local strict="${2:-0}"
  local label="${3:-file}"
  if [[ -f "$path" ]]; then
    return 0
  fi
  echo "[warn] ${label} missing: $path"
  [[ "$strict" == "1" ]] && exit 1
  exit 0
}

require_qemu_or_warn() {
  local qemu_bin="$1"
  local strict="${2:-0}"
  if command -v "$qemu_bin" >/dev/null 2>&1; then
    return 0
  fi
  echo "[warn] ${qemu_bin} not installed"
  [[ "$strict" == "1" ]] && exit 1
  exit 0
}

run_qemu_timeout_to_log() {
  local timeout_secs="$1"
  local logfile="$2"
  shift 2
  rm -f "$logfile"
  set +e
  timeout "$timeout_secs" "$@" | tee "$logfile"
  local status=$?
  set -e
  return "$status"
}

check_common_boot_markers() {
  local logfile="$1"
  local marker_regex="$2"
  local init_regex="$3"
  if rg -n "$marker_regex" "$logfile" >/dev/null 2>&1 \
    && rg -n "$init_regex" "$logfile" >/dev/null 2>&1; then
    echo "[ok] boot shell and init-server markers detected"
    return 0
  fi
  return 1
}
