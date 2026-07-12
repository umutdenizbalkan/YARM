# First-Cohort Global-Lock Retirement Seal (Stage 197)

This document is the authoritative record of the **first four global-lock
retirement classes** sealed across all three architectures. It is built from the
current source (not copied from prior stage reports). It **enables zero new
retirement classes** — it audits, normalizes, tests, documents, and seals what is
already live.

> **Scope caveat.** This seal covers exactly the four classes below. It does **not**
> claim the broad global `KernelState` lock is fully retired — every non-cohort
> syscall still takes the broad `with_cpu` phase.

Baseline (unchanged): `SYSCALL_COUNT = 32`, `Syscall::VARIANT_COUNT = 23`.
NR identities: `Yield = 0`, `FutexWait = 9`, `FutexWake = 10`, `DebugLog = 15`.

## 1. Canonical cohort identity

```
FirstCohort = { DebugLog, FutexWake, FutexWait, Yield }
```

**Explicitly excluded** (NOT part of the permanent cohort):
`InitramfsReadChunk` (NR 27), `D2` recv/send drains, `IpcSend*` (plain / ordinary-cap
/ reply-cap / enqueue variants), VM operations, fork/COW, spawn, cap
mint/materialization, `ReapFaultedTask`.

Canonical class-name vocabulary (exact spelling/case, no drift):
`DebugLog`, `FutexWake`, `FutexWait`, `Yield`.

## 2. Authoritative 3×4 implementation matrix

Legend: **producer** = where the class is serviced; **publish** = pre-lock split vs
in-lock publication; **deferral** = per-CPU deferral state; **drain** = post-lock
drain; **restore** = arch activation hook.

### DebugLog (NR 15) — pre-lock split, same-task return, no scheduler mutation

| | x86_64 | AArch64 | RISC-V |
|---|---|---|---|
| producer | `syscall_split::try_split_debug_log_into_frame` | same | same (via `handle_riscv_trap_entry_shared` gate) |
| publish | pre-lock split (early return, no broad-lock phase) | same | same (NR 15 in the wrapper split gate) |
| default-on | yes | yes | yes |
| fallback | broad-lock handler once | same | same |
| deferral | none (no switch) | none | none |
| drain | none | none | none |
| restore | none (caller stays current) | none | none |
| user return | same-task syscall return | same | bridge same-task ecall write-back |
| idle | n/a | n/a | n/a |
| proof | core boot (natural) + `RISCV_DEBUGLOG_SPLIT_USER_RETURN_OK` | core boot | core boot + user-return marker |
| marker | `YARM_LOCK_SPLIT_DISPATCH arch=x86_64 nr=15` + `GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=DebugLog result=ok` | `arch=aarch64` | `arch=riscv64` |

### FutexWake (NR 10) — pre-lock split, waiter+enqueue mutation, caller stays current

| | x86_64 | AArch64 | RISC-V |
|---|---|---|---|
| producer | `syscall_split::try_split_futex_wake_into_frame` → `futex_wake_split_mut` | same | same |
| publish | pre-lock split (early return) | same | same (NR 10 in the wrapper split gate) |
| default-on | yes | yes | yes |
| deferral | none (caller not switched) | none | none |
| drain | none | none | none |
| restore | none (caller stays current) | none | none |
| user return | same-task, wake count in a0/x0 | same | bridge same-task write-back |
| idle | n/a | n/a | n/a |
| proof | FutexWake oracle (first=1/second=0) | oracle | oracle (`RISCV_FUTEX_WAKE_LIVE_ORACLE_DONE result=ok first_wake=1 second_wake=0`) |
| marker | `YARM_LOCK_SPLIT_DISPATCH arch=x86_64 nr=10` + `class=FutexWake` | `arch=aarch64` | `arch=riscv64` |

### FutexWait (NR 9) — in-lock Blocked publish, current cleared, post-lock switch, idle possible

