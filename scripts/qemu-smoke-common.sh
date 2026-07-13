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

# Stage 198A1: deterministic idle-aware QEMU completion. Runs QEMU capturing the serial to
# `logfile`, and terminates it PROMPTLY once the canonical terminal-idle marker appears — instead
# of always waiting out the wall-clock timeout (a WFI-idling kernel never self-exits, so the plain
# timeout run always burns the full budget and returns 124). Returns:
#   0   — the terminal-idle marker was observed (clean quiescent boot); QEMU was killed.
#   124 — the marker never appeared within `max_secs` (an early silent hang / stuck boot).
# The caller still runs its full positive/forbidden marker verdict on `logfile`, so a boot that
# idles BEFORE the required proof markers fired still fails (its markers are simply absent).
run_qemu_until_idle_or_timeout() {
  local max_secs="$1"
  local logfile="$2"
  local idle_marker="$3"
  shift 3
  rm -f "$logfile"
  : >"$logfile"
  set +e
  "$@" >"$logfile" 2>&1 &
  local qpid=$!
  local waited=0
  local status=124
  while :; do
    if ! kill -0 "$qpid" 2>/dev/null; then
      # QEMU exited on its own (e.g. a fatal kernel halt wrote the log then stopped).
      status=0
      break
    fi
    if rg -a -q -- "$idle_marker" "$logfile" 2>/dev/null; then
      # Terminal idle reached — grace window to capture any trailing serial, then stop QEMU.
      sleep 2
      status=0
      break
    fi
    if [[ "$waited" -ge "$max_secs" ]]; then
      status=124
      break
    fi
    sleep 1
    waited=$((waited + 1))
  done
  kill "$qpid" 2>/dev/null
  sleep 1
  kill -9 "$qpid" 2>/dev/null
  wait "$qpid" 2>/dev/null
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

check_log_sequence() {
  local logfile="$1"
  shift
  local last_line=0
  local pattern=""
  local line=0
  for pattern in "$@"; do
    line=$(rg -n -m 1 "$pattern" "$logfile" | cut -d: -f1 | head -n1 || true)
    if [[ -z "$line" ]]; then
      return 1
    fi
    if (( line <= last_line )); then
      return 1
    fi
    last_line=$line
  done
  return 0
}

check_required_patterns() {
  local logfile="$1"
  shift
  local pattern=""
  for pattern in "$@"; do
    if ! rg -n "$pattern" "$logfile" >/dev/null 2>&1; then
      echo "[warn] required pattern missing: $pattern"
      return 1
    fi
  done
  return 0
}

# ---------------------------------------------------------------------------
# Stage 180 (CI-PROFILES): shared fatal-marker policy helpers.
#
# Additive only — existing scripts keep their own inline gates; these give the
# unified profile runner (and any new script) a single source of truth for the
# fatal-marker taxonomy documented in doc/KERNEL_UNLOCKING.md §7.1.16.
#
# A generic PAGE_FAULT is NOT fatal: a handled COW/DEMAND fault emits many benign
# PAGE_FAULT_* diagnostics (ENTRY / HW_REGS / FRAME_WORDS / FRAME_DECODE /
# HW_PTE_WALK / RAW / X86_ERROR / CR3_COMPARE) before HANDLED_COW/HANDLED_DEMAND.
# Only the EXPLICIT unhandled/fatal markers are fatal.
# ---------------------------------------------------------------------------

# Returns 0 (true) if the log contains a hard crash breadcrumb.
log_has_fatal_breadcrumb() {
  local logfile="$1"
  [[ -f "$logfile" ]] || return 1
  local tail
  tail="$(tr '\r' '\n' <"$logfile")"
  local pat
  for pat in '^!Fv' '^!BNv' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
    if printf '%s\n' "$tail" | rg -a -q -- "$pat"; then
      return 0
    fi
  done
  return 1
}

# Returns 0 (true) if the log contains an EXPLICIT unhandled/fatal page fault.
# Generic + handled PAGE_FAULT_* diagnostics do NOT match.
log_has_unhandled_page_fault() {
  local logfile="$1"
  [[ -f "$logfile" ]] || return 1
  local tail
  tail="$(tr '\r' '\n' <"$logfile")"
  local pat
  for pat in 'PAGE_FAULT_UNHANDLED' 'PAGE_FAULT_FATAL' 'PAGE_FAULT_NOT_HANDLED'; do
    if printf '%s\n' "$tail" | rg -a -F -q -- "$pat"; then
      return 0
    fi
  done
  return 1
}

# Returns 0 (true) if the log contains any of the profile-specific / common
# failure markers passed as arguments (fixed-string match, binary-safe).
log_has_profile_failure() {
  local logfile="$1"
  shift
  [[ -f "$logfile" ]] || return 1
  local tail
  tail="$(tr '\r' '\n' <"$logfile")"
  local marker
  for marker in "$@"; do
    if printf '%s\n' "$tail" | rg -a -F -q -- "$marker"; then
      return 0
    fi
  done
  return 1
}
