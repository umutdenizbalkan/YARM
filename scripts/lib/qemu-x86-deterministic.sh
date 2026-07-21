#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 199A2D3 — deterministic x86_64 QEMU lifecycle for the direct-IPC SMP smokes.
#
# The SCRIPT (not `timeout`, not an external killer) owns QEMU termination:
#
#   launch fresh QEMU  →  monitor a fresh log  →  wait for ALL terminal proof markers
#   →  scan for fatal conditions  →  terminate QEMU from the script  →  wait for exit
#   →  return success ONLY when the complete terminal condition was observed first.
#
# This closes the Stage 199A2D2C2C gap where QEMU was terminated externally (or by
# `timeout`) and the seal was inferred from whatever the log happened to contain.
#
# Contract of `qemu_run_deterministic`:
#   $1            = log file path (TRUNCATED here, so no marker can predate this run)
#   $2            = per-iteration fatal-scan regex (extended, `rg -e`)
#   $3            = timeout seconds (hard ceiling; a timeout WITHOUT full proof fails)
#   remaining $@  = the QEMU argv (qemu-system-x86_64 ...)
# Terminal markers are read from the global array `QEMU_TERMINAL_MARKERS` (all must
# appear, as fixed substrings, before termination).
#
# Return codes (also echoed as QEMU_LIFECYCLE_RESULT=<reason>):
#   0  proof_complete_then_terminated   — all markers seen, no fatal, script killed QEMU
#   2  qemu_exited_before_proof         — QEMU process died before all markers appeared
#   3  fatal_before_termination         — a fatal marker appeared before proof completed
#   4  timeout_before_completion        — ceiling hit without all markers
#
# The caller emits its seal ONLY on return 0.

# Wait for a spawned QEMU pid to exit, escalating TERM→KILL. Echoes the wait status.
_qemu_reap() {
  local pid="$1"
  kill -TERM "$pid" 2>/dev/null || true
  # Give QEMU a moment to exit on SIGTERM, then hard-kill.
  local i
  for i in $(seq 1 25); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.2
  done
  kill -0 "$pid" 2>/dev/null && kill -KILL "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null
  return $?
}

qemu_run_deterministic() {
  local logfile="$1"; shift
  local fatal_re="$1"; shift
  local timeout_secs="$1"; shift
  # Remaining args are the QEMU argv.
  local -a qemu_argv=("$@")

  # Fresh log — truncate so a stale/manual terminal marker cannot be mistaken for this
  # run's proof (the "log predating script start" hard-stop).
  : >"$logfile"
  local norm="${logfile}.norm"

  local start_epoch deadline
  start_epoch=$(date +%s)
  deadline=$((start_epoch + timeout_secs))

  # Launch QEMU in the background so the SCRIPT retains control of termination.
  stdbuf -oL -eL "${qemu_argv[@]}" >"$logfile" 2>&1 &
  local qpid=$!

  local proof_seen=0
  while :; do
    # Ceiling.
    if (( $(date +%s) >= deadline )); then
      _qemu_reap "$qpid" >/dev/null 2>&1
      QEMU_LIFECYCLE_RESULT=timeout_before_completion
      return 4
    fi
    # Normalize CR→LF once per poll for substring/regex scanning.
    tr '\r' '\n' <"$logfile" >"$norm" 2>/dev/null || true
    # Fatal BEFORE proof completes → fail (fatal-marker-before-termination hard-stop).
    if rg -a -q -e "$fatal_re" "$norm" 2>/dev/null; then
      _qemu_reap "$qpid" >/dev/null 2>&1
      QEMU_LIFECYCLE_RESULT=fatal_before_termination
      return 3
    fi
    # All terminal markers present (fixed substrings)?
    local all=1 m
    for m in "${QEMU_TERMINAL_MARKERS[@]}"; do
      rg -a -q -F "$m" "$norm" 2>/dev/null || { all=0; break; }
    done
    if (( all )); then proof_seen=1; break; fi
    # QEMU exited on its own before proof → fail (early-exit hard-stop).
    if ! kill -0 "$qpid" 2>/dev/null; then
      # Final re-scan in case the last lines landed as it exited.
      tr '\r' '\n' <"$logfile" >"$norm" 2>/dev/null || true
      all=1
      for m in "${QEMU_TERMINAL_MARKERS[@]}"; do
        rg -a -q -F "$m" "$norm" 2>/dev/null || { all=0; break; }
      done
      if (( all )); then proof_seen=1; fi
      break
    fi
    sleep 1
  done

  if (( ! proof_seen )); then
    _qemu_reap "$qpid" >/dev/null 2>&1
    QEMU_LIFECYCLE_RESULT=qemu_exited_before_proof
    return 2
  fi

  # Proof complete: NOW the script terminates QEMU and waits for it to exit. A SIGTERM
  # exit status is acceptable ONLY because the terminal condition was observed first.
  _qemu_reap "$qpid" >/dev/null 2>&1
  QEMU_LIFECYCLE_RESULT=proof_complete_then_terminated
  return 0
}
