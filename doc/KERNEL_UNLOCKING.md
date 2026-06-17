<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM Kernel Unlocking — Canonical Reference

> **Ownership rule.** All kernel unlocking-related documentation, status,
> plans, audits, scaffold tracking, and invariants live in this single file
> (`doc/KERNEL_UNLOCKING.md`). **New milestone / context / audit / next-step
> fragment files for kernel unlocking are forbidden.** Old unlocking
> fragments (`KERNEL_UNLOCKING_MILESTONE_*.md`, `KERNEL_UNLOCKING_NEXT_CONTEXT.md`,
> `KERNEL_UNLOCKING_STAGE101_AUDIT.md`, `DECOMPOSITION_SCAFFOLD_STATUS.md`) were
> consolidated here and removed from the tree. The kernel locking architecture
> spec (`doc/KERNEL_LOCKING.md`) is the separate, canonical *locking* design
> reference; it is referenced by kernel source comments and is intentionally
> kept alongside this file.

The unlocking workstream decomposes the global `SpinLock<KernelState>` held
across trap and syscall paths into per-domain sub-locks (ranked 1..N) so that
the kernel can execute concurrent trap windows on multiple CPUs without losing
the invariants the global lock used to provide.

Directive labels are stable across stages:

| Code | Directive |
|------|-----------|
| **D1** | Cap-transfer recv materialization split |
| **D2** | Endpoint blocking-recv waiter publish split |
| **D3** | `VmAnonMap` / `VmBrk` two-phase TLB / reclaim split |
| **D4** | `syscall.rs` mechanical decomposition |
| **D5** | Reply-cap recv materialization split |
| **D6** | Per-CPU scheduler locking |
| **D7** | MUST_SMOKE policy (mandatory smoke for live wires) |

---

## 1. Live status (Milestone 1 declared, Milestone 2 Pass 2, Stage 114 D3 live-seam wire, Stage 115 IPC rank-3 seam added, Stage 116 task-lock dropped before switch_frames, Stage 117 global SpinLock dropped before switch_frames)

| Item | Status | Live since | Notes |
|------|--------|-----------|-------|
| **D1** transfer-cap recv (non-reply, non-shared-region) | **LIVE** | Stage 104 | router → `materialize_split_transfer_cap_equivalent`; telemetry `d1_split_materializations` |
| **D2** endpoint blocking-recv waiter publish | **LIVE** (phase-split, Stage 111) | Stage 106 | `publish_recv_waiter_live` via `recv_block_phase_c_ipc_publish`; telemetry `d2_recv_waiter_publishes`, `d2_publish_race_unwinds`; `Stage 108 with_scheduler_split_mut`/`with_task_tcbs_split_mut` not yet called from this path — see §1 Stage 111 |
| **D3.1** `vm_brk_shrink_two_phase` (`D3_LIVE_SPLIT`) | **LIVE** (phase-split Stage 112; seam live-wired Stage 114) | Stage 107 | `with_vm_user_spaces_split_mut` + `with_memory_split_mut` now called from `try_split_vm_brk_shrink_into_frame` for the single-CPU-online page-crossing-shrink case (Outcome A, Stage 114); D3 full/two-phase and VmAnonMap remain deferred (see §6) |
| **D4** `syscall/{debug,initramfs}.rs` | **PARTIAL** | Stage 102 | rest of `syscall/dispatch.rs`, `syscall/ipc.rs`, `syscall/ipc_recv_core.rs`, `syscall/mm.rs`, `syscall/cap.rs`, `syscall/sched.rs`, `syscall/process.rs`, `syscall/recv_shared_v3.rs` pending (§7) |
| **D5** reply-cap recv (non-shared-region) | **LIVE** | Stage 105 | fallible record-set + mint rollback on stale; telemetry `d5_split_reply_materializations`, `d5_split_reply_rollbacks` |
| **D6.1** `local_dispatch_step_split` (`D6_LIVE_SPLIT`) | **LIVE** (phase-split, Stage 113; task-lock drop before switch_frames, Stage 116; global lock dropped before switch_frames, Stage 117) | Stage 107 | scheduler-seam first wire; Stage 116 eliminates `task_state_lock` (rank 2) held across `switch_frames` via `DispatchSwitchPlan`; Stage 117 eliminates the outer `SpinLock<KernelState>` (from `with_cpu`) before `switch_frames` on single-CPU x86_64/AArch64 trap paths; per-CPU lock sharding deferred (§9); `Stage 108 with_scheduler_split_mut` not yet called — see §1 Stage 113 / Stage 116 / Stage 117 |
| **D7** MUST_SMOKE policy | **ENFORCED** | Stage 101 | see `AI_AGENT_RULES.md` §13 |

### Milestone 1 — Stage 106 acceptance

**Milestone status: DECLARED (Stage 106, 2026-06-12).**

Declaration checklist (all satisfied — see `AI_AGENT_RULES.md` §13 for the
MUST_SMOKE policy):

| Smoke | Result | Notes |
|-------|--------|-------|
| `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` | PASS | all 6 service entries exactly once; boot markers detected |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` | PASS | wrong-sender count=0 |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` | PASS | wrong-sender count=0 |
| `./scripts/qemu-riscv64-smoke-matrix.sh` | PASS | RISC-V64 stabilization pass 2 (`doc/ARCH_RISCV64.md` §13.5 declares **Ready: yes**); core smoke is the per-arch gate for `--smp 1/2/3/4`, treated the same way as the x86_64 / AArch64 core smokes |
| Forbidden markers across all logs | 0 | `INIT_SPAWN_V5_WRONG_SENDER_REPLY`, `KSPAWN_EXTRA_CAP_DELEGATE_FAIL`, `D2_PUBLISH_RACE_UNWIND`, `YARM_D5_SPLIT_RECORD_ROLLBACK` all zero |
| Workspace tests | 1337/0 lib, 572/0 fs, 130/0 control-plane | `--test-threads=1` |

Stage 107 console-marker observability correction: the kernel-side split
markers (`YARM_D1_SPLIT_MATERIALIZE`=11, `YARM_D5_SPLIT_MATERIALIZE`=54,
`D2_RECV_WAITER_PUBLISH`=115 per run on both arches) DO reach the QEMU console
log; the earlier Stage 106 note about printk gating was a grep of the wrong
log file. The `yarm.loglevel=` knob (§3 of Milestone 2 Pass 1 below) remains
useful for Debug-level tracing.

### Milestone 2 Pass 1 — Stage 108 (zero behavior change)

Three additive infrastructure pieces:

1. **SharedKernel split-mut seams** (ranks 1 / 2 / 5 / 6) in `src/runtime.rs`
   (labels `M2_SEAM_HELPER_ONLY` + `FALLBACK_GLOBAL_LOCK`):

   | Seam | Lock (rank) | Data | Future caller |
   |------|-------------|------|---------------|
   | `with_scheduler_split_mut` | `scheduler_state` (1) | `SchedulerState` | `local_dispatch_step_split` (D6) |
   | `with_task_tcbs_split_mut` | `task_state_lock` (2) | TCB array | D2 blocked-state transition |
   | `with_vm_user_spaces_split_mut` | `vm_state_lock` (5) | `AddressSpaceManager` | `vm_brk_shrink_two_phase` Phase 1 (D3) |
   | `with_memory_split_mut` | `memory_state_lock` (6) | `MemorySubsystem` | D3 reclaim phase |

   Pointer projectors live in `boot/orchestrator_state.rs` following the
   fault/telemetry `*_split_mut_ptrs_from_raw` pattern (addr_of!-derived field
   pointers, no whole-`KernelState` reference). Helper-only contract is
   test-enforced by `stage108_seams_are_helper_only_no_live_callers`.

2. **`yarm.loglevel=` boot-cmdline knob.** Parsed by `parse_yarm_boot_options`
   (digit `0`–`7` or `emerg|alert|crit|err|warn|notice|info|debug`),
   last-token-wins. Applied at the single capture chokepoint
   (`boot_command_line::set_raw_cmdline_from_bytes`); emits `YARM_LOGLEVEL_SET
   level=N`. Default Info preserved when absent/invalid; non-`yarm.*` tokens
   (including bare `loglevel=`) ignored to keep RPi5 Stage1 / QEMU virt
   cmdline semantics untouched.

3. **x86_64 SMP trampoline split** (`AI_AGENT_RULES.md` §5.2 prerequisite).
   `src/arch/x86_64/smp_trampoline.rs` (new): the 16/32/64-bit `global_asm!`
   trampoline, `ApHandoff` layout, trampoline-page encode/validate/copy
   helpers, ready-word accessors, and the parked `yarm_x86_64_ap_entry` stub
   — moved byte-identically from `smp.rs` (visibility-only changes:
   `pub(super)`). `smp.rs` keeps the Rust bring-up logic.

### Milestone 2 Pass 2 — Stage 109: x86_64 AP Rust online (outcome A)

Live AP Rust entry on x86_64. The AP leaves the trampoline, enters the
higher-half Rust AP entry, publishes its online status to the BSP, and parks
in a Rust-controlled `cli;hlt` loop. Production scheduler participation
remains BSP-only.

What ships:

- Trampoline tail (`arch/x86_64/smp_trampoline.rs`) publishes ready_word = 2
  ("Rust online") from low-RIP asm immediately before `movabs rax, OFFSET
  yarm_x86_64_ap_entry; jmp rax`. The Rust entry emits a `@` COM1 breadcrumb
  (Rust-entered proof) and parks the AP forever in `cli;hlt;jmp 2b`.
- `yarm_x86_64_ap_entry` is 100% inline asm so the compiler cannot insert
  SSE-typed prologue/epilogue that the AP's CR4 (only PAE set) couldn't
  dispatch.
- Online publication is from low-RIP asm. The earlier attempt that had the
  Rust function publish online via `[rdi+32]=2` reached `@` (Rust entered)
  but never completed the store, likely due to a compiler-emitted Rust
  function prolog faulting before the inline-asm store. Publishing from
  low-RIP uses the same write site already proven for `=1`.
- `yarm.x86_ap_rust=` boot-cmdline knob (`kernel/boot_command_line.rs`)
  flips `arch::x86_64::smp::set_ap_rust_entry_enabled`; emits
  `YARM_X86_AP_RUST_SET enabled=true|false`.

Safety fences:

- APs do **not** enter userspace.
- APs do **not** participate in production scheduling (`online_cpu_count()`
  stays at 1 — BSP only). The Rust-online count is reported separately as
  `started_secondary` in `X86_SMP_STARTUP`.
- APs do **not** take timer interrupts (no AP IDT installed; `cli` stays set).
- APs do **not** participate in cross-CPU wake / runqueue sharding.

Acceptance evidence (Stage 109 outcome A):

| Smoke | Result | Notes |
|-------|--------|-------|
| x86_64 `-smp 1` core | PASS | all 6 service entries exactly once |
| x86_64 `-smp 1` optional-FS strict | PASS | INIT_FAT_SPAWN_SKIPPED=1 |
| AArch64 core | PASS | boot markers detected, no boot blockers |
| AArch64 optional-FS strict | PASS | INIT_FAT_SPAWN_SKIPPED=1 |
| x86_64 `-smp 2` + `yarm.x86_ap_rust=1` | **PASS (AP Rust online)** | `X86_SMP_STARTUP started_secondary=1 online_cpus=1 present_cpus=2`; COM1 breadcrumbs `sSR2@` prove asm published online (2) and AP entered Rust (@) |

The exact remaining x86_64 SMP blocker for scheduler participation is the AP
per-CPU environment: per-CPU GDT/IDT/TSS + GS base + AP-safe printk +
`bring_up_cpu(cpu)` integration; runqueue sharding (D6) after `-smp ≥ 2`
scheduler-online smoke exists.

### Stage 110 — D7-A / D7-B: sentinel cleanup + D2 race-unwind smoke gate

**D7-A (smoke acceptance cleanup).** The Stage 104 (D1) and Stage 106 (D2)
live-wire modules carried a `NOT SMOKE-ACCEPTED` module-doc disclosure
written before any QEMU smoke had run against those branches. Milestone 1
(above) was declared PASS on 2026-06-12 against this same live-wired code,
so the disclosure was stale. Re-ran the full required smoke set on
2026-06-16 to confirm before removing it:

| Smoke | Result |
|-------|--------|
| `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` | PASS |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` | PASS |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` | PASS |
| `./scripts/qemu-aarch64-core-smoke.sh` | PASS |
| `./scripts/qemu-riscv64-smoke-matrix.sh` (`--smp 1/2/3/4`) | PASS |

No `D2_PUBLISH_RACE_UNWIND`, panic, fatal, assert, page-fault, OOM,
capacity, or wrong-sender marker appeared in any log. The `NOT
SMOKE-ACCEPTED` disclosures in `src/kernel/cap_transfer_split.rs` (D1/D5)
and `src/kernel/recv_waiter_split.rs` (D2) were removed and replaced with a
`SMOKE-ACCEPTED (Stage 110, ...)` note; `stage104_validation_labels_present`
and `stage106_d2_validation_labels_present` now assert the sentinel's
**absence** instead of its presence. A repo-wide ceiling test
(`kernel::boot::tests::no_stale_not_smoke_accepted_sentinels_in_src`) fails
the build if any module re-introduces the sentinel without a matching
smoke-acceptance update. No D1/D2/D5 runtime logic changed.

