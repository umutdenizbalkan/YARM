#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 197 — FIRST-COHORT CROSS-ARCHITECTURE RETIREMENT SEAL validation.
#
# Runs the full per-architecture QEMU matrix against FRESH artifacts and asserts the four sealed
# retirement classes { DebugLog, FutexWake, FutexWait, Yield } are proven on x86_64, AArch64 and
# RISC-V with the canonical arch-tagged markers:
#
#   GLOBAL_LOCK_RETIRE_CLASS_DONE arch=<arch> class=<class> result=ok
#
# For each (arch, class) the seal is "live" when the marker appears in a fresh QEMU boot, or
# "source_guard" when no live boot trigger exists on that architecture (currently only
# x86_64/FutexWake — x86_64 has no slot-5 oracle wiring; the identical retired `futex_wake_split_mut`
# seam IS live-proven on AArch64 + RISC-V, and the x86_64 arch-tagged emission is source-verified).
#
# The seal markers below are emitted BY THIS SCRIPT from the per-arch logs — no kernel markers were
# added to fabricate the matrix. Exits non-zero on any missing proof or any forbidden marker.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/first-cohort-seal}
mkdir -p "$LOGDIR"
STRICT=${QEMU_SMOKE_STRICT:-1}
fail=0
CLASSES=(DebugLog FutexWake FutexWait Yield)

note() { echo "[seal] $*"; }
die()  { echo "[seal][fail] $*"; fail=1; }

# ── 1. Require fresh artifacts + record hashes/mtimes ──
note "artifact hashes / mtimes:"
for f in build-x86_64/kernel_boot.elf build-aarch64/yarm-aarch64.bin build-riscv64/yarm-riscv64.bin; do
  if [[ ! -f "$f" ]]; then die "missing artifact: $f (build fresh first)"; continue; fi
  printf '  %s  %s  %s\n' "$(sha256sum "$f" | cut -d' ' -f1)" "$(stat -c '%y' "$f" | cut -d'.' -f1)" "$f"
done
(( fail )) && { echo "FIRST_COHORT_CROSS_ARCH_SEAL arches=3 classes=4 result=fail reason=missing_artifacts"; exit 1; }

# ── 2. Run the per-architecture matrix, aggregate logs per arch ──
run() { # run <logfile> <env=val...> <script> [script-args...]
  local log="$1"; shift
  note "run: $* (log=$log)"
  # All VAR=val assignments (mine + the caller's oracle vars in "$@") must precede the command,
  # so `env` parses them as environment and treats the trailing token as the script to exec.
  env LOGFILE="$log" QEMU_SMOKE_STRICT="$STRICT" "$@" >/dev/null 2>&1 || true
}

# RISC-V: core + FutexWake + FutexWait switch + FutexWait idle + Yield two-task + Yield lone.
run "$LOGDIR/rv_core.log"   scripts/qemu-riscv64-core-smoke.sh
run "$LOGDIR/rv_fw.log"     FUTEX_WAKE_ORACLE=1        scripts/qemu-riscv64-core-smoke.sh
run "$LOGDIR/rv_fwait.log"  FUTEX_WAIT_ORACLE=1        scripts/qemu-riscv64-core-smoke.sh
run "$LOGDIR/rv_idle.log"   FUTEX_WAIT_IDLE_ORACLE=1   scripts/qemu-riscv64-core-smoke.sh
run "$LOGDIR/rv_ytwo.log"   YIELD_TWO_TASK_ORACLE=1    scripts/qemu-riscv64-core-smoke.sh
run "$LOGDIR/rv_ylone.log"  YIELD_LONE_TASK_ORACLE=1   scripts/qemu-riscv64-core-smoke.sh

# AArch64: core SMP=2 + FutexWake + FutexWait switch + FutexWait idle + Yield two-task + Yield lone.
run "$LOGDIR/arm_core.log"  QEMU_SMP=2                 scripts/qemu-aarch64-core-smoke.sh
run "$LOGDIR/arm_fw.log"    FUTEX_WAKE_ORACLE=1        scripts/qemu-aarch64-core-smoke.sh
run "$LOGDIR/arm_fwait.log" FUTEX_WAIT_ORACLE=1        scripts/qemu-aarch64-core-smoke.sh
run "$LOGDIR/arm_idle.log"  FUTEX_WAIT_IDLE_ORACLE=1   scripts/qemu-aarch64-core-smoke.sh
run "$LOGDIR/arm_ytwo.log"  YIELD_ORACLE=1             scripts/qemu-aarch64-core-smoke.sh
run "$LOGDIR/arm_ylone.log" YIELD_LONE_ORACLE=1        scripts/qemu-aarch64-core-smoke.sh

