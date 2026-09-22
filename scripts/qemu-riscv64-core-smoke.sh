#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail
source "$(dirname "$0")/qemu-smoke-common.sh"

# Official RISC-V64 core smoke. The kernel image and initramfs paths default to
# the artifacts emitted by scripts/build-qemu-riscv64-artifacts.sh.
KERNEL_IMAGE=${KERNEL_IMAGE:-build-riscv64/yarm-riscv64.bin}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-build-riscv64/initramfs-core.cpio}
TIMEOUT_SECS=${TIMEOUT_SECS:-30}
QEMU_SMOKE_STRICT=${QEMU_SMOKE_STRICT:-0}
QEMU_MACHINE=${QEMU_MACHINE:-virt}
QEMU_CPU=${QEMU_CPU:-rv64}
QEMU_MEMORY=${QEMU_MEMORY:-512M}
QEMU_SMP=${QEMU_SMP:-1}
QEMU_BIOS=${QEMU_BIOS:-default}
KERNEL_CMDLINE=${KERNEL_CMDLINE:-"console=ttyS0 rdinit=/init"}

# CLI: --smp N           → enable smp=N secondary-park assertion.
#      --timeout SECS    → override TIMEOUT_SECS for this run.
while (( $# > 0 )); do
  case "$1" in
    --smp)
      QEMU_SMP="$2"
      shift 2
      ;;
    --smp=*)
      QEMU_SMP="${1#--smp=}"
      shift
      ;;
    --timeout)
      TIMEOUT_SECS="$2"
      shift 2
      ;;
    --timeout=*)
      TIMEOUT_SECS="${1#--timeout=}"
      shift
      ;;
    *)
      echo "[warn] unknown arg: $1" >&2
      shift
      ;;
  esac
done

