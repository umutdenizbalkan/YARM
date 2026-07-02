#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# Stage 180 (CI-PROFILES): a single, deterministic runner for the accepted
# manual smoke/oracle profiles from Stages 163P and 166-179.
#
# It does NOT duplicate smoke logic — it invokes the existing scripts
# (qemu-*-core-smoke.sh, qemu-ipc-recv-v2-oracle-smoke.sh) with the documented
# env knobs, per-profile timeout, QEMU SMP setting, and a deterministic log path,
# then prints a PASS/FAIL/SKIP summary and exits nonzero if any required profile
# failed.
#
# This is tooling only: it changes NO kernel behavior and weakens NO smoke gate.
#
# Usage:
#   scripts/run-ci-profiles.sh <group|profile>... [options]
#   scripts/run-ci-profiles.sh list
#
# Groups:   quick | full | extended
# Options:
#   --dry-run          print the commands only; do not launch QEMU
#   --keep-going       continue after a profile fails (default: stop)
#   --logs-dir <dir>   store wrapper + QEMU logs under <dir> (default: logs/ci-profiles)
#   --timeout <secs>   override the default per-profile timeout
#   --build            (re)build the QEMU artifacts for the needed arches first
#   -h | --help        show this help

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# --------------------------------------------------------------------------
# Profile registry.
#
# Each profile maps (via the case in `profile_field`) to five fields:
#   arch     x86_64 | aarch64 | riscv64   (which artifacts it needs)
#   smp      QEMU_SMP for this profile
#   timeout  default TIMEOUT_SECS
#   env      extra `KEY=VAL` env passed to the smoke script
#   runner   core | oracle                (which script family)
# --------------------------------------------------------------------------

ALL_PROFILES=(
  x86_64-core
  aarch64-core
  riscv64-core
  sender-wake
  ipc-final
  d6-switch-a
  d6-switch-proof
  d6-genuine
  d2-recv
  d2-send
  sched-timeout
  vm-cow
  cap-cnode
  fault-delivery
  spawn-lifecycle
  global-state
  smp-ready
  smp-ready-4
  cross-arch-d6-aarch64
  cross-arch-d6-riscv64
  d3-full
  unlock-graduated
  unlock-optout
)

QUICK_PROFILES=(x86_64-core unlock-graduated sender-wake d2-recv d2-send d3-full)

FULL_PROFILES=(
  x86_64-core aarch64-core riscv64-core
  unlock-graduated unlock-optout
  sender-wake ipc-final
  d6-switch-a d6-switch-proof d6-genuine
  d2-recv d2-send
  sched-timeout vm-cow cap-cnode fault-delivery spawn-lifecycle global-state
  smp-ready
  cross-arch-d6-aarch64 cross-arch-d6-riscv64
  d3-full
)

EXTENDED_PROFILES=("${FULL_PROFILES[@]}" smp-ready-4)

profile_desc() {
  case "$1" in
    x86_64-core)            echo "x86_64 normal core smoke (-smp 1)";;
    aarch64-core)           echo "AArch64 core smoke (boot-to-shell)";;
    riscv64-core)           echo "RISC-V core smoke (boot markers)";;
    sender-wake)            echo "Stage 163P sender-wake recv-v2 oracle (x86_64)";;
    ipc-final)              echo "IPC_FINAL=1 strict recv-v2 oracle (x86_64)";;
    d6-switch-a)            echo "D6_SWITCH_A=1 first narrow production Outcome A (x86_64 -smp 1)";;
    d6-switch-proof)        echo "D6_SWITCH_PROOF=1 controlled switch proof, 5 min (x86_64 -smp 1)";;
    d6-genuine)             echo "D6_GENUINE=1 out-of-lock dispatch slice (x86_64 -smp 1)";;
    d2-recv)                echo "D2_RECV_GENUINE=1 blocking-recv rank-clean dispatch";;
    d2-send)                echo "D2_SEND_GENUINE=1 blocking-send rank-clean dispatch";;
    sched-timeout)          echo "SCHED_TIMEOUT=1 scheduler timeout/deadline diagnostics";;
    vm-cow)                 echo "VM_COW=1 VM/COW/page-table/fork diagnostics";;
    cap-cnode)              echo "CAP_CNODE=1 capability/CNode lifecycle diagnostics";;
    fault-delivery)         echo "FAULT_DELIVERY=1 fault -> supervisor delivery diagnostics";;
    spawn-lifecycle)        echo "SPAWN_LIFECYCLE=1 spawn/image/lifecycle diagnostics";;
    global-state)           echo "GLOBAL_STATE=1 direct global-state mutation audit";;
    smp-ready)              echo "SMP_READY=1 SMP_READY_CPUS=2 x86_64 SMP-readiness audit";;
    smp-ready-4)            echo "SMP_READY=1 SMP_READY_CPUS=4 x86_64 SMP-readiness audit (extended)";;
    cross-arch-d6-aarch64)  echo "CROSS_ARCH_D6=1 AArch64 D6 restore-path audit (deferred)";;
    cross-arch-d6-riscv64)  echo "CROSS_ARCH_D6=1 RISC-V D6 restore-path audit (deferred)";;
    d3-full)                echo "D3_FULL=1 VM anon map/unmap two-phase proof";;
    unlock-graduated)       echo "UNLOCK_GRADUATED=1 accepted x86_64 -smp1 seams graduated (primary)";;
    unlock-optout)          echo "UNLOCK_GRADUATED=0 emergency opt-out / conservative fallback boots";;
    *)                      echo "unknown profile";;
  esac
}

