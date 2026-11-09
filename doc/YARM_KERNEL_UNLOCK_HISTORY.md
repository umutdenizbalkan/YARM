<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM kernel unlocking — the full history

Evidence labels: **[VC]** verified in source at `b81e0317`; **[VH]** verified from commits or
committed records; **[SR]** session-reported; **[INF]** inferred; **[UNK]** unknown. Sources:
`doc/YARM_RECOVERY_SOURCES.md`. The canonical, line-by-line record of every stage is
`doc/KERNEL_UNLOCKING.md`; this file is the navigable account of it, not a replacement.

---

## 0. What "unlocked" means — and the two things it is easy to confuse

* **The broad lock.** `SharedKernel` (`src/runtime.rs`) used to own one mutex over the entire
  kernel, `state: SpinLock<KernelState>`, entered through `lock` (test-only), `with` and
  `with_cpu`. Every syscall, trap and IRQ ran inside it. [VH]
* **The measurable definition** (owner-supplied, `doc/KERNEL_UNLOCKING.md` §0.1): full unlock =
  no runtime guard exists (canonical **204E**), sealed cross-architecture (canonical **205D**,
  markers `KERNEL_RUNTIME_GLOBAL_LOCK_CALLS arch=<a> count=0`, `KERNEL_FULL_UNLOCK_SEAL`,
  `KERNEL_FULL_UNLOCK_CROSS_ARCH_SEAL arches=3`). [VH]
* **Status vocabulary** (§0.2), which must never be collapsed:
  * *hosted foundation* — mechanism exists and is hosted-tested, possibly `HELPER_ONLY`;
  * *live proof* — proven on a genuine clean QEMU boot, possibly knob-gated (proves the
    mechanism, **not** the production path);
  * *stage complete* — full scope retired on the production path, every required
    architecture, no broad-lock fallback. [VH]
* **Directive vs. canonical stage.** Directives (U1…U9, BL*, QEMU-*, AP-SCHED*, …) are bounded
  work packages. Canonical stages (199C…205D) are properties of the kernel defined by the owner.
  "A historical stage carrying the same number does not complete the canonical stage unless it
  satisfies the definition." Example: directive U3 met its exit (census ≤ 24) while canonical
  203C stayed OPEN. [VH, `doc/KERNEL_UNLOCKING.md` §0.1]

### Current position [VC + VH]

* **No production broad lock exists.** U9 removed the last production acquisition
  (U9-TERMINAL-FINAL §5, `0dce4a7f`, 2026-09-24; U9 closed at `93900ae0`). BROAD-LOCK-RETIREMENT1
  (BLR1, `a75b1601`, 2026-10-13) then **deleted** `SpinLock<KernelState>`, `SharedKernel::with`
  and `with_cpu` from production builds. Boot owns the state through a `BootExclusive` token
  (`src/runtime.rs:27-39`) consumed by `publish` before the first user entry; afterwards each
  field has exactly one recorded authority (`FIELD_AUTHORITY`, enforced by
  `tests/broad_lock_census_guard.rs`, which also rejects any reintroduction). [VC, VH]
* **The canonical stage table was NOT re-classified** by U9 or BLR1. By the owner's rule, the
  census "completes no stage other than 204A", and BLR1 states that deleting the lock "does not
  by itself complete 204B–205". So the §0.3 table (written pre-U9) still reports 3 of 35
  complete (199C, 199E, 204A). **Read it as "not re-classified", not as "the broad lock
  exists".** [VH, contradiction recorded in `doc/YARM_RECOVERY_SOURCES.md` §6]
* State today is one `KernelState` struct reached through one cell, partitioned by **ranked
  domain locks** acquired in ascending order (`doc/KERNEL_LOCKING.md`): 1 scheduler (now
  per-CPU, SCHED-LOCK1), 2 task/TCB, 3 IPC, 4 capability, 5 VM/user spaces, 6 memory/frames,
  7 driver, 8 fault, 9 restart, 10 telemetry (the docs count 11 ranked locks including the
  per-CPU split). [VH]

---

## 1. Phase A — before the programme (Feb–May 2026)

