#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Drives scripts/qemu-riscv64-core-smoke.sh across --smp 1/2/3/4 in a
# single invocation, then summarizes per-N pass/fail, the observed boot
# hart, the present_cpus / present_bitmap, whether the service chain
# reached idle, and the timer / PLIC state. Use this for the regular
# RISC-V smoke pass; the per-N script is the canonical gate.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUILD_SCRIPT="${BUILD_SCRIPT:-${SCRIPT_DIR}/build-qemu-riscv64-artifacts.sh}"
SMOKE_SCRIPT="${SMOKE_SCRIPT:-${SCRIPT_DIR}/qemu-riscv64-core-smoke.sh}"
LOG_DIR="${LOG_DIR:-.}"
SKIP_BUILD="${SKIP_BUILD:-0}"
SMP_MATRIX="${SMP_MATRIX:-1 2 3 4}"
TIMEOUT_SECS="${TIMEOUT_SECS:-30}"

if [[ "$SKIP_BUILD" != "1" ]]; then
  echo "[info] building riscv64 qemu artifacts"
  "$BUILD_SCRIPT"
else
  echo "[info] skipping build (SKIP_BUILD=1)"
fi

mkdir -p "$LOG_DIR"

declare -a SUMMARY=()
overall_failures=0

run_one() {
  local n="$1"
  local log="${LOG_DIR}/qemu-riscv64-smp${n}.log"
  echo
  echo "[info] === qemu-riscv64-core-smoke --smp ${n} ==="

  local rc=0
  LOGFILE="$log" "$SMOKE_SCRIPT" --smp "$n" --timeout "$TIMEOUT_SECS" >"${log}.stdout" 2>&1 || rc=$?
  cat "${log}.stdout"

  local status="PASS"
  if (( rc != 0 )); then
    status="FAIL"
    overall_failures=$((overall_failures + 1))
  fi
  # The smoke script's own "[fail]"/"[ok]" line is the source of truth:
  # treat any "[fail]" line as a failure even if the script's exit code
  # is 0 (QEMU_SMOKE_STRICT=0).
  if rg -n "^\[fail\]" "${log}.stdout" >/dev/null 2>&1; then
    if [[ "$status" == "PASS" ]]; then
      status="FAIL"
      overall_failures=$((overall_failures + 1))
    fi
  fi

  local boot_hart="?"
  local present="?"
  local bitmap="?"
  local online="?"
  local idle="no"
  local timer="?"
  local plic="?"

  if [[ -f "$log" ]]; then
    boot_hart=$(rg -aN "RISCV_BOOT_HART_SELECTED hart=" "$log" 2>/dev/null \
      | head -n1 | sed -E 's/.*hart=([0-9]+).*/\1/' || true)
    [[ -z "$boot_hart" ]] && boot_hart="?"
    local topo
    topo=$(rg -aN "YARM_BOOT_OK present_cpus=" "$log" 2>/dev/null | head -n1 || true)
    if [[ -n "$topo" ]]; then
      present=$(echo "$topo" | sed -E 's/.*present_cpus=([0-9]+).*/\1/')
      bitmap=$(echo "$topo" | sed -E 's/.*present_bitmap=(0x[0-9a-fA-F]+).*/\1/')
      online=$(echo "$topo" | sed -E 's/.*online_cpus=([0-9]+).*/\1/')
    fi
    rg -aN "RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked" "$log" >/dev/null 2>&1 && idle="yes"
    if rg -aN "RISCV_TIMER_SMOKE_OK ticks=" "$log" >/dev/null 2>&1; then
      timer="live"
    elif rg -aN "RISCV_TIMER_DEFERRED reason=" "$log" >/dev/null 2>&1; then
      timer="deferred"
    fi
    if rg -aN "RISCV_PLIC_INIT_DONE" "$log" >/dev/null 2>&1; then
      plic="live"
    elif rg -aN "RISCV_PLIC_DEFERRED reason=" "$log" >/dev/null 2>&1; then
      plic="deferred"
    fi
  fi

  SUMMARY+=("${status}|${n}|${boot_hart}|${present}|${bitmap}|${online}|${idle}|${timer}|${plic}|${log}")
}

for n in $SMP_MATRIX; do
  run_one "$n"
done

echo
echo "=== qemu-riscv64 smoke matrix summary ==="
printf "%-6s %-4s %-9s %-7s %-9s %-6s %-7s %-9s %-9s %s\n" \
  "STATUS" "SMP" "BOOT_HART" "PRESENT" "BITMAP" "ONLINE" "IDLE" "TIMER" "PLIC" "LOG"
for row in "${SUMMARY[@]}"; do
  IFS='|' read -r st smp bh pr bm onl id tm pl logf <<< "$row"
  printf "%-6s %-4s %-9s %-7s %-9s %-6s %-7s %-9s %-9s %s\n" \
    "$st" "$smp" "$bh" "$pr" "$bm" "$onl" "$id" "$tm" "$pl" "$logf"
done

if (( overall_failures > 0 )); then
  echo
  echo "[fail] qemu-riscv64-smoke-matrix: ${overall_failures} run(s) failed"
  exit 1
fi
echo
echo "[ok] qemu-riscv64-smoke-matrix: all configurations passed"
exit 0