# Echoes: "<arch> <smp> <timeout> <runner> [ENV=VAL ...]"
profile_field() {
  case "$1" in
    x86_64-core)            echo "x86_64 1 120 core";;
    aarch64-core)           echo "aarch64 1 120 core";;
    riscv64-core)           echo "riscv64 1 120 core";;
    sender-wake)            echo "x86_64 1 120 oracle YARM_IPC_RECV_PROOF_SENDER_WAKE=1";;
    ipc-final)              echo "x86_64 1 120 oracle IPC_FINAL=1";;
    d6-switch-a)            echo "x86_64 1 120 core D6_SWITCH_A=1";;
    d6-switch-proof)        echo "x86_64 1 300 core D6_SWITCH_PROOF=1";;
    d6-genuine)             echo "x86_64 1 120 core D6_GENUINE=1";;
    d2-recv)                echo "x86_64 1 120 core D2_RECV_GENUINE=1";;
    d2-send)                echo "x86_64 1 120 core D2_SEND_GENUINE=1";;
    sched-timeout)          echo "x86_64 1 120 core SCHED_TIMEOUT=1";;
    vm-cow)                 echo "x86_64 1 120 core VM_COW=1";;
    cap-cnode)              echo "x86_64 1 120 core CAP_CNODE=1";;
    fault-delivery)         echo "x86_64 1 120 core FAULT_DELIVERY=1";;
    spawn-lifecycle)        echo "x86_64 1 120 core SPAWN_LIFECYCLE=1";;
    global-state)           echo "x86_64 1 120 core GLOBAL_STATE=1";;
    smp-ready)              echo "x86_64 2 120 core SMP_READY=1 SMP_READY_CPUS=2";;
    smp-ready-4)            echo "x86_64 4 120 core SMP_READY=1 SMP_READY_CPUS=4";;
    cross-arch-d6-aarch64)  echo "aarch64 4 120 core CROSS_ARCH_D6=1";;
    cross-arch-d6-riscv64)  echo "riscv64 1 120 core CROSS_ARCH_D6=1";;
    d3-full)                echo "x86_64 1 120 core D3_FULL=1";;
    unlock-graduated)       echo "x86_64 1 120 core UNLOCK_GRADUATED=1";;
    unlock-optout)          echo "x86_64 1 120 core UNLOCK_GRADUATED=0";;
    *)                      return 1;;
  esac
}

core_script_for_arch() {
  case "$1" in
    x86_64)  echo "$HERE/qemu-x86_64-core-smoke.sh";;
    aarch64) echo "$HERE/qemu-aarch64-core-smoke.sh";;
    riscv64) echo "$HERE/qemu-riscv64-core-smoke.sh";;
  esac
}

build_script_for_arch() {
  echo "$HERE/build-qemu-$1-artifacts.sh"
}

# --------------------------------------------------------------------------
# Option parsing.
# --------------------------------------------------------------------------
DRY_RUN=0
KEEP_GOING=0
DO_BUILD=0
LOGS_DIR="logs/ci-profiles"
TIMEOUT_OVERRIDE=""
SELECTED=()

usage() {
  sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while (($# > 0)); do
  case "$1" in
    --dry-run)     DRY_RUN=1; shift;;
    --keep-going)  KEEP_GOING=1; shift;;
    --build)       DO_BUILD=1; shift;;
    --logs-dir)    LOGS_DIR="$2"; shift 2;;
    --logs-dir=*)  LOGS_DIR="${1#--logs-dir=}"; shift;;
    --timeout)     TIMEOUT_OVERRIDE="$2"; shift 2;;
    --timeout=*)   TIMEOUT_OVERRIDE="${1#--timeout=}"; shift;;
    -h|--help)     usage; exit 0;;
    list)          SELECTED+=("list"); shift;;
    quick|full|extended) SELECTED+=("$1"); shift;;
    -*)            echo "[error] unknown option: $1" >&2; exit 2;;
    *)             SELECTED+=("$1"); shift;;
  esac
done