| | x86_64 | AArch64 | RISC-V |
|---|---|---|---|
| producer | `exec_state::futex_wait_current` (publish) + `arch/trap_entry.rs` drain | same publish + `arch/trap_entry.rs` (aarch64 arm) | same publish + `arch/riscv64/trap.rs` drain |
| publish | in-lock: `Blocked(Futex)` + `block_current` + defer | same | same |
| default-on | yes (`d6_genuine_enabled`, single-CPU) | yes (BSP, single dispatcher) | yes (BSP, single dispatcher) |
| deferral | `FUTEX_WAIT_DISPATCH_*` (shared) | shared | shared |
| reverify | `futex_wait_reverify_blocked` (still Blocked) | same | same |
| dequeue | `futex_wait_dispatch_step_mut` | same | same |
| restore | CR3/ASID via `d2_recv_switch_incoming_asid` + `post_switch_restore_arch_thread_state` → `iretq`/`sysret` | TTBR0_EL1/ASID + EL0 frame → `eret` | `cr3_for_asid`+`map_kernel_shared_into_asid`+`write_satp` (real `sfence.vma`) + frame → `sret` |
| idle (no-incoming) | drain `else`: clear deferral, `incoming=idle`, NO frame restored, x86 scheduler HLT idle terminal | `enter_post_lock_idle` (dedicated) | drain returns `Err(Internal)` w/ `current==None` → bridge `RISCV_KERNEL_IDLE_WAITING_FOR_IO` |
| idle proof | source-audited (see §4) | `AARCH64_FUTEX_WAIT_IDLE_ORACLE_DONE result=ok` | `RISCV_FUTEX_WAIT_IDLE_ORACLE_DONE result=ok lock_dropped=1 current_none=1 outgoing_blocked=1` |
| switch proof | FutexWait switch oracle | switch oracle SMP=2 | switch oracle (`RISCV_FUTEX_WAIT_LIVE_ORACLE_DONE result=ok wake_count=1`) |
| marker | `class=FutexWait` (arch=x86_64) | `arch=aarch64` | `arch=riscv64` |

### Yield (NR 0) — in-lock Runnable re-enqueue, current cleared, post-lock switch, NO idle

| | x86_64 | AArch64 | RISC-V |
|---|---|---|---|
| producer | `exec_state::yield_current` (re-enqueue) + `arch/trap_entry.rs` drain | same | same publish + `arch/riscv64/trap.rs` drain |
| publish | in-lock: `Runnable` + `preempt_reenqueue_current_cpu` (once at FIFO tail) + clear current + defer | same | same |
| default-on | yes | yes (BSP) | yes (BSP) |
| deferral | `YIELD_DISPATCH_*` (shared) | shared | shared |
| reverify | `yield_reverify_ready` (current still cleared) | same | same |
| dequeue | `yield_dispatch_step_mut` (FIFO head; caller itself when alone) | same | same |
| restore | CR3/ASID + frame → return | TTBR0/ASID + EL0 frame → `eret` | SATP + `sfence.vma` + frame → `sret` |
| idle | NONE (always an incoming: another task or the re-enqueued caller) | NONE | NONE — no-incoming is `RISCV_YIELD_DISPATCH_FAIL reason=no_incoming` (not idle, no `Err(Internal)` sentinel) |
| two-task proof | Yield two-task oracle | two-task SMP=2 | `RISCV_YIELD_TWO_TASK_ORACLE_DONE result=ok outgoing_resumed=1` |
| lone/self proof | Yield lone-task oracle | lone-task SMP=2 | `RISCV_YIELD_LONE_TASK_ORACLE_DONE result=ok redispatched_self=1` + `RISCV_YIELD_LONE_TASK_REPEAT_OK` |
| marker | `class=Yield` (arch=x86_64) | `arch=aarch64` | `arch=riscv64` |

## 3. Canonical marker contract

Post-lock / in-lock classes (FutexWait, Yield) and pre-lock classes (DebugLog,
FutexWake) all emit:

```
GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=<arch> class=<class>
GLOBAL_LOCK_RETIRE_CLASS_DONE  arch=<arch> class=<class> result=ok
```

Pre-lock classes additionally emit:

```
YARM_LOCK_SPLIT_DISPATCH arch=<arch> nr=15     # DebugLog
YARM_LOCK_SPLIT_DISPATCH arch=<arch> nr=10     # FutexWake
```

`<arch> ∈ { x86_64, aarch64, riscv64 }`. **Stage 197 normalized x86_64 from the
historical untagged text** (`class=<class>` with no `arch=`) to `arch=x86_64`; no
architecture emits both tagged and untagged production markers. `YARM_LOCK_SPLIT_DISPATCH`
is **not** required for FutexWait or Yield (they retire via in-lock publish + post-lock drain).

## 4. x86_64 FutexWait no-incoming audit (asymmetry, explicitly scoped)

The three architectures reach the same **safety guarantees** for the no-incoming
(idle) outcome, via **different terminals** — this is an intentional, documented
design difference, not an implementation identity claim:

| guarantee | x86_64 | AArch64 | RISC-V |
|---|---|---|---|
| broad lock released before idle | yes (drain runs post-`with_cpu`) | yes | yes (re-acquire proof `POST_LOCK_IDLE_LOCK_DROPPED_OK`) |
| `current` None/idle | yes (`step_mut`→None, current stays cleared) | yes | yes (`current_none=1`) |
| outgoing stays `Blocked(Futex)` | yes (reverify_ok gate) | yes | yes (`outgoing_blocked=1`) |
| no userspace frame restored | yes (`incoming=idle` branch skips restore) | yes | yes |
| terminal | x86 scheduler HLT idle (drain clears deferral + emits `incoming=idle`) | `enter_post_lock_idle` (dedicated fn) | bridge `Err(Internal)` w/ `current==None` → `RISCV_KERNEL_IDLE_WAITING_FOR_IO` |
| live idle oracle | not separately wired (source-audited; equivalent guarantees) | `aarch64_futex_wait_idle_oracle` | `riscv64_futex_wait_idle_oracle` |