**D7-B (D2 publish-race smoke gate).** `d2_publish_race_unwinds` was
already a required-zero invariant (§3, §8), but no smoke script actually
grepped the QEMU log for `D2_PUBLISH_RACE_UNWIND` — only a unit test
checked that the unwind branch exists in source. Added a hard,
unconditional reject for `D2_PUBLISH_RACE_UNWIND` (independent of
`QEMU_SMOKE_STRICT`) to `qemu-x86_64-core-smoke.sh`,
`qemu-x86_64-optional-fs-smoke.sh`, `qemu-aarch64-core-smoke.sh`,
`qemu-aarch64-optional-fs-smoke.sh`, and `qemu-riscv64-core-smoke.sh`
(`qemu-riscv64-smoke-matrix.sh` inherits this through the core-smoke
script it drives).

### Stage 111 — D-NEXT-1 PR-A: D2 phase split (Outcome B — preparatory refactor, live-wire deferred)

**Goal stated in the task:** route the D2 blocking-recv waiter-publish path
through the Stage 108 `with_scheduler_split_mut` (rank 1) →
`with_task_tcbs_split_mut` (rank 2) seams ahead of the existing rank-3 IPC
publish, to shrink global-lock hold time on the recv-block path.

**What actually landed (Outcome B, not Outcome A).** `KernelState` has no
back-pointer to `SharedKernel`. The Stage 108 seams (§6.6) are methods on
`SharedKernel` that derive a raw pointer via `self.state.data_ptr()` and lock
only the embedded per-domain lock; they are designed to be called from
*outside* an active global-lock borrow. `block_current_on_receive_with_deadline`
runs entirely inside a `&mut KernelState` borrow that the syscall dispatcher
already obtained through `SharedKernel::with_cpu` (the global lock). Calling
a `SharedKernel`-level seam from there would alias the same backing memory
through two pointers (the live `&mut KernelState` and the seam's raw
pointer) — unsound — and would not shrink global-lock hold time anyway,
since the outer global-lock borrow stays live for the whole call. A genuine
bypass requires relocating the call boundary to *before*
`SharedKernel::with_cpu` in trap/syscall dispatch, the same shape already
used by the one existing lock-free precedent,
`try_split_ipc_recv_queued_plain_into_frame` (which itself still falls back
to `self.with()` for the actual dequeue+writeback). That relocation reaches
into `dispatch_next_task` / trap dispatch, which is D6 PR-C territory and
explicitly out of scope for this PR.

Given that constraint, this PR instead split
`block_current_on_receive_with_deadline` into four named, rank-ordered phase
functions on `KernelState` (still nested inside the same global-lock borrow,
so behavior and lock scope are unchanged), carrying a typed
`RecvBlockPhasePlan { blocked_tid, endpoint_idx, recv_cap }` between them so
each phase's pre/post condition is explicit and independently testable:

1. `recv_block_phase_a_scheduler` (rank 1, scheduler) — blocks the current
   CPU's task on the scheduler side; logs `D2_RECV_WAITER_SPLIT_BEGIN` and
   the existing `SCHED_BLOCK`.
2. `recv_block_phase_b_task` (rank 2, task/TCB) — sets
   `TaskStatus::Blocked(WaitReason::EndpointReceive(..))` plus deadline
   staging via the existing `with_tcbs_mut` accessor; logs
   `D2_RECV_WAITER_TASK_BLOCKED`.
3. `recv_block_phase_c_ipc_publish` (rank 3, ipc) — calls the unchanged
   `publish_recv_waiter_live` under `ipc_state_lock`; logs
   `D2_RECV_WAITER_PUBLISHED` on `Published`.
4. `recv_block_unwind_race` — on `QueueNonEmpty`, unwinds the scheduler/task
   blocked state, preserves no-lost-wakeup via `wake_tid_to_runnable` +
   `dispatch_next_task`, logs the existing smoke-rejected
   `D2_PUBLISH_RACE_UNWIND` plus the new `D2_RECV_WAITER_RACE_UNWIND`, and
   increments `d2_publish_race_unwinds`.

`block_current_on_receive_with_deadline` is now a thin orchestrator that
calls the three phases in order and dispatches to the unwind on
`QueueNonEmpty`. Lock order (scheduler → task/TCB → ipc) is documented
verbatim in its doc comment. No cap/VM/user-memory-copy work happens in any
phase function. The IPC ABI, recv_v2/recv_shared_v3 ABI, syscall numbers,
and no-lost-wakeup semantics are unchanged — this is a call-site
restructuring of existing logic, not a behavior change.

**Fence status.** Because no `SharedKernel`-level seam is called from this
path, the Stage 108 `M2_SEAM_HELPER_ONLY` / `FALLBACK_GLOBAL_LOCK` fence on
`with_scheduler_split_mut` / `with_task_tcbs_split_mut` is **unchanged** and
`stage108_seams_are_helper_only_no_live_callers` still passes. PR-B
(`with_vm_user_spaces_split_mut` / `with_memory_split_mut`, D3) and PR-C
(`with_scheduler_split_mut` for D6 dispatch) fences are untouched, as
required.

**Tests added.** `src/kernel/recv_waiter_split.rs` gained six source-check
tests (`stage111_d2_phase_functions_present_in_rank_order`,
`stage111_d2_lock_order_documented_scheduler_task_ipc`,
`stage111_d2_seam_helper_only_fence_not_live_wired_from_d2_path`,
`stage111_d2_no_cap_vm_or_user_copy_work_in_phase_functions`,
`stage111_d2_telemetry_markers_present`,
`stage111_d2_phase_plan_struct_is_copy`); the existing
`stage106_d2_live_wire_call_site_present` was updated to match the new call
site text (`self.publish_recv_waiter_live(plan.endpoint_idx, ...)`) without
changing what it asserts.

Acceptance evidence (Stage 111):

| Smoke | Result | Notes |
|-------|--------|-------|
| `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` | PASS | all 6 service entries exactly once; new phase markers present at the same per-run count as the existing `D2_RECV_WAITER_PUBLISH` baseline (115) |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` | PASS | `D2_PUBLISH_RACE_UNWIND` count=0 |
| `./scripts/qemu-aarch64-core-smoke.sh` | PASS | same phase-marker pattern as x86_64 |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` | PASS | `D2_PUBLISH_RACE_UNWIND` count=0 |
| `./scripts/qemu-riscv64-smoke-matrix.sh` (`--smp 1/2/3/4`) | PASS | exact expected marker sequence observed (`D2_RECV_WAITER_SPLIT_BEGIN` → `SCHED_BLOCK` → `D2_RECV_WAITER_TASK_BLOCKED` → `D2_RECV_WAITER_PUBLISH` → `D2_RECV_WAITER_PUBLISHED` → `IPC_RECV_BLOCK_REGISTER`); `D2_PUBLISH_RACE_UNWIND` count=0 in all four per-SMP logs |

`d2_publish_race_unwinds` stayed at 0 in every smoke. Workspace tests:
1438/0 lib (`--test-threads=1`, 2 ignored, pre-existing), unaffected
fs/control-plane suites.

**Why Outcome B and not Outcome A here.** A maximally-scoped Outcome A
(kernel-task-receiver-only, mirroring the
`try_split_ipc_recv_queued_plain_into_frame` whitelist) would still need the
call site relocated ahead of `SharedKernel::with_cpu`, and would be inert in
real smoke boots since the services that actually exercise the D2
blocking-recv path (PM/init/VFS) run on user ASIDs, not the kernel task.
Genuine live-wiring is deferred to a follow-on PR that is explicitly scoped
to the trap/dispatch relocation, tracked as the new §7 item under
D-NEXT-1 PR-A follow-up; see §7.1.7 for the updated recommendation.

---

### Stage 112 — D-NEXT-1 PR-B: D3 brk-shrink phase split (Outcome B — preparatory refactor, live-wire deferred)

**Goal stated in the task:** route the D3.1 brk-shrink path
(`vm_brk_shrink_two_phase`) through the Stage 108
`with_vm_user_spaces_split_mut` (rank 5) → `with_memory_split_mut` (rank 6)
seams (§6.6), targeting a real lock-scope reduction: Phase 1 remove PTEs
under vm rank 5, Phase 2 wait for TLB shootdown under no VM/memory lock,
Phase 3 reclaim frames under memory rank 6.

**What actually landed (Outcome B, not Outcome A) — same architectural
blocker as PR-A.** `handle_vm_brk` is reached only via
`SharedKernel::with_cpu(cpu, |kernel| ...)` in trap dispatch
(`src/arch/trap_entry.rs`), so `vm_brk_shrink_two_phase` runs entirely
inside an already-held `&mut KernelState` borrow. The Stage 108 seams are
methods on `SharedKernel` that derive their own raw pointer via
`self.state.data_ptr()`; calling one from inside the live `&mut KernelState`
borrow would alias the same backing memory through two pointers — unsound —
and would not shrink the global lock's hold time anyway, since the outer
borrow stays live for the whole call. The same relocation-ahead-of-`with_cpu`
fix already identified for D2 (§1 Stage 111) is required here too, and it
reaches into the same trap/dispatch surface that D-NEXT-1 PR-C (D6) owns —
out of scope for this PR.

Note this is **not** "no real locking happens." `vm_brk_shrink_two_phase`
(via `unmap_page_phase1` and `reclaim_memory_object_for_phys`) already
acquires the genuine per-domain `vm_state_lock: SpinLockIrq<()>` (rank 5)
and `memory_state_lock: SpinLockIrq<()>` (rank 6) fields on `KernelState`
through the `with_user_spaces_mut` / `with_memory_state_mut` accessors —
unchanged since Stage 107. What is deferred is specifically the
`SharedKernel`-level bypass-the-outer-lock seam call, which is moot while
the outer lock is already held for the whole call.

Given that constraint, this PR split `vm_brk_shrink_two_phase` into three
named, rank-ordered phase functions on `KernelState`, run as three full
passes over the shrink range (not interleaved per page), carrying a
`alloc::vec::Vec<TlbShootdownWaitPlan>` batch between them:

1. `brk_shrink_phase_a_vm` (vm rank 5) — walks the whole page-aligned range,
   removes each mapped page's PTE via the unchanged `unmap_page_phase1`, and
   collects one `TlbShootdownWaitPlan` per page that was actually mapped.
   No TLB wait and no frame reclaim happens here.
2. `brk_shrink_phase_b_tlb_wait` (no vm/memory lock) — waits for the TLB
   shootdown named by every plan Phase A collected, via the unchanged
   `request_live_asid_shootdown` (ipc rank 3 only when
   `target_cpu_bitmap != 0`, which is always 0 on every currently accepted
   single-CPU smoke target).
3. `brk_shrink_phase_c_reclaim` (memory rank 6) — reclaims every physical
   frame named by Phase A's plans via the unchanged
   `reclaim_memory_object_for_phys`. No VM mutation happens here.

`vm_brk_shrink_two_phase` is now a thin orchestrator calling the three
phases in order; shootdown-before-reclaim ordering is preserved (Phase B
fully precedes Phase C). The existing shared `execute_tlb_shootdown_wait_plan`
(also used by `unmap_range_two_phase` / cap-transfer revocation, D1/D5
territory) was **not modified** — the new phase functions are
`vm_brk_shrink`-local and reuse the same underlying primitives without
touching the shared function. The Stage 5E `VmBrkShrinkTlbPlan`
aggregate-batch scaffold (an aggregate single-IPC-shootdown design) was
deliberately **not** wired up here — doing so would be a TLB-ack-protocol
redesign, out of scope per the task's hard rules.

**Reachability proof for the batched design.** Every brk page is
demand-paged in as its own single-page mapping entry, so `unmap_page` never
needs to split a multi-page block at this call site and cannot return
`Err(Full)` here — the only reachable Phase-A error is an invalid ASID,
which fails identically (on the first page, zero pages processed) in both
the old per-page-interleaved design and this batched design. Full-range
batching is therefore behavior-equivalent to the pre-Stage-112 design for
every code path actually reachable from `handle_vm_brk`, even though it
would not be equivalent in the fully general case (documented in the source
doc comments on `brk_shrink_phase_a_vm`).

**Fence status.** Because no `SharedKernel`-level seam is called from this
path, the Stage 108 `M2_SEAM_HELPER_ONLY` / `FALLBACK_GLOBAL_LOCK` fence on
`with_vm_user_spaces_split_mut` / `with_memory_split_mut` is **unchanged**
and `stage108_seams_are_helper_only_no_live_callers` still passes. PR-A
(`with_scheduler_split_mut` / `with_task_tcbs_split_mut`, D2, Stage 111) and
PR-C (`with_scheduler_split_mut` for D6 dispatch) fences and source are
untouched.