# Stage 178 (CROSS-ARCH-D6): CROSS_ARCH_D6=1 appends yarm.cross_arch_d6=1 to emit the
# RISC-V D6 restore-path audit markers (model=trapframe_sret; read-only observe of
# sepc/sstatus/sp + satp/ASID). Live lock-dropped restore is DEFERRED — the audit
# records the explicit deferral, not a fake live restore. No behavior change.
CROSS_ARCH_D6=${CROSS_ARCH_D6:-0}
if [[ "$CROSS_ARCH_D6" == "1" && "$KERNEL_CMDLINE" != *"yarm.cross_arch_d6="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.cross_arch_d6=1"
fi

# Stage 196A (POST-LOCK DRAIN FOUNDATION): POST_LOCK_FOUNDATION_ORACLE=1 appends
# yarm.riscv64_post_lock_foundation_oracle=1 to arm the default-off one-shot
# drain-ordering proof (publish token in the bounded broad-lock phase, consume
# it after the guard drops via a real with_cpu re-acquire, return to the same
# task through sret). It enables no retirement class and does not alter boot.
TERMINAL_FAULT_ORACLE=${TERMINAL_FAULT_ORACLE:-0}
TERMINAL_FAULT_FETCH_ORACLE=${TERMINAL_FAULT_FETCH_ORACLE:-0}
POST_LOCK_FOUNDATION_ORACLE=${POST_LOCK_FOUNDATION_ORACLE:-0}
# U9-PAGEFAULT1 §2: the RISC-V terminal-fault witness cell, default-off.
# U9-PAGEFAULT1 §2: the INSTRUCTION-FETCH variant of the same cell. It exercises the
# instruction-abort decode rather than the data-abort decode, and reaches the terminal class by
# construction (the demand screen refuses Execute outright) rather than by elimination.
if [[ "$TERMINAL_FAULT_FETCH_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.terminal_fault_fetch_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.terminal_fault_fetch_oracle=1"
  TERMINAL_FAULT_ORACLE=1
fi
# Slot 5 carries exactly ONE scenario and the kernel writes the read selector first, so the two
# knobs are mutually exclusive here too.
if [[ "$TERMINAL_FAULT_ORACLE" == "1" && "$TERMINAL_FAULT_FETCH_ORACLE" != "1" \
      && "$KERNEL_CMDLINE" != *"yarm.terminal_fault_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.terminal_fault_oracle=1"
fi
if [[ "$POST_LOCK_FOUNDATION_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.riscv64_post_lock_foundation_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.riscv64_post_lock_foundation_oracle=1"
fi

# Stage 196C (FUTEXWAKE LIVE ORACLE): FUTEX_WAKE_ORACLE=1 appends
# yarm.riscv64_futex_wake_oracle=1 to arm the default-off parent/child split-FutexWake proof
# (child blocks on legacy NR 9 FutexWait; parent wakes via split NR 10, counts 1 then 0).
FUTEX_WAKE_ORACLE=${FUTEX_WAKE_ORACLE:-0}
if [[ "$FUTEX_WAKE_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.riscv64_futex_wake_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.riscv64_futex_wake_oracle=1"
fi

# Stage 196D (QUEUE-SWITCH FOUNDATION ORACLE): QUEUE_SWITCH_ORACLE=1 appends
# yarm.riscv64_queue_switch_foundation_oracle=1 to arm the default-off two-task post-lock
# context-switch proof (task A yields → real SATP/sfence/frame switch to task B → B runs →
# A resumes). Enables NO syscall retirement class.
QUEUE_SWITCH_ORACLE=${QUEUE_SWITCH_ORACLE:-0}
if [[ "$QUEUE_SWITCH_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.riscv64_queue_switch_foundation_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.riscv64_queue_switch_foundation_oracle=1"
fi

# Stage 196E (FUTEXWAIT RETIREMENT ORACLE): FUTEX_WAIT_ORACLE=1 appends
# yarm.riscv64_futex_wait_oracle=1 to arm the default-off, one-shot two-task FutexWait (NR 9)
# queue-advancing RETIREMENT proof (task A blocks on FutexWait → real SATP/sfence/frame switch to
# task B → B wakes A via split FutexWake NR 10 → A resumes exactly once).
FUTEX_WAIT_ORACLE=${FUTEX_WAIT_ORACLE:-0}
if [[ "$FUTEX_WAIT_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.riscv64_futex_wait_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.riscv64_futex_wait_oracle=1"
fi

# Stage 196F (FUTEXWAIT NO-INCOMING IDLE ORACLE): FUTEX_WAIT_IDLE_ORACLE=1 appends
# yarm.riscv64_futex_wait_idle_oracle=1 to arm the default-off last-task idle workload (init blocks
# on a never-woken futex; the production default-on drain takes the post-lock IDLE outcome).
FUTEX_WAIT_IDLE_ORACLE=${FUTEX_WAIT_IDLE_ORACLE:-0}
if [[ "$FUTEX_WAIT_IDLE_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.riscv64_futex_wait_idle_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.riscv64_futex_wait_idle_oracle=1"
fi

# Stage 196G (YIELD TWO-TASK ORACLE): YIELD_TWO_TASK_ORACLE=1 appends
# yarm.riscv64_yield_two_task_oracle=1 (A yields → post-lock switch to B → B blocks → A resumes).
YIELD_TWO_TASK_ORACLE=${YIELD_TWO_TASK_ORACLE:-0}
if [[ "$YIELD_TWO_TASK_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.riscv64_yield_two_task_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.riscv64_yield_two_task_oracle=1"
fi

# Stage 196G (YIELD LONE-TASK ORACLE): YIELD_LONE_TASK_ORACLE=1 appends
# yarm.riscv64_yield_lone_task_oracle=1 (the only task yields and self-redispatches, never idles).
YIELD_LONE_TASK_ORACLE=${YIELD_LONE_TASK_ORACLE:-0}
if [[ "$YIELD_LONE_TASK_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.riscv64_yield_lone_task_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.riscv64_yield_lone_task_oracle=1"
fi

# Stage 197B (TYPED-OUTCOME NEGATIVE / genuine-Internal oracle): TYPED_INTERNAL_ERROR_ORACLE=1
# appends yarm.riscv_typed_outcome_internal_error_oracle=1 to force a GENUINE internal
# trap-handling error on the first syscall from a live task. The bridge must fatal
# (RISCV_TRAP_HANDLE_FAILED) and NEVER enter FutexWait typed idle.
TYPED_INTERNAL_ERROR_ORACLE=${TYPED_INTERNAL_ERROR_ORACLE:-0}
if [[ "$TYPED_INTERNAL_ERROR_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.riscv_typed_outcome_internal_error_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.riscv_typed_outcome_internal_error_oracle=1"
fi

# Stage 198A (SECOND-COHORT PLAIN PARITY): the plain-IpcSend live oracles are arch-neutral, so
# RISC-V honors the same env-var -> cmdline knob translations the x86_64 core smoke does. The
# oracle wrapper (qemu-ipc-recv-v2-oracle-smoke.sh) exports IPC_RECV_PROOF / IPC_SEND_PLAIN_ORACLE
# / IPC_SEND_ENQUEUE_ORACLE; without these translations the knobs never reach the RISC-V kernel
# cmdline and the oracle workload never runs.
IPC_RECV_PROOF=${IPC_RECV_PROOF:-0}
if [[ "$IPC_RECV_PROOF" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_recv_proof="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.ipc_recv_proof=1"
fi
# Stage 163: the sender-wake proof additionally needs the sub-knob
# yarm.ipc_recv_proof_sender_wake=1 (gates the coordination hook + workload). RISC-V forwards it
# exactly as x86_64 and AArch64 do; without this translation the sub-knob never reaches the RISC-V
# kernel cmdline and the sender-wake workload never runs.
IPC_RECV_PROOF_SENDER_WAKE=${IPC_RECV_PROOF_SENDER_WAKE:-0}
if [[ "$IPC_RECV_PROOF_SENDER_WAKE" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_recv_proof_sender_wake="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.ipc_recv_proof_sender_wake=1"
fi
IPC_SEND_PLAIN_ORACLE=${IPC_SEND_PLAIN_ORACLE:-0}
if [[ "$IPC_SEND_PLAIN_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_send_plain_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.ipc_send_plain_oracle=1"
fi
IPC_SEND_ENQUEUE_ORACLE=${IPC_SEND_ENQUEUE_ORACLE:-0}
if [[ "$IPC_SEND_ENQUEUE_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_send_enqueue_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.ipc_send_enqueue_oracle=1"
fi
# Stage 198B (ORDINARY-CAP PARITY): the ordinary-cap IpcSend live oracles are arch-neutral; RISC-V
# honors the same env-var -> cmdline knob translations the x86_64 core smoke does. The oracle
# wrapper exports IPC_SEND_CAP_ORACLE / IPC_SEND_CAP_ENQUEUE_ORACLE; without these the knobs never
# reach the RISC-V kernel cmdline and the ordinary-cap oracle workload never runs.
IPC_SEND_CAP_ORACLE=${IPC_SEND_CAP_ORACLE:-0}
if [[ "$IPC_SEND_CAP_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_send_cap_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.ipc_send_cap_oracle=1"
fi
IPC_SEND_CAP_ENQUEUE_ORACLE=${IPC_SEND_CAP_ENQUEUE_ORACLE:-0}
if [[ "$IPC_SEND_CAP_ENQUEUE_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_send_cap_enqueue_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.ipc_send_cap_enqueue_oracle=1"
fi
# Stage 198C2: propagate the reply-cap DIRECT oracle knob to the RISC-V cmdline (mirrors the
# ordinary-cap knob above); without it yarm.ipc_send_reply_cap_oracle=1 never reaches the
# kernel and the reply-cap direct oracle workload / boot provisioning never runs.
IPC_SEND_REPLY_CAP_ORACLE=${IPC_SEND_REPLY_CAP_ORACLE:-0}
if [[ "$IPC_SEND_REPLY_CAP_ORACLE" == "1" && "$KERNEL_CMDLINE" != *"yarm.ipc_send_reply_cap_oracle="* ]]; then
  KERNEL_CMDLINE="${KERNEL_CMDLINE:+$KERNEL_CMDLINE }yarm.ipc_send_reply_cap_oracle=1"
fi

require_file_or_warn "$KERNEL_IMAGE" "$QEMU_SMOKE_STRICT" "kernel image"
require_file_or_warn "$INITRAMFS_IMAGE" "$QEMU_SMOKE_STRICT" "initramfs image"

QEMU_BIN=${QEMU_BIN:-qemu-system-riscv64-hwe}
if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  QEMU_BIN=qemu-system-riscv64
fi
require_qemu_or_warn "$QEMU_BIN" "$QEMU_SMOKE_STRICT"

LOGFILE=${LOGFILE:-qemu-riscv64-core.log}
rm -f "$LOGFILE"

# Stage 200C2C2C-R2B: opt-in SINGLE-BOOT-INSTANCE enforcement. With `QEMU_SINGLE_BOOT=1`,
# QEMU is told never to restart the guest, so a reset can never silently produce a second
# boot inside one log. Default-off: every existing caller keeps its exact prior invocation.
QEMU_SINGLE_BOOT=${QEMU_SINGLE_BOOT:-0}
SINGLE_BOOT_ARGS=()
if [[ "$QEMU_SINGLE_BOOT" == "1" ]]; then
  SINGLE_BOOT_ARGS=(-no-reboot -no-shutdown)
fi

echo "[info] qemu command: $QEMU_BIN -machine $QEMU_MACHINE -cpu $QEMU_CPU -m $QEMU_MEMORY -smp $QEMU_SMP -nographic -monitor none -serial stdio -bios $QEMU_BIOS -kernel $KERNEL_IMAGE -initrd $INITRAMFS_IMAGE ${SINGLE_BOOT_ARGS[*]-} -append '$KERNEL_CMDLINE'"

if run_qemu_timeout_to_log "$TIMEOUT_SECS" "$LOGFILE" "$QEMU_BIN" \
  -machine "$QEMU_MACHINE" \
  -cpu "$QEMU_CPU" \
  -m "$QEMU_MEMORY" \
  -smp "$QEMU_SMP" \
  -nographic \
  -monitor none \
  -serial stdio \
  -bios "$QEMU_BIOS" \
  -kernel "$KERNEL_IMAGE" \
  -initrd "$INITRAMFS_IMAGE" \
  ${SINGLE_BOOT_ARGS[@]+"${SINGLE_BOOT_ARGS[@]}"} \
  -append "$KERNEL_CMDLINE" \
; then
  QEMU_STATUS=0
else
  QEMU_STATUS=$?
fi

# Stage 178 (CROSS-ARCH-D6): validate the RISC-V D6 restore-path audit when booted
# with yarm.cross_arch_d6=1. Honest acceptance: either a live RESTORE_DONE OR an
# explicit FALLBACK/DEFERRED reason, plus INVARIANT_OK + PROOF_DONE. RISC-V live
# restore is DEFERRED in Stage 178, so the DEFERRED branch is the expected path.
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
  if cad_has "CROSS_ARCH_D6_ARCH_MODEL arch=riscv64 model=trapframe_sret"; then
    echo "[ok] CROSS-ARCH-D6: RISC-V model=trapframe_sret (not switch_frames)"
  else
    echo "[warn] CROSS-ARCH-D6: riscv64 trapframe_sret model marker not observed"
  fi
  if cad_has "CROSS_ARCH_D6_RESTORE_DONE" || cad_has "CROSS_ARCH_D6_FALLBACK" || cad_has "CROSS_ARCH_D6_RISCV_DEFERRED"; then
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
  echo "[ok] CROSS-ARCH-D6: RISC-V D6 restore-path audit diagnostics clean (live restore DEFERRED)"
fi

# Stage 197B negative (genuine-Internal) oracle acceptance: a forced GENUINE internal
# trap-handling error must fatal via RISCV_TRAP_HANDLE_FAILED (distinct WFI reason
# handle_trap_entry_err) and NEVER enter FutexWait typed idle. This boot halts early (before
# services come up), so short-circuit the normal boot-ok acceptance below.
if [[ "$TYPED_INTERNAL_ERROR_ORACLE" == "1" ]]; then
  neg_fail=0
  for pat in \
    "RISCV_TYPED_OUTCOME_INTERNAL_ERROR_ORACLE_BEGIN" \
    "RISCV_TRAP_HANDLE_FAILED reason=handle_trap_entry_err" \
    "RISCV_TRAP_HALTED reason=handle_trap_entry_err"; do
    if ! tr '\r' '\n' <"$LOGFILE" | rg -a -F -q -- "$pat"; then
      echo "[fail] TYPED-INTERNAL-ERROR: required marker missing: $pat"
      neg_fail=1
    fi
  done
  # A genuine error must produce ZERO idle-success markers (no typed idle, no FutexWait idle
  # attestation, no idle terminal). The fatal WFI halt uses a DISTINCT reason (above).
  for forbidden in \
    "RISCV_TYPED_IDLE_OUTCOME" \
    "RISCV_FUTEX_WAIT_IDLE_ORACLE_DONE" \
    "RISCV_FUTEX_WAIT_POST_LOCK_IDLE_ENTERED" \
    "RISCV_KERNEL_IDLE_WAITING_FOR_IO" \
    "RISCV_TRAP_HALTED reason=kernel_idle_awaiting_io"; do
    if tr '\r' '\n' <"$LOGFILE" | rg -a -F -q -- "$forbidden"; then
      echo "[fail] TYPED-INTERNAL-ERROR: genuine error emitted idle marker: $forbidden"
      neg_fail=1
    fi
  done
  if [[ "$neg_fail" -eq 0 ]]; then
    echo "RISCV_TYPED_OUTCOME_INTERNAL_ERROR_ORACLE_DONE result=ok idle_entered=0"
    echo "[ok] RISC-V typed-outcome negative oracle: genuine Internal error -> fatal, zero idle"
    exit 0
  fi
  echo "RISCV_TYPED_OUTCOME_INTERNAL_ERROR_ORACLE_DONE result=fail"
  exit 1
fi

REQUIRED_PATTERNS=(
  "YARM_BOOT_OK"
  "RISCV_KERNEL_BOOT_OK"
  "RISCV_BOOT_ENTRY hart="
  "RISCV_BOOT_HART_SELECTED hart="
  "RISCV_BOOT_HART_ID_STORED hart="
  "RISCV_DTB_CPU_SCAN_DONE bitmap="
  "RISCV_HART_TOPOLOGY present_cpus="
  "RISCV_SCHEDULER_BSP_ONLY online_cpus=1 reason=riscv_smp_scheduler_not_enabled"
  "RISCV_LIVEEEEEEE"
  "RISCV_SYSCALL_ROUNDTRIP_OK"
  "RISCV_USER_RESUMED"
  # Stage 184 follow-up (RISC-V startup handoff): the RISC-V startup-cap
  # write-back must deliver the fresh task's ABI registers so process_manager
  # boots with real caps. RISCV_STARTUP_ARGS proves the per-task register
  # hand-off; the install-path OK marker proves userspace received them;
  # RISCV_PM_STARTUP_CAPS_OK + PM_BLOCKING_RECV_LOOP prove PM got a usable
  # request-recv cap and entered its blocking service loop (it must NOT fall
  # into the PM_NO_RECV_CAP dead-yield loop — see REJECT_PATTERNS).
  "RISCV_STARTUP_ARGS tid="
  "RISCV_STARTUP_CAPS_INSTALL_BEGIN"
  "RISCV_STARTUP_CAPS_INSTALL_OK"
  "RISCV_PM_STARTUP_CAPS_OK"
  "PM_BLOCKING_RECV_LOOP"
  "INITRAMFS_SRV_ENTRY"
  "DEVFS_SRV_ENTRY"
  "VFS_SRV_ENTRY"
  # Downstream servers spawned by the driver stack. Before the startup-handoff
  # fix these never spawned (PM stalled with zero caps), so require them to lock
  # in the full RISC-V userspace service chain.
  "DRIVER_MANAGER_ENTRY"
  "BLKCACHE_SRV_ENTRY"
  "VIRTIO_BLK_SRV_ENTRY"
  "VFS_MOUNT_TABLE_READY"
  "RAMFS_MOUNT_READY"
  "VFS_MOUNT_REGISTER_RAMFS_OK"
  "EXT4_SRV_READY"
  "VFS_MOUNT_REGISTER_EXT4_OK"
  "RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked"
  "RISCV_TIMER_AUDIT_BEGIN"
  "RISCV_TIMER_AUDIT_DONE sbi_time="
  "RISCV_TIMER_INIT_BEGIN"
  "RISCV_TIMER_MECHANISM value="
  "RISCV_PLIC_BASE value="
  "RISCV_PLIC_CONTEXT value="
  # Stage 184 (CROSS-ARCH-LIVE): default-on cross-arch live audit. RISC-V is
  # single-dispatcher (BSP-only scheduler), so mode=in_lock_single_dispatcher —
  # the graduated D2/D6/D3 correctness runs in-lock (NOT the removed global-lock
  # fallback); no x86-style AP/TLB-ACK is claimed.
  "CROSS_ARCH_TOPOLOGY_OK arch=riscv64 reason=single_dispatcher"
  "CROSS_ARCH_D2_RECV_OK arch=riscv64"
  "CROSS_ARCH_D2_SEND_OK arch=riscv64"
  "CROSS_ARCH_D6_OK arch=riscv64"
  "CROSS_ARCH_D3_OK arch=riscv64"
  "CROSS_ARCH_SYSCALL_PARITY_OK arch=riscv64"
  "CROSS_ARCH_LIVE_DONE arch=riscv64 result=ok"
  # Stage 196A (RISC-V SHARED TRAP-PATH + POST-LOCK DRAIN FOUNDATION): the trap
  # bridge routes through the shared wrapper, which owns the active-flag lifecycle
  # (set true before the bounded broad-lock phase, cleared after) and runs a
  # post-lock drain after the guard drops. These structural markers are one-shot
  # latched (first trap), so they appear exactly once per boot.
  "RISCV_SHARED_TRAP_ENTRY_BEGIN cpu="
  "RISCV_GLOBAL_LOCK_DROP_ACTIVE_SET cpu="
  "RISCV_GLOBAL_LOCK_PHASE_DONE cpu="
  "RISCV_GLOBAL_LOCK_DROP_ACTIVE_CLEAR cpu="
  "RISCV_POST_LOCK_DRAIN_BEGIN cpu="
  "RISCV_POST_LOCK_DRAIN_DONE cpu="
  "RISCV_SHARED_TRAP_ENTRY_DONE cpu="
  "YARM_LOCK_SPLIT_STAGE196A_INSTALLED arch=riscv64 shared=1 raw=0"
  # Stage 196B (RISC-V DEBUGLOG SPLIT-DISPATCH RETIREMENT): DebugLog (NR 15) is the
  # ONE live RISC-V split-dispatch class. The pre-lock split path services it off the
  # global lock and returns to the same task via `sret`; the userspace return marker
  # is emitted by init AFTER the split DebugLog returns (proving same-task resume).
  "RISCV_SPLIT_ABI_IMPORT_OK nr=15"
  "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=15 cpu=0 result=ok"
  "GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=riscv64 class=DebugLog"
  "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=DebugLog result=ok"
  "RISCV_SPLIT_FINALIZE_OK nr=15 result=ok"
  "RISCV_DEBUGLOG_SPLIT_USER_RETURN_OK"
)

# Nothing optional today: all required RAMFS/EXT4 markers above are
# emitted deterministically on QEMU virt. This array is kept for future
# additions that may need to land soft before being promoted to required.
OPTIONAL_FS_PATTERNS=()

# Timer / PLIC / external-IRQ acceptance: either the live marker OR the
# explicit deferral with reason. The kernel must emit exactly one of each
# pair so partial bring-up is detectable.
TIMER_ACCEPT_REGEX='RISCV_TIMER_SMOKE_OK ticks=|RISCV_TIMER_DEFERRED reason='
PLIC_ACCEPT_REGEX='RISCV_PLIC_INIT_DONE|RISCV_PLIC_DEFERRED reason='
EXTIRQ_ACCEPT_REGEX='RISCV_EXTIRQ_SMOKE_OK source=|RISCV_EXTIRQ_DEFERRED reason='

# Canonical timer-deferred reasons. The smoke gate accepts only these
# values when timer is on the deferred branch; an unknown reason means
# the kernel emitted a marker the gate doesn't yet understand and the
# operator must update both sides explicitly.
#
# Canonical 199E retired the timer ADMISSION gate, so the policy reasons are gone:
# "timer_irq_feature_disabled", "trap_bridge_reentrancy_not_ready" and "stie_audit_pending" named
# a build/audit decision, not a platform fact, and a default build can no longer take any of them.
# What remains are real facts about the machine or about ownership.
TIMER_DEFERRED_REASONS=(
  "sbi_time_ext_unavailable"
  "not_boot_hart"
  "already_armed"
  "unsafe_under_current_satp"
)

# Patterns that must NOT appear in a healthy boot.
REJECT_PATTERNS=(
  'RISCV_EARLY_TRAP'
  '\bPANIC\b'
  '\bFATAL\b'
  '\bASSERT\b'
  # U9-PAGEFAULT1 §2: removed from the reject list ONLY in the terminal-fault cell, below,
  # where exactly one is the point. Everywhere else it stays fatal.
  'PAGE_FAULT_UNHANDLED'
  'TRAP_HANDLE failed'
  'Vm\(Full\)'
  '\boom\b'
  # Stage 199D: the generic `\bcapacity\b` reject was RETIRED here. It was added at
  # Stage 181 (2a30515d) beside `Vm\(Full\)` and `\boom\b` as a proxy for resource
  # exhaustion, when no kernel marker printed the word benignly. Since Stage 199D
  # (fcfc55e3) the structural ack-store bound and the independent waiter census report
  # `capacity=256`, `ack_capacity=256` and `capacity_refused=0` on EVERY healthy boot,
  # so the bare word stopped discriminating and failed a healthy RISC-V core boot.
  #
  # Capacity checking is NOT removed — it is narrowed to the exact exhaustion forms the
  # current emitters produce:
  #   direct ack store    IPC_DIRECT_ACK_FUSES ... capacity_refused=N   (N > 0)
  #   shared region       SHARED_REGION_CANCEL_FUSE_SET reason=capacity_exhausted
  #   reply-link install  IPC_SERVER_REPLY_LINK_REGISTER_FAIL ... reason=capacity result=rolled_back
  #   fork COW            FORK_COW_FAIL reason=cow_capacity
  #   kernel stack map    D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_FAILED ... reason=page_table_capacity
  #   user VM grow        ... reason=user_vm_capacity
  #   exit deferral       EXIT_TASK_SYSCALL_DECLINED ... reason=deferred_capacity
  #   reply-cap mint      IPC_RECV_REPLY_CAP_MATERIALIZE_FAIL ... cnode_capacity=
  #
  # `reason=capacity\b` does NOT match `reason=capacity_exhausted` (`_` is a word
  # character, so there is no boundary), and `reason=deferred_capacity` deliberately does
  # NOT match the benign `deferred_capacity=ok` in EXIT_TASK_PREFLIGHT_OK.
  'capacity_refused=[1-9][0-9]*'
  'reason=capacity_exhausted'
  'reason=capacity\b'
  'reason=cow_capacity'
  'reason=page_table_capacity'
  'reason=user_vm_capacity'
  'reason=deferred_capacity'
  'IPC_RECV_REPLY_CAP_MATERIALIZE_FAIL'
  # These two exhaustion errors surface only as the Debug field `kernel_error={:?}` of
  # FORK_COW_FAIL, and were never covered by the retired word match (they do not contain
  # the substring "capacity"). Named explicitly so the narrowing does not leave them the
  # only unchecked `*Full` errors beside the already-rejected `Vm\(Full\)`.
  'kernel_error=CapabilityFull'
  'kernel_error=TaskTableFull'
  # Stage 184 (CROSS-ARCH-LIVE): the cross-arch live audit must not report a
  # blocked topology, an ungraduated seam, or any fallback/opt-out.
  'CROSS_ARCH_TOPOLOGY_BLOCKED arch=riscv64'
  'CROSS_ARCH_D2_RECV_FAIL'
  'CROSS_ARCH_D2_SEND_FAIL'
  'UNLOCK_GRADUATED_FALLBACK'
  'UNEXPECTED_INLOCK_DISPATCH'
  'emergency_optout'
  # A real QEMU virt DTB always has a well-formed /cpus node; a scan
  # failure here means the bitmap silently fell back to the single-hart
  # default instead of reflecting the real topology.
  'RISCV_DTB_CPU_SCAN_FAILED'
  # D2 endpoint-recv waiter-publish no-lost-wakeup unwind. Per
  # doc/AI_AGENT_RULES.md §14.3 / doc/KERNEL_UNLOCKING.md §3 this must be 0 —
  # any occurrence is a stop-ship bug.
  'D2_PUBLISH_RACE_UNWIND'
  # Stage 184 follow-up (RISC-V startup handoff): a task that reached userspace
  # with a zeroed startup register hand-off. PM_NO_RECV_CAP means PM never got a
  # request-recv cap (the pre-fix failure that stalled the whole service chain);
  # the *_BAD attestations mean the install/PM cap check saw task_id/caps == 0.
  'PM_NO_RECV_CAP'
  'RISCV_STARTUP_CAPS_INSTALL_BAD'
  'RISCV_PM_STARTUP_CAPS_BAD'
  # The S-mode illegal-instruction / page-fault trap the zeroed hand-off led to.
  # A healthy boot reaches RISCV_KERNEL_IDLE_WAITING_FOR_IO, never a kernel trap.
  'RISCV_TRAP_UNHANDLED'
  'reason=trap_from_s_mode'
  # Stage 196C/196E/196F/196G (DebugLog + FutexWake + FutexWait + Yield are the retired RISC-V
  # classes): every OTHER retirement class must stay global-lock-only. class=DebugLog,
  # class=FutexWake, class=FutexWait (196F), and class=Yield (196G) are DEFAULT-ON and allowed;
  # these class-specific rejects catch any accidental additional retirement. NR27/D2/IpcSend
  # queue-advancing markers must NEVER appear.
  'GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=InitramfsReadChunk'
  # The retired raw-pointer trap path must not resurface.
  'reason=no_trap_kernel_state'
)

failures=0

# U9-PAGEFAULT1 §2: REQUIRED_PATTERNS describes a boot in which init survives to drive the
# SpawnV5 service chain. The terminal-fault cell is a boot in which init DELIBERATELY does not:
# it takes one unhandled read at address 0 so the terminal PageFault route has a trigger of its
# own. Asserting the service chain there would be asserting that the deliberate fault did not
# happen. The cell replaces these with its own POSITIVE chain — every marker of the fault, the
# report, the transition, the drain and the replacement task, each exactly once — which is
# strictly stronger for what this cell exists to prove.
if [[ "$TERMINAL_FAULT_ORACLE" == "1" ]]; then
  echo "[info] U9-PF1-TERM: service-chain required markers are not asserted in the terminal-fault cell (init faults by design); the cell's own positive chain replaces them"
else
for pat in "${REQUIRED_PATTERNS[@]}"; do
  if ! rg -n -F "$pat" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] required marker missing: $pat"
    failures=$((failures + 1))
  fi
done
fi

for pat in "${OPTIONAL_FS_PATTERNS[@]}"; do
  if ! rg -n -F "$pat" "$LOGFILE" >/dev/null 2>&1; then
    echo "[warn] optional-FS marker missing: $pat"
  fi
done

if ! rg -n "$TIMER_ACCEPT_REGEX" "$LOGFILE" >/dev/null 2>&1; then
  echo "[fail] neither RISCV_TIMER_SMOKE_OK nor RISCV_TIMER_DEFERRED present"
  failures=$((failures + 1))
fi

# If timer is on the deferred branch, the reason must be one the gate
# recognizes; an unknown deferred reason means kernel + gate are
# out-of-sync. (Live ticks don't go through this branch.)
if rg -n "RISCV_TIMER_DEFERRED reason=" "$LOGFILE" >/dev/null 2>&1; then
  timer_reason=$(rg -aN "RISCV_TIMER_DEFERRED reason=[A-Za-z0-9_]+" "$LOGFILE" 2>/dev/null \
    | head -n1 | sed -E 's/.*reason=([A-Za-z0-9_]+).*/\1/')
  if [[ -n "$timer_reason" ]]; then
    canonical=0
    for reason in "${TIMER_DEFERRED_REASONS[@]}"; do
      [[ "$timer_reason" == "$reason" ]] && canonical=1 && break
    done
    if (( canonical == 0 )); then
      echo "[fail] RISCV_TIMER_DEFERRED reason=${timer_reason} is not canonical"
      failures=$((failures + 1))
    fi
  fi
fi

if ! rg -n "$PLIC_ACCEPT_REGEX" "$LOGFILE" >/dev/null 2>&1; then
  echo "[fail] neither RISCV_PLIC_INIT_DONE nor RISCV_PLIC_DEFERRED present"
  failures=$((failures + 1))
fi

if ! rg -n "$EXTIRQ_ACCEPT_REGEX" "$LOGFILE" >/dev/null 2>&1; then
  echo "[fail] neither RISCV_EXTIRQ_SMOKE_OK nor RISCV_EXTIRQ_DEFERRED present"
  failures=$((failures + 1))
fi

# Repeated missing-DTB loop: more than one occurrence of source=missing_dtb in
# YARM_BOOT_CMDLINE_CAPTURE is the failure mode the kernel guards against.
missing_dtb_count=$(rg -cF "source=missing_dtb" "$LOGFILE" 2>/dev/null || true)
missing_dtb_count=${missing_dtb_count:-0}
if (( missing_dtb_count > 1 )); then
  echo "[fail] repeated source=missing_dtb loop (count=$missing_dtb_count)"
  failures=$((failures + 1))
fi

for pat in "${REJECT_PATTERNS[@]}"; do
  # U9-PAGEFAULT1 §2: in the terminal-fault cell the unhandled fault is the SUBJECT, not a
  # defect. The cell asserts it appears EXACTLY once, with the exact tid, address and access —
  # which a blanket reject cannot express and a relaxed reject would not check at all.
  if [[ "$TERMINAL_FAULT_ORACLE" == "1" && "$pat" == 'PAGE_FAULT_UNHANDLED' ]]; then
    continue
  fi
  if rg -n "$pat" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] rejected pattern present: $pat"
    failures=$((failures + 1))
  fi
done

# SpawnV5 wrong-sender reply guard: the kernel marks any SpawnV5 reply that
# arrives on the wrong endpoint.
if rg -n "SPAWN_V5_WRONG_SENDER|YARM_SPAWN_V5_REPLY_WRONG_SENDER" "$LOGFILE" >/dev/null 2>&1; then
  echo "[fail] SpawnV5 wrong-sender reply observed"
  failures=$((failures + 1))
fi

# Unexpected early halt: any RISCV_TRAP_HALTED reason that is not the
# expected idle terminal state is an unexpected halt.
if rg -n "RISCV_TRAP_HALTED reason=" "$LOGFILE" >/dev/null 2>&1; then
  if rg -nv "RISCV_TRAP_HALTED reason=kernel_idle_awaiting_io" "$LOGFILE" 2>/dev/null \
      | rg -n "RISCV_TRAP_HALTED reason=" >/dev/null 2>&1; then
    echo "[fail] unexpected RISCV_TRAP_HALTED reason"
    failures=$((failures + 1))
  fi
fi

if (( QEMU_SMP >= 2 )); then
  if ! rg -n "RISCV_SECONDARY_HART_PARK hart=" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] --smp ${QEMU_SMP} requires RISCV_SECONDARY_HART_PARK hart=N"
    failures=$((failures + 1))
  fi
fi

# Multi-hart topology must come from a completed binary-FDT /cpus scan, not
# a silent single-hart fallback. RISCV_DTB_CPU_SCAN_FAILED is rejected
# above; this requires the positive completion marker as well so a scan
# that neither completes nor fails (e.g. an early return bypassing both)
# cannot pass unnoticed.
if (( QEMU_SMP >= 2 )); then
  if ! rg -n "RISCV_DTB_CPU_SCAN_DONE bitmap=0x[0-9a-f]+ count=${QEMU_SMP}\b" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] --smp ${QEMU_SMP} requires RISCV_DTB_CPU_SCAN_DONE bitmap=... count=${QEMU_SMP}"
    failures=$((failures + 1))
  fi
fi

# Topology assertions: YARM must report present_cpus matching --smp N, and the
# present_bitmap must be the contiguous 0..N-1 mask for QEMU virt.
#
# Stage 199D link 2: `online_cpus` is no longer pinned to 1. Every TRAP-READY-acknowledged
# secondary is registered scheduler-online in the WAKE-ONLY (explicitly non-dispatchable) state,
# so online_cpus == present_cpus. Wake-only CPUs accept no task placement
# (`least_loaded_online_cpu` skips them) and never dispatch (`dispatching = online & !wake_only`),
# so this is NOT a claim of RISC-V SMP user dispatch — the per-CPU assertions below pin that.
expected_bitmap_hex=""
case "$QEMU_SMP" in
  1) expected_bitmap_hex="0x1" ;;
  2) expected_bitmap_hex="0x3" ;;
  3) expected_bitmap_hex="0x7" ;;
  4) expected_bitmap_hex="0xf" ;;
esac
if [[ -n "$expected_bitmap_hex" ]]; then
  if ! rg -an "YARM_BOOT_OK present_cpus=${QEMU_SMP} present_bitmap=${expected_bitmap_hex} online_cpus=${QEMU_SMP}" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] YARM_BOOT_OK must report present_cpus=${QEMU_SMP} present_bitmap=${expected_bitmap_hex} online_cpus=${QEMU_SMP}"
    failures=$((failures + 1))
  fi
fi

# Stage 199D link 2: every secondary that came online must be WAKE-ONLY, and must have done so
# only after publishing its link-7 trap-ready state. At -smp 1 there are no secondaries, so none
# of these markers may appear at all.
if [[ "$QEMU_SMP" -gt 1 ]]; then
  expected_secondaries=$(( QEMU_SMP - 1 ))
  for marker in \
    "RISCV_SECONDARY_CPU_ID_BOUND" \
    "RISCV_SECONDARY_KERNEL_SATP_ACTIVE" \
    "RISCV_SECONDARY_SSCRATCH_READY" \
    "RISCV_SECONDARY_TRAP_VECTOR_INSTALLED" \
    "RISCV_SECONDARY_INTERRUPTS_DISABLED" \
    "RISCV_SECONDARY_TRAP_READY_PARKED" \
    "RISCV_SCHEDULER_SMP_ONLINE cpu="; do
    seen=$(rg -ac -- "$marker" "$LOGFILE" 2>/dev/null || echo 0)
    if [[ "$seen" != "$expected_secondaries" ]]; then
      echo "[fail] expected ${expected_secondaries}x '${marker}', saw ${seen}"
      failures=$((failures + 1))
    fi
  done
  # The online secondary must be non-dispatchable in every dimension it reports.
  if ! rg -an "RISCV_SCHEDULER_SMP_ONLINE cpu=[0-9]+ present=1 online=1 wake_only=1 dispatchable=0 user_dispatch=0 timer=0 queue=0 irq=0" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] RISCV_SCHEDULER_SMP_ONLINE must report wake_only=1 and every dispatch dimension 0"
    failures=$((failures + 1))
  fi
  # Trap-ready state must precede the scheduler registration.
  ready_line=$(rg -an "RISCV_SECONDARY_TRAP_READY_PARKED" "$LOGFILE" 2>/dev/null | head -n1 | cut -d: -f1)
  online_line=$(rg -an "RISCV_SCHEDULER_SMP_ONLINE cpu=" "$LOGFILE" 2>/dev/null | head -n1 | cut -d: -f1)
  if [[ "$ready_line" =~ ^[0-9]+$ && "$online_line" =~ ^[0-9]+$ ]] \
     && (( ready_line >= online_line )); then
    echo "[fail] scheduler registration must follow the trap-ready park (ready=${ready_line} online=${online_line})"
    failures=$((failures + 1))
  fi
else
  for marker in \
    "RISCV_SECONDARY_CPU_ID_BOUND" \
    "RISCV_SECONDARY_TRAP_READY_PARKED" \
    "RISCV_SCHEDULER_SMP_ONLINE"; do
    if rg -an -- "$marker" "$LOGFILE" >/dev/null 2>&1; then
      echo "[fail] -smp 1 must emit no secondary marker, but '${marker}' is present"
      failures=$((failures + 1))
    fi
  done
fi

# Boot hart must not be parked: the RISCV_BOOT_HART_SELECTED hart=N and the
# RISCV_SECONDARY_HART_PARK hart=N lines must NOT share the same hart-id.
boot_hart=$(rg -n "RISCV_BOOT_HART_SELECTED hart=" "$LOGFILE" 2>/dev/null \
  | head -n1 | sed -E 's/.*hart=([0-9]+).*/\1/')
if [[ -n "$boot_hart" ]]; then
  if rg -n "RISCV_SECONDARY_HART_PARK hart=${boot_hart}\b" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] boot hart ${boot_hart} appears in RISCV_SECONDARY_HART_PARK list"
    failures=$((failures + 1))
  fi
fi

# Scheduler breadcrumb emitted during the DTB scan, BEFORE kernel bootstrap. It records the
# scheduler state at that moment; Stage 199D link 2's RISCV_SCHEDULER_SMP_ONLINE supersedes it
# later in the boot for any trap-ready secondary. Still always required.
if ! rg -n "RISCV_SCHEDULER_BSP_ONLY online_cpus=1 reason=riscv_smp_scheduler_not_enabled" "$LOGFILE" >/dev/null 2>&1; then
  echo "[fail] RISCV_SCHEDULER_BSP_ONLY breadcrumb missing"
  failures=$((failures + 1))
fi

# Multi-hart topology must record the live-IRQ deferral while the
# multi-hart timer/PLIC path is not yet validated end-to-end. Single-hart
# boots must NOT emit this marker so the marker can be a positive signal
# that the gating is correctly engaged.
if (( QEMU_SMP >= 2 )); then
  if ! rg -n "RISCV_IRQ_SMP_TOPOLOGY_DEFERRED reason=present_topology_not_live_validated" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] multi-hart boot must emit RISCV_IRQ_SMP_TOPOLOGY_DEFERRED"
    failures=$((failures + 1))
  fi
fi

# Stage 196C + U9-QA §2 + 199G-B §2 + U9-SPAWN-IC1 §5: the RISC-V split dispatcher may service
# DebugLog (NR 15), FutexWake (NR 10), FutexWait (NR 9), IpcRecvTimeout (NR 5) and IpcSend (NR 1)
# ONLY. NR 15 and NR 10 are NON-SWITCHING and return early; NR 9 and NR 5 are the SWITCHING
# classes — each publishes its block pre-lock and settles through an existing drain (Stage 196E
# for NR 9, the homologous D2-recv drain for NR 5), which is why their lines read
# `result=queue_advance_committed` rather than `result=ok`.
#
# U9-SPAWN-IC1 §5 — NR 1 is the ORACLE CORRECTION, not a kernel change. Stage 199G-C4 §2 gave
# RISC-V the POST-WORK committed disposition for the U6 blocking-send lifecycle, and one
# `IpcSend` per boot legitimately settles through it:
#
#   YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=1 cpu=0 result=post_work_committed finalized=1
#
# This allow-list was never widened when that class landed, so the total has been exactly one over
# the sum of the other four ever since — a stale expectation, not a leaked class. Only the
# expectation changes here; no kernel behavior is touched. NR 1 is pinned to its exact disposition
# below, so it can only ever be the post-work class and never an ordinary early return.
#
# Any `YARM_LOCK_SPLIT_DISPATCH arch=riscv64` line whose nr is none of the five means another
# class was wrongly retired off the global lock.
riscv_split_total=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=" "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_nr15=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=15 " "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_nr10=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=10 " "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_nr9=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=9 " "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_nr5=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=5 " "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_nr1=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=1 " "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_total=${riscv_split_total:-0}
riscv_split_nr15=${riscv_split_nr15:-0}
riscv_split_nr10=${riscv_split_nr10:-0}
riscv_split_nr9=${riscv_split_nr9:-0}
riscv_split_nr5=${riscv_split_nr5:-0}
riscv_split_nr1=${riscv_split_nr1:-0}
# U9-SPAWN-IC1 §5: every NR 1 line must be the POST-WORK committed disposition. A NR 1 that
# early-returned would be a genuinely new class off the global lock, which is what the totals
# check below is for — this pins WHICH NR 1 is admissible, so widening the list did not widen the
# contract.
riscv_split_nr1_post_work=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=1 .*result=post_work_committed" "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_nr1_post_work=${riscv_split_nr1_post_work:-0}
if (( riscv_split_nr1 != riscv_split_nr1_post_work )); then
  echo "[fail] RISC-V IpcSend split serviced a non-post-work disposition (nr1=${riscv_split_nr1} post_work=${riscv_split_nr1_post_work})"
  failures=$((failures + 1))
fi
# Every NR 9 line must be a committed queue advance — never an early return, which for a
# switching class would mean returning through the parked caller's own frame.
riscv_split_nr9_committed=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=9 .*result=queue_advance_committed" "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_nr9_committed=${riscv_split_nr9_committed:-0}
if (( riscv_split_nr9 != riscv_split_nr9_committed )); then
  echo "[fail] RISC-V FutexWait split did not commit a queue advance (nr9=${riscv_split_nr9} committed=${riscv_split_nr9_committed})"
  failures=$((failures + 1))
fi
# 199G-B §2: the same contract for NR 5, and for the same reason — a blocking receive that parked
# pre-lock may never return through its own frame.
riscv_split_nr5_committed=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=5 .*result=queue_advance_committed" "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_nr5_committed=${riscv_split_nr5_committed:-0}
if (( riscv_split_nr5 != riscv_split_nr5_committed )); then
  echo "[fail] RISC-V IpcRecvTimeout split did not commit a queue advance (nr5=${riscv_split_nr5} committed=${riscv_split_nr5_committed})"
  failures=$((failures + 1))
fi
# 199G-B §2: a committed NR 5 must also have captured its outgoing user context. `captured=0`
# with a real outgoing tid is the exact defect this stage fixed: the parked receiver kept a stale
# saved pc and faulted on resume.
riscv_split_nr5_uncaptured=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=5 .*result=queue_advance_committed.*captured=0" "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_nr5_uncaptured=${riscv_split_nr5_uncaptured:-0}
if (( riscv_split_nr5_uncaptured != 0 )); then
  echo "[fail] RISC-V IpcRecvTimeout committed without capturing the outgoing context (n=${riscv_split_nr5_uncaptured})"
  failures=$((failures + 1))
fi
# U9-SPAWN-TXN3 §4/§6 — SpawnProcess (NR 23) and SpawnFromMemoryObject (NR 29) are retired off
# the terminal broad dispatcher by this pass, so they join the allow-list. Both are pinned to the
# NON-SWITCHING `result=ok` disposition below: each runs the one generic spawn transaction and
# returns the child TID in the caller's own frame, so a committed queue advance or a post-work
# deferral from either would mean the class had silently become something else.
riscv_split_nr23=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=23 " "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_nr29=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=29 " "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_nr23=${riscv_split_nr23:-0}
riscv_split_nr29=${riscv_split_nr29:-0}
riscv_split_nr23_ok=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=23 .*result=ok" "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_nr29_ok=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=29 .*result=ok" "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_nr23_ok=${riscv_split_nr23_ok:-0}
riscv_split_nr29_ok=${riscv_split_nr29_ok:-0}
if (( riscv_split_nr23 != riscv_split_nr23_ok )); then
  echo "[fail] RISC-V SpawnProcess split serviced a switching disposition (nr23=${riscv_split_nr23} ok=${riscv_split_nr23_ok})"
  failures=$((failures + 1))
fi
if (( riscv_split_nr29 != riscv_split_nr29_ok )); then
  echo "[fail] RISC-V SpawnFromMemoryObject split serviced a switching disposition (nr29=${riscv_split_nr29} ok=${riscv_split_nr29_ok})"
  failures=$((failures + 1))
fi
# And the counts are the boot's own spawn census: three NR 23 and five NR 29, matching
# KSPAWN_ENTER and SPAWN_FROM_MO_OK. A route that silently stopped firing would show here.
#
# U9-PAGEFAULT1 §2: this census counts spawns INIT performs. In the terminal-fault cell init
# faults before the SpawnV5 chain by design, so the census is zero and asserting three and five
# would be asserting that the deliberate fault did not happen. The equality checks above still
# run — a switching disposition would still be caught — and the cell's own chain covers what
# this boot is for.
if [[ "$TERMINAL_FAULT_ORACLE" == "1" ]]; then
  echo "[info] U9-PF1-TERM: the spawn census is not asserted in the terminal-fault cell (init faults before the SpawnV5 chain by design)"
elif (( riscv_split_nr23 != 3 )); then
  echo "[fail] RISC-V SpawnProcess split count is ${riscv_split_nr23}, expected 3"
  failures=$((failures + 1))
fi
if [[ "$TERMINAL_FAULT_ORACLE" != "1" ]] && (( riscv_split_nr29 != 5 )); then
  echo "[fail] RISC-V SpawnFromMemoryObject split count is ${riscv_split_nr29}, expected 5"
  failures=$((failures + 1))
fi
# U9-RECV-FINAL §1: NR 2 (`IpcRecv`) joins the retired set. It was the last recognized IPC
# syscall this port excluded, so until now every `IpcRecv` on RISC-V — queued deliveries as much
# as blocking ones — reached the terminal broad dispatcher while the same traps were served
# pre-lock on the other two ports. The accounting is WIDENED by exactly one term, never relaxed:
# the sum below is still what catches a syscall being serviced that nobody authorized.
riscv_split_nr2=$(rg -c "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=2 " "$LOGFILE" 2>/dev/null || echo 0)
riscv_split_nr2=${riscv_split_nr2:-0}
if (( riscv_split_nr2 == 0 )); then
  echo "[fail] RISC-V serviced no IpcRecv through the split dispatcher (nr2=0)"
  failures=$((failures + 1))
fi
if (( riscv_split_total != riscv_split_nr15 + riscv_split_nr10 + riscv_split_nr9 + riscv_split_nr5 + riscv_split_nr2 + riscv_split_nr1 + riscv_split_nr23 + riscv_split_nr29 )); then
  echo "[fail] RISC-V split-dispatch serviced a syscall outside the retired set (total=${riscv_split_total} nr15=${riscv_split_nr15} nr10=${riscv_split_nr10} nr9=${riscv_split_nr9} nr5=${riscv_split_nr5} nr2=${riscv_split_nr2} nr1=${riscv_split_nr1} nr23=${riscv_split_nr23} nr29=${riscv_split_nr29})"
  failures=$((failures + 1))
fi

# Stage 196A (POST-LOCK DRAIN FOUNDATION): when armed, the one-shot oracle must
# prove genuine drain ordering end-to-end (publish in-lock → lock dropped →
# consumed post-lock → sret return to the same task). All five markers required.
if [[ "$POST_LOCK_FOUNDATION_ORACLE" == "1" ]]; then
  POST_LOCK_ORACLE_PATTERNS=(
    "RISCV_POST_LOCK_FOUNDATION_ORACLE_PUBLISH_OK cpu="
    "RISCV_POST_LOCK_FOUNDATION_ORACLE_LOCK_DROPPED_OK cpu="
    "RISCV_POST_LOCK_FOUNDATION_ORACLE_DRAIN_OK cpu="
    "RISCV_POST_LOCK_FOUNDATION_ORACLE_USER_RETURN_OK tid="
    "RISCV_POST_LOCK_FOUNDATION_ORACLE_DONE result=ok"
  )
  for pat in "${POST_LOCK_ORACLE_PATTERNS[@]}"; do
    if ! rg -n -F "$pat" "$LOGFILE" >/dev/null 2>&1; then
      echo "[fail] post-lock foundation oracle marker missing: $pat"
      failures=$((failures + 1))
    fi
  done
  # A task-switched DONE is a proof failure: the oracle syscall must return to the
  # same task (no scheduler mutation).
  if rg -n "RISCV_POST_LOCK_FOUNDATION_ORACLE_DONE result=task_switched" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] post-lock foundation oracle returned to a different task"
    failures=$((failures + 1))
  fi
fi

# Stage 196C (FUTEXWAKE LIVE ORACLE): when armed, the parent/child split-FutexWake proof
# must complete — child blocked, first wake=1, second wake=0, userspace return, live DONE.
if [[ "$FUTEX_WAKE_ORACLE" == "1" ]]; then
  FUTEX_WAKE_ORACLE_PATTERNS=(
    "RISCV_FUTEX_ORACLE_CHILD_SPAWNED child_tid="
    "RISCV_FUTEX_ORACLE_CHILD_WAIT_BEGIN observed="
    "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=10 cpu=0 result=ok"
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=FutexWake result=ok"
    "FUTEX_WAKE_SPLIT_DONE arch=riscv64 result=ok woke=1"
    "RISCV_SPLIT_FINALIZE_OK nr=10 result=ok"
    "RISCV_FUTEX_WAKE_USER_RETURN_OK first_wake=1 second_wake=0"
    "RISCV_FUTEX_WAKE_LIVE_ORACLE_DONE result=ok first_wake=1 second_wake=0 waiter_tid="
    "RISCV_FUTEX_ORACLE_CHILD_WOKE"
  )
  for pat in "${FUTEX_WAKE_ORACLE_PATTERNS[@]}"; do
    if ! rg -n -F "$pat" "$LOGFILE" >/dev/null 2>&1; then
      echo "[fail] FutexWake live oracle marker missing: $pat"
      failures=$((failures + 1))
    fi
  done
  # A failed oracle DONE (wrong counts) is a proof failure.
  if rg -n "RISCV_FUTEX_WAKE_LIVE_ORACLE_DONE result=fail" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] FutexWake live oracle reported wrong wake counts"
    failures=$((failures + 1))
  fi
fi

# Stage 196D (QUEUE-SWITCH FOUNDATION ORACLE): when armed, the two-task post-lock context
# switch must complete end-to-end — publish/re-enqueue outgoing, handler bypass, lock dropped,
# dequeue incoming, current set, running, real SATP + sfence.vma, frame restore, sret into B,
# B runs in userspace, A resumes. NO Yield/FutexWait retirement marker may appear.
if [[ "$QUEUE_SWITCH_ORACLE" == "1" ]]; then
  QUEUE_SWITCH_PATTERNS=(
    "RISCV_QUEUE_SWITCH_FOUNDATION_PUBLISH_BEGIN cpu=0 outgoing="
    "RISCV_QUEUE_SWITCH_FOUNDATION_REENQUEUE_OK tid="
    "RISCV_QUEUE_SWITCH_FOUNDATION_HANDLER_RETURN_OK cpu=0"
    "RISCV_QUEUE_SWITCH_FOUNDATION_DRAIN_BEGIN cpu=0"
    "RISCV_QUEUE_SWITCH_FOUNDATION_LOCK_DROPPED_OK cpu=0"
    "RISCV_QUEUE_SWITCH_FOUNDATION_DEQUEUE_OK cpu=0 incoming="
    "RISCV_QUEUE_SWITCH_FOUNDATION_CURRENT_SET_OK cpu=0 incoming="
    "RISCV_QUEUE_SWITCH_FOUNDATION_RUNNING_OK incoming="
    "RISCV_QUEUE_SWITCH_FOUNDATION_SATP_OK incoming="
    "RISCV_QUEUE_SWITCH_FOUNDATION_SFENCE_OK incoming="
    "RISCV_QUEUE_SWITCH_FOUNDATION_FRAME_OK incoming="
    "RISCV_QUEUE_SWITCH_FOUNDATION_SRET_ARMED incoming="
    "RISCV_QUEUE_SWITCH_FOUNDATION_DRAIN_DONE result=ok"
    "RISCV_QUEUE_SWITCH_FOUNDATION_INCOMING_USER_OK tid="
    "RISCV_QUEUE_SWITCH_FOUNDATION_ORACLE_DONE result=ok outgoing="
  )
  for pat in "${QUEUE_SWITCH_PATTERNS[@]}"; do
    if ! rg -n -F "$pat" "$LOGFILE" >/dev/null 2>&1; then
      echo "[fail] queue-switch foundation oracle marker missing: $pat"
      failures=$((failures + 1))
    fi
  done
  # outgoing_resumed=1 (round trip) is mandatory in the DONE marker. Use -a (force text): the
  # boot log contains NUL bytes, so ripgrep would otherwise treat it as binary and skip it.
  if ! rg -a "RISCV_QUEUE_SWITCH_FOUNDATION_ORACLE_DONE result=ok .*outgoing_resumed=1" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] queue-switch foundation oracle did not prove the outgoing round-trip"
    failures=$((failures + 1))
  fi
  # Any FAIL / result=fail is a proof failure.
  if rg -n "RISCV_QUEUE_SWITCH_FOUNDATION_FAIL|RISCV_QUEUE_SWITCH_FOUNDATION_ORACLE_DONE result=fail" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] queue-switch foundation oracle reported a failure"
    failures=$((failures + 1))
  fi
fi

# Stage 196E/196F (FUTEXWAIT SWITCH ORACLE, run with the DEFAULT-ON mechanism): the off-global-lock
# RISC-V retirement that context-switches the blocking caller must complete end-to-end — production
# default-on marker, in-lock block publish, handler bypass, lock dropped, blocked-state reverify,
# dequeue incoming, current set, running, real SATP + sfence.vma, frame restore, sret into B, B runs
# in userspace + wakes A via split FutexWake (count 1), A resumes once. NO Yield/NR27 marker appears.
if [[ "$FUTEX_WAIT_ORACLE" == "1" ]]; then
  FUTEX_WAIT_PATTERNS=(
    # U9-QA §2: FutexWait now publishes its block PRE-LOCK, so the five in-lock attestations this
    # list used to require are deliberately absent — §3 of the directive requires exactly that
    # ("no in-lock FutexWait production marker"). Each is replaced by its pre-lock counterpart,
    # and the set is strictly stronger: the broad dispatcher is SKIPPED entirely rather than
    # bypassed from inside the handler it already entered.
    #   RISCV_FUTEX_WAIT_RETIRE_DEFAULT_ON        -> FUTEX_WAIT_SPLIT_BEGIN
    #   RISCV_FUTEX_WAIT_DISPATCH_DEFER_BEGIN     -> QUEUE_ADVANCING_DISPATCH_DEFERRED
    #   RISCV_FUTEX_WAIT_DISPATCH_BLOCK_PUBLISH_OK-> FUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK
    #   RISCV_FUTEX_WAIT_HANDLER_BYPASS_{BEGIN,DONE} -> result=queue_advance_committed
    #                                                 + QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED
    "FUTEX_WAIT_SPLIT_BEGIN"
    "FUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK tid="
    "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=futex_wait_switch_required tid="
    "result=queue_advance_committed outgoing="
    "QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED cpu=0 reason=publication_committed"
    "RISCV_FUTEX_WAIT_DISPATCH_DRAIN_BEGIN cpu=0"
    "RISCV_FUTEX_WAIT_DISPATCH_LOCK_DROPPED_OK cpu=0"
    "RISCV_FUTEX_WAIT_DISPATCH_REVERIFY_OK tid="
    "RISCV_FUTEX_WAIT_DISPATCH_DEQUEUE_OK cpu=0 incoming="
    "RISCV_FUTEX_WAIT_DISPATCH_CURRENT_SET_OK cpu=0 incoming="
    "RISCV_FUTEX_WAIT_DISPATCH_RUNNING_OK incoming="
    "RISCV_FUTEX_WAIT_DISPATCH_SATP_OK incoming="
    "RISCV_FUTEX_WAIT_DISPATCH_SFENCE_OK incoming="
    "RISCV_FUTEX_WAIT_DISPATCH_FRAME_OK incoming="
    "RISCV_FUTEX_WAIT_DISPATCH_SRET_ARMED incoming="
    "RISCV_FUTEX_WAIT_DISPATCH_DONE result=ok"
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=FutexWait result=ok"
    "RISCV_FUTEX_WAIT_INCOMING_USER_OK tid="
    "RISCV_FUTEX_WAIT_USER_RETURN_OK tid="
  )
  for pat in "${FUTEX_WAIT_PATTERNS[@]}"; do
    if ! rg -n -F "$pat" "$LOGFILE" >/dev/null 2>&1; then
      echo "[fail] futex-wait retirement oracle marker missing: $pat"
      failures=$((failures + 1))
    fi
  done
  # The round-trip DONE with wake_count=1 is mandatory. Use -a (force text): NUL bytes in the log.
  if ! rg -a "RISCV_FUTEX_WAIT_LIVE_ORACLE_DONE result=ok .*wake_count=1" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] futex-wait retirement oracle did not prove the round-trip (wake_count=1)"
    failures=$((failures + 1))
  fi
  # Any FAIL / state_changed decline / result=fail is a proof failure.
  if rg -n "RISCV_FUTEX_WAIT_DISPATCH_FAIL|RISCV_FUTEX_WAIT_DISPATCH_DEFERRED reason=state_changed|RISCV_FUTEX_WAIT_LIVE_ORACLE_DONE result=fail" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] futex-wait retirement oracle reported a failure"
    failures=$((failures + 1))
  fi
  # Zero Yield / NR27 retirement markers may appear (this stage retires ONLY FutexWait).
  if rg -n "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=InitramfsReadChunk" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] futex-wait oracle boot leaked an NR27 retirement marker"
    failures=$((failures + 1))
  fi
fi

# Stage 196F (FUTEXWAIT NO-INCOMING IDLE ORACLE): the last runnable user task blocks on a
# never-woken futex; the production default-on drain must take the post-lock IDLE outcome — no
# incoming, real lock-dropped proof, deferral cleared, NO frame restored, NO sret, and the real
# RISC-V idle loop entered. The blocked caller stays Blocked and current stays None.
if [[ "$FUTEX_WAIT_IDLE_ORACLE" == "1" ]]; then
  FUTEX_WAIT_IDLE_PATTERNS=(
    # U9-QA §2: FutexWait now publishes its block PRE-LOCK, so the five in-lock attestations this
    # list used to require are deliberately absent — §3 of the directive requires exactly that
    # ("no in-lock FutexWait production marker"). Each is replaced by its pre-lock counterpart,
    # and the set is strictly stronger: the broad dispatcher is SKIPPED entirely rather than
    # bypassed from inside the handler it already entered.
    #   RISCV_FUTEX_WAIT_RETIRE_DEFAULT_ON        -> FUTEX_WAIT_SPLIT_BEGIN
    #   RISCV_FUTEX_WAIT_DISPATCH_DEFER_BEGIN     -> QUEUE_ADVANCING_DISPATCH_DEFERRED
    #   RISCV_FUTEX_WAIT_DISPATCH_BLOCK_PUBLISH_OK-> FUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK
    #   RISCV_FUTEX_WAIT_HANDLER_BYPASS_{BEGIN,DONE} -> result=queue_advance_committed
    #                                                 + QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED
    "FUTEX_WAIT_SPLIT_BEGIN"
    "FUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK tid="
    "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=futex_wait_switch_required tid="
    "result=queue_advance_committed outgoing="
    "QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED cpu=0 reason=publication_committed"
    "RISCV_FUTEX_WAIT_DISPATCH_DRAIN_BEGIN cpu=0"
    "RISCV_FUTEX_WAIT_DISPATCH_NO_INCOMING cpu=0"
    "RISCV_FUTEX_WAIT_POST_LOCK_IDLE_BEGIN cpu=0"
    "RISCV_FUTEX_WAIT_POST_LOCK_IDLE_LOCK_DROPPED_OK cpu=0"
    "RISCV_FUTEX_WAIT_DISPATCH_DONE result=idle"
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=FutexWait result=ok"
    "RISCV_FUTEX_WAIT_POST_LOCK_IDLE_ENTERED cpu=0"
  )
  for pat in "${FUTEX_WAIT_IDLE_PATTERNS[@]}"; do
    if ! rg -n -F "$pat" "$LOGFILE" >/dev/null 2>&1; then
      echo "[fail] futex-wait idle oracle marker missing: $pat"
      failures=$((failures + 1))
    fi
  done
  # The idle-oracle attestation with all three flags set is mandatory. Use -a (NUL bytes in log).
  if ! rg -a "RISCV_FUTEX_WAIT_IDLE_ORACLE_DONE result=ok lock_dropped=1 current_none=1 outgoing_blocked=1" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] futex-wait idle oracle did not prove the idle outcome (lock_dropped/current_none/outgoing_blocked)"
    failures=$((failures + 1))
  fi
  # The canonical RISC-V idle terminal must be reached (reused, not a duplicate idle impl).
  if ! rg -n -F "RISCV_KERNEL_IDLE_WAITING_FOR_IO" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] futex-wait idle oracle did not reach the canonical RISC-V idle terminal"
    failures=$((failures + 1))
  fi
  # Forbidden: the blocked caller must NOT be resumed (no sret into it), and no switch markers.
  if rg -n "RISCV_FUTEX_WAIT_IDLE_ORACLE_UNEXPECTED_RETURN|RISCV_FUTEX_WAIT_DISPATCH_SRET_ARMED|RISCV_FUTEX_WAIT_DISPATCH_FAIL|RISCV_FUTEX_WAIT_DISPATCH_DEFERRED reason=state_changed" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] futex-wait idle oracle took a switch/return/fail path instead of idle"
    failures=$((failures + 1))
  fi
  # Zero Yield / NR27 retirement markers.
  if rg -n "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=InitramfsReadChunk" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] futex-wait idle oracle boot leaked an NR27 retirement marker"
    failures=$((failures + 1))
  fi
fi

# Stage 196G (YIELD TWO-TASK ORACLE): the default-on Yield retirement must switch A→B and later
# re-dispatch A — full re-enqueue + real SATP/sfence/frame/sret switch + round trip.
if [[ "$YIELD_TWO_TASK_ORACLE" == "1" ]]; then
  YIELD_TWO_PATTERNS=(
    "RISCV_YIELD_RETIRE_DEFAULT_ON result=ok"
    "RISCV_YIELD_DISPATCH_DEFER_BEGIN cpu=0 outgoing="
    "RISCV_YIELD_DISPATCH_REENQUEUE_OK cpu=0 outgoing="
    "RISCV_YIELD_HANDLER_BYPASS_BEGIN cpu=0 outgoing="
    "RISCV_YIELD_HANDLER_BYPASS_DONE cpu=0"
    "RISCV_YIELD_DISPATCH_DRAIN_BEGIN cpu=0"
    "RISCV_YIELD_DISPATCH_LOCK_DROPPED_OK cpu=0"
    "RISCV_YIELD_DISPATCH_REVERIFY_OK outgoing="
    "RISCV_YIELD_DISPATCH_DEQUEUE_OK cpu=0 incoming="
    "RISCV_YIELD_DISPATCH_CURRENT_SET_OK cpu=0 incoming="
    "RISCV_YIELD_DISPATCH_RUNNING_OK incoming="
    "RISCV_YIELD_DISPATCH_SATP_OK incoming="
    "RISCV_YIELD_DISPATCH_SFENCE_OK incoming="
    "RISCV_YIELD_DISPATCH_FRAME_OK incoming="
    "RISCV_YIELD_DISPATCH_SRET_ARMED incoming="
    "RISCV_YIELD_DISPATCH_DONE result=ok"
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=Yield result=ok"
    "RISCV_YIELD_TWO_TASK_INCOMING_USER_OK tid="
    "RISCV_YIELD_TWO_TASK_OUTGOING_RESUMED_OK tid="
  )
  for pat in "${YIELD_TWO_PATTERNS[@]}"; do
    if ! rg -n -F "$pat" "$LOGFILE" >/dev/null 2>&1; then
      echo "[fail] yield two-task oracle marker missing: $pat"
      failures=$((failures + 1))
    fi
  done
  # The round-trip DONE with outgoing_resumed=1 is mandatory. Use -a (NUL bytes in the log).
  if ! rg -a "RISCV_YIELD_TWO_TASK_ORACLE_DONE result=ok .*outgoing_resumed=1" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] yield two-task oracle did not prove the round-trip (outgoing_resumed=1)"
    failures=$((failures + 1))
  fi
  # No Yield FAIL / state_changed / result=fail.
  if rg -n "RISCV_YIELD_DISPATCH_FAIL|RISCV_YIELD_DISPATCH_DEFERRED reason=state_changed|RISCV_YIELD_TWO_TASK_ORACLE_DONE result=fail" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] yield two-task oracle reported a failure"
    failures=$((failures + 1))
  fi
fi

# Stage 196G (YIELD LONE-TASK ORACLE): the only runnable task yields and self-redispatches (the
# drain dequeues the caller itself) — NEVER idle. Repeated Yields prove the mechanism is not one-shot.
if [[ "$YIELD_LONE_TASK_ORACLE" == "1" ]]; then
  YIELD_LONE_PATTERNS=(
    "RISCV_YIELD_RETIRE_DEFAULT_ON result=ok"
    "RISCV_YIELD_DISPATCH_REENQUEUE_OK cpu=0 outgoing="
    "RISCV_YIELD_DISPATCH_DEQUEUE_OK cpu=0 incoming="
    "RISCV_YIELD_DISPATCH_SATP_OK incoming="
    "RISCV_YIELD_DISPATCH_SFENCE_OK incoming="
    "RISCV_YIELD_DISPATCH_FRAME_OK incoming="
    "RISCV_YIELD_DISPATCH_DONE result=ok"
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=Yield result=ok"
    "RISCV_YIELD_LONE_TASK_REPEAT_OK tid="
  )
  for pat in "${YIELD_LONE_PATTERNS[@]}"; do
    if ! rg -n -F "$pat" "$LOGFILE" >/dev/null 2>&1; then
      echo "[fail] yield lone-task oracle marker missing: $pat"
      failures=$((failures + 1))
    fi
  done
  # redispatched_self=1 is mandatory. Use -a (NUL bytes in the log).
  if ! rg -a "RISCV_YIELD_LONE_TASK_ORACLE_DONE result=ok .*redispatched_self=1" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] yield lone-task oracle did not prove self-redispatch (redispatched_self=1)"
    failures=$((failures + 1))
  fi
  # The lone-Yield transition must NEVER fail or idle: the Yield drain has NO idle branch, so a
  # Yield FAIL / state_changed is the failure signal (redispatched_self=1 proves the self-switch).
  # (A parks via FutexWait AFTER the proof, which legitimately reaches the FutexWait post-lock idle
  # terminal — that is not a Yield-transition idle and is not forbidden here.)
  if rg -n "RISCV_YIELD_DISPATCH_FAIL|RISCV_YIELD_DISPATCH_DEFERRED reason=state_changed" "$LOGFILE" >/dev/null 2>&1; then
    echo "[fail] yield lone-task oracle reported a Yield-transition failure"
    failures=$((failures + 1))
  fi
fi

# U9-PAGEFAULT1 §2: the RISC-V terminal PageFault route, asserted POSITIVELY in its own cell.
#
# Same trigger, same selector and same knob as the AArch64 and x86_64 cells: one deliberate
# unhandled read at address 0, taken by init before the service chain. The BASELINE was measured
# before the routing row was added — one fault per boot, reaching the broad dispatcher — so the
# row rests on a fault this port actually produces, not on the other two ports' evidence.
if [[ "$TERMINAL_FAULT_ORACLE" != "1" ]]; then
  echo "[info] U9-PF1-TERM: not armed (set TERMINAL_FAULT_ORACLE=1) -- the RISC-V terminal-fault witness runs in its own cell"
else
pf1t_fail=0
# U9-PAGEFAULT1 §2: one cell, two scenarios. The chain is identical apart from the access the
# CPU reports and which decoder produced it, so the assertions are parameterised rather than
# duplicated -- a second copy would drift.
if [[ "$TERMINAL_FAULT_FETCH_ORACLE" == "1" ]]; then
  pf1t_access=Execute
  pf1t_user_access=fetch
  pf1t_provision=TERMINAL_FAULT_FETCH_ORACLE_PROVISION_OK
  pf1t_slot5=24
else
  pf1t_access=Read
  pf1t_user_access=read
  pf1t_provision=TERMINAL_FAULT_ORACLE_PROVISION_OK
  pf1t_slot5=23
fi
pf1t_log="$(tr '\r' '\n' <"$LOGFILE")"
pf1t_count() {
  local n
  n="$(printf '%s\n' "$pf1t_log" | rg -a -F -c -- "$1" || true)"
  printf '%s' "${n:-0}"
}
pf1t_require_one() {
  local want_desc="$1" pat="$2" n
  n="$(pf1t_count "$pat")"
  if [[ "$n" != "1" ]]; then
    echo "[error] U9-PF1-TERM: $want_desc -- expected exactly 1, got ${n:-0}: $pat"
    pf1t_fail=1
  else
    echo "[ok] U9-PF1-TERM: $want_desc"
  fi
}
pf1t_require_zero() {
  local want_desc="$1" pat="$2" n
  n="$(pf1t_count "$pat")"
  if [[ "$n" != "0" ]]; then
    echo "[error] U9-PF1-TERM: $want_desc -- expected 0, got $n: $pat"
    pf1t_fail=1
  else
    echo "[ok] U9-PF1-TERM: $want_desc"
  fi
}
pf1t_require_one "the RISC-V terminal-fault oracle is provisioned once" \
  "${pf1t_provision} arch=riscv64 slot5=${pf1t_slot5} caps=none result=ok"
pf1t_require_one "init takes exactly one deliberate ${pf1t_user_access} at 0x0" \
  "TERMINAL_FAULT_ORACLE_BEGIN arch=riscv64 init_tid=1 addr=0x0 access=${pf1t_user_access}"
pf1t_require_zero "the deliberate ${pf1t_user_access} never returns" 'TERMINAL_FAULT_ORACLE_UNREACHABLE'
pf1t_require_one "tid 1 ${pf1t_access} fault at 0x0 entered once" \
  "PAGE_FAULT_ENTRY tid=1 addr=0x0 access=${pf1t_access}"
pf1t_require_one "the fault is reported unhandled exactly once" \
  "PAGE_FAULT_UNHANDLED tid=1 addr=0x0 access=${pf1t_access}"
pf1t_require_one "report targets endpoint 3 at its exact generation" \
  'TASK_FAULT_REPORT_TARGET tid=1 endpoint=3 generation=1'
pf1t_require_one "report is BUFFERED exactly once with woke=0" \
  'TASK_FAULT_REPORT_ENQUEUE_OK tid=1 endpoint=3 queued=1 woke=0'
# U9-PAGEFAULT2 §2: ONE total delivery owner now names its own ending. These make "exactly one
# publication, and the route knows which" live rather than structural.
pf1t_require_one "the report delivery owner runs exactly once" \
  'TASK_FAULT_REPORT_BEGIN tid=1'
pf1t_require_one "and it reports the BUFFERED ending, off the broad lock" \
  'TERMINAL_FAULT_SPLIT_REPORT cpu=0 tid=1 outcome=buffered terminates=1 broad_lock=0'
pf1t_require_zero "and the waiter ending is not taken when no waiter is blocked" \
  'TASK_FAULT_REPORT_BLOCKED_WAITER_FOUND'
pf1t_require_one "terminal task transition commits exactly once" \
  'TERMINAL_FAULT_SPLIT_COMMITTED cpu=0 tid=1 captured=1 advance=deferred'
pf1t_require_one "the queue-advance deferral is published exactly once" \
  'QUEUE_ADVANCING_DISPATCH_DEFERRED reason=terminal_fault_switch_required tid=1 cpu=0'
# THE MEASUREMENT: settled without the broad dispatcher, asserted both ways.
pf1t_require_one "broad dispatcher skipped for the terminal fault" \
  'QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED cpu=0 reason=terminal_fault_committed'
pf1t_require_zero "the fault never reaches the broad dispatcher" \
  "PF1_BROAD_ARRIVAL cpu=0 tid=1 addr=0x0 access=${pf1t_access}"
# ANOTHER TASK PROGRESSES: the existing RISC-V drain selects tid 2 and restores its exact frame.
pf1t_require_one "queue selection happens once and chooses tid 2" \
  'RISCV_FUTEX_WAIT_DISPATCH_DEQUEUE_OK cpu=0 incoming=2'
pf1t_require_one "replacement tid 2 is marked Running" \
  'RISCV_FUTEX_WAIT_DISPATCH_RUNNING_OK incoming=2'
pf1t_require_one "replacement tid 2 gets its exact frame" \
  'RISCV_FUTEX_WAIT_DISPATCH_FRAME_OK incoming=2'
pf1t_require_one "the drain completes once" 'RISCV_FUTEX_WAIT_DISPATCH_DONE result=ok'
pf1t_require_zero "the faulting PC never resumes (no ownerless re-fault)" \
  'PAGE_FAULT_ENTRY tid=18446744073709551615'
pf1t_require_zero "no split refusal on the witnessed path" 'TERMINAL_FAULT_SPLIT_REFUSED'
pf1t_require_zero "no fail-closed settlement on the witnessed path" \
  'TERMINAL_FAULT_SPLIT_FAILED_CLOSED'
pf1t_require_zero "no unattributable fault on the witnessed path" 'PF1_UNATTRIBUTABLE_FAULT'
# U9-PAGEFAULT2 §3: none of the newly settled refusals fire on the clean path. Each is a real
# settlement now rather than a fall-through, so a silent appearance here would change the
# witnessed outcome rather than merely hand it to the broad arm.
pf1t_require_zero "no raced recovery class reaches the terminal route" \
  'TERMINAL_FAULT_SPLIT_RACED'
pf1t_require_zero "the queue admission is not refused under the trap authority" \
  'TERMINAL_FAULT_SPLIT_REFUSED cpu=0 tid=1 phase=queue_admit'
pf1t_require_zero "the deferral is not contended" \
  'TERMINAL_FAULT_SPLIT_REFUSED cpu=0 tid=1 phase=defer'
if [[ "$pf1t_fail" -eq 1 ]]; then
  echo "[error] U9-PAGEFAULT1 RISC-V terminal-fault witness FAILED"
  exit 1
fi
echo "[ok] U9-PF1-TERM: RISC-V terminal PageFault route witness chain complete"
fi

if (( failures > 0 )); then
  echo "[fail] qemu-riscv64-core-smoke: ${failures} check(s) failed (qemu_status=${QEMU_STATUS})"
  [[ "$QEMU_SMOKE_STRICT" == "1" ]] && exit 1
  exit 0
fi

echo "[ok] qemu-riscv64-core-smoke passed (smp=${QEMU_SMP}, qemu_status=${QEMU_STATUS})"
exit 0
