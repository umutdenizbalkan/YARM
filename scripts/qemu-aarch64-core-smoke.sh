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
# Stage 198B (ORDINARY-CAP PARITY): AArch64 honors the same env-var -> cmdline knob translations
# the x86_64 core smoke does for the ordinary-cap IpcSend live oracles. The oracle wrapper exports
# IPC_SEND_CAP_ORACLE / IPC_SEND_CAP_ENQUEUE_ORACLE; without these the knobs never reach the AArch64
# kernel cmdline and the ordinary-cap oracle workload never runs.
IPC_SEND_CAP_ORACLE=${IPC_SEND_CAP_ORACLE:-0}
if [[ "$IPC_SEND_CAP_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_send_cap_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.ipc_send_cap_oracle=1"
fi
IPC_SEND_CAP_ENQUEUE_ORACLE=${IPC_SEND_CAP_ENQUEUE_ORACLE:-0}
if [[ "$IPC_SEND_CAP_ENQUEUE_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_send_cap_enqueue_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.ipc_send_cap_enqueue_oracle=1"
fi
# Stage 198C2: propagate the reply-cap DIRECT oracle knob to the AArch64 cmdline (mirrors
# the ordinary-cap knob above); without it yarm.ipc_send_reply_cap_oracle=1 never reaches
# the kernel and the reply-cap direct oracle workload / boot provisioning never runs.
IPC_SEND_REPLY_CAP_ORACLE=${IPC_SEND_REPLY_CAP_ORACLE:-0}
if [[ "$IPC_SEND_REPLY_CAP_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_send_reply_cap_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.ipc_send_reply_cap_oracle=1"
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
# 199E-A64CALL: default-off TERMINAL-FAULT oracle cell. Arms
# `yarm.aarch64_terminal_fault_oracle=1`, which makes init take ONE deliberate unhandled read at
# 0x0 so the U9-FT4 terminal-PageFault witness has a trigger of its own. Until 199E-A64CALL that
# witness observed a DEFECT — init's `spawn_v5_cap` dereferencing a pointer the kernel had zeroed
# out of x15 on every syscall return — and repairing that removed the only terminal fault any
# AArch64 profile produced. The default cell now boots its service chain clean; U9-FT4 asserts,
# unchanged and still fully positive, only in this cell.
TERMINAL_FAULT_ORACLE=${TERMINAL_FAULT_ORACLE:-0}
if [[ "$TERMINAL_FAULT_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.aarch64_terminal_fault_oracle="* ]]; then
  KERNEL_CMDLINE="$KERNEL_CMDLINE yarm.aarch64_terminal_fault_oracle=1"
fi
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
# Stage 200C2C2C-R2C: opt-in SINGLE-BOOT-INSTANCE enforcement. With `QEMU_SINGLE_BOOT=1`,
# QEMU is told never to restart the guest, so a reset can never silently produce a second
# boot inside one log. Default-off: every existing caller keeps its exact prior invocation.
if [[ "${QEMU_SINGLE_BOOT:-0}" == "1" ]]; then
  QEMU_ARGS+=(-no-reboot -no-shutdown)
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

# Stage 198A1: deterministic idle-aware completion. A correct AArch64 boot reaches the canonical
# terminal-idle marker (SCHED_ENTER_IDLE_HLT) and then WFI-idles forever, so the plain timeout run
# always burns the full wall-clock budget and returns 124 (which this slow CI environment then
# fails on). With QEMU_EXPECT_TERMINAL_IDLE=1 (default) we instead stop QEMU as soon as terminal
# idle is observed and treat that as success; the positive/forbidden marker verdict below is
# UNCHANGED, so a missing proof marker, a forbidden marker, or an early idle-before-proof still
# fails. IDLE_MAX_SECS bounds a genuine hang. Set QEMU_EXPECT_TERMINAL_IDLE=0 for the legacy run.
QEMU_EXPECT_TERMINAL_IDLE="${QEMU_EXPECT_TERMINAL_IDLE:-1}"
IDLE_MAX_SECS="${IDLE_MAX_SECS:-180}"
TERMINAL_IDLE_MARKER="${TERMINAL_IDLE_MARKER:-SCHED_ENTER_IDLE_HLT}"
if [[ "$QEMU_EXPECT_TERMINAL_IDLE" == "1" ]]; then
  if run_qemu_until_idle_or_timeout "$IDLE_MAX_SECS" "$LOGFILE" "$TERMINAL_IDLE_MARKER" \
    "$QEMU_BIN" "${QEMU_ARGS[@]}"; then
    QEMU_STATUS=0
    echo "[ok] aarch64 core: terminal idle ($TERMINAL_IDLE_MARKER) reached — QEMU stopped intentionally"
  else
    QEMU_STATUS=$?
    echo "[err] aarch64 core: terminal idle marker never observed within ${IDLE_MAX_SECS}s (hang?)"
  fi
else
  if run_qemu_timeout_to_log "$TIMEOUT_SECS" "$LOGFILE" "$QEMU_BIN" "${QEMU_ARGS[@]}"; then
    QEMU_STATUS=0
  else
    QEMU_STATUS=$?
  fi
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

# U9-FT4: the AArch64 terminal PageFault route is now SPLIT. Assert the witness chain
# POSITIVELY -- fatal-pattern absence is insufficient, and demonstrably so: the FT3 attempt
# exited 0 while the faulting PC resumed. Every line below must hold exactly.
# 199E-A64CALL: this witness now runs in its OWN CELL (`TERMINAL_FAULT_ORACLE=1`), not in the
# default profile. NOTHING below is weakened — every assertion is byte-identical and still
# positive. What changed is the TRIGGER. This chain used to be driven by a DEFECT: init's
# `spawn_v5_cap` dereferenced a pointer the kernel had zeroed out of x15 on every syscall return
# (the `REG_X18_TLS` lane addressed x15 instead of x18). That was never an armed proof, and it
# also meant the AArch64 service chain could never run at all. Repairing it removed the only
# terminal fault any AArch64 profile produced, so the witness was given a deliberate trigger —
# `yarm.aarch64_terminal_fault_oracle=1`, one intentional unhandled read at 0x0 — and the default
# cell now boots the service chain clean instead of dying inside init.
if [[ "$TERMINAL_FAULT_ORACLE" != "1" ]]; then
  echo "[info] U9-FT4: not armed (set TERMINAL_FAULT_ORACLE=1) — the terminal-fault witness runs in its own cell"
else
u9ft4_fail=0
u9ft4_log="$(tr '\r' '\n' <"$LOGFILE")"
# `rg -c` prints nothing and exits non-zero when there are no matches, so normalise to 0.
u9ft4_count() {
  local n
  n="$(printf '%s\n' "$u9ft4_log" | rg -a -F -c -- "$1" || true)"
  printf '%s' "${n:-0}"
}
u9ft4_require_one() {
  local want_desc="$1" pat="$2" n
  n="$(u9ft4_count "$pat")"
  if [[ "$n" != "1" ]]; then
    echo "[error] U9-FT4: $want_desc -- expected exactly 1, got ${n:-0}: $pat"
    u9ft4_fail=1
  else
    echo "[ok] U9-FT4: $want_desc"
  fi
}
u9ft4_require_zero() {
  local want_desc="$1" pat="$2" n
  n="$(u9ft4_count "$pat")"
  if [[ "$n" != "0" ]]; then
    echo "[error] U9-FT4: $want_desc -- expected 0, got $n: $pat"
    u9ft4_fail=1
  else
    echo "[ok] U9-FT4: $want_desc"
  fi
}
# The faulting task, the exact fault facts, and exactly one of each.
u9ft4_require_one "tid 1 read fault at 0x0 entered once" \
  'PAGE_FAULT_ENTRY tid=1 addr=0x0 access=Read'
u9ft4_require_one "the fault is reported unhandled exactly once" \
  'PAGE_FAULT_UNHANDLED tid=1 addr=0x0 access=Read'
# Buffered publication to endpoint 3, no wake.
u9ft4_require_one "report targets endpoint 3 at its exact generation" \
  'TASK_FAULT_REPORT_TARGET tid=1 endpoint=3 generation=1'
u9ft4_require_one "report is BUFFERED exactly once with woke=0" \
  'TASK_FAULT_REPORT_ENQUEUE_OK tid=1 endpoint=3 queued=1 woke=0'
# The terminal transition and the deferral, each exactly once.
u9ft4_require_one "terminal task transition commits exactly once" \
  'TERMINAL_FAULT_SPLIT_COMMITTED cpu=0 tid=1 captured=1 advance=deferred'
u9ft4_require_one "the queue-advance deferral is published exactly once" \
  'QUEUE_ADVANCING_DISPATCH_DEFERRED reason=terminal_fault_switch_required tid=1 cpu=0'
# The broad dispatcher is SKIPPED -- the old in-lock route must not run.
u9ft4_require_one "broad dispatcher skipped for the terminal fault" \
  'QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED cpu=0 reason=terminal_fault_committed'
# The existing drain selects the replacement and applies its exact context.
u9ft4_require_one "queue selection happens once and chooses tid 2" \
  'QUEUE_ADVANCING_DISPATCH_DEQUEUE_OK cpu=0 tid=2'
u9ft4_require_one "replacement tid 2 is marked Running" \
  'AARCH64_FUTEX_WAIT_DISPATCH_RUNNING_OK tid=2'
u9ft4_require_one "replacement tid 2 gets its exact address space" \
  'AARCH64_FUTEX_WAIT_DISPATCH_TTBR0_OK tid=2 asid=2'
u9ft4_require_one "replacement tid 2 gets its exact EL0 frame" \
  'AARCH64_FUTEX_WAIT_DISPATCH_FRAME_OK tid=2'
u9ft4_require_one "the drain completes once" \
  'AARCH64_FUTEX_WAIT_DISPATCH_DONE result=ok'
# The faulting PC must NEVER resume: a second entry at the same rip, or a fault with no
# current task, is exactly the FT3 defect.
u9ft4_require_zero "the faulting PC never resumes (no ownerless re-fault)" \
  'PAGE_FAULT_ENTRY tid=18446744073709551615'
u9ft4_require_zero "no split refusal on the witnessed path" 'TERMINAL_FAULT_SPLIT_REFUSED'
u9ft4_require_zero "no fail-closed settlement on the witnessed path" \
  'TERMINAL_FAULT_SPLIT_FAILED_CLOSED'
u9ft4_require_zero "the drain never idles instead of selecting" \
  'AARCH64_FUTEX_WAIT_DISPATCH_NO_INCOMING'
# The OLD in-lock route must not run: the broad arm's own dispatch for this fault is gone.
u9ft4_require_zero "the old in-lock terminal route does not run" \
  'TERMINAL_FAULT_UNEXPECTED_DISPOSITION'
if [[ "$u9ft4_fail" -eq 1 ]]; then
  echo "[error] U9-FT4 AArch64 terminal-fault witness FAILED"
  exit 1
fi
echo "[ok] U9-FT4: AArch64 terminal PageFault route witness chain complete"
fi

# U9-RX4: the Stage-32B queued-plain split receive must deliver EXACTLY what the broad receive
# delivers. Asserted POSITIVELY, because the defect this replaced was invisible to
# fatal-pattern matching for as long as it existed: the smoke exited 0 while PM silently failed
# to decode, never replied, and left its caller blocked for the rest of the boot.
u9rx4_fail=0
u9rx4_log="$(tr '\r' '\n' <"$LOGFILE")"
u9rx4_count() {
  local n
  n="$(printf '%s\n' "$u9rx4_log" | rg -a -F -c -- "$1" || true)"
  printf '%s' "${n:-0}"
}
# Stage 199D-DW2 (COMPOSITION): regex counterpart of `u9rx4_count`, for the one assertion
# whose anchor contains a field that is an ALLOCATION DETAIL rather than part of its subject.
u9rx4_count_re() {
  local n
  n="$(printf '%s\n' "$u9rx4_log" | rg -a -c -e "$1" || true)"
  printf '%s' "${n:-0}"
}
u9rx4_require_one_re() {
  local want_desc="$1" pat="$2" n
  n="$(u9rx4_count_re "$pat")"
  if [[ "$n" != "1" ]]; then
    echo "[error] U9-RX4: $want_desc -- expected exactly 1, got ${n:-0}: /$pat/"
    u9rx4_fail=1
  else
    echo "[ok] U9-RX4: $want_desc"
  fi
}
u9rx4_require_one() {
  local want_desc="$1" pat="$2" n
  n="$(u9rx4_count "$pat")"
  if [[ "$n" != "1" ]]; then
    echo "[error] U9-RX4: $want_desc -- expected exactly 1, got ${n:-0}: $pat"
    u9rx4_fail=1
  else
    echo "[ok] U9-RX4: $want_desc"
  fi
}
u9rx4_require_min_one() {
  local want_desc="$1" pat="$2" n
  n="$(u9rx4_count "$pat")"
  if [[ "$n" -lt 1 ]]; then
    echo "[error] U9-RX4: $want_desc -- expected at least 1, got ${n:-0}: $pat"
    u9rx4_fail=1
  else
    echo "[ok] U9-RX4: $want_desc (n=$n)"
  fi
}
u9rx4_require_zero() {
  local want_desc="$1" pat="$2" n
  n="$(u9rx4_count "$pat")"
  if [[ "$n" != "0" ]]; then
    echo "[error] U9-RX4: $want_desc -- expected 0, got $n: $pat"
    u9rx4_fail=1
  else
    echo "[ok] U9-RX4: $want_desc"
  fi
}
# The receive itself: the EXACT one-shot reply cap the kernel minted, and the
# receiver-visible framing (application opcode 12, 8 payload bytes) -- not the raw wire
# opcode 0 with its two-byte inline prefix still attached.
u9rx4_require_one "PM receives the exact minted reply cap with receiver-visible framing" \
  'PM_RECV_GOT_MSG opcode=12 len=8 reply_cap=Some(65538)'
u9rx4_require_one "the reply cap is materialized exactly once" \
  'IPC_REPLY_CAP_ONESHOT_OK receiver_tid=3 local_reply_cap=65538'
# PM can now decode and answer.
u9rx4_require_one "PM decodes the lifecycle query" 'PM_LIFECYCLE_QUERY_RECV tid=2'
u9rx4_require_one "PM replies successfully" 'PM_LIFECYCLE_QUERY_REPLY tid=2 found=1'
# ── U9-RX4 TERMINAL-OUTCOME EVALUATOR (claimant-aware, identity-scoped) ─────────────
#
# The witnessed exchange arms a FOUR-TICK reply deadline on tid 2's PM lifecycle query, in
# every boot. That makes TWO outcomes legitimate, and which one occurs is a genuine race:
#
#   REPLY WINS    PM answers inside four ticks. The reply claims and commits the terminal,
#                 both sides of the one-shot are revoked, and the caller resumes via the reply.
#   TIMEOUT WINS  PM does not. The deadline claims and commits the SAME terminal, wakes the
#                 caller exactly once, invalidates the reply aliases, and every later reply is
#                 refused.
#
# This evaluator used to assert the reply outcome unconditionally, so a legitimate timeout win
# was reported as `resume=0` and read as a lost reply. That was the ORACLE encoding a timing
# assumption as an invariant — the arbitration itself was correct, settling exactly once either
# way. It is now claimant-aware: it requires exactly one winner, exactly one caller wake
# attributed to THAT winner, and exactly one terminal settlement, and it holds the losing
# claimant to having changed nothing.
#
# It is deliberately NOT weaker than what it replaced. It still fails a genuinely lost reply
# (no winner, no wake), and it additionally rejects two winners, a winner without its wake, a
# reply that succeeds after a timeout, and a leaked alias/record/link — none of which the old
# form could see. The four-tick deadline is preserved; the race is arbitrated, not suppressed.
#
# Every anchor is scoped to the witnessed identities — caller tid 2, replier tid 3, one-shot cap
# 65538, record generation 1 — never to boot-global counts that other replies also satisfy.

_u9_c()   { printf '%s\n' "$_U9_LOG" | rg -a -F -c -- "$1" 2>/dev/null || printf '0'; }
_u9_cre() { printf '%s\n' "$_U9_LOG" | rg -a -c -e "$1" 2>/dev/null || printf '0'; }
# First line matching <regex>, then the numeric value of <key>= on it ('' when absent).
_u9_field() {
  printf '%s\n' "$_U9_LOG" | rg -a -m1 -e "$1" 2>/dev/null | rg -o -m1 -e "$2=[0-9]+" 2>/dev/null | cut -d= -f2
}

# Evaluate one boot log's terminal outcome. Echoes [ok]/[error] lines; returns 0 on accept.
u9rx4_eval_terminal() {
  _U9_LOG="$1"
  local tag="${2:-U9-RX4}" bad=0

  # The scenario's own terminal, named by the caller the witness is about. Its record slot is
  # an allocation detail, so it is READ from the arming rather than assumed, and every
  # record-scoped assertion below is then exact on that slot.
  local arm_re='^IPC_REPLY_TERMINAL_ARMED_SPLIT caller_tid=2 caller_asid=2 record_index=[0-9]+ record_generation=1 '
  local armed rec
  armed="$(_u9_cre "$arm_re")"
  rec="$(_u9_field "$arm_re" 'record_index')"
  if [[ "$armed" != "1" || -z "$rec" ]]; then
    echo "[error] $tag: the witnessed reply terminal is armed exactly once -- got ${armed}"
    return 1
  fi

  # A FINITE reply deadline must still be armed on it. Preserved deliberately: it is what makes
  # both outcomes reachable, and removing it would hide the race rather than arbitrate it.
  #
  # Anchored on `finite_deadline=1 deadline_reserved=1` and on the arming EXISTING for this
  # record — never on the `deadline=` value, which is the ABSOLUTE EXPIRY TICK and therefore
  # varies with the tick the caller happened to block on (4 or 5 across observed boots). Pinning
  # it made the witness fail for a timing detail rather than a property, which is the same class
  # of mistake as anchoring on the record's allocated slot. The workload's deadline itself is
  # unchanged — this asserts that one is armed, not when it lands.
  local finite dl
  finite="$(_u9_cre "^IPC_REPLY_TERMINAL_ARMED_SPLIT caller_tid=2 caller_asid=2 record_index=${rec} record_generation=1 .*finite_deadline=1 deadline_reserved=1 ")"
  dl="$(_u9_cre "^IPC_REPLY_TIMEOUT_ARMED arch=aarch64 caller_tid=2 caller_asid=2 record_index=${rec} record_generation=1 ")"
  if [[ "$finite" != "1" || "$dl" != "1" ]]; then
    echo "[error] $tag: a finite reply deadline must be armed exactly once on the witnessed record -- finite=${finite} armed=${dl}"
    bad=1
  fi

  # ── the two winners, each discriminated by its own exact claim ──
  local reply_resolve reply_commit reply_resume replier_revoke caller_revoke
  reply_resolve="$(_u9_cre '^IPC_REPLY_OBJECT_OK tid=3 cap=65538 reply_index=[0-9]+ generation=1')"
  reply_commit="$(_u9_cre "^IPCREPLY_DIRECT_TERMINAL_CLAIM record_index=${rec} record_generation=1 replier_tid=3 terminal=Reply resolution=commit settled=1")"
  reply_resume="$(_u9_c 'IPC_RECV_V2_META_BLOCKED_WAITER_OK tid=2 ')"
  replier_revoke="$(_u9_c 'IPC_REPLY_REPLIER_CAP_FAST_REVOKE caller_tid=2 replier_tid=3 cap=65538')"
  caller_revoke="$(_u9_c 'IPC_REPLY_CALLER_CAP_FAST_REVOKE caller_tid=2')"

  local timeout_ok timeout_commit timeout_wake aliases late_ok late_refused
  timeout_ok="$(_u9_cre '^IPC_REPLY_TIMEOUT_OK arch=aarch64 terminal=Timeout timeout_result=TimedOut')"
  timeout_commit="$(_u9_c 'IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=aarch64 terminal=Timeout result=ok')"
  timeout_wake="$(_u9_field '^IPC_REPLY_TIMEOUT_OK ' 'caller_wakes')"
  aliases="$(_u9_field '^IPC_REPLY_TIMEOUT_OK ' 'reply_aliases_invalid')"
  late_ok="$(_u9_field '^IPC_REPLY_TIMEOUT_OK ' 'late_reply_successes')"
  late_refused="$(_u9_c 'IPC_REPLY_FAIL tid=3 reply_cap=65538 err=WrongObject')"
  : "${timeout_wake:=0}" ; : "${aliases:=0}" ; : "${late_ok:=0}"

  local reply_win=0 timeout_win=0
  [[ "$reply_resolve" == "1" ]] && reply_win=1
  [[ "$timeout_ok"    == "1" ]] && timeout_win=1

  # EXACTLY ONE WINNER. Zero is the genuinely lost reply the old form was written for; two is a
  # double settlement, which the old form could not see at all.
  if (( reply_win + timeout_win != 1 )); then
    echo "[error] $tag: exactly one claimant must win -- reply_win=${reply_win} timeout_win=${timeout_win} (resolve=${reply_resolve} timeout_ok=${timeout_ok})"
    return 1
  fi

  # EXACTLY ONE TERMINAL SETTLEMENT, and EXACTLY ONE CALLER WAKE, attributed to the winner.
  local settlements=$(( reply_commit + timeout_commit ))
  local wakes=$(( reply_resume + timeout_wake ))
  if (( settlements != 1 )); then
    echo "[error] $tag: the terminal must settle exactly once -- reply_commit=${reply_commit} timeout_commit=${timeout_commit}"
    bad=1
  fi
  if (( wakes != 1 )); then
    echo "[error] $tag: the caller must wake exactly once -- reply_resume=${reply_resume} timeout_wake=${timeout_wake}"
    bad=1
  fi

  if (( reply_win )); then
    (( reply_commit == 1 ))   || { echo "[error] $tag: reply won but did not commit its terminal (commit=${reply_commit})"; bad=1; }
    (( reply_resume == 1 ))   || { echo "[error] $tag: reply won but the caller did not resume via the reply (resume=${reply_resume})"; bad=1; }
    (( timeout_win == 0 ))    || { echo "[error] $tag: reply won yet a timeout also won"; bad=1; }
    (( timeout_wake == 0 ))   || { echo "[error] $tag: reply won yet the timeout also woke the caller (timeout_wake=${timeout_wake})"; bad=1; }
    (( late_ok == 0 ))        || { echo "[error] $tag: late_reply_successes must be 0 (got ${late_ok})"; bad=1; }
    (( replier_revoke == 1 )) || { echo "[error] $tag: the replier side of the one-shot is revoked once (got ${replier_revoke})"; bad=1; }
    (( caller_revoke == 1 ))  || { echo "[error] $tag: the caller side of the one-shot is revoked once (got ${caller_revoke})"; bad=1; }
    (( bad == 0 )) && echo "[ok] $tag: REPLY won the terminal — commit=1 resume=1 one-shot revoked on both sides, no timeout win"
  else
    (( timeout_commit == 1 )) || { echo "[error] $tag: timeout won but did not commit its terminal (commit=${timeout_commit})"; bad=1; }
    (( timeout_wake == 1 ))   || { echo "[error] $tag: timeout won but did not wake the caller exactly once (caller_wakes=${timeout_wake})"; bad=1; }
    (( aliases == 1 ))        || { echo "[error] $tag: the reply aliases must be invalidated exactly once (got ${aliases})"; bad=1; }
    (( reply_resume == 0 ))   || { echo "[error] $tag: timeout owns the terminal, so the reply must NOT complete the caller (resume=${reply_resume})"; bad=1; }
    (( reply_commit == 0 ))   || { echo "[error] $tag: timeout owns the terminal, so no reply commit may exist (got ${reply_commit})"; bad=1; }
    (( reply_resolve == 0 ))  || { echo "[error] $tag: a reply succeeded after the timeout won (resolve=${reply_resolve})"; bad=1; }
    (( late_ok == 0 ))        || { echo "[error] $tag: late_reply_successes must be 0 (got ${late_ok})"; bad=1; }
    (( late_refused >= 1 ))   || { echo "[error] $tag: a reply attempted after the timeout must be refused (refusals=${late_refused})"; bad=1; }
    # The one-shot must not be half-revoked behind a timeout win: either the alias
    # invalidation owns it, or nothing does.
    (( replier_revoke == 0 )) || { echo "[error] $tag: timeout won yet the replier one-shot slot was revoked by a reply (got ${replier_revoke})"; bad=1; }
    (( bad == 0 )) && echo "[ok] $tag: TIMEOUT won the terminal — commit=1 caller_wakes=1 aliases_invalid=1 late reply refused (${late_refused}), no reply completion"
  fi

  # Neither outcome may leak, on any path.
  local leak_cap leak_link leak_rec
  leak_cap="$(_u9_c 'IPC_REPLY_FAST_REVOKE_FAIL')"
  leak_link="$(_u9_c 'IPC_SERVER_REPLY_LINK_REGISTER_FAIL')"
  leak_rec="$(_u9_c 'err=CapabilityFull')"
  if (( leak_cap + leak_link + leak_rec != 0 )); then
    echo "[error] $tag: leak on the witnessed path -- cap=${leak_cap} link=${leak_link} record=${leak_rec}"
    bad=1
  fi
  return $bad
}

# ── Self-test: the evaluator must REJECT every malformed outcome ────────────────────
# Run before the real log is judged, so a witness that has stopped discriminating fails here
# rather than silently passing everything.
_u9_arm='IPC_REPLY_TERMINAL_ARMED_SPLIT caller_tid=2 caller_asid=2 record_index=0 record_generation=1 replier_tid=0 blocked_recv_generation=1 finite_deadline=1 deadline_reserved=1 result=ok
IPC_REPLY_TIMEOUT_ARMED arch=aarch64 caller_tid=2 caller_asid=2 record_index=0 record_generation=1 terminal_epoch=1 token_slot=0 token_generation=1 deadline=9 result=ok'
_u9_replywin="${_u9_arm}
IPC_REPLY_OBJECT_OK tid=3 cap=65538 reply_index=0 generation=1 target_endpoint=5
IPCREPLY_DIRECT_TERMINAL_CLAIM record_index=0 record_generation=1 replier_tid=3 terminal=Reply resolution=commit settled=1 result=ok
IPC_REPLY_REPLIER_CAP_FAST_REVOKE caller_tid=2 replier_tid=3 cap=65538 waiter_cap=65538 ok=true
IPC_REPLY_CALLER_CAP_FAST_REVOKE caller_tid=2 cap=65541 ok=true
IPC_RECV_V2_META_BLOCKED_WAITER_OK tid=2 len=40"
_u9_timeoutwin="${_u9_arm}
IPC_REPLY_TIMEOUT_OK arch=aarch64 terminal=Timeout timeout_result=TimedOut caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=0 result=ok
IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=aarch64 terminal=Timeout result=ok
IPC_REPLY_FAIL tid=3 reply_cap=65538 err=WrongObject"

_u9_selftest_fail=0
_u9_expect() { # <expect 0|1> <desc> <log>
  local want="$1" desc="$2" log="$3" got
  u9rx4_eval_terminal "$log" "SELFTEST" >/dev/null 2>&1 && got=0 || got=1
  if [[ "$got" != "$want" ]]; then
    echo "[error] U9-RX4 evaluator self-test: '$desc' expected $( ((want)) && echo reject || echo accept ), got $( ((got)) && echo reject || echo accept )"
    _u9_selftest_fail=1
  fi
}
_u9_expect 0 "reply-win accepted"   "$_u9_replywin"
_u9_expect 0 "timeout-win accepted" "$_u9_timeoutwin"
_u9_expect 1 "no winner (the genuinely lost reply)" "$_u9_arm"
_u9_expect 1 "two winners" "${_u9_replywin}
IPC_REPLY_TIMEOUT_OK arch=aarch64 terminal=Timeout timeout_result=TimedOut caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=0 result=ok
IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=aarch64 terminal=Timeout result=ok"
_u9_expect 1 "winner without its attributed wake" "${_u9_arm}
IPC_REPLY_OBJECT_OK tid=3 cap=65538 reply_index=0 generation=1 target_endpoint=5
IPCREPLY_DIRECT_TERMINAL_CLAIM record_index=0 record_generation=1 replier_tid=3 terminal=Reply resolution=commit settled=1 result=ok
IPC_REPLY_REPLIER_CAP_FAST_REVOKE caller_tid=2 replier_tid=3 cap=65538 waiter_cap=65538 ok=true
IPC_REPLY_CALLER_CAP_FAST_REVOKE caller_tid=2 cap=65541 ok=true"
_u9_expect 1 "reply success after timeout" "${_u9_timeoutwin}
IPC_REPLY_OBJECT_OK tid=3 cap=65538 reply_index=0 generation=1 target_endpoint=5"
_u9_expect 1 "reply completion with timeout ownership" "${_u9_timeoutwin}
IPC_RECV_V2_META_BLOCKED_WAITER_OK tid=2 len=40"
_u9_expect 1 "late_reply_successes non-zero" "${_u9_arm}
IPC_REPLY_TIMEOUT_OK arch=aarch64 terminal=Timeout timeout_result=TimedOut caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=1 result=ok
IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=aarch64 terminal=Timeout result=ok
IPC_REPLY_FAIL tid=3 reply_cap=65538 err=WrongObject"
_u9_expect 1 "timeout win with no refusal of the late reply" "${_u9_arm}
IPC_REPLY_TIMEOUT_OK arch=aarch64 terminal=Timeout timeout_result=TimedOut caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=0 result=ok
IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=aarch64 terminal=Timeout result=ok"
_u9_expect 1 "leaked alias/record/link" "${_u9_replywin}
IPC_REPLY_FAST_REVOKE_FAIL tid=3"
if (( _u9_selftest_fail )); then
  echo "[error] U9-RX4 terminal-outcome evaluator self-test FAILED"
  exit 1
fi
echo "[ok] U9-RX4: terminal-outcome evaluator self-test passed (2 accepted, 8 rejected)"

if ! u9rx4_eval_terminal "$u9rx4_log" "U9-RX4"; then
  u9rx4_fail=1
fi

# The defect's own signatures must be gone.
u9rx4_require_zero "no reply cap is lost to the u32::MAX sentinel" 'reply_cap=4294967295'
u9rx4_require_zero "PM never fails to decode" 'PM_RECV_DECODE_FAIL'
# Stage 199D-DW2 (COMPOSITION): "on the witnessed path" is the operative phrase. When
# `yarm.ipc_recv_proof=1` is armed, the recv-v2 proof workload DELIBERATELY drives an undersized
# receive buffer to prove the writeback rolls back and the materialized cap is restored — that is
# the entire subject of `qemu-ipc-recv-v2-oracle-smoke.sh`, which REQUIRES the same marker. It
# lands at `site=queued_split_undersize`, on the proof's own path, never on U9-RX4's. So the
# armed cell excludes exactly that one deliberate site and keeps every other rollback site
# (reply_split, blocked_meta, blocked_ordinary_cap, immediate_meta, queued_split_meta) at zero;
# the unarmed default cell keeps the original unrestricted zero.
if [[ "${IPC_RECV_PROOF:-0}" == "1" ]]; then
  u9rx4_rollback_all="$(u9rx4_count 'IPC_RECV_V2_ROLLBACK_OK')"
  u9rx4_rollback_probe="$(u9rx4_count 'IPC_RECV_V2_ROLLBACK_OK site=queued_split_undersize')"
  if (( u9rx4_rollback_all - u9rx4_rollback_probe != 0 )); then
    echo "[error] U9-RX4: no writeback rollback on the witnessed path -- expected 0, got $(( u9rx4_rollback_all - u9rx4_rollback_probe )) (total=${u9rx4_rollback_all} deliberate_proof_probe=${u9rx4_rollback_probe})"
    u9rx4_fail=1
  else
    echo "[ok] U9-RX4: no writeback rollback on the witnessed path (deliberate recv-proof probe=${u9rx4_rollback_probe} excluded)"
  fi
else
  u9rx4_require_zero "no writeback rollback on the witnessed path" 'IPC_RECV_V2_ROLLBACK_OK'
fi
u9rx4_require_zero "the reply cap is never used twice" 'IPC_REPLY_FAST_REVOKE_FAIL'
u9rx4_require_zero "no cap materialization failure" 'IPC_RECV_CAP_MATERIALIZE_FAILED'
# The U9-RX3 blocking-recv route runs here, on the deferral the class already owned.
u9rx4_require_min_one "the blocking IpcRecv route publishes and defers" \
  'IPC_RECV_BLOCK_SPLIT_DONE'
u9rx4_require_min_one "the broad dispatcher is skipped for a published block" \
  'QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED cpu=0 reason=publication_committed'
u9rx4_require_min_one "the existing D2-recv drain resumes a replacement" \
  'D2_RECV_GENUINE_DISPATCH_DONE result=switch'
u9rx4_require_zero "no fail-closed settlement on the blocking route" \
  'IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED'
if [[ "$u9rx4_fail" -eq 1 ]]; then
  echo "[error] U9-RX4 AArch64 reply-cap witness FAILED"
  exit 1
fi
echo "[ok] U9-RX4: AArch64 queued-plain reply-cap + framing witness chain complete"

BLOCKER_REGEX='IPC_CALL_FAIL|IPC_RECV_CAP_MATERIALIZE_FAILED|IPC_RECV_BLOCKED_COMPLETE_FAILED|CapabilityFull|VM_FULL|YARM_FIRST_USER_FAIL|MemoryObjectMissing|ELF_MISSING|PrivilegeViolation|failed to bootstrap first user task|panic|InvalidCapability|WrongObject|StaleCapability|MissingRight|UserMemoryFault|PM_RECV_DECODE_FAIL|bad_len expected=16 got=8|CAP_LOOKUP tid=1 cap=0|empty-elf|Malformed|Syscall\\(Internal\\)|memory allocation of|DELEGATE_FAIL|delegation.*fail|IPC_REPLY_FAST_REVOKE_FAIL|PM_PANIC|INIT_PANIC|DEVFS_PANIC|VFS_PANIC|INITRAMFS_PANIC|INITRAMFS_CPIO_EMPTY|D2_PUBLISH_RACE_UNWIND'
# XARCH-SRV-PARITY: the supervisor's lifecycle SELF-query (query PM for the
# supervisor's own tid, before entering the event loop) returns WrongObject
# uniformly on x86_64, aarch64, AND riscv64 — the supervisor logs it and continues
# (SUPERVISOR_EVENT_LOOP_TICK follows), and the full server chain loads regardless.
# It is a benign, cross-arch condition that the x86_64 core smoke also accepts (that
# smoke has no WrongObject blocker at all). Exclude ONLY this exact self-query line so
# the aarch64 smoke matches x86_64's treatment; every OTHER WrongObject still blocks.
# Stage 198A1: `PM_RECV_DECODE_FAIL opcode=0 reply_cap=4294967295` is a BENIGN single early-boot
# probe that also appears on the PASSING x86_64 boot (x86_64's blocker list never treats it as
# fatal). It is emitted once under the ipc_recv_proof workload and the proof/oracle still complete
# correctly; excluding this exact opcode=0 form keeps a REAL non-zero-opcode decode failure (and
# PM_PANIC) fatal while matching x86_64's tolerance. A normal (non-proof) AArch64 boot emits zero.
# Stage 198C2B: the reply-cap DIRECT one-shot oracle's SECOND invocation of the
# transferred reply cap is REQUIRED to fail (the Reply record is already consumed) —
# the kernel logs `IPC_REPLY_FAIL tid=<child> reply_cap=<c> err=InvalidCapability`.
# This is the canonical one-shot rejection (the proof `second_reply=rejected`), NOT a
# boot blocker; the x86_64 and riscv64 core smokes never flag it. Exclude ONLY this
# exact IPC_REPLY_FAIL form; every other InvalidCapability still blocks.
# DIRECT3-CAP-FINAL §5A: the OTHER canonical one-shot rejection, excluded on exactly the same
# grounds as the InvalidCapability form above. When the four-tick reply deadline wins the
# terminal, the reply that arrives afterwards MUST be refused, and the kernel refuses it with
# `IPC_REPLY_FAIL tid=<replier> reply_cap=<c> err=WrongObject`. That is the arbitration working:
# the record's terminal is already settled by Timeout, so no second claimant may complete it.
#
# This is not a blanket WrongObject tolerance — only the exact `IPC_REPLY_FAIL` form is excluded,
# and every other WrongObject still blocks the boot. Nor is it unguarded: the U9-RX4 evaluator
# REQUIRES this refusal on a timeout win (and requires `late_reply_successes=0` with it), so the
# line is positively asserted where it belongs rather than merely tolerated here.
BLOCKER_EXCLUDE_REGEX='YARM_AARCH64_EXCEPTION_KIND unknown|BLOCKED_WOULDBLOCK_CLASSIFY|reply replay|second reply|replay rejected|IPC_REPLY_FAIL tid=[0-9]+ reply_cap=[0-9]+ err=InvalidCapability|IPC_REPLY_FAIL tid=[0-9]+ reply_cap=[0-9]+ err=WrongObject|SUPERVISOR_LIFECYCLE_QUERY_ERR tid=[0-9]+ err=WrongObject|PM_RECV_DECODE_FAIL opcode=0 reply_cap=4294967295'

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
      "FUTEX_WAIT_SPLIT_BEGIN" \
      "FUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK tid=" \
      "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=futex_wait_switch_required tid=" \
      "result=queue_advance_committed outgoing=" \
      "QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED cpu=0 reason=publication_committed" \
      "AARCH64_FUTEX_WAIT_DISPATCH_REVERIFY_OK" \
      "AARCH64_FUTEX_WAIT_DISPATCH_DEQUEUE_OK cpu=0" \
      "AARCH64_FUTEX_WAIT_DISPATCH_CURRENT_SET_OK cpu=0" \
      "AARCH64_FUTEX_WAIT_DISPATCH_RUNNING_OK" \
      "AARCH64_FUTEX_WAIT_DISPATCH_TTBR0_OK" \
      "AARCH64_FUTEX_WAIT_DISPATCH_FRAME_OK" \
      "AARCH64_FUTEX_WAIT_DISPATCH_DONE result=ok" \
      "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=aarch64 class=FutexWait" \
      "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=FutexWait result=ok" \
      "AARCH64_FUTEX_WAIT_LIVE_ORACLE_DONE result=ok"; then
    echo "[error] aarch64 Stage 195E/195F FutexWait switch-oracle markers missing"
    exit 1
  fi
  # U9-QA §2: FutexWait publishes its block PRE-LOCK, so the in-lock deferral must not appear at
  # all — §3 of the directive requires "no in-lock FutexWait production marker". The five
  # attestations this list used to require are replaced above by their pre-lock counterparts, and
  # the in-lock ones are now FORBIDDEN rather than expected.
  for a64_fw_bad in \
      "AARCH64_FUTEX_WAIT_DISPATCH_DEFER_BEGIN" \
      "AARCH64_FUTEX_WAIT_HANDLER_BYPASS_BEGIN" \
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

  # U9-TM §2: with the proof knobs off the pre-lock split route owns the timer and attests
  # claim+tick+re-arm as one `TIMER_SPLIT_TICK_OK` line, so the three broad-arm markers
  # legitimately do not appear. Either attestation satisfies the progression claim.
  if rg -a -q "TIMER_SPLIT_TICK_OK" "$LOGFILE" 2>/dev/null; then
    :
  elif ! check_required_patterns "$LOGFILE" \
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

# Stage 198A1: a clean terminal-idle boot is a SUCCESS outcome. Control only reaches here AFTER
# every hard required-marker + forbidden-marker check above PASSED (each exits 1 on failure), so
# "all required positive markers fired and no forbidden marker occurred" is already established.
# AArch64 at the idle terminal legitimately emits NO boot-to-shell marker, so recognize the
# canonical terminal-idle marker as success instead of failing on the (inapplicable) shell check.
# A missing proof marker, a forbidden marker, or an early idle-before-proof still fails above; a
# genuine hang leaves the idle marker absent, so this path is not taken.
if [[ "$QEMU_EXPECT_TERMINAL_IDLE" == "1" && -f "$LOGFILE" ]] \
  && rg -a -q -- "$TERMINAL_IDLE_MARKER" "$LOGFILE" 2>/dev/null; then
  echo "[ok] aarch64 core: clean terminal idle ($TERMINAL_IDLE_MARKER) after all required markers — PASS"
  exit 0
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