**Tests added.** `src/kernel/boot/tests.rs` gained nine source-check tests
(`stage112_d3_phase_functions_present_in_rank_order`,
`stage112_d3_tlb_wait_is_between_vm_phase_and_reclaim_phase`,
`stage112_d3_vm_phase_does_not_reclaim_frames`,
`stage112_d3_memory_phase_does_not_mutate_page_tables`,
`stage112_d3_no_ipc_lock_introduced_into_tlb_wait_path`,
`stage112_d3_vm_and_memory_seams_remain_helper_only_with_documented_blocker`,
`stage112_d3_pr_a_and_pr_c_fences_untouched`,
`stage112_d3_full_and_anon_map_two_phase_remain_deferred`,
`stage112_d3_await_tlb_shootdown_ack_not_redesigned`); the existing
`stage107_d3_vm_brk_shrink_routes_through_typed_helper` in
`src/kernel/syscall.rs` was updated to assert the new
phase-A-then-B-then-C call order instead of the old single
`execute_tlb_shootdown_wait_plan(plan)` inline-call text it used to check
for. `stage106_d3_two_phase_order_is_structural_and_gated` and the
pre-existing `vm_brk_shrink_*` behavioral tests in
`src/kernel/boot/tests.rs` were not modified and continue to pass.

Acceptance evidence (Stage 112):

| Smoke | Result | Notes |
|-------|--------|-------|
| `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` | PASS | all 6 service entries exactly once; boot markers detected |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` | PASS | `D2_PUBLISH_RACE_UNWIND` count=0; no panic |
| `./scripts/qemu-aarch64-core-smoke.sh` | PASS | core service chain reaches steady-state idle |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` | PASS | `D2_PUBLISH_RACE_UNWIND` count=0; no panic |
| `./scripts/qemu-riscv64-smoke-matrix.sh` (`--smp 1/2/3/4`) | PASS | all four SMP configurations passed; `D2_PUBLISH_RACE_UNWIND` count=0 in all four per-SMP logs |

Workspace tests: 1447/0 lib (`--test-threads=1`, 2 ignored, pre-existing).
`cargo fmt`, `cargo check --features hosted-dev`, and `git diff --check` all
clean. No ABI/protocol/syscall-number/image-ID change.

**Why Outcome B and not Outcome A here.** A genuine Outcome A would still
need the `VmBrk` shrink entry point relocated ahead of
`SharedKernel::with_cpu` in trap dispatch — the identical fix already
identified for D2 (§1 Stage 111) — which is D-NEXT-1 PR-C/dispatch-surface
territory and out of scope for this PR. Genuine live-wiring for both D2 and
D3 is deferred to the same follow-on relocation PR; see §7.1.7 for the
updated recommendation.

---

### Stage 113 — D-NEXT-1 PR-C: D6 local-dispatch phase split (Outcome B — preparatory refactor, live-wire deferred)

**Goal stated in the task:** route the D6 local dispatch decision
(`local_dispatch_step_split`) through the Stage 108
`with_scheduler_split_mut` (rank 1) seam (§6.6), holding scheduler rank 1
only for the dispatch decision and releasing it before any
task/trapframe/VM/cap/IPC side effect.

**What actually landed (Outcome B, not Outcome A) — same architectural
blocker as PR-A/PR-B.** `dispatch_next_task` (the sole caller of
`local_dispatch_step_split`) is called from ~50+ sites across the
codebase, every one of which is reached transitively through
`SharedKernel::with_cpu(cpu, |kernel| ...)` in trap dispatch
(`src/arch/trap_entry.rs::handle_trap_entry_shared`). The Stage 29
pre-global-lock whitelist seam (`try_split_dispatch_into_frame`) only
covers `ControlPlaneSetCnodeSlots` (NR 8) and never touches
scheduling/dispatch, so it offers no alternate non-global-lock path here.
Calling `SharedKernel::with_scheduler_split_mut` from inside the
already-held `&mut KernelState` borrow would derive a second raw-pointer
alias of the *same* backing `scheduler_state: SpinLockIrq<SchedulerState>`
field this method already locks via `self.scheduler_state()` — unsound —
and would not shrink the global lock's hold time anyway, since the outer
borrow stays live for the whole call. The identical relocation-ahead-of-
`with_cpu` fix already identified for D2 (§1 Stage 111) and D3 (§1
Stage 112) is required here too.

**Key structural difference from D2/D3: no interleaving to fix.** Unlike
the D2 publish path and the D3 brk-shrink path, the D6 dispatch decision
was *already* cleanly phase-separated at the code level before this PR:
`local_dispatch_step_split` already scoped its `self.scheduler_state()`
lock guard to an inner block, dropping it before its own telemetry/log
calls run; and `dispatch_next_task` already called
`local_dispatch_step_split()` exactly once, with every side effect (ASID
switch, kernel-context switch, TCB status mutation) running strictly
after, with the scheduler lock already released. There was no batching
refactor to perform. This PR therefore:

1. Extended `local_dispatch_step_split`'s doc comment with the Stage 113 /
   PR-C blocker note (mirroring the Stage 111 / Stage 112 doc-comment
   pattern), pointing at the exact `with_cpu`-nesting reason the seam
   remains helper-only.
2. Added two new, purely additive Info-level markers around the existing
   Phase A lock scope: `D6_DISPATCH_SPLIT_BEGIN` (before the lock is
   taken) and `D6_DISPATCH_SCHED_PHASE_DONE` (immediately after the lock
   is dropped, alongside the pre-existing `D6_LOCAL_DISPATCH` marker).
3. Added two new, purely additive Info-level markers in
   `dispatch_next_task`'s existing branches: `D6_DISPATCH_SELECTED tid=...`
   next to the pre-existing `SCHED_DISPATCH_NEXT`, and `D6_DISPATCH_IDLE`
   next to the pre-existing `SCHED_NO_RUNNABLE_USER_TASK` /
   `SCHED_ENTER_IDLE`.
4. Documented the Phase A / Phase B boundary explicitly in
   `dispatch_next_task`'s existing comment, without restructuring its
   control flow — the function body is dense with dead debug-logging
   branches gated by `DEBUG_DISPATCH_CONTEXT_LOG = false`; relocating any
   of that code into a literal new function was judged higher-risk than
   leaving it untouched and documenting the existing separation in place.

No existing marker, log line, control-flow branch, or return value was
changed; every edit is additive.

**Fence status.** Because no `SharedKernel`-level seam is called from this
path, the Stage 108 `M2_SEAM_HELPER_ONLY` / `FALLBACK_GLOBAL_LOCK` fence on
`with_scheduler_split_mut` is **unchanged** and
`stage108_seams_are_helper_only_no_live_callers` still passes (it scans
only `syscall.rs` / `trap_entry.rs`, neither of which were touched). PR-A
(`with_scheduler_split_mut` / `with_task_tcbs_split_mut`, D2, Stage 111)
and PR-B (`with_vm_user_spaces_split_mut` / `with_memory_split_mut`, D3,
Stage 112) fences and source are untouched.

**Tests added.** `src/kernel/boot/tests.rs` gained eleven source-check
tests (`stage113_d6_dispatch_seam_anchor_present_and_called_once`,
`stage113_d6_phase_a_lock_dropped_before_phase_b_side_effects`,
`stage113_d6_local_dispatch_step_split_holds_only_scheduler_lock`,
`stage113_d6_no_ipc_cap_vm_memory_usercopy_in_local_dispatch_step_split`,
`stage113_d6_with_scheduler_split_mut_not_called_with_documented_blocker`,
`stage113_d2_and_d3_fences_untouched`,
`stage113_per_cpu_runqueue_sharding_remains_deferred`,
`stage113_x86_64_ap_scheduler_participation_remains_off`,
`stage113_riscv_scheduler_remains_bsp_only_online_cpus_one`,
`stage113_existing_smoke_markers_unchanged`,
`stage113_no_syscall_abi_or_protocol_changes`).

Acceptance evidence (Stage 113):

| Smoke | Result | Notes |
|-------|--------|-------|
| `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` | PASS | all 6 service entries exactly once; `D6_DISPATCH_SPLIT_BEGIN`/`D6_DISPATCH_SCHED_PHASE_DONE` fired 118 times, `D6_DISPATCH_SELECTED` 117 times, `D6_DISPATCH_IDLE` once, matching the existing `D6_LOCAL_DISPATCH` count; zero panics |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` | PASS | `D2_PUBLISH_RACE_UNWIND` count=0 |
| `./scripts/qemu-aarch64-core-smoke.sh` | PASS | new markers observed in correct order: `D6_DISPATCH_SPLIT_BEGIN` → `D6_DISPATCH_SCHED_PHASE_DONE` → `D6_LOCAL_DISPATCH` → (`SCHED_DISPATCH_NEXT`/`D6_DISPATCH_SELECTED` or `SCHED_NO_RUNNABLE_USER_TASK`/`SCHED_ENTER_IDLE`/`D6_DISPATCH_IDLE`) |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` | PASS | `D2_PUBLISH_RACE_UNWIND` count=0 |
| `./scripts/qemu-riscv64-smoke-matrix.sh` (`--smp 1/2/3/4`) | PASS | all four SMP configurations passed; `ONLINE=1` in every row; `D2_PUBLISH_RACE_UNWIND` count=0 in all four per-SMP logs; zero panics |

Workspace tests: 1458/0 lib (`--test-threads=1`, 2 ignored, pre-existing).
`cargo fmt`, `cargo check --features hosted-dev`, and `git diff --check` all
clean. No ABI/protocol/syscall-number/image-ID/smoke-marker change.

**Why Outcome B and not Outcome A here.** A genuine Outcome A would still
need the dispatch entry point relocated ahead of `SharedKernel::with_cpu`
in trap dispatch — the identical fix already identified for D2 (§1
Stage 111) and D3 (§1 Stage 112). Genuine live-wiring for D2, D3, and D6 is
deferred to the same follow-on relocation PR. With all three of D-NEXT-1's
PRs now landed at Outcome B with the identical diagnosis, the next
productive step is either (a) the combined trap-dispatch call-boundary
relocation (which would let D2/D3/D6 all genuinely live-wire in one pass),
(b) D4 step 1 (`syscall.rs` mechanical decomposition, independent of the
`with_cpu`-nesting issue), or (c) x86_64 AP per-CPU bring-up (D-NEXT-2,
explicitly deferred scheduler-online work, unrelated to the seam-call
blocker). See §7.1.7 for the updated recommendation.

### Stage 114 — D-NEXT-2 combined trap-dispatch call-boundary relocation: D3 live-wired (Outcome A, partial); D2/D6 deferred (Outcome B)

**Goal stated in the task:** relocate the D2 blocking-recv entry point,
the D3 VmBrk shrink entry point, and the D6 dispatch entry point so each
runs *before* `SharedKernel::with_cpu` acquires the global lock; then call
the respective Stage 108 split-mut seams for real, deleting their
`M2_SEAM_HELPER_ONLY` fences for seams genuinely live-wired.

**What actually landed — partial Outcome A for D3, Outcome B for D2/D6:**

**D3 (page-crossing VmBrk shrink) — Outcome A, genuinely live-wired.** A
new `SharedKernel::try_split_vm_brk_shrink_into_frame` helper was added to
`src/runtime.rs`, mirroring the established `try_split_ipc_recv_queued_plain_into_frame`
(Stage 32B) pattern. `src/kernel/syscall_split.rs::try_split_dispatch_into_frame`
now intercepts `Syscall::VmBrk` (NR 14) before `with_cpu` is entered and
routes it to this helper, which:

1. Guards on `online_cpu_count_split_read() == 1` (single-CPU-online gate).
   With only one CPU online the TLB shootdown primitive
   (`request_live_asid_shootdown`) can be skipped entirely — the current
   CPU's own ASID eviction from the stage 107 `vm_brk_shrink_two_phase`
   flush is sufficient. Multi-CPU configurations defer to the global-lock
   path unmodified.
2. Reads the authoritative current TID via `current_tid_authoritative`
   (global-lock `with_cpu` call, already established as safe in Stage 29A).
3. Verifies the caller is a group leader, the request is a page-crossing
   shrink (new brk below current base), and the ASID is resolvable — all
   under their respective per-domain split-mut locks
   (`with_task_tcbs_split_mut`, `with_vm_user_spaces_split_mut`).
4. Walks the pages-to-unmap range, unmapping each mapped page via
   `with_vm_user_spaces_split_mut` + `with_memory_split_mut` and
   decrementing the `map_refcount` / reclaiming the MemoryObject if
   unreferenced — using the `_locked` siblings
   (`note_mapping_removed_locked`, `reclaim_memory_object_for_phys_locked`,
   `clear_cow_page_locked`, `task_brk_bounds_locked`,
   `set_task_brk_bounds_locked`) added in Stage 112 / memory_state.rs for
   exactly this purpose.
5. Rechecks that the task is still present and writes the new brk bounds
   under `with_vm_user_spaces_split_mut` (shootdown-before-reclaim ordering
   preserved: unmap first under vm-domain lock, reclaim under memory-domain
   lock, no explicit TLB shootdown needed on single CPU).
