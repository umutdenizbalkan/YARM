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

## 1. Live status (Milestone 1 declared, Milestone 2 Pass 2, Stage 114 D3 live-seam wire, Stage 115 IPC rank-3 seam added, Stage 116 task-lock dropped before switch_frames, Stage 117 global-lock-drop stash scaffold Outcome B, Stage 118 first-resume handler + production switch-frame init Outcome B, Stage 119 minimal task pair + TSS RSP0 fix Outcome B, Stage 120 controlled x86_64 switch proof harness, Stage 121 first-resume ABI diagnostics, Stage 122 first-instruction proof, Stage 123 no pre-Rust marker call, Stage 124 Rust tail-jump stack-shape fix, Stage 125 Rust entry bridge, Stage 126 kernel switch-stack mapping/backing gate, Stage 127 target-ASID stack mapping retry, Stage 128 active-CR3 shared switch-stack coverage, Stage 129 active-root VmFull on-demand repair)

| Item | Status | Live since | Notes |
|------|--------|-----------|-------|
| **D1** transfer-cap recv (non-reply, non-shared-region) | **LIVE** | Stage 104 | router → `materialize_split_transfer_cap_equivalent`; telemetry `d1_split_materializations` |
| **D2** endpoint blocking-recv waiter publish | **LIVE** (phase-split, Stage 111) | Stage 106 | `publish_recv_waiter_live` via `recv_block_phase_c_ipc_publish`; telemetry `d2_recv_waiter_publishes`, `d2_publish_race_unwinds`; `Stage 108 with_scheduler_split_mut`/`with_task_tcbs_split_mut` not yet called from this path — see §1 Stage 111 |
| **D3.1** `vm_brk_shrink_two_phase` (`D3_LIVE_SPLIT`) | **LIVE** (phase-split Stage 112; seam live-wired Stage 114) | Stage 107 | `with_vm_user_spaces_split_mut` + `with_memory_split_mut` now called from `try_split_vm_brk_shrink_into_frame` for the single-CPU-online page-crossing-shrink case (Outcome A, Stage 114); D3 full/two-phase and VmAnonMap remain deferred (see §6) |
| **D4** `syscall/{debug,initramfs,recv_shared_v3,process,sched,cap,vm,ipc,helpers,ipc_abi,ipc_recv_core}.rs` | **COMPLETE (mechanical) + cap-boundary in progress** | Stage 102 + D4 steps 1–4 + Stage 145/146/149/150/151 + **Stage 152** completeness audit + **Stage 153** seam audit + **Stage 154** cap-boundary scaffold + **Stage 155** recv-v2 codec convergence + **Stage 156** IPC smoke oracle | 11 modules landed; mechanical decomposition complete (Stage 152); Stage 153 proved the IPC/cap seams are order-pinned; Stage 154 created `ipc_recv_core.rs` and migrated the pure recv-v2 meta codec (Option 2); Stage 155 converged all 3 production recv-v2 meta encoders onto that single pure helper (byte-identical); Stage 156 added a QEMU byte-identical delivery smoke oracle (markers + `scripts/qemu-ipc-recv-v2-oracle-smoke.sh`) — QEMU unavailable, so no stateful seam moved; **Stage 157** moved the reply-cap/transfer-cap oracle markers onto the *live* D1/D5 split arms (they were stranded on the canonical fallback that real boots never reach) and added an `extended` oracle mode that hard-requires them — proven by the existing init spawn workload, no new client; **Stage 158** then used the validated oracle (x86_64 extended + AArch64 manual) to re-home the cap-materialization trio (`materialize_received_message_cap_routed`, `materialize_received_message_cap`, `materialize_received_transfer_cap`) into `ipc_recv_core.rs` (re-exported from `syscall.rs`); the queued-split DELIVERY cluster stays pinned in `syscall.rs` (AArch64 did not exercise `IPC_RECV_V2_META_QUEUED_SPLIT_OK`, so no cross-arch proof); see §5.1.2/§5.1.3/§5.1.4/§5.1.5/§5.1.6; `dispatch.rs` not planned (syscall.rs stays dispatch owner); see Stage 148–158 decomposition map |
| **D5** reply-cap recv (non-shared-region) | **LIVE** | Stage 105 | fallible record-set + mint rollback on stale; telemetry `d5_split_reply_materializations`, `d5_split_reply_rollbacks` |
| **D6.1** `local_dispatch_step_split` (`D6_LIVE_SPLIT`) | **LIVE** (phase-split, Stage 113; task-lock drop before switch_frames, Stage 116; global-lock stash scaffold, Stage 117 Outcome B; first-resume handler + switch-frame init, Stage 118 Outcome B; minimal task pair + TSS RSP0 fix, Stage 119 Outcome B) | Stage 107 | scheduler-seam first wire; Stage 116 eliminates `task_state_lock` (rank 2) held across `switch_frames` via `DispatchSwitchPlan`; Stage 117 adds `PerCpuSwitchPlanStash` / `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE`; Stage 118 adds `FIRST_RESUME_STASH` / real trampoline / production init for tid=1 (x86_64); Stage 119 extends init to tid=2 and fixes TSS RSP0 in trampoline switch-back; Stage 120 adds a default-off `yarm.d6_switch_proof=1` / `D6_SWITCH_PROOF=1` x86_64 single-CPU one-shot proof harness for the unlocked `switch_frames` path; Stage 121 audits/fixes the x86_64 first-resume ABI boundary with an assembly shim + SysV stack shape diagnostics; Stage 122 adds raw COM1 `!R`/`!RA` first-instruction breadcrumbs to prove whether the CPU reaches the shim before Rust logging; Stage 123 removes the pre-Rust marker bridge call and replaces it with raw `!RM`; Stage 124 removes the obsolete shim stack adjustment and adds raw `!RJ`; Stage 125 routes `!RJ` to an x86_64 ABI bridge that emits `!RB`, aligns for a normal `call`, and calls the Rust real handler; Stage 126 gates `initialized=true` on a mapped writable kernel-only switch-stack page; Stage 127 corrects that gate to map/check the target task ASID/root and retries after ASID binding instead of depending on temporal active-ASID presence; Stage 128 strengthens the invariant again by mapping/checking the incoming switch-stack page in every existing task root that may be the active/outgoing CR3 during `switch_frames`, plus an active-root proof check before stashing; Stage 129 fixes the VmFull capacity-blocker by adding on-demand repair in the active-root guard when the active ASID was created after the incoming stack was initialized; per-CPU lock sharding deferred (§9); see §1 Stage 116 / Stage 117 / Stage 118 / Stage 119 |
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

### Stage 117 — Solution 2: global `SpinLock<KernelState>` stash infrastructure (Outcome B — scaffolding, not smoke-proven)

**Goal stated in the task:** release the outer `SpinLock<KernelState>` held by
`SharedKernel::with_cpu` BEFORE calling `switch_frames`, while keeping the CPU
non-preemptible (interrupts still disabled by hardware trap entry).

**Outcome: B — preparatory scaffolding; `switch_frames` not exercised in production smoke.**

The stash mechanism (`PerCpuSwitchPlanStash`, `DISPATCH_SWITCH_PLAN_STASH`,
`GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE`) is correctly implemented and tested. The
production trap path (`handle_trap_entry_shared` → `with_cpu`) does reach
`maybe_switch_kernel_context`. However, `switch_frames` is NEVER called in
production smoke because no production task has `kernel_context.initialized = true`:
`provision_default_kernel_context` (called by `register_task`) explicitly leaves
`initialized = false`; only `initialize_thread_kernel_switch_frame` sets it to
`true`, and that function has no production callers. The required proof markers
(`D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH`, `D6_SWITCH_FRAMES_ENTER_UNLOCKED`,
`D6_SWITCH_FRAMES_RETURNED_UNLOCKED`) therefore never appear in smoke logs.

**Why the stash path is never reached in production:**
1. **Timer interrupt path:** `dispatch_next()` returns `Some(current_task_tid)` —
   the currently running task is always re-selected (no preemption, same task).
   `maybe_switch_kernel_context(Some(A), A)` hits the `outgoing_tid == incoming_tid`
   early return at line 787 (no `D6_SWITCH_PLAN_BEGIN`).
2. **IPC blocking path:** `recv_block_phase_a_scheduler` sets `scheduler.current = None`
   before `dispatch_next_task` is called. `current_tid()` returns `None`.
   `maybe_switch_kernel_context(None, B)` hits the `outgoing_tid == None` early
   return at line 784 (no `D6_SWITCH_PLAN_BEGIN`). Emits
   `D6_GLOBAL_LOCK_DROP_DEFERRED reason=no_outgoing_task` on x86_64/AArch64 trap path.
3. **Yield path (different tasks):** when `outgoing_tid != incoming_tid`, the
   `with_tcbs_mut` block attempts to build a plan but finds
   `!tcb.kernel_context.initialized` for both tasks → returns `Ok(None)`. Emits
   `D6_GLOBAL_LOCK_DROP_DEFERRED reason=no_kernel_ctx_switch_frame` on
   x86_64/AArch64 trap path. `D6_SWITCH_PLAN_BEGIN` fires but no plan is built.

**Smoke-observable deferred markers (prove production trap path is reached):**
These appear in x86_64 and AArch64 smoke logs in lieu of the unlocked-switch
markers, proving the decision point is reached but the actual lock drop is deferred:
- `D6_GLOBAL_LOCK_DROP_DEFERRED reason=no_outgoing_task incoming=N` — IPC
  blocking dispatch, no outgoing kernel context to save
- `D6_GLOBAL_LOCK_DROP_DEFERRED reason=no_kernel_ctx_switch_frame outgoing=M incoming=N` —
  different tasks selected, but neither has an initialized kernel switch frame
- `D6_GLOBAL_LOCK_DROP_DEFERRED reason=riscv_lockless_trap_path` — RISC-V (unchanged)
- `D6_GLOBAL_LOCK_DROP_DEFERRED reason=multi_cpu_not_proven` — multi-CPU fallback
  (fires when kernel threads exist but multi-CPU proof is pending)

**Stash path wiring (correct but dormant in production):**

- **Phase B** (inside `with_tcbs_mut`): existing Stage 116 path. `DispatchSwitchPlan`
  is built when BOTH tasks have `kernel_context.initialized = true`.
- **Phase C / D / E — stash path** (single-CPU, x86_64/AArch64, production
  trap path, kernel threads only): `maybe_switch_kernel_context` stashes the plan
  in `DISPATCH_SWITCH_PLAN_STASH[cpu_idx]`. `handle_trap_entry_with_fault_bookkeeping_mode`
  skips `restore_arch_thread_state`. `handle_trap_entry_shared` drains the stash
  after `with_cpu` drops the lock, calls `switch_frames` unlocked, re-acquires
  the lock for `post_switch_restore_arch_thread_state`.
- **Fallback path** (RISC-V, multi-CPU, test direct-call, all production user tasks):
  `D6_GLOBAL_LOCK_DROP_DEFERRED reason=...`, Stage 116 direct path or early return.

**Key infrastructure:**

- `PerCpuSwitchPlanStash` (`src/kernel/boot/mod.rs`): `UnsafeCell<Option<DispatchSwitchPlan>>`
  with `unsafe store / take / has_plan` operations. `Sync` via `unsafe impl` —
  safe because single-CPU, interrupts disabled.
- `DISPATCH_SWITCH_PLAN_STASH: [PerCpuSwitchPlanStash; MAX_CPUS]` static.
- `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE: [AtomicBool; MAX_CPUS]` static. Set to
  `true` by `handle_trap_entry_shared` before `with_cpu`; cleared after the
  stash drain. Unit tests (which never call `handle_trap_entry_shared`) always
  use the Stage 116 fallback path.
- `post_switch_restore_arch_thread_state` (`src/arch/trap_entry.rs`):
  arch-dispatched wrapper — `restore_arch_thread_state` (x86_64) /
  `restore_arch_thread_state_post_switch` (AArch64, `syscall_return=false`) /
  no-op (RISC-V). Called from the second `with_cpu` after `switch_frames`.
- `can_stash_for_lock_drop` condition: `!cfg!(target_arch = "riscv64") &&
  online_cpu_count() <= 1 && GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu_idx]`.

**IRQ safety argument (correct even if dormant).**
`SpinLock<KernelState>` is NOT a `SpinLockIrq`; dropping it does NOT re-enable
IRQs. Hardware disables IRQs on x86_64/AArch64 trap entry; they remain disabled
throughout the stash drain and `switch_frames` call. The second `with_cpu` call
is safe (IRQs still off).

**What this stage does NOT do (hard rules preserved):**
No ABI changes. No syscall number changes. No image ID changes. No IPC recv ABI
changes. No D2-B send blocking split. No D3-FULL. No D6-full per-CPU sharding.
No x86_64 AP scheduler-online. No `switch_frames` assembly ABI change. No lock
handoff/guard transfer. `SYSCALL::VARIANT_COUNT` remains 23.

**Why Outcome B and not Outcome A here.** The stash infrastructure and IRQ safety
argument are sound. The blocker is `kernel_context.initialized = false` for all
production tasks. Activating Outcome A requires either: (a) adding kernel-thread
infrastructure (`initialize_thread_kernel_switch_frame` callers in the production
boot path, plus a real `yarm_kernel_thread_switch_trampoline` that handles first-time
kernel-side resumption), or (b) wiring the lock-drop to the trap-frame-only context
switch path (not `switch_frames`). Both are follow-on work. This stage establishes
the complete stash mechanism, IRQ-safety proof, and smoke-observable deferred markers
as scaffolding for that follow-on.

**Tests added.** `src/kernel/boot/tests.rs` gained 21 Stage 117 tests in
`mod stage117_global_lock_drop_before_switch`:
`stage117_per_cpu_stash_struct_exists`,
`stage117_dispatch_switch_plan_stash_static_exists`,
`stage117_trap_path_active_flag_exists`,
`stage117_exec_state_emits_global_lock_drop_plan_begin`,
`stage117_exec_state_emits_global_lock_drop_plan_ready`,
`stage117_exec_state_emits_deferred_marker`,
`stage117_exec_state_emits_no_outgoing_task_deferred_reason`,
`stage117_exec_state_emits_no_kernel_ctx_deferred_reason`,
`stage117_exec_state_checks_trap_path_active_flag`,
`stage117_exec_state_stash_gated_on_single_cpu`,
`stage117_exec_state_stash_gated_on_riscv_cfg`,
`stage117_trap_entry_sets_trap_path_active_flag`,
`stage117_trap_entry_emits_global_lock_dropped_before_switch`,
`stage117_trap_entry_emits_switch_frames_enter_unlocked`,
`stage117_trap_entry_emits_switch_frames_returned_unlocked`,
`stage117_trap_entry_post_switch_restore_function_exists`,
`stage117_x86_trap_skips_restore_when_stash_pending`,
`stage117_aarch64_trap_skips_restore_when_stash_pending`,
`stage117_stage116_fallback_markers_preserved`,
`stage117_dispatch_switch_plan_struct_preserved`,
`stage117_syscall_count_unchanged`.

Acceptance evidence (Stage 117):

| Smoke | Result | Notes |
|-------|--------|-------|
| `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` | PENDING | `D6_GLOBAL_LOCK_DROP_DEFERRED reason=no_outgoing_task` and/or `reason=no_kernel_ctx_switch_frame` must appear; `D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH` not required (Outcome B) |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` | PENDING | same |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` | PENDING | same |
| `./scripts/qemu-riscv64-smoke-matrix.sh` (`--smp 1/2/3/4`) | PENDING | `D6_GLOBAL_LOCK_DROP_DEFERRED reason=riscv_lockless_trap_path` must appear (unchanged from Stage 116) |

Workspace tests: 1548/0 lib (`--test-threads=1`, 2 ignored, pre-existing crash
in `load_elf_returns_heap_base_aligned_to_max_pt_load_end` under parallel runner).
`cargo check --features hosted-dev` and `git diff --check` clean.
No ABI/protocol/syscall-number/image-ID change.
`Syscall::VARIANT_COUNT` remains 23.

---

### Stage 118 — Production switch-frame init and first-resume handler (Outcome B — scaffolding, not smoke-proven)

**Goal stated in the task:** implement the narrow next step required before Stage
117 can become Outcome A: (a) initialize a production kernel switch frame for the
supervisor/init task (`BOOTSTRAP_FIRST_USER_TID = 1`) on x86_64; (b) replace the
spin-loop trampoline with a real first-resume handler that re-acquires the global
lock and calls `post_switch_restore_arch_thread_state`; (c) prove via D6 markers
that the handler can safely reacquire the lock; (d) keep all behavior safe and
fallback-gated.

**Outcome: B — preparatory scaffolding; `switch_frames` + first-resume path not
exercised in production smoke.**

Stage 118 adds the second half of the kernel-thread switch frame infrastructure.
The `switch_frames` call in `handle_trap_entry_shared` still never fires in
production smoke: only task 1 (tid = 1) gets `initialized = true`, and
`switch_frames` requires BOTH outgoing AND incoming tasks to have
`initialized = true`. No dispatch event pairs two initialized tasks in the current
smoke scenario.

**Changes by part:**

**Part A — Audit.** `ArchSwitchContext`, `KernelExecutionContext`,
`provision_default_kernel_context`, `initialize_thread_kernel_switch_frame`,
`yarm_kernel_thread_switch_trampoline`, `maybe_switch_kernel_context`,
`post_switch_restore_arch_thread_state`, and the per-arch `switch_frames`
implementations were audited to verify the type changes and new trampoline design
are safe.

**Part B — Narrow production init call** (`exec_state.rs`
`spawn_user_task_from_image`, x86_64 + `tid == BOOTSTRAP_FIRST_USER_TID` only):
calls `self.initialize_thread_kernel_switch_frame(spec.tid, trampoline_entry)`
after `register_task_with_class`. Emits:
- `D6_KERNEL_SWITCH_FRAME_INIT_BEGIN tid=1`
- `D6_KERNEL_SWITCH_FRAME_INIT_DONE tid=1 entry=0x... stack=0x...` on success
- `D6_KERNEL_SWITCH_FRAME_INIT_DEFERRED reason=init_failed tid=1 err=...` on failure

Task 1's `kernel_context.initialized` is now set to `true` on x86_64 at spawn
time. No other task gets `initialized = true` (the gate only fires for
`tid == BOOTSTRAP_FIRST_USER_TID`).