* 2026-02-25 `3088f528` initial import (Codex PR #1): RISC-V trap decoder, IPC, capabilities,
  VM, syscalls, Linux-compat. [VH]
* 2026-04-01 `ac180f38` "Complete phase 2 SchedulerState extraction"; 2026-04-02 `be5addf0`
  frame allocator under `SpinLockIrq`; 2026-04-30 `b21bb013` "Document current locking status
  and narrow stage-2 candidates" — the first written locking plan. [VH]
* The single broad lock was the design: correctness by serialization while IPC, capabilities,
  spawn, VM and the three architecture ports were built. [INF from history]

## 2. Phase B — Kernel Unlocking Milestones 1–2 (June 2026, Stages 101–132)

| Stage | Commit | What happened |
|---|---|---|
| 101 | `3d2cd43f` 06-11 | "Kernel unlocking restart": MUST_SMOKE policy, syscall decomposition map, directives D1–D7 (D1/D5 recv-side cap materialization, D2 endpoint blocking recv, D3 VmAnonMap/VmBrk two-phase, D6 scheduler dispatch). |
| 102–105 | `02c3ddfb`… | mechanical syscall decomposition; D1 transfer-cap recv split live-wired (104); D5 reply-cap split with mint rollback (105). |
| 106 | `6241deb7` 06-12 | **Milestone 1 declared**: D1/D2/D5 live splits, D3 gated with structural proof; smoke on x86_64 `-smp 1` core + optional-FS strict and AArch64 optional-FS strict. |
| 108 | `84033c82` | Milestone 2 Pass 1: split-mut seams for ranks 1/2/5/6, `yarm.loglevel=`, x86 SMP trampoline split; zero behaviour change. |
| 109 | `f8b6418e` | Milestone 2 Pass 2: x86 AP enters Rust and parks; scheduler BSP-only. |
| 110–115 | 06-16 | D2/D3/D6 phase splits ("Outcome B" = preparatory refactor, live wire deferred); D3 VmBrk shrink live-wired (114); IPC rank-3 seam (115). |
| 116 | `41fb0e63` | task lock dropped before `switch_frames` (`DispatchSwitchPlan`). |
| 117–132 | 06-17 → 06-19 | x86 D6 unlocked `switch_frames` proof harness bring-up: first-resume ABI, switch-stack mapping (126–129), `MXCSR=0` after `fxrstor` from a zeroed area (131, `b5874437`), unmapped kernel-stack #PF (132). The harness is a default-off diagnostic, mutually exclusive with production D6-genuine. |

Remaining limitation at the end of Phase B: everything live ran x86 `-smp 1`; the broad lock
still wrapped every trap. [VH]

## 3. Phase C — graduation, cross-arch, first cohorts (late June – July, Stages 133–198)

* **133–167:** stability and extraction (syscall families split into `syscall/vm.rs`, helpers,
  IPC ABI audits), IPC oracle knobs (`yarm.ipc_recv_proof`, 159), sender-wake oracle — Stage 161
  explicitly recorded as **DEFERRED with a precise blocker, not faked** (`f076a5a3`); fork/COW
  repairs 163D–163P (including a capacity raise `MAX_ADDRESS_SPACES` 32→48 at 163D, the kind of
  shortcut later briefs forbid), D6 proof stack fixes 165B–165J (x86 kernel stacks 64→128 KiB).
* **168–169** (07-01): D6-GENUINE-B mutating dispatch relocation, D2-GENUINE-RECV and
  D2-GENUINE-SEND — blocking IPC dispatch out of the global lock on x86_64.
* **170–180:** IPC-FINAL, SCHED-TIMEOUT, VM-COW, CAP-CNODE, FAULT-DELIVERY, SPAWN-LIFECYCLE,
  GLOBAL-STATE, SMP-READY, CROSS-ARCH-D6 audits + diagnostics; CI-PROFILES (180).
* **181 GRADUATE-KNOBS / "big-bang unlock"** (accepted x86 `-smp 1` seams default-on);
  **182 REMOVE-FALLBACKS** (`a4cddd44`); **183 SMP-LIVE**; **184 CROSS-ARCH-LIVE**.
* **185–188:** GLOBAL-LOCK-RETIRE inventory; several audits ended **HARD-STOP, no code change**
  (185B, 185V, 186B, 186D4, 187D) — the correct outcome when the seam is not provably safe.
  188D solved the reply-cap rank inversion by phase separation.
* **189–190:** x86 first live AP ring-3 dispatch (189C6 `8b0ffaf6`), SMP user-dispatch seal
  (189D), AP scheduler loop (190A).
* **191–197: syscall cohorts retired from the broad lock.** FutexWake (191B), FutexWait
  block-publish seam (191D, live conversion deferred) then queue-advancing out-of-lock dispatch
  (192A); IpcSend shapes (193A/C/E; 193G NO-GO for reply-cap enqueue and shared region); AArch64
  (195A–G) and RISC-V (196B–F) ports. **Stage 197 `53574ccb` (07-12): first-cohort
  cross-architecture seal** — DebugLog, FutexWake, FutexWait, Yield on 3 arches = 12 live cells.
  197A-B `696db6cb`: **NR 27 removed**, zero-copy ELF loading mandatory.
* **198:** fail-closed builds and seal isolation (198B1), reply-cap one-shot direct delivery
  (198C*, 198D-S direct-only reply caps), shared-region post-lock transaction (198E*,
  generation-bearing endpoint waiter identity 198E3B2B2), **shared-region direct 3/3 seal**
  (198E3D-S `f1d424b6`).

## 4. Phase D — the 199/200 canonical series (2026-07-18 → 08-10)

| Stage | Result |
|---|---|
| **199A** (07-18 → 07-21) | Off-lock direct `IpcCall`/`IpcReply` (NR 6/NR 7): reservation seams, generation-bearing CNode resolver, AckLease; AArch64 round-trip seal (199A2C1); exact-commit 3-arch matrix (199A2C3 `0065f397`); x86 SMP cross-CPU request/reply live (199A2D2*, including the **AP `EFER.NXE` fix** 199A2D2C2B3 `ccceb03d`); freeze seal 199A2D3 `b73a6b35`. **Proof-gated / default OFF.** |
| **200A–200C** (07-21 → 07-27) | Reply/timeout/peer-death terminal ownership model (hosted), generation-bearing deadline token store (hosted), reply-receive deadline completion; x86 live functional seal + off-lock retirement (200C2A/B); AArch64 (200C2C1B fixes blocked-recv timeout delivery) and RISC-V (200C2C2C-R2A/B) ports; **three-arch reply-timeout matrix `72a4ebff`** (6 cells). |
| **200D** (07-27 → 07-29) | Server-death mechanism (hosted → deferred post-lock completion); `ExitCurrentTask` NR 16 ABI, x86 cell `0b5e98f3`, AArch64 cell, **RISC-V cell never earned**; ServerDies transition counters; x86 ServerDies live attempts 2B1D2–D5 refused five times before the post-drain restore-owner revalidation (2B1D5A/B). |
| **199D** (07-29 → 08-10) | ServerDies reverse-link accounting scoped to one transaction — the `IPC_SERVER_DEATH_LINK_LEAK created=54 detached=1` "leak" was one counter pair answering two questions, not a leak. **x86 ServerDies live cell** `f5669cb5` (sixth attempt). **x86 NR6/NR7 production default ON** `0b5ec254` (`IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL`, 53 NR6 + 41 NR7 calls off-lock). Then **199D-WA1-GATE (`029d3562`, 08-05) disabled the default again** pending waiter ownership; WA2A generation-bearing waiter primitive; WA2B wake-owner census; WA3A transition barriers; WA3B one-shot spawn reservation; WA3C1 (`10b91677`) generation-bearing waiter record, central removal, two production defects fixed (displaced waiter's ack lease never released; `destroy_endpoint` raw clear leaked the lease and stranded the receiver); **WA3C2** authoritative endpoint-waiter ownership and the x86 production default restored. |

Canonical outcome (§0.3): **199C CLOSED** (via U6-FRAME, blocking-send envelope preserved,
x86 + RISC-V live; AArch64 pre-admission deferral recorded), **199E CLOSED** (off-lock timeout
pipeline production-live and default-reachable on all three arches; `MAX_DEADLINE_TOKENS = 4`
[VC] remains a scaling debt), **199D OPEN** (ServerDies cell 1 of 3), **199F OPEN**
(notifications: only a read helper existed), **199G OPEN**. [VH]

## 5. Phase E — the U-series: from 49 acquisitions to zero (2026-08-11 → 09-24)

| Directive | Commit(s) | Effect on the census |
|---|---|---|
| U1 | `26364834` | deleted 2 obsolete acquisitions (49 → 47) |
| U2 | `7bb73d85` | relocated 3 test-only acquisitions (47 → 44) |
| U3 | `b7d4dcf8` … `0912a279` (24 commits) | retired the post-lock **drain re-acquisition class** (44 → 7); exit condition met; canonical 203C still OPEN |
| U4 / U4-CLOSE | `4f74f55b`, `c817ab66` | cross-arch queue-advancing dispatch for blocking IpcRecv/IpcSend |
| U6 | `b83ae3a8` … `b59b7225` | off-lock blocking-send commit (partial cohort); U6-FRAME later closed 199C |
| U7 | `8275c927` | production IPC-timeout pipeline on all three arches |
| U9-A … U9-F | 08-24 → 08-25 | inventory; "STOP — the four-site cohort is two fences" (U9-B); reply-cap materialization as an ordered transaction (U9-C); capability-revoke teardown split per class (U9-F); D3 fence discharged (census 6 → 2) |
| U9-QA, -TM, -PF, -FT1..4, -RX1..4, -COW2, -MO2, -ASPACE1 | 08-26 → 09-02 | off-lock queue-advance prerequisite, timer checkpoint, PageFault classification and terminal fault routes, receive writeback, reply-cap writeback repair, MemoryObject reclaim |
| U9-SPAWN-VM1/IC1, -FORK1, -A64-COW2, -REAP1, -EXIT1..4, -RESIDUAL1, -YIELD2 | 09-03 → 09-07 | spawn provisioner with exact rollback; rank-local endpoint+cap provisioning; one COW clone owner; reap and exit transactions (EXIT1's retirement claim corrected by EXIT2; EXIT2's post-clear settlement found unsafe and repaired with a token in EXIT3; EXIT4 removed the remaining races; RESIDUAL1 corrected EXIT4's admission proof) |
| U9-DISPATCH-CPU1..3 | `26c1c442`, `8265461b`, `a556bbde` | removed ambient CPU authority from queue advance; CPU2 **removed a broad-lock regression CPU1 had introduced**; CPU3 restored the missing `Running` mark |
| U9-TIMER1..5, -TIMER-FINAL | 09-08 → 09-21 | preempting and idle-boundary timer broad dispatch retired; quantum separated from hardware interval (TIMER1 §3); TIMER1 §4b repaired a RISC-V resume defect the conversion introduced |
| U9-VM-ENTRY1 (-R, -L, -S), -XFER1/2 | 09-09 | VmMap/VmAnonMap/VmBrk terminal populations closed; rollback contract; transaction-owned backing lifetime; shootdown excludes the real requester; NR 4 / NR 30 |
| U9-IPC-RESIDUAL1..3, -SEND-FINAL, -RECV-FINAL, -RECV-QUEUE1, -RECV-BLOCK1/2, -FUTEX-WAIT-FINAL | 09-10 → 09-15 | NR 6/7 closed at the type level (fall-through inexpressible), NR 1, NR 2/5, NR 9 |
| U9-PAGEFAULT1..3, -IRQ-UNKNOWN1, -IRQ-FINAL | 09-21 → 09-22 | every PageFault exit authenticated through the entering frame; IRQ claim once at the entry boundary |
| U9-TERMINAL-FINAL §5 | `0dce4a7f` 09-24 | **deleted both terminal broad acquisitions** on a measured closure (a live `TERMINAL_BROAD_DISPATCH_ENTER nr=8` arrival was closed first) |
| U9-TERMINAL-SETTLEMENT, U9-CLOSURE-ACCEPTANCE | → `93900ae0` | declined-Yield frame authority; switch-plan leak; relocated oracle (a regression found and repaired) |
| **U9 CLOSED** | `93900ae0` | `with_cpu` 0, `with_broad` 0; "No further unlock package follows from this programme." |
| POST-U9-STABILIZATION | `14b61286` 09-24 | the four failures carried out of U9 settled; **reference tree for hardware bring-up — preserved by every later brief**. |

## 6. Phase F — QEMU SMP evidence (2026-09-25 → 10-08)

QEMU-BASELINE1 (mapping runs maximal; page-table backing), QEMU-IRQ1/2/3 (UART0→PLIC on RISC-V,
PL011→GICv2 on AArch64, COM1→IOAPIC→LAPIC on x86, all live), QEMU-CONTEXT1 (user execution state
witness), QEMU-SMP1 (FP homes authenticated at the SMP boundary; `ap-cross-cpu-reply` repaired),
QEMU-SMP2 (AArch64 AP pinned dispatch and reschedule SGI), QEMU-SMP3 (RISC-V secondary dispatch,
SBI IPI, remote fence), each with -ACCEPTANCE and -SEAL rounds that **closed grader false
passes**; QEMU-LOCK1/2/3 (contention on `vm_state_lock` with IPI work deferred through the masked
section on all three ports). [VH, `doc/KERNEL_UNLOCKING.md` QEMU sections]

## 7. Phase G — backlog, deletion, production SMP (2026-10-09 → 10-21)

* **BL1–BL6 backlog** (10-09 → 10-12): BL1 multi-page shared-region frames, BL2 user-origin x86
  #GP and RISC-V illegal instruction are the task's fault (previously a kernel halt), BL3a/b/c
  futex/oracle/ack-store edges (BL3b: park only while the word still equals `expected`), BL4a/b
  RISC-V idle tick dispatch and quantum ≠ hardware deadline, BL5a–d checker scoping and
  artifact binding and printk record loss, BL6 witness repairs. Closing ledger `a91632ba`.
* **SHARED-REGION-OFFSET1 / -LIFETIME1 / -LIFETIME2** (`96114d69`, `89273030`, `0d25d699`):
  grant offsets, per-mapping lifetime references, invalidation-gated settlement.
* **BROAD-LOCK-RETIREMENT1** (`a75b1601`, 10-13): physical deletion (above). Owner constraints:
  no renamed wrapper, no other global mutex, no unrestricted closure over `&mut KernelState`,
  stable addresses preserved.
* **VM-INVALIDATION1** (`a72c69a7`): a VM change is complete only when its invalidation is; every
  shootdown names its executing CPU; loaded-root teardown gate and permanent quarantine.
* **AP-SCHED1–4** (`e9c8173c`, `7a76f789`, `8cb30dd1`, `df5ee7b6`): production user scheduling
  on secondary CPUs — RISC-V first (1 then all harts), AArch64 (GICv2, ceiling 8 CPUs,
  `PRODUCTION_CPU_CEILING = 8` [VC]), x86_64 (APIC-map admission [VC]; ≤ 8 CPUs per the AP-SCHED4 record [SR]; authenticated executing CPU;
  lossless parked server-death handoff). **This is production SMP**, not a proof knob.
* **VM-INVALIDATION2 (+ACCEPTANCE, +SEAL)** (`da591ee8`, `9e4d3c49`, `63537414`): x86
  dispatch-gap invalidation hole — the per-page unmap shootdown must target root residency, not
  placement; then two rounds of grader false-pass repairs.

## 8. Phase H — scheduling contract, lock split, kernel threads (2026-10-21 → 11-04)

| Stage | From → delivered | Essence |
|---|---|---|
| SCHED-CONTRACT1 | `63537414` → `e41b3be9` | `doc/SCHEDULING_CONTRACT.md`: source-derived findings (one unfair TTAS rank-1 lock, non-preemptive kernel, wakes never preempt, time = interrupt count, kernels used FP/SIMD), RT-P1 profile, SC-* requirements, model + mutation checks. No production change. |
| SCHED-CONTRACT1-CORRECTION | → `04b94fc0` | RT-P1 debt conserved; exhausted threads hold no queue position. |
| SCHED-OVERFLOW1 | → `91030225` | debt overflow becomes a latched eligibility refusal. |
| KERNEL-NOFP1 | → `fec063af` | kernel code uses no FP/SIMD; proven by disassembly of final images against an allowlist. |
| KERNEL-NOFP1-ACCEPTANCE | → `9b6c889b` | checker prefix and exclusion false passes closed. |
| BUILD-GATES1 | → `adf5fa63` | shared tool resolution, artifact provenance; every boot bound to its exact artifacts (refuses mismatched features/hashes). |
| **SCHED-LOCK1** | → `b7f33257` | rank-1 scheduler lock decomposed into **per-CPU local ownership** with a remote-wake protocol; QEMU independence witness. |
| SCHED-LOCK1-ACCEPTANCE / -SEAL | → `035d0f56` / `0e21b2a6` | witness identity, grader validity vs coverage, execution bound to the installation timeline. **SC-LOCK-2 PARTIAL**: lock order excludes the deadlock cycles, but no progress argument is established. |
| SMP3-EVIDENCE1 | → `298c591b` | RISC-V shootdown decision inputs recorded; IPIs attributed by mailbox protocol. |
| **KTHREAD1** | → `40132e1a` | cooperative kernel threads on three ports: bounded pool (32 × 16 KiB [VC]), entry ABI, scheduling points, kernel IRQ return, wait owner, residency-based reclamation. |
| KTHREAD1-ACCEPTANCE | → `a7da04f1` | wakes never refused; device IRQs on kernel threads on three ports; RISC-V D2 empty-current settlement (F1); lifecycle grader false passes closed; one-CPU coexistence and timeout delivery. |
| **KTHREAD-CLOSURE1** | `a7da04f1` → **`b81e0317`** | five closures, below. |

### KTHREAD-CLOSURE1 — the five closures (verified in `doc/KERNEL_UNLOCKING.md` closure table and `doc/SCHEDULING_CONTRACT.md` §18.10.4 at `b81e0317`) [VH]

| # | Closure | Stage / commit | Remaining limit |
|---|---|---|---|
| 1 | KT1 replay permitted overlapping execution on one CPU | A, `136e8ae2` | evidence depends on the default-off execution stamp |
| 2 | single-CPU timeout grading skipped invalid sequences | A | — |
| 3 | AArch64 SMP2 verifier race (`sgi_unexplained_arrival`, 4/16 boots) — GICv2 order was legal; both verifiers repaired (per-pair rank rule), plus witness handshakes and void TLB rounds | A | a void round is self-reported, so it can only lose coverage |
| 4 | no hardware stack protection for kernel threads | C, `fc429b00` | no stack-depth bound; the image bound assumes compiled-code frame behaviour; QEMU only |
| **5** | **plain non-masking `SpinLock` held across a scheduling point was not detected** (SC-KT-8) | **B, `a540aef2`** | a `mem::forget` guard keeps its thread ineligible for good (fail-closed); underflow and cross-stack latches exercised hosted only |

Stage D (`b81e0317`) re-qualified all five together (one predeclared run, every step passed:
40/40 AArch64 SMP2 boots, KT1 at 1/2/4/8 CPUs on three ISAs, 9/9 overflow probes, device
interrupts on kernel threads ×3, 6275 hosted tests, 41/41 integration binaries). [VH, `qd1` log]

**Stage C details worth remembering:** each kernel-thread stack runs at the bottom of its own
16 MiB stride of a kernel-only window (x86 PML4[510], AArch64 L1[64], RISC-V a kernel-thread
root installed around each switch); fault paths never push onto the faulting stack (x86 IST
#PF/#DF, AArch64 synchronous entry always on a per-CPU emergency stack — the bit-14 entry test is
a reporting aid, not the defence — RISC-V `tp` low-bound check before any store); an image
checker bounds every single stack-pointer decrement. Qualification run 1 found that the RISC-V
kernel-thread root lacked root slot 255 (the device window), which silently broke device
interrupts on kernel threads; fixed at `bb451a3b` with a live control. [VH]

---

## 9. Where unlocking stands, honestly

| Item | State | Evidence |
|---|---|---|
| Production broad `SpinLock<KernelState>` | **deleted** | [VC] |
| Canonical stages complete per owner table | 199C, 199E, 204A (table not re-classified since) | [VH] |
| Direct NR6/NR7 off-lock IPC | **production default on x86_64, AArch64, RISC-V** (`ipccall_direct_production_enabled()`, `src/kernel/boot/mod.rs:4154`) | [VC] |
| D6-genuine authoritative dispatch | compile-time **x86_64 only** (`d6_genuine_enabled()`, `mod.rs:1134`) | [VC] |
| Production SMP user scheduling | all three ports (AP-SCHED1–4); AArch64 ceiling 8 [VC]; x86 ≤ 8 [SR] | [VH, VC, SR] |
| Scheduler lock | per-CPU 1L (SCHED-LOCK1); SC-LOCK-2 progress argument **not established** | [VH] |
| Kernel threads | cooperative, qualified, **no production user** (only tests and default-off witnesses create them) | [VC] |
| Asynchronous kernel preemption (SC-KT-4) | **not enabled** | [VH] |
| Notification wait/signal off-lock (199F) | **not delivered as a canonical stage** [INF: U9 removed the lock from every path, but 199F's parity scope — wait-with-timeout, multi-signal accumulation — was never qualified as a package] | [VH/INF] |
| ServerDies live cells | x86 only (1 of 3) | [VH] |
| `ExitCurrentTask` RISC-V live cell | never earned | [VH] |
| Deadline store | 4 slots (`MAX_DEADLINE_TOKENS`) | [VC] |
| 205B fault-injection matrix, 205C torture harness, 205D cross-arch full-unlock seal | **do not exist** | [VH] |
| Parallel `cargo test --lib` | 58–71 shared-state assertion failures; hosted suite must run `--test-threads=1` | [VH] |

### The 35 canonical stages: historical status vs. current relevance

The owner's stage ladder (`doc/KERNEL_UNLOCKING.md` §0.3) was written while the broad
`SpinLock<KernelState>` still existed (baseline `757993b`); most definitions say "off the broad
lock" plus a functional scope. The lock has since been deleted (U9 `93900ae0`, BLR1 `a75b1601`).
Two different questions therefore apply to every stage, and they must not be merged:

* **Historical completion status** — what the owner's table records. Unchanged here: 199C, 199E
  and 204A complete; everything else OPEN or partial. Nothing below re-classifies a stage; by the
  owner's rule only a review against the full definition can.
* **Current architectural relevance** — what remains meaningful *after* the lock is gone. This is
  an **[INF] assessment** from source at `b81e0317` and the delivered records, to guide planning.

Relevance classes: **L** = lock premise now satisfied structurally (no broad lock exists to take
[VC]) — the remaining scope is functional parity / qualification; **T** = transactional work
largely delivered by later directives (U9-*, VMI*, AP-SCHED*) under other names — owed is a
formal re-qualification against the definition, not new design; **S** = a clause of the definition
is superseded by later architecture; **D** = core action already done, closure review owed;
**R** = still fully relevant, work outstanding; **E** = evidence/seal stage, the evidence package
does not exist yet.

| Stage | Historical status (§0.3) | Relevance | Why |
|---|---|---|---|
| 199C blocking send | complete | current | AArch64 pre-admission deferral recorded |
| 199D call/reply + crash cleanup | OPEN | L + R | off-lock now; ServerDies live cells AArch64/RISC-V never earned; timeout/cancellation cleanup parity unqualified |
| 199E timeouts | complete | current (scaling debt) | `MAX_DEADLINE_TOKENS = 4` [VC]; per-arch timebases |
| 199F notifications | OPEN | L + R | no lock exists on the IRQ path [VC]; wait-with-timeout, multi-signal accumulation, exactly-once wake never qualified as a package |
| 199G IPC seal | OPEN | L + E | "zero runtime broad-lock acquisitions" now holds structurally [VC]; the seal itself was never produced |
| 200A CNode slot seams | OPEN (narrow) | L + T | rank-local provisioning with exact rollback (U9-SPAWN-IC1), cap teardown split (U9-F) |
| 200B copy/mint/move/release | OPEN (partial) | L + T + R | `M2_SEAM_HELPER_ONLY` fences still present (16 refs) [VC] |
| 200C object lifetime | OPEN (partial) | L + T | reply-cap transaction (U9-C), shared-region lifetime (SRL1/2), MemoryObject reclaim (U9-MO2) |
| 200D cap seal (failure injection) | OPEN | E | needs the 205B-style injection matrix |
| 201A–201E VM / ZC / page fault / COW | OPEN | L + T | U9-VM-ENTRY1 (-R/-L/-S), U9-XFER1/2, U9-PAGEFAULT1–3, U9-FORK1, U9-A64-COW2, U9-ASPACE1, VMI1/VMI2 |
| 201F TLB seal | OPEN (x86 mechanism) | T + S | x86 IPI + ACK, RISC-V SBI remote fence (QEMU-SMP3), AArch64 broadcast TLBI [VC]; the "wake-only APs" rationale clause is **superseded** — APs run user tasks since AP-SCHED1–4 |
| 201G VM seal | OPEN | E | |
| 202A thread creation | OPEN | L + R | explicit entry-ABI contract still unwritten as a stage deliverable |
| 202B–202E spawn / fork / exit / reap | OPEN | L + T + R | U9-SPAWN-VM1/IC1, U9-FORK1, U9-EXIT1–4, U9-REAP1 delivered the transactions; RISC-V `ExitCurrentTask` live cell never earned |
| 202F lifecycle seal | OPEN | E | |
| 203A timer / 203B IRQ hot paths | OPEN | L (+ R for RT) | "no broad lock from timer/IRQ context" holds structurally; calibration and bounded latency belong to RT-SCHED1 |
| 203C scheduler core | OPEN (partial) | R | `d6_genuine_enabled()` x86-only [VC]; scheduler lock now per-CPU (SCHED-LOCK1); SC-LOCK-2 partial |
| 203D cross-CPU work | OPEN (x86, knob-gated) | T + S | cross-CPU wake/IPI/shootdown now production on three ports (AP-SCHED1–4, QEMU-SMP*, SMP3-EVIDENCE1); the "AArch64/RISC-V may remain BSP-only" provision is **superseded** |
| 204A census | complete | current | guard now a four-kind census with build probe (BLR1) |
| 204B decompose `KernelState` | OPEN (partial) | R | still one struct through one cell, with per-field authority (`FIELD_AUTHORITY`) |
| 204C remove fallback-to-global | OPEN | S (+ review) | re-acquiring the monolithic lock is impossible now; residual intent — unsupported cases settle explicitly (`settle_unowned_trap` exists [VC]) — needs a review, not new design |
| 204D remove retirement scaffolding | OPEN | R | scaffolding still in tree: `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE` (39 refs), `GLOBAL_LOCK_RETIRE_CLASS_DONE` one-shots (32), `M2_SEAM_HELPER_ONLY` (16) [VC] |
| 204E delete runtime `SpinLock` | OPEN ("status not re-classified") | D | BLR1 deleted it and guards reintroduction [VC]; the formal closure review is the only thing owed |
| 205A syscall matrix | OPEN (drafted) | R | "locks acquired" column now means domain locks; address-space-restore column unpopulated |
| 205B fault injection, 205C torture, 205D cross-arch seal | OPEN | R + E | no harness; `KERNEL_FULL_UNLOCK_SEAL` / `KERNEL_RUNTIME_GLOBAL_LOCK_CALLS` markers do not exist [VC] |

Practical reading: the **unfinished** work that still matters is functional parity (199D cells,
199F, 202A entry ABI, RISC-V exit cell), scaffolding removal (204D, 200B fences), structural
decomposition (204B), the x86-only authoritative scheduler seam and lock fairness (203C,
SC-LOCK-2), and the evidence stages (199G, 200D, 201G, 202F, 205A–D). Work that was
**superseded** is limited to specific clauses (BSP-only/wake-only AP provisions in 201F/203D, the
re-acquisition clause of 204C) — never whole stages. 204E is **done in substance, unreviewed**.

### Remaining structural barriers

1. Canonical re-classification of 199D/199F/199G, 200A–D, 201A–G, 202A–F, 203A–D, 204B–E, 205A–D
   against the post-BLR1 tree — owed, never done; must be done stage by stage against the owner
   definitions, not inferred from the census.
2. D6-genuine on AArch64/RISC-V (203C) — authoritative scheduler seam is x86-only.
3. Fair kernel lock / contention bound and a progress argument (SC-LOCK-2) — prerequisite for
   any RT latency claim and for RT-SCHED1.
4. Preemptible kernel threads (SC-KT-4) and a stack-depth bound.
5. 205B/205C/205D harnesses and markers.
6. Hardware in the loop — every timing bound in the contract needs real hardware; QEMU timing is
   not hardware timing.

## 10. Roadmap as recorded at `b81e0317` [VH, `doc/KERNEL_UNLOCKING.md` §0.8, contract §18.10.4]

1. **RT-SCHED1** — 32-level fixed-priority preemptive selection, scheduling contexts and budgets,
   calibrated time and earliest-deadline programming, structural no-migration, a fair kernel lock
   or contention bound, buffered diagnostics (SC-PRI-10…13, SC-CTX-*, SC-TIME-3…6, SC-CPU-2,
   SC-KT-12).
2. **PARTITION1** — cyclic schedule, windows, OPERATIONAL transition, system allowance.
3. **FP-POLICY1** — optional per-task FP policy (after KERNEL-NOFP1).
4. Outside the scheduling line, recorded in §0.8 as "not started **by SCHED-CONTRACT1**" (not as
   never started): VFS work and hardware bring-up. Both have earlier history — VFS is implemented: `vfs_server` (mount/fd tables, frozen v1 opcodes) is live in the core chain; shared-memory VFS I/O exists as both direction helpers plus a RAMFS-only live route through `RecvV3SharedIoMapper` (Stages 63–87, 2026-06-05 → 06-10; `VFS_READ_SHARED_REPLY_ENABLED` and `VFS_WRITE_SHARED_REQUEST_ENABLED` are `true` in `crates/yarm-fs-servers/src/fs/common/shared_io_adapter.rs`), but the default `handle_request` still returns `Unsupported` for the shared opcodes, FAT live mount is disabled and ext4 is read-only; and
   the RPi5 HH-5 high-half initrd/allocator/kernel-entry bridge and BOOT-4 heap landed 2026-06-21/22
   (`1e62310a`, `bbe6b6c7`, `7eb743de`, `ec1a063d`, `d1374f9a`) with no recorded hardware result
   afterwards [UNK]. See `doc/YARM_DEVELOPMENT_HISTORY.md` Era 3.