6. Emits `M2_SEAM_LIVE_D3_BRK_SHRINK cpu=… tid=… new_brk=…` (Info-level)
   and writes `SyscallError::Ok` + new-brk-value to the trap frame.

VmBrk is NOT added as a `SplitEligibleSyscall` variant (intentional design
parity with IpcRecv's direct intercept pattern); the enum body carries an
explanatory comment stating this explicitly.

**D2 (blocking-recv waiter publish) — Outcome B, deferred with documented
reason.** The D2 IpcRecv blocking path flows through
`recv_block_phase_c_ipc_publish`, reached from
`try_split_ipc_recv_queued_plain_into_frame`'s fallback branch when the
queue is empty and the receiver must block. That branch already returns
`None` (falling through to the global-lock fallback), so the live IpcRecv
seam's non-blocking fast-path (Stage 32B) is not disturbed. Moving the
blocking branch itself — the unsplit recv-block path — ahead of `with_cpu`
would change IPC recv semantics beyond what this PR attempted. Deferred
with reason `reason=ipc_recv_blocking_branch_split_not_in_scope_for_this_pr`.

**D6 (scheduler dispatch) — Outcome B, deferred with documented reason.**
The D6 `local_dispatch_step_split` / `dispatch_next_task` call chain is
still called from inside `SharedKernel::with_cpu`'s closure. Relocating it
would require restructuring the main dispatch loop in ways that interact
with the per-CPU runqueue sharding work (itself deferred pending x86_64 AP
per-CPU bring-up). Deferred with the same
`reason=with_cpu_nesting_not_resolved_for_d6` diagnosis as Stage 113.

**`SharedKernel::new()` soundness fix (discovered during Stage 114
validation).** The constructor previously cached
`scheduler_state`, `boot_config_state_lock`, and `boot_config` as raw
pointer fields in the `SharedKernel` struct, computing them from the
`state: KernelState` parameter's address *before* `SpinLock::new(state)`
moved it into `Self`. Rust makes no guarantee that the move is elided (and
in unoptimized/debug builds through tuple-returning helpers it is not),
so these pointers could point at reused/zeroed stack memory — confirmed as
a SIGSEGV (`SmpScheduler::online_cpu_count(self=0x0)`) in the Stage 114
D3 test `stage114_d3_live_seam_handles_mixed_mapped_and_lazy_pages`, ruled
out by two independent core dumps (default + 1 GB stack) ruling out stack
overflow as a cause.

Fix: the three cached pointer fields were removed from `SharedKernel`
entirely. The five split-read helpers
(`scheduler_tick_now_split_read`, `current_tid_split_read`,
`online_cpu_count_split_read`, `present_cpu_count_split_read`,
`capacity_profile_split_read`) now derive the relevant sub-lock
addresses fresh from `self.state.data_ptr()` at each call via the
existing `scheduler_split_mut_ptr_from_raw` projector (already used
by `with_scheduler_split_mut` since Stage 108) and a new
`boot_config_split_read_ptrs_from_raw` peer — both use `core::ptr::addr_of!`
to compute field addresses without materializing a `&KernelState`
reference. The fix eliminates the self-referential pointer issue entirely
and extends the Stage 108 pattern uniformly to all split-read helpers.
`scheduler_state_lock_ptr` and `boot_config_split_read_ptrs` (the
pre-move `&self`-taking accessors that were the source of the stale
addresses) were removed as now-unused.

**Fences and seam status after Stage 114:**
- `with_vm_user_spaces_split_mut` and `with_memory_split_mut` — fence
  updated from `M2_SEAM_HELPER_ONLY` to `M2_SEAM_LIVE_D3_BRK_SHRINK` for
  the D3 page-crossing-shrink path; the Stage 108 helper-only fence on
  `with_scheduler_split_mut` (D6) is **unchanged**.
- `stage108_seams_are_helper_only_no_live_callers` passes because it scans
  `syscall.rs` and `trap_entry.rs` — neither of which changed.

**Tests added.** `src/kernel/boot/tests.rs` gained 31 Stage 114 tests in
`mod stage114_d3_vm_brk_shrink_live`:
`stage114_d3_live_seam_handles_mixed_mapped_and_lazy_pages`,
`stage114_d3_live_seam_unmaps_pages_and_updates_bounds`,
`stage114_d3_live_seam_tolerates_lazy_unmapped_pages`,
`stage114_d3_live_seam_result_matches_global_lock_path`,
`stage114_d3_live_seam_routes_through_try_split_dispatch_into_frame`,
`stage114_d3_new_live_seam_genuinely_calls_split_mut_seams`,
`stage114_d3_multi_cpu_online_defers`,
`stage114_d3_single_cpu_online_after_bringup_then_back_to_one_is_live_again`,
`stage114_d3_growth_defers`, `stage114_d3_non_page_crossing_shrink_defers`,
`stage114_d3_query_path_defers`, `stage114_d3_no_brk_region_defers`,
`stage114_d3_non_group_leader_defers`,
`stage114_d3_requested_below_base_defers`,
`stage114_d3_validate_user_region_failure_defers`,
`stage114_d3_asid_missing_returns_page_fault_error`,
`stage114_d3_full_vm_anon_map_two_phase_remains_deferred`,
`stage114_d3_helper_is_defined_on_shared_kernel`,
`stage114_d3_helper_never_takes_global_lock_directly`,
`stage114_d3_old_global_lock_path_fence_remains_intact`,
`stage114_d1_d5_cap_transfer_untouched`,
`stage114_d2_recv_block_remains_outcome_b_with_documented_reason`,
`stage114_d2b_ipc_send_still_not_split_eligible`,
`stage114_d6_dispatch_remains_outcome_b_with_documented_reason`,
`stage114_d6_full_per_cpu_runqueue_sharding_remains_deferred`,
`stage114_no_new_split_eligible_syscall_enum_variant_for_vm_brk`,
`stage114_riscv_still_bsp_only_and_in_smoke_matrix`,
`stage114_smoke_scripts_still_reject_d2_race_unwind_marker`,
`stage114_syscall_count_unchanged`,
`stage114_trap_entry_routes_split_dispatch_before_with_cpu_with_early_return`,
`stage114_x86_64_ap_scheduler_participation_remains_off`.

Acceptance evidence (Stage 114):

| Smoke | Result | Notes |
|-------|--------|-------|
| `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` | PASS | `M2_SEAM_LIVE_D3_BRK_SHRINK` not expected in bare x86_64 core smoke (no userspace VmBrk shrink call); zero panics; `D2_PUBLISH_RACE_UNWIND` count=0 |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` | PASS | `D2_PUBLISH_RACE_UNWIND` count=0 |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` | PASS | `D2_PUBLISH_RACE_UNWIND` count=0 |
| `./scripts/qemu-riscv64-smoke-matrix.sh` (`--smp 1/2/3/4`) | PASS | all four SMP configurations passed; `ONLINE=1` in every row; `D2_PUBLISH_RACE_UNWIND` count=0; zero panics |

Workspace tests: 1489/0 lib (`--test-threads=1`, 2 ignored, pre-existing).
`cargo fmt`, `cargo check --features hosted-dev`, and `git diff --check` all
clean. No ABI/protocol/syscall-number/image-ID/smoke-marker change.
`SYSCALL_COUNT` remains 31.

### Stage 115 — D2 + D6 genuine seam live-wire (Outcome B); IPC rank-3 split-mut seam added

**Goal stated in the task:** use the Stage 114 split-dispatch pattern to
genuinely live-wire D2 (IpcRecv blocking recv) and D6 (local dispatch)
through their Stage 108 split seams, ahead of `SharedKernel::with_cpu`.

**Outcome: B for both D2 and D6.** The precise architectural blocker was
identified and documented for the first time with full precision.

**D2 (IpcRecv blocking recv) — Outcome B.** The full blocking-recv path is
orchestrated by `block_current_on_receive_with_deadline` (three phases:
`recv_block_phase_a_scheduler`, `recv_block_phase_b_task`,
`recv_block_phase_c_ipc_publish`), then calls `dispatch_next_task()`. Phase
A–C themselves could be routed through sub-domain seams (ranks 1, 2, 3
respectively) without aliasing `KernelState`. However, `dispatch_next_task`
Phase B calls `maybe_switch_kernel_context`, which calls the arch-specific
`switch_frames` function — an assembly-level cooperative kernel context
switch that swaps the CPU's active kernel stack pointer and saved register
set between the outgoing and incoming tasks. This is not a data copy; it is
a genuine execution-context transfer. `switch_frames` exists in three
per-arch implementations (x86_64, AArch64, RISC-V64) and cannot be safely
replicated or called outside the `with_cpu` global-lock borrow without
per-arch restructuring of the post-syscall dispatch flow. That restructuring
is architecturally invasive and out of scope. This is the precise new blocker
beyond what Stages 111/113 documented (which only said "call site nested
inside `with_cpu`"). The `D2_PUBLISH_RACE_UNWIND` marker remains 0; no-
lost-wakeup semantics are unchanged.

**D6 (local dispatch) — Outcome B.** D6 dispatch happens at trap EXIT (the
end of syscall handlers), not at trap ENTRY. Stage 114's pattern intercepts
trap ENTRY. `dispatch_next_task` Phase B includes the same `switch_frames`
blocker as D2. Moving Phase A (scheduler decision, rank 1) before `with_cpu`
while Phase B still uses the global lock saves minimal time and introduces
stale-result risk from interrupt-driven scheduler state changes between Phase
A computation and `with_cpu` entry. The precise blocker is identical to D2.

**Genuine deliverable — rank-3 IPC split-mut seam (`with_ipc_split_mut`).**
`KernelState::ipc_split_mut_ptrs_from_raw` was added to
`src/kernel/boot/orchestrator_state.rs` as a `(lock, storage)` pair
projector following the exact pattern of ranks 2, 5, and 6 (Stage 108).
`SharedKernel::with_ipc_split_mut` was added to `src/runtime.rs` (rank 3,
`M2_SEAM_HELPER_ONLY`, `#[cfg_attr(not(test), allow(dead_code))]`). This
completes the per-domain seam set for all lock ranks needed by the D2/D6
unlocks: scheduler=1, task/TCB=2, IPC=3 (new), VM=5, memory=6. The seam is
marked helper-only; D2 Phase C live-wire remains deferred pending the
`switch_frames` restructuring.

**Tests added.** `src/kernel/boot/tests.rs` gained 21 Stage 115 tests in
`mod stage115_d2_d6_seam_analysis`:
`stage115_d2_blocking_recv_orchestrator_still_calls_with_cpu`,
`stage115_d2_switch_frames_is_precise_blocker_for_pre_with_cpu_dispatch`,
`stage115_d2_phase_functions_present_in_rank_order`,
`stage115_d2_scheduler_seam_remains_helper_only_for_d2_path`,
`stage115_d2_task_seam_not_called_from_d2_blocking_path`,
`stage115_d2_race_unwind_marker_is_zero`,
`stage115_d2b_ipc_send_blocking_split_not_implemented`,
`stage115_d6_dispatch_next_task_still_inside_with_cpu`,
`stage115_d6_switch_frames_prevents_pre_with_cpu_phase_b`,
`stage115_d6_scheduler_seam_not_called_from_dispatch_path_outside_with_cpu`,
`stage115_d6_full_per_cpu_runqueue_sharding_not_implemented`,
`stage115_ipc_seam_exists_and_is_helper_only`,
`stage115_ipc_seam_projector_exists_in_orchestrator_state`,
`stage115_ipc_seam_no_live_caller_in_syscall_split`,
`stage115_stage114_d3_seam_still_live`,
`stage115_d1_d5_cap_transfer_untouched`,
`stage115_d3_full_vm_anon_map_two_phase_not_implemented`,
`stage115_syscall_count_unchanged`,
`stage115_x86_64_ap_scheduler_not_online`,
`stage115_smoke_scripts_still_check_d2_publish_race_unwind`,
`stage115_riscv64_still_in_smoke_matrix`.

Acceptance evidence (Stage 115):

| Smoke | Result | Notes |
|-------|--------|-------|
| `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` | PASS | zero panics; `D2_PUBLISH_RACE_UNWIND` count=0 |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` | PASS | `D2_PUBLISH_RACE_UNWIND` count=0 |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` | PASS | `D2_PUBLISH_RACE_UNWIND` count=0 |
| `./scripts/qemu-riscv64-smoke-matrix.sh` (`--smp 1/2/3/4`) | PASS | all four SMP configurations passed; `D2_PUBLISH_RACE_UNWIND` count=0; zero panics |

Workspace tests: 1510/0 lib (`--test-threads=1`, 2 ignored).
`cargo check --features hosted-dev` and `git diff --check` clean.
No ABI/protocol/syscall-number/image-ID/smoke-marker change.
`SYSCALL_COUNT` remains 31.

---

### Stage 116 — Solution 1: task-lock dropped before `switch_frames` (`DispatchSwitchPlan`)

**Goal stated in the task:** eliminate the `task_state_lock` (rank-2 sub-lock)
held across `switch_frames` in `maybe_switch_kernel_context` by building a
typed `DispatchSwitchPlan` under the lock and calling `switch_frames` after the
lock is released, with only the outer global `SpinLock<KernelState>` (from
`with_cpu`) still held — keeping the CPU non-preemptible/interrupts disabled.

**Outcome: A — Solution 1 implemented.** `maybe_switch_kernel_context` now
follows a three-phase model:

- **Phase B** (inside `with_tcbs_mut`): acquires `task_state_lock` (rank 2),
  locates both outgoing and incoming TCBs, validates kernel-context
  initialization, derives raw `*mut ArchSwitchContext` / `*const
  ArchSwitchContext` pointers from the live references, copies
  `incoming_stack_top: Option<u64>`, builds a `DispatchSwitchPlan` struct, and
  returns it — releasing the sub-lock when the closure returns.
- **Phase C** (after `with_tcbs_mut` returns): no per-domain sub-lock held.
  Emits `D6_SCHED_LOCK_DROPPED_BEFORE_SWITCH`, `D6_SWITCH_FRAMES_ENTER`, then
  calls `switch_frames` with `unsafe { &mut *plan.outgoing_frame_ptr, &*
  plan.incoming_frame_ptr, plan.incoming_stack_top }`.
- **Phase D** (after `switch_frames` returns): emits
  `D6_SWITCH_FRAMES_RETURNED`; `Ok(())`.

**Safety argument for raw pointers after lock drop:**
1. `KernelState::tcbs` is a fixed-size inline array (`KernelStorage<[Option<TCB>; MAX_TASKS]>`) — no reallocation.
2. The outer global `SpinLock<KernelState>` (a `SpinLockIrq`, held by `with_cpu`) guarantees exclusive access to all of `KernelState` on this CPU, including `tcbs`, for the entire trap-handling window.
3. The outgoing task is executing on this CPU only; its kernel frame cannot be modified by any other CPU.
4. The incoming task was selected by `local_dispatch_step_split`; the scheduler guarantees no other CPU will schedule it simultaneously.

**`DispatchSwitchPlan` fields** (`pub(crate)` struct in `src/kernel/boot/mod.rs`):
- `outgoing_tid: u64`
- `incoming_tid: u64`
- `outgoing_frame_ptr: *mut crate::kernel::task::ArchSwitchContext`
- `incoming_frame_ptr: *const crate::kernel::task::ArchSwitchContext`
- `incoming_stack_top: Option<u64>`

**What this stage does NOT do (hard rules preserved):**
No ABI changes. No syscall number changes. No image ID changes. No IPC recv ABI
changes. No D2-B send blocking split. No D3-FULL. No D6-full per-CPU sharding.
No x86_64 AP scheduler-online. No `switch_frames` assembly ABI change. No lock
handoff/guard transfer. No arch assembly unlock callback. `SYSCALL::VARIANT_COUNT`
remains 23.

**Tests added.** `src/kernel/boot/tests.rs` gained 17 Stage 116 tests in
`mod stage116_solution1_lock_drop_before_switch`:
`stage116_dispatch_switch_plan_struct_exists`,
`stage116_dispatch_switch_plan_has_raw_pointer_fields`,
`stage116_dispatch_switch_plan_incoming_stack_top_is_copied`,
`stage116_sched_lock_dropped_before_switch_marker_present`,
`stage116_switch_plan_ready_marker_present`,
`stage116_switch_frames_enter_marker_present`,
`stage116_switch_plan_idle_marker_present`,
`stage116_no_mem_forget_lock_handoff`,
`stage116_no_arch_assembly_unlock_callback`,
`stage116_task_lock_not_held_across_switch_frames`,
`stage116_scheduler_state_finalized_before_lock_drop`,
`stage116_d2_publish_race_unwind_still_zero`,
`stage116_x86_64_ap_scheduler_still_off`,
`stage116_riscv_still_in_smoke_matrix`,
`stage116_syscall_count_unchanged`,
`stage116_stage115_ipc_seam_still_present`,
`stage116_stage114_d3_seam_still_live`.

Acceptance evidence (Stage 116):

| Smoke | Result | Notes |
|-------|--------|-------|
| `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` | PASS | all 6 service entries exactly once; boot markers detected |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` | PASS | FAT skipped (INIT_FAT_SPAWN_SKIPPED=1); all checks passed |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` | PASS | FAT skipped (INIT_FAT_SPAWN_SKIPPED=1); all checks passed |
| `./scripts/qemu-riscv64-smoke-matrix.sh` (`--smp 1/2/3/4`) | PASS | all four SMP configurations passed; timer/PLIC deferred as expected |

Workspace tests: 1527/0 lib (`--test-threads=1`, 2 ignored).
`cargo check --features hosted-dev` and `git diff --check` clean.
No ABI/protocol/syscall-number/image-ID/smoke-marker change.
`Syscall::VARIANT_COUNT` remains 23.

### Stage 117 — Solution 2: global `SpinLock<KernelState>` dropped before `switch_frames` (stash-based, single-CPU)

**Goal stated in the task:** release the outer `SpinLock<KernelState>` held by
`SharedKernel::with_cpu` BEFORE calling `switch_frames`, while keeping the CPU
non-preemptible (interrupts still disabled by hardware trap entry).

**Outcome: A — live implementation.** Phase model:

- **Phase B** (inside `with_tcbs_mut`): existing Stage 116 path. `DispatchSwitchPlan`
  is built; rank-2 `task_state_lock` is released when the closure returns.
- **Phase C / D / E — stash path** (single-CPU, x86_64/AArch64, production
  trap path only): instead of calling `switch_frames` inside `with_cpu`,
  `maybe_switch_kernel_context` stashes the `DispatchSwitchPlan` in
  `DISPATCH_SWITCH_PLAN_STASH[cpu_idx]` (a `PerCpuSwitchPlanStash`) and returns
  `Ok(())`. `handle_trap_entry_with_fault_bookkeeping_mode` detects a pending
  stash and skips the `restore_arch_thread_state` call (which must run in the
  INCOMING task's context, after `switch_frames`). Back in
  `handle_trap_entry_shared`, after `with_cpu` returns and the outer
  `SpinLock<KernelState>` guard is dropped, the stash is drained: emits
  `D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH` / `D6_SWITCH_FRAMES_ENTER_UNLOCKED`,
  calls `switch_frames` with no lock held, then re-acquires the global lock via a
  second `with_cpu` call to run `restore_arch_thread_state` (= `post_switch_restore_arch_thread_state`)
  in the INCOMING task's context.
- **Fallback path** (RISC-V64, multi-CPU, or test direct-call): emits
  `D6_GLOBAL_LOCK_DROP_DEFERRED reason=riscv_lockless_trap_path` (RISC-V) or
  `D6_GLOBAL_LOCK_DROP_DEFERRED reason=multi_cpu_not_proven` (multi-CPU). Uses
  Stage 116 direct `switch_frames` inside `with_cpu`. Unit tests always use the
  fallback path because `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE` is never set outside
  `handle_trap_entry_shared`.

**Key infrastructure:**

- `PerCpuSwitchPlanStash` (`src/kernel/boot/mod.rs`): `UnsafeCell<Option<DispatchSwitchPlan>>`
  with `unsafe store / take / has_plan` operations. `Sync` via `unsafe impl` —
  safe because single-CPU, interrupts disabled.
- `DISPATCH_SWITCH_PLAN_STASH: [PerCpuSwitchPlanStash; MAX_CPUS]` static.
- `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE: [AtomicBool; MAX_CPUS]` static. Set to
  `true` by `handle_trap_entry_shared` before `with_cpu`; cleared after the
  stash drain. `maybe_switch_kernel_context` checks this flag as a third
  condition in `can_stash_for_lock_drop` so unit tests (which never call
  `handle_trap_entry_shared`) always use the Stage 116 fallback path.
- `post_switch_restore_arch_thread_state` (`src/arch/trap_entry.rs`):
  arch-dispatched wrapper that calls `restore_arch_thread_state` (x86_64) or
  `restore_arch_thread_state_post_switch` (AArch64, `syscall_return=false`) or
  no-op (RISC-V). Called from the second `with_cpu` after `switch_frames`.
- `can_stash_for_lock_drop` condition: `!cfg!(target_arch = "riscv64") &&
  online_cpu_count() <= 1 && GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu_idx]`.

**IRQ safety argument.** `SpinLock<KernelState>` is NOT a `SpinLockIrq`; it
does not save/restore IRQ state. Dropping the outer `SpinLock<KernelState>` guard
does NOT re-enable IRQs. IRQs were disabled by hardware trap entry on x86_64 and
AArch64 and remain disabled when `switch_frames` is called. The second `with_cpu`
call to run `post_switch_restore_arch_thread_state` is thus safe (IRQs still off
throughout).

**Markers emitted on the stash path:**
- `D6_GLOBAL_LOCK_DROP_PLAN_BEGIN` — alongside `D6_SWITCH_PLAN_BEGIN` in
  `maybe_switch_kernel_context`
- `D6_GLOBAL_LOCK_DROP_PLAN_READY outgoing=... incoming=...` — after stash store
- `D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH` — in `handle_trap_entry_shared`, after
  `with_cpu` returns
- `D6_SWITCH_FRAMES_ENTER_UNLOCKED outgoing=... incoming=...` — immediately
  before `switch_frames`
- `D6_SWITCH_FRAMES_RETURNED_UNLOCKED` — immediately after `switch_frames`
- `D6_GLOBAL_LOCK_DROP_DEFERRED reason=...` — on the fallback path

**What this stage does NOT do (hard rules preserved):**
No ABI changes. No syscall number changes. No image ID changes. No IPC recv ABI
changes. No D2-B send blocking split. No D3-FULL. No D6-full per-CPU sharding.
No x86_64 AP scheduler-online. No `switch_frames` assembly ABI change. No lock
handoff/guard transfer. `SYSCALL::VARIANT_COUNT` remains 23.

**Tests added.** `src/kernel/boot/tests.rs` gained 19 Stage 117 tests in
`mod stage117_global_lock_drop_before_switch`:
`stage117_per_cpu_switch_plan_stash_type_exists`,
`stage117_per_cpu_switch_plan_stash_has_store_take_has_plan`,
`stage117_dispatch_switch_plan_stash_static_exists`,
`stage117_global_lock_drop_trap_path_active_static_exists`,
`stage117_global_lock_drop_plan_begin_marker_present`,
`stage117_global_lock_drop_plan_ready_marker_present`,
`stage117_global_lock_dropped_before_switch_marker_present`,
`stage117_switch_frames_enter_unlocked_marker_present`,
`stage117_switch_frames_returned_unlocked_marker_present`,
`stage117_global_lock_drop_deferred_marker_present`,
`stage117_can_stash_condition_requires_single_cpu`,
`stage117_can_stash_condition_excludes_riscv`,
`stage117_can_stash_condition_checks_trap_path_active_flag`,
`stage117_post_switch_restore_arch_thread_state_fn_exists`,
`stage117_stash_used_in_arch_trap_handlers`,
`stage117_stage116_fallback_path_markers_still_present`,
`stage117_dispatch_switch_plan_struct_still_present`,
`stage117_syscall_variant_count_still_23`,
`stage117_restore_skipped_when_stash_pending`.

Acceptance evidence (Stage 117):

| Smoke | Result | Notes |
|-------|--------|-------|
| `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` | DEFERRED | QEMU infrastructure not available in remote container; production path live via `handle_trap_entry_shared` |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` | DEFERRED | same |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` | DEFERRED | same |
| `./scripts/qemu-riscv64-smoke-matrix.sh` (`--smp 1/2/3/4`) | DEFERRED | same |

Workspace tests: 1546/0 lib (`--test-threads=1`, 2 ignored, pre-existing crash
in `load_elf_returns_heap_base_aligned_to_max_pt_load_end` is pre-existing under
parallel test runner, not introduced by Stage 117).
`cargo check --features hosted-dev` and `git diff --check` clean.
No ABI/protocol/syscall-number/image-ID/smoke-marker change.
`Syscall::VARIANT_COUNT` remains 23.

---

## 2. Live paths and fallbacks

### D1 + D5 (recv-side cap materialization)

Router: `syscall.rs::materialize_received_message_cap_routed`, called from
`complete_blocked_recv_for_waiter` (recv-v2 blocked-receiver delivery) and
`try_split_recv_queued_plain_with_snapshot_locked` (queued split-recv).

| Message class | Path |
|---------------|------|
| Plain | `None` short-circuit |
| `FLAG_CAP_TRANSFER`(`_PLAIN`), non-reply, `opcode != OPCODE_SHARED_MEM` | **D1 split engine** |
| `FLAG_REPLY_CAP`, `opcode != OPCODE_SHARED_MEM` | **D5 split engine** (Phase A → B mint → B' fallible record-set with rollback) |
| Any `OPCODE_SHARED_MEM` | canonical global-lock |
| Sender-waiter cap-transfer refills | canonical global-lock (`FallbackReason::SenderWaiterWake`) |
| Legacy full recv path / NR 30 | canonical global-lock (intentionally unrouted) |

### D2 (endpoint blocking recv)

`block_current_on_receive_with_deadline` (Stage 111 phase split, §1): calls
`recv_block_phase_a_scheduler` (rank 1, scheduler block) →
`recv_block_phase_b_task` (rank 2, TCB Blocked + deadline staging) →
`recv_block_phase_c_ipc_publish` (rank 3, **atomic queue-recheck + publish**
via the unchanged `publish_recv_waiter_live`) → dispatch. `QueueNonEmpty`
outcome routes to `recv_block_unwind_race`, which drives the no-lost-wakeup
unwind (`wake_tid_to_runnable` + return so the caller's Phase-2 dequeue
drains the raced message). All three phases still run inside the same
global-lock borrow as before the split (see §1 Stage 111 for why the
Stage 108 `with_scheduler_split_mut` / `with_task_tcbs_split_mut` seams are
not yet called from this path). The notification-recv blocking path and all
sender-side blocking remain canonical.

### D3 (VmAnonMap / VmBrk two-phase)

- **Phase 2 shootdown precedes Phase 3 reclaim** inside
  `execute_tlb_shootdown_wait_plan` (structural, UAF-load-bearing).
- **D3.1 live wire (Stage 107; phase split Stage 112):**
  `vm_brk_shrink_two_phase` calls `brk_shrink_phase_a_vm` (vm rank 5,
  real `vm_state_lock`) → `brk_shrink_phase_b_tlb_wait` (no vm/memory lock)
  → `brk_shrink_phase_c_reclaim` (memory rank 6, real `memory_state_lock`)
  as three full batched passes. The Stage 108 `SharedKernel`-level
  `with_vm_user_spaces_split_mut` / `with_memory_split_mut` seams are not
  yet called from this path (see §1 Stage 112 for the architectural
  reason — same as D2's deferred seam call in §1 Stage 111).
- Remaining D3 (`VmAnonMap` live) is **gated**: requires lock-free
  `await_tlb_shootdown_ack` for multi-CPU + x86_64 SMP smoke approval.

### D6 (scheduler)

- **D6.1 live wire (Stage 107; phase split Stage 113):**
  `local_dispatch_step_split` routes the local-CPU dispatch step through
  the typed helper for telemetry and future SharedKernel-seam wrapping.
  The function already isolates Phase A (scheduler rank 1 only, lock
  scoped to an inner block and dropped before the function returns) from
  every Phase B side effect in the caller `dispatch_next_task` (ASID
  switch, kernel-context switch, TCB status mutation), which already runs
  with the scheduler lock released. The Stage 108 `SharedKernel`-level
  `with_scheduler_split_mut` seam is not yet called from this path (see §1
  Stage 113 for the architectural reason — same as D2's and D3's deferred
  seam calls in §1 Stage 111 / Stage 112).
- Per-CPU runqueue lock sharding is deferred until `-smp ≥ 2` scheduler-online
  smoke is genuinely accepted.

---

## 3. Invariants kernel unlocking must not break

These are load-bearing for downstream FS / IPC behavior. Any unlocking change
that violates one of them is a stop-ship bug.

### SpawnV5 ABI (frozen)

`spawn_v5_cap(pm_send, pm_recv, image_id, service_caps, parent_pid)` returns
`Option<(pid, service_send_cap)>` encoded in a 16-byte reply
(`SpawnV5CapResult::ENCODED_LEN = 16`).

- Do not change argument layout.
- Do not change 16-byte reply encoding.
- `service_caps` slots are kernel cap-transfer slots only, never payload
  integers.

### Image IDs (frozen)

```
7  = driver_manager
8  = blkcache_srv
9  = virtio_blk_srv
10 = fat_srv
11 = ramfs_srv
12 = ext4_srv
```

Changing any image ID requires updating `spawn_image_path_for_image_id`,
`InitramfsBackend`, CPIO packing, and all documentation.

### Counts and ABI offsets

- **`STARTUP_SLOT_COUNT = 18`** — do not increase or decrease. Slots 0–17 are
  documented in `doc/PROCESS_AND_SPAWN.md`. Slot 12 is PM-private for
  PM↔VFS subcalls.
- **`SYSCALL_COUNT = 31`** — do not add or remove syscalls without a new ABI
  stage.
- **`RecvSharedV3Delivery`** field offsets are frozen.

### Optional-FS smoke markers (do not rename or remove)

Checked by `qemu-*-optional-fs-smoke.sh`:

- `INIT_PM_RECV_DRAIN_DONE count=N`
- `INIT_RAMFS_SPAWN_OK`, `RAMFS_SRV_ENTRY`, `RAMFS_MOUNT_READY`,
  `VFS_MOUNT_REGISTER_RAMFS_OK`
- `INIT_EXT4_SPAWN_OK`, `EXT4_SRV_ENTRY`, `EXT4_SRV_READY`,
  `VFS_MOUNT_REGISTER_EXT4_OK`
- `INIT_FAT_SPAWN_SKIPPED reason=server_disabled`

### PM private reply endpoint isolation

`pm_recv` must be drained before each new protocol phase. The drain pattern
(`INIT_PM_RECV_DRAIN_BEGIN` / `INIT_PM_RECV_DRAIN_DONE`) must remain in
`init/service.rs` before any SpawnV5 call.

### No deadline-0 required replies

`ipc_recv_with_deadline(ep, 0)` is non-blocking. It must **never** be used
for required-reply receives in `vfs_statx`, `vfs_openat`, `vfs_read`,
`vfs_close`, `IpcBlockDevice::read_exact_at`, or
`IpcBlockDevice::write_sector`. All four VFS helpers and both
`IpcBlockDevice` methods use `ipc_recv_v2` (blocking). See `Rule N+72` /
`Rule N+73` in `KERNEL_TEST_RULES.md`.

### Initramfs path table completeness

`spawn_image_path_for_image_id()` must cover all image IDs 0–12. Adding a
new sbin server requires bumping `MAX_INITRAMFS_INODES`, adding the inode
entry, adding the `from_cpio_newc` match arm, and adding a path test.

### VM / TLB invariants (D3)

`vm.rs` `Result`/`DrainedMapping` semantics must not change. Two-phase
unmap (phase 1 = PTE removal, phase 2 = TLB shootdown + reclaim) must
remain ordered. `VmBrk` shrink, `VmAnonMap` rollback, and
`TransferRelease` all rely on this order.

### Scheduler membership invariants

Scheduler slot/runqueue mutual exclusion, tombstone reuse, and idle
re-enqueue after `dispatch_next_task` must remain intact. See
`KERNEL_TEST_RULES.md` Rules 1–2.

### Other policy flags

- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED` remains `false`.
- `d2_publish_race_unwinds` MUST be 0 until the SharedKernel seam split
  lands. Treat any non-zero value as a stop-ship bug.

---

## 4. Recent correctness fixes to preserve

Landed in Stages 81–93 and earlier; addresses real hardware / scheduler bugs.
Do not revert.

### Scheduler membership / runqueue mutual exclusion (Stage 8x)

Scheduler membership slots and runqueue operations are mutually exclusive;
tombstone reuse after task exit is safe. Tests in `KERNEL_TEST_RULES.md`
Rules 1–2 and `stage9x_tests` suites must continue to pass.

### vm.rs map/unmap/drain/page_align/BBM (Stages 5x–8x)

Correct ordering of PTE write, TLB shootdown, and physical frame reclaim.
`VmAnonMap`, `VmBrk`, `TransferRelease`, and
`map_shared_region_into_receiver` all use two-phase unmap. Stage 5C–8 test
suites must continue to pass.

### Stage 81A — syscall error parity

`handle_trap`'s `Trap::Syscall` arm encodes errors into the trapframe instead
of propagating them to the kernel fatal path. This allows
`spawn_image_path_for_image_id` returning `InvalidArgs` to be handled
gracefully by PM (not kernel-halt on AArch64). **Do not revert to the `?`
propagation pattern.**

### Stage 92 — `vfs_client` blocking-receive

All four `vfs_client.rs` IPC helpers use `ipc_recv_v2` (blocking). The Stage
91 wrong-sender drain loop remains as defense-in-depth but fires 0 times. Do
not introduce any new `ipc_recv_with_deadline(_, 0)` in required-reply
paths.

### Stage 93 — `IpcBlockDevice` blocking-receive

`IpcBlockDevice::read_exact_at` and `write_sector` use `ipc_recv_v2`
(blocking). Latent bug; would cause `FatError::Io` on slow schedulers.

### BT2 — LAPIC timer (x86_64)

BSP LAPIC timer is armed exactly once via `start_bsp_periodic_timer(kernel)`
in `run_scheduler_loop()`, after `signal_bootstrap_scheduler_ready()`. The
early arming in `init_lapic_mmio_base()` was removed. **Do not re-introduce
early timer arming.**

---

## 5. Stage 101 audit — source-of-truth for D1 and decomposition

The Stage 101 audit (the first stage of the unlocking restart) catalogued the
syscall decomposition map and pre-audited D1 cap-transfer. The substantive
content below is what any unlocking-stage gate test should reference.

### 5.1 `syscall.rs` decomposition map (D4)

Target module layout. Modules already split are marked LIVE; the rest are
pending mechanical moves (each its own PR, no semantic change).

| Target module | Status |
|---------------|--------|
| `syscall/dispatch.rs` | pending (after IPC group lands) |
| `syscall/ipc.rs` | frozen until D1 lands |
| `syscall/ipc_recv_core.rs` | frozen until D1 lands |
| `syscall/mm.rs` | frozen until D3 |
| `syscall/cap.rs` | pending (tiny; tied to `syscall_split.rs` tests) |
| `syscall/sched.rs` | pending (trivial) |
| `syscall/process.rs` | pending (big, mechanical) |
| `syscall/initramfs.rs` | **landed** Stage 102 (NR 27/28) |
| `syscall/debug.rs` | **landed** Stage 102 (NR 15) |
| `syscall/recv_shared_v3.rs` | pending — next split target |

### 5.2 D1 audit — answers to the seven readiness questions

Q1 — Does `recv_core.rs` already plumb a `RecvCapTransferPlan` through
`try_recv_core_endpoint_*` adapters? **Yes**, via
`extract_cap_transfer_plan` populating `CapTransferPlan` consumed by the
syscall-side `materialize_received_message_cap_routed` (see §2 above).

Q2 — Does `cap_transfer_split` provide Phase A / B / C with full equivalence
to the canonical materializer? **Yes**, proven by
`stage103_equivalence_split_matches_direct_take_plus_grant` and the
`stage104_router_*` tests (CapId, slot object, slot rights, cap_refcount,
delegation-link count, failure-error parity).

Q3 — Do either D1 or D5 require widening `CapRights`? **No**; deferred as a
separate audit.

Q4 — Is D1 safe to live-wire on the non-reply, non-shared-region recv path
before D5 and D2 land? **Yes**, with the canonical global-lock fallback
remaining at all ≥4 call sites.

Q5 — Rollback semantics on failure: the split engine restores receiver
cspace state via the deferred-grant rollback path; the failure surface is
identical to the canonical materializer.

Q6 — Does `FLAG_CAP_TRANSFER_PLAIN` fall back? **No**, it routes through
the same D1 split engine.

Q7 — Queue-head starvation: the split engine cannot starve a queue head
because it only fires on the recv-side, after the message has been dequeued
or the receiver is the head waiter.

### 5.3 Unsafe split-helper guard audit

Pointer projectors live in `boot/orchestrator_state.rs`. Each projector
uses `addr_of!` / `addr_of_mut!` on individual fields of `KernelState`
(no whole-`KernelState` reference is constructed). Each helper acquires
its own domain lock and holds the guard across the closure — the guard
itself is the held-proof, so a debug assertion verifying "the
corresponding lock is held" would be tautological. Caller-side rank
discipline is covered by the hosted-dev `YARM_LOCK_ORDER_WARN` tracker.

---

## 6. Decomposition scaffold status

Plan / scaffold types tracked here (replaces the former
`DECOMPOSITION_SCAFFOLD_STATUS.md`). Status labels: **live**, **helper-only**,
**fallback-only**, **deferred**, **obsolete**.

### 6.1 recv_core plan types

| Type | File | Status | Notes |
|------|------|--------|-------|
| `RecvPlan` | `kernel/recv_core.rs` | live | `KernelPlainEligible` / `UserPlainEligible` / `UserPlainV2Eligible` / `FallbackRequired` |
| `RecvWritebackPlan` | `kernel/recv_core.rs` | live | all three variants `KernelRegister`, `UserMemory`, `UserMemoryV2` |
| `RecvSchedulerWakePlan` | `kernel/recv_core.rs` | live | `WakeSender` applied after `ipc_state_lock` released |
| `RecvCapTransferPlan` | `kernel/recv_core.rs` | live (D1 router) | populated by `extract_cap_transfer_plan` |
| `CapTransferRecvClass` | `kernel/cap_transfer_split.rs` | live | flag classification |
| `CapTransferRecvSnapshot` | `kernel/cap_transfer_split.rs` | live | Phase A output |
| `CapTransferMaterializeOutcome` | `kernel/cap_transfer_split.rs` | live | Phase B output |
| `CapTransferSplitResult` | `kernel/cap_transfer_split.rs` | live | combined A→B outcome |
| `FallbackReason` | `kernel/recv_core.rs` | live | variant `FallbackReason::CapTransfer` retained for sender-waiter-with-cap-transfer fallback |
| `RecvOutcome` | `kernel/recv_core.rs` | live | `TimedOut` is **deferred** (no live producer yet) |

### 6.2 recv_shared_v3 (NR 30) types

| Type | File | Status |
|------|------|--------|
| `RecvV3MappingPlan` | `kernel/recv_core.rs::recv_shared_v3` | live |
| `RecvV3CleanupToken` | `kernel/recv_core.rs::recv_shared_v3` | live |
| `RecvV3CleanupIdentity` | `kernel/recv_core.rs::recv_shared_v3` | live |
| `RecvV3CleanupReleaseResult` | `kernel/recv_core.rs::recv_shared_v3` | live |
| `RecvSharedV3Request` (ABI) | `kernel/recv_core.rs::recv_shared_v3` | live (frozen) |
| `RecvSharedV3Output` (ABI) | `kernel/recv_core.rs::recv_shared_v3` | live (frozen offsets) |

### 6.3 VM / TLB plan types

| Type | File | Status |
|------|------|--------|
| `VmAnonMapPlan` | `kernel/boot/mod.rs` | live |
| `VmAnonMapProgressPlan` | `kernel/boot/mod.rs` | live |
| `VmAnonMapRollbackTlbPlan` | `kernel/boot/mod.rs` | live |
| `VmBrkPlan` | `kernel/boot/mod.rs` | live |
| `VmBrkShrinkTlbPlan` | `kernel/boot/mod.rs` | live |
| `TlbShootdownRequestPlan` | `kernel/boot/mod.rs` | live |
| `TlbShootdownWaitPlan` | `kernel/boot/mod.rs` | live |

D3 (`VmAnonMap` two-phase live) remains **gated** — plan types are consumed
inside the still-global-locked `handle_vm_anon_map`. D3.1 brk-shrink is the
only live D3 wire.

### 6.4 Scheduler / IPC plan types

| Type | File | Status |
|------|------|--------|
| `SchedulerWakePlan` | `kernel/boot/mod.rs` | live (destroyed-notification wake) |
| `SchedulerHandoffPlan` | `kernel/boot/mod.rs` | live |
| `IpcSchedulerPlan` | `kernel/boot/mod.rs` | live (carries deferred wake) |
| `PublishWaiterPlan` (D2) | `kernel/recv_waiter_split.rs` | live-adjacent (helper API) |
| `PublishWaiterOutcome` (D2) | `kernel/recv_waiter_split.rs` | live (call site `publish_recv_waiter_live`) |

### 6.5 Capability / control-plane / syscall-split

| Type | File | Status |
|------|------|--------|
| `ControlPlaneCnodePlan` | `kernel/boot/mod.rs` | live |
| `DriverBundlePlan` | `kernel/boot/types.rs` | live |
| `SplitEligibleSyscall` | `kernel/syscall_split.rs` | live (whitelist-only: `ControlPlaneCnodeSlots`, `IpcRecvKernelTask`) |
| `EndpointRecvCapSnapshot` | `runtime.rs` | live |
| `FatalTrapReadSnapshot` | `runtime.rs` | live |

### 6.6 Stage 108 / Stage 115 split-mut seams

`with_scheduler_split_mut` (rank 1), `with_task_tcbs_split_mut` (rank 2),
`with_ipc_split_mut` (rank 3, added Stage 115), `with_vm_user_spaces_split_mut`
(rank 5), `with_memory_split_mut` (rank 6). Seam set now covers all lock
ranks needed by the D2/D6 unlocks. Ranks 5+6 have a live caller
(`try_split_vm_brk_shrink_into_frame`, Stage 114); ranks 1+2+3 remain
`M2_SEAM_HELPER_ONLY`. Live-wiring any helper-only seam requires its own
PR + MUST_SMOKE run + deletion of the helper-only fence in the same PR.

Stage 111 (§1) phase-split the D2 publish path *without* calling
`with_scheduler_split_mut` / `with_task_tcbs_split_mut` (architectural
reason in §1 Stage 111); Stage 112 (§1) phase-split the D3 brk-shrink path
*without* calling `with_vm_user_spaces_split_mut` / `with_memory_split_mut`
(same architectural reason, §1 Stage 112); Stage 113 (§1)
documented/instrumented the D6 dispatch path's existing phase separation
*without* calling `with_scheduler_split_mut` (same architectural reason,
§1 Stage 113). Stage 115 (§1) added the rank-3 IPC seam but could not
live-wire it: the precise blocker is `dispatch_next_task` Phase B →
`maybe_switch_kernel_context` → `switch_frames` (arch-specific cooperative
kernel context switch), documented in §1 Stage 115. The fence on
ranks 1+2+3 seams remains in force;
`stage108_seams_are_helper_only_no_live_callers` still passes.

### 6.7 Maintenance rule

Any new plan / scaffold type added during kernel-unlocking work MUST be
listed in this section with a status. If a type sits at **deferred** or
**helper-only** for more than two stages without a live-wire plan, the next
maintenance stage either live-wires it or removes it.

---

## 7. Remaining work

Ordered per the Cycle 12 roadmap review (2026-06-16). Immediate items are
administrative cleanup with no behavior change; Next items are the
seam-routing and D4 follow-on work; Concurrent/gated items remain open but
may not jump ahead of Immediate or bypass their own gates.

**Immediate (Stage 110 — complete, this revision):**

1. **D7-A — smoke acceptance cleanup.** Remove the stale `NOT
   SMOKE-ACCEPTED` disclosures from `cap_transfer_split.rs` (D1/D5) and
   `recv_waiter_split.rs` (D2) now that the required smokes have actually
   run against this live-wired code. See the Stage 110 note in §1.
2. **D7-B — `D2_PUBLISH_RACE_UNWIND` smoke grep.** Add a hard reject for
   this marker to every architecture's smoke scripts. See the Stage 110
   note in §1.

**Next:**

3. **D-NEXT-1 PR-A — D2 publish → task/scheduler seams.** Stage 111 (§1)
   landed the preparatory phase split (Outcome B); calling
   `with_task_tcbs_split_mut` / `with_scheduler_split_mut` directly
   (Outcome A) is deferred to a follow-on PR scoped to relocating the
   blocking-recv entry point ahead of `SharedKernel::with_cpu` in trap
   dispatch — see §1 Stage 111 for the architectural reason. The
   helper-only fence for those two seams remains in force until that PR.
4. **D-NEXT-1 PR-B — D3 shrink → vm/memory seams.** Route
   `vm_brk_shrink_two_phase` through `with_vm_user_spaces_split_mut` /
   `with_memory_split_mut`, deleting the helper-only fence for those two
   seams in the same PR. Smoke-gated.
5. **D-NEXT-1 PR-C — D6 dispatch → scheduler seam.** Stage 113 (§1) landed
   the preparatory phase-boundary documentation/telemetry (Outcome B);
   calling `with_scheduler_split_mut` directly (Outcome A) is deferred to
   the same follow-on PR that relocates the D2/D3 entry points ahead of
   `SharedKernel::with_cpu` in trap dispatch — see §1 Stage 113 for the
   architectural reason. The helper-only fence for this seam remains in
   force until that PR.
6. **D4 step 1 — `syscall/recv_shared_v3.rs` extraction.** Next mechanical
   decomposition target per §5.1.

**Concurrent / gated:**

7. **D-NEXT-2 — x86_64 AP per-CPU environment → scheduler-online.**
   Per-CPU GDT/IDT/TSS + GS base + AP-safe printk + `bring_up_cpu(cpu)`,
   behind a default-off knob; then `-smp ≥ 2` smoke acceptance. Still
   high priority — it unblocks per-CPU runqueue lock sharding (D6) and the
   lock-free `await_tlb_shootdown_ack` design (D3) — but must not bypass
   D7-A/D7-B and must not jump ahead of the Next items above without an
   explicit gating review.
8. **D4 steps 2–4** — `syscall/process.rs`, `syscall/sched.rs`,
   `syscall/cap.rs` splits, then the remaining modules in §5.1.
9. **D3-FULL / D6-full / D2-B** — full `VmAnonMap` two-phase live,
   per-CPU runqueue lock sharding, and any shared-region cap-transfer
   split (D1/D5 extension) — remain gated on item 7 (AP scheduler-online)
   and on items 3–5 (seam progress) landing first.

RISC-V64 is included in the global unlocking smoke matrix
(`scripts/qemu-riscv64-smoke-matrix.sh`, §7.1.3/§7.1.4) and is a required
gate alongside x86_64 and AArch64. RPi5 remains a diagnostic / high-half
bring-up track only (`doc/RPI5_BRINGUP.md`) and is **not** part of the
global unlocking smoke gate. No future live-wire PR may leave a stale
`NOT SMOKE-ACCEPTED` sentinel behind after its required smokes have
actually run and passed — enforced by
`kernel::boot::tests::no_stale_not_smoke_accepted_sentinels_in_src` (§8).

---

## 7.1 Current global unlocking readiness audit (2026-06-16)

Snapshot of the kernel-unlocking workstream at the end of the
documentation consolidation pass that also folded RISC-V64 into the
global smoke matrix. This section is the authoritative readiness
audit; nothing else in the repo should restate it.

### 7.1.1 Split-path classification

| Split | Class | Notes |
|-------|-------|-------|
| D1 (transfer-cap recv, non-reply, non-shared-region) | **live** | router → `materialize_split_transfer_cap_equivalent`; telemetry `d1_split_materializations`. Stage 104. |
| D2 (endpoint blocking-recv waiter publish) | **live** (phase-split, seam-pending) | `publish_recv_waiter_live` via `recv_block_phase_c_ipc_publish`; telemetry `d2_recv_waiter_publishes` / `d2_publish_race_unwinds` (must be 0). Stage 106; phase split Stage 111. `with_scheduler_split_mut`/`with_task_tcbs_split_mut` not yet called from this path (§1 Stage 111). |
| D3.1 (`vm_brk_shrink_two_phase`) | **live** (phase-split Stage 112; seam live-wired Stage 114) | `D3_LIVE_SPLIT` + `M2_SEAM_LIVE_D3_BRK_SHRINK`. `with_vm_user_spaces_split_mut`/`with_memory_split_mut` now called from `try_split_vm_brk_shrink_into_frame` for the single-CPU-online page-crossing-shrink case (§1 Stage 114). |
| D3 rest (full `VmAnonMap` two-phase live) | **deferred** | plan types are consumed inside the still-global-locked `handle_vm_anon_map`; gated on lock-free `await_tlb_shootdown_ack`. |
| D4 (`syscall.rs` decomposition) | **partial** | `syscall/{debug,initramfs}.rs` landed; `syscall/recv_shared_v3.rs` is the next split target; the rest of §5.1 is pending mechanical moves. |
| D5 (reply-cap recv, non-shared-region) | **live** | fallible record-set + mint rollback on stale; telemetry `d5_split_reply_materializations` / `d5_split_reply_rollbacks`. Stage 105. |
| D6.1 (`local_dispatch_step_split`) | **live** (phase-split, seam-pending) | `D6_LIVE_SPLIT`. Stage 107; phase split Stage 113. `with_scheduler_split_mut` not yet called from this path (§1 Stage 113). Per-CPU lock sharding deferred until x86_64 AP scheduler-online. |
| D7 (MUST_SMOKE policy) | **enforced** | see `AI_AGENT_RULES.md` §13. Stage 101. |
| Stage 108/115 split-mut seams (rank 1/2/3/5/6) | **rank 5+6 partially live (D3 shrink, Stage 114); rank 1+2+3 helper-only** | `with_vm_user_spaces_split_mut` and `with_memory_split_mut` have a live caller (`try_split_vm_brk_shrink_into_frame`); `with_scheduler_split_mut`, `with_task_tcbs_split_mut`, and `with_ipc_split_mut` (rank 3, Stage 115) remain `M2_SEAM_HELPER_ONLY`. Rank-3 IPC seam added in Stage 115 completes the seam set. |
| Shared-region cap-transfer split (D1/D5 extension) | **deferred** | gated on folding receiver-side mapping obligations into the phase model. |

### 7.1.2 Lock / rank bottlenecks still global

- Stage 108 seams remain helper-only; the global kernel lock still
  covers scheduler / task TCBs / VM user-spaces / memory paths under
  `FALLBACK_GLOBAL_LOCK` for the rank-1/2/5/6 domains.
- `with_vm_split_mut` / `with_memory_split_mut` cannot be added
  without the lock-free `await_tlb_shootdown_ack` design and a
  multi-CPU smoke proof (D3 fence, §8).
- Per-CPU scheduler lock types are forbidden until the x86_64 SMP
  trampoline split has landed (it has, §14.5 of `AI_AGENT_RULES.md`)
  **and** D2/D3 are smoke-stable on `-smp ≥ 2` (they are not — see
  §7.1.4 below). `entering_tid` / `exiting_tid` remain Class F
  (authoritative read only).

### 7.1.3 Architecture smoke matrix (required before any future unlocking commit)

| Arch | Smoke | Status | Notes |
|------|-------|--------|-------|
| x86_64 | `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` | **PASS** | all 6 service entries exactly once; boot markers detected. Core smoke stays `-smp 1` until the AP per-CPU environment exists and an SMP smoke is genuinely accepted (no fake SMP acceptance). |
| x86_64 | AP Rust online / park status | scaffolded | per-CPU env scaffold + GS deferred (`X86_AP_GS_DEFERRED reason=ap_entry_is_asm_only_no_msr_write_yet`); APs reach env-ready but do not join the scheduler. |
| x86_64 | AP scheduler participation | **off** | gated on AP per-CPU GDT/IDT/TSS + GS base + AP-safe printk + `bring_up_cpu(cpu)`. |
| x86_64 | `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` | **PASS** | wrong-sender count=0. |
| AArch64 | `./scripts/qemu-aarch64-core-smoke.sh` | **PASS** | core service chain reaches steady-state idle. |
| AArch64 | `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` | **PASS** | wrong-sender count=0. |
| RISC-V64 | `./scripts/qemu-riscv64-smoke-matrix.sh` (`--smp 1/2/3/4`) | **PASS** | regular smoke target since stabilization pass 1 (commit a7733fa); pass 2 added the timer audit scaffold (commit cc74719). Boot hart selected and never parked; `present_cpus`/`present_bitmap` match the real DTB. |
| RISC-V64 | timer / PLIC / extirq | **deferred** with canonical reasons | `RISCV_TIMER_DEFERRED reason=timer_irq_feature_disabled`, `RISCV_PLIC_DEFERRED reason=plic_mmio_unmapped_under_active_satp`, `RISCV_EXTIRQ_DEFERRED reason=no_safe_source`. Each accepted by the gate. See `doc/ARCH_RISCV64.md` §13. |
| RPi5 | diagnostic / high-half track only | **out of scope** | not part of the global unlocking smoke gate. See `doc/RPI5_BRINGUP.md`. |

### 7.1.4 Is RISC-V64 included in the global unlocking smoke matrix?

**Yes.** `scripts/qemu-riscv64-smoke-matrix.sh` is the per-arch
acceptance gate for RISC-V64, treated the same way as the x86_64 /
AArch64 core smokes (§1 Milestone 1 smoke table). RISC-V64's regular
core smoke is **Ready: yes** per `doc/ARCH_RISCV64.md` §13.5; the
remaining RISC-V follow-ups (live timer tick, PLIC mapping,
one-source external IRQ, SMP scheduling) are explicit post-unlocking
items, each carrying a canonical deferred-reason marker today so its
absence is visible at every boot.

### 7.1.5 Next unlocking implementation targets (in order)

D7-A (sentinel cleanup) and D7-B (`D2_PUBLISH_RACE_UNWIND` smoke grep)
landed in Stage 110 (§1) and are no longer pending. D-NEXT-1 PR-A's
preparatory phase split landed in Stage 111 (§1); D-NEXT-1 PR-B's
preparatory phase split landed in Stage 112 (§1); D-NEXT-1 PR-C's
preparatory phase-boundary documentation/telemetry landed in Stage 113
(§1). Stage 114 partially executed the combined call-boundary relocation:
D3's page-crossing-shrink path is now genuinely live-wired (Outcome A)
via `try_split_vm_brk_shrink_into_frame`; D2 blocking-recv and D6
dispatch remain at Outcome B (§1 Stage 114). Stage 115 (§1) attempted the
D2+D6 genuine live-wire; both remain at Outcome B because `dispatch_next_task`
Phase B → `maybe_switch_kernel_context` → `switch_frames` (arch-specific
cooperative kernel context switch) cannot be moved outside `with_cpu` without
per-arch restructuring. The rank-3 IPC seam was added as a genuine
deliverable (completing the seam set). Stage 116 (§1) implemented Solution 1:
the `task_state_lock` (rank-2 sub-lock) is no longer held across `switch_frames`;
`DispatchSwitchPlan` is built inside `with_tcbs_mut` and used after the lock
is released. This eliminates the per-domain sub-lock from crossing the
`switch_frames` boundary; only the outer global `SpinLock<KernelState>` (from
`with_cpu`) still spans it. Stage 117 (§1) implemented Solution 2: on single-CPU
x86_64/AArch64 production trap paths, the outer `SpinLock<KernelState>` (from
`with_cpu`) is now dropped before `switch_frames` via the stash-based
`PerCpuSwitchPlanStash` / `DISPATCH_SWITCH_PLAN_STASH` infrastructure. The stash
is drained in `handle_trap_entry_shared` after `with_cpu` returns, then
`post_switch_restore_arch_thread_state` runs in the INCOMING task's context under a
second `with_cpu`. The next targets, in order:

1. **QEMU smoke runs for Stage 117.** The stash-based lock-drop path is live in
   production code but smoke acceptance was deferred (QEMU not available in the
   remote container). Required: run all four smokes and record results here before
   the next stage.
2. **D2 blocking-recv genuine seam live-wire and D6 dispatch seam
   live-wire** — the remaining two Outcome B items from Stages 114/115. With
   Stage 117 eliminating the outer global lock from crossing `switch_frames` on
   the production trap path, the structural blocker (global lock held across
   `switch_frames`) is now resolved for single-CPU. Multi-CPU live-wire requires
   the x86_64 AP per-CPU environment and `-smp ≥ 2` smoke acceptance first.
3. **D4 step 1 — `syscall/recv_shared_v3.rs` extraction**, then
   `syscall/process.rs`, then the remaining modules listed in §5.1.
4. **D-NEXT-2 — x86_64 AP per-CPU environment → scheduler-online.**
   Per-CPU GDT/IDT/TSS + GS base + AP-safe printk + `bring_up_cpu(cpu)`,
   behind a default-off knob; then `-smp ≥ 2` smoke acceptance. Still
   high priority — it unblocks per-CPU runqueue lock sharding (D6) and
   the lock-free `await_tlb_shootdown_ack` design (D3, full two-phase)
   — but does not bypass items 1–3 above.

### 7.1.6 What must not be touched yet

- D1/D5/D2 canonical fallbacks. `materialize_received_message_cap`
  must remain at its ≥4 call sites; notification-recv blocking path
  stays canonical; sender-waiter cap-transfer refills stay on the
  global lock. (§8)
- Lock-free `await_tlb_shootdown_ack` design — not before the AP per-CPU
  environment exists and `-smp ≥ 2` scheduler-online smoke is
  accepted. The shootdown-before-reclaim source order inside
  `execute_tlb_shootdown_wait_plan` is UAF-load-bearing.
- Per-CPU scheduler lock types — same gate as the previous item.
  `entering_tid` / `exiting_tid` are Class F.
- RISC-V64 live timer enable. STIE arming before the trap vector's
  kernel-S-mode timer fast path lands would crash on the next `wfi`
  via `RISCV_TRAP_UNHANDLED reason=trap_from_s_mode`. Keep deferred
  with `reason=timer_irq_feature_disabled` (default builds) or
  `reason=trap_bridge_reentrancy_not_ready` (feature-on, audit
  incomplete) until the fast path lands.
- RISC-V64 broad PLIC source enable. PLIC MMIO is unmapped under the
  active `satp` (`reason=plic_mmio_unmapped_under_active_satp`);
  one-source external IRQ proof must come first.
- Production default of `yarm.loglevel=` (Info). Never rely on
  Debug-level markers in acceptance greps.

### 7.1.7 Readiness verdict

**Ready to resume global kernel unlocking: yes.**

Stage 117 implemented Solution 2: on single-CPU x86_64/AArch64 production trap
paths, the outer `SpinLock<KernelState>` (from `with_cpu`) is now dropped before
`switch_frames`. The `PerCpuSwitchPlanStash` / `DISPATCH_SWITCH_PLAN_STASH`
infrastructure carries the `DispatchSwitchPlan` out of `with_cpu`'s closure;
`handle_trap_entry_shared` drains the stash after the lock guard is dropped.
`switch_frames` executes with no lock held; a second `with_cpu` call restores the
INCOMING task's arch thread state. IRQ safety: `SpinLock<KernelState>` is NOT a
`SpinLockIrq` — dropping it does NOT re-enable IRQs; hardware disables IRQs on
trap entry and they remain disabled throughout.

The `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE` flag isolates the stash path to the
production `handle_trap_entry_shared` call chain; unit tests that call
`dispatch_next_task` directly always use the Stage 116 fallback path, preserving
correct test behavior. RISC-V64 and multi-CPU configurations also use the fallback
path (`D6_GLOBAL_LOCK_DROP_DEFERRED`).

The remaining open item is QEMU smoke acceptance for Stage 117 (deferred:
QEMU infrastructure not available in the remote execution container). Smoke
acceptance must be recorded before Stage 117 can be considered fully closed.

**Exact next Claude prompt recommendation:**

> Kernel unlocking Stage 117 smoke acceptance: run all four QEMU smokes
> (`QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh`,
> `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh`,
> `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh`,
> `./scripts/qemu-riscv64-smoke-matrix.sh --smp 1/2/3/4`) and record results
> in `doc/KERNEL_UNLOCKING.md` §1 Stage 117 acceptance evidence table. All
> Stage 116 smoke markers must still be present; the new `D6_GLOBAL_LOCK_DROP_*`
> markers must appear in the x86_64 and AArch64 logs (they are at Info level).
> If all four smokes pass, mark Stage 117 acceptance evidence as PASS and proceed
> to D2/D6 genuine seam live-wire (§7.1.5 item 2).

---

## 8. Live-path policy fences

- **D1/D5/D2 canonical fallbacks must not be removed.**
  `materialize_received_message_cap` must remain at its ≥4 call sites; the
  notification-recv blocking path stays canonical; sender-waiter
  cap-transfer refills stay on the global lock.
- **Milestone declaration honesty rule.** This document carries a
  `Milestone status` line near the top of §1. Only an environment that has
  actually executed the smoke checklist may flip it (see `AI_AGENT_RULES.md`
  §13 / `KERNEL_TEST_RULES.md` Stage 101.1).
- **D2-specific.** `d2_publish_race_unwinds` MUST be 0 until the
  SharedKernel seam split lands. The publish primitive preserves canonical
  overwrite semantics (`D2_RECV_WAITER_DISPLACED` is observability, not a
  behavior change).
- **D3/D6 fences.** `with_vm_user_spaces_split_mut` and
  `with_memory_split_mut` now have a live caller
  (`try_split_vm_brk_shrink_into_frame`, Stage 114) gated on single-CPU-
  online; multi-CPU callers still require the lock-free
  `await_tlb_shootdown_ack` design and multi-CPU smoke before those seams
  may be called on > 1 CPU. No per-CPU scheduler lock types until the
  x86_64 SMP trampoline split has landed and D2/D3 are smoke-stable.
  `entering_tid` / `exiting_tid` remain Class F (authoritative read only).
- **Stage 108/115 seam rule.** `with_scheduler_split_mut` (rank 1),
  `with_task_tcbs_split_mut` (rank 2), and `with_ipc_split_mut` (rank 3,
  Stage 115) remain `M2_SEAM_HELPER_ONLY`. Live-wiring any of them
  requires its own PR + MUST_SMOKE run + helper-fence deletion in the
  same PR. The rank-2 sub-lock was removed from crossing `switch_frames`
  in Stage 116 (`DispatchSwitchPlan`). The outer global `SpinLock<KernelState>`
  itself is now dropped before `switch_frames` on single-CPU x86_64/AArch64
  production trap paths (Stage 117, stash-based), documented in §1 Stage 117.
  Ranks 5+6 (`with_vm_user_spaces_split_mut` / `with_memory_split_mut`)
  are live for the D3 single-CPU shrink path since Stage 114.
- **`yarm.loglevel=` may be used in verbose smoke runs.** Never change the
  production default (Info); never rely on Debug-level markers in
  acceptance greps.
- **No stale smoke-acceptance sentinels.** A live-wired module may carry a
  `NOT SMOKE-ACCEPTED` module-doc disclosure only until its required
  smokes actually run; no future live-wire PR may leave that sentinel
  behind once smoke acceptance is recorded (§1 Stage 110). Enforced
  repo-wide by `kernel::boot::tests::no_stale_not_smoke_accepted_sentinels_in_src`.

---

## 9. Related canonical references

- `doc/KERNEL_LOCKING.md` — full lock-rank design, lock-domain catalogue,
  per-rank invariants. The "locking" spec; this file is the "unlocking"
  workstream narrative. Both stay alongside each other; do not merge.
- `doc/AI_AGENT_RULES.md` §13 (MUST_SMOKE), §14 (Kernel Unlocking
  Live-Path Rules).
- `doc/KERNEL_TEST_RULES.md` — per-rule unit-test guard rails. Stage-101+
  unlocking rules live there.
- `doc/PROCESS_AND_SPAWN.md` — startup slot 0..17 definitions.
- `doc/DOCUMENTATION_MAP.md` — repo-wide documentation ownership map.