if ((${#SELECTED[@]} == 0)); then
  echo "[error] no group/profile selected. Try: list | quick | full | extended | <profile>" >&2
  usage
  exit 2
fi

# --------------------------------------------------------------------------
# `list` command.
# --------------------------------------------------------------------------
if [[ "${SELECTED[0]}" == "list" ]]; then
  echo "Stage 180 CI profiles:"
  for p in "${ALL_PROFILES[@]}"; do
    printf '  %-24s %s\n' "$p" "$(profile_desc "$p")"
  done
  echo
  echo "Groups:"
  echo "  quick     ${QUICK_PROFILES[*]}"
  echo "  full      ${FULL_PROFILES[*]}"
  echo "  extended  ${EXTENDED_PROFILES[*]}"
  exit 0
fi

# --------------------------------------------------------------------------
# Expand groups into a concrete, de-duplicated profile list.
# --------------------------------------------------------------------------
RUN_LIST=()
add_profile() {
  local p="$1"
  local existing
  for existing in "${RUN_LIST[@]:-}"; do
    [[ "$existing" == "$p" ]] && return 0
  done
  RUN_LIST+=("$p")
}

for sel in "${SELECTED[@]}"; do
  case "$sel" in
    quick)    for p in "${QUICK_PROFILES[@]}"; do add_profile "$p"; done;;
    full)     for p in "${FULL_PROFILES[@]}"; do add_profile "$p"; done;;
    extended) for p in "${EXTENDED_PROFILES[@]}"; do add_profile "$p"; done;;
    *)
      if profile_field "$sel" >/dev/null 2>&1; then
        add_profile "$sel"
      else
        echo "[error] unknown profile: $sel (try: scripts/run-ci-profiles.sh list)" >&2
        exit 2
      fi
      ;;
  esac
done

mkdir -p "$LOGS_DIR"

# --------------------------------------------------------------------------
# Optional artifact build (only for the arches actually needed).
# --------------------------------------------------------------------------
if ((DO_BUILD == 1)); then
  declare -A NEED_ARCH=()
  for p in "${RUN_LIST[@]}"; do
    read -r arch _rest <<<"$(profile_field "$p")"
    NEED_ARCH["$arch"]=1
  done
  for arch in "${!NEED_ARCH[@]}"; do
    bs="$(build_script_for_arch "$arch")"
    echo "[build] $arch artifacts: $bs"
    if ((DRY_RUN == 1)); then
      echo "    DRY-RUN: bash $bs"
    else
      bash "$bs"
    fi
  done
fi

# --------------------------------------------------------------------------
# Run the profiles.
# --------------------------------------------------------------------------
declare -a RESULT_NAMES=()
declare -a RESULT_STATUS=()
declare -a RESULT_LOGS=()
OVERALL_RC=0

run_one() {
  local profile="$1"
  read -r arch smp timeout runner env_kv <<<"$(profile_field "$profile")"
  local extra_env="${env_kv:-}"
  # collect ALL trailing env tokens (profile_field may echo more than one)
  local fields
  fields="$(profile_field "$profile")"
  # fields = "arch smp timeout runner ENV1=.. ENV2=.."
  local -a toks=($fields)
  arch="${toks[0]}"; smp="${toks[1]}"; timeout="${toks[2]}"; runner="${toks[3]}"
  local -a envs=("${toks[@]:4}")
  [[ -n "$TIMEOUT_OVERRIDE" ]] && timeout="$TIMEOUT_OVERRIDE"

  local logfile="$LOGS_DIR/qemu-$profile.log"
  local wrapper="$LOGS_DIR/qemu-$profile.wrapper.log"

  local -a cmd=(env "TIMEOUT_SECS=$timeout" "QEMU_SMP=$smp" "LOGFILE=$logfile")
  local e
  for e in "${envs[@]:-}"; do
    [[ -n "$e" ]] && cmd+=("$e")
  done
  if [[ "$runner" == "oracle" ]]; then
    cmd+=("$HERE/qemu-ipc-recv-v2-oracle-smoke.sh" "$arch")
  else
    cmd+=("$(core_script_for_arch "$arch")")
  fi

  echo "=== profile: $profile ($(profile_desc "$profile")) ==="
  printf '    cmd: %s\n' "${cmd[*]}"

  if ((DRY_RUN == 1)); then
    RESULT_NAMES+=("$profile"); RESULT_STATUS+=("SKIP(dry-run)"); RESULT_LOGS+=("$logfile")
    return 0
  fi

  local rc=0
  "${cmd[@]}" 2>&1 | tee "$wrapper" || rc=${PIPESTATUS[0]}
  if ((rc == 0)); then
    RESULT_NAMES+=("$profile"); RESULT_STATUS+=("PASS"); RESULT_LOGS+=("$logfile")
  else
    RESULT_NAMES+=("$profile"); RESULT_STATUS+=("FAIL(rc=$rc)"); RESULT_LOGS+=("$logfile")
    OVERALL_RC=1
    if ((KEEP_GOING == 0)); then
      return 1
    fi
  fi
  return 0
}

for profile in "${RUN_LIST[@]}"; do
  if ! run_one "$profile"; then
    echo "[error] profile '$profile' failed; stopping (use --keep-going to continue)"
    break
  fi
done

# --------------------------------------------------------------------------
# Summary table.
# --------------------------------------------------------------------------
echo
echo "==================== CI PROFILE SUMMARY ===================="
printf '%-24s %-16s %s\n' "PROFILE" "STATUS" "LOG"
i=0
while ((i < ${#RESULT_NAMES[@]})); do
  printf '%-24s %-16s %s\n' "${RESULT_NAMES[$i]}" "${RESULT_STATUS[$i]}" "${RESULT_LOGS[$i]}"
  i=$((i + 1))
done
echo "==========================================================="

exit "$OVERALL_RC"