`DispatchSwitchPlan.incoming_frame_ptr` changed from `*const` to `*mut`
(trampoline needs mutable access for `switch_frames` `prev` argument).
`DispatchSwitchPlan.outgoing_stack_top: Option<u64>` added (trampoline needs
the outgoing task's stack top to restore the TSS RSP0 on switch-back).
`maybe_switch_kernel_context` updated: `incoming_tcb` now uses `.as_mut()`,
`incoming_frame_ptr` is `*mut`, and `outgoing_stack_top` is populated.

**Part C — Real first-resume handler** (`thread_state.rs`
`yarm_kernel_thread_switch_trampoline`):

On non-x86_64: emits `D6_FIRST_RESUME_DEFERRED reason=non_x86_64_arch` and spins.

On x86_64:
1. Takes `FIRST_RESUME_STASH[BOOTSTRAP_CPU_ID]` (per-CPU context stashed by
   the trap drain).
2. Emits `D6_FIRST_RESUME_ENTER tid={incoming} cpu={cpu_id}`.
3. `Bootstrap::shared_static_ref()` → emits `D6_FIRST_RESUME_DEFERRED
   reason=shared_not_ready` and spins if `None`.
4. Emits `D6_FIRST_RESUME_LOCK_REACQUIRE_BEGIN`.
5. `shared.with_cpu(cpu_id, |kernel| { ... })`:
   - Emits `D6_FIRST_RESUME_LOCK_REACQUIRE_DONE`
   - Emits `D6_FIRST_RESUME_POST_SWITCH_RESTORE_BEGIN`
   - Calls `post_switch_restore_arch_thread_state(kernel, cpu_id, None)` → no-op
     on x86_64 (frame is `None` → `restore_arch_thread_state` returns `Ok(())`)
   - Emits `D6_FIRST_RESUME_POST_SWITCH_RESTORE_DONE`
6. Calls `switch_frames(&mut *incoming_frame_ptr, &*outgoing_frame_ptr,
   outgoing_stack_top)` to switch back to the outgoing task. In production,
   execution never returns here — it resumes the outgoing task at POINT 2 in
   `handle_trap_entry_shared`.
7. Defensive spin for test builds (where `switch_frames` is a no-op).

`kernel_switch_frame_trampoline_ip() -> usize` helper added as `pub(crate)` in
`thread_state.rs`.

**Part D — Integration with Stage 117 stash path** (`trap_entry.rs`):

`post_switch_restore_arch_thread_state` made `pub(crate)` (all three arch
variants) so the trampoline in `thread_state.rs` can call
`crate::arch::trap_entry::post_switch_restore_arch_thread_state(...)`.

`D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH` and `D6_SWITCH_FRAMES_RETURNED_UNLOCKED`
now include `outgoing={}` and `incoming={}` fields.

x86_64-gated first-resume detection block added before `switch_frames` in the
stash drain: compares `incoming_frame.instruction_ptr()` to the trampoline
address (via `unsafe extern "C" { fn yarm_kernel_thread_switch_trampoline() -> !; }`).
If equal, populates `FIRST_RESUME_STASH[cpu_idx]` with a `FirstResumeContext`.

Required D6 proof-marker sequence (emitted when the live switch path fires):
```
D6_SWITCH_PLAN_READY outgoing=A incoming=B
D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH outgoing=A incoming=B
D6_SWITCH_FRAMES_ENTER_UNLOCKED outgoing=A incoming=B
D6_FIRST_RESUME_ENTER tid=B cpu=0
D6_FIRST_RESUME_LOCK_REACQUIRE_BEGIN
D6_FIRST_RESUME_LOCK_REACQUIRE_DONE
D6_FIRST_RESUME_POST_SWITCH_RESTORE_BEGIN
D6_FIRST_RESUME_POST_SWITCH_RESTORE_DONE
D6_SWITCH_FRAMES_RETURNED_UNLOCKED outgoing=A incoming=B
```
None of these appear in current smoke (Outcome B — `switch_frames` never fires).

**Part E — Gating.** x86_64 only for the init call and trampoline. Single-CPU
only (inherited from Stage 117's `can_stash_for_lock_drop` condition). Only when
both tasks have `initialized = true` (Stage 117 precondition, never met in smoke).

**Part F — New infrastructure types** (`mod.rs`):
- `FirstResumeContext`: `cpu_id`, `incoming_tid`, `outgoing_frame_ptr: *const`,
  `incoming_frame_ptr: *mut`, `outgoing_stack_top: Option<u64>`.
- `PerCpuFirstResumeStash`: `UnsafeCell<Option<FirstResumeContext>>` with
  `unsafe impl Sync`, `store`, `take` methods.
- `FIRST_RESUME_STASH: [PerCpuFirstResumeStash; MAX_CPUS]` static.

**Hard rules preserved.** No ABI changes. No syscall number changes. No image ID
changes. No service protocol changes. No FS gate changes. No x86_64 AP
scheduler-online. No per-CPU runqueue sharding. No D2-B send blocking. No
D3-FULL. No `switch_frames` assembly ABI change. No lock handoff / `mem::forget`
lock guards. No assembly unlock callbacks. RISC-V remains in smoke matrix on
Stage 117 fallback. `SYSCALL::VARIANT_COUNT` remains 23.

**Tests added.** `src/kernel/boot/tests.rs` gained 21 Stage 118 tests in
`mod stage118_production_switch_frame_init`:
`stage118_dispatch_switch_plan_has_outgoing_stack_top`,
`stage118_dispatch_switch_plan_incoming_frame_ptr_is_mut`,
`stage118_first_resume_context_struct_exists`,
`stage118_per_cpu_first_resume_stash_struct_exists`,
`stage118_first_resume_stash_static_exists`,
`stage118_exec_state_emits_switch_frame_init_begin`,
`stage118_exec_state_emits_switch_frame_init_done`,
`stage118_exec_state_emits_switch_frame_init_deferred`,
`stage118_exec_state_switch_frame_init_gated_on_x86_64_and_tid1`,
`stage118_exec_state_incoming_frame_ptr_derived_as_mut`,
`stage118_exec_state_outgoing_stack_top_in_plan`,
`stage118_thread_state_trampoline_ip_helper_exists`,
`stage118_thread_state_trampoline_emits_first_resume_enter`,
`stage118_thread_state_trampoline_emits_lock_reacquire_markers`,
`stage118_thread_state_trampoline_emits_post_switch_restore_markers`,
`stage118_thread_state_trampoline_emits_deferred_on_non_x86_64`,
`stage118_trap_entry_emits_global_lock_dropped_with_tids`,
`stage118_trap_entry_emits_switch_frames_returned_with_tids`,
`stage118_trap_entry_post_switch_restore_is_pub_crate`,
`stage118_trap_entry_populates_first_resume_stash`,
`stage118_stage117_seams_preserved`.

Stage 116 test `stage116_dispatch_switch_plan_has_raw_pointer_fields` updated:
`incoming_frame_ptr: *const` assertion changed to `*mut` to reflect Stage 118's
widening of the type (trampoline needs `*mut` for `switch_frames` `prev` argument).

Acceptance evidence (Stage 118):

| Smoke | Result | Notes |
|-------|--------|-------|
| `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` | PASS | `D6_KERNEL_SWITCH_FRAME_INIT_DONE tid=1` observed; `D6_FIRST_RESUME_ENTER` absent (Outcome B — only tid=1 initialized) |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` | PASS | all required markers; `D2_PUBLISH_RACE_UNWIND` count=0 |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` | PASS | `D6_KERNEL_SWITCH_FRAME_INIT_BEGIN` not emitted on AArch64 (x86_64 only); Stage 117 deferred markers unchanged |
| `./scripts/qemu-riscv64-smoke-matrix.sh` (`--smp 1/2/3/4`) | PASS | RISC-V unchanged; all four SMP configurations passed |

Workspace tests: 1569/0 lib (`--test-threads=1`, 2 ignored).
No ABI/protocol/syscall-number/image-ID change.
`Syscall::VARIANT_COUNT` remains 23.

---

### Stage 119 — Minimal task pair for first real `switch_frames` on x86_64 (Outcome B)

**Goal stated in the task:** extend the x86_64 production switch-frame
initialization from only `tid=1` to the minimal required task pair `(tid=1,
tid=2)`, fix the TSS RSP0 bug in the first-resume trampoline switch-back, and
prove that a real unlocked `switch_frames` fires in smoke.

**Outcome: B — expanded scaffold; `switch_frames` still does not fire in smoke.**

Both tid=1 (init server) and tid=2 (supervisor) now have `initialized = true` on
x86_64 at spawn time. The `D6_KERNEL_SWITCH_FRAME_INIT_DONE` markers appear for
both in the x86_64 core smoke. The TSS RSP0 preservation bug in the trampoline
switch-back is corrected. The dispatch infrastructure is complete. The smoke still
quiesces before a timer-driven preemption can pair two initialized tasks:
`maybe_switch_kernel_context` fires with `outgoing=0` (initial idle CPU state),
returns `None` because tid=0 is uninitialized, then all user tasks block on IPC
receive before any further preemption occurs.

**Changes by part:**

**Part A — Minimal task-pair init** (`exec_state.rs`): added
`BOOTSTRAP_SUPERVISOR_TID: u64 = 2` constant. Extended the x86_64
`spawn_user_task_from_image` init gate from
`spec.tid == BOOTSTRAP_FIRST_USER_TID` to
`spec.tid == BOOTSTRAP_FIRST_USER_TID || spec.tid == BOOTSTRAP_SUPERVISOR_TID`.
Both tasks now emit `D6_KERNEL_SWITCH_FRAME_INIT_BEGIN/DONE/DEFERRED` at spawn,
and both have `kernel_context.initialized = true` on x86_64.

**Part C — TSS RSP0 fix** (`thread_state.rs`
`yarm_kernel_thread_switch_trampoline`):

The trampoline switch-back previously passed `ctx.outgoing_stack_top` as the
`next_kernel_stack_top` argument to `switch_frames`. On x86_64 this calls
`refresh_boot_tss_rsp0(A.stack_top)`, overwriting the TSS RSP0 value that the
stash-drain's `switch_frames(A, B, B.stack_top)` had set to B's kernel stack top.
After IRETQ starts B in user mode, any subsequent interrupt on B would then use
A's kernel stack — silent stack corruption.

Fix: pass `None` instead. The stash-drain `switch_frames` already set
`TSS RSP0 = B.stack_top`; passing `None` in the trampoline preserves it.

**Part D — Fallback paths preserved.** All deferred markers remain:
`D6_GLOBAL_LOCK_DROP_DEFERRED reason=no_outgoing_task`,
`D6_FIRST_RESUME_DEFERRED reason=non_x86_64_arch/stash_empty/shared_not_ready`,
`maybe_switch_kernel_context` initialized guard for non-pair tasks. RISC-V
remains on the Stage 117 fallback path.

**What this stage does NOT do (hard rules preserved).** No ABI changes. No
syscall number changes. No image ID changes. No service protocol changes. No FS
gate changes. No x86_64 AP scheduler-online. No per-CPU runqueue sharding. No
D2-B send blocking. No D3-FULL. No `switch_frames` assembly ABI change. No lock
handoff / `mem::forget` lock guards. No assembly unlock callbacks. All current
smokes preserved. RISC-V remains in smoke matrix.
`Syscall::VARIANT_COUNT` remains 23.

**Why Outcome B and not Outcome A here.** Both tasks are initialized. The
dispatch infrastructure is complete. The blocker is scheduling dynamics: in the
current smoke, the very first dispatch event has `outgoing=0` (the idle CPU
state, uninitialized), so `maybe_switch_kernel_context` returns `None`. After
that, all user tasks block on IPC receive (supervisor waiting for init calls, pm
waiting for requests) before any timer tick can preempt a running initialized
task while another initialized task is queued. Outcome A requires either (a) a
longer-running user-mode workload that doesn't immediately block, or (b) a
synthetic smoke with explicit timer forcing. Neither is in scope for Stage 119.

**Tests added.** `src/kernel/boot/tests.rs` gained 18 Stage 119 tests in
`mod stage119_minimal_task_pair`:
`stage119_bootstrap_supervisor_tid_constant_defined`,
`stage119_bootstrap_supervisor_tid_is_two`,
`stage119_exec_state_init_gate_covers_supervisor_tid`,
`stage119_exec_state_init_gate_uses_or_for_both_tids`,
`stage119_exec_state_init_gate_still_covers_first_user_tid`,
`stage119_exec_state_switch_frame_init_markers_still_present`,
`stage119_trampoline_switchback_does_not_pass_outgoing_stack_top`,
`stage119_trampoline_switchback_passes_none_for_tss_rsp0`,
`stage119_trampoline_switchback_has_tss_rsp0_preservation_comment`,
`stage119_trampoline_non_x86_64_deferred_path_preserved`,
`stage119_trampoline_stash_empty_deferred_path_preserved`,
`stage119_trampoline_shared_not_ready_deferred_path_preserved`,
`stage119_exec_state_no_outgoing_task_deferred_path_preserved`,
`stage119_maybe_switch_kernel_context_initialized_guard_preserved`,
`stage119_first_resume_stash_seam_preserved`,
`stage119_stage117_switch_plan_stash_seam_preserved`,
`stage119_provision_default_kernel_context_still_sets_initialized_false`,
`stage119_supervisor_tid_init_gated_on_x86_64_cfg`.

Acceptance evidence (Stage 119):

| Smoke | Result | Notes |
|-------|--------|-------|
| `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` | PASS | `D6_KERNEL_SWITCH_FRAME_INIT_DONE tid=2` and `D6_KERNEL_SWITCH_FRAME_INIT_DONE tid=1` both observed; `D6_FIRST_RESUME_ENTER` absent (Outcome B) |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` | PASS | all required markers; `D2_PUBLISH_RACE_UNWIND` count=0 |
| `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` | PASS | AArch64 unaffected; RAMFS + ext4 live |
| `./scripts/qemu-riscv64-smoke-matrix.sh` (`--smp 1/2/3/4`) | PASS | all four SMP configurations passed; RISC-V on Stage 117 fallback |

Workspace tests: 1587/0 lib (`--test-threads=1`, 2 ignored).
No ABI/protocol/syscall-number/image-ID change.
`Syscall::VARIANT_COUNT` remains 23.


### Stage 120 — Controlled one-shot x86_64 unlocked `switch_frames` proof harness

**Goal stated in the task:** add a diagnostic-only harness that can force exactly
one initialized task-to-task kernel context switch on x86_64, single-CPU only, so
the existing Stage 117/118/119 global-lock-drop + first-resume path can be proven
without turning it into scheduler policy.

**Outcome: B locally — harness landed, proof smoke pending artifact availability.** The harness is gated by
the boot command-line knob `yarm.d6_switch_proof=1`; the x86_64 core smoke script
adds that knob only when invoked as `D6_SWITCH_PROOF=1 QEMU_SMP=1
./scripts/qemu-x86_64-core-smoke.sh`. Default smokes do not request the proof and
therefore do not require the proof markers. The harness is intended to produce
Outcome A once the x86_64 QEMU artifacts are available locally and the proof
smoke can observe `D6_CONTROLLED_SWITCH_PROOF_DONE`.

**Design:**

- x86_64 only: the live hook is inside `#[cfg(target_arch = "x86_64")]` and the
  command-line knob is ignored on non-x86_64 builds.
- Single-CPU only: `maybe_run_d6_controlled_switch_proof` defers with
  `D6_CONTROLLED_SWITCH_PROOF_DEFERRED reason=multi_cpu online_cpus=N` unless
  `online_cpu_count() == 1`.
- One-shot only: `D6_CONTROLLED_SWITCH_PROOF_STARTED` uses atomic
  `compare_exchange`; `D6_CONTROLLED_SWITCH_PROOF_DONE` permanently suppresses
  repeats after success.
- Safe pair: the harness waits until current `outgoing=1` and `incoming=2` both
  have `kernel_context.initialized == true`; otherwise it emits a precise
  deferred marker (`no_current_tid`, `wrong_outgoing_tid`, or
  `frames_uninitialized`).
- Existing path only: after `D6_CONTROLLED_SWITCH_PROOF_BEGIN` and
  `D6_CONTROLLED_SWITCH_PROOF_PAIR outgoing=1 incoming=2`, it calls
  `maybe_switch_kernel_context(Some(1), 2)`, which builds the existing
  `DispatchSwitchPlan`, stores it in `DISPATCH_SWITCH_PLAN_STASH`, drops the
  global lock in `handle_trap_entry_shared`, calls `switch_frames`, enters the
  x86_64 first-resume trampoline, reacquires the lock, runs
  `post_switch_restore_arch_thread_state`, switches back, and finally emits
  `D6_CONTROLLED_SWITCH_PROOF_DONE`.

**Expected proof markers:**

```text
D6_CONTROLLED_SWITCH_PROOF_BEGIN
D6_CONTROLLED_SWITCH_PROOF_PAIR outgoing=1 incoming=2
D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH outgoing=1 incoming=2
D6_SWITCH_FRAMES_ENTER_UNLOCKED outgoing=1 incoming=2
D6_FIRST_RESUME_ENTER tid=2 cpu=0
D6_FIRST_RESUME_LOCK_REACQUIRE_BEGIN
D6_FIRST_RESUME_LOCK_REACQUIRE_DONE
D6_FIRST_RESUME_POST_SWITCH_RESTORE_BEGIN
D6_FIRST_RESUME_POST_SWITCH_RESTORE_DONE
D6_SWITCH_FRAMES_RETURNED_UNLOCKED outgoing=1 incoming=2
D6_CONTROLLED_SWITCH_PROOF_DONE
```

Deferred mode emits `D6_CONTROLLED_SWITCH_PROOF_DEFERRED reason=<exact_reason>`
and never fakes success.

**Hard boundaries preserved:** no timer preemption enablement, no scheduler
fairness change, no x86_64 AP scheduler-online, no per-CPU runqueue sharding, no
D2-B send blocking, no D3-FULL VmAnonMap, no `await_tlb_shootdown_ack` redesign,
no `switch_frames` assembly ABI change, no lock handoff / `mem::forget`, no
assembly unlock callback, and no ABI/syscall/image-ID/service/FS-gate change.
AArch64 and RISC-V remain unchanged/fallback-safe; they do not call the proof
hook and do not require proof markers in smoke.

**Tests added.** `src/kernel/boot/tests.rs` gained Stage 120 source checks in
`mod stage120_controlled_switch_proof` covering the x86_64-only gate, single-CPU
gate, one-shot atomics, boot knob, initialized tid-pair requirement, reuse of
`DispatchSwitchPlan`, reuse of the stash/global-lock-drop path, no timer
preemption/fairness/AP/lock-handoff/assembly-callback changes, AArch64/RISC-V
non-participation, Stage 119 tid=1/tid=2 initialization, D4 extracted modules,
`SYSCALL_COUNT == 31`, and `Syscall::VARIANT_COUNT == 23`.

---

### Stage 121 — x86_64 first-resume entry/frame ABI diagnostics and source fix

**Goal stated in the task:** make the x86_64 `switch_frames` restore →
first-resume boundary diagnosable, and correct the source-level frame/entry ABI
if the audit shows why the Stage 120 proof crashes after
`D6_SWITCH_FRAMES_ENTER_UNLOCKED` but before `D6_FIRST_RESUME_ENTER`.

**Outcome: A-source — source audit identified and fixed the first-resume ABI
shape; QEMU proof validation is pending user/local run.** The Stage 120 proof
now reaches the unlocked `switch_frames` entry with incoming RIP equal to the
expected first-resume trampoline. The audited x86_64 switch primitive restores
`rsp` from `ArchSwitchContext.words[0]` and enters `rip` from
`ArchSwitchContext.words[1]` using `jmp [rsi + 8]` rather than `ret`. A direct
Rust `extern "C" fn` entry therefore must still receive normal SysV callee
stack shape (`rsp % 16 == 8`). Stage 120 initialized the first-resume stack to a
16-byte-aligned top (`rsp % 16 == 0`), which is not the ABI shape a Rust function
expects when entered by a jump.

**Fix / diagnostics:**

- x86_64 keeps the `switch_frames` assembly ABI unchanged. No callback, lock
  handoff, or extra argument was added.
- The first-resume entry symbol is now a tiny x86_64-only assembly shim,
  `yarm_kernel_thread_switch_trampoline`, which emits the ultra-early
  `!RM` raw marker at the removed pre-Rust marker-bridge boundary and then
  tail-jumps directly to the Rust handler `yarm_kernel_thread_switch_trampoline_rust`.
- `initialize_thread_kernel_switch_frame` now reserves one word below the
  16-byte-aligned kernel stack top on x86_64, so the first-resume handler sees
  `rsp % 16 == 8` after `switch_frames` jumps to the shim. The word is a fake
  return-address slot for ABI shape only; the handler is `-> !`, so it is never
  consumed. Non-x86_64 keeps the previous stack-top behavior.
- The Rust handler now emits `D6_FIRST_RESUME_RUST_ENTER`,
  `D6_FIRST_RESUME_STACK_ALIGN value=...`, `D6_FIRST_RESUME_STASH_OK`, and
  `D6_FIRST_RESUME_STASH_MISSING` before the existing lock-reacquire markers,
  making the exact first-resume boundary observable.
- `FIRST_RESUME_STASH` is still populated in the stash drain before
  `switch_frames`; `D6_FIRST_RESUME_STASH_MISSING` distinguishes an entry ABI
  success from a missing-stash failure.

**Expected local validation markers after this source fix:**

```text
D6_CONTROLLED_SWITCH_PROOF_BEGIN
D6_CONTROLLED_SWITCH_PROOF_PAIR outgoing=1 incoming=2
D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH outgoing=1 incoming=2
D6_SWITCH_FRAMES_ENTER_UNLOCKED outgoing=1 incoming=2
!R
!RA
!RM
!RJ
D6_FIRST_RESUME_RUST_ENTER
D6_FIRST_RESUME_STACK_ALIGN value=8
D6_FIRST_RESUME_STASH_OK
D6_FIRST_RESUME_ENTER tid=2 cpu=0
D6_FIRST_RESUME_LOCK_REACQUIRE_BEGIN
D6_FIRST_RESUME_LOCK_REACQUIRE_DONE
D6_FIRST_RESUME_POST_SWITCH_RESTORE_BEGIN
D6_FIRST_RESUME_POST_SWITCH_RESTORE_DONE
D6_SWITCH_FRAMES_RETURNED_UNLOCKED outgoing=1 incoming=2
D6_CONTROLLED_SWITCH_PROOF_DONE
```

**Hard boundaries preserved:** x86_64 proof-mode path only; Stage 120 remains
default-off behind `yarm.d6_switch_proof=1` / `D6_SWITCH_PROOF=1`; no scheduler
policy, timer/preemption, AP scheduler-online, per-CPU runqueue, D2/D3/D6
semantic, ABI/syscall/image-ID/service/FS, lock-handoff, `mem::forget`, or
assembly-unlock-callback change. AArch64 and RISC-V paths are unchanged and do
not use the x86_64 first-resume shim.

**Tests added.** `src/kernel/boot/tests.rs` gained Stage 121 source checks for
the x86_64 assembly shim and early markers, `ArchSwitchContext` layout vs.
`switch_frames` offsets, initialized-frame entry symbol and `rsp % 16 == 8`
shape, fake return-address documentation, `FIRST_RESUME_STASH` population before
`switch_frames`, stash-present/missing markers, absence of `mem::forget` /
assembly unlock callbacks, AArch64/RISC-V non-participation, Stage 120
default-off gating, D4 extracted modules, `SYSCALL_COUNT == 31`, and
`Syscall::VARIANT_COUNT == 23`.

---

### Stage 122 — x86_64 first-resume trampoline first-instruction proof

**Goal stated in the task:** prove whether the CPU reaches the first instruction
of `yarm_kernel_thread_switch_trampoline` after x86_64 `switch_frames` restores
the incoming frame and jumps to the trampoline.

**Outcome: A-source — ultra-early first-instruction breadcrumbs landed; QEMU
proof validation is pending user/local run.** The Stage 121 local proof log
showed the controlled pair reached `D6_SWITCH_FRAMES_ENTER_UNLOCKED` and the
low-level switch breadcrumbs showed the incoming RIP/RSP pair, with
`rsp % 16 == 8`. No `D6_FIRST_RESUME_ASM_ENTER` / `RUST_ENTER` / `STASH_OK`
markers appeared, so the remaining boundary is the jump into the first-resume
trampoline itself.

**Audit result:**

- `kernel_switch_frame_trampoline_ip()` takes the address of the
  `yarm_kernel_thread_switch_trampoline` assembly shim symbol, not the Rust
  handler. The Stage 119/120 init path uses that helper when logging
  `D6_KERNEL_SWITCH_FRAME_INIT_DONE entry=...`, so the logged entry value should
  match the shim label.
- The shim is declared in executable kernel text (`.section .text, "ax",
  @progbits`) with `.global yarm_kernel_thread_switch_trampoline` and function
  type metadata; there is no Rust symbol alias for the live x86_64 shim in
  non-test builds.
- The first raw marker does not depend on stack validity. It writes directly to
  COM1 with `out dx, al` before any Rust call and before complex logging; Stage
  124 later removed the temporary shim stack adjustment entirely.

**Raw marker order:**

```text
yarm_kernel_thread_switch_trampoline:
  !R   # emitted through raw COM1 as the first-instruction proof
  !RA  # emitted through raw COM1 at the former stack-adjust boundary
  !RM  # raw replacement for the removed Rust marker bridge
  !RJ  # emitted immediately before the Rust tail-jump
  jmp yarm_kernel_thread_switch_trampoline_rust
```

**Local interpretation for the next proof run:**

- no `!R`: `switch_frames` jumps to the wrong address, the target is not
  executable, or execution faults before the first shim instruction can emit.
- `!R` but no `!RA`: crash before the former stack-adjust diagnostic boundary.
- `!R`/`!RA`/`!RM` but no `!RJ`: crash before the final Rust tail-jump marker.
- `!RJ` but no `D6_FIRST_RESUME_RUST_ENTER`: tail-jump / Rust handler ABI
  boundary.
- `D6_FIRST_RESUME_RUST_ENTER` but no `D6_FIRST_RESUME_STASH_OK`: stash
  visibility or population bug.
- Full chain to `D6_CONTROLLED_SWITCH_PROOF_DONE`: Stage 120/121/122 live proof
  succeeds.

**Hard boundaries preserved:** x86_64 first-resume/proof path only; Stage 120
remains default-off behind `yarm.d6_switch_proof=1` / `D6_SWITCH_PROOF=1`; no
scheduler policy, timer/preemption, AP scheduler-online, `switch_frames` ABI,
lock-handoff, `mem::forget`, assembly-unlock-callback, ABI/syscall/image-ID/
service/FS-gate, AArch64, or RISC-V behavior change.

**Tests added.** `src/kernel/boot/tests.rs` gained Stage 122 source checks that
prove the trampoline IP helper names the assembly shim symbol, the shim is an
executable text symbol, raw `!R` appears before the Rust tail-jump, raw `!RA`,
`!RM`, and `!RJ` appear in order before entering Rust, initialized frames use the
trampoline helper, `switch_frames` ABI is
unchanged, no `mem::forget` / unlock callback is introduced, Stage 120 remains
default-off, AArch64/RISC-V paths remain untouched, and syscall counts stay at
`SYSCALL_COUNT == 31` / `Syscall::VARIANT_COUNT == 23`.

---

### Stage 123 — remove Rust call from first-resume asm marker boundary

**Goal stated in the task:** the Stage 122 local proof showed `!R` and `!RA`,
then crashed before `D6_FIRST_RESUME_ASM_ENTER`. That proves the CPU reaches the
trampoline first instruction and survives the stack-adjust boundary; the failure
is the pre-Rust call to `yarm_x86_first_resume_asm_marker`.

**Outcome: A-source — the pre-Rust marker bridge call was removed.** The
x86_64 first-resume shim now stays raw-COM1-only until it tail-jumps into the
Rust first-resume handler. `!R` and `!RA` remain, and a new `!RM` marker is
emitted at the point where the Rust marker bridge used to run. The shim then
jumps directly to `yarm_kernel_thread_switch_trampoline_rust`. Stage 124 later removed the now-obsolete stack adjustment so the initialized `rsp % 16 == 8` shape is preserved at the Rust tail-jump.

**Raw marker order after Stage 123:**

```text
yarm_kernel_thread_switch_trampoline:
  !R   # reached shim entry
  !RA  # reached the former stack-adjust boundary
  !RM  # would-have-entered ASM marker bridge; no Rust call occurs here
  !RJ  # final pre-Rust tail-jump marker (Stage 124)
  jmp yarm_kernel_thread_switch_trampoline_rust
```

**Expected next proof chain:**

```text
!R
!RA
!RM
!RJ
D6_FIRST_RESUME_RUST_ENTER
D6_FIRST_RESUME_STACK_ALIGN value=8
D6_FIRST_RESUME_STASH_OK
D6_FIRST_RESUME_ENTER tid=2 cpu=0
```

If `D6_FIRST_RESUME_RUST_ENTER` still does not appear after `!RM`, the next
boundary is the tail-jump to the Rust first-resume handler / Rust ABI entry.

**Hard boundaries preserved:** x86_64 first-resume/proof path only; Stage 120
remains default-off behind `yarm.d6_switch_proof=1` / `D6_SWITCH_PROOF=1`; no
`switch_frames` ABI change, scheduler policy change, timer/preemption change, AP scheduler-online, lock-handoff, `mem::forget`, assembly
unlock callback, ABI/syscall/image-ID/service/FS-gate change, or AArch64/RISC-V
behavior change.

**Tests added.** `src/kernel/boot/tests.rs` gained Stage 123 source checks that
prove the shim keeps `!R`/`!RA`, emits `!RM`, contains no call/function for
`yarm_x86_first_resume_asm_marker`, tail-jumps to the Rust handler after `!RM`,
keeps `switch_frames` ABI unchanged, leaves Stage 120 default-off, leaves
AArch64/RISC-V paths untouched, and preserves `SYSCALL_COUNT == 31` /
`Syscall::VARIANT_COUNT == 23`.

---

### Stage 124 — x86_64 first-resume Rust tail-jump ABI stack-shape fix

**Goal stated in the task:** the Stage 123 local proof reached `!R`, `!RA`, and
`!RM`, then crashed before `D6_FIRST_RESUME_RUST_ENTER`. That proves
`switch_frames` reaches the first-resume shim, the raw marker sequence runs, and
the failure boundary is the final `jmp yarm_kernel_thread_switch_trampoline_rust`
/ Rust ABI entry.

**Outcome: A-source — source audit identified and fixed the Rust tail-jump stack
shape. QEMU validation is pending the user/local proof run.** The initialized
x86_64 first-resume frame already reserves a fake return-address word below the
16-byte-aligned kernel stack top, so the shim is entered with the normal SysV
callee shape (`rsp % 16 == 8`). After Stage 123 removed the pre-Rust Rust call,
the shim no longer needs to realign for an internal call. Keeping a final
`add rsp, 8` before the tail-jump can undo the fake return-slot shape and enter
Rust with `rsp % 16 == 0`.

**Fix / diagnostics:**

- The x86_64 first-resume shim remains raw-COM1-only before Rust. It emits `!R`,
  `!RA`, `!RM`, and the new `!RJ` marker, then tail-jumps directly to
  `yarm_kernel_thread_switch_trampoline_rust`.
- The temporary `sub rsp, 8` / `add rsp, 8` shim adjustment is removed. The final
  tail-jump preserves the initialized `rsp % 16 == 8` shape supplied by the fake
  return slot.
- `!RJ` is emitted immediately before the final jump, so local proof logs can
  distinguish a crash before the tail-jump marker from a Rust entry ABI/target
  failure.

**Raw marker order after Stage 124:**

```text
yarm_kernel_thread_switch_trampoline:
  !R   # reached shim entry
  !RA  # reached the former stack-adjust boundary; no stack adjustment occurs
  !RM  # would-have-entered ASM marker bridge; no Rust call occurs here
  !RJ  # final marker immediately before Rust tail-jump
  jmp yarm_kernel_thread_switch_trampoline_rust
```

**Expected local interpretation:**

- `!R !RA !RM` but no `!RJ`: crash before the final jump marker.
- `!RJ` but no `D6_FIRST_RESUME_RUST_ENTER`: Rust entry ABI or target symbol
  still wrong.
- `D6_FIRST_RESUME_RUST_ENTER` but no `D6_FIRST_RESUME_STASH_OK`: stash
  visibility/population boundary.
- Full chain to `D6_CONTROLLED_SWITCH_PROOF_DONE`: Stage 120 proof succeeds.

**Hard boundaries preserved:** x86_64 proof-mode path only; Stage 120 remains
default-off behind `yarm.d6_switch_proof=1` / `D6_SWITCH_PROOF=1`; no
`switch_frames` ABI change, scheduler policy change, timer/preemption change, AP
scheduler-online, per-CPU runqueue, lock-handoff, `mem::forget`, assembly unlock
callback, ABI/syscall/image-ID/service/FS-gate change, or AArch64/RISC-V
behavior change.

**Tests added.** `src/kernel/boot/tests.rs` gained Stage 124 source checks that
prove `!RM` precedes `!RJ`, `!RJ` precedes the Rust tail-jump, the final
stack-shape contract is documented, `sub rsp, 8` / `add rsp, 8` stay absent from
the shim, no pre-Rust Rust marker call is reintroduced, the Rust handler remains
a tail-jump rather than a call, `switch_frames` ABI is unchanged, Stage 120
remains default-off, AArch64/RISC-V paths remain untouched, and
`SYSCALL_COUNT == 31` / `Syscall::VARIANT_COUNT == 23`.

### Stage 125 — x86_64 first-resume Rust entry bridge

**Goal stated in the task:** the Stage 124 local proof reached `!R`, `!RA`,
`!RM`, and `!RJ`, then crashed before `D6_FIRST_RESUME_RUST_ENTER`. That proves
the raw trampoline reaches its final pre-Rust marker, and the remaining boundary
is the transition from the raw trampoline into the Rust first-resume function.

**Outcome: A-source — an x86_64-only Rust-entry ABI bridge landed. QEMU
validation is pending the user/local proof run.** The raw trampoline no longer
jumps directly to a normal Rust ABI function. Instead, it jumps to
`yarm_kernel_thread_switch_trampoline_rust_bridge`, a tiny x86_64 assembly bridge
that emits `!RB`, adjusts the stack from the initialized `rsp % 16 == 8` bridge
entry shape to the caller-side `rsp % 16 == 0` shape required before `call`, then
uses `call yarm_kernel_thread_switch_trampoline_rust_real`. The Rust real handler
continues to emit `D6_FIRST_RESUME_RUST_ENTER`, stack alignment diagnostics, and
stash-present/missing markers.

**Bridge marker order after Stage 125:**

```text
yarm_kernel_thread_switch_trampoline:
  !R
  !RA
  !RM
  !RJ
  jmp yarm_kernel_thread_switch_trampoline_rust_bridge

yarm_kernel_thread_switch_trampoline_rust_bridge:
  !RB
  sub rsp, 8
  call yarm_kernel_thread_switch_trampoline_rust_real
  !RX  # only if the Rust real handler unexpectedly returns, then halt loop
```

**Expected local interpretation:**

- `!RJ` but no `!RB`: raw trampoline → bridge target problem.
- `!RB` but no `D6_FIRST_RESUME_RUST_ENTER`: bridge call → Rust handler ABI
  problem.
- `D6_FIRST_RESUME_RUST_ENTER` but no `D6_FIRST_RESUME_STASH_OK`: stash
  visibility/population boundary.
- Full chain to `D6_CONTROLLED_SWITCH_PROOF_DONE`: Stage 120 proof succeeds.

**Hard boundaries preserved:** x86_64 proof-mode path only; Stage 120 remains
default-off behind `yarm.d6_switch_proof=1` / `D6_SWITCH_PROOF=1`; no
`switch_frames` ABI change, scheduler policy change, timer/preemption change, AP
scheduler-online, per-CPU runqueue, lock-handoff, `mem::forget`, assembly unlock
callback, ABI/syscall/image-ID/service/FS-gate change, or AArch64/RISC-V
behavior change.

**Tests added.** `src/kernel/boot/tests.rs` gained Stage 125 source checks that
prove the raw trampoline targets the bridge rather than the Rust handler, the
bridge emits `!RB` before Rust, the bridge uses `call` rather than `jmp` for the
Rust real handler, the stack-alignment contract is documented, the Rust real
handler keeps `D6_FIRST_RESUME_RUST_ENTER` / stack / stash diagnostics,
`switch_frames` ABI is unchanged, `mem::forget` / lock handoff / assembly unlock
callbacks stay absent, Stage 120 remains default-off, AArch64/RISC-V paths remain
untouched, and `SYSCALL_COUNT == 31` / `Syscall::VARIANT_COUNT == 23`.

### Stage 126 — x86_64 kernel switch-stack mapping/backing invariant

**Status: Outcome A-source (QEMU validation pending user/local run).** Stage 125
local proof reached `!RB` and then faulted at the bridge `callq
 yarm_kernel_thread_switch_trampoline_rust_real`: the call pushed its return
address to `rsp - 8` (`0xffff800000007fe8` when the initialized switch stack top
is `0xffff800000008000`) and the page fault was a kernel write to a non-present
page. That moves the failing boundary from the Rust handler ABI to the incoming
kernel switch-stack mapping/backing invariant.

**Audit result.** x86_64 `stack_top` / `incoming_stack_top` values are virtual
higher-half kernel stack tops. `provision_default_kernel_context` assigns those
virtual tops in the fixed kernel-stack arena and leaves `initialized = false`;
`initialize_thread_kernel_switch_frame` is the only production helper that
publishes `kernel_context.initialized = true`. Before Stage 126, that publish did
not prove the page below the virtual top was physically backed and mapped in the
user CR3 that is active while `switch_frames` executes. User address-space shadow
bookkeeping intentionally rejects kernel-only mappings, so the Stage 126 helper
uses the x86_64 page-table layer directly and checks the hardware-visible PTEs.

**Fix.** `initialize_thread_kernel_switch_frame` now calls
`ensure_kernel_switch_stack_mapped(tid, stack_base, stack_top)` before writing the
frame RIP/RSP and before `initialized = true`. On x86_64 non-test builds, the
helper computes the page containing `top - 8`, verifies the same page covers the
bridge slots (`top - 16` and the observed `top - 24` call-push write), and rejects
non-writable/user-accessible PTEs. Stage 126 originally attempted to use active
ASID presence as the mapping target; Stage 127 corrects that below, and
Stage 128 further strengthens the invariant so the page is also present in roots
that may remain active while `switch_frames` uses the incoming stack:

```text
kernel_context.initialized == true
  implies the initialized x86_64 switch-stack top page is backed,
  mapped writable, supervisor/kernel-only, present in the target task
  ASID/root that will own the first-resume switch frame, and present in every
  existing task root that may be the active/outgoing CR3 while `switch_frames`
  and the first-resume bridge use the incoming stack.
```

**Markers.** Stage 126 adds the following initialization/proof diagnostics:

- `D6_KERNEL_SWITCH_STACK_CHECK_BEGIN tid=... top=0x...`
- `D6_KERNEL_SWITCH_STACK_MAP_BEGIN tid=... asid=... va=0x...`
- `D6_KERNEL_SWITCH_STACK_MAP_DONE tid=... asid=... va=0x...`
- `D6_KERNEL_SWITCH_STACK_CHECK_OK tid=... probe=0x...`
- `D6_KERNEL_SWITCH_STACK_CHECK_FAILED tid=... probe=0x... reason=...`
- `D6_KERNEL_SWITCH_STACK_MAP_DEFERRED reason=... tid=...`

**Expected local interpretation.** If stack check/map fails before the switch,
the backing/mapping blocker is now explicit and `initialized=true` is not
published. If `D6_KERNEL_SWITCH_STACK_CHECK_OK` appears and proof reaches `!RB`
but still no `D6_FIRST_RESUME_RUST_ENTER`, the bridge call stack push likely
succeeded and the next boundary is Rust call/prologue. Full chain to
`D6_CONTROLLED_SWITCH_PROOF_DONE` proves the Stage 120 path end-to-end.

**Hard boundaries preserved:** x86_64 proof/default-off path only; no
`switch_frames` ABI change, scheduler policy change, timer/preemption change, AP
scheduler-online, per-CPU runqueue change, lock handoff, `mem::forget`, assembly
unlock callback, ABI/syscall/image-ID/service/FS-gate change, or AArch64/RISC-V
behavior change.

**Tests added.** `src/kernel/boot/tests.rs` gained Stage 126 source checks that
pin the `initialized=true` gate, the `top - 8` / `top - 16` / observed
`0xffff800000007fe8` fault-page audit, kernel-only writable CR3-visible mapping,
Stage 125 bridge marker preservation, `switch_frames` ABI preservation, default-
off proof gating, AArch64/RISC-V non-impact, and `SYSCALL_COUNT == 31` /
`Syscall::VARIANT_COUNT == 23`.

### Stage 127 — target-ASID/root switch-stack mapping retry

**Status: Outcome A-source (QEMU validation pending user/local run).** Stage 126
moved the first-resume proof from an unsafe bridge `callq` page fault to a safe
initialization deferral. The local proof then showed the remaining blocker:
`tid=2` reached `D6_KERNEL_SWITCH_STACK_CHECK_BEGIN` but deferred with
`reason=no_active_asids`, while `tid=1` could later map/check its stack. That
proved active-ASID enumeration was too temporal for early supervisor/init spawn.

**Audit result.** In `spawn_user_task_from_image`, the first x86_64 switch-frame
initialization attempt runs immediately after `register_task_with_class`, before
the task's `tcb.asid = Some(asid)` assignment. Therefore `tid=2` can have a valid
spawn target ASID/root in the surrounding spawn spec while its TCB does not yet
publish that ASID. Active ASIDs are the wrong gate: the switch-stack invariant is
about the target task root that will own the initialized frame, not whether any
ASID is currently running at that instant. The x86_64 page-table API can map a
kernel-only page into a specific ASID/root directly once `task_asid(tid)` is
bound and `AddressSpaceManager::get(target_asid)` confirms the root exists.

**Fix.** `ensure_kernel_switch_stack_mapped` now maps/checks only the target
`task_asid(tid)` root. If the TCB has not published an ASID yet it emits
`D6_KERNEL_SWITCH_STACK_MAP_DEFERRED reason=target_asid_unavailable tid=...`; if
the ASID lacks a root it emits `reason=target_root_unavailable`. After
`spawn_user_task_from_image` binds `tcb.asid = Some(asid)`, Stage 127 retries
initialization for the tid=1/tid=2 proof pair and emits:

- `D6_KERNEL_SWITCH_FRAME_INIT_RETRY tid=...`
- `D6_KERNEL_SWITCH_FRAME_INIT_RETRY_DONE tid=...`

The critical invariant remains:

```text
kernel_context.initialized == true
  implies the page containing stack_top - 8 is backed/mapped writable,
  supervisor/kernel-only, and present in the target task ASID/root that will own
  the first-resume switch frame.
```

**Expected local interpretation.** If `tid=2` still defers with
`target_asid_unavailable` or `target_root_unavailable`, the next boundary is ASID
creation/binding timing. If `tid=2` reaches `D6_KERNEL_SWITCH_STACK_CHECK_OK` and
the proof reaches `!RB`, the stack mapping gate is fixed and the next boundary is
Rust call/prologue or stash. If `D6_FIRST_RESUME_RUST_ENTER` appears, the bridge
call push succeeded; full chain to `D6_CONTROLLED_SWITCH_PROOF_DONE` proves the
path end-to-end.

**Hard boundaries preserved:** x86_64 proof/default-off path only; no
`switch_frames` ABI change, scheduler policy change, timer/preemption change, AP
scheduler-online, per-CPU runqueue change, lock handoff, `mem::forget`, assembly
unlock callback, ABI/syscall/image-ID/service/FS-gate change, broad full-stack-
region mapping, user-accessible stack mapping, or AArch64/RISC-V behavior change.

**Tests added.** `src/kernel/boot/tests.rs` gained Stage 127 source checks that
pin target-ASID/root mapping, retry-after-ASID-bind ordering, `initialized=true`
gating, absence of `no_active_asids` as a terminal blocker, narrow `stack_top - 8`
page mapping, kernel-only writable flags, Stage 125 bridge preservation, default-
off proof gating, AArch64/RISC-V non-impact, D4 module preservation, and
`SYSCALL_COUNT == 31` / `Syscall::VARIANT_COUNT == 23`.


### Stage 128 — active-CR3/kernel-shared switch-stack coverage

**Status: Outcome A-source (QEMU validation pending user/local run).** Stage 127
fixed target-root initialization and the local proof again reached the Stage 125
bridge marker `!RB`, but the proof still faulted before
`D6_FIRST_RESUME_RUST_ENTER` around `0xffff800000007fe8`. That shows the target
ASID/root mapping alone is insufficient for the bridge `callq` push: at the
instant `switch_frames` changes `%rsp` and the bridge executes, the CPU may still
be using the outgoing/current CR3.

**Active CR3/root audit result.** `switch_frames` is a kernel stack/register
context switch; it does **not** switch CR3. The normal scheduler path switches
address spaces before `maybe_switch_kernel_context`, but the controlled Stage 120
proof directly stashes a `DispatchSwitchPlan` from the trap path and intentionally
reuses the Stage 117 stash/drain path without changing scheduler policy. Thus the
incoming tid=2 switch stack can be used while the outgoing tid=1 root is still the
active CR3. Kernel switch stacks are higher-half kernel VAs, so the page covering
`stack_top - 8` must be installed as a kernel-only shared mapping in every task
root that can be active during the proof, not merely in the incoming target root.

**Fix.** `ensure_kernel_switch_stack_mapped` still uses the target
`task_asid(tid)` root as the authority for allocating/backing the page, but then
installs the same physical page as `PageFlags::KERNEL_RW` into each currently
existing task root with a bound TCB ASID. This is intentionally narrow: it maps
only the single page containing `stack_top - 8` / the observed `top - 24` push,
not the full kernel-stack arena. The helper still rejects non-writable or
user-accessible PTEs and still runs before `kernel_context.initialized = true`.

The Stage 120 proof now also performs a pre-stash active-root check for the
incoming stack page. If the current HAL active ASID root does not resolve the
incoming stack page as writable supervisor memory, the proof defers with
`reason=active_stack_unmapped` rather than dropping the global lock and faulting.

The strengthened invariant is now:

```text
kernel_context.initialized == true
  implies the page containing stack_top - 8 is backed/mapped writable,
  supervisor/kernel-only, present in the target task ASID/root, and present in
  every existing task root that may be active while switch_frames/first-resume
  uses that incoming kernel stack.
```

**Markers.** Stage 128 keeps the Stage 126/127 markers and adds:

- `D6_KERNEL_SWITCH_STACK_ACTIVE_ROOT cpu=... active_asid=... cr3=0x...`
- `D6_KERNEL_SWITCH_STACK_MAP_SHARED_BEGIN tid=... va=0x...`
- `D6_KERNEL_SWITCH_STACK_MAP_SHARED_ROOT tid=... asid=... va=0x... result=...`
- `D6_KERNEL_SWITCH_STACK_MAP_SHARED_DONE tid=... va=0x...`
- `D6_KERNEL_SWITCH_STACK_MAP_SHARED_DEFERRED reason=... tid=...`
- `D6_KERNEL_SWITCH_STACK_ACTIVE_CHECK_OK tid=... active_asid=... probe=0x...`
- `D6_KERNEL_SWITCH_STACK_ACTIVE_CHECK_FAILED tid=... active_asid=... probe=0x... reason=...`

**Expected local interpretation.** If the active-root check fails, the proof now
identifies the exact CR3/root coverage blocker before `switch_frames`. If
`D6_KERNEL_SWITCH_STACK_ACTIVE_CHECK_OK` appears, the proof reaches `!RB`, and no
Rust marker follows, the call-push root coverage is likely fixed and the next
boundary is the Rust call/prologue. If `D6_FIRST_RESUME_RUST_ENTER` appears, the
bridge call entered the Rust handler; full chain to `D6_CONTROLLED_SWITCH_PROOF_DONE`
proves the path end-to-end.

**Hard boundaries preserved:** x86_64 proof/default-off path only; no
`switch_frames` ABI change, scheduler policy change, timer/preemption change, AP
scheduler-online, per-CPU runqueue change, lock handoff, `mem::forget`, assembly
unlock callback, ABI/syscall/image-ID/service/FS-gate change, broad full-stack-
region mapping, user-accessible stack mapping, or AArch64/RISC-V behavior change.

**Tests added.** `src/kernel/boot/tests.rs` gained Stage 128 source checks that
pin the CR3 audit, active-root proof check before stashing, shared-root one-page
mapping, kernel-only writable flags, `initialized=true` gate, Stage 125 bridge
markers, default-off proof gating, AArch64/RISC-V non-impact, D4 module
preservation, and `SYSCALL_COUNT == 31` / `Syscall::VARIANT_COUNT == 23`.

### Stage 129 — fix x86_64 active-root switch-stack mapping VmFull / capacity blocker

**Status: Outcome A-source (QEMU validation pending user/local run).** Stage 128
strengthened the invariant so the proof safely deferred with
`D6_CONTROLLED_SWITCH_PROOF_DEFERRED reason=active_stack_unmapped outgoing=1 incoming=2 err=VmFull`
instead of faulting. Stage 129 fixes the underlying blocker.

**Local diagnostic.** The deferred log showed:
```text
D6_CONTROLLED_SWITCH_PROOF_DEFERRED reason=active_stack_unmapped outgoing=1 incoming=2 err=VmFull
```
`ensure_active_root_can_use_kernel_switch_stack()` called `resolve_page(active_asid=1, stack_page=0xffff800000007ff8)`, got `None`, and returned `KernelError::VmFull` — not a true capacity error, but the fallback error code for "page not found."

**Root cause.** ASID 1 (the outgoing task's root) was created *after*
`initialize_thread_kernel_switch_frame(tid=2)` ran. The Stage 128 shared-root
loop maps the incoming switch-stack page into all existing task roots, but ASID 1
did not exist at that time, so it was never included.

**Fix.** `ensure_active_root_can_use_kernel_switch_stack()` now performs on-demand
repair when `resolve_page(active_asid, stack_page)` returns `None`:

1. Look up the physical address from the target ASID (`task_asid(incoming_tid)`)
   via `resolve_page(target_asid, stack_page)` — the target was properly mapped at
   init time.
2. Call `page_table::map_page(active_asid, stack_page, PhysAddr(phys), PageFlags::KERNEL_RW)`
   directly, bypassing user VM-region accounting.
3. Verify with a second `resolve_page` call that the PTE is now writable and
   supervisor-only.
4. If repair succeeds, emit `D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_DONE` and return OK.
5. If repair fails, classify the error (`page_table_capacity`, `page_table_invalid_addr`,
   `target_not_mapped`, `target_asid_missing`), set a one-shot `ACTIVE_ROOT_REPAIR_FAILED`
   `AtomicBool` to prevent log spam, emit `D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_FAILED` /
   `D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_DEFERRED`, and return `Err(VmFull)`.

The fast path (correct PTE already present) returns OK immediately without repair
(idempotent). Wrong-flags PTEs (non-writable or user-accessible) reject with a
classified reason without attempting repair.

**Strengthened invariant.** After Stage 129 the active-root guard can self-heal
the case where the active/outgoing ASID was created after the incoming task's
switch-stack was initialized, eliminating the `VmFull` capacity-blocker deferral
for normal task orderings.

**Markers.** Stage 129 keeps all Stage 126/127/128 markers and adds:

- `D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_BEGIN tid=... active_asid=... probe=0x...`
- `D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_DONE tid=... active_asid=... probe=0x...`
- `D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_FAILED tid=... active_asid=... probe=0x... reason=...`
- `D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_DEFERRED tid=... active_asid=... probe=0x... reason=...`
- `D6_KERNEL_SWITCH_STACK_ACTIVE_CHECK_OK tid=... active_asid=... probe=0x...` (refined — now emitted after successful repair too)

**Hard boundaries preserved:** x86_64 proof/default-off path only; no
`switch_frames` ABI change, scheduler policy change, timer/preemption change, AP
scheduler-online, per-CPU runqueue change, lock handoff, `mem::forget`, assembly
unlock callback, ABI/syscall/image-ID/service/FS-gate change, broad full-stack-
region mapping, user-accessible stack mapping, or AArch64/RISC-V behavior change.
`SYSCALL_COUNT == 31`, `Syscall::VARIANT_COUNT == 23`.

**Tests added.** `src/kernel/boot/tests.rs` gained a `stage129_active_root_repair`
module (18 tests) covering: bypasses user VM accounting, VmFull source classified,
maps only the probe page, kernel-only writable flags, idempotent PTE acceptance,
user-accessible PTE rejection, active ASID checked before stash, one-shot
`ACTIVE_ROOT_REPAIR_FAILED` flag, initialized gate, bridge markers, default-off
proof, `switch_frames` ABI unchanged, no forbidden patterns, AArch64/RISC-V
untouched, D4/syscall counts, new Stage 129 markers present in source.

### Stage 130 — D6 proof cleanup / post-proof stability

**Status: Outcome A-source (QEMU validation pending user/local run).** After
`D6_CONTROLLED_SWITCH_PROOF_DONE`, the proof state must quiesce cleanly: stale
stash entries cleared, atomics zeroed, and x86_64 architectural state
(scheduler current TID, active CR3/ASID, TSS RSP0) verified consistent.

**TSS RSP0 fix.** The trampoline (`yarm_kernel_thread_switch_trampoline_rust_real`
in `thread_state.rs`) previously called `switch_frames(..., None)` for the
switch-back from TID2 to TID1. Passing `None` left TSS RSP0 pointing to TID2's
kernel stack top — a latent stack-corruption bug: any interrupt firing while TID1
ran in user mode after the proof would push its frame onto TID2's kernel stack.
Stage 130 passes `ctx.outgoing_stack_top` (TID1's kernel stack top, already
stored in `FirstResumeContext.outgoing_stack_top`) to correctly restore TSS RSP0
on switch-back. The `stage119_trampoline_switchback_*` tests were updated to
match the corrected behavior.

**Cleanup markers.** `handle_trap_entry_shared` in `trap_entry.rs` now emits a
cleanup sequence at POINT 2 when `take_pending_done()` succeeds:

- `D6_CONTROLLED_SWITCH_PROOF_CLEANUP_BEGIN` — cleanup phase started
- `D6_CONTROLLED_SWITCH_PROOF_STASH_CLEAR_OK` — both `DISPATCH_SWITCH_PLAN_STASH`
  and `FIRST_RESUME_STASH` verified empty after the proof round-trip
- `D6_CONTROLLED_SWITCH_PROOF_STATE_CLEAR_OK` — `PENDING_DONE` swapped to false,
  `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE` cleared
- `D6_CONTROLLED_SWITCH_PROOF_CURRENT_OK tid=...` — emitted from
  `d6_emit_proof_cleanup_arch_markers` (x86_64 only, inside the re-acquired lock)
- `D6_CONTROLLED_SWITCH_PROOF_CR3_OK asid=...` — active ASID/CR3 logged
- `D6_CONTROLLED_SWITCH_PROOF_TSS_OK` — TSS RSP0 structurally correct after fix
- `D6_CONTROLLED_SWITCH_PROOF_CLEANUP_DONE` — emitted on all arches when proof done

The arch-specific markers (CURRENT_OK, CR3_OK, TSS_OK) are emitted from a new
`KernelState::d6_emit_proof_cleanup_arch_markers()` method (x86_64-gated) added
to `exec_state.rs`, avoiding direct access to the private `hal` field from
`trap_entry.rs`. `CLEANUP_DONE` is emitted unconditionally (all arches) after
the arch block.

**Hard boundaries preserved:** x86_64 proof/default-off path only; no
`switch_frames` ABI change beyond correcting the trampoline stack-top argument,
no scheduler policy change, no timer/preemption change, AP scheduler-online, or
per-CPU runqueue change. No lock handoff, `mem::forget`, assembly unlock callback,
syscall/image-ID/service/IPC/VFS/FS change. AArch64/RISC-V behavior unchanged.
`SYSCALL_COUNT == 31`, `Syscall::VARIANT_COUNT == 23`.

**Tests added.** `src/kernel/boot/tests.rs` gained a `stage130_d6_proof_cleanup`
module (20 tests) covering: trampoline passes `outgoing_stack_top`, no bare `None`
in switch-back args, `FirstResumeContext` field propagation, `CLEANUP_BEGIN` after
`DONE`, `STASH_CLEAR_OK` verifies both stashes, `STATE_CLEAR_OK` verifies both
atomics, `CURRENT_OK`/`CR3_OK`/`TSS_OK` markers from helper, helper is x86_64-only,
`CLEANUP_DONE` emitted unconditionally, CAS-based one-shot enforcement, Stage 129
markers intact, scheduler quantum fix intact, default-off proof, `switch_frames`
ABI unchanged, AArch64/RISC-V untouched, Stage 125 bridge markers intact, D4/syscall
counts, helper emits all three per-lock markers.

### Stage 131 — ArchSwitchContext / switch_frames ABI audit and post-cleanup crash fix

**Status: Outcome A-source (QEMU validation pending user/local run).** Stage 130
reached `D6_CONTROLLED_SWITCH_PROOF_CLEANUP_DONE` but the kernel crashed afterward
at `KernelState::handle_trap` — disassembly showed `leaq 0x3e1780(%r14), %rbx;
callq SpinLockIrqSave`, with `%r14` holding a bad address for the
`scheduler_state` SpinLock.

**ABI audit findings.** `ArchSwitchContext` is `#[repr(C, align(16))]` with
`words: [usize; 8]` at offset 0 and `fxsave: [u8; 512]` at offset 64; total 576
bytes. `yarm_x86_switch_frame` saves and restores rsp at offset 0, rip at 8, rbx
at 16, rbp at 24, r12 at 32, r13 at 40, **r14 at 48**, r15 at 56, and issues
`fxsave`/`fxrstor` at offset 64. All offsets are **correct**. The layout-level
root cause (wrong offset for r14) was **ruled out**.

**Actual root cause: all-zero fxsave area → MXCSR=0.** `initialize_frame_fpu_state`
was NOT called when `initialize_thread_kernel_switch_frame` set up the supervisor
thread's (TID2's) kernel switch frame. The `fxsave` area defaulted to all zeros.
When `switch_frames` switched from TID1 to TID2 for the first time, `fxrstor`
loaded MXCSR=0 — **unmasking all SSE exceptions**. Any subsequent SSE operation
in kernel code (including format-string intrinsics compiled with SSE) raised a
`#XF` (SIMD floating-point exception, vector 19), corrupting the trap sequence and
ultimately producing the observed crash.

**Fix (x86_64 only).** `initialize_thread_kernel_switch_frame` in `thread_state.rs`
now calls `initialize_frame_fpu_state(&mut tcb.kernel_context.frame)` behind a
`#[cfg(target_arch = "x86_64")]` gate after setting the stack pointer and
instruction pointer, but before publishing `initialized = true`. This runs
`fninit; fxsave` to capture a valid FPU state (MXCSR=0x1F80, all exceptions
masked; x87 CW=0x037F) in the frame's `fxsave` area. AArch64/RISC-V paths have
no `fxsave` area and are unaffected.

**Diagnostic markers added** (emitted once per proof run from `maybe_run_d6_controlled_switch_proof` in `exec_state.rs`):

- `D6_SWITCH_CONTEXT_AUDIT_BEGIN` — audit phase started
- `D6_SWITCH_CONTEXT_LAYOUT_OK` — layout verified (offsets correct)
- `D6_SWITCH_CONTEXT_R14_RESTORE_CHECK` — r14 offset 48 confirmed
- `D6_SWITCH_CONTEXT_AUDIT_DONE` — audit complete, root cause found in fxsave area

**Hard boundaries preserved:** x86_64 only fix; no scheduler policy change, no
timer/preemption change, no AP scheduler-online, no per-CPU runqueue change; no
lock handoff, `mem::forget`, assembly unlock callback, syscall/image-ID/IPC/VFS/FS
change. Stage 129/130 markers intact. `SYSCALL_COUNT == 31`, `Syscall::VARIANT_COUNT == 23`.

**Tests added.** `src/kernel/boot/tests.rs` gained a `stage131_arch_switch_context_abi_audit`
module (22 tests) covering: `ArchSwitchContext` size=576 and align=16; words at
offset 0, fxsave at offset 64; assembly offsets for rsp/rip/rbx/rbp/r12-r15 (each
pinned); r14 save offset 48 and restore offset 48; fxsave/fxrstor at offset 64;
`initialize_frame_fpu_state` called in `initialize_thread_kernel_switch_frame` and
is x86_64-gated; `initialize_frame_fpu_state` runs `fninit` then `fxsave`; all four
audit markers in exec_state.rs; Stage 130 CLEANUP_BEGIN/CLEANUP_DONE preserved;
Stage 129/130 structural invariants and ABI boundary (no mem::forget, no AArch64/RISC-V
audit markers).

### Stage 132 — Post-cleanup #PF diagnosis and full-stack mapping fix

**Status: Outcome A-source (QEMU validation pending user/local run).** Stage 131
assumed the post-cleanup crash was `#XF` (vector 19) from MXCSR=0, but the actual
crash token from hardware was `!Fv000000000000000e e0000000000000002`, which is
`#PF` (vector 0x0e = 14), error code 0x2 (kernel write to non-present page).
CR2 = `0xffff80000000d9d8`, which is several kilobytes below the only mapped page
(`0xffff80000000f000`–`0xffff800000010000`).

**Stage 131 correction.** Stage 131's fxsave fix is still correct and necessary
(MXCSR=0 from all-zero fxsave would cause `#XF` on any SSE operation), but that
was not the first crash after CLEANUP_DONE. The immediate crash is a `#PF` on the
kernel stack — the fxsave fix will matter once the stack is fully mapped.

**Root cause: single mapped stack page.** `ensure_kernel_switch_stack_mapped`
(Stage 127) maps only the **top page** of the kernel switch stack — the one
containing `stack_top - 8` (the fake return address slot). After the D6 proof
handoff, TSS RSP0 is set to TID1's `stack_top` (`0xffff800000010000`). The very
first kernel trap after proof completion re-enters `handle_trap` (called from
`KernelState::handle_trap`), which grows the stack approximately 9760 bytes deep
via `SpinLockIrqSave` before any user code runs. At that depth, RSP has descended
well below the single 4 KB mapped page. When `callq SpinLockIrqSave` pushes the
return address to RSP-8, the CPU faults: CR2 = RSP-8 = `0xffff80000000d9d8` —
an unmapped kernel address → `#PF`, error code 0x2 (present=0, write=1, kernel).

**Diagnostic instrumentation.** To capture the exact fault parameters before any
fix is applied:

- `D6_POST_CLEANUP_DIAG_PENDING` — per-CPU `AtomicBool` array in `mod.rs`; set
  to `true` (under `if is_proof_done`) in `trap_entry.rs` immediately after
  `D6_CONTROLLED_SWITCH_PROOF_CLEANUP_DONE` is logged; consumed (swapped to false)
  at the very start of the next `handle_trap_entry_with_fault_bookkeeping_mode`
  entry in `x86_64/trap.rs` (after `ensure_boot_descriptor_tables_scaffolded`).
- `d6_emit_post_cleanup_first_trap_diag(kernel, cpu, context)` — new x86_64-only
  function in `x86_64/trap.rs` (gated on `not(feature = "hosted-dev")`). Captures:
  vector, error code, CR2, derived RSP (= CR2 + 8), kernel pointer (R14 proxy),
  current TID, active ASID, CR3 (as ASID), TSS RSP0 (via new
  `read_boot_tss_rsp0()` accessor in `descriptor_tables.rs`), CR2==RSP-8 flag, and
  a stack classification label.
- `read_boot_tss_rsp0()` — new accessor in `descriptor_tables.rs` that reads
  `YARM_X86_SYSCALL_RSP0` (the atomic mirror of TSS RSP0) with `Acquire` ordering.

**Stack classification labels** emitted by `D6_POST_CLEANUP_FIRST_TRAP_STACK_CLASS`:
- `cr2_below_mapped_stack` — CR2 is in stack bounds but below the single mapped page
- `cr2_inside_mapped_stack` — CR2 is inside the top (mapped) page (unexpected)
- `cr2_below_expected_stack_page` — CR2 is below stack_base entirely
- `rsp_above_expected_stack_top` — RSP is above stack_top (likely wrong TSS RSP0)
- `unknown` — none of the above

**Diagnostic markers** emitted by `d6_emit_post_cleanup_first_trap_diag`:
`D6_POST_CLEANUP_FIRST_TRAP_BEGIN`, `_VECTOR`, `_ERROR`, `_CR2`, `_RIP`, `_RSP`,
`_R14`, `_CURRENT`, `_ASID`, `_CR3`, `_TSS_RSP0`, `_CR2_EQUALS_RSP_MINUS_8`,
`_STACK_CLASS`, `D6_POST_CLEANUP_FIRST_TRAP_DONE`.

**Fix: map all stack pages before the proof switch.**
`d6_ensure_full_proof_switch_stack_mapped(tid)` — new function in `thread_state.rs`
(real version: `#[cfg(all(target_arch = "x86_64", not(test)))]`; stub returns
`Ok(())` under test/non-x86). Called from `maybe_run_d6_controlled_switch_proof`
in `exec_state.rs` for **both** `outgoing_tid` and `incoming_tid`, before
`maybe_switch_kernel_context`. Iterates page-by-page from `stack_base` to
`stack_top` using a `while` loop (no `KERNEL_STACK_REGION_SIZE` reference —
Stage 127/128/129 test invariants preserved). For each page: resolves in target
ASID; allocates a new physical frame if absent (`alloc_user_data_frame`); maps in
target ASID (`PageFlags::KERNEL_RW`); shares in every other currently-live ASID.
On failure, emits `D6_PROOF_FULL_STACK_MAP_FAILED` and returns `Ok(())` (deferred,
no panic).

**Markers emitted** by `d6_ensure_full_proof_switch_stack_mapped`:
`D6_PROOF_FULL_STACK_MAP_BEGIN tid=... base=0x... top=0x...`,
`D6_PROOF_FULL_STACK_MAP_SKIP tid=... va=0x...` (already-mapped pages),
`D6_PROOF_FULL_STACK_MAP_PAGE_MAPPED tid=... va=0x...` (newly mapped),
`D6_PROOF_FULL_STACK_MAP_DONE tid=...`.

**Hard boundaries preserved.** x86_64 proof-mode only; `ensure_kernel_switch_stack_mapped`
unchanged; no `KERNEL_STACK_REGION_SIZE` in new code; no timer/preemption,
AP scheduler-online, per-CPU runqueue, lock handoff, `mem::forget`, assembly unlock
callback, syscall/image-ID/IPC/VFS/FS change; AArch64/RISC-V untouched.
`SYSCALL_COUNT == 31`, `Syscall::VARIANT_COUNT == 23`.

**Tests added.** `src/kernel/boot/tests.rs` gained a `stage132_post_cleanup_pf_diagnosis`
module (20 tests) covering: `D6_POST_CLEANUP_DIAG_PENDING` declared in `mod.rs`
and set after `CLEANUP_DONE`; pending flag consumed via `swap(false)` in x86
trap.rs; diagnostic emitted before `handle_trap_event`; all 12 required `D6_POST_CLEANUP_FIRST_TRAP_*`
markers; all 5 stack-class labels; CR2==RSP-8 and `wrapping_add(8)` derivation;
`read_boot_tss_rsp0` accessor; `d6_ensure_full_proof_switch_stack_mapped` declared;
while-loop iteration without `KERNEL_STACK_REGION_SIZE`; all 4 `D6_PROOF_FULL_STACK_MAP_*`
markers; `PageFlags::KERNEL_RW`; called for both tids before `maybe_switch_kernel_context`;
failure emits `D6_PROOF_FULL_STACK_MAP_FAILED` and returns `Ok(())`; gated on
`is_proof_done`; AArch64/RISC-V untouched; `ensure_kernel_switch_stack_mapped`
unmodified; diag function gated on `not(feature = "hosted-dev")`.

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
Updated Stage 152 (decomposition-completeness audit; ipc_abi.rs boundary
audit landed Stage 151).

**Stage 152 — decomposition is at its irreducible core.** The mechanical D4
decomposition is complete: 10 submodules landed (8 handler + 2
shared-helper/codec), covering every low-risk implementation group named in the
plan (debug, initramfs, control/cap, process, sched, vm). The implementation
that remains in `syscall.rs` is exclusively the dispatch table, the ABI
types/constants, the thin delegation shims, and the IPC/cap cross-boundary
seams. Each remaining seam is forbidden to move by the hard boundary rules
**and** is already pinned in place by an existing source-guard test (Stage 104
pins `materialize_received_message_cap_routed`; Stage 147/148 pin
`try_endpoint_split_recv`, both `try_split_recv_queued_plain_*` seams,
`clear_blocked_recv_state`, `complete_blocked_recv_for_waiter`, and
`materialize_received_message_cap`). No further low-risk extraction is
available, so Stage 152 lands **no new module and moves no source**; it instead
hardens the whole boundary surface with `boot::tests::
stage152_syscall_decomposition_completeness_audit`. `syscall/dispatch.rs` and
`syscall/ipc_recv_core.rs` remain deferred — splitting either would either
violate "syscall.rs remains dispatch owner" / "no submodule may define
dispatch" (dispatch.rs) or require the D1/D5 cap-slot/lock-ordering audit
(ipc_recv_core.rs).

| Target module | Status |
|---------------|--------|
| `syscall/dispatch.rs` | **not planned** — would violate "syscall.rs remains dispatch owner" / "no submodule defines dispatch"; dispatch stays in syscall.rs |
| `syscall/ipc_recv_core.rs` | **landed** Stage 154 (scaffold) + **Stage 155** (all 3 recv-v2 meta encoders converged onto the single pure `encode_recv_v2_meta`, now `pub(crate)`). Pure codec only; the stateful cap/materialization seams and `complete_blocked_recv_for_waiter` remain in `syscall.rs` until a QEMU-validated re-home (§5.1.2/§5.1.3) |
| `syscall/ipc_abi.rs` | **landed** Stage 150; **audited** Stage 151 — pure ABI/frame codec only (no kernel-state mutation, no lock acquisition, no cap-slot materialization, no VM/shared-memory mapping, no reply-cap lifecycle); `syscall.rs` remains dispatch owner; `ipc.rs` remains stateful IPC owner |
| `syscall/helpers.rs` | **landed** Stage 149 ([S] current_tid, validate_user_region, round_up_page, record_user_fault, validate_endpoint_right, current_task_has_user_asid) |
| `syscall/vm.rs` | **landed** Stage 145 (NR 3/13/14 VmMap/AnonMap/Brk) |
| `syscall/ipc.rs` | **landed** Stage 146 (NR 1/2/5/6/7 IpcSend/Recv/RecvTimeout/Call/Reply) |
| `syscall/cap.rs` | **landed** D4 step 4 (TransferRelease / CNode slot control handlers) |
| `syscall/sched.rs` | **landed** D4 step 3 (yield/futex scheduler handlers) |
| `syscall/process.rs` | **landed** D4 step 2 (process-domain spawn/fork handlers) |
| `syscall/initramfs.rs` | **landed** Stage 102 (NR 27/28) |
| `syscall/debug.rs` | **landed** Stage 102 (NR 15) |
| `syscall/recv_shared_v3.rs` | **landed** D4 step 1 (NR 30) |

**Remaining in syscall.rs (Stage 152 audit, classified — irreducible core):**

| Group | Items | Classification |
|-------|-------|----------------|
| [D] dispatch-owned | `Syscall` enum, `SyscallError`, `SYSCALL_COUNT`, ABI constants, thin shims, `pub fn dispatch()` | Must stay in syscall.rs |
| [I] IPC cross-boundary | `complete_blocked_recv_for_waiter`, `clear_blocked_recv_state`, `materialize_received_message_cap` + routing helpers, `try_endpoint_split_recv` | Stay until D1/D5 global-lock-drop phase (5 pure codec fns moved to ipc_abi.rs Stage 150) |
| [R] split-recv seam | `try_split_recv_queued_plain_into_frame_locked` (test), `try_split_recv_queued_plain_with_snapshot_locked` (live) | Stay for D2/D3 split-path protocol |
| [X] future extract, risky | `materialize_received_message_cap` (cap-slot + TrapFrame ordering), `complete_blocked_recv_for_waiter` (same) | Dedicated cap-slot/lock-ordering audit required |

### 5.1.1 Stage 153 — D1/D5 IPC/cap seam ownership/order audit

Stage 153 is a **dedicated audit/proof stage** for the pinned IPC/cap cluster
that a future `syscall/ipc_recv_core.rs` would need to absorb. **No code is
moved.** The mandatory lock-nesting order (doc/KERNEL_LOCKING.md §4) referenced
below is: `scheduler_state` (rank 2) → `task_state` (rank 3) → `ipc_state`
(rank 4) → `capability_state` (rank 5) → `vm_state` (rank 6).

**Per-seam ownership/order proof.** For each function: locks it may touch /
cap-slot mutation / receiver-local cap materialization / reply-cap lifecycle /
blocked-recv state / user-memory copy / scheduler-or-TCB mutation / IPC-lock
coexistence / required before–after ordering / why it stays.

| Seam | Locks | Cap mut | Materializes | Reply-cap | Blocked-recv | User copy | Sched/TCB | Why it stays |
|------|-------|---------|--------------|-----------|--------------|-----------|-----------|--------------|
| `clear_blocked_recv_state` | task (3) | no | no | no | **yes (clear)** | no | TCB field | shared blocked-recv-state owner; pinned stage147 |
| `try_endpoint_split_recv` | ipc (4) | no | no | no | no | no | returns deferred wake plan only | `LIVE_OFF_TRAP` seam; pinned stage147/148 |
| `try_split_recv_queued_plain_into_frame_locked` (test) | cap-read (5), ipc (4) | no | no | no | no | no | no (rejects sender-waiter refill) | Stage 31 regression anchor; pinned stage148 |
| `materialize_received_transfer_cap` (priv) | ipc (4) → capability (5) | **yes (grant)** | yes | no | no | no | cnode/cap tables | cap-mutation helper; hard rule |
| `materialize_received_message_cap` | ipc (4) → capability (5) | **yes (mint/grant)** | yes | **yes (one-shot mint + record)** | no | no | cnode/cap tables | reply-cap one-shot + cap-slot mint ordering; hard rule + stage147/148 |
| `materialize_received_message_cap_routed` (priv) | ipc (4) → capability (5) | **yes (split or canonical)** | yes | **yes (D5 arm)** | no | no | cnode/cap tables + D1/D5 telemetry | D1/D5 router; **Stage 104 guard pins definition + call sites in syscall.rs** |
| `complete_blocked_recv_for_waiter` | task (3) → capability (5) → vm (6) → task (3) | **yes (via router)** | yes | **yes (mint + rollback on meta fault)** | **yes (take→clear)** | **yes (payload + meta)** | zeroes return GPRs (TCB) | cross-domain order-critical; external caller `boot/ipc_state.rs`; hard rule + stage147/148 |
| `try_split_recv_queued_plain_with_snapshot_locked` (live) | ipc (4) → capability (5) → scheduler (2) → vm (6) | **yes (via router)** | yes | **yes (rollback on writeback fault)** | no | **yes (user_plain / v2)** | applies sender wake | ordering-sensitive live split; pinned stage148; calls Stage-104 router |

**Exact ordering invariants (must be preserved by any future move):**

1. `complete_blocked_recv_for_waiter` (recv-v2 blocked-waiter delivery): take
   `blocked_recv_state` (task 3) → resolve recv cap (capability 5) → **copy
   payload to user (vm 6)** → `materialize_received_message_cap_routed` (cap
   mint/grant + reply-cap record) → encode recv-v2 meta → **copy meta to user
   (vm 6)**; on meta-copy fault **roll back the freshly-minted cap** (capability
   5) to avoid a cnode-slot / reply-cap leak → zero the four x86_64 return-GPR
   slots (task 3) → clear state. Payload copy precedes materialization here.
2. `try_split_recv_queued_plain_with_snapshot_locked` (queued split-recv) uses
   the **opposite** payload/materialize order, matching the full-path §58
   sequence: dequeue under ipc (4, released inside `recv_core`) → **materialize
   cap first** (capability 5) → apply sender wake (scheduler 2) → user writeback
   (vm 6) → roll back cap on writeback fault. The two delivery paths therefore
   encode *different* but individually load-bearing orderings; they cannot be
   collapsed into one core routine without preserving both.
3. Reply caps are **one-shot**: `materialize_received_message_cap` mints the
   Reply object directly (bypassing the delegation-link table) and records the
   minted `CapId` via `set_reply_cap_waiter_cap`; `ipc_reply` later fast-revokes
   exactly that slot. Any move must keep mint-then-record atomic w.r.t. the
   delivery that exposes the cap to the receiver.
4. The D1/D5 router runs the `cap_transfer_split` phase-separated engine for the
   transfer (D1) and reply (D5) arms and falls back to the canonical helper for
   shared-region (`OPCODE_SHARED_MEM`) and every `FallbackRequired` outcome;
   failure log markers are byte-identical to the canonical arms (smoke-log
   contract).

**Current blockers for `syscall/ipc_recv_core.rs`:**

- **B1 (guard pin).** `materialize_received_message_cap_routed` is pinned to
  `syscall.rs` by the Stage 104 guard (`stage104_live_wire_call_sites_present`),
  which asserts both its definition and ≥3 occurrences of the call live in
  `syscall.rs`. Relocating the router would break that guard.
- **B2 (cap/reply lifecycle).** The cluster performs capability-slot mutation
  (`mint_capability_in_cnode`, `grant_task_to_task_with_rights`) and the reply-
  cap one-shot lifecycle (`set_reply_cap_waiter_cap`, `rollback_materialized_recv_cap`).
  The hard rules forbid relocating cap/CNode mutation helpers except in a
  dedicated, audited cap-boundary stage.
- **B3 (external caller + order).** `complete_blocked_recv_for_waiter` is
  `pub(crate)` and called from `boot/ipc_state.rs`; it interleaves task → cap →
  vm → task domains in a fault-rollback-safe order that must not be re-sequenced.
- **B4 (no pure helper).** The only genuinely pure fragment in the cluster is the
  recv-v2 metadata byte-encoding (a function of opcode, payload length, cap id,
  flags, sender tid → `[u8; IPC_RECV_META_V2_ENCODED_LEN]`). Its natural home is
  the pure-codec module `ipc_abi.rs`, but the Stage 151 purity guard
  (`stage151_recv_meta_len_stays_in_syscall_rs`) forbids referencing
  `IPC_RECV_META_V2_ENCODED_LEN` there, and inlining the literal `40` would
  duplicate the ABI constant. So even the pure fragment has no safe new home
  today; Stage 153 extracts nothing.

**What a future move would require (preconditions, in order):**

1. A dedicated **cap-boundary stage** that relocates the Stage 104 router and its
   guard together (update `stage104_live_wire_call_sites_present` to target the
   new module), proving the split engine's equivalence tests still hold.
2. A home for the recv-v2 meta codec that does not duplicate
   `IPC_RECV_META_V2_ENCODED_LEN` — e.g. a new `ipc_recv_core.rs` that *owns* the
   const (moved out of `syscall.rs`) with the Stage 147/151/152 guards updated in
   the same change, or a const re-export contract that keeps a single definition.
3. Re-pointing the `boot/ipc_state.rs` and `runtime.rs` external call sites and
   re-homing the Stage 147/148/152 pins to the new module.
4. Bare-metal + QEMU smoke validation that the recv-v2 / reply-cap / split-recv
   delivery markers are byte-identical before and after.

**What must remain in `syscall.rs` until then:** all eight seams above, the
`IPC_RECV_META_V2_ENCODED_LEN` constant, the reply-cap one-shot record/rollback
calls, the D1/D5 router, and `pub fn dispatch`. Stage 153 hardens these with
`boot::tests::stage153_ipc_cap_boundary_audit`.

### 5.1.2 Stage 154 — D1/D5 cap-boundary migration scaffold (Option 2)

Stage 154 begins the dedicated cap-boundary migration toward
`syscall/ipc_recv_core.rs`. **Chosen outcome: Option 2 — pure-helper move.** It
creates the landing module and migrates the single genuinely pure fragment of
the recv cluster; it does **not** re-home any stateful cap/materialization seam.

**Seam migration classification** (per the Stage 153 proof):

| Seam | Stage 154 class | Disposition |
|------|-----------------|-------------|
| recv-v2 meta byte-encoder | (4) pure helper split | **Moved** → `ipc_recv_core::encode_recv_v2_meta` |
| `clear_blocked_recv_state` | (3) must remain | pinned in syscall.rs |
| `try_endpoint_split_recv` | (3) must remain | pinned (LIVE_OFF_TRAP seam) |
| `try_split_recv_queued_plain_into_frame_locked` | (3) must remain | Stage 31 regression anchor |
| `try_split_recv_queued_plain_with_snapshot_locked` | (5) until QEMU smoke | live split; cap+wake+copy ordering |
| `materialize_received_transfer_cap` | (2) move only with guard re-home | cap-mutation helper |
| `materialize_received_message_cap` | (2) move only with guard re-home | cap mint/grant + reply-cap |
| `materialize_received_message_cap_routed` | (2) move only with guard re-home | Stage 104-pinned D1/D5 router |
| `complete_blocked_recv_for_waiter` | (5) until QEMU smoke | external caller + task→cap→vm→task order |
| `IPC_RECV_META_V2_ENCODED_LEN` | (3) single definition stays | referenced from `ipc_recv_core` via `super::` |

**What moved:** `encode_recv_v2_meta(opcode, payload_len, cap_id, recv_meta_flags,
sender_tid) -> [u8; IPC_RECV_META_V2_ENCODED_LEN]`. It is a pure byte codec — no
kernel state, no lock, no cap mutation, no reply-cap lifecycle, no user-memory
copy, no VM mutation — and is byte-for-byte identical to the prior inline
encoding. (The parallel inline encoders in `syscall/ipc.rs` and
`kernel/recv_core.rs` are intentionally left untouched this stage; converging
them onto this single definition is a future step.)

**How the Stage 153 ordering proofs remain true:** the encoder is invoked at the
identical point of the blocked-waiter path — after `materialize_received_message_cap_routed`
and after the payload copy, immediately before the meta `copy_to_user` — so the
copy-before-materialize-then-meta sequence, the rollback-on-meta-fault, the
return-GPR zeroing, and the blocked-state clear are all unchanged. The encoder
has no side effects, so it cannot perturb any lock, cap, or copy ordering. The
queued-split path, the cap router, and the reply-cap lifecycle are not touched.

**Why Option 3 (full re-home) was NOT chosen:** the cap/materialization cluster
is classified (5) "must remain until QEMU smoke proves behavior." QEMU
(`qemu-system-*`) is unavailable in this environment, so the byte-identical
recv-v2 / reply-cap / split-recv delivery markers cannot be smoke-validated here.
Re-homing the Stage 104-pinned router and the order-critical delivery functions
without that proof would violate the Stage 153 finding. Those seams stay pinned.

**Roadmap — future D1/D5 unlock (Stage 155+ candidate), in order:**

1. ~~Converge the `ipc.rs` and `recv_core.rs` inline recv-v2 encoders onto
   `ipc_recv_core::encode_recv_v2_meta`.~~ **Done in Stage 155** (pure-codec
   convergence; byte-identity proven by unit + delivery tests — see §5.1.3).
2. Re-home the Stage 104 D1/D5 router + the `materialize_*` trio into
   `ipc_recv_core.rs`, moving `IPC_RECV_META_V2_ENCODED_LEN`'s single definition
   with them and updating the Stage 104/147/148/152/153 guards to enforce the new
   ownership (not weaken it); re-point `boot/ipc_state.rs` and `runtime.rs`.
3. Re-home `complete_blocked_recv_for_waiter` and the live split path last, each
   gated on a QEMU smoke proving the recv-v2 / reply-cap / split-recv markers are
   byte-identical before and after.

Stage 154 hardens the current boundary with
`boot::tests::stage154_ipc_recv_core_boundary`.

### 5.1.3 Stage 155 — recv-v2 meta codec convergence (pure-codec only)

Stage 155 converges **every** production recv-v2 metadata encoder onto the single
pure helper `ipc_recv_core::encode_recv_v2_meta`. **This is a pure-codec
unification only — no stateful IPC/cap code is moved, no cap/reply/transfer/
materialization logic is re-homed, and `complete_blocked_recv_for_waiter` stays
in `syscall.rs`.**

**Encoders found and converged (3 production sites):**

| Path | File (pre-Stage-155) | Disposition |
|------|----------------------|-------------|
| blocked-waiter recv-v2 delivery | `syscall.rs` `complete_blocked_recv_for_waiter` | already on helper (Stage 154); call updated to 7-arg form |
| immediate full-recv recv-v2 | `syscall/ipc.rs` `handle_ipc_recv_result_with_empty_error` | inline encoder replaced by `super::ipc_recv_core::encode_recv_v2_meta(...)` |
| queued user-ASID split recv-v2 | `kernel/recv_core.rs` `execute_user_asid_plain_v2_writeback` | inline encoder replaced by `crate::kernel::syscall::ipc_recv_core::encode_recv_v2_meta(...)` |

The other `[0u8; 40]` arrays in the tree are unrelated (an aarch64 FDT
descriptor and a test wire buffer), and `recv_shared_v3.rs` uses a different
metadata format (NR 30), so neither is a recv-v2 encoder.

**Byte-identity preserved despite historical per-path divergence.** The three
encoders shared the identical *offset* layout but disagreed on two *values*:
`meta[0..8]` (status word: blocked-waiter wrote `0`; the immediate and queued
paths wrote the sender/status word) and `meta[10..12]` (msg-flags word:
blocked-waiter wrote `0`; the other two wrote `msg.flags`). To converge without
changing any path's bytes, those two fields became explicit parameters
(`status`, `msg_flags`); each call site passes exactly what it wrote before, so
every path is byte-for-byte identical. A unit test
(`encode_recv_v2_meta_reproduces_per_path_bytes`) plus the existing recv-v2
delivery integration tests prove this.

**Visibility.** `encode_recv_v2_meta` was widened `pub(super)` → `pub(crate)` and
the module to `pub(crate) mod ipc_recv_core` because `kernel/recv_core.rs` lives
outside the `syscall` subtree and is a genuine cross-module caller. It is **not**
bare `pub`; `boot::tests::stage155_recv_v2_codec_convergence` guards this.

**ABI constant single-ownership.** `IPC_RECV_META_V2_ENCODED_LEN` keeps its
single definition in `syscall.rs`; `ipc_recv_core.rs` only references it via
`use super::`. `recv_core.rs` retains its pre-existing `META_V2_MIN_LEN = 40`
length-gate constant (used by recv eligibility checks, unrelated to and not a
duplicate of the encoder's length); Stage 155 does not touch it.

**Ordering proofs (Stage 153/154) remain true.** The helper is pure and has no
side effects, so swapping each inline encoder for a call at the identical point
cannot perturb any lock, cap, copy, wake, rollback, or blocked-state ordering.
The blocked-waiter copy-before-materialize sequence, the queued-split
materialize-before-copy + sender-wake + writeback sequence, and the
rollback-on-fault rules are unchanged.

**Roadmap unchanged for the next cap-boundary move.** Re-homing the Stage 104
D1/D5 router, the `materialize_*` trio, `complete_blocked_recv_for_waiter`, and
the live split path into `ipc_recv_core.rs` still requires a QEMU smoke proof
that the recv-v2 / reply-cap / split-recv delivery markers are byte-identical
before and after (unavailable in the current environment). Until then those
stateful seams stay pinned in `syscall.rs`.

Stage 155 hardens this with `boot::tests::stage155_recv_v2_codec_convergence`.

### 5.1.4 Stage 156 — IPC recv/reply/transfer/split smoke oracle (QEMU-gated)

Stage 156 prepares the next cap-boundary migration **smoke-oracle-first**: it adds
a byte-identical delivery oracle that must pass before *and* after any future
stateful re-home of the `materialize_*` / `complete_blocked_recv_for_waiter`
cluster into `ipc_recv_core.rs`. **QEMU (`qemu-system-*`) was unavailable in the
authoring environment, so no stateful seam was moved (Option A).**

**Oracle markers (additive `yarm_log!` only; no behavior/ordering change).** Seven
named markers anchor the load-bearing delivery points proven order-critical in
Stage 153:

| Marker | Site | Proves |
|--------|------|--------|
| `IPC_RECV_V2_META_BLOCKED_WAITER_OK` | `syscall.rs` `complete_blocked_recv_for_waiter` | blocked-waiter recv-v2 meta delivered |
| `IPC_RECV_V2_META_IMMEDIATE_OK` | `syscall/ipc.rs` immediate full-recv | immediate recv-v2 meta delivered |
| `IPC_RECV_V2_META_QUEUED_SPLIT_OK` | `syscall.rs` queued split writeback | queued-split recv-v2 meta delivered |
| `IPC_REPLY_CAP_ONESHOT_OK` | reply-cap mint + waiter-cap record | reply-cap one-shot creation/record for exact-slot fast-revoke |
| `IPC_TRANSFER_CAP_MATERIALIZE_OK` | `materialize_received_transfer_cap` | transfer-cap grant materialized into receiver |
| `IPC_RECV_V2_ROLLBACK_OK` | every recv-v2 rollback site (blocked/immediate/queued) | freshly-minted cap rolled back on meta/payload-copy fault |
| `IPC_RECV_V2_SENDER_WAKE_ORDER_OK` | queued split, right after `apply_split_sender_wake_plan` | sender wake applied BEFORE user writeback |

**Smoke script.** `scripts/qemu-ipc-recv-v2-oracle-smoke.sh [x86_64|aarch64|riscv64]`
delegates the boot to the existing per-arch core smoke (which itself warns+skips
when QEMU/artifacts are missing), then greps the boot log for the oracle markers.
It (a) fails if a fatal IPC marker appears (`IPC_RECV_CAP_MATERIALIZE_FAILED`,
`IPC_RECV_BLOCKED_COMPLETE_FAILED`, `IPC_RECV_REPLY_CAP_MATERIALIZE_FAIL`),
(b) fails if no recv-v2 meta delivery marker appears at all, and (c) writes a
marker-set snapshot (`ipc-oracle-markers-$ARCH.txt`). With `ORACLE_BASELINE=<snapshot>`
it fails on any baseline marker that regressed — this is the byte-identical
proof gate for a future re-home: snapshot before, diff after.

**Why the full cap-boundary re-home remains QEMU-gated.** The Stage 153 proof
showed the cluster's two delivery paths have distinct, load-bearing
copy/materialize/wake/rollback orderings, and reply-cap one-shot consumption is
observable only at runtime. Hosted lib tests cover byte-layout and many delivery
behaviours, but they do not exercise the full multi-server PM↔VFS reply/transfer
cycles on real trap/CR3 paths. Moving the stateful cluster therefore requires a
QEMU environment to record the oracle marker set before the move and confirm it
is byte-identical after. Until then the seams stay pinned in `syscall.rs`.

**Roadmap — next cap-boundary move (Stage 157+, QEMU-equipped environment):**

1. Run `qemu-ipc-recv-v2-oracle-smoke.sh` for x86_64/aarch64/riscv64; save each
   `ipc-oracle-markers-$ARCH.txt` as the baseline.
2. Move the smallest stateful unit first — the Stage 104 D1/D5 router
   (`materialize_received_message_cap_routed`) plus its direct dependencies
   (`materialize_received_message_cap`, `materialize_received_transfer_cap`) —
   into `ipc_recv_core.rs`, carrying `IPC_RECV_META_V2_ENCODED_LEN`'s single
   definition and updating the Stage 104/147/148/152/153 guards to enforce the
   new ownership (re-home, do not weaken); re-point `boot/ipc_state.rs` /
   `runtime.rs`.
3. Re-run the oracle with `ORACLE_BASELINE=...`; require a byte-identical marker
   set on all arches.
4. Only then move `complete_blocked_recv_for_waiter` and the live split path,
   each behind the same baseline gate.

Stage 156 hardens this with `boot::tests::stage156_ipc_smoke_oracle`.

### 5.1.5 Stage 157 — IPC oracle live-path coverage + extended mode

Stage 156 placed `IPC_REPLY_CAP_ONESHOT_OK` and `IPC_TRANSFER_CAP_MATERIALIZE_OK`
**only in the canonical** `materialize_received_message_cap` /
`materialize_received_transfer_cap` arms (`syscall.rs:717`, `syscall.rs:586`).
But every real boot delivers reply and transfer caps through the **live D1/D5
split engine** in `materialize_received_message_cap_routed`, whose split arms
`return Ok(..)` *before* the canonical fallback is ever reached
(`syscall.rs:789–847`). The init control-plane spawn workload alone proves this:
each `spawn_v5_cap` issues an `ipc_call` carrying a reply cap (→ D5 split reply
materialize) and delegates send caps into the child (→ D1 split transfer
materialize). So on QEMU the two cap-delivery markers never fired — **not for
lack of a workload, but because the markers were on the dead fallback arm.**

**Fix (additive `yarm_log!` only; no behavior/ordering change).** Stage 157 emits
the *same two markers* on the live split arms, co-located with the existing
`YARM_D1_SPLIT_MATERIALIZE` / `YARM_D5_SPLIT_MATERIALIZE` markers:

| Marker | Live site (new) | Canonical site (Stage 156) |
|--------|-----------------|----------------------------|
| `IPC_TRANSFER_CAP_MATERIALIZE_OK` | `materialize_received_message_cap_routed` D1 arm (`syscall.rs:804`) | `materialize_received_transfer_cap` (`syscall.rs:586`) |
| `IPC_REPLY_CAP_ONESHOT_OK` | `materialize_received_message_cap_routed` D5 arm (`syscall.rs:841`) | `materialize_received_message_cap` reply arm (`syscall.rs:717`) |

Each marker now appears on **both** arms, making the oracle *path-agnostic*: it
fires whether the live split engine or the canonical fallback services the
delivery. This needs **no userspace exercise client** — the existing init spawn
cycles are the workload; they just lacked instrumentation on the path they take.

**Extended oracle mode.** `scripts/qemu-ipc-recv-v2-oracle-smoke.sh` gains an
`ORACLE_MODE` switch:

* `basic` (default) — unchanged Stage 156 contract: ≥1 recv-v2 meta delivery
  marker required; reply/transfer/rollback/wake only recorded.
* `extended` — additionally hard-requires `IPC_REPLY_CAP_ONESHOT_OK` **and**
  `IPC_TRANSFER_CAP_MATERIALIZE_OK`, both now proven by the live spawn workload.

`IPC_RECV_V2_ROLLBACK_OK` (a recv-v2 user-copy **fault** path) and
`IPC_RECV_V2_SENDER_WAKE_ORDER_OK` (contention-dependent) stay **recorded-only**:
a healthy boot must not fault and need not contend, so requiring them would be
incorrect. They remain covered by the hosted seam tests; deterministically
triggering them on QEMU is left to a future fault/contention workload.

Stage 157 hardens this with `boot::tests::stage157_ipc_oracle_live_path` (live
split arms emit both markers, both arms carry each marker, extended mode requires
exactly the two cap-delivery markers, fault/contention markers are *not*
promoted to required, and the basic-mode default is preserved).

### 5.1.6 Stage 158 — cap-materialization trio re-home (QEMU-validated)

The Stage 156/157 oracle was run on real hardware/emulation and recorded:

* **x86_64** — `ORACLE_MODE=extended` **PASS**: all three recv-v2 meta markers
  present, plus `IPC_REPLY_CAP_ONESHOT_OK` and `IPC_TRANSFER_CAP_MATERIALIZE_OK`.
* **AArch64** (manual) — present: `IPC_RECV_V2_META_BLOCKED_WAITER_OK`,
  `IPC_RECV_V2_META_IMMEDIATE_OK`, `IPC_REPLY_CAP_ONESHOT_OK`,
  `IPC_TRANSFER_CAP_MATERIALIZE_OK`. **Missing**: `IPC_RECV_V2_META_QUEUED_SPLIT_OK`
  (queued-split delivery was not exercised on this manual run).

**Accepted interpretation.** AArch64 validates the D1/D5
materialization/router markers; it does **not** validate queued-split delivery
on this run. Therefore Stage 158 re-homes **only** the byte-identical-proven
cap-materialization cluster and leaves queued-split code untouched.

**Moved into `syscall/ipc_recv_core.rs`** (re-exported from `syscall.rs` via
`pub(crate) use self::ipc_recv_core::{materialize_received_message_cap,
materialize_received_message_cap_routed}` so every call site and sibling
`super::` import resolves unchanged; behaviour and all log markers are
byte-identical to the pre-move code):

* `materialize_received_message_cap_routed` — the D1/D5 split router
* `materialize_received_message_cap` — canonical reply/transfer materializer
* `materialize_received_transfer_cap` — module-private transfer helper

**Explicitly NOT moved (queued-split delivery cluster, stays in `syscall.rs`):**
`complete_blocked_recv_for_waiter`, `try_endpoint_split_recv`,
`try_split_recv_queued_plain_with_snapshot_locked`,
`try_split_recv_queued_plain_into_frame_locked`, `clear_blocked_recv_state`, and
the queued-split writeback/delivery code — none has a cross-arch byte-identical
proof (AArch64 did not exercise `IPC_RECV_V2_META_QUEUED_SPLIT_OK`).

**Guard re-homing (re-home, do not weaken).** The Stage 147/148/152/153/154/155/
156/157 and Stage 104 guards that previously pinned the trio to `syscall.rs` were
updated to enforce the new ownership: the trio must now be defined in
`ipc_recv_core.rs` (router + canonical entry points `pub(crate)`, transfer helper
module-private) and re-exported from `syscall.rs`; the queued-split cluster must
remain defined in `syscall.rs` and must NOT appear in `ipc_recv_core.rs`. The
`ipc_recv_core.rs` purity guards now permit the cap-materialization calls
(`mint_capability_in_cnode`, `grant_task_to_task_with_rights`,
`set_reply_cap_waiter_cap`) it legitimately owns, while still forbidding the
delivery concerns that stayed (`copy_to_user`, `map_shared_region`,
`rollback_materialized_recv_cap`, `ipc_state_lock`).

**Re-validation requested after Stage 158** (`ORACLE_MODE=extended` on x86_64;
manual on AArch64) — at minimum `IPC_RECV_V2_META_BLOCKED_WAITER_OK`,
`IPC_RECV_V2_META_IMMEDIATE_OK`, `IPC_REPLY_CAP_ONESHOT_OK`,
`IPC_TRANSFER_CAP_MATERIALIZE_OK`. Queued split remains recorded-only for AArch64
until a deterministic queued-split workload exists.

### 5.1.7 Stage 159A — `yarm.ipc_recv_proof` knob foundation (accepted)

Stage 159A landed and was **accepted** the arch-neutral, default-off boot knob
`yarm.ipc_recv_proof=1`, mirroring the `yarm.d6_switch_proof` plumbing:
`BootOptions.ipc_recv_proof` parse → `apply_boot_option_knobs` →
`kernel::boot::{set_ipc_recv_oracle_proof_enabled, ipc_recv_oracle_proof_enabled}`.
When off (the default) it provisions nothing and runs nothing; normal boot is
byte-identical.

Validated for 159A: x86_64 extended oracle PASS; AArch64 boot with the knob
PASS; AArch64 service + reply/transfer markers present; only non-fatal
`BLOCKED_WOULDBLOCK_CLASSIFY ... nonfatal=true` in the fatal grep (normal
blocking-IPC classification). Accepted markers: `IPC_RECV_V2_META_BLOCKED_WAITER_OK`,
`IPC_RECV_V2_META_IMMEDIATE_OK`, `IPC_REPLY_CAP_ONESHOT_OK`,
`IPC_TRANSFER_CAP_MATERIALIZE_OK`.

### 5.1.8 Stage 159BC/D — userspace IPC recv-v2 oracle workload (workload/oracle only)

Goal: deterministically drive the three still-missing recv-v2 delivery markers
— `IPC_RECV_V2_META_QUEUED_SPLIT_OK` (notably absent on AArch64),
`IPC_RECV_V2_ROLLBACK_OK`, `IPC_RECV_V2_SENDER_WAKE_ORDER_OK` — using **only** a
real userspace workload. **No IPC/cap code moved**; this is workload + oracle
coverage. All five hard-ruled stateful seams (`complete_blocked_recv_for_waiter`,
`try_endpoint_split_recv`, `try_split_recv_queued_plain_with_snapshot_locked`,
`try_split_recv_queued_plain_into_frame_locked`, `clear_blocked_recv_state`)
stay exactly where Stage 158 pinned them; SYSCALL_COUNT stays 31 and
`Syscall::VARIANT_COUNT` stays 23 (no ABI change).

**Production endpoint constraint.** Userspace cannot mint endpoints — there is
no create-endpoint syscall; every endpoint is minted by the kernel and its caps
delivered through the spawn / `ControlPlaneSetCnodeSlots` cap-delegation
protocol. So the workload cannot conjure its own channel. The
architecture-native solution: the kernel bootstrap, **gated by the knob**, mints
one loopback endpoint and grants the init server (TID 1) **both** a SEND and a
RECV cap to it (`provision_init_ipc_recv_proof_loopback` in
`src/kernel/boot/mod.rs`, called from all three arch first-user bootstraps). The
caps land in init's otherwise-unused startup slots 6/7 (`init_alert_send_ep` /
`init_alert_recv_ep` — init never receives an alert endpoint in the bootstrap
today, so reusing them needs no slot/ABI change). Their joint presence is the
userspace gate: a normal boot leaves both zero and init runs byte-identically.

**Why a loopback (single process).** Holding both caps in one process makes the
queued-split and rollback subtests fully deterministic with one thread, no
timing race: a send-to-self enqueues (no receiver is blocked), then a
recv-from-self drains the queued message through the kernel queued-split
delivery path. `run_ipc_recv_proof_workload` in the init service runs after all
service spawns, before init parks.

**Implemented subtests** (emit a userspace `*_DONE` marker; the kernel emits the
real delivery marker):

* **Queued split** — enqueue a plain message, drain with a normal recv-v2 →
  kernel `IPC_RECV_V2_META_QUEUED_SPLIT_OK`; workload
  `IPC_RECV_PROOF_QUEUED_SPLIT_DONE`.
* **Rollback** — enqueue a cap-bearing message (carrying a transferable cap),
  drain with a deliberately undersized payload buffer
  (`yarm_user_rt::syscall::ipc_recv_v2_proof_undersized`). The kernel
  materializes the carried cap, finds the payload buffer too small
  (`RecvV2WritebackOutcome::PayloadUndersized`), and rolls the cap back →
  `IPC_RECV_V2_ROLLBACK_OK site=queued_split_undersize`; workload
  `IPC_RECV_PROOF_ROLLBACK_DONE`. The undersize trigger is used (rather than a
  bad meta pointer) precisely because it is deterministic and needs no
  unmapped-address guess.

**Deferred subtest (not faked).**

* **Sender-wake** — `IPC_RECV_V2_SENDER_WAKE_ORDER_OK` fires only when a sender
  is *blocked* in `ipc_send` (full-queue or rendezvous) at the instant the
  receiver drains. That requires a second execution context whose blocked state
  cannot be observed/sequenced from userspace without a timing race — the exact
  thing this stage forbids. It is left unimplemented: the workload logs
  `IPC_RECV_PROOF_SENDER_WAKE_DEFERRED` and never emits a `*_DONE` marker, and no
  `SpawnThread` user-rt wrapper was added. A future deterministic
  implementation (a minimal user-thread blocked sender with an observable
  ready-then-block protocol) can lift it.

**Oracle script.** `scripts/qemu-ipc-recv-v2-oracle-smoke.sh` is unchanged in
basic mode. Three independent, default-off proof requirements were added —
`YARM_IPC_RECV_PROOF_QUEUED_SPLIT`, `YARM_IPC_RECV_PROOF_ROLLBACK`,
`YARM_IPC_RECV_PROOF_SENDER_WAKE` — each enforced only when set, and only passing
when **both** the userspace `*_SEQUENCE_DONE` marker and the kernel marker are
present. The script reports each as required/pass/missing and reports sender-wake
as deferred. The sender-wake knob exists but will fail by design until the
deferred subtest lands (do not enable it before then).

#### 5.1.8.1 Fix pass (after first validation)

First QEMU validation surfaced two defects, both now fixed:

1. **x86_64 workload never ran.** The oracle delegated to the per-arch core
   smoke, which never appended the boot knob, so x86_64 booted without
   `yarm.ipc_recv_proof=1`. **Fix:** when any proof requirement env var is set the
   oracle now exports `IPC_RECV_PROOF=1`, and both the x86_64 and AArch64 core
   smokes append `yarm.ipc_recv_proof=1` to the kernel cmdline (mirroring the
   `D6_SWITCH_PROOF` plumbing). A guard
   (`stage159bcd_proof_env_implies_boot_knob`) pins this.

2. **DONE markers were dishonest.** On AArch64 the workload ran and emitted
   `*_DONE` even though no kernel delivery marker fired — because the markers were
   emitted unconditionally after the syscall returned. Root cause: the
   `IPC_RECV_V2_META_QUEUED_SPLIT_OK` / queued-split `IPC_RECV_V2_ROLLBACK_OK`
   markers are emitted **only** by the trap-entry split fast-path
   (`try_split_recv_queued_plain_with_snapshot_locked`). When that path falls
   back, the recv is serviced by the global-lock `handle_ipc_recv`, which delivers
   the queued message via the *immediate* path (`IPC_RECV_V2_META_IMMEDIATE_OK`)
   and the undersized recv does not hit the queued-split rollback site. The
   workload cannot observe which kernel path delivered, so a `DONE` after the call
   returns proves nothing.

   **Fix (honesty + diagnostics):**
   * The userspace markers are renamed to `*_SEQUENCE_DONE` and emitted **only**
     on the observed expected outcome — queued-split only inside the `Ok(Some(_))`
     delivered arm, rollback only on the expected `Err` return.
   * The oracle requires the kernel delivery marker **separately** (and primarily);
     a sequence marker alone cannot pass a requirement.
   * Per-phase diagnostics now bracket every operation with return/value codes
     (`IPC_RECV_PROOF_{QS,ROLLBACK}_{SEND,RECV}_{BEGIN,RET}`, `code=`,
     `payload_len=`, `sender_tid=`) so the next run pins exactly where a subtest
     diverges. To see *why* the split path was taken or skipped, grep the
     kernel-side `YARM_RECV_CORE_PLAN` / `YARM_RECV_CORE_ADAPTER` /
     `YARM_RECV_CORE_FALLBACK` / `YARM_LOCK_SPLIT_IPC_RECV` markers between the
     `*_RECV_BEGIN` and `*_RECV_RET` lines.

   Guards `stage159bcd_sequence_markers_are_conditional` and (updated)
   `stage159bcd_target_markers_are_kernel_emitted` pin the conditional emission.

**Open item carried forward.** The queued-split and queued-split-rollback kernel
markers both depend on the recv being serviced by the trap-entry split path. On
x86_64 normal boots already exercise that path (the marker appears), so the fixed
knob plumbing is expected to make the workload reproduce both on x86_64. On
AArch64 the split path has not been observed to deliver a queued recv (the
pre-existing Stage 158 observation), so the kernel markers may remain absent even
with the workload running — that would be a property of the AArch64 split-recv
path, **not** a workload defect, and is intentionally *not* "fixed" here by moving
any IPC/cap seam. The next run's phase diagnostics + `YARM_RECV_CORE_*` markers
will confirm whether the AArch64 split path runs and, if it falls back, the exact
`FallbackReason`; remediation of that (if desired) is a separate, seam-touching
effort outside this workload/oracle stage.

**Validation in-repo:** `cargo fmt`, `cargo check --features hosted-dev`,
`cargo test --lib --features hosted-dev` (incl. the `stage159bcd_*` guards),
`cargo test --test rpi5_stage1_scope`, `git diff --check`, and x86_64 / aarch64 /
riscv64 bare-metal bootstrap builds all pass. QEMU is run by the maintainer:
boot each arch with `yarm.ipc_recv_proof=1` and run the oracle with
`YARM_IPC_RECV_PROOF_QUEUED_SPLIT=1 YARM_IPC_RECV_PROOF_ROLLBACK=1`.

#### 5.1.8.2 Fix pass #2 (after second validation)

Second QEMU validation: x86_64 queued-split **passed**, and the rollback reached
the real kernel rollback path (`YARM_RECV_CORE_V2_WRITEBACK result=payload_undersized`
→ `IPC_RECV_MATERIALIZE_ROLLBACK kind=transfer ok=true` →
`IPC_RECV_V2_ROLLBACK_OK site=queued_split_undersize`). Two issues remained.

**A — x86_64 split rollback error became a fatal trap (fixed).** After the
correct rollback, the recv returned `SyscallError::InvalidArgs` (the undersized
writeback) as `Some(Err(TrapHandleError::Syscall(InvalidArgs)))` from the
trap-entry split fast path. `handle_trap_entry_shared` returned that `Err`
straight to the arch entry, and **all three arch entries treat an
`Err(TrapHandleError)` as a fatal kernel halt** — so an expected, user-visible
syscall error became a fatal trap dump (`YARM_LOCK_SPLIT_DISPATCH nr=2 result=err`
followed by the dump). The global-lock path never has this problem because
`KernelState::handle_trap` (`boot/fault_state.rs`) encodes normal `SyscallError`s
into the trap frame via `set_err(e.code())` and returns `Ok`.

  **Fix (arch-neutral, no seam moved):** `handle_trap_entry_shared` now matches the
  split-dispatch outcome and, for `Err(TrapHandleError::Syscall(e))`, encodes
  `e.code()` into the frame and returns `Ok` (logging
  `YARM_LOCK_SPLIT_DISPATCH nr=… result=handled_err code=…`) — exactly the
  global-path principle. Genuinely fatal variants (`MissingTrapFrame`) still
  propagate. PageFault is encoded as an error code (conservative, non-fatal); the
  global path keeps the genuine task-fault semantics. This is a syscall-error
  *parity* fix in the trap-entry layer; no IPC/cap seam, no materialization or
  queued-split code, and no D6/CR3/TSS/PF path was touched. Guard:
  `stage159bcd_split_dispatch_syscall_error_is_not_fatal`. Expected result: the
  cap still rolls back, `IPC_RECV_V2_ROLLBACK_OK` still fires, the trap returns
  normally, the userspace wrapper observes the error, and the workload emits
  `IPC_RECV_PROOF_ROLLBACK_SEQUENCE_DONE`.

**B — AArch64 falls back to legacy_full_path (diagnosed, not reworked).** On
AArch64 the proof recv logs `YARM_RECV_CORE_ADAPTER kind=legacy_full_path
is_kernel_task=false` (emitted by the global `handle_ipc_recv`, `syscall/ipc.rs`)
— i.e. the trap-entry user-ASID split recv fast path
(`try_split_recv_queued_plain_with_snapshot_locked`, which would log
`kind=user_plain_v2`) returned `None` and the recv fell through to the
global-lock path. Because that path delivers via the *immediate* route, neither
`IPC_RECV_V2_META_QUEUED_SPLIT_OK` nor the queued-split rollback site fires. This
is a **separate AArch64 split-recv routing/parity issue**, not a workload defect,
and is **not** addressed in this workload/oracle stage (it would require touching
the split-recv routing, which is out of scope here). To localize it on a run,
grep — between `IPC_RECV_PROOF_QS_RECV_BEGIN` and `IPC_RECV_PROOF_QS_RECV_RET` —
for `YARM_LOCK_SPLIT_IPC_RECV nr=2 phase=cap_plan` (did the snapshot resolve?)
and `YARM_RECV_CORE_PLAN` (did the snapshot adapter run?). Their absence pins the
fallback to the pre-snapshot dispatch (e.g. the authoritative current-TID read or
snapshot resolution); the correct future work is an **AArch64 split-recv
fast-path routing/parity stage**.

**C — Oracle acceptance is now arch-aware.** The userspace `*_SEQUENCE_DONE`
marker is always required (the workload ran and observed the expected return).
The kernel delivery marker is REQUIRED on x86_64 (`PROOF_KERNEL_REQUIRED=1`) and
recorded-but-DEFERRED on AArch64/riscv64 (`=0`): its absence there is reported as
`DEFERRED` (neither pass nor failure) and its presence as `PASS`. AArch64
queued-split is therefore **never** reported as a pass unless
`IPC_RECV_V2_META_QUEUED_SPLIT_OK` actually appears. Guard:
`stage159bcd_oracle_acceptance_is_arch_aware`. Sender-wake remains deferred.

### 5.1.9 Stage 160 — AArch64 split-recv fast-path routing/parity

**x86_64 Stage 159BC/D is accepted** (third validation): queued-split and rollback
proofs pass, the rollback `InvalidArgs` is handled as a normal syscall error
(`result=handled_err`), and there is no fatal trap dump.

**AArch64 was deferred** because the proof recv routed through
`YARM_RECV_CORE_ADAPTER kind=legacy_full_path` — the trap-entry user-ASID
queued-split fast path returned `None` and the recv fell to the global-lock
immediate path, which never emits `IPC_RECV_V2_META_QUEUED_SPLIT_OK` /
`IPC_RECV_V2_ROLLBACK_OK`.

**Root cause (CPU-binding parity gap, not arch-specific delivery).** The
trap-entry split recv resolves the requester TID under `with_cpu(cpu)` but then
ran the snapshot dispatch (`try_split_recv_queued_plain_with_snapshot_locked`)
under `SharedKernel::with` — which does **not** bind `current_cpu`. That seam
computes its receiver class from the *ambient* current task
(`current_task_has_user_asid` → `current_tid`, read off `current_cpu`), exactly
as the global-lock path does — but the global-lock path always runs under
`with_cpu(cpu)` (`handle_trap_entry_shared`). On a single-CPU boot (the x86_64
smoke runs `-smp 1`) `current_cpu` is always CPU0, so the unbound read happened
to be correct. On a multi-CPU boot (the AArch64 smoke runs `-smp 2`) the unbound
read could observe another CPU's current task → `is_kernel_task = true` →
`plan_recv_core` returns `FallbackRequired(RecvV2MetaUserCopy)` (a kernel task
cannot take a V2-meta user copy) → `None` → global `legacy_full_path`.

**Fix (smallest parity change; no seam moved).** In
`SharedKernel::try_split_ipc_recv_queued_plain_into_frame` (`src/runtime.rs`) the
snapshot dispatch now runs under `with_cpu(cpu, …)` instead of the unbound
`with`, so `current_cpu` is bound to the trapping CPU for the receiver-class
read — identical to the global-lock path. This touches only the runtime dispatch
layer: the pinned delivery seam
(`try_split_recv_queued_plain_with_snapshot_locked`) is byte-identical and stays
in `syscall.rs`; no materialization or queued-split delivery code moved; no
syscall/IPC ABI change (`SYSCALL_COUNT == 31`, `VARIANT_COUNT == 23`); RPi5 boot
and the x86_64 D6/CR3/TSS/PF paths are untouched; the global-lock fallback is
unchanged. x86_64 (`-smp 1`) is behaviourally unchanged (binding CPU0 is a no-op
there), so the x86_64 proof oracle stays green.

**Diagnostics.** `YARM_SPLIT_RECV_PROBE step={enter,tid,snapshot,bind_cpu,outcome}`
now brackets each decision point in the split-recv method, so a boot log pins the
exact step if any residual fallback remains. Guards:
`stage160_both_arches_share_trap_dispatch_hook`,
`stage160_split_recv_binds_current_cpu`, `stage160_fallback_diagnostics_exist`,
`stage160_no_stateful_seam_moved`, `stage160_no_rpi5_coupling_counts_unchanged`.

**Expected after Stage 160.** On AArch64 the proof recv should now resolve the
user-ASID receiver class correctly and take the queued-split path, emitting
`IPC_RECV_V2_META_QUEUED_SPLIT_OK` and `IPC_RECV_V2_ROLLBACK_OK` alongside the
proof sequence markers. If any fallback remains, the `YARM_SPLIT_RECV_PROBE`
trail plus `YARM_RECV_CORE_PLAN` / `YARM_RECV_CORE_FALLBACK` identify the exact
blocker. (QEMU is run by the maintainer.)

#### 5.1.9.1 Stage 160B — AArch64 recv split-dispatch routing audit (diagnostic)

The Stage 160 CPU-binding fix did not change AArch64: the runtime logs showed the
proof recv never reaching `try_split_ipc_recv_queued_plain_into_frame` at all (no
`YARM_SPLIT_RECV_PROBE`), going straight to the global `legacy_full_path`. The
failure is **above** that helper, in `try_split_dispatch_into_frame` / the trap
routing into it.

**Root cause (frame ABI import ordering).** `TrapFrame` carries the syscall ABI in
three separate places: `syscall_num`, `args[]`, and `user_gprs[]`. The
arch-neutral split dispatcher decides eligibility from the *decoded*
`frame.syscall_num()` / `frame.arg()`. On x86_64 the trap stub fills those before
the shared dispatch, so the split path sees the real NR. On AArch64 the vector
handler builds the trap_frame with **only** `set_user_gpr` (x0–x30); the decoded
`syscall_num`/`args` are populated by `import_syscall_abi_from_user_gprs`
(x8→`syscall_num`, x0–x5→`args`), which runs at `arch/aarch64/trap.rs:246` inside
the **global** handler — i.e. *after* the split dispatch. So when
`try_split_dispatch_into_frame` runs on AArch64, `frame.syscall_num()` is still
`0`; it decodes as `Yield`, the NR gate rejects it, and every recv falls through
to the global path (which then imports the ABI and dispatches `IpcRecv` →
`legacy_full_path`).

**Diagnostics added (proof-knob–gated, arch-neutral).**
`YARM_SPLIT_DISPATCH_ENTER nr=…`, `YARM_SPLIT_DISPATCH_FALLBACK reason={nr_undecodable,nr_not_eligible} nr=…`,
`YARM_SPLIT_DISPATCH_RECV_CONSIDER nr=…`, `YARM_SPLIT_DISPATCH_RECV_CALL`. On the
same proof boot these show the contrast directly: x86_64 logs `ENTER nr=2 →
RECV_CONSIDER → RECV_CALL → YARM_SPLIT_RECV_PROBE step=enter`, while AArch64 logs
`ENTER nr=0 → FALLBACK reason=nr_not_eligible nr=0`. Gated behind
`ipc_recv_oracle_proof_enabled()` so normal/fast boots are unchanged. Guards:
`stage160b_routing_diagnostics_exist`, `stage160b_diagnostics_gated_by_proof_knob`,
`stage160b_no_seam_moved_and_abi_helpers_intact`, `stage160b_counts_unchanged`.

**Why this is NOT fixed in this pass (deferred to a dedicated arch-integration
stage).** Making the AArch64 split path actually service the recv is not a narrow
change: the split path returns early from `handle_trap_entry_shared`, bypassing
not only the ABI **import** (before) but also the result **export**
(`export_syscall_result_to_user_gprs`, ret lanes → user GPRs — AArch64 returns
results via user GPRs, unlike x86_64 where the ret lanes are the return registers)
and the **SVC PC-advance** (`needs_plus4`, `arch/aarch64/trap.rs:272-293`), both of
which currently run only inside the global handler. Enabling the split path on
AArch64 without those would route real recvs through a path that never returns
results or advances past the `SVC`, risking corrupted IPC / an `SVC` re-execution
loop — and it cannot be validated here (no QEMU). Per the stage's own fallback
clause ("if not obvious and narrow, leave it diagnostic-only and report"), the fix
is scoped as a follow-up: bracket the shared dispatch on AArch64 with import
(before) and export + PC-advance (after) so the split path participates in the
full syscall ABI exactly as the global path does. x86_64 stays green and untouched;
RPi5 boot and the global-lock fallback are untouched.

#### 5.1.9.2 Stage 160C — AArch64 trap-ABI bracketing for split dispatch

Implements the follow-up scoped in 5.1.9.1: bracket the pre-global-lock split
dispatch with the AArch64 syscall ABI so split-eligible syscalls are both
*entered* and *returned* correctly.

**Import (before split).** `handle_trap_entry_shared` now calls an arch hook
`pre_split_import_syscall_abi(frame)` immediately before
`try_split_dispatch_into_frame`. On AArch64 it runs `split_import_syscall_abi`,
which reuses the existing `import_syscall_abi_from_user_gprs` (x8→`syscall_num`,
x0–x5→`args`) and logs `AARCH64_SPLIT_ABI_IMPORT_DONE nr=…`. The split dispatch
now sees the real NR (e.g. `nr=2`) instead of `0`.

**Export + SVC-advance (after a handled split).** Both the `Ok` and the
handled-error arms of the split-dispatch match call
`finalize_split_handled_syscall(shared, cpu, frame)`. On AArch64 it runs
`split_finalize_handled_syscall` under `with_cpu`, mirroring the global
non-task-switched syscall-return path: set the resume PC to
`last_vector_raw_elr() + 4` (the **same** formula the proven global
`IpcRecv`-success path uses), `set_thread_user_context`,
`restore_arch_thread_state(syscall_return = true)`, then
`export_syscall_result_to_user_gprs`. It always advances +4 because the split
path returns `Some` ONLY for a *completed* syscall (success or a definitive error
such as the rollback `InvalidArgs`); `WouldBlock` (the only retry case) returns
`None` and stays on the global path with its own retry-PC policy. Diagnostics:
`AARCH64_SPLIT_SVC_ADVANCE_DONE pc=…`, `AARCH64_SPLIT_ABI_EXPORT_DONE`.

**Fallback path unchanged.** When the split dispatch returns `None`, the syscall
falls through to the unchanged global path (which re-imports the ABI
idempotently, dispatches, exports, and applies its own PC policy). No finalize
runs on the fallback path.

**Gated for safe incremental validation.** Both hooks are gated behind the IPC
recv oracle proof knob (`ipc_recv_oracle_proof_enabled()`). With the knob OFF
(every normal boot), the import is skipped, so the AArch64 split dispatch keeps
seeing `syscall_num=0` and falls back exactly as before — **normal AArch64 boots
are byte-identical**, eliminating the risk of routing real recvs through the
newly-enabled path before it is QEMU-validated. With the knob ON (the oracle
proof boot) the AArch64 split path is fully active. x86_64 / riscv64 hooks are
compile-time no-ops: x86_64 already populates the decoded ABI and returns via the
ret lanes, and riscv64 does not enter `handle_trap_entry_shared`. Un-gating for
general AArch64 split-dispatch is a follow-up once the maintainer confirms the
proof boot.

**Constraints.** No IPC/cap seam moved (the fix is purely in the trap-entry/arch
layer; it reuses the existing AArch64 import/export helpers); no ABI change
(`SYSCALL_COUNT == 31`, `VARIANT_COUNT == 23`); RPi5 boot untouched; the x86_64 D6
proof hook in `trap_entry.rs` is intact and x86_64 behavior is unchanged. Guards:
`stage160c_imports_abi_before_split_dispatch`,
`stage160c_exports_and_advances_on_handled_split`,
`stage160c_bracketing_gated_by_proof_knob`,
`stage160c_non_aarch64_hooks_are_noops`,
`stage160c_no_seam_moved_counts_and_x86_intact`.

**Expected after Stage 160C (AArch64 proof boot).** `YARM_SPLIT_DISPATCH_ENTER
nr=2 → RECV_CONSIDER → RECV_CALL → YARM_SPLIT_RECV_PROBE step=enter →
YARM_RECV_CORE_PLAN plan=UserPlainV2Eligible → YARM_RECV_CORE_ADAPTER
kind=user_plain_v2 → IPC_RECV_V2_META_QUEUED_SPLIT_OK`, then for rollback
`IPC_RECV_V2_ROLLBACK_OK`, with `AARCH64_SPLIT_ABI_IMPORT_DONE` /
`AARCH64_SPLIT_SVC_ADVANCE_DONE` / `AARCH64_SPLIT_ABI_EXPORT_DONE` bracketing each,
and the userspace `IPC_RECV_PROOF_QUEUED_SPLIT_SEQUENCE_DONE` /
`IPC_RECV_PROOF_ROLLBACK_SEQUENCE_DONE`. (QEMU is run by the maintainer.)

#### 5.1.9.3 Stage 160D — AArch64 split handled-error export parity

Stage 160C made the AArch64 split path fire both kernel markers
(`IPC_RECV_V2_META_QUEUED_SPLIT_OK`, `IPC_RECV_V2_ROLLBACK_OK`) with no fatal
trap, and the queued-split sequence completed. The rollback userspace completion
still failed: kernel `…result=handled_err code=2`, but userspace logged
`IPC_RECV_PROOF_ROLLBACK_RECV_RET code=0` and `…ROLLBACK_SEQUENCE_DONE` was
missing.

**Audit (Task A/B) — the export ordering was already correct.** The global
AArch64 non-task-switched syscall-return order is context-save
(`set_thread_user_context`) → `restore_arch_thread_state` →
`export_syscall_result_to_user_gprs`; `restore_arch_thread_state` /
`apply_user_context` only restore GPR/PC/SP and do **not** touch the error lane,
so the export still writes the `set_err` error code to x0. The split finalize
already mirrors that order exactly. Decisive evidence it was not an export bug:
the **global** AArch64 path returned `code=0` for the same rollback recv too (the
Stage 160 pre-split run).

**Real root cause — the AArch64 recv-v2 error heuristic, not the export.** The
recv-v2 writeback is meta-first: it copies the 40-byte meta (with
`status = sender_tid`) and only *then* detects the undersized payload and rolls
back. So `meta.status` is no longer `u64::MAX` on the rollback. The proof
undersize wrapper's AArch64 detection was `ret0 != 0 && meta.status == u64::MAX`
— the second clause is false once the meta has been written, so the wrapper
returned `Ok` (`code=0`) even though x0 carried the error. x86_64 is immune
because it reads a dedicated `ret.error` lane, not the meta heuristic.

**Fix (Task C).** AArch64/riscv64 have no separate error lane; the kernel encodes
the failure into x0 via `set_err` + the Stage 160C export, and a successful
recv-v2 sets x0 = 0. So for this proof-only undersize recv a **non-zero x0 IS the
error**: `ipc_recv_v2_proof_undersized` now detects it with `if ret.ret0 != 0`
(dropping the invalid `meta.status` clause). This is a userspace-helper
interpretation change only — no syscall/IPC ABI change, and the general
`ipc_recv_v2` wrapper (which needs the `meta.status` heuristic to separate
WouldBlock from a delivered message) is untouched. The export ordering is kept
(mirrors global) and proven by diagnostics.

**Diagnostics (Task D).** `split_finalize_handled_syscall` now logs
`AARCH64_SPLIT_CONTEXT_SAVE_DONE x0=…`, `AARCH64_SPLIT_SVC_ADVANCE_DONE pc=…`,
`AARCH64_SPLIT_ABI_EXPORT_BEGIN err=… x0_before=…`, and
`AARCH64_SPLIT_ABI_EXPORT_DONE err=… x0_after=…`. On the rollback, `x0_after`
must be `0x2` (InvalidArgs), proving the kernel export is correct and that the
prior `code=0` came solely from the userspace heuristic.

**Constraints.** No IPC/cap seam moved (the v2 meta-first writeback is the pinned
delivery seam and is left untouched — the fix is in the userspace wrapper +
arch-layer diagnostics); no ABI change (`SYSCALL_COUNT == 31`,
`VARIANT_COUNT == 23`); RPi5 boot untouched; x86_64 D6/CR3/TSS/PF intact and the
x86_64 oracle stays green; the AArch64 split bracketing remains proof-knob-gated.
Guards: `stage160d_split_finalize_mirrors_global_export_order`,
`stage160d_handled_error_export_diagnostics`,
`stage160d_proof_wrapper_detects_error_from_x0`,
`stage160d_svc_advance_exactly_once`, `stage160d_invariants`.

**Expected after Stage 160D (AArch64 proof boot).** `…result=handled_err code=2`,
`AARCH64_SPLIT_ABI_EXPORT_DONE err=2 x0_after=0x2`,
`IPC_RECV_PROOF_ROLLBACK_RECV_RET code=2` (nonzero), and
`IPC_RECV_PROOF_ROLLBACK_SEQUENCE_DONE` present — alongside the queued-split
markers and with no fatal trap. (QEMU is run by the maintainer.)

#### 5.1.9.4 Stage 161 — deterministic sender-wake oracle proof (DEFERRED, not faked)

**Stage 160D accepted:** the cross-arch queued-split + rollback proof is complete.
x86_64 and AArch64 both prove `IPC_RECV_V2_META_QUEUED_SPLIT_OK` +
`IPC_RECV_PROOF_QUEUED_SPLIT_SEQUENCE_DONE` and `IPC_RECV_V2_ROLLBACK_OK` +
`IPC_RECV_PROOF_ROLLBACK_SEQUENCE_DONE`, with no fatal trap. `IPC_RECV_V2_SENDER_WAKE_ORDER_OK`
is the only remaining oracle marker.

**Trigger requirement.** `IPC_RECV_V2_SENDER_WAKE_ORDER_OK` fires only when
`ipc_try_recv_queued_with_cap_transfer` returns `ReceivedWithSenderWake` — i.e. a
sender is **blocked as a waiter** (queue full + a *timed/blocking* send) at the
instant the receiver drains. The sender must already be in the endpoint
sender-waiter list before the drain.

**Why a pure userspace workload cannot do this deterministically (the Stage 161
blocker).** Stage 161's scope is "workload/oracle coverage only", preferring an
existing spawn pattern over broad thread infrastructure. Within that scope it is
not achievable, for several independent reasons:

* The userspace `ipc_send` is **non-blocking** (`send_timeout_ticks == 0` →
  `WouldBlock` on a full queue, so the sender never becomes a waiter). Creating a
  waiter needs a *timed/blocking* send wrapper, which does not exist.
* There is **no userspace-observable "a sender is a waiter on endpoint E" signal**
  and **no userspace CPU-affinity control**.
* The proof runs **after the secondary CPUs are released**
  (`bootstrap_first_user_task` only *enqueues* init;
  `release_secondary_cpus_after_bootstrap()` runs before the scheduler dispatches
  init), and a spawned thread is placed by `enqueue_balanced` on the least-loaded
  CPU — so on AArch64 `-smp 2` a spawned sender thread runs concurrently on CPU1
  and can drain-race the receiver. Without an observable or affinity pin, the
  receiver cannot deterministically drain *after* the sender has blocked; it can
  only be made to *sometimes* coincide (a timing race), which the stage forbids.

On x86_64 `-smp 1` a single-CPU scheduler hand-off would be deterministic, but the
acceptance requires BOTH arches, and shipping that x86-only path still needs the
unvalidatable timed-send + spawn-thread + stack infrastructure. So sender-wake is
kept **DEFERRED, not faked**: the workload logs
`IPC_RECV_PROOF_SENDER_WAKE_DEFERRED reason=needs_deterministic_blocked_sender_multicpu`
and never emits `IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`; the kernel marker is
never faked. The oracle's `YARM_IPC_RECV_PROOF_SENDER_WAKE=1` requirement remains
default-off and, when set, requires BOTH
`IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE` and `IPC_RECV_V2_SENDER_WAKE_ORDER_OK`
— so it fails by design (sequence marker absent) until the infrastructure lands.
Do NOT enable that knob before then. Queued-split + rollback remain green and
required when their env vars are set. Guards: `stage161_*`.

**Proposed Stage 162 (minimal proof-gated infrastructure for determinism).** Add,
all gated behind `yarm.ipc_recv_proof=1`:

1. a timed blocking-send user-rt wrapper (`ipc_send_with_timeout`) so the sender
   genuinely blocks and becomes a real waiter;
2. a minimal `spawn_thread` user-rt wrapper + a small fixed proof stack;
3. a **proof-gated CPU-affinity pin** so the spawned proof sender thread is
   enqueued on init's CPU (`enqueue_on(cpu)` instead of `enqueue_balanced`),
   giving a single-CPU `init → sender → init` hand-off that is deterministic on
   both `-smp 1` and `-smp 2`.

Then the deterministic sequence is: init fills the loopback endpoint to capacity →
spawns the (CPU-pinned) sender thread → `yield`s so the sender runs and **blocks**
on the full queue (real waiter) → init `recv-v2` drains one →
`ReceivedWithSenderWake` → `IPC_RECV_V2_SENDER_WAKE_ORDER_OK` → init observes the
sender made progress and emits `IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`. The
phase markers requested for Stage 161 (`..._BEGIN`, `..._SETUP_*`,
`..._SENDER_BLOCKED`, `..._RECV_RET`, `..._SENDER_DONE`, `..._SEQUENCE_DONE`)
belong to that workload. This keeps it real (the sender genuinely blocks; the
kernel marker fires on the real wake-order point) and adds no syscall/IPC ABI
change. None of this moves an IPC/cap seam.

#### 5.1.9.5 Stage 162 — sender-wake proof infrastructure (feasibility audit; still DEFERRED)

Stage 162 set out to build the minimal proof-gated infrastructure to make
sender-wake **strictly deterministic** and then prove
`IPC_RECV_V2_SENDER_WAKE_ORDER_OK` + `IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`.
A full feasibility audit of the four parts was done. Three of the four pieces are
buildable; the determinism requirement is the hard blocker, so sender-wake stays
**DEFERRED, not faked**, and queued-split + rollback remain green and untouched.

**Part A — timed/blocking send wrapper: feasible (no blocker).** The kernel reads
the send timeout from `frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1)` (arg slot 4) via
`decode_ipc_send_timeout_ticks` for a user-ASID sender; the public `ipc_send`
zeroes that slot (non-blocking). A proof wrapper that sets slot 4 to a non-zero
timeout routes to `ipc_send_with_deadline`, so the sender genuinely blocks and
becomes a real waiter — reusing the existing ABI with **no syscall/ABI change**.

**Part B — second execution context: feasible but from-scratch.** `SpawnThread`
(NR 11) and `Fork` (NR 12) exist in the kernel; a thread shares the parent cnode
(so it inherits init's proof caps directly) and `Fork` inherits "ordinary
userspace IPC/memory-object caps". **But there is no existing userspace
thread/fork usage anywhere in the tree** — the user-rt thread-bootstrap ABI
(entry trampoline, stack/TLS setup, no-return convention) would be invented from
scratch and is entirely unvalidatable here. A faulty thread bootstrap faults/hangs
the boot.

**Part C/D — deterministic ordering: the hard blocker.** init is already pinned to
`BOOTSTRAP_CPU_ID`, and a proof-gated affinity pin could keep a spawned sender
thread on the same CPU, and `ipc_send` wakes a receiver by marking it runnable
(no immediate `YieldTo` handoff) — all of which favors a single-CPU hand-off.
**However, the timer preempts running user tasks** (`should_preempt` in
`KernelState::handle_trap`), so *any* pure-userspace handshake (a second endpoint,
a futex, or a yield-poll) has a sub-microsecond race window between the sender
signalling "ready" and the sender actually blocking: if the timer fires in that
gap and the receiver drains first, the sender never blocks (the queue is no longer
full) and the marker never fires. That is precisely the "timing race" the stage
forbids. The **only strictly race-free signal is one emitted by the kernel at the
exact `enqueue_sender_waiter` point** for the proof endpoint. Delivering that to
userspace needs either a futex wait-address channel that does not exist
(registering init's wait word with the kernel would be a new mechanism), or
sending/waking from inside the locked sender-waiter-enqueue path — a lock-ordering
hazard in the IPC state code. Both are non-trivial, risky kernel-IPC-path changes
that cannot be validated without QEMU.

**Blast-radius consideration.** The queued-split + rollback proofs run under the
*same* `yarm.ipc_recv_proof=1` knob, so a from-scratch sender-wake workload (or a
risky IPC-path coordination hook) landed blind could destabilize the
currently-green proof boots. The honest, low-risk decision is therefore to keep
sender-wake deferred rather than ship a large, unvalidatable, boot-risking change.

**Proposed Stage 163 (concrete, strictly race-free design).** All proof-gated and
behind a *separate* `yarm.ipc_recv_proof_sender_wake=1` sub-knob so the green
queued-split/rollback proof boots are never affected even if it misbehaves:

1. Provision a second proof endpoint `E2` (signal channel) into init alongside the
   `E1` loopback (the kernel already mints the loopback in
   `provision_init_ipc_recv_proof_loopback`).
2. Add a proof-only timed blocking-send user-rt wrapper (Part A) and a minimal
   proof-only `SpawnThread` wrapper with one small static stack (Part B); pin the
   spawned sender to init's CPU (Part C).
3. **Strict signal:** in the sender-waiter-enqueue path, *only* when
   `ipc_recv_oracle_proof_enabled()` and the endpoint is the proof `E1`, emit
   `IPC_RECV_PROOF_SENDER_WAKE_WAITER_PRESENT endpoint=.. tid=..` and stage a
   deferred wake of init's `E2` recv (computed under the lock, applied after lock
   release — mirroring the existing `IpcSchedulerPlan`/`apply_split_*_wake_plan`
   deferred-wake discipline, so no lock-ordering violation). init does a blocking
   recv on `E2`; it returns exactly when the sender is provably a waiter, with no
   race window.
4. init then `recv-v2` drains `E1` → the real path emits
   `IPC_RECV_V2_SENDER_WAKE_ORDER_OK`; init confirms the sender's message arrived
   (sender_tid match) and emits `IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`.

This keeps the sender genuinely blocked, the kernel marker genuinely emitted at the
real wake-order point (never faked), no IPC/cap seam moved, and no syscall/IPC ABI
change. The oracle's `YARM_IPC_RECV_PROOF_SENDER_WAKE=1` requirement (added in
Stage 161, requiring BOTH `IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE` and
`IPC_RECV_V2_SENDER_WAKE_ORDER_OK`) stays default-off and fails by design until
that lands. Queued-split + rollback remain green and required.

#### 5.1.9.6 Stage 163 — proof-gated deterministic sender-wake oracle (IMPLEMENTED)

Stage 163 lands the design proposed in 5.1.9.5, with two simplifications that make
it strictly *less* risky than the proposal. Sender-wake is now **proven, not
deferred**, and is isolated behind a **separate** sub-knob
`yarm.ipc_recv_proof_sender_wake=1` layered atop `yarm.ipc_recv_proof=1`. With the
sub-knob absent, *nothing* changes: the kernel coordination hook is inert, the
second coordination endpoint `E2` is never provisioned, the sender-wake workload
never runs, and the already-green queued-split + rollback proof boots (which set
only the base knob) are byte-for-byte unchanged.

**Sub-knob (Task A).** `boot_command_line.rs` parses
`yarm.ipc_recv_proof_sender_wake` into `BootOptions.ipc_recv_proof_sender_wake`
(default `None` → off), applied to the `IPC_RECV_PROOF_SENDER_WAKE_ENABLED`
atomic. `boot::ipc_recv_proof_sender_wake_active()` is the AND of the base proof
knob and the sub-knob — the single precondition for any sender-wake behavior. The
parser is verified to not prefix-alias the base knob.

**Timed/blocking send wrapper (Task B).** `yarm_user_rt::syscall::ipc_send_timeout_ticks`
is `ipc_send` with arg slot 4 (`SYSCALL_ARG_INLINE_PAYLOAD1`) set to a non-zero
timeout, routing the kernel to `ipc_send_with_deadline` so the sender genuinely
blocks and becomes a real waiter. This **reuses the existing send ABI — no syscall
or IPC ABI change** (`SYSCALL_COUNT == 31`, `Syscall::VARIANT_COUNT == 23`
unchanged).

**Second execution context (Task C) — Fork, not SpawnThread.** The proposal
suggested a from-scratch `SpawnThread` bootstrap; we chose **`Fork` (NR 12)**
instead because there is no existing userspace thread-bootstrap pattern (entry
trampoline / stack / TLS) to reuse, whereas `Fork` returns child-tid to the parent
and `0` to the child, inheriting init's COW address space and ordinary IPC caps
with no manual stack/TLS setup. `yarm_user_rt::syscall::fork()` wraps NR 12. The
child is the blocked sender; the parent (init) is the receiver. The child parks in
a `yield_now` loop after its send and never re-enters init's flow.

**Proof-gated kernel coordination hook (Task D).** When and only when
`proof_sender_wake_coordination_target(endpoint_idx)` returns `Some(e2_idx)` — i.e.
the sub-knob is active *and* `endpoint_idx` is the provisioned proof `E1` — the
`enqueue_sender_waiter` path calls `proof_sender_wake_push_coordination_locked`,
which pushes a one-byte signal into `E2`'s queue **inside the same `ipc_state_lock`
critical section** that makes the proof sender a waiter on `E1`, and logs
`IPC_RECV_PROOF_SENDER_WAKE_WAITER_PRESENT`. This is even simpler than the proposed
deferred wake: because init **non-blocking-polls** `E2` (rather than blocking-recv),
**no scheduler wake is needed at all**, so the hook does *zero* scheduler / cap /
user-copy work under the lock — it only mutates `E2`'s in-domain message queue.
There is therefore **no lock-order hazard** (the proposed `apply_split_*_wake_plan`
deferred-wake dance is unnecessary). "E2 has the signal" is an atomic proxy for
"the sender is a waiter on E1", with no race window even on SMP — so the timer-
preemption race that blocked Stages 161/162 is closed without any CPU-affinity pin.
The kernel mints `E2` in `provision_init_ipc_recv_proof_sender_wake_e2` (gated on
`ipc_recv_proof_sender_wake_active()`) and grants init a RECEIVE cap, wired into
init startup slot 13 (`service_extra_cap_0`, otherwise unused) identically across
the x86_64 / AArch64 / riscv64 boots, on none of the D6/CR3/TSS/PF or RPi5 paths.

**Deterministic sequence (Task E).** `run_ipc_recv_proof_sender_wake` (init): (1)
fills `E1` to capacity with plain non-blocking sends; (2) forks; (3) the child does
a TIMED blocking send on the full `E1` → becomes a real sender-waiter, triggering
the kernel hook → `E2` signal; (4) init non-blocking-polls `E2` (bounded) until the
signal appears — exactly when the sender is provably a waiter; (5) init `recv-v2`
drains `E1` (NR 2 → trap-entry split path), the real path emits
`IPC_RECV_V2_SENDER_WAKE_ORDER_OK` and refills + wakes the sender; (6) init drains
until it observes the child's own message (`sender_tid == child`) and only then
emits `IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`. ~12 phase markers
(`..._BEGIN/_SETUP/_FILL/_SENDER_*/_RECV_*/_SEQUENCE_DONE`) bracket the run. All
waits are bounded so a missing child (e.g. fork failure) degrades to a logged
give-up (`..._NO_WAITER_SIGNAL` / `..._SENDER_MSG_ABSENT`), never a hang. The
kernel marker is **never faked** from userspace; `SEQUENCE_DONE` is gated on real
observed child progress.

**Oracle (Task F).** `qemu-ipc-recv-v2-oracle-smoke.sh` with
`YARM_IPC_RECV_PROOF_SENDER_WAKE=1` exports both `IPC_RECV_PROOF=1` and
`IPC_RECV_PROOF_SENDER_WAKE=1`, the per-arch core smokes append both boot knobs,
and `proof_require "sender-wake"` requires BOTH the userspace `SEQUENCE_DONE` and
the kernel `SENDER_WAKE_ORDER_OK`. As with queued-split, the kernel marker (emitted
only on the trap-entry split path via `apply_split_sender_wake_plan`) is REQUIRED
on x86_64 and DEFERRED on AArch64/riscv64 (whose proof recv falls back to
`legacy_full_path`) — the existing per-arch `proof_require` policy.

**Isolation invariant.** Queued-split + rollback remain green and required under the
base knob alone; their boots never provision `E2`, never run the sender-wake
workload, and never hit the coordination hook. No IPC/cap stateful seam
(`complete_blocked_recv_for_waiter`, `try_endpoint_split_recv`,
`try_split_recv_queued_plain_*`, `clear_blocked_recv_state`) was moved. Stage 163
guards in `boot/tests.rs::stage163_sender_wake_proven` pin every one of these
properties.

#### 5.1.9.7 Stage 163A — fix sender-wake sequencing + oracle log analysis

Initial Stage 163 validation on QEMU surfaced two defects. Base queued-split +
rollback stayed green on x86_64 and AArch64 throughout.

**Defect 1 — AArch64 fill blocked init (`tid=1`) before the fork.** The boot log
showed `IPC_RECV_PROOF_SENDER_WAKE_WAITER_PRESENT endpoint=6 tid=1` during the FILL
phase, before `FILL_DONE`/fork/`SENDER_START`, and the sequence then stalled. Root
cause: a buffered `IpcSend` on a **full** endpoint *blocks the sender as a waiter
even with a zero timeout* — `ipc_send_with_optional_deadline` has no try-send for a
full buffered queue; the `!queued` branch calls `block_current_on_send_with_deadline`
with `deadline = None`. The Stage 163 fill used a fixed `FILL_MAX = 64` overrun
against an 8-deep endpoint, so init's 9th fill send blocked init itself as a
sender-waiter (firing the coordination hook for init's own TID) and deadlocked,
since init is also the receiver. The user's suggested "non-blocking fill-until-
WouldBlock" is not achievable with the current kernel for exactly this reason. (Why
x86_64 happened to complete is timing-dependent and was not relied upon.)

The fix makes init fill to **exactly** E1's buffered capacity and never one more,
so every fill send succeeds and init never blocks:

- The kernel publishes E1's capacity to init. `boot::IPC_RECV_PROOF_E1_DEPTH` (the
  same const the loopback's `create_endpoint` uses) is written into init startup
  slot 14 (`service_extra_cap_1`, unused by init) whenever the sub-knob provisions
  E2. init reads it as the fill target (defaulting to a safe small value).
- The workload first **drains** any residual E1 messages (the base subtests share
  E1), then fills exactly `capacity` messages with non-blocking `ipc_send`,
  emitting `FILL_SEND_RET idx/code`, `FILL_STOP_FULL`, and `FILL_DONE count`. A
  fill send that ever returns an error is treated as a fill-phase blocker
  (`FILL_UNEXPECTED_BLOCKER tid=..`) and the proof aborts rather than risk blocking
  init. The timed/blocking send (`ipc_send_timeout_ticks`) now appears ONLY in the
  forked child's sender branch — never in fill.
- Only then does init fork; the **child** does the timed blocking send on the full
  E1 and is the sole sender-waiter, so the coordination hook fires for the child's
  TID, not init's.

**Defect 2 — oracle false negative on x86_64.** The oracle's `proof_require`
evaluated an in-memory `present[]` array (a per-marker snapshot) rather than the
live boot log, so a marker truly present in the raw `$CORE_LOG` could be reported
absent. `proof_require` now greps the actual current core-smoke log
(`tr '\r' '\n' < "$CORE_LOG" | rg -a`) for BOTH the sequence and kernel markers,
echoes which log it analyzed plus `have_seq`/`have_kern`, and is no longer coupled
to any snapshot. The per-arch policy is unchanged: x86_64 requires the kernel
marker, AArch64/riscv64 defer it (split recv falls back to `legacy_full_path`).

**Waiter-identity defense (kernel-agnostic).** Because the kernel cannot know the
expected child TID, init verifies it from userspace. The E2 coordination message
carries the waiter's TID (`Message::sender_tid`); init reads it, logs
`WAITER_OBSERVED waiter_tid=.. child_pid=..`, and proceeds ONLY when the waiter is
the forked child. A waiter-present for init (`tid == init_tid`) is rejected as
`WAITER_UNEXPECTED` (and a non-child, non-init waiter as `WAITER_MISMATCH`); neither
completes the sequence. `SEQUENCE_DONE` still requires the full trail — fill-done,
the child's sender-start, the waiter observation, the recv return, and the observed
child message (`sender_tid == child`).

No syscall/IPC ABI change (slot 14 is internal kernel→init bootstrap state, not a
syscall/IPC contract), no IPC/cap seam moved, counts unchanged
(`SYSCALL_COUNT == 31`, `VARIANT_COUNT == 23`), RPi5 boot untouched. New guards in
`boot/tests.rs::stage163_sender_wake_proven` (`stage163a_*`) pin: fill is
non-blocking and never uses a timed send; a waiter-present for init is not accepted
as proof; `SEQUENCE_DONE` requires the full ordered trail; the oracle analyzes the
live log; and the capacity is communicated via slot 14.

#### 5.1.9.8 Stage 163B — single-log oracle + fork ordering/diagnostics

Stage 163A fixed the fill overrun (init no longer blocks during fill — confirmed on
AArch64: `WAITER_PRESENT tid=1` during fill is gone) but validation exposed two
remaining problems. Base queued-split + rollback stayed green on x86_64 and AArch64.

**Defect 1 — the oracle still false-negatived on x86_64, even reading `$CORE_LOG`.**
The initial marker scan found the markers but `proof_require` reported
`have_seq=0 have_kern=0` for the *same* markers (and the same effect hit the
queued-split kernel marker), proving the two checks were not reading the same bytes.
Rather than keep chasing which file `$CORE_LOG` resolved to, Stage 163B makes the
oracle analyze **one** log through **one** helper:

- The delegated core-smoke's combined stdout/stderr is captured explicitly into
  `ipc-oracle-core-stdout-$ARCH.log` via `tee` (with `CORE_STATUS` preserved from
  `PIPESTATUS[0]`). The core-smoke tees the raw QEMU serial to its stdout, so this
  captures exactly the markers the run produced.
- A single `ANALYSIS_LOG=ipc-oracle-run-$ARCH.log` is built as the CR-normalized
  union of that captured output and the raw serial `$CORE_LOG`, so a marker that
  reached either sink is visible to one consistent scan. The analyzed path + byte
  count are printed.
- Every marker check — initial scan, fatal/required/extended, and `proof_require` —
  now goes through one `marker_present "$marker" "$ANALYSIS_LOG"` helper using
  fixed-string `rg -F`. `proof_require` no longer reads a separate file or the
  `present[]` snapshot, so a marker the initial scan saw can never be reported
  absent. A standalone functional check confirms `proof_require sender-wake`
  returns PASS when a log contains both sender-wake markers.

**Defect 2 — AArch64 sender-wake never produced a sender.** After `FILL_DONE`, the
AArch64 log showed init looping on the E2 non-blocking receive with **no child
markers at all** — no `SENDER_START`. The forked child was never observed running,
so the waiter-present signal could never be produced. Stage 163B adds explicit
fork-ordering diagnostics and tightens the contract so the next run pinpoints the
cause rather than presenting an opaque poll loop:

- `fork()` now returns `Option<u64>`: `None` on an ABI-flagged failure (x86_64's
  separate error lane), `Some(0)` in the child, `Some(child_tid)` in the parent. On
  AArch64/riscv64 there is no error lane, so a failure there still returns
  `Some(value)` and the bounded poll + waiter-identity checks catch it.
- The workload emits `FORK_BEGIN`, then `FORK_RET raw=.. role=parent|child|err`
  inside each branch, `CHILD_ENTRY` + `SENDER_START` in the child before the timed
  blocking send, and `PARENT_WAIT_BEGIN child_pid=..` in the parent. The parent
  reaches the E2 poll ONLY through the `Some(child_pid)` arm — it never polls E2
  before fork has returned a parent-side child pid. A `None` (failed) fork emits
  `FORK_FAILED` and returns immediately, never spinning on E2.

This makes the AArch64 failure mode self-describing on the next run: if
`FORK_RET role=child`/`CHILD_ENTRY` are absent while the parent logs
`FORK_RET role=parent`, the child task is created (it is enqueued `Runnable` with
`arg0=0` in `fork_complete_post_clone`) but is not resuming into userspace on
AArch64 — a fork-child first-resume / COW concern to address next, distinct from the
proof workload. The proof does **not** fake `IPC_RECV_V2_SENDER_WAKE_ORDER_OK`:
without an observed child waiter it stops at the appropriate diagnostic marker.

No syscall/IPC ABI change, no IPC/cap seam moved, counts unchanged, RPi5 untouched,
base proofs green. New `stage163b_*` guards pin: the oracle uses one analysis log +
one helper; `fork` reports failure distinctly; the fork is ordered after `FILL_DONE`
and before the E2 wait with full diagnostics; and `SEQUENCE_DONE` requires the full
ordered trail (fork → child-entry/sender-start → waiter-observed → recv-ret →
sender-done).

#### 5.1.9.9 Stage 163C — fork failure audit + diagnostics

Stage 163B's single-log oracle fix was validated, but the sender-wake run then
showed the fork wrapper logging `FORK_RET raw=err role=err` + `FORK_FAILED` with
**no** `role=parent` ever appearing. Only `role=err` with no parent means fork
returned an error to the single init process — a **genuine fork failure before any
child exists**, not a child with a stale return lane. (The earlier "AArch64 child
first-resume" hypothesis was therefore premature: there is no child to resume.)
Notably, Stage 163B's wrapper change — `if ret.error != 0 { return None }` on
x86_64 — is what surfaced this; the Stage 163A wrapper ignored the error lane.

Stage 163C is an audit: it makes the failure self-describing without faking
anything. Base queued-split + rollback stay green; nothing is gated except behind
the sender-wake sub-knob.

**Non-lossy userspace fork diagnostics.** A new `fork_raw()` returns every return
lane (`ret0/ret1/ret2/err/arch`) with no conversion. The workload logs
`FORK_SYSCALL_BEGIN`, `FORK_SYSCALL_RET ret0=.. ret1=.. ret2=.. err=.. arch=..`,
then a decoded `FORK_DECODE code=.. meaning=..` and, on failure,
`FORK_FAILED code=.. meaning=..` (mapping the `SyscallError` discriminant, e.g.
`8 → PageFault`, `6 → QueueFull`, `2 → InvalidArgs`). Role decode is by `ret0`
(`!= 0` → parent; `== 0` with a small known error code → failure; `== 0` with
`err == 0` or a large/stale lane → child), so a future successful child whose
x86_64 error lane is a stale RCX is not misread as a failure.

**Proof-gated kernel fork diagnostics.** Under the sub-knob only, `handle_fork`
emits `FORK_PROOF_ENTER` / `FORK_PROOF_PARENT_RET` / `FORK_PROOF_RETURN_ERR code=..
reason=..`, and the clone path emits step markers — `FORK_PROOF_PRECHECK_OK`,
`FORK_PROOF_COW_BEGIN`/`_FAIL`, `FORK_PROOF_ALLOC_CHILD_BEGIN`/`_OK`/`_FAIL`,
`FORK_PROOF_CNODE_BEGIN`/`_FAIL`, `FORK_PROOF_CHILD_TF_RET0_SET`,
`FORK_PROOF_CHILD_ENQUEUE_BEGIN`/`_OK`/`_FAIL` — so the exact failing step and
`KernelError` reason are visible. Behavior is unchanged; only logging is added, and
nothing fires on a normal boot.

**Clean-state fork smoke.** Before E1 is filled, the workload runs
`run_ipc_recv_proof_fork_smoke` (`FORK_SMOKE_BEGIN` → `FORK_SMOKE_SYSCALL_RET ...` →
`FORK_SMOKE_PARENT` / `FORK_SMOKE_CHILD_ENTRY` / `FORK_SMOKE_FAILED code=..`). If
fork fails here too — with an empty E1 — the full buffer / queued IPC state is ruled
out, isolating the cause from the proof's own setup.

**Audit answers (to be confirmed by the next run's `FORK_PROOF_*` trail).** Fork
(NR 12) is reached (`FORK_PROOF_ENTER` will print). The kernel path is
`handle_fork → fork_user_process_cow` (precheck → `clone_user_address_space_cow`
COW → `fork_complete_post_clone`: `allocate_thread_id` → `register_task_with_class`
→ cnode + `inherit_parent_capabilities_for_fork` → child TCB (`arg0=0`, `Runnable`)
→ `enqueue_task`). Each step now has a marker, so the next run names the failing
step. The decoded `err`/`reason` distinguishes missing-right / invalid-args /
capacity-full / VM-COW / cnode / enqueue. The clean-state smoke answers whether the
full E1 is implicated. The existing hosted fork/COW unit tests exercise a synthetic
path, not this live init-after-bootstrap fork under the post-merge resource state —
which is why the diagnostics run on real hardware/QEMU. The Stage 163B wrapper
decode was the immediate change that exposed the error; Stage 163C makes it fully
faithful. **The fix (Task E) is deferred until the next run names the exact failing
step and code** — no blind broadening of fork semantics, and
`IPC_RECV_V2_SENDER_WAKE_ORDER_OK` is never faked.

No syscall/IPC ABI change, no IPC/cap seam moved, counts unchanged
(`SYSCALL_COUNT == 31`, `VARIANT_COUNT == 23`, fork still NR 12), RPi5 untouched.
New `stage163c_*` guards pin: fork diagnostics are non-lossy and expose the actual
error code; a failure is not collapsed to a bare `raw=err` and the sequence aborts;
the kernel step-level diagnostics exist and are proof-gated; and the clean-state
smoke runs before the fill.

#### 5.1.9.10 Stage 163D — fix fork COW `Vm(Full)`

Stage 163C's diagnostics pinned the failure exactly. On x86_64:

```
FORK_PROOF_ENTER parent_tid=1
FORK_PROOF_PRECHECK_OK parent_tid=1
FORK_PROOF_COW_BEGIN
FORK_PROOF_COW_FAIL reason=Vm(Full)
FORK_PROOF_RETURN_ERR code=255 reason=Vm(Full)
```

and the clean-state smoke (before E1 fill) failed identically — so a full E1 /
queued IPC state is ruled out. The failure is in `clone_user_address_space_cow`
allocating the child address space.

**Cause — address-space budget exhausted after the merge.** Fork worked in Stage
163A (pre-merge) and fails post-merge; the merge expanded the driver_manager service
set, raising the count of live user address spaces to/over the old 32-slot bound.
`clone_user_address_space_cow` calls `create_user_space`, which needs (a) a free
slot in `entries[MAX_ADDRESS_SPACES]`, (b) a page-table root from
`MAX_ASID_ROOTS = MAX_ADDRESS_SPACES * 8`, and (c) page-table pages from
`MAX_PT_PAGES = MAX_ADDRESS_SPACES * (1 + MAX_MAPPINGS * 4)` — **all three derive
from `MAX_ADDRESS_SPACES`**, so a single bound was the binding constraint and a
single knob relieves whichever filled first. (`KernelError::Vm(VmError::Full)` maps
to the generic `SyscallError::Internal` = code 255 — the ABI has no dedicated
"resource-full" lane; that mapping is intentionally left unchanged to avoid an ABI
break, with the kernel-side `reason=Vm(Full)` carrying the detail.)

**Fix — raise `MAX_ADDRESS_SPACES` 32 → 48 (bare-metal, all three arches).** This
gives headroom for the current service set plus a forked child. `hosted-dev` stays
16 (unit-test capacity behavior unchanged). On bare-metal `PageTablePage` is just
`{ phys: u64 }`, so the larger derived pools cost ~190 KiB of static memory — modest.
The CR3 / page-table derivation logic is untouched; only the input bound changed.

**Exhaustion diagnostics (so future regressions self-quantify).** Proof-gated
`clone_user_address_space_cow` now logs `FORK_PROOF_COW_STATS parent_asid=..
vmas_used=.. vmas_cap=..`, `FORK_PROOF_COW_STATS_ASID asid_used=.. asid_cap=..
asid_retired=..`, and per-site `FORK_PROOF_COW_FAIL_DETAIL site=create_user_space|
map_child|map_parent|mark_cow_* used=.. cap=.. reason=..`. `asid_retired` separates a
genuine capacity shortfall from a shootdown-acknowledge leak. `AddressSpaceManager`
gained `live_count`/`slot_capacity`/`retired_count` accessors for this.

**Status.** Sender-wake remains pending until a real run confirms the fork smoke and
the child sender both succeed: the expected next-run trail is
`FORK_SMOKE_SYSCALL_RET ... err=0` → `FORK_SMOKE_PARENT`/`FORK_SMOKE_CHILD_ENTRY`,
then `FILL_DONE` → `FORK_RET role=parent`/`role=child` → `CHILD_ENTRY` →
`SENDER_START` → `WAITER_PRESENT tid=<child>` → `WAITER_OBSERVED` → `RECV_RET` →
`IPC_RECV_V2_SENDER_WAKE_ORDER_OK` → `SEQUENCE_DONE`. If `asid_used` is still at
`asid_cap` after the bump, the diagnostics quantify exactly how much more is needed;
nothing is faked. AArch64 is addressed by the same arch-shared bump; if it then forks
but the child does not resume, that opens a separate AArch64 first-resume stage.

No syscall/IPC ABI change, no IPC/cap seam moved, counts unchanged
(`SYSCALL_COUNT == 31`, `VARIANT_COUNT == 23`, fork still NR 12), RPi5 boot
untouched, x86_64 D6/CR3/TSS/PF logic untouched. New `stage163d_*` guards pin the
diagnostics, the raised bound (with hosted-dev held at 16), the unchanged pool
derivations, and the no-seam/no-count/no-RPi5/no-D6 invariants.

> **Correction (Stage 163E).** The Stage 163D 32→48 bump was a *misdiagnosis*. The
> next run's diagnostics showed `asid_used=11 asid_cap=48 asid_retired=0` — the ASID
> table was never the binding structure. The real `Vm(Full)` was at
> `site=map_parent index=127`, and the failed fork *leaked* the parent table from
> `vmas_used=80` to `128`. Stage 163E reverts the bump and fixes the actual bug.

#### 5.1.9.11 Stage 163E — transactional, run-preserving COW fork clone

Stage 163D's per-site diagnostics localised the failure precisely: the COW clone
fails at `map_parent index=127` with `Vm(Full)`, and — critically — a *failed* fork
leaves the parent mutated (`vmas_used` 80 → 128), so the second fork starts already
full. So the bug was twofold: a non-transactional mutation/leak, AND a table balloon.

**Root cause.** The old `clone_user_address_space_cow` iterated the *live* parent
table and re-mapped each page write-protected. Re-mapping a single page inside a
multi-page run **splits** that run (the map primitive isolates the page), so the
loop then walked the split-off tails and kept splitting — ballooning the parent
table one entry per page until it hit `MAX_MAPPINGS = 128` and failed at
`map_parent`. The only rollback (`restore_parent_write_permissions`) restored write
*permission* but never undid the *splits*, so the parent stayed bloated. This is
`asid_used=11/48` proof that ASIDs were irrelevant — the binding structure was the
per-ASID mapping (VMA) table.

**Fix — snapshot + preflight + in-place write-protect + full rollback.** The COW
fault handler (`try_handle_cow_fault`) already splits a run lazily on the first
write, so eager per-page splitting at clone time is unnecessary. The rewritten clone:

1. **Snapshots** the parent's runs `(head virt, phys, flags, pages)` before any
   mutation and iterates the *snapshot*, never the live table (no runaway).
2. **Preflights**: the child needs at most one entry per parent run (adjacent
   same-flag pages MERGE in the child, never grow) and the parent is write-protected
   in place (entry count unchanged), so the only bindable capacity is the child
   table. If `required_child > MAX_MAPPINGS` it returns `Vm(Full)` **before any
   mutation** — a rejected fork leaves the parent byte-identical.
3. **Maps whole runs into the child** page-by-page (they merge → run-compact).
4. **Write-protects each parent run IN PLACE** via the new
   `AddressSpace::write_protect_run_head_in_place` — clears the run's write flag and
   updates every page's hardware PTE but does **not** split the entry, so the parent
   table never grows. The per-page split happens lazily on the first write.
5. **Records every parent write-protect** and, on any later failure, calls
   `rollback_cow_clone`: destroy the partial child, restore each parent run's flags
   in place, and clear the COW marks — leaving the parent byte-identical.

Because the parent table no longer balloons (init's 80 runs stay 80) and the child
stays run-compact (≤ 80), the fork now fits comfortably in `MAX_MAPPINGS = 128`
**with no capacity bump** — so Stage 163E also **reverts** the Stage 163D
`MAX_ADDRESS_SPACES` 48 → 32 (the well-tested value; `asid_used=11` leaves ample
headroom).

**Diagnostics** (proof-gated): `FORK_PROOF_COW_STATS_BEFORE`,
`FORK_PROOF_COW_PREFLIGHT required_parent/available_parent/required_child/available_child`,
`FORK_PROOF_COW_MAP_PARENT_BEGIN/OK/FAIL`, `FORK_PROOF_COW_ROLLBACK_BEGIN/DONE`, and
`FORK_PROOF_COW_STATS_AFTER_FAIL` (which must show `parent_used` equal to its
pre-clone value). **Regression tests**: `write_protect_run_head_in_place_does_not_
split_or_grow` (data-structure level: a 4-page run stays one entry through
write-protect + restore) and `fork_cow_clone_is_transactional_no_parent_mapping_leak`
(integration level: two successive forks leave the parent entry count unchanged).
All 26 existing COW/fork tests still pass (single-page mappings behave identically —
in-place == old for `pages == 1`).

No syscall/IPC ABI change, no IPC/cap seam moved, counts unchanged
(`SYSCALL_COUNT == 31`, `VARIANT_COUNT == 23`, fork still NR 12), RPi5 boot
untouched, x86_64 D6/CR3/TSS/PF logic untouched. New `stage163e_*` guards pin the
transactional preflight/rollback, the in-place (no-split) parent write-protect, and
the reverted bound; the stale `stage163d` ASID-bump guard was updated to assert the
revert. Sender-wake remains pending a real run, where the expected trail is
`FORK_SMOKE ... err=0` → `FORK_SMOKE_CHILD_ENTRY`, then `FILL_DONE` → `FORK_RET
role=parent/child` → `CHILD_ENTRY` → `SENDER_START` → `WAITER_PRESENT tid=<child>` →
`WAITER_OBSERVED` → `IPC_RECV_V2_SENDER_WAKE_ORDER_OK` → `SEQUENCE_DONE`; nothing is
faked.

#### 5.1.9.12 Stage 163F — VM module audit

Before re-running the Stage 163E QEMU smoke, the VM module was audited (14 claims).
Fixes confined to `vm.rs` (+ tests): `has_mapping_for_phys` now tests whole-run
containment (was base-only); `VirtAddr::checked_add` + documented wrapping `Add`;
checked-arithmetic coalescing (`entry_end_virt` saturates, new
`run_precedes_page`/`page_precedes_run`); `create_user_space` checks a free slot
before allocating an ASID; `is_canonical` + `map_page` rejects non-canonical x86_64
VAs; `DrainedMapping` gained `virt`; `acknowledge_shootdown` debug-asserts a nonzero
bit. Documented/guarded: external-lock contract, `tick_retired_shootdowns`'s
intentional `0`, `drain_mappings` stack array, linear-scan bound (`const` assert
`MAX_ADDRESS_SPACES <= 64`), GUARD cache policy, `Asid` never-zero contract. No
IPC/cap seam moved; counts unchanged; `MAX_ADDRESS_SPACES` stayed 32.

#### 5.1.9.13 Stage 163G — fork-child COW page-fault routing

Stage 163E/163F got the x86_64 sender-wake fork past `Vm(Full)` to a running child
(`tid=10008`, `task_asid=12`, child CR3 active), but the child then looped on a
present/write/user fault (`error=0x7`) at a stack address, the handler logging
`PAGE_FAULT_HANDLED_DEMAND` forever. The fault routing already tries COW before
demand for write faults, so `HANDLED_DEMAND` means `try_handle_cow_fault` declined
(the page was not COW-marked) and demand then mis-handled it. Two real bugs fixed:

1. **Demand masked a present write-protect fault.** `try_handle_demand_page_fault`'s
   `already_mapped` branch did `invalidate_page` + `return Ok(true)` for *any* present
   page in a demand region — including a present **read-only** page faulting on
   **write**, which is a protection/COW fault, not a stale-TLB demand fault. It now
   checks write satisfiability (`!Write || mapping.flags.write`) and **declines**
   (`Ok(false)`) an unsatisfiable write so the fault routes to COW / task-fault
   instead of looping on an unchanged RO PTE.

2. **Re-fork did not propagate COW marks.** Stage 163E only COW-marked the child for
   parent runs that were currently *writable*. But a parent can hold a page
   **read-only because it is COW-shared from an EARLIER fork** (the proof runs a
   clean-state smoke fork before the sender-wake fork). Such an RO-COW page was
   shared with the new child read-only but **not** COW-marked, so the child's first
   write found it present+RO and not-COW → `try_handle_cow_fault` declined → loop.
   The clone now also COW-marks the child for any parent page that is currently
   `is_cow_page` even when its run is read-only (logged `FORK_PROOF_COW_INHERIT_SHARED`),
   keeping the Stage 163E in-place (no-split) write-protect for writable runs.

**Diagnostics (proof-gated, sender-wake sub-knob only):** `PF_PROOF_CLASSIFY` and
`PF_PROOF_LOOKUP_MAPPING` (found/writable/cow/demand/phys) at fault entry, and
`PF_PROOF_COW_CONSIDER`/`_HANDLE_BEGIN`/`_HANDLE_OK`/`_HANDLE_FAIL` in the COW
handler. These pinpoint, on the next run, whether the faulting page is
RO-not-COW-marked (fix 2 applies) or writable-in-software-but-RO-in-hardware (a
distinct page-table-writeback issue for a follow-up). Nothing is faked: the marker
still comes only from the real split path.

**Regression tests:** `fork_refork_propagates_cow_mark_to_grandchild` (a twice-forked
parent's RO-COW page is COW-marked in the grandchild and its write yields a private
writable page) and `demand_declines_present_read_only_write_fault` (demand declines a
present-RO write, still handles a satisfiable read). New `stage163g_*` guards pin the
demand write-check, the proof-gated diagnostics, the inherited-COW propagation, and
the no-seam/no-count/no-RPi5/no-D6 / `MAX_ADDRESS_SPACES==32` invariants.

No syscall/IPC ABI change, no IPC/cap seam moved, counts unchanged
(`SYSCALL_COUNT == 31`, `VARIANT_COUNT == 23`), RPi5 untouched, x86_64
D6/CR3/TSS/PF switch machinery untouched (the fix is confined to page-fault routing
+ COW marking). `MAX_ADDRESS_SPACES` remains 32.

#### 5.1.9.14 Stage 163H — fork-child software-vs-hardware PTE mismatch

Stage 163G's diagnostics on QEMU were decisive. The faulting child stack page was
**not** an RO-COW-inherited page at all — it was a demand-mapped private page:

```
PF_PROOF_LOOKUP_MAPPING ... va=0x7fffffbff000 found=1 writable=1 cow=0 demand=1 phys=0x104dd000
PF_PROOF_COW_CONSIDER  ... reason=not_cow_page
PAGE_FAULT_DEMAND_VERIFY ... task_flags=0x80000000104dd007 active_flags=0x80000007
PAGE_FAULT_HANDLED_DEMAND   (repeats forever)
```

So the page is correctly writable in the **child's own ASID** (`task_flags`:
present/write/user/NX, phys `0x104dd000`), but the **active page table the CPU
actually walks** is a *different* ASID holding a stale, wrong, but **present** entry
(`active_flags=0x80000007`: phys `0x80000000`, no NX). The CPU therefore keeps
walking the wrong table and re-faulting. There is no software/hardware flag mismatch
within the child's ASID — the mismatch is **CR3 vs the child's ASID**: the fork
child was running on a stale/incorrect active page table for this page.

The demand-verify already had a CR3-correction (`switch_address_space(task_asid)`)
"to fix ASID/CR3 if the task's address space differs from what the HAL recorded"
(Stage 137), but it only fired when the active entry was **absent**
(`!active_present`). Here the wrong active entry is *present*, so the correction
never ran and `HANDLED_DEMAND` returned to userspace on the wrong CR3 → loop.

**Fix (minimal, in the page-fault path):** broaden the existing correction to also
fire on a *stale-but-present* mismatch — when the active table is a different ASID
whose PTE flags for the page disagree with the task's correct mapping
(`active_asid != task_asid && active_flags != task_flags`) — then
`switch_address_space(task_asid)` and `invalidate_page` so the CPU re-walks the
child's own table. When active == task the flags match and nothing switches.
`HANDLED_DEMAND` remains gated on the post-correction `hw_demand_ok` hardware walk.

**Diagnostics (Task B):** a fully-decoded `pf_proof_log_hw_pte` helper logs
`PF_PROOF_HW_PTE_BEFORE`/`_AFTER` (real active-CR3 walk: present/writable/user/nx +
raw, alongside the software writable/cow/demand flags), plus `PF_PROOF_DEMAND_
CONSIDER`/`_DECLINE` and `PF_PROOF_DEMAND_SWITCH_CR3` — all proof-gated. These make
any residual SW-vs-HW or CR3 mismatch unambiguous on the next run.

Most of this fix is a hardware-path (real CR3/PTE) change validated by QEMU; the
hosted suite covers the unchanged COW/demand behavior, and `stage163h_*` source
guards pin the broadened switch condition, the decoded diagnostics, and the
preserved Stage 163G decline. No syscall/IPC ABI change, no IPC/cap seam moved,
counts unchanged (`SYSCALL_COUNT == 31`, `VARIANT_COUNT == 23`), RPi5 untouched, the
D6/CR3/TSS *switch* machinery untouched beyond broadening the existing demand-verify
`switch_address_space` correction, Stage 163E transactional/run-preserving COW clone
preserved, `MAX_ADDRESS_SPACES` remains 32.

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
6. **D4 step 1 — `syscall/recv_shared_v3.rs` extraction.** Complete: NR 30
   helpers/handler now live in `src/kernel/syscall/recv_shared_v3.rs`;
   `syscall.rs` keeps the unchanged dispatch arm.

**Concurrent / gated:**

7. **D-NEXT-2 — x86_64 AP per-CPU environment → scheduler-online.**
   Per-CPU GDT/IDT/TSS + GS base + AP-safe printk + `bring_up_cpu(cpu)`,
   behind a default-off knob; then `-smp ≥ 2` smoke acceptance. Still
   high priority — it unblocks per-CPU runqueue lock sharding (D6) and the
   lock-free `await_tlb_shootdown_ack` design (D3) — but must not bypass
   D7-A/D7-B and must not jump ahead of the Next items above without an
   explicit gating review.
8. **D4 mechanical decomposition — COMPLETE (Stage 152).** D4 steps 1–4 plus
   Stage 145/146/149/150/151 landed all 10 submodules
   (`recv_shared_v3.rs`, `process.rs`, `sched.rs`, `cap.rs`, `vm.rs`, `ipc.rs`,
   `helpers.rs`, `ipc_abi.rs`, `debug.rs`, `initramfs.rs`). Stage 152 audits the
   decomposition as complete to its irreducible IPC/cap dispatch core: the only
   implementation left in `syscall.rs` is the dispatch table, ABI types/shims,
   and the IPC/cap cross-boundary seams that the hard rules + existing
   source-guards pin in place. No further low-risk module remains to peel off
   (§5.1).
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
| D4 (`syscall.rs` decomposition) | **complete (mechanical)** | All 10 submodules landed (`debug,initramfs,recv_shared_v3,process,sched,cap,vm,ipc,helpers,ipc_abi`); Stage 152 audits the decomposition as complete to its irreducible IPC/cap dispatch core — what remains in `syscall.rs` is dispatch + cross-boundary seams pinned by the hard rules and existing source-guards (§5.1). |
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
`with_cpu`) still spans it. Stage 117 (§1) added the global-lock-drop stash infrastructure
(`PerCpuSwitchPlanStash`, `DISPATCH_SWITCH_PLAN_STASH`, `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE`)
but landed at Outcome B: `switch_frames` is never called in production because no
task has `kernel_context.initialized = true` (`provision_default_kernel_context` leaves
it `false`; `initialize_thread_kernel_switch_frame` is never called in production).
The proof markers do not appear in smoke; smoke-observable deferred markers
(`D6_GLOBAL_LOCK_DROP_DEFERRED reason=no_outgoing_task` / `reason=no_kernel_ctx_switch_frame`)
prove the trap path reaches the decision point. The next targets, in order:

1. **Stage 117 QEMU smoke acceptance.** Run all four smokes and verify that
   `D6_GLOBAL_LOCK_DROP_DEFERRED reason=no_outgoing_task` and/or
   `reason=no_kernel_ctx_switch_frame` appear in x86_64 and AArch64 logs, and
   `reason=riscv_lockless_trap_path` appears in RISC-V logs. Record results in
   §1 Stage 117 acceptance evidence table.
2. **Stage 117 Outcome A — kernel-thread infrastructure.** Wire
   `initialize_thread_kernel_switch_frame` into the production boot path (requires
   a real `yarm_kernel_thread_switch_trampoline` that handles first-time kernel-side
   resumption). Once any production task has `kernel_context.initialized = true`,
   the stash path will fire and the proof markers will appear in smoke.
3. **D2 blocking-recv genuine seam live-wire and D6 dispatch seam live-wire.**
   The structural blocker (global lock held across `switch_frames`) is resolved for
   kernel threads once Stage 117 Outcome A is achieved. For user tasks (trap-frame
   switching only), the lock drop needs to be wired to `restore_arch_thread_state`
   instead of `switch_frames`.
4. **D4 syscall decomposition — mechanically complete (Stage 152).** All 10
   submodules landed; the decomposition has reached its irreducible IPC/cap
   dispatch core. Any further unlocking here is the D1/D5 cap-slot/lock-ordering
   audit work, not a mechanical module move (§5.1).
5. **D-NEXT-2 — x86_64 AP per-CPU environment → scheduler-online.**
   Per-CPU GDT/IDT/TSS + GS base + AP-safe printk + `bring_up_cpu(cpu)`,
   behind a default-off knob; then `-smp ≥ 2` smoke acceptance. Still
   high priority — it unblocks per-CPU runqueue lock sharding (D6) and
   the lock-free `await_tlb_shootdown_ack` design (D3, full two-phase)
   — but does not bypass items 1–4 above.

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

Stage 117 landed at Outcome B. The stash infrastructure
(`PerCpuSwitchPlanStash`, `DISPATCH_SWITCH_PLAN_STASH`, `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE`)
and IRQ safety argument are correct. The stash path is exercised correctly by
unit tests. The production smoke blocker: all production tasks have
`kernel_context.initialized = false` (set by `provision_default_kernel_context`,
never overridden by `initialize_thread_kernel_switch_frame`), so `switch_frames`
is never called in smoke and the proof markers do not appear.

Smoke-observable deferred markers prove the production trap path reaches the
decision point: `D6_GLOBAL_LOCK_DROP_DEFERRED reason=no_outgoing_task` (IPC
blocking dispatch) and `reason=no_kernel_ctx_switch_frame` (yield-path, different
tasks but uninitialized frames) appear in x86_64 and AArch64 logs.

The Outcome A unlock for Stage 117 requires kernel-thread infrastructure:
`initialize_thread_kernel_switch_frame` must be called in the production boot path,
and `yarm_kernel_thread_switch_trampoline` must be a real function (not a spin
loop) that handles first-time kernel-side resumption after `switch_frames`.

**Exact next Claude prompt recommendation:**

> Kernel unlocking Stage 117 QEMU smoke acceptance and Outcome A upgrade:
> (1) Run all four QEMU smokes and verify `D6_GLOBAL_LOCK_DROP_DEFERRED
> reason=no_outgoing_task` and/or `reason=no_kernel_ctx_switch_frame` appear
> in x86_64/AArch64 logs, and `reason=riscv_lockless_trap_path` appears in
> RISC-V logs; record results in `doc/KERNEL_UNLOCKING.md` §1 Stage 117 table.
> (2) To upgrade Stage 117 to Outcome A: wire `initialize_thread_kernel_switch_frame`
> into the production boot (e.g., for the supervisor tid=1) and implement a real
> `yarm_kernel_thread_switch_trampoline` that acquires the global lock and calls
> `post_switch_restore_arch_thread_state` on its first invocation. The proof
> markers (`D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH`, `D6_SWITCH_FRAMES_ENTER_UNLOCKED`,
> `D6_SWITCH_FRAMES_RETURNED_UNLOCKED`) must appear in smoke before claiming Outcome A.

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