**The seal is defined in terms of equivalent safety guarantees, not identical
implementation.** The AArch64 + RISC-V idle oracles provide the live idle proof;
x86_64's `incoming=idle` branch is source-audited and shares the generic
reverify/dequeue seam. In a natural x86_64 boot, the final tasks block on IPC recv
(not FutexWait), so the FutexWait no-incoming branch is a defensive path rather than
a naturally-hit one.

## 5. Eligibility equivalence (semantic, not source-identical)

The seal does **not** force identical source predicates. It establishes semantic
equivalence — every architecture requires:

- a genuine post-lock drainer exists (`GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu]`);
- the calling CPU owns supported userspace dispatch (BSP);
- a safe dispatcher topology (`dispatching_cpu_count() <= 1`);
- no same-class deferral pending;
- no conflicting queue-switch deferral pending (FutexWait ⟂ Yield ⟂ 196D foundation);
- legacy in-lock fallback remains when ineligible.

Intentional per-arch differences (documented, not defects):

- **x86_64**: gated on `d6_genuine_enabled()`; experimental AP dispatch exists but is
  off by default (`ap_user_dispatch` knob).
- **AArch64**: BSP-only user dispatch with wake-only APs (195D affinity guarantees
  `dispatching_cpu_count() <= 1` under SMP=2).
- **RISC-V**: single-dispatcher model (AP user dispatch not enabled).

## 6. Architecture restore matrix (all real, no cross-arch logic)

| | x86_64 | AArch64 | RISC-V |
|---|---|---|---|
| address space | CR3 (PCID-compatible) via `switch_address_space` | TTBR0_EL1/ASID via `switch_address_space` (carries DSB/ISB/TLBI) | SATP via `write_satp` (real `csrw satp` + `sfence.vma x0,x0`) |
| TLB | real TLB-shootdown ack work preserved | DSB/ISB/TLBI ordering preserved | global `sfence.vma` |
| frame | RIP/RFLAGS/GPR | SPSR_EL1/ELR_EL1/GPR | sepc/sstatus/GPR (no double +4) |
| return | `iretq`/`sysret` (as implemented) | `eret` | `sret` (bridge) |

No architecture uses another architecture's restore logic (hosted guards assert the
RISC-V drain never references `arch::x86_64::page_table` / `arch::aarch64::page_table`
/ `write_cr3(` / `set_ttbr0`).

## 7. Known debts (kept open)

- **NR 27 (InitramfsReadChunk):** obsolete fallback/test ABI. Remove after all PM and
  crash-restart paths use the ZC-grant loader. **NOT part of the permanent cohort.**
- **RISC-V trap stack:** 2 MiB is an emergency correctness size. Measure maximum
  trap/syscall stack depth and remove oversized frames before RISC-V SMP scaling. Not
  reduced here.
- **RISC-V idle sentinel:** the FutexWait idle handoff uses an `Err(Internal)`-shaped
  bridge exit. A hosted regression guard (Stage 197) proves the FutexWait idle SUCCESS
  attestation (`RISCV_FUTEX_WAIT_IDLE_ORACLE_DONE`) is emitted **only** from the
  attested FutexWait idle branch (gated on the idle-oracle knob) and can never be
  produced by an unrelated `Internal` error. The sentinel convention is **not** used
  by Yield or any other class. A future typed idle outcome remains recommended.

## 8. Normal-boot vs oracle distinction

- **Default-on mechanism**: production eligibility is active with **no oracle knob**.
- **Oracle**: a **default-off** workload that deterministically causes a specific
  class/path to execute (e.g. a two-task FutexWait switch).

A plain boot naturally executes **DebugLog** but generally **not** FutexWait or Yield
(servers block on IPC recv). Absence of a `class=FutexWait`/`class=Yield` marker on a
workload that never invokes NR 9/NR 0 is **not** evidence the mechanism is disabled —
the mechanism is proven default-on by **hosted source guards** + **live oracles**
together.

## 9. Seal result

The combined validation script `scripts/qemu-first-cohort-retirement-seal.sh` runs
the full fresh matrix and emits:

```
FIRST_COHORT_SEAL arch=x86_64  classes=4 result=ok
FIRST_COHORT_SEAL arch=aarch64 classes=4 result=ok
FIRST_COHORT_SEAL arch=riscv64 classes=4 result=ok
FIRST_COHORT_CROSS_ARCH_SEAL arches=3 classes=4 result=ok
```

These are **validation-script** markers derived from the per-arch QEMU logs — no
kernel markers were added solely to fabricate the matrix.