# x86_64: core + IPC cap-enqueue + SMP=2 + SMP=4 + crash-restart (DebugLog/FutexWait/Yield natural).
run "$LOGDIR/x86_core.log"  scripts/qemu-x86_64-core-smoke.sh
run "$LOGDIR/x86_ipc.log"   IPC_SEND_CAP_ENQUEUE_ORACLE=1 scripts/qemu-x86_64-core-smoke.sh
run "$LOGDIR/x86_smp2.log"  QEMU_SMP=2                 scripts/qemu-x86_64-core-smoke.sh
run "$LOGDIR/x86_smp4.log"  QEMU_SMP=4                 scripts/qemu-x86_64-core-smoke.sh
run "$LOGDIR/x86_crash.log" scripts/qemu-supervisor-crash-restart-smoke.sh

# ── 3. Reject forbidden markers across every log ──
# NB: NR27 (InitramfsReadChunk) is a pre-existing NON-cohort retirement on x86_64/AArch64 — the
# rule is that it must not be PORTED to RISC-V, so only `arch=riscv64 class=InitramfsReadChunk` is
# forbidden (the rv_* logs must never contain it), not the class marker globally.
FORBIDDEN='arch=riscv64 class=InitramfsReadChunk|RISCV_YIELD_DISPATCH_FAIL|RISCV_FUTEX_WAIT_DISPATCH_FAIL|reason=trap_from_s_mode|RISCV_TRAP_UNHANDLED|FATAL|!BN'
for log in "$LOGDIR"/rv_*.log "$LOGDIR"/arm_*.log "$LOGDIR"/x86_*.log; do
  [[ -f "$log" ]] || continue
  if rg -a -n "$FORBIDDEN" "$log" >/dev/null 2>&1; then
    die "forbidden marker in $(basename "$log"): $(rg -a -oN "$FORBIDDEN" "$log" | head -1)"
  fi
done

# ── 4. Per-arch / per-class seal (live marker across the arch's aggregated logs) ──
seal_class() { # seal_class <arch> <class> <logglob>
  local arch="$1" class="$2"; shift 2
  local marker="GLOBAL_LOCK_RETIRE_CLASS_DONE arch=${arch} class=${class} result=ok"
  if rg -a -N "$marker" "$@" >/dev/null 2>&1; then
    echo "FIRST_COHORT_SEAL arch=${arch} class=${class} result=ok proof=live"
    return 0
  fi
  # x86_64/FutexWake has no live boot trigger; fall back to a source guard (the arch-tagged emission
  # exists in the shared split producer, and the identical seam is live on AArch64 + RISC-V).
  if [[ "$arch" == "x86_64" && "$class" == "FutexWake" ]] \
     && rg -F "arch=x86_64 class=FutexWake result=ok" src/kernel/syscall_split.rs >/dev/null 2>&1; then
    echo "FIRST_COHORT_SEAL arch=${arch} class=${class} result=ok proof=source_guard note=no_live_x86_futexwake_trigger"
    return 0
  fi
  echo "FIRST_COHORT_SEAL arch=${arch} class=${class} result=MISSING"
  die "no proof for arch=${arch} class=${class}"
  return 1
}

echo "── first-cohort seal matrix ──"
arch_ok() { # arch_ok <arch> <logglob...>
  local arch="$1"; shift
  local n=0
  for c in "${CLASSES[@]}"; do
    seal_class "$arch" "$c" "$@" && n=$((n+1))
  done
  echo "FIRST_COHORT_SEAL arch=${arch} classes=${n} result=$([[ $n -eq 4 ]] && echo ok || echo fail)"
  [[ $n -eq 4 ]]
}

arches_ok=0
arch_ok x86_64  "$LOGDIR"/x86_*.log  && arches_ok=$((arches_ok+1))
arch_ok aarch64 "$LOGDIR"/arm_*.log  && arches_ok=$((arches_ok+1))
arch_ok riscv64 "$LOGDIR"/rv_*.log   && arches_ok=$((arches_ok+1))

# ── 5. Idle-outcome cross-check (AArch64 + RISC-V live; x86 source-audited, see the doc) ──
rg -a -N "AARCH64_FUTEX_WAIT_IDLE_ORACLE_DONE result=ok" "$LOGDIR"/arm_idle.log >/dev/null 2>&1 \
  || die "aarch64 FutexWait idle oracle proof missing"
rg -a -N "RISCV_FUTEX_WAIT_IDLE_ORACLE_DONE result=ok lock_dropped=1 current_none=1 outgoing_blocked=1" \
  "$LOGDIR"/rv_idle.log >/dev/null 2>&1 || die "riscv64 FutexWait idle oracle proof missing"

# ── 6. Final cross-architecture seal ──
if [[ $arches_ok -eq 3 && $fail -eq 0 ]]; then
  echo "FIRST_COHORT_CROSS_ARCH_SEAL arches=3 classes=4 result=ok"
  exit 0
else
  echo "FIRST_COHORT_CROSS_ARCH_SEAL arches=${arches_ok} classes=4 result=fail"
  exit 1
fi
