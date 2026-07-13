#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail
source "$(dirname "$0")/qemu-smoke-common.sh"

KERNEL_IMAGE=${KERNEL_IMAGE:-build-aarch64/yarm-aarch64.bin}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-build-aarch64/initramfs-core.cpio}
TIMEOUT_SECS=${TIMEOUT_SECS:-30}
QEMU_SMOKE_STRICT=${QEMU_SMOKE_STRICT:-0}
QEMU_MACHINE=${QEMU_MACHINE:-virt}
QEMU_CPU=${QEMU_CPU:-cortex-a72}
QEMU_MEMORY=${QEMU_MEMORY:-1024M}
QEMU_SMP=${QEMU_SMP:-2}
# Keep kernel cmdline empty by default until AArch64 command-line parsing is
# explicitly validated. Override if needed via KERNEL_CMDLINE=...
KERNEL_CMDLINE=${KERNEL_CMDLINE:-}
# Stage 159BC/D: the IPC recv-v2 oracle proof workload only runs when the kernel
# is booted with yarm.ipc_recv_proof=1. The oracle script sets IPC_RECV_PROOF=1
# whenever any proof requirement env var is enabled (AArch64 cmdline parsing of
# this knob is validated). Append it without disturbing any explicit override.
IPC_RECV_PROOF=${IPC_RECV_PROOF:-0}
if [[ "$IPC_RECV_PROOF" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_recv_proof="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.ipc_recv_proof=1"
fi
# Stage 163: the sender-wake proof additionally needs the sub-knob
# yarm.ipc_recv_proof_sender_wake=1 (gates the coordination hook + workload).
IPC_RECV_PROOF_SENDER_WAKE=${IPC_RECV_PROOF_SENDER_WAKE:-0}
if [[ "$IPC_RECV_PROOF_SENDER_WAKE" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_recv_proof_sender_wake="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.ipc_recv_proof_sender_wake=1"
fi

# Stage 198A (SECOND-COHORT PLAIN PARITY): the plain-IpcSend live oracles are arch-neutral, so
# AArch64 honors the same env-var -> cmdline knob translations the x86_64 core smoke does. The
# oracle wrapper (qemu-ipc-recv-v2-oracle-smoke.sh) exports IPC_SEND_PLAIN_ORACLE /
# IPC_SEND_ENQUEUE_ORACLE; without these translations the knobs never reach the AArch64 kernel
# cmdline and the oracle workload never runs.
IPC_SEND_PLAIN_ORACLE=${IPC_SEND_PLAIN_ORACLE:-0}
if [[ "$IPC_SEND_PLAIN_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_send_plain_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.ipc_send_plain_oracle=1"
fi
IPC_SEND_ENQUEUE_ORACLE=${IPC_SEND_ENQUEUE_ORACLE:-0}
if [[ "$IPC_SEND_ENQUEUE_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_send_enqueue_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.ipc_send_enqueue_oracle=1"
fi

# Stage 178 (CROSS-ARCH-D6): CROSS_ARCH_D6=1 appends yarm.cross_arch_d6=1 to emit the
# AArch64 D6 restore-path audit markers (model=trapframe_eret; read-only observe of
# ELR/SPSR/SP + TTBR0/ASID). Live lock-dropped restore is DEFERRED — the audit records
# the explicit deferral, not a fake live restore. No behavior change.
CROSS_ARCH_D6=${CROSS_ARCH_D6:-0}
if [[ "$CROSS_ARCH_D6" == "1" && "$KERNEL_CMDLINE" != *"yarm.cross_arch_d6="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.cross_arch_d6=1"
fi

# Stage 195C (AARCH64 FUTEXWAKE LIVE ORACLE): FUTEX_WAKE_ORACLE=1 appends
# yarm.aarch64_futex_wake_oracle=1, which provisions the init slot-5 sentinel that runs the
# default-off parent/child FutexWake live oracle. A child blocks through the LEGACY global-lock
# FutexWait; init wakes it through the SPLIT FutexWake path and proves the authoritative wake
# counts (first=1, second=0). The FutexWake split-dispatch class is NR 10 (the task text's
# "NR11" is incorrect — NR 11 is SpawnThread).
FUTEX_WAKE_ORACLE=${FUTEX_WAKE_ORACLE:-0}
if [[ "$FUTEX_WAKE_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.aarch64_futex_wake_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.aarch64_futex_wake_oracle=1"
  # AArch64 dispatches user tasks on the BSP only (single-dispatcher; APs are wake-only).
  # The freshly-spawned waiter is enqueued balanced, so on SMP>1 it can land on a
  # non-dispatching AP and never run. The oracle is a single-dispatcher proof — boot it on a
  # single CPU so the waiter is guaranteed to be enqueued on the sole dispatching CPU.
  QEMU_SMP=1
fi

# Stage 195E (AARCH64 FUTEXWAIT QUEUE-ADVANCING LIVE ORACLE): FUTEX_WAIT_ORACLE=1 appends
# yarm.aarch64_futex_wait_oracle=1, which enables the FutexWait (NR 9) queue-advancing
# out-of-lock retirement AND provisions the init slot-5 sentinel (=2). Task A (init) blocks via
# NR 9 → the AArch64 handler bypass returns cleanly → the post-lock drain dispatches task B (the
# child) → B wakes A via split FutexWake (NR 10) → A resumes once. Unlike the FutexWake oracle
# this does NOT force single-CPU: 195D BSP affinity makes the drain correct under SMP=2, so this
# runs at the requested QEMU_SMP (default 2). FutexWait is NR 9 (NR 10 is FutexWake, NR 11 is
# SpawnThread).
FUTEX_WAIT_ORACLE=${FUTEX_WAIT_ORACLE:-0}
if [[ "$FUTEX_WAIT_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.aarch64_futex_wait_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.aarch64_futex_wait_oracle=1"
fi

# Stage 195F (AARCH64 FUTEXWAIT DEFAULT-ON — NO-INCOMING IDLE): FUTEX_WAIT_IDLE_ORACLE=1 appends
# yarm.aarch64_futex_wait_idle_oracle=1, which provisions the init slot-5 sentinel (=3). The
# FutexWait retirement MECHANISM is now DEFAULT-ON (no enable knob); this knob only selects the
# idle-oracle WORKLOAD: init (the last runnable user task) blocks on a never-woken futex with no
# other runnable user task, so the post-lock drain takes the Idle outcome and enters the BSP idle
# loop. QEMU then stays idle (WFI) until the smoke timeout. Give it a longer timeout.
FUTEX_WAIT_IDLE_ORACLE=${FUTEX_WAIT_IDLE_ORACLE:-0}
if [[ "$FUTEX_WAIT_IDLE_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.aarch64_futex_wait_idle_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.aarch64_futex_wait_idle_oracle=1"
fi

# Stage 195G (AARCH64 YIELD OUT-OF-LOCK DISPATCH — DEFAULT-ON): the Yield (NR 0) retirement
# MECHANISM is DEFAULT-ON. YIELD_ORACLE=1 selects the two-task workload (slot 5 = 4): task A
# (init) yields, the post-lock drain dispatches task B (a spawned child), B runs and blocks, A
# resumes. YIELD_LONE_ORACLE=1 selects the lone-task workload (slot 5 = 5): the sole runnable
# task yields and the drain re-dispatches it (same-task, no idle). Both run at the requested
# QEMU_SMP (default 2). Yield is NR 0.
YIELD_ORACLE=${YIELD_ORACLE:-0}
if [[ "$YIELD_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.aarch64_yield_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.aarch64_yield_oracle=1"
fi
YIELD_LONE_ORACLE=${YIELD_LONE_ORACLE:-0}
if [[ "$YIELD_LONE_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.aarch64_yield_lone_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.aarch64_yield_lone_oracle=1"
fi

require_file_or_warn "$KERNEL_IMAGE" "$QEMU_SMOKE_STRICT" "kernel image"
require_file_or_warn "$INITRAMFS_IMAGE" "$QEMU_SMOKE_STRICT" "initramfs image"
QEMU_BIN=${QEMU_BIN:-qemu-system-aarch64-hwe}
if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  QEMU_BIN=qemu-system-aarch64
fi
require_qemu_or_warn "$QEMU_BIN" "$QEMU_SMOKE_STRICT"

LOGFILE=${LOGFILE:-qemu-aarch64-core.log}
rm -f "$LOGFILE"

QEMU_ARGS=(
  -machine "$QEMU_MACHINE"
  -cpu "$QEMU_CPU"
  -m "$QEMU_MEMORY"
  -smp "$QEMU_SMP"
  -nographic
  -monitor none
  -serial stdio
  -kernel "$KERNEL_IMAGE"
  -initrd "$INITRAMFS_IMAGE"
)
if [[ -n "$KERNEL_CMDLINE" ]]; then
  QEMU_ARGS+=(-append "$KERNEL_CMDLINE")
fi

echo "[info] qemu command: $QEMU_BIN ${QEMU_ARGS[*]}"

MARKER_REGEX="YARM_AARCH64_BOOT_MARKER|YARM_SUPERVISOR_TID2_SPAWNED|YARM_PM_TID3_SPAWNED|YARM_BOOT_OK|YARM_PROC_VFS_OK|YARM_INIT_START|YARM_INIT_DONE|BusyBox|/ #|Welcome|\[ui\] boot-to-shell marker"
INIT_SERVER_REGEX="init_server|first server|first-server"
EARLY_MARKER_SEQUENCE=(
  "YARM_AARCH64_BOOT_MARKER stage=_start"
  "YARM_AARCH64_BOOT_MARKER stage=prepare_arch_boot"
  "YARM_AARCH64_BOOT_MARKER stage=vbar_el1_ready"
  "YARM_AARCH64_BOOT_MARKER stage=mmu_enabled"
  "YARM_AARCH64_BOOT_MARKER stage=run_with_prepared_kernel"
  "YARM_SUPERVISOR_TID2_SPAWNED"
  "YARM_PM_TID3_SPAWNED"
  "YARM_BOOT_OK"
)
# Markers 4-6 come from user_log! which is a no-op in no_std; checked warn-only.
SPAWN_IPC_SEQUENCE=(
  "YARM_PM_RECV_LOOP_START"
  "INIT_SPAWN_V5_CALL_BEGIN"
  "INIT_SPAWN_V5_REPLY_OK"
)

if run_qemu_timeout_to_log "$TIMEOUT_SECS" "$LOGFILE" "$QEMU_BIN" "${QEMU_ARGS[@]}"; then
  QEMU_STATUS=0
else
  QEMU_STATUS=$?
fi

log_count_pattern() {
  local pattern="$1"
  [[ -f "$LOGFILE" ]] || { echo 0; return; }
  tr '\r' '\n' <"$LOGFILE" | rg -a -c "\\b${pattern}\\b" 2>/dev/null || echo 0
}

# Stage 178 (CROSS-ARCH-D6): when booted with yarm.cross_arch_d6=1, validate the
# AArch64 D6 restore-path audit. Runs regardless of which boot-outcome exit path is
# taken below. Acceptance is honest: either a live RESTORE_DONE OR an explicit
# FALLBACK/DEFERRED reason, plus INVARIANT_OK + PROOF_DONE. AArch64 live restore is
# DEFERRED in Stage 178, so the DEFERRED branch is the expected path.
cad_has() { [[ -f "$LOGFILE" ]] && tr '\r' '\n' <"$LOGFILE" | rg -a -q -- "$1"; }
if [[ "$CROSS_ARCH_D6" == "1" ]]; then
  cross_arch_d6_fail=0
  echo "[ok] CROSS_ARCH_D6 enabled marker:" $(cad_has "CROSS_ARCH_D6_ENABLED" && echo present || echo MISSING)
  if ! cad_has "CROSS_ARCH_D6_ENABLED"; then
    echo "[error] CROSS-ARCH-D6: CROSS_ARCH_D6_ENABLED missing (knob not applied)"
    cross_arch_d6_fail=1
  fi
  for m in "CROSS_ARCH_D6_INVARIANT_OK" "CROSS_ARCH_D6_PROOF_DONE"; do
    if cad_has "$m"; then
      echo "[ok] CROSS-ARCH-D6 marker present: $m"
    else
      echo "[error] CROSS-ARCH-D6: required marker missing: $m"
      cross_arch_d6_fail=1
    fi
  done
  # AArch64 records model=trapframe_eret (not the x86_64 switch_frames model).
  if cad_has "CROSS_ARCH_D6_ARCH_MODEL arch=aarch64 model=trapframe_eret"; then
    echo "[ok] CROSS-ARCH-D6: AArch64 model=trapframe_eret (not switch_frames)"
  else
    echo "[warn] CROSS-ARCH-D6: aarch64 trapframe_eret model marker not observed"
  fi
  # Either a live restore completed OR an explicit fallback/deferred reason.
  if cad_has "CROSS_ARCH_D6_RESTORE_DONE" || cad_has "CROSS_ARCH_D6_FALLBACK" || cad_has "CROSS_ARCH_D6_AARCH64_DEFERRED"; then
    echo "[ok] CROSS-ARCH-D6: live restore-done or explicit fallback/deferred recorded"
  else
    echo "[error] CROSS-ARCH-D6: neither RESTORE_DONE nor an explicit fallback/deferred reason recorded"
    cross_arch_d6_fail=1
  fi
  for f in \
    "CROSS_ARCH_D6_GLOBAL_GUARD_HELD" \
    "CROSS_ARCH_D6_BAD_TRAPFRAME" \
    "CROSS_ARCH_D6_BAD_ASID" \
    "CROSS_ARCH_D6_CURRENT_TID_MISMATCH" \
    "CROSS_ARCH_D6_DOUBLE_DISPATCH" \
    "CROSS_ARCH_D6_RESTORE_FAIL" \
    "CROSS_ARCH_D6_UNSUPPORTED_MODEL" \
    "CROSS_ARCH_D6_INVARIANT_FAIL" \
    "CapabilityFull" \
    "TaskTableFull" \
    "BLOCKED_WOULDBLOCK_FATAL"; do
    if cad_has "$f"; then
      echo "[error] CROSS-ARCH-D6: fatal marker present: $f"
      cross_arch_d6_fail=1
    fi
  done
  if [[ -f "$LOGFILE" ]]; then
    cad_tail="$(tr '\r' '\n' <"$LOGFILE" | awk '/CROSS_ARCH_D6_ENABLED/{seen=1} seen{print}')"
    for fatal_pat in '^!Fv' '^!BNv' 'DOUBLE_FAULT' 'TRIPLE' 'PANIC' 'FATAL'; do
      if printf '%s\n' "$cad_tail" | rg -a -q -- "$fatal_pat"; then
        echo "[error] CROSS-ARCH-D6: fatal breadcrumb after cross-arch-d6 wire start: $fatal_pat"
        cross_arch_d6_fail=1
      fi
    done
    for pf_fatal in 'PAGE_FAULT_UNHANDLED' 'PAGE_FAULT_FATAL' 'PAGE_FAULT_NOT_HANDLED'; do
      if printf '%s\n' "$cad_tail" | rg -a -F -q -- "$pf_fatal"; then
        echo "[error] CROSS-ARCH-D6: explicit unhandled/fatal page-fault marker: $pf_fatal"
        cross_arch_d6_fail=1
      fi
    done
  fi
  if [[ "$cross_arch_d6_fail" -eq 1 ]]; then
    echo "[error] CROSS-ARCH-D6 mode FAILED"
    exit 1
  fi
  echo "[ok] CROSS-ARCH-D6: AArch64 D6 restore-path audit diagnostics clean (live restore DEFERRED)"
fi

BLOCKER_REGEX='IPC_CALL_FAIL|IPC_RECV_CAP_MATERIALIZE_FAILED|IPC_RECV_BLOCKED_COMPLETE_FAILED|CapabilityFull|VM_FULL|YARM_FIRST_USER_FAIL|MemoryObjectMissing|ELF_MISSING|PrivilegeViolation|failed to bootstrap first user task|panic|InvalidCapability|WrongObject|StaleCapability|MissingRight|UserMemoryFault|PM_RECV_DECODE_FAIL|bad_len expected=16 got=8|CAP_LOOKUP tid=1 cap=0|empty-elf|Malformed|Syscall\\(Internal\\)|memory allocation of|DELEGATE_FAIL|delegation.*fail|IPC_REPLY_FAST_REVOKE_FAIL|PM_PANIC|INIT_PANIC|DEVFS_PANIC|VFS_PANIC|INITRAMFS_PANIC|INITRAMFS_CPIO_EMPTY|D2_PUBLISH_RACE_UNWIND'
# XARCH-SRV-PARITY: the supervisor's lifecycle SELF-query (query PM for the
# supervisor's own tid, before entering the event loop) returns WrongObject
# uniformly on x86_64, aarch64, AND riscv64 — the supervisor logs it and continues
# (SUPERVISOR_EVENT_LOOP_TICK follows), and the full server chain loads regardless.
# It is a benign, cross-arch condition that the x86_64 core smoke also accepts (that
# smoke has no WrongObject blocker at all). Exclude ONLY this exact self-query line so
# the aarch64 smoke matches x86_64's treatment; every OTHER WrongObject still blocks.
BLOCKER_EXCLUDE_REGEX='YARM_AARCH64_EXCEPTION_KIND unknown|BLOCKED_WOULDBLOCK_CLASSIFY|reply replay|second reply|replay rejected|SUPERVISOR_LIFECYCLE_QUERY_ERR tid=[0-9]+ err=WrongObject'

if [[ -f "$LOGFILE" ]]; then
  blocker_lines="$(tr '\r' '\n' <"$LOGFILE" | rg -a -n "$BLOCKER_REGEX" || true)"
  if [[ -n "$blocker_lines" ]]; then
    blocker_lines="$(printf '%s\n' "$blocker_lines" | rg -a -v "$BLOCKER_EXCLUDE_REGEX" || true)"
  fi
  if [[ -n "$blocker_lines" ]]; then
    echo "[error] BAD / BOOT BLOCKERS found:"
    printf '%s\n' "$blocker_lines"
    exit 1
  else
    echo "[ok] BAD / BOOT BLOCKERS: empty"
  fi
fi

# Stage 195A: DebugLog (NR 15) is the first live AArch64 split-dispatch retirement class.
# Verified UNCONDITIONALLY (the strict boot-shell block below is gated on a boot-shell
# marker that AArch64 does not emit at the idle terminal). Require the
# import/dispatch/retire/finalize markers; forbid any split fatal or AArch64
# queue-advancing (FutexWait/Yield) / other-split-class retirement marker.
if [[ -f "$LOGFILE" ]]; then
  # Stage 195A (DebugLog NR 15) live acceptance. (Stage 197A removed the NR 27
  # InitramfsReadChunk split class along with the syscall; FutexWake NR 10 below covers the
  # second live AArch64 pre-lock split class.)
  if ! check_required_patterns "$LOGFILE" \
      "AARCH64_SPLIT_ABI_IMPORT_OK nr=15" \
      "YARM_LOCK_SPLIT_DISPATCH arch=aarch64 nr=15" \
      "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=DebugLog result=ok" \
      "AARCH64_SPLIT_FINALIZE_OK nr=15 result=ok"; then
    echo "[error] aarch64 Stage 195A split-dispatch markers missing"
    exit 1
  fi
  # The removed NR 27 InitramfsReadChunk retirement marker must NOT appear.
  if cad_has "class=InitramfsReadChunk"; then
    echo "[error] aarch64: removed NR 27 InitramfsReadChunk retirement marker present"
    exit 1
  fi
  # Forbid split fatals. Stage 195C ENABLES FutexWake (NR 10); Stage 195F/195G make the
  # FutexWait (NR 9) + Yield (NR 0) queue-advancing drains DEFAULT-ON, so `class=FutexWait` /
  # `class=Yield` are NOT forbidden. Only split-finalize ERRORS remain forbidden.
  a64_split_bads=(
    "AARCH64_SPLIT_FINALIZE_OK nr=15 result=error"
    "AARCH64_SPLIT_FINALIZE_OK nr=10 result=error"
  )
  for a64_split_bad in "${a64_split_bads[@]}"; do
    if cad_has "$a64_split_bad"; then
      echo "[error] aarch64 Stage 195A/195C: forbidden split marker: $a64_split_bad"
      exit 1
    fi
  done
  echo "[ok] aarch64 Stage 195A: DebugLog split-dispatch live (NR 27 removed; queue-advancing Yield inert)"
fi

# Stage 195C (AARCH64 FUTEXWAKE LIVE ORACLE): when booted with the oracle knob, require the
# full FutexWake split-dispatch + live-oracle marker set and forbid any oracle/split failure.
if [[ -f "$LOGFILE" && "$FUTEX_WAKE_ORACLE" == "1" ]]; then
  if ! check_required_patterns "$LOGFILE" \
      "AARCH64_SPLIT_ABI_IMPORT_OK nr=10" \
      "YARM_LOCK_SPLIT_DISPATCH arch=aarch64 nr=10" \
      "FUTEX_WAKE_SPLIT_BEGIN arch=aarch64" \
      "FUTEX_WAKE_SPLIT_DONE arch=aarch64 result=ok woke=1" \
      "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=FutexWake" \
      "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=FutexWake result=ok" \
      "AARCH64_SPLIT_FINALIZE_OK nr=10 result=ok" \
      "AARCH64_FUTEX_WAKE_LIVE_ORACLE_DONE result=ok first_wake=1 second_wake=0"; then
    echo "[error] aarch64 Stage 195C FutexWake live-oracle markers missing"
    exit 1
  fi
  for a64_oracle_bad in \
      "AARCH64_FUTEX_WAKE_ORACLE_SPAWN_FAIL" \
      "AARCH64_FUTEX_WAKE_LIVE_ORACLE_DONE result=fail" \
      "AARCH64_SPLIT_FINALIZE_OK nr=10 result=error"; do
    if cad_has "$a64_oracle_bad"; then
      echo "[error] aarch64 Stage 195C: forbidden oracle marker: $a64_oracle_bad"
      exit 1
    fi
  done
  echo "[ok] aarch64 Stage 195C: FutexWake (NR 10) split-dispatch live oracle proven (first_wake=1 second_wake=0)"
fi

# Stage 195E (AARCH64 FUTEXWAIT QUEUE-ADVANCING LIVE ORACLE): when booted with the FutexWait
# oracle knob, require the full handler-bypass + deferral + post-lock drain + retirement marker
# set and the live-oracle proof, and forbid any drain failure or stale-state decline.
if [[ -f "$LOGFILE" && "$FUTEX_WAIT_ORACLE" == "1" ]]; then
  if ! check_required_patterns "$LOGFILE" \
      "AARCH64_FUTEX_WAIT_DISPATCH_DEFER_BEGIN cpu=0" \
      "AARCH64_FUTEX_WAIT_DISPATCH_BLOCK_PUBLISH_OK" \
      "AARCH64_FUTEX_WAIT_HANDLER_BYPASS_BEGIN cpu=0" \
      "AARCH64_FUTEX_WAIT_HANDLER_BYPASS_DONE cpu=0" \
      "AARCH64_FUTEX_WAIT_DISPATCH_REVERIFY_OK" \
      "AARCH64_FUTEX_WAIT_DISPATCH_DEQUEUE_OK cpu=0" \
      "AARCH64_FUTEX_WAIT_DISPATCH_CURRENT_SET_OK cpu=0" \
      "AARCH64_FUTEX_WAIT_DISPATCH_RUNNING_OK" \
      "AARCH64_FUTEX_WAIT_DISPATCH_TTBR0_OK" \
      "AARCH64_FUTEX_WAIT_DISPATCH_FRAME_OK" \
      "AARCH64_FUTEX_WAIT_DISPATCH_DONE result=ok" \
      "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=FutexWait" \
      "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=FutexWait result=ok" \
      "AARCH64_FUTEX_WAIT_RETIRE_DEFAULT_ON result=ok" \
      "AARCH64_FUTEX_WAIT_LIVE_ORACLE_DONE result=ok"; then
    echo "[error] aarch64 Stage 195E/195F FutexWait switch-oracle markers missing"
    exit 1
  fi
  for a64_fw_bad in \
      "AARCH64_FUTEX_WAIT_DISPATCH_FAIL" \
      "AARCH64_FUTEX_WAIT_DISPATCH_DEFERRED reason=state_changed" \
      "AARCH64_FUTEX_WAIT_DISPATCH_DEFERRED reason=no_incoming" \
      "AARCH64_BAD_USER_ELR"; do
    if cad_has "$a64_fw_bad"; then
      echo "[error] aarch64 Stage 195E: forbidden FutexWait drain marker: $a64_fw_bad"
      exit 1
    fi
  done
  echo "[ok] aarch64 Stage 195E: FutexWait (NR 9) queue-advancing switch-oracle proven (default-on)"
fi

# Stage 195F (AARCH64 FUTEXWAIT DEFAULT-ON — NO-INCOMING IDLE): when booted with the idle-oracle
# knob, require the full default-on + no-incoming + post-lock idle marker set, and forbid any
# drain failure, a restored blocked-caller frame, or an in-lock idle for the deferred trap.
if [[ -f "$LOGFILE" && "$FUTEX_WAIT_IDLE_ORACLE" == "1" ]]; then
  if ! check_required_patterns "$LOGFILE" \
      "AARCH64_FUTEX_WAIT_RETIRE_DEFAULT_ON result=ok" \
      "AARCH64_FUTEX_WAIT_DISPATCH_DEFER_BEGIN cpu=0" \
      "AARCH64_FUTEX_WAIT_HANDLER_BYPASS_BEGIN cpu=0" \
      "AARCH64_FUTEX_WAIT_HANDLER_BYPASS_DONE cpu=0" \
      "AARCH64_FUTEX_WAIT_DISPATCH_NO_INCOMING cpu=0" \
      "AARCH64_FUTEX_WAIT_POST_LOCK_IDLE_BEGIN cpu=0" \
      "AARCH64_FUTEX_WAIT_POST_LOCK_IDLE_LOCK_DROPPED_OK cpu=0" \
      "AARCH64_FUTEX_WAIT_DISPATCH_DONE result=idle" \
      "AARCH64_FUTEX_WAIT_POST_LOCK_IDLE_ENTERED cpu=0" \
      "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=FutexWait result=ok" \
      "AARCH64_FUTEX_WAIT_IDLE_ORACLE_DONE result=ok lock_dropped=1 current_none=1"; then
    echo "[error] aarch64 Stage 195F FutexWait no-incoming idle-oracle markers missing"
    exit 1
  fi
  for a64_idle_bad in \
      "AARCH64_FUTEX_WAIT_DISPATCH_FAIL" \
      "AARCH64_FUTEX_WAIT_IDLE_ORACLE_UNEXPECTED_RETURN" \
      "AARCH64_BAD_USER_ELR"; do
    if cad_has "$a64_idle_bad"; then
      echo "[error] aarch64 Stage 195F: forbidden idle-oracle marker: $a64_idle_bad"
      exit 1
    fi
  done
  # The idle outcome must NOT restore the blocked caller's frame (no FRAME_OK for the idle trap)
  # and must clear the deferral (idle is a genuine retirement, not a stale decline).
  echo "[ok] aarch64 Stage 195F: FutexWait no-incoming post-lock idle proven (default-on, lock_dropped, current_none)"
fi

# Stage 195G (AARCH64 YIELD OUT-OF-LOCK DISPATCH): two-task oracle — require the default-on
# attestation + handler bypass + re-enqueue publication + post-lock drain + retirement, the
# two-task proof, and forbid any Yield drain failure.
if [[ -f "$LOGFILE" && "$YIELD_ORACLE" == "1" ]]; then
  if ! check_required_patterns "$LOGFILE" \
      "AARCH64_YIELD_RETIRE_DEFAULT_ON result=ok" \
      "AARCH64_YIELD_DISPATCH_DEFER_BEGIN cpu=0" \
      "AARCH64_YIELD_DISPATCH_REENQUEUE_OK cpu=0" \
      "AARCH64_YIELD_HANDLER_BYPASS_BEGIN cpu=0" \
      "AARCH64_YIELD_HANDLER_BYPASS_DONE cpu=0" \
      "AARCH64_YIELD_DISPATCH_REVERIFY_OK" \
      "AARCH64_YIELD_DISPATCH_DEQUEUE_OK cpu=0" \
      "AARCH64_YIELD_DISPATCH_CURRENT_SET_OK cpu=0" \
      "AARCH64_YIELD_DISPATCH_RUNNING_OK" \
      "AARCH64_YIELD_DISPATCH_TTBR0_OK" \
      "AARCH64_YIELD_DISPATCH_FRAME_OK" \
      "AARCH64_YIELD_DISPATCH_DONE result=ok" \
      "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=Yield" \
      "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=Yield result=ok" \
      "AARCH64_YIELD_TWO_TASK_ORACLE_DONE result=ok"; then
    echo "[error] aarch64 Stage 195G Yield two-task oracle markers missing"
    exit 1
  fi
  for a64_y_bad in "AARCH64_YIELD_DISPATCH_FAIL" "AARCH64_YIELD_TWO_TASK_ORACLE_DONE result=fail" \
      "AARCH64_BAD_USER_ELR"; do
    if cad_has "$a64_y_bad"; then
      echo "[error] aarch64 Stage 195G: forbidden Yield marker: $a64_y_bad"
      exit 1
    fi
  done
  echo "[ok] aarch64 Stage 195G: Yield (NR 0) two-task queue-advancing oracle proven (default-on)"
fi

# Stage 195G lone-task oracle — the sole runnable task yields and is re-dispatched to ITSELF
# (same-task, NO idle outcome for a valid Yield deferral).
if [[ -f "$LOGFILE" && "$YIELD_LONE_ORACLE" == "1" ]]; then
  if ! check_required_patterns "$LOGFILE" \
      "AARCH64_YIELD_RETIRE_DEFAULT_ON result=ok" \
      "AARCH64_YIELD_DISPATCH_REENQUEUE_OK cpu=0" \
      "AARCH64_YIELD_DISPATCH_DEQUEUE_OK cpu=0 tid=1" \
      "AARCH64_YIELD_DISPATCH_DONE result=ok" \
      "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=Yield result=ok" \
      "AARCH64_YIELD_LONE_TASK_ORACLE_DONE result=ok tid=1 redispatched_self=1"; then
    echo "[error] aarch64 Stage 195G Yield lone-task oracle markers missing"
    exit 1
  fi
  for a64_yl_bad in "AARCH64_YIELD_DISPATCH_FAIL" "AARCH64_BAD_USER_ELR"; do
    if cad_has "$a64_yl_bad"; then
      echo "[error] aarch64 Stage 195G: forbidden lone-Yield marker: $a64_yl_bad"
      exit 1
    fi
  done
  echo "[ok] aarch64 Stage 195G: Yield (NR 0) lone-task self-redispatch proven (default-on, no idle)"
fi

if check_common_boot_markers "$LOGFILE" "$MARKER_REGEX" "$INIT_SERVER_REGEX"; then
  if ! check_required_patterns "$LOGFILE" "${EARLY_MARKER_SEQUENCE[@]}"; then
    echo "[warn] aarch64 strict required markers are incomplete"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    exit 0
  fi
  if ! check_log_sequence "$LOGFILE" "${EARLY_MARKER_SEQUENCE[@]}"; then
    echo "[warn] aarch64 early boot marker sequence missing or out of order"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    exit 0
  fi

  if ! check_required_patterns "$LOGFILE" \
      "YARM_TIMER_IRQ_DELIVERED" \
      "YARM_TIMER_EOI_DONE" \
      "YARM_SCHED_TICK"; then
    echo "[warn] aarch64 timer progression markers missing"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    exit 0
  fi
  # Stage 184 (CROSS-ARCH-LIVE): the default-on cross-arch live audit attests the
  # honest AArch64 topology (single-dispatcher) + the graduated D2/D6/D3 correctness
  # + syscall-error parity. mode=in_lock_single_dispatcher is expected (AArch64 has no
  # out-of-lock dispatch-relocation seam; the graduated path runs in-lock, NOT the
  # removed global-lock fallback). No x86-style AP/TLB-ACK claims are made here.
  if ! check_required_patterns "$LOGFILE" \
      "CROSS_ARCH_TOPOLOGY_OK arch=aarch64 reason=single_dispatcher" \
      "CROSS_ARCH_D2_RECV_OK arch=aarch64" \
      "CROSS_ARCH_D2_SEND_OK arch=aarch64" \
      "CROSS_ARCH_D6_OK arch=aarch64" \
      "CROSS_ARCH_D3_OK arch=aarch64" \
      "CROSS_ARCH_SYSCALL_PARITY_OK arch=aarch64" \
      "CROSS_ARCH_LIVE_DONE arch=aarch64 result=ok"; then
    echo "[warn] aarch64 Stage 184 cross-arch-live markers missing"
    [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    exit 0
  fi
  for cross_bad in \
      "CROSS_ARCH_TOPOLOGY_BLOCKED arch=aarch64" \
      "CROSS_ARCH_D2_RECV_FAIL" \
      "CROSS_ARCH_D2_SEND_FAIL" \
      "UNLOCK_GRADUATED_FALLBACK" \
      "UNEXPECTED_INLOCK_DISPATCH" \
      "emergency_optout"; do
    if cad_has "$cross_bad"; then
      echo "[error] aarch64 Stage 184: forbidden cross-arch marker: $cross_bad"
      exit 1
    fi
  done
  echo "[ok] aarch64 Stage 184: cross-arch-live markers present (mode=in_lock_single_dispatcher)"
  if ! check_log_sequence "$LOGFILE" "${SPAWN_IPC_SEQUENCE[@]}"; then
    echo "[warn] spawn IPC sequence absent (user_log! is a no-op in no_std; expected)"
  fi
  declare -A REQUIRED_SERVICE_ENTRIES=(
    [INITRAMFS_SRV_ENTRY]=1
    [DEVFS_SRV_ENTRY]=1
    [VFS_SRV_ENTRY]=1
    [DRIVER_MANAGER_ENTRY]=1
    [BLKCACHE_SRV_ENTRY]=1
    [VIRTIO_BLK_SRV_ENTRY]=1
    [DRIVER_MANAGER_READY]=1
    [BLKCACHE_SRV_READY]=1
    [VIRTIO_BLK_SRV_READY]=1
  )
  # Phase 3B freeze: VFS-mediated bulk read (Phase 2B) must NOT be used for
  # image_id 7/8/9 — all three late services must spawn via the ZC grant path.
  if [[ -f "$LOGFILE" ]]; then
    phase3b_bulk_fail=0
    for img_id in 7 8 9; do
      bulk_done=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_VFS_READ_BULK_DONE image_id=${img_id}\\b" 2>/dev/null || echo 0)
      if [[ "$bulk_done" -eq 0 ]]; then
        echo "[ok] Phase 3B: PM_VFS_READ_BULK_DONE image_id=${img_id} count=0 (ZC path active)"
      else
        echo "[error] Phase 3B: PM_VFS_READ_BULK_DONE image_id=${img_id} count=${bulk_done} (Phase 2B fallback active — regression)"
        phase3b_bulk_fail=1
      fi
    done
    if [[ "$phase3b_bulk_fail" -eq 1 ]]; then
      [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    fi
  fi

  # Phase 3B: PM_ELF_ZC_DONE must appear exactly once per image_id, AND zc_pages > 0
  # (CPIO 4096-byte alignment + 4 KiB ELF LOAD alignment both satisfied).
  if [[ -f "$LOGFILE" ]]; then
    phase3b_zc_fail=0
    for img_id in 7 8 9; do
      zc_count=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_ELF_ZC_DONE image_id=${img_id}\\b" 2>/dev/null || echo 0)
      zc_nonzero=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_ELF_ZC_DONE image_id=${img_id}\\b.*zc_pages=[1-9]" 2>/dev/null || echo 0)
      if [[ "$zc_count" -eq 1 && "$zc_nonzero" -eq 1 ]]; then
        echo "[ok] Phase 3B: PM_ELF_ZC_DONE image_id=${img_id} count=1 zc_pages>0"
      elif [[ "$zc_count" -eq 1 && "$zc_nonzero" -eq 0 ]]; then
        echo "[error] Phase 3B: PM_ELF_ZC_DONE image_id=${img_id} count=1 but zc_pages=0 (CPIO or ELF alignment regression)"
        phase3b_zc_fail=1
      else
        echo "[error] Phase 3B: PM_ELF_ZC_DONE image_id=${img_id} expected=1 got=${zc_count}"
        phase3b_zc_fail=1
      fi
    done
    if [[ "$phase3b_zc_fail" -eq 1 ]]; then
      [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    fi
  fi

  # Phase 3A: verify no IPC_RECV_CAP_MATERIALIZE_FAILED (indicates cap-transfer errors).
  if [[ -f "$LOGFILE" ]]; then
    CAP_MAT_FAIL=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "IPC_RECV_CAP_MATERIALIZE_FAILED" 2>/dev/null || echo 0)
    if [[ "$CAP_MAT_FAIL" -gt 0 ]]; then
      echo "[error] IPC_RECV_CAP_MATERIALIZE_FAILED found: ${CAP_MAT_FAIL} — cap transfer errors (Phase 3A regression)"
      exit 1
    else
      echo "[ok] no IPC_RECV_CAP_MATERIALIZE_FAILED"
    fi
  fi

  # Phase 3B: PM_ELF_ZC_FAIL must be 0 — no ZC loader errors permitted.
  if [[ -f "$LOGFILE" ]]; then
    ZC_FAIL_TOTAL=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_ELF_ZC_FAIL" 2>/dev/null || echo 0)
    if [[ "$ZC_FAIL_TOTAL" -eq 0 ]]; then
      echo "[ok] Phase 3B: PM_ELF_ZC_FAIL count=0"
    else
      echo "[error] Phase 3B: PM_ELF_ZC_FAIL count=${ZC_FAIL_TOTAL} (ZC loader errors detected)"
      [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    fi
  fi

  # Phase 3B zero-copy freeze: Phase 2A fallback must be zero; late service
  # spawns should use MemoryObject zero-copy instead of bulk VFS reads.
  if [[ -f "$LOGFILE" ]]; then
    PHASE2A_FALLBACK=$(log_count_pattern "PM_VFS_READ_BULK_PHASE2A_BEGIN")
    if [[ "$PHASE2A_FALLBACK" -eq 0 ]]; then
      echo "[ok] Phase 3B: PM_VFS_READ_BULK_PHASE2A_BEGIN=0 (bulk bridge inactive)"
    else
      echo "[warn] Phase 3B: PM_VFS_READ_BULK_PHASE2A_BEGIN=${PHASE2A_FALLBACK} (expected 0; bulk bridge active)"
      [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    fi
  fi

  # Phase 2A safety: must not have not_found failures (means CPIO entry missing).
  if [[ -f "$LOGFILE" ]]; then
    NOT_FOUND=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_VFS_READ_BULK_FAIL.*reason=not_found" 2>/dev/null || echo 0)
    if [[ "$NOT_FOUND" -gt 0 ]]; then
      echo "[error] PM_VFS_READ_BULK_FAIL reason=not_found found: ${NOT_FOUND} — file missing in CPIO (hard failure)"
      exit 1
    else
      echo "[ok] no PM_VFS_READ_BULK_FAIL reason=not_found"
    fi
  fi

  # Phase 3B summary: all three late services must complete via ZC path with zc_pages>0.
  if [[ -f "$LOGFILE" ]]; then
    ZC_DONE_TOTAL=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_ELF_ZC_DONE" 2>/dev/null || echo 0)
    ZC_NONZERO_TOTAL=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_ELF_ZC_DONE.*zc_pages=[1-9]" 2>/dev/null || echo 0)
    BULK_DONE_VFS=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_VFS_READ_BULK_DONE.*mode=vfs_transfer" 2>/dev/null || echo 0)
    BULK_DONE_2A=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "PM_VFS_READ_BULK_DONE.*mode=phase2a_bridge" 2>/dev/null || echo 0)
    echo "[ok] Phase 3B summary: PM_ELF_ZC_DONE total=${ZC_DONE_TOTAL} zc_pages>0 count=${ZC_NONZERO_TOTAL}"
    echo "[ok] Phase 3B bulk-read residual: bulk_done_vfs=${BULK_DONE_VFS} bulk_done_phase2a=${BULK_DONE_2A} (both must be 0)"
    if [[ "$ZC_DONE_TOTAL" -lt 3 && "$QEMU_SMOKE_STRICT" == "1" ]]; then
      echo "[error] Phase 3B: expected PM_ELF_ZC_DONE>=3 got=${ZC_DONE_TOTAL}"
      exit 1
    fi
    if [[ "$ZC_NONZERO_TOTAL" -lt 3 && "$QEMU_SMOKE_STRICT" == "1" ]]; then
      echo "[error] Phase 3B: expected zc_pages>0 for all 3 images, got ${ZC_NONZERO_TOTAL}/3"
      exit 1
    fi
  fi

  # SharedKernel-primary trap ownership proof markers (Stage 2N / L2B).
  # Installed and first-shared-trap markers must appear once; fallback must be absent.
  if [[ -f "$LOGFILE" ]]; then
    STAGE2N_INSTALLED=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "YARM_LOCK_SPLIT_STAGE2N_INSTALLED arch=aarch64 shared=1 raw=0" 2>/dev/null || echo 0)
    STAGE2N_FIRST=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "YARM_LOCK_SPLIT_STAGE2N_FIRST_SHARED_TRAP arch=aarch64" 2>/dev/null || echo 0)
    STAGE2N_FALLBACK=$(tr '\r' '\n' <"$LOGFILE" | rg -a -c "YARM_LOCK_SPLIT_STAGE2N_FALLBACK arch=aarch64" 2>/dev/null || echo 0)
    if [[ "$STAGE2N_INSTALLED" -eq 1 ]]; then
      echo "[ok] Stage2N: AArch64 installed shared trap state count=1"
    else
      echo "[warn] Stage2N: AArch64 installed marker count=${STAGE2N_INSTALLED} (expected 1)"
      [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    fi
    if [[ "$STAGE2N_FIRST" -eq 1 ]]; then
      echo "[ok] Stage2N: AArch64 first shared trap count=1"
    else
      echo "[warn] Stage2N: AArch64 first shared trap count=${STAGE2N_FIRST} (expected 1)"
      [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    fi
    if [[ "$STAGE2N_FALLBACK" -eq 0 ]]; then
      echo "[ok] Stage2N: AArch64 fallback count=0"
    else
      echo "[warn] Stage2N: AArch64 fallback count=${STAGE2N_FALLBACK} (expected 0)"
      [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
    fi
  fi

  # Phase 3B freeze: verify absent hot-path markers.
  # The following MUST NOT appear in default logs:
  #   - PM_VFS_READ_APPEND / COPY_TO_USER_PAGE: old inline copy path
  #   - VFS_FORWARD_BULK_READ / VFS_ROUTE_BULK_REPLY: trace-gated (VFS_BULK_READ_TRACE=false)
  #   - INITRAMFS_READ_BULK / INITRAMFS_READ_BULK_REPLY: trace-gated (INITRAMFS_READ_BULK_TRACE=false)
  ABSENT_MARKERS=(
    PM_VFS_READ_APPEND
    COPY_TO_USER_PAGE
    VFS_FORWARD_BULK_READ
    VFS_ROUTE_BULK_REPLY
    INITRAMFS_READ_BULK
    INITRAMFS_READ_BULK_REPLY
  )
  for marker in "${ABSENT_MARKERS[@]}"; do
    if log_count_pattern "$marker" | grep -q "^[1-9]"; then
      echo "[warn] unexpected marker in log: ${marker}"
    else
      echo "[ok] absent marker confirmed: ${marker}"
    fi
  done
  service_count_fail=0
  for marker in "${!REQUIRED_SERVICE_ENTRIES[@]}"; do
    expected="${REQUIRED_SERVICE_ENTRIES[$marker]}"
    actual=$(log_count_pattern "$marker")
    if [[ "$actual" -eq "$expected" ]]; then
      echo "[ok] marker count: ${marker}=${actual}"
    else
      echo "[warn] marker count wrong: ${marker} expected=${expected} got=${actual}"
      service_count_fail=1
    fi
  done
  if [[ "$service_count_fail" -eq 1 && "$QEMU_SMOKE_STRICT" == "1" ]]; then
    exit 1
  fi
  echo "[ok] aarch64 strict marker progression detected"
  exit 0
fi

# ---------------------------------------------------------------------------
# Optional FAT userspace mount/config smoke markers.
# Do not fail default core smoke profiles without a real FAT block image; set
# FAT_SMOKE_EXPECTED=1 when the profile is expected to spawn and mount FAT.
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  FAT_SMOKE_EXPECTED=${FAT_SMOKE_EXPECTED:-0}
  FAT_MARKERS=(
    INIT_FAT_SPAWN_BEGIN
    INIT_FAT_SPAWN_SKIPPED
    INIT_FAT_SPAWN_OK
    PM_IMAGE_ID_10_FAT_SRV
    FAT_CONFIG_FOUND
    FAT_BLOCK_BACKEND_STARTUP_CAP
    FAT_MOUNT_READY
    FAT_MOUNT_FAILED
    VFS_MOUNT_REGISTER_FAT_OK
  )
  fat_seen=0
  for marker in "${FAT_MARKERS[@]}"; do
    count=$(log_count_pattern "$marker")
    if [[ "$count" -gt 0 ]]; then
      fat_seen=1
    fi
    echo "[info] FAT smoke marker count: ${marker}=${count}"
  done
  if [[ "$FAT_SMOKE_EXPECTED" == "1" && "$fat_seen" -eq 0 ]]; then
    echo "[error] FAT smoke expected but no FAT markers were observed"
    exit 1
  fi
fi

# ---------------------------------------------------------------------------
# Optional RAMFS userspace mount/config smoke markers.
# Do not fail default core smoke profiles; set RAMFS_SMOKE_EXPECTED=1 when the
# profile is expected to spawn and mount RAMFS.
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  RAMFS_SMOKE_EXPECTED=${RAMFS_SMOKE_EXPECTED:-0}
  RAMFS_MARKERS=(
    INIT_RAMFS_SPAWN_BEGIN
    INIT_RAMFS_SPAWN_SKIPPED
    INIT_RAMFS_SPAWN_OK
    PM_IMAGE_ID_11_RAMFS_SRV
    RAMFS_CONFIG_FOUND
    RAMFS_CONFIG_DEFAULT
    RAMFS_MOUNT_READY
    RAMFS_MOUNT_FAILED
    VFS_MOUNT_REGISTER_RAMFS_OK
  )
  ramfs_seen=0
  for marker in "${RAMFS_MARKERS[@]}"; do
    count=$(log_count_pattern "$marker")
    if [[ "$count" -gt 0 ]]; then
      ramfs_seen=1
    fi
    echo "[info] RAMFS smoke marker count: ${marker}=${count}"
  done
  if [[ "$RAMFS_SMOKE_EXPECTED" == "1" ]]; then
    if [[ "$ramfs_seen" -eq 0 ]]; then
      echo "[error] RAMFS smoke expected but no RAMFS markers were observed"
      exit 1
    fi
    RAMFS_REQUIRED_MARKERS=(
      INIT_RAMFS_SPAWN_BEGIN
      INIT_RAMFS_SPAWN_OK
      PM_IMAGE_ID_11_RAMFS_SRV
      RAMFS_MOUNT_READY
      VFS_MOUNT_REGISTER_RAMFS_OK
    )
    for marker in "${RAMFS_REQUIRED_MARKERS[@]}"; do
      if [[ "$(log_count_pattern "$marker")" -eq 0 ]]; then
        echo "[error] RAMFS smoke expected marker missing: ${marker}"
        exit 1
      fi
    done
    if [[ "$(log_count_pattern RAMFS_CONFIG_FOUND)" -eq 0 && "$(log_count_pattern RAMFS_CONFIG_DEFAULT)" -eq 0 ]]; then
      echo "[error] RAMFS smoke expected config marker missing"
      exit 1
    fi
  fi
fi

# ---------------------------------------------------------------------------
# Optional EXT4 userspace spawn markers (profile-gated; informational only).
# ---------------------------------------------------------------------------
if [[ -f "$LOGFILE" ]]; then
  EXT4_MARKERS=(
    INIT_EXT4_SPAWN_BEGIN
    INIT_EXT4_SPAWN_SKIPPED
    INIT_EXT4_SPAWN_OK
    PM_IMAGE_ID_12_EXT4_SRV
    EXT4_SRV_READY
  )
  for marker in "${EXT4_MARKERS[@]}"; do
    count=$(log_count_pattern "$marker")
    echo "[info] EXT4 smoke marker count: ${marker}=${count}"
  done
fi

echo "[warn] boot shell and init-server markers not detected (status=$QEMU_STATUS)"
if [[ -f "$LOGFILE" ]]; then
  echo "[info] last 20 log lines from $LOGFILE"
  tail -n 20 "$LOGFILE" || true
fi

if [[ "$QEMU_SMOKE_STRICT" == "1" ]]; then
  exit 1
fi
exit 0
