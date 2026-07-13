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

## 1. Live status (Milestone 1 declared, Milestone 2 Pass 2, Stage 114 D3 live-seam wire, Stage 115 IPC rank-3 seam added, Stage 116 task-lock dropped before switch_frames, Stage 117 global-lock-drop stash scaffold Outcome B, Stage 118 first-resume handler + production switch-frame init Outcome B, Stage 119 minimal task pair + TSS RSP0 fix Outcome B, Stage 120 controlled x86_64 switch proof harness, Stage 121 first-resume ABI diagnostics, Stage 122 first-instruction proof, Stage 123 no pre-Rust marker call, Stage 124 Rust tail-jump stack-shape fix, Stage 125 Rust entry bridge, Stage 126 kernel switch-stack mapping/backing gate, Stage 127 target-ASID stack mapping retry, Stage 128 active-CR3 shared switch-stack coverage, Stage 129 active-root VmFull on-demand repair, Stage 163P cross-arch accepted sender-wake regression oracle)

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
- **`SYSCALL_COUNT = 32`** — do not add or remove syscalls without a new ABI
  stage. NR 31 is `ReapFaultedTask` (PM-only terminal-task reap), added by the
  SUP/PM crash-restart merge; existing NRs 0–30 are unchanged. Per-stage history
  entries below that record `SYSCALL_COUNT == 31` describe the pre-merge baseline
  at those stages and are left as historical record.
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
| `syscall/initramfs.rs` | **landed** Stage 102 (NR 28; NR 27 handler removed in Stage 197A) |
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

#### 5.1.9.15 Stage 163I — writable demand page that still faults: intermediate permission + stale TLB

Stage 163H corrected the CR3-vs-ASID mismatch, but QEMU then showed the child
still looping on the *same* page with the active table now **correct and
unchanged**:

```
PF_PROOF_HW_PTE_BEFORE ... cr3=0x100e1000 raw=0x80000000104dd007 present=1 writable=1 user=1 nx=1
PAGE_FAULT_DEMAND_VERIFY ... (active_asid == task_asid, no switch)
PF_PROOF_HW_PTE_AFTER  ... cr3=0x100e1000 raw=0x80000000104dd007
PAGE_FAULT_HANDLED_DEMAND   (repeats forever)
```

Decoding the evidence (Task A):

- `raw=0x80000000104dd007` = present + writable + user + NX, phys `0x104dd000`.
  The **leaf** PTE is genuinely writable; `cow=0` is correct (a private demand
  stack page, never COW).
- `cr3=0x100e1000` has its low 12 bits clear → **PCIDE is disabled**
  (`arch/x86_64/boot.rs` sets CR4 = PAE|OSFXSR|OSXMMEXCPT + conditional
  SMEP/SMAP, never bit 17). So there is a single TLB namespace and a per-page
  `invlpg` on this CPU *must* clear a stale leaf entry — yet it loops. A stale
  *leaf* TLB entry alone therefore cannot be the whole story.

Two real causes were masked by **leaf-only** checks:

1. **Intermediate permission (root cause).** On x86_64 the effective access
   rights are the logical-AND of the bits in *every* paging-structure entry used
   to translate the address (Intel SDM Vol. 3A §4.6), not just the leaf. A
   writable+user leaf under an intermediate (PML4E/PDPTE/PDE) that lacks USER (or
   WRITABLE) is still inaccessible and faults `present+write+user` (error 0x7)
   forever. `walk_or_create_table` returned an already-present intermediate
   **without upgrading its flags**, so an intermediate first created for a
   stricter mapping permanently gated the permissive leaf. The leaf-only
   `hw_demand_ok` and `pf_proof_log_hw_pte` could not see it.
2. **Stale local-CPU TLB.** A per-page `invlpg` only guarantees the current
   PCID's entry for one address; the recovery escalates to a full architectural
   flush to drop any cached translation the per-page form missed.

**Fix (minimal, in the page-fault/TLB path):**

- `walk_or_create_table` now OR-widens an existing intermediate with the
  USER|WRITABLE bits the requested mapping needs (never narrows, never touches
  huge leaves) — the root-cause fix at map time.
- A new arch entry point `repair_user_path_intermediates(asid, va)` widens any
  already-installed under-permissioned intermediate **in place** (leaf
  untouched), and `flush_tlb_local_full()` forces a full local TLB flush by
  reloading CR3 with architectural-flush semantics (it does not change the active
  root, so it never switches address spaces). Both exist on all three arches so
  the shared handler links; the repair is a real walk only on x86_64 (the
  AND-of-levels architecture), a typed no-op on AArch64/RISC-V where permissions
  live on the leaf.
- The demand "already-present" branch, on a **write** fault, now repairs
  intermediates, issues `invlpg`, then `flush_tlb_local_full()` before returning
  handled. The non-write recovery keeps the cheap per-page `invlpg`.
- `hw_demand_ok` now folds the **effective** permission across the whole walk
  (`eff_present && eff_user && (!need_write || eff_writable)`), so an
  under-permissioned intermediate can never again loop as `HANDLED_DEMAND`; if it
  somehow recurs the fault routes to the task-fault path with the full
  per-level walk logged.

**Diagnostics (Task B):** proof-gated `PF_PROOF_TLB_STALE_CANDIDATE`,
`PF_PROOF_INTERMEDIATE_REPAIR levels_upgraded=N`, `PF_PROOF_INVLPG_BEGIN/DONE`,
`PF_PROOF_CR3_RELOAD_BEGIN/DONE`, and
`PF_PROOF_DEMAND_HANDLE_OK reason=already_writable_after_flush`, plus the
`PAGE_FAULT_POST_DEMAND_HW_PTE_WALK` line extended with `eff_present/eff_user/
eff_writable`. `PF_PROOF_HW_PTE_BEFORE/_AFTER` are retained.

**Expected QEMU sequence (Task E):** for `va=0x7fffffbff000` the recovery fires
**at most once** — `PF_PROOF_TLB_STALE_CANDIDATE` → `PF_PROOF_INTERMEDIATE_REPAIR`
→ `PF_PROOF_INVLPG_BEGIN/DONE` → `PF_PROOF_CR3_RELOAD_BEGIN/DONE` →
`PF_PROOF_DEMAND_HANDLE_OK reason=already_writable_after_flush` →
`PAGE_FAULT_HANDLED_DEMAND` — then the child makes progress to `SENDER_START` →
`WAITER_PRESENT` → `WAITER_OBSERVED` →
`IPC_RECV_V2_SENDER_WAKE_ORDER_OK` →
`IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`.

This is again primarily a hardware-path (real CR3/PTE/TLB) change validated by
QEMU; the hosted suite covers the unchanged COW/demand behavior and the
`stage163i_*` source guards pin the write-recovery escalation, the effective
AND-of-levels gate, the `walk_or_create_table` upgrade, the cross-arch helper
definitions, the preserved Stage 163G decline, and that the recovery never
touches COW state. No syscall/IPC ABI change, no IPC/cap seam moved, counts
unchanged (`SYSCALL_COUNT == 31`, `VARIANT_COUNT == 23`), RPi5 untouched, the
D6/CR3/TSS *switch* machinery untouched beyond the page-fault TLB fix, Stage 163E
transactional/run-preserving COW clone preserved, `MAX_ADDRESS_SPACES` remains 32,
and PCIDE stays disabled.

#### 5.1.9.16 Stage 163J — fork child return-lane / first-resume correctness

Stage 163I cleared the page-fault/COW loop (the child/parent stack faults now
route correctly through COW: `PF_PROOF_COW_HANDLE_OK ... path=private_copy →
PAGE_FAULT_HANDLED_COW`). QEMU then exposed the next blocker: the **fork child
returns as the parent**.

```
tid=1:     FORK_SMOKE_SYSCALL_RET ret0=10008 err=0   ; parent: child pid
tid=1:     FORK_SMOKE_PARENT child_pid=10008
tid=10008: FORK_SMOKE_SYSCALL_RET ret0=12 err=0      ; child WRONGLY sees 12
tid=10008: FORK_SMOKE_PARENT child_pid=12            ; misclassified as parent
```

The child must see `ret0 == 0` and log `CHILD_ENTRY`; instead it saw `12` and
recursively re-ran the sender-wake workload, whose second fork then failed
(`err=255`), so `SENDER_START`/`WAITER_PRESENT`/`SEQUENCE_DONE` were never
reached.

**Why the child saw `ret0 = 12` (Task A).** `12` is **`NR_fork`**, the fork
syscall number — which also happened to equal the child's ASID (`12`), making it
*look* like an ASID leak. It is not the ASID; it is a stale syscall number in the
return register, for a precise reason:

- The fork child is a **resumed** task. On x86_64 a resumed task is restored by
  `write_task_gprs_to_saved_regs`, which sets `rax = trap_frame.user_gpr(0)`.
- At syscall entry `build_trap_frame_from_saved_regs` records `user_gpr(0) = rax`,
  and on a `syscall` the user's `rax` holds the **syscall number** (`NR_fork =
  12`). So the parent's captured context has `user_gprs[0] = 12`.
- `fork_complete_post_clone` cloned the parent context and set
  `child.user_context.arg0 = 0` — but `arg0` only feeds `rdi`/`arg(0)` on the
  **new-task** entry path. The child is a *resumed* task, so `arg0` never reaches
  its `rax`; the inherited `user_gprs[0] = 12` is delivered verbatim.

So the return lane was set in the wrong place: `arg0` instead of the saved-GPR
return register. (No ASID is ever written to a user return lane; `12` was purely
the syscall number.)

**Fix (Task C, minimal, at the authoritative child-frame creation point).**
After cloning the parent context, zero the real return lane:
`child.user_context.user_gprs[0] = 0;` (x86_64 `RAX`), keeping `arg0 = 0`. This
mirrors how `complete_blocked_recv_for_waiter` delivers a resumed task's return
value through `user_gpr(0)`. The userspace decode (`ret0 != 0 → parent`,
`ret0 == 0 && err == 0 → child`) was already correct and was **not** changed —
in particular it was not hacked to treat an ASID-like `ret0` as the child; this
was a kernel fork-ABI bug fixed in the kernel.

**Diagnostics (Task B):** proof-gated `FORK_PROOF_CHILD_RET_SET`,
`FORK_PROOF_PARENT_RET_SET`, `FORK_PROOF_CHILD_FRAME_BEFORE_ENQUEUE`, and an
x86_64 `FORK_PROOF_FIRST_RESUME_AFTER_ARCH_RESTORE` that logs the `rax` actually
delivered by the resumed-task restore — proving the lane end-to-end.

**Expected x86_64 QEMU after fix (Task E):**
`tid=1: FORK_SMOKE_SYSCALL_RET ret0=<child> → FORK_SMOKE_PARENT` and
`tid=<child>: FORK_SMOKE_SYSCALL_RET ret0=0 → FORK_SMOKE_CHILD_ENTRY`; then
`FILL_DONE → FORK_RET role=parent/child → CHILD_ENTRY → SENDER_START`, child
blocks on full E1, `WAITER_PRESENT`, parent observes,
`IPC_RECV_V2_SENDER_WAKE_ORDER_OK → IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`.

**AArch64 (Task F):** not chased here. Its resume path delivers x0 from
`frame.ret0()` (not `user_gpr(0)`), a different mechanism; zeroing `user_gprs[0]`
is harmless there. Reclassify AArch64's `nr=0`/yield loop separately after
x86_64 passes.

The `stage163j_*` source guards pin the zeroed return lane (snapshot-then-
override), the parent `child_tid` return, the absence of any ASID→return-lane
write, the x86_64 `rax = user_gpr(0)` restore + first-resume trace, the
proof-gated diagnostics, the unchanged userspace decode, and that Stage 163E COW
and Stage 163I PF/intermediate-permission behavior remain intact. No syscall/IPC
ABI change, no IPC/cap seam moved, counts unchanged (`SYSCALL_COUNT == 31`,
`VARIANT_COUNT == 23`), RPi5 untouched, the D6/CR3/TSS/PF machinery untouched
beyond the fork-child return-lane fix, and `MAX_ADDRESS_SPACES` remains 32.

#### 5.1.9.17 Stage 163K — remove fork-smoke capacity interference

With Stage 163J the x86_64 fork child returns `ret0=0` and logs
`FORK_SMOKE_CHILD_ENTRY` (and `FORK_PROOF_FIRST_RESUME_AFTER_ARCH_RESTORE` shows
`rax=0`). QEMU then exposed the next blocker — the **real** sender-wake fork
failed:

```
FORK_PROOF_ALLOC_CHILD_FAIL reason=CapabilityFull step=register
FORK_PROOF_RETURN_ERR code=255 reason=CapabilityFull
IPC_RECV_PROOF_SENDER_WAKE_FORK_FAILED code=255 meaning=Internal
```

The proof did **two** forks: a clean-state diagnostic smoke (Stage 163C) and then
the real sender-wake fork. The smoke child (tid=10008) succeeded and then *parks
and yields forever*.

**Capacity source (Task A).** `CapabilityFull` at `step=register` comes from
`ensure_cnode_space_with_slots`, which enforces the **global aggregate CNode-slot
budget** `max_total_cnode_slots`: every live process reserves `slot_capacity`,
and the sum across all live `cnode_spaces` must stay within budget. (The
*task-table* limit is a distinct `TaskTableFull`, not this error.) The parked
smoke child permanently holds its reservation, so the second (real) fork's CNode
reservation overflowed the budget — i.e. the smoke child was consuming the
capacity the sender child needed. A new proof-gated
`FORK_PROOF_ALLOC_CHILD_CAPACITY step=register reason=… live_tasks=… max_tasks=…
reserved_cnode_slots=… max_total_cnode_slots=…` line in `fork_complete_post_clone`
makes the exhausted pool explicit.

**Fix (Task B, minimal).** The clean-state fork smoke is no longer called from
the required sender-wake path — the real sender-wake fork is itself the fork
proof, so acceptance needs exactly **one** live fork child. The smoke remains
defined as a diagnostic-only helper (`#[allow(dead_code)]`), off by default, so
it can be re-enabled for ad-hoc diagnosis without reintroducing the regression.
No exit/reap infrastructure was added.

**Expected x86_64 sender-wake sequence (Task C):** `FILL_DONE count=8 →
FORK_BEGIN → FORK_RET role=parent child_pid=<child>` / `<child>: FORK_RET
role=child → CHILD_ENTRY → SENDER_START`, child timed-send blocks on full E1,
`IPC_RECV_PROOF_SENDER_WAKE_WAITER_PRESENT tid=<child>`, parent `WAITER_OBSERVED`,
`IPC_RECV_V2_SENDER_WAKE_ORDER_OK → IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`.

`stage163k_*` guards pin: the acceptance path forks exactly once and does not
invoke the smoke; the smoke is diagnostic-only/uncalled; the register-fail path
reports the CNode-slot budget; and Stage 163J return-lane, Stage 163E COW, and
Stage 163I PF/intermediate-permission behavior all remain intact. No syscall/IPC
ABI change, no IPC/cap seam moved, counts unchanged (`SYSCALL_COUNT == 31`,
`VARIANT_COUNT == 23`), RPi5 untouched, `MAX_ADDRESS_SPACES` remains 32.

#### 5.1.9.18 Stage 163L — non-x86 fork return-lane fix + post-proof child parking

**Goal.** Two issues remained after Stage 163K:

1. **RISC-V and AArch64 fork parent return lanes were broken.**  The parent
   saw `ret0=0` instead of the child TID because the arch restore path
   (`apply_user_context`) overwrote the return registers with the pre-syscall
   TCB snapshot before the `+4` PC advance and export could set them.
2. **Child post-proof park loop spun on `yield_now()` (nr=0)**, polluting the
   syscall trace with repeated nr=0 noise during sender-wake verification.

**RISC-V root cause and fix** (`src/arch/riscv64/trap.rs`):

`handle_trap_entry_with_fault_bookkeeping_mode` previously advanced `saved_pc`
by 4 *before* `restore_arch_thread_state`.  `restore_arch_thread_state` calls
`resume_current_thread_with_frame` → `apply_user_context`, which overwrites
`frame.saved_pc` with the TCB's `instruction_ptr = ecall_addr`, undoing the
advance.  The same call also set `frame.user_gprs[10] = TCB.user_gprs[10] = 0`
(pre-syscall a0), wiping any `ret0` value.

Fix: call `restore_arch_thread_state(kernel, cpu, frame.as_deref_mut())` first
(preserving `frame` for later use), then advance `saved_pc += 4`, then export
`ret0 → user_gpr(10)` and `ret1 → user_gpr(11)` (or `error → user_gpr(10)` on
error).

**AArch64 root cause and fix** (`src/arch/aarch64/trap.rs`):

`export_syscall_result_to_user_gprs` correctly placed `ret0=child_tid` in
`user_gprs[REG_X0]` on the direct-ERET path.  But the TCB had been saved
earlier (lines 385–395) with `user_gprs[0]=0` (pre-syscall frame state before
export ran).  When the Stage 117 switch-plan stash is active (`switch_pending =
true`) `restore_arch_thread_state(syscall_return=true)` is skipped; the task is
later resumed via `restore_arch_thread_state_post_switch(syscall_return=false)`,
which runs the arg-mirror (`user_gprs[x0..x2] = args[0..2]`).  Since
`args[0..2]` still held the original input arguments (e.g. `args[0]=0` for
fork), the mirror overwrote the exported `user_gprs[REG_X0]=child_tid` with 0.

Fix: after `export_syscall_result_to_user_gprs(trapframe)`, unconditionally sync
`args[0..2]` from the just-exported `user_gprs[x0..x2]`, then re-call
`set_thread_user_context` to persist the updated context to the TCB.  This makes
the arg-mirror idempotent: `user_gprs[REG_X0] = args[0] = child_tid`.

**Proof-gated diagnostic markers added (Task A):**

| Marker | Architecture | Emitted when |
|--------|-------------|--------------|
| `RISCV_FORK_PARENT_RET_BEFORE_RETURN tid=<t> ret0=<r> a0=<a0> err=<e>` | riscv64 | Before a0 export, on ecall return |
| `RISCV_TRAP_RETURN_FRAME tid=<t> a0=<a0> a1=<a1> a2=<a2> err=<e>` | riscv64 | After a0/a1 export |
| `NONX86_SYSCALL_RETURN_LANE_SET arch=riscv64 tid=<t> nr=<nr> ret0=<r> err=<e>` | riscv64 | After export |
| `AARCH64_FORK_PARENT_RET_BEFORE_RETURN tid=<t> ret0=<r> x0=<x0> err=<e>` | aarch64 | Before export, in the !task_switched Syscall block |
| `AARCH64_TRAP_RETURN_FRAME tid=<t> x0=<x0> x1=<x1> x2=<x2> err=<e>` | aarch64 | After export and arg sync |
| `NONX86_SYSCALL_RETURN_LANE_SET arch=aarch64 tid=<t> nr=<nr> ret0=<r> err=<e>` | aarch64 | After export and arg sync |

All markers gated on `crate::kernel::boot::ipc_recv_proof_sender_wake_active()`.

**Task D: child parking + completion markers** (`service.rs`):

The child's `loop { yield_now() }` park was replaced by
`loop { ipc_recv(e1_recv) }` (blocking on the proof endpoint, emitting nr=2
instead of nr=0).  New markers bracket both roles:

| Marker | Role | Where |
|--------|------|-------|
| `IPC_RECV_PROOF_SENDER_WAKE_CHILD_DONE` | child | Replaces `SENDER_DONE` (child path); before park loop |
| `IPC_RECV_PROOF_SENDER_WAKE_PARK_BEGIN role=child` | child | Before blocking park loop |
| `IPC_RECV_PROOF_SENDER_WAKE_PARENT_DONE` | parent | After `SEQUENCE_DONE` |
| `IPC_RECV_PROOF_SENDER_WAKE_PARK_BEGIN role=parent` | parent | After `PARENT_DONE` |
| `IPC_RECV_PROOF_SENDER_WAKE_PARKED role=parent` | parent | Before function return |

**Task C (user action required):** Rerun sender-wake on AArch64 and RISC-V to
observe `AARCH64_FORK_PARENT_RET_BEFORE_RETURN`, `RISCV_FORK_PARENT_RET_BEFORE_RETURN`,
and the `NONX86_SYSCALL_RETURN_LANE_SET` markers in the proof log and confirm
parent returns `ret0=child_tid, err=0` on those arches.

**Preserved invariants:**

- Stage 163J x86_64 child return lane (`user_gprs[0]=0` in
  `fork_complete_post_clone`) untouched.
- Stage 163K no-smoke-interference (smoke not called in sender-wake path) untouched.
- Stage 163E COW behavior and Stage 163I PF/intermediate-permission behavior
  intact.
- No syscall/IPC ABI change, no IPC/cap seam moved, counts unchanged
  (`SYSCALL_COUNT == 31`, `VARIANT_COUNT == 23`), RPi5 untouched,
  `MAX_ADDRESS_SPACES` remains 32.

#### 5.1.9.19 Stage 163M — x86_64 IpcSend nonfatal classification + E2 blocking recv

**Goal.** After Stage 163L, the sender-wake proof still blocked indefinitely
on x86_64 when the child's `IpcSend` on E1 returned `WouldBlock` (sender-waiter
enqueue raced the recv).  Two sub-issues:

1. **`IpcSend` was classified as non-blocking** in `dispatch_syscall`.  A
   sender-waiter `IpcSend` that blocks legitimately was treated as a fatal
   `WouldBlock`, killing the proof.
2. **E2 wait used `ipc_recv_no_wait`** (busy-polling), which starved the
   child on AArch64/RISC-V because the child never got scheduled.

**Fix — Task A (`src/kernel/syscall.rs`):**

`IpcSend` is now always classified `blocking_syscall = true`.  A `WouldBlock`
result causes `dispatch_next_task` to yield the current thread, letting the
child make progress.  A `BLOCKED_OK` marker (proof-gated) distinguishes the
expected sender-waiter park from real fatal errors.

**Fix — Task B (`service.rs`):**

Replaced `ipc_recv_no_wait` polling with a single blocking
`ipc_recv_with_deadline(e2_recv, E2_WAIT_DEADLINE_TICKS=500_000_000)`.

*Note: Stage 163N superseded this; see §5.1.9.20 for the final E2 wait design.*

**Preserved invariants:** x86_64 child return lane (Stage 163J), no-smoke
interference (Stage 163K), Stage 163L AArch64/RISC-V parent return fix, no
syscall/IPC ABI changes.

---

#### 5.1.9.20 Stage 163N — E2 coordination polling + AArch64 fork-child CPU affinity

**Goal.** After Stage 163M, two root causes remained on AArch64 and RISC-V:

1. **E2 blocking recv deadlocked on AArch64/RISC-V.**
   `proof_sender_wake_push_coordination_locked` runs inside
   `enqueue_sender_waiter` while holding the IPC state lock (rank 3/4).  It
   cannot acquire the scheduler lock (rank 1) to wake blocked receivers.
   Therefore `ipc_recv_with_deadline(e2_recv, 500_000_000)` blocks the parent
   forever — the E2 signal is queued but the parent is never woken.

2. **Fork child placed on wrong CPU on AArch64 (4 CPUs online).**
   `enqueue_task` calls `enqueue_balanced` → `least_loaded_online_cpu`, which
   picks CPU1-3 while the parent is on CPU0.  When the parent then blocks on
   E2, CPU0 goes idle.  No IPI is sent to wake the child on its remote CPU, so
   the proof stalls until the scheduler timer fires.

**Fix — Task A: E2 polling loop** (`service.rs`):

Replaced the single blocking recv with a bounded polling loop:

```rust
const E2_POLL_YIELD_TICKS: u64 = 5_000_000;   // yield budget per iteration
const E2_POLL_MAX_ITERS: usize = 100;           // total budget = 500_000_000 ticks

'e2_poll: for poll_iter in 0..E2_POLL_MAX_ITERS {
    match ipc_recv_with_deadline(e2_recv, E2_POLL_YIELD_TICKS) {
        Ok(Some(sig)) => { waiter_tid = Some(sig.sender_tid.0); break 'e2_poll; }
        Ok(None)  => { /* timeout: loop and yield again */ }
        Err(_)    => { break 'e2_poll; }
    }
}
```

Each iteration's `ipc_recv_with_deadline` Phase 1 immediately catches an
already-queued E2 signal; Phase 2 blocks briefly to yield the CPU so the
child can run.  Total budget = 500 M ticks, same as Stage 163M.

**Fix — Task B: `enqueue_woken_task` in fork** (`src/kernel/boot/thread_state.rs`):

`fork_complete_post_clone` now calls `enqueue_woken_task(child_tid)` instead
of `enqueue_task(child_tid)`.  `enqueue_woken_task` uses `current_cpu()` and
places the child on the **same CPU** as the fork-calling parent, eliminating
the need for cross-CPU IPIs and ensuring the child is scheduled before the
parent's next timer tick.

**Fix — Task C: RISC-V diagnostic markers** (`src/arch/riscv64/boot.rs`):

Added proof-gated markers at the boot bridge writeback point to confirm at
runtime whether `task_switched` is set correctly during fork:

| Marker | Where |
|--------|-------|
| `RISCV_FORK_PARENT_A0_EXPORT entering_tid=<t> resume_tid=<r> task_switched=<b> scause=<s>` | After `resume_tid`/`task_switched` computed, before writeback |
| `RISCV_TCB_A0_SAVE_AFTER_EXPORT tid=<t> ret0=<r> ret1=<r1> err=<e>` | Inside ecall return path, just before `frame.regs[A0] = tframe.ret0()` |

Both markers gated on `ipc_recv_proof_sender_wake_active()`.

**New poll-loop log markers** (all gated on `ipc_recv_proof_sender_wake_active()`):

| Marker | When |
|--------|------|
| `IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_BEGIN` | Before poll loop starts |
| `IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_HIT iter=<n>` | E2 signal received at iteration `n` |
| `IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_EXHAUSTED` | All `E2_POLL_MAX_ITERS` timed out |

**Unit-test guards added** (`src/kernel/boot/tests.rs`, module
`stage163n_sender_wake_fix`):

| Test | What it verifies |
|------|-----------------|
| `stage163n_e2_uses_polling_loop_not_blocking_recv` | Polling loop + marker presence |
| `stage163n_e2_poll_iter_uses_yield_ticks` | `E2_POLL_YIELD_TICKS=5_000_000`, `E2_POLL_MAX_ITERS=100` |
| `stage163n_e2_poll_budget_bounded_below_send_timeout` | `5M * 100 = 500M <= 1_000_000_000` |
| `stage163n_fork_child_enqueued_with_woken_task` | `enqueue_woken_task`, not `enqueue_task` |
| `stage163n_fork_child_enqueue_log_includes_cpu_reason` | Log includes `cpu=` and `reason=` |
| `stage163n_riscv_boot_bridge_has_a0_export_marker` | `RISCV_FORK_PARENT_A0_EXPORT` present |
| `stage163n_riscv_boot_bridge_has_tcb_a0_save_marker` | `RISCV_TCB_A0_SAVE_AFTER_EXPORT` present |

**Preserved invariants:**

- Stage 163J x86_64 child return lane (`user_gprs[0]=0`) untouched.
- Stage 163K no-smoke-interference untouched.
- Stage 163L AArch64/RISC-V parent return fix untouched.
- Stage 163M x86_64 nonfatal `IpcSend` classification untouched.
- No syscall/IPC ABI change, no IPC/cap seam moved.
- Counts unchanged (`SYSCALL_COUNT == 31`, `VARIANT_COUNT == 23`,
  `MAX_ADDRESS_SPACES == 32`).
- RPi5 boot behavior untouched.

**User action required (QEMU smoke):**

Run `yarm.ipc_recv_proof=1 yarm.ipc_recv_proof_sender_wake=1` on all three
architectures and verify:

- `IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_HIT iter=0` appears (E2 signal caught
  immediately by Phase 1 — confirms the polling fix works).
- `RISCV_FORK_PARENT_A0_EXPORT ... task_switched=false` appears on RISC-V
  (confirms single-core path: parent returns without switching).
- `RISCV_TCB_A0_SAVE_AFTER_EXPORT tid=1 ret0=<child_tid>` appears on RISC-V
  (confirms parent's fork return value is the child TID).
- `IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE` is reached on all architectures.

#### 5.1.9.21 Stage 163O — E2 poll retry-on-timeout + RISC-V A0 preservation

**Goal.** After Stage 163N, the E2 poll loop still silently failed on all
architectures because `Err(TimedOut)` was treated as a fatal break condition.
Additionally, on RISC-V, a post-fork COW page fault clobbered the parent's
a0=child_tid before it could log the fork result, causing a role-swap bug.

**Root cause — Task A: E2 poll `Err(TimedOut)` treated as break**

`ipc_recv_with_deadline(e2_recv, E2_POLL_YIELD_TICKS)` on iter 0:

1. Phase 1 (immediate) finds E2 queue empty — child not yet scheduled.
2. `block_current_on_receive_with_deadline` is called; Phase C calls
   `dispatch_next_task()` which sets `current_tid=child` but does NOT switch
   CPU context (YARM dispatch model: context switch happens at trap-bridge
   return, not in kernel code).
3. Phase 2 runs immediately in the **same kernel call** before the child has
   had any CPU time: `ipc_recv_endpoint_take` returns `(None, None)`.
4. `timed_out = fired || received.is_none()` → `true` → `Err(TimedOut)`.

The `Err(_) => break 'e2_poll` arm exited the loop after ONE iteration.  The
child then ran (trap bridge sret'd to it), enqueued itself as sender-waiter on
E1, kernel pushed to E2.  Timer fired, parent resumed — but the poll loop had
already exited.  On iter 1, `try_endpoint_split_recv` (Phase 1) would have
found the queued E2 message and returned `Ok(Some(sig))` immediately.

**Fix — Task A** (`service.rs`):

Grouped `Err(TimedOut)` and `Err(WouldBlock)` with `Ok(None)` as retry arms:

```rust
Ok(None)
| Err(yarm_user_rt::syscall::SyscallError::TimedOut)
| Err(yarm_user_rt::syscall::SyscallError::WouldBlock) => {
    // Timeout or would-block — child not yet a sender-waiter; yield and retry.
}
Err(_) => { break 'e2_poll; }
```

**Root cause — Task B: RISC-V non-syscall trap clobbers fork return A0**

After fork ecall, the kernel sets `frame.regs[A0] = tframe.ret0() = child_tid`
and sret returns to the parent with a0=child_tid.  The parent's first
instruction after fork (e.g., `user_log!` store) triggers a COW page fault
(scause=0xf).

The non-syscall else branch in `yarm_riscv64_trap_bridge` wrote:
```rust
frame.regs[RiscvTrapFrame::A0] = tframe.user_gpr(10) as u64;  // BUG
```

After `restore_arch_thread_state` → `apply_user_context`, `tframe.user_gprs`
is reloaded from the TCB snapshot taken at **fork ecall entry**
(`sync_current_thread_from_frame`), which has `user_gprs[10]=0` (pre-ecall
a0).  The fork handler exports ret0=child_tid AFTER the sync; that value is
never saved back.  So `tframe.user_gpr(10)=0` clobbers a0=child_tid → parent
sees a0=0 → thinks it's the child → role-swap bug.

**Fix — Task B** (`src/arch/riscv64/boot.rs`):

Removed all `tframe.user_gpr()` writes from the non-syscall else branch.  The
mirror loop (lines 824–838) already skips A0–A7, so `frame.regs[A0..A7]` still
hold the hardware-saved values from the ASM trap saver — exactly what we need
for a same-task non-syscall trap.

**New RISC-V diagnostic markers** (gated on `ipc_recv_proof_sender_wake_active()`):

| Marker | When |
|--------|------|
| `RISCV_NON_SYSCALL_TRAP_FRAME_SAVE tid=<t> scause=<s>` | Non-syscall else branch entered |
| `RISCV_PAGE_FAULT_PRESERVE_GPRS tid=<t> a0=<v> a1=<v>` | A0/A1 hardware-preserved values logged |
| `RISCV_POST_FAULT_TRAP_RETURN tid=<t> a0=<v>` | A0 value about to be restored to userspace |
| `RISCV_FORK_PARENT_A0_PRESERVED_AFTER_FAULT tid=<t> a0=<v>` | Non-zero a0 preserved (fork return intact) |

**Unit-test guards added** (`src/kernel/boot/tests.rs`, module
`stage163o_e2_poll_fix`):

| Test | What it verifies |
|------|-----------------|
| `stage163o_e2_poll_timed_out_grouped_with_ok_none` | TimedOut/WouldBlock before the break arm |
| `stage163o_e2_poll_timed_out_arm_is_not_break` | No break between TimedOut and WouldBlock |
| `stage163o_riscv_non_syscall_else_does_not_write_a0_from_tframe` | A0/A1 NOT written from `tframe.user_gpr()` |
| `stage163o_riscv_non_syscall_else_has_preserve_markers` | All four new RISC-V markers present |

**Preserved invariants:**

- Stage 163J x86_64 child return lane (`user_gprs[0]=0`) untouched.
- Stage 163K no-smoke-interference untouched.
- Stage 163L AArch64/RISC-V parent return fix untouched.
- Stage 163M x86_64 nonfatal `IpcSend` classification untouched.
- Stage 163N `enqueue_woken_task` fork-child placement untouched.
- No syscall/IPC ABI change, no IPC/cap seam moved.
- Counts unchanged (`SYSCALL_COUNT == 31`, `VARIANT_COUNT == 23`,
  `MAX_ADDRESS_SPACES == 32`).
- RPi5 boot behavior untouched.

**User action required (QEMU smoke):**

Run `yarm.ipc_recv_proof=1 yarm.ipc_recv_proof_sender_wake=1` on all three
architectures and verify:

- x86_64: `^IPC_RECV_V2_SENDER_WAKE_ORDER_OK` appears (real recv-v2 split
  path fired the sender-wake).
- All arches: `USER_LOG .*msg=IPC_RECV_PROOF_SENDER_WAKE_WAITER_OBSERVED`
  appears (E2 poll succeeded, waiter TID captured).
- All arches: `USER_LOG .*msg=IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`
  appears (full proof sequence completed).
- No `BLOCKED_WOULDBLOCK_FATAL`, `CapabilityFull`, or `TaskTableFull` errors.
- RISC-V: `RISCV_FORK_PARENT_A0_PRESERVED_AFTER_FAULT` appears when a COW
  fault fires after fork (confirms a0=child_tid was preserved).

#### 5.1.9.22 Stage 163P — cooperative E2 poll + RISC-V full-frame fault preservation

**Goal.** Stage 163O's fixes were not accepted by QEMU on any architecture. Two
distinct root causes remained, both confirmed by runtime logs and (for RISC-V)
disassembly.

**Root cause — Task A: parent blocks on E2 and is never re-scheduled**

The Stage 163N/O E2 poll used `ipc_recv_with_deadline(e2_recv, E2_POLL_YIELD_TICKS)`
per iteration. On an empty E2 this **blocks** the parent (TCB → `Blocked`, removed
from the run queue) and dispatches the child; the syscall then returns
`Err(TimedOut)` synchronously into the parent's saved frame, but the parent's TCB
is left `Blocked` and **off the run queue**. It can only be made runnable again by
`process_ipc_timeout_deadlines` on a **timer interrupt**. Once both parent (blocked
on E2) and child (blocked on its E1 send) are parked, the CPU idles; the parent
never resumes to run the next poll iteration, so it never observes the queued E2
signal (no `E2_POLL_HIT`), and the proof stalls on every arch. Runtime logs
confirmed: child reaches the E1 waiter state, but no `E2_POLL_HIT` /
`WAITER_OBSERVED` / `SEQUENCE_DONE` ever follows.

**Fix — Task A** (`service.rs`): replace the blocking poll with a **non-blocking
probe + explicit yield**:

```rust
'e2_poll: for poll_iter in 0..E2_POLL_MAX_ITERS {
    match ipc_recv_with_deadline(e2_recv, 0) {        // timeout 0 → never blocks
        Ok(Some(sig)) => { /* HIT */ break 'e2_poll; }
        Ok(None) | Err(WouldBlock) | Err(TimedOut) => {} // transient — retry
        Err(e) => break 'e2_poll,                          // genuine error
    }
    yield_now();   // hand CPU to child; parent stays Runnable
}
```

`yield_current` marks the parent `Runnable` (keeps it on the run queue) and
switches to the child. When the child becomes a sender-waiter and parks, the
scheduler returns to the parent, which finds the queued E2 signal on its next
non-blocking probe. No timer dependency; portable across all three arches. The
kernel still pushes the E2 signal atomically inside `enqueue_sender_waiter`
(race-free); the child does **not** send E2 itself. `E2_POLL_YIELD_TICKS` is
removed. New diagnostics: `E2_CAPS`, `E2_POLL_RET iter=<n> result=<...>`.

**Root cause — Task B: RISC-V page-fault path overlays a stale TCB snapshot**

Disassembly of the parent's fork return (release build) showed userspace is
correct — it banks the syscall return in a callee-saved register before reusing
a0:

```
ecall            # a0 = child_tid (10008)
mv   s2, a0      # s2 = child_tid  (banked BEFORE a0 is reused)
auipc a0, 0x5    # a0 := format-string addr (~0x4073d9)
sd   a0, 0x40(sp)  # FIRST stack store after fork → COW page fault (scause=0xf)
...
sd   s2, 0x50(sp)  # fork return stored from s2
```

The fault is the parent's first COW stack write. In the RISC-V trap bridge,
`restore_arch_thread_state → apply_user_context` reloads `tframe.user_gprs`/`args`
from the TCB's `user_context`, which was last synced at the previous **syscall**
entry (`sync_current_thread_from_frame` runs only on the Syscall arm; page faults
never re-sync). The bridge's mirror loop then copied that **stale** snapshot over
the **live hardware-saved frame**, resetting every callee-saved/temp register —
including `s2` — to its pre-fork value (here `s2` held a stale format-string text
address `0x4073d9`). Userspace then stored that as the fork return
(`ret0=4223961`), so the parent decoded itself as the child → role-swap. Stage
163O only protected A0–A7; the mirror loop still clobbered `s2`.

**Fix — Task B** (`src/arch/riscv64/boot.rs`): gate the entire `tframe → frame`
writeback (sepc/SP reload, the GPR mirror loop, and the ABI-lane writes) behind
`if task_switched || scause == EXC_USER_ECALL`. For a **same-task non-syscall
trap** (COW/demand page fault) take the `else` branch and leave the hardware-saved
`frame` **entirely untouched** — `frame.regs`/`sepc`/`SP` already hold the exact
state the CPU trapped on, so the faulting instruction re-executes transparently
after the COW copy. This is a general RISC-V correctness fix: any callee-saved or
temp register mutated since the last syscall now survives a fault. Markers
retained, and `RISCV_PAGE_FAULT_PRESERVE_GPRS` now also logs `s2`.

**Unit-test guards** (`src/kernel/boot/tests.rs`, module `stage163p_sender_wake_fix`):

| Test | Verifies |
|------|----------|
| `stage163p_e2_poll_is_nonblocking` | probe is `ipc_recv_with_deadline(e2_recv, 0)`; no yield-ticks |
| `stage163p_e2_poll_yields_between_probes` | `yield_now()` between probes |
| `stage163p_e2_poll_transient_arms_do_not_break` | TimedOut/WouldBlock/none are retry arms |
| `stage163p_e2_poll_emits_diag_markers` | `E2_CAPS` / `E2_POLL_RET` / `E2_POLL_HIT` |
| `stage163p_riscv_non_syscall_branch_preserves_full_hw_frame` | no `tframe.*`/mirror writes in the fault branch |
| `stage163p_riscv_writeback_gated_on_switch_or_ecall` | writeback gated on `task_switched || ecall` |
| `stage163p_riscv_non_syscall_branch_has_markers` | preserve markers incl. `s2=` |

The Stage 163M/N tests that asserted the blocking yield-ticks design were updated
to the cooperative non-blocking form.

**Preserved invariants:** Stage 163J/K/M, Stage 163N `enqueue_woken_task` child
placement; no syscall/IPC ABI change, no IPC/cap seam moved; `SYSCALL_COUNT == 31`,
`VARIANT_COUNT == 23`, `MAX_ADDRESS_SPACES == 32`; RPi5 boot untouched; no
`BLOCKED_WOULDBLOCK_FATAL` for the expected proof child block.

**Acceptance evidence (Stage 163P — ACCEPTED on x86_64, AArch64, RISC-V):**

| Arch | Marker / evidence | Result |
|------|-------------------|--------|
| x86_64 | `^IPC_RECV_V2_SENDER_WAKE_ORDER_OK` (real recv-v2 split kernel marker, line-start anchored) | PASS |
| x86_64 | `USER_LOG .*msg=IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE` | PASS |
| x86_64 | no `BLOCKED_WOULDBLOCK_FATAL`, no `CapabilityFull`, no `TaskTableFull` | PASS |
| AArch64 | parent role, child role, waiter present, waiter observed, sequence done | PASS |
| AArch64 | no fatal / capacity markers | PASS |
| RISC-V | parent `raw=10008`, child `raw=0` | PASS |
| RISC-V | waiter present, waiter observed, sequence done | PASS |
| RISC-V | no `raw=4223961` regression, no `arch_code=0xc` regression | PASS |
| RISC-V | no fatal / capacity markers | PASS |

> **Stage 163P is now a cross-arch regression oracle workload, not the current
> kernel-unlocking frontier.** It must continue to pass on all three architectures
> under `yarm.ipc_recv_proof=1 yarm.ipc_recv_proof_sender_wake=1`; treat any
> regression as a blocker before any future live-wire PR. The current unlocking
> frontier is D6-SWITCH — dropping the global `SpinLock<KernelState>` across
> cooperative kernel context switch / `switch_frames` — see §7.1.5 for the
> re-anchored roadmap.

---

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
| x86_64 | AP Rust online / park status | scaffolded (superseded by Stage 183: idle-live + env-ready; GS/CR3/GDT/TSS/LAPIC proven by the AP, graded by the admit poll) | per-CPU env scaffold; APs reach env-ready but do not join the scheduler. |
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

**Rebaselined after Stage 163P acceptance (Cycle 13 review).** The fork/sender-wake
saga (Stages 163A–163P) is now accepted as a cross-arch oracle. That oracle is not the
kernel-unlocking frontier — it is a regression gate. The real gate blocking D2-GENUINE
and D6-GENUINE is still **D6/SWITCH**: the global `SpinLock<KernelState>` is still
held across cooperative kernel context switch / `switch_frames`. Stages 117–129 + the
default-off `D6_SWITCH_PROOF` harness (Stages 120–132 + 139) put all the plumbing in
place. The next items drive that plumbing into the production smoke path. D4 is
mechanically complete (Stage 152). The roadmap, in order:

1. **D6-SWITCH-SMOKE — get the unlocked `switch_frames` proof markers into core smoke.**
   The `D6_SWITCH_PROOF=1` harness (Stages 120–132 + 139) demonstrates the unlocked
   path on x86_64 under a controlled pair. The next step is to move that gate from
   default-off proof mode into the default core smoke so that
   `D6_SWITCH_FRAMES_ENTER_UNLOCKED` and `D6_SWITCH_FRAMES_RETURNED_UNLOCKED` appear
   in a regular (non-`D6_SWITCH_PROOF=1`) x86_64 core smoke. Accepted evidence: both
   markers appear at least once in `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh`
   without the proof knob; `D6_GLOBAL_LOCK_DROP_DEFERRED` must no longer appear for
   the switched task pair. (Outcome B: preparatory phase split, no genuine seam call.)

   **Stage 165B–165I (D6-SWITCH-SMOKE stabilization, x86_64).** Bringing the
   `D6_SWITCH_PROOF=1` harness through a clean QEMU run surfaced a chain of
   post-cleanup faults: a guard-adjacent #PF on tid=3's switch stack (165B/165F),
   a `VmFull` from mis-classifying the boot/CPU stack as a per-task stack
   (165C), a false-success when a demand-paged stack page had no source frame
   (165E), and the no-owner tid=0 idle/trap stack being skipped (165G). These
   were fixed with proof-only post-cleanup stack mapping that shares every live
   task's kernel stack (owner-root or active-root sourced, supervisor-only,
   guard-adjacent page included) across all roots, with explicit
   `D6_POST_CLEANUP_STACK_MAP_*` markers and a hardened smoke gate. The terminal
   issue (165H/165I) was a genuine **kernel-stack overflow**: the deep
   post-cleanup trap path (`handle_trap` ~8 KiB frame +
   `process_ipc_timeout_deadlines`' `[None; 512]` ~8 KiB + nested chain) reaches
   ~33 KiB, overflowing the 32 KiB per-task region; at tid=0's region — which
   sits exactly at the canonical boundary `0xFFFF_8000_0000_0000` — the overflow
   descends into **non-canonical** space and escalates to a #DF (vector 8,
   CR2=0). Non-canonical pages cannot be mapped, so the durable fix enlarges the
   x86_64 per-task kernel stack region: **Stage 165I** 32 KiB → 64 KiB
   (`0x8000 → 0x10000`), **Stage 165J** 64 KiB → 128 KiB (`0x10000 → 0x20000`),
   both `#[cfg(target_arch = "x86_64")]`; AArch64/RISC-V keep 32 KiB and are
   untouched. **Caveat:** the observed overflow depth tracked the region size
   (~33 KiB at 32 KiB, ~64 KiB at 64 KiB) because tid=0 always bottoms at the
   canonical boundary — and a single timer-trap chain is only ~16–20 KiB, so
   reaching 64 KiB implies ~4× nesting. If 128 KiB still #DFs, the post-cleanup
   path is nested/recursive (interrupts re-enabled during the handler, or a
   re-entrant fault loop) rather than fixed-deep, and the fix is to bound the
   nesting, not enlarge the stack again. **Stage 165J is ACCEPTED** (user QEMU,
   2026): the 5-minute `D6_SWITCH_PROOF=1` run reached the 300 s idle timeout with
   no `!Fv`/`!BNv`/`PAGE_FAULT`/`DOUBLE_FAULT`/`PANIC`/`FATAL`, with
   `D6_CONTROLLED_SWITCH_PROOF_DONE` / `…CLEANUP_DONE` /
   `D6_POST_CLEANUP_STACK_MAP_DONE … failures=0` present and 128 KiB stack regions
   in `KERNEL_STACK_RANGE`; normal core smoke and the Stage 163P sender-wake oracle
   also passed. So the 64 KiB depth was a fixed-deep path (handle_trap + IPC-timeout
   chain), not unbounded recursion — 128 KiB cleared it. All of this is default-off
   proof path / stack-capacity only: no production Outcome A, no genuine seam
   live-wire, no ABI change. See the `stage165*` guards in
   `src/kernel/boot/tests.rs`.

2. **D6-SWITCH-A — first narrow x86_64 production Outcome A (Stage 166, ACCEPTED).**
   With D6-SWITCH-SMOKE accepted, Stage 166 adds a default-off knob
   `yarm.d6_switch_a=1` (script: `D6_SWITCH_A=1`) that drives the *same proven
   production* `maybe_switch_kernel_context` → `DISPATCH_SWITCH_PLAN_STASH` →
   trap-entry drain → unlocked `switch_frames` path (the one validated by
   D6-SWITCH-SMOKE) for a real task pair (tid=1 → tid=2, both
   `initialize_thread_kernel_switch_frame`-initialized in the production boot path),
   with the global `SpinLock<KernelState>` genuinely dropped before `switch_frames`.
   It is x86_64-only, single-CPU, one-shot, and **reversible** (default-off ⇒ the
   diagnostic `D6_SWITCH_PROOF` path and all other switch paths are unchanged; the
   Stage 116 lock-held fallback is preserved). Markers:
   `D6_SWITCH_A_ENABLED`, `D6_SWITCH_A_CANDIDATE outgoing=<tid> incoming=<tid>`,
   `D6_SWITCH_A_LOCK_DROPPED`, `D6_SWITCH_A_SWITCH_ENTER`,
   `D6_SWITCH_A_FIRST_RESUME incoming=<tid>`, `D6_SWITCH_A_RETURNED`,
   `D6_SWITCH_A_DONE`, and `D6_SWITCH_A_FALLBACK reason=<…>` on a deferral.
   Acceptance (user QEMU): (A) D6 proof regression —
   `TIMEOUT_SECS=300 D6_SWITCH_PROOF=1 QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh`;
   (B) production gate —
   `TIMEOUT_SECS=120 D6_SWITCH_A=1 QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh`
   (must show the `D6_SWITCH_A_*` markers and reach service baseline with no fatal
   breadcrumb); (C) normal core smoke; (D) Stage 163P sender-wake oracle.
   Stage 166 deliberately does **not** delete the `with_scheduler_split_mut` /
   global-lock fences or broaden to all switch paths — that is the follow-on
   D6-GENUINE. See the `stage166_d6_switch_a` guards in `src/kernel/boot/tests.rs`.

   **ACCEPTED (user QEMU, 2026).** The `D6_SWITCH_A=1` run produced a real
   production unlocked switch — `D6_SWITCH_A_ENABLED`,
   `D6_SWITCH_A_CANDIDATE outgoing=1 incoming=2`,
   `D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH outgoing=1 incoming=2`,
   `D6_SWITCH_A_LOCK_DROPPED`/`SWITCH_ENTER`/`FIRST_RESUME incoming=2`/`RETURNED`/`DONE`
   — with no `!Fv`/`!BNv`/`PAGE_FAULT`/`DOUBLE_FAULT`/`PANIC`/`FATAL` and the
   x86_64 service baseline reached. The 5-minute `D6_SWITCH_PROOF=1` regression
   ran clean (`PROOF_DONE` / `CLEANUP_DONE` /
   `D6_POST_CLEANUP_STACK_MAP_DONE tasks=4 roots=3 failures=0 guard_pages=4`),
   and the normal core smoke + Stage 163P sender-wake oracle passed.

   **Stage 166B (smoke false-negative fix).** The same run was initially reported
   as failed by `scripts/qemu-x86_64-core-smoke.sh` due to a stale Stage 165D
   heuristic ("`D6_KERNEL_SWITCH_STACK_CHECK_FAILED tid=N` with no later
   `CHECK_OK`"). Those CHECK_FAILED lines are early `target_asid_unavailable`
   retries; once the proof completes via the accepted path the mapping succeeds
   through a different code path that need not emit a matching `CHECK_OK`. The
   heuristic is now **suppressed when the proof completed cleanly** (PROOF_DONE +
   CLEANUP_DONE + post-cleanup `failures=0` + no fatal breadcrumb after proof
   start). All hard runtime gates remain unconditional (fatal breadcrumbs,
   `…STACK_MAP_SKIP`, `…STACK_MAP_ROOT … result=failed`, `…STACK_MAP_DONE …
   failures>0`, `…GUARD_PAGE … included=0`, no-owner NOTE,
   `D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_FAILED`, `D6_PROOF_LIVE_RSP_STACK_MAP_FAILED`,
   `D6_FIRST_RESUME_STASH_MISSING`), so runtime safety is unchanged. Guarded by
   `stage166b_check_failed_heuristic_suppressed_when_clean`.

3. **D6-GENUINE — D6 dispatch seam fully live-wired.** Extend the Outcome A unlock to
   all production tasks on x86_64; verify that the deferred markers
   (`D6_GLOBAL_LOCK_DROP_DEFERRED`) no longer appear in the default x86_64 smoke.
   Gated on D6-SWITCH-A.

   **D6-GENUINE-A — first live caller of the rank-1 scheduler seam (Stage 167).**
   The intermediate step that makes `SharedKernel::with_scheduler_split_mut`
   (rank 1) its **first live production caller**, ending its `M2_SEAM_HELPER_ONLY`
   status, behind a default-off knob `yarm.d6_genuine=1` (script: `D6_GENUINE=1`;
   x86_64-only, single-CPU). After `handle_trap_entry_shared`'s `with_cpu` returns
   and the global `SpinLock<KernelState>` guard is dropped, the trap-entry path
   calls `SharedKernel::d6_genuine_local_dispatch_observe(cpu)`, which runs **one
   `local_dispatch_step_split` dispatch observation through the seam holding ONLY
   the scheduler lock** — the global lock is genuinely not held. The observation
   is **non-mutating** (it reads the committed dispatch decision via
   `current_tid_on` / `runnable_count_on`, never `dispatch_next_on`), so it can
   never double-advance the run queue; the **in-lock `local_dispatch_step_split`
   inside `with_cpu` stays the authoritative dispatch decision and the preserved
   fallback**. The knob is mutually exclusive with `d6_switch_proof` /
   `d6_switch_a` so those paths are untouched. Markers: `D6_GENUINE_ENABLED`,
   `D6_LOCAL_DISPATCH_SEAM_CANDIDATE`, `D6_LOCAL_DISPATCH_SEAM_ENTER`,
   `D6_LOCAL_DISPATCH_SEAM_LOCK_SCOPE_DROPPED`,
   `D6_LOCAL_DISPATCH_STEP_SPLIT cpu=<n> tid=<…> runnable=<n>`,
   `D6_LOCAL_DISPATCH_SEAM_COUNT cpu=<n> n=<n> tid=<…>`,
   `D6_LOCAL_DISPATCH_SEAM_DONE`, and `D6_LOCAL_DISPATCH_SEAM_FALLBACK` for the
   ineligible (multi-CPU) case. Stage 167 deliberately does **not** relocate the
   in-lock authoritative dispatch out of the global lock (that is the remaining
   D6-GENUINE work, which requires moving the dispatch entry point ahead of
   `with_cpu` — see the documented blocker on `local_dispatch_step_split`), nor
   broaden to mutating the scheduler from the new out-of-lock path, nor live-wire
   the D2/D3/D5 seams. The scheduler seam's validation label moves from
   `M2_SEAM_HELPER_ONLY` to `M2_SEAM_LIVE_D6_GENUINE` in `runtime.rs`; the other
   seams keep their fences. Guarded by the `stage167_d6_genuine` module in
   `src/kernel/boot/tests.rs`; all three arches cross-build (the wire is
   x86_64-only and a no-op elsewhere). Acceptance (user QEMU): (A) production
   gate — `TIMEOUT_SECS=120 D6_GENUINE=1 QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh`
   (must show the `D6_LOCAL_DISPATCH_SEAM_*` markers and reach service baseline
   with no fatal breadcrumb after the seam wire begins); (B) D6-SWITCH-A
   regression (`D6_SWITCH_A=1`); (C) D6 proof regression (`D6_SWITCH_PROOF=1`);
   (D) normal core smoke; (E) Stage 163P sender-wake oracle.

   **ACCEPTED (user QEMU, 2026).** The `D6_GENUINE=1` run passed (the
   `D6_LOCAL_DISPATCH_SEAM_*` markers appeared and the x86_64 service baseline was
   reached with no fatal breadcrumb), and the D6 proof, `D6_SWITCH_A`, normal
   x86_64 smoke, and Stage 163P sender-wake oracle regressions all passed. Stage
   167 is **observation-only**: it proved the scheduler seam can execute live
   outside the global lock, but the authoritative **mutating** dispatch decision
   still ran in-lock. Stage 168 (below) begins the mutating dispatch relocation.

   **D6-GENUINE-B — relocate the authoritative mutating dispatch out of the global
   lock (Stage 168).** Under the same default-off `yarm.d6_genuine=1` gate
   (x86_64-only, single-CPU), the in-lock `dispatch_next_task` now **declines to
   perform the authoritative mutating `local_dispatch_step_split`** for the
   *eligible, queue-neutral* slice: it records a per-CPU deferral
   (`D6_GENUINE_DISPATCH_DEFERRED`) and returns the peeked next TID. After
   `handle_trap_entry_shared`'s `with_cpu` returns and the global
   `SpinLock<KernelState>` guard is dropped, the trap-entry drain runs the single
   authoritative mutating step through `SharedKernel::d6_genuine_local_dispatch_step_mut`
   — calling the real mutating `dispatch_next_on` under ONLY the rank-1 scheduler
   lock — plus the deferred (idempotent) Phase-B TCB `Running` write through the
   rank-2 task seam. Eligibility is restricted to the **queue-neutral** case
   (`!(runnable > 0 && current ∈ {none, idle-tid-0})`) so `dispatch_next_on`
   provably never dequeues out of lock — it **cannot double-advance the run
   queue** — and the drain re-verifies queue-neutrality before running. Every
   ineligible case (switch required / idle→runnable, multi-CPU, no trap drainer,
   already deferred) emits `D6_GENUINE_MUT_DISPATCH_FALLBACK reason=<…>` and takes
   the **unchanged in-lock `local_dispatch_step_split`**. Markers:
   `D6_GENUINE_MUT_DISPATCH_CANDIDATE/ELIGIBLE/PREPARED/FALLBACK` (exec_state) and
   `…_GLOBAL_DROPPED/ENTER/STEP_SPLIT/DONE/COUNT` (trap_entry / seam). The
   remaining, queue-*advancing* dispatch (the real context switch that a blocking
   recv or preemption needs) still uses the in-lock fallback — its relocation
   requires moving Phase B (ASID switch + `switch_frames` stash + TCB status) out
   of the global lock and is deferred to a follow-on stage. Guarded by
   `stage168_d6_genuine_b_and_d2_recv`.

4. **D2-GENUINE — D2 blocking-recv waiter-publish seam fully live-wired.** With the
   global lock no longer spanning `switch_frames` (D6-GENUINE), relocate the D2
   `block_current_on_receive_with_deadline` call boundary ahead of
   `SharedKernel::with_cpu` so that `with_scheduler_split_mut` (rank 1) and
   `with_task_tcbs_split_mut` (rank 2) are called without the outer global-lock
   borrow. Delete the helper-only fences for those two seams in the same PR.
   Gated on D6-GENUINE.

   **D2-GENUINE-RECV — rank-clean blocking-recv phase live-wire (Stage 168A,
   PARTIAL).** Under a new default-off `yarm.d2_recv_genuine=1` gate (x86_64-only; script:
   `D2_RECV_GENUINE=1`), the canonical blocking-recv path
   (`ipc_recv_with_optional_deadline` → `block_current_on_receive_with_deadline`,
   which backs both `IpcRecv` and `IpcRecvTimeout`, plain and recv-v2) exposes its
   existing **rank-ordered scheduler(1) → task(2) → ipc(3) → dispatch** phase
   boundaries live, with explicit markers: `D2_RECV_GENUINE_ENABLED`,
   `…_CANDIDATE tid=<t> endpoint=<id>`, `…_PHASE_CAP_OK`, `…_PHASE_IPC_LOCK`,
   `…_PHASE_TASK_BLOCK`, `…_PHASE_DISPATCH`, `…_BLOCKED_OK`, `…_IMMEDIATE_OK`,
   `…_TIMEOUT_OK`, `…_NOWAIT_OK`, `…_ROLLBACK_OK` (the no-lost-wakeup
   `recv_block_unwind_race`), `…_FALLBACK`, and `…_DONE result=<…>`. When combined
   with `yarm.d6_genuine=1`, the recv-block's `dispatch_next_task` routes an
   eligible (queue-neutral) dispatch through the Stage 168 out-of-global-lock
   scheduler seam; a blocking recv usually requires a real switch, which stays on
   the preserved in-lock fallback. Immediate delivery, NoWait probe, timeout, and
   the no-lost-wakeup rollback are **byte-identical** whether the knob is on or
   off — the knob only exposes the phase boundaries (and uses the D6 seam where
   eligible); the full relocation of the recv phase *mutations* out of the global
   lock is deferred (it depends on the queue-advancing switch relocation above,
   Stage 169+). The D2 **send** path, D3/D5 seams, and IPC-FINAL are untouched.
   Guarded by `stage168_d6_genuine_b_and_d2_recv`. Acceptance (user QEMU): the six
   commands in §7.1.5-acceptance below — D6 proof, `D6_SWITCH_A`, `D6_GENUINE=1`
   (must emit `D6_GENUINE_MUT_DISPATCH_ENTER/STEP_SPLIT/DONE`),
   `D6_GENUINE=1 D2_RECV_GENUINE=1` (must emit the D2 recv phase markers), normal
   x86_64 smoke, and the Stage 163P sender-wake oracle — all reaching service
   baseline with no `!Fv`/`!BNv`/`PAGE_FAULT`/`DOUBLE_FAULT`/`TRIPLE`/`PANIC`/`FATAL`.

   **Stage 168A PARTIAL result (user QEMU).** The queue-neutral D6 mutating
   dispatch ran outside the global lock (`D6_GENUINE_MUT_DISPATCH_ELIGIBLE/
   PREPARED/GLOBAL_DROPPED/ENTER/STEP_SPLIT/DONE/COUNT`) and the D2 recv phase
   markers went live — but the **blocking recv is not complete**: every blocking
   recv reached `D2_RECV_GENUINE_PHASE_DISPATCH` and then
   `D6_GENUINE_MUT_DISPATCH_FALLBACK reason=switch_required`, i.e. the
   queue-*advancing* dispatch a blocking recv needs still ran on the in-lock
   fallback. So Stage 168A is **partial-accepted only**; the D2-GENUINE-RECV
   target is finished by Stage 168B.

   **D2-GENUINE-RECV COMPLETION (Stage 168B).** Moves the blocking recv's
   queue-advancing dispatch OUT of the global lock. When
   `block_current_on_receive_with_deadline` commits the block (waiter published,
   recv task `Blocked` via Phase A `block_current` which removes it from
   `current`), it no longer calls `dispatch_next_task` in-lock: instead it
   records a per-CPU deferral (`d2_recv_dispatch_try_defer`) and emits
   `D2_RECV_GENUINE_DISPATCH_DEFERRED` + `D2_RECV_GENUINE_NO_INLOCK_DISPATCH`,
   returning without dispatching. After `handle_trap_entry_shared`'s `with_cpu`
   returns and the global `SpinLock<KernelState>` guard is dropped, the trap
   entry drains it: `D2_RECV_GENUINE_GLOBAL_DROPPED` → re-verify the recv task is
   still `Blocked(EndpointReceive)` through the rank-2 task seam
   (`D2_RECV_GENUINE_DISPATCH_REVERIFY_OK`) → `D2_RECV_GENUINE_DISPATCH_ENTER` →
   run the authoritative **queue-advancing** `dispatch_next_on` through ONLY the
   rank-1 scheduler seam (`d2_recv_dispatch_step_mut`,
   `D2_RECV_GENUINE_DISPATCH_STEP_SPLIT cpu=<n> result=switch|idle incoming=<t>`)
   → commit `Running` via the rank-2 task seam → restore the incoming task's arch
   thread state (frame + CR3) via the **hardened D6-SWITCH-A** re-acquire
   (`post_switch_restore_arch_thread_state`; a user-task recv resumes via
   trap-frame restore + syscall restart, so no new `switch_frames` mechanism is
   introduced — the `D2_RECV_GENUINE_SWITCH_*` markers document the dormant
   kernel-thread variant) → `D2_RECV_GENUINE_DISPATCH_DONE`. Fallbacks
   (`D2_RECV_GENUINE_FALLBACK reason=…`) preserve the in-lock path for
   `multi_cpu` / `no_trap_drainer` / `already_deferred` / `state_changed`; the
   normal x86_64 `-smp 1` `D6_GENUINE=1 D2_RECV_GENUINE=1` blocking recv must NOT
   fall back with `reason=switch_required` (the smoke gate enforces this on the
   recv path specifically). The no-lost-wakeup rollback (`recv_block_unwind_race`
   → `D2_RECV_GENUINE_ROLLBACK_OK`) is preserved (the deferral is only recorded
   on the committed Published path). Task A hardens the smoke script with a
   `YARM_MODE_ISOLATION` normalization (precedence
   `D6_SWITCH_PROOF > D6_SWITCH_A > {D6_GENUINE, D2_RECV_GENUINE}`) so a clean
   `D6_SWITCH_PROOF=1` run cannot inherit/require the genuine knobs or markers.
   The D2 **send** path, D3/D5 seams, and IPC-FINAL remain untouched; ABI/counts
   unchanged (SYSCALL_COUNT=31, VARIANT_COUNT=23, x86_64 MAX_ADDRESS_SPACES=32);
   AArch64/RISC-V no-op. Guarded by `stage168b_d2_recv_genuine_completion`.
   Acceptance (user QEMU, run each with `env -u` for the other modes): D6 proof
   (clean, no D6_GENUINE markers), `D6_SWITCH_A`, `D6_GENUINE`,
   `D6_GENUINE=1 D2_RECV_GENUINE=1` (must show a real blocking recv with
   `PHASE_TASK_BLOCK` → `PHASE_DISPATCH` → `DISPATCH_DEFERRED` → `GLOBAL_DROPPED`
   → `DISPATCH_ENTER` → `DISPATCH_STEP_SPLIT` → `DISPATCH_DONE` and no fatal),
   normal x86_64 smoke, and the Stage 163P sender-wake oracle. **Stage 169
   (D2-GENUINE-SEND) must NOT start until Stage 168B QEMU acceptance passes.**

   **ACCEPTED (user QEMU, 2026).** The `D6_GENUINE=1 D2_RECV_GENUINE=1` run showed
   a real blocking recv relocating its queue-advancing dispatch out of the global
   lock — `D2_RECV_GENUINE_DISPATCH_DEFERRED`, `…_NO_INLOCK_DISPATCH`,
   `…_GLOBAL_DROPPED`, `…_DISPATCH_REVERIFY_OK`,
   `…_DISPATCH_STEP_SPLIT result=switch`, `…_DISPATCH_DONE` (and
   `SWITCH_STASHED`/`SWITCH_ENTER`/`FIRST_RESUME` also observed) — reaching the
   x86_64 service baseline with no fatal breadcrumb, and the D6 proof,
   `D6_SWITCH_A`, `D6_GENUINE`, normal smoke, and Stage 163P sender-wake oracle
   regressions all passed. Stage 168B is **ACCEPTED**; Stage 169 begins
   D2-GENUINE-SEND (below) and must preserve the Stage 163P sender-wake oracle.

   **D2-GENUINE-SEND (Stage 169).** Moves the blocking IpcSend / sender-waiter
   path onto the same rank-clean seams and out-of-global-lock dispatch as
   Stage 168B recv, behind a new default-off x86_64-only knob
   `yarm.d2_send_genuine=1` (script `D2_SEND_GENUINE=1`, marker
   `D2_SEND_GENUINE_ENABLED`). A blocking send occurs when a sender must wait
   (synchronous endpoint with no waiter, or a full async queue): both call
   `block_current_on_send_with_deadline`, which runs Phase A (`block_current`,
   scheduler rank 1) → Phase B (TCB `Blocked(EndpointSend(cap))`, task rank 2,
   `D2_SEND_GENUINE_PHASE_TASK_BLOCK`) → Phase C (`enqueue_sender_waiter` publish,
   ipc rank 3, `D2_SEND_GENUINE_PHASE_IPC_LOCK`) → `PHASE_DISPATCH`. The
   sender-waiter (message riding with it) is published BEFORE the deferral, so the
   receiver-side wake/handoff and the Stage 163P sender-wake coordination are
   unchanged. It then records a per-CPU deferral (`d2_send_dispatch_try_defer`,
   `D2_SEND_GENUINE_DISPATCH_DEFERRED` + `…_NO_INLOCK_DISPATCH` + `…_BLOCKED_OK` +
   `…_DONE result=blocked`) and returns WITHOUT dispatching in-lock. After
   `handle_trap_entry_shared`'s `with_cpu` returns and the global
   `SpinLock<KernelState>` guard is dropped, the trap entry drains it exactly like
   recv: `D2_SEND_GENUINE_GLOBAL_DROPPED` → re-verify the sender is still
   `Blocked(EndpointSend)` via the rank-2 task seam
   (`D2_SEND_GENUINE_DISPATCH_REVERIFY_OK`) → `…_DISPATCH_ENTER` → authoritative
   queue-advancing `dispatch_next_on` under ONLY the rank-1 scheduler seam
   (`d2_send_dispatch_step_mut`, `…_DISPATCH_STEP_SPLIT result=switch|idle`) →
   commit `Running` via the task seam → restore the incoming task's arch state via
   the **hardened D6-SWITCH-A** `post_switch_restore_arch_thread_state` re-acquire
   (no new switch mechanism; the dormant `D2_SEND_GENUINE_SWITCH_*` markers cover
   the kernel-thread variant) → `…_DISPATCH_DONE`. Fallbacks
   (`D2_SEND_GENUINE_FALLBACK reason=multi_cpu|no_trap_drainer|already_deferred|state_changed`),
   immediate send (`…_IMMEDIATE_OK` / `…_DONE result=immediate`), and NoWait /
   timeout are preserved; no message loss, no orphaned waiter, no duplicate wake,
   no reply-cap change. **Stage 163P sender-wake preserved:** blocking IpcSend
   stays nonfatal (`caller_blocked=true` → `Ok`), and the smoke auto-enables the
   sender-wake proof workload under `D2_SEND_GENUINE=1` so a blocking send is
   deterministically exercised AND `IPC_RECV_PROOF_SENDER_WAKE_BLOCKED_OK`,
   `^IPC_RECV_V2_SENDER_WAKE_ORDER_OK`, and the
   `USER_LOG …IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE` oracle sequence are
   re-checked. Mode isolation extends the precedence to
   `D6_SWITCH_PROOF > D6_SWITCH_A > {D6_GENUINE, D2_RECV_GENUINE, D2_SEND_GENUINE}`.
   **Explicitly NOT done in Stage 169:** no IPC-FINAL; no D3/D5 live-wire; no SMP
   broadening (single-CPU only); no AArch64/RISC-V D6 switch unlock (knob is
   x86_64-only / no-op elsewhere); no syscall/IPC/service/image ABI change
   (SYSCALL_COUNT=31, VARIANT_COUNT=23, x86_64 MAX_ADDRESS_SPACES=32). Guarded by
   `stage169_d2_send_genuine`. Acceptance (user QEMU): (1) `D2_SEND_GENUINE=1`
   (must show the full `D2_SEND_GENUINE_*` blocking-send phase + out-of-lock
   dispatch markers and the Stage 163P oracle markers, no fatal); (2)
   `D6_GENUINE=1 D2_RECV_GENUINE=1` recv regression; (3) `D6_GENUINE`; (4)
   `D6_SWITCH_A` + `D6_SWITCH_PROOF`; (5) Stage 163P sender-wake oracle; (6) normal
   smoke.

   **ACCEPTED (user QEMU, 2026).** The `D2_SEND_GENUINE=1` run produced a real
   blocking IpcSend that relocated its queue-advancing dispatch out of the global
   lock — `D2_SEND_GENUINE_PHASE_TASK_BLOCK` → `PHASE_IPC_LOCK` → `PHASE_DISPATCH`
   → `DISPATCH_DEFERRED` → `NO_INLOCK_DISPATCH` → `GLOBAL_DROPPED` →
   `DISPATCH_REVERIFY_OK` → `DISPATCH_ENTER` → `DISPATCH_STEP_SPLIT result=switch`
   → `SWITCH_STASHED`/`SWITCH_ENTER`/`FIRST_RESUME` → `DISPATCH_DONE` — while the
   Stage 163P sender-wake markers remained present
   (`IPC_RECV_PROOF_SENDER_WAKE_BLOCKED_OK`, line-start
   `IPC_RECV_V2_SENDER_WAKE_ORDER_OK`, and the `USER_LOG …SEQUENCE_DONE` line).
   Normal, `D6_GENUINE`, `D2_RECV_GENUINE`, `D6_SWITCH_A`, `D6_SWITCH_PROOF`, and
   sender-wake regressions all passed. Stage 169 is **ACCEPTED**; the D2 blocking
   send and recv genuine paths are complete for single-CPU x86_64.

### 7.1.6 Stage 170 — IPC-FINAL (recv-v2 IPC surface stability milestone)

**This is a stability milestone, not a new behavior stage.** Stage 170 freezes
the accepted IPC recv-v2 surface behind a single strict, repeatable acceptance
profile (`IPC_FINAL=1` in `scripts/qemu-ipc-recv-v2-oracle-smoke.sh`). No runtime
behavior changes; the only code change is a doc-comment refresh on
`local_dispatch_step_split` (recording the 168B/169 relocations) — no syscall /
IPC / message / cap / endpoint semantics change, and no D3/D5/VM/CNode live-wire.

Accepted IPC surface (frozen):

- **recv-v2 metadata paths** — `IPC_RECV_V2_META_BLOCKED_WAITER_OK`,
  `IPC_RECV_V2_META_IMMEDIATE_OK`, `IPC_RECV_V2_META_QUEUED_SPLIT_OK`.
- **immediate recv** — endpoint had a message; delivered under the ipc lock with
  no block (recv side), `IPC_RECV_V2_META_IMMEDIATE_OK`.
- **queued split recv** — the trap-entry split fast path
  (`IPC_RECV_PROOF_QUEUED_SPLIT_SEQUENCE_DONE` + `…_META_QUEUED_SPLIT_OK`).
- **rollback** — recv-v2 meta user-copy fault unwinds cleanly
  (`IPC_RECV_V2_ROLLBACK_OK` + `IPC_RECV_PROOF_ROLLBACK_SEQUENCE_DONE`); the
  no-lost-wakeup `recv_block_unwind_race` (`D2_RECV_GENUINE_ROLLBACK_OK`).
- **reply-cap one-shot** — `IPC_REPLY_CAP_ONESHOT_OK` (single-use reply cap,
  consumed exactly once; live D1/D5 split path every spawn cycle).
- **transfer-cap materialization** — `IPC_TRANSFER_CAP_MATERIALIZE_OK`.
- **sender-wake ordering** — line-start `IPC_RECV_V2_SENDER_WAKE_ORDER_OK` +
  `USER_LOG …IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`; the proof-gated kernel
  coordination hook (Stage 163P) makes the handshake deterministic.
- **blocking recv (D2-GENUINE-RECV, Stage 168B)** — queue-advancing dispatch out
  of the global lock: `D2_RECV_GENUINE_DISPATCH_DEFERRED` / `NO_INLOCK_DISPATCH`
  / `GLOBAL_DROPPED` / `DISPATCH_REVERIFY_OK` / `DISPATCH_STEP_SPLIT` /
  `DISPATCH_DONE` (checked when `D2_RECV_GENUINE=1`).
- **blocking send (D2-GENUINE-SEND, Stage 169)** — same relocation on the send
  side: `D2_SEND_GENUINE_DISPATCH_DEFERRED` / `NO_INLOCK_DISPATCH` /
  `GLOBAL_DROPPED` / `DISPATCH_REVERIFY_OK` / `DISPATCH_STEP_SPLIT` /
  `DISPATCH_DONE` (checked when `D2_SEND_GENUINE=1`).
- **timeout/deadline behavior** — as currently accepted: `IpcRecvTimeout` with
  `timeout_ticks==0` is NoWait; `>0` is a deadline; `process_ipc_timeout_deadlines`
  wakes `Blocked(EndpointReceive|EndpointSend)` at expiry (`ipc_timeout_fired`).
- **syscall error parity / nonfatal blocked syscall** — a blocking IpcSend/IpcRecv
  is nonfatal (`caller_blocked=true` → `Ok`, dispatch switches away); normal
  syscall errors are encoded into the trap frame and returned to userspace, never
  turned into a fatal trap halt.

**Strict `IPC_FINAL=1` profile.** Enables all three proof workloads (queued-split
+ rollback + sender-wake) and extended mode (reply-cap + transfer-cap), then
HARD-requires the full accepted marker set with **line-start anchoring** for
`IPC_RECV_V2_SENDER_WAKE_ORDER_OK` (an "absent:"/"present:" wrapper echo can never
satisfy it) and a **strict failure gate**: fails on `BLOCKED_WOULDBLOCK_FATAL`,
`CapabilityFull`, `TaskTableFull`, a committed-recv/-send `reason=switch_required`
in-lock fallback, missing sender-wake / rollback / queued-split sequences, and the
fatal breadcrumbs `^!Fv`, `^!BNv`, `DOUBLE_FAULT`, `TRIPLE`, `PANIC`, `FATAL`.
**Handled COW page faults are NOT fatal** (`PAGE_FAULT` accompanied by
`PAGE_FAULT_HANDLED_COW`). Guarded by `stage170_ipc_final`.

**Explicitly NOT in Stage 170:** no IPC-FINAL *behavior* change; no D3/D5/VM/CNode
live-wire; no SMP broadening (single-CPU only); no AArch64/RISC-V D6 switch unlock
(the D2 knobs are x86_64-only / no-op elsewhere); no syscall / IPC / service /
image ABI change (SYSCALL_COUNT=31, VARIANT_COUNT=23, x86_64
MAX_ADDRESS_SPACES=32); no RPi5 change.

Acceptance (user QEMU): (1) `IPC_FINAL=1 QEMU_SMP=1 scripts/qemu-ipc-recv-v2-oracle-smoke.sh x86_64`;
(2) `D6_GENUINE=1 D2_RECV_GENUINE=1 QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh`;
(3) `D2_SEND_GENUINE=1 QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` (sender-wake
auto-enabled); (4) `QEMU_SMP=1 YARM_IPC_RECV_PROOF_SENDER_WAKE=1 scripts/qemu-ipc-recv-v2-oracle-smoke.sh x86_64`;
(5) normal `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh`; (6)
`D6_SWITCH_A=1 …`; (7) `TIMEOUT_SECS=300 D6_SWITCH_PROOF=1 …`.

**ACCEPTED (user QEMU, 2026).** All seven commands passed with `[ok]` and no fatal
breadcrumbs: the `IPC_FINAL=1` oracle, `D2_RECV_GENUINE=1`, `D2_SEND_GENUINE=1`,
the sender-wake oracle, normal x86_64 smoke, the `D6_SWITCH_A` regression, and the
5-minute `D6_SWITCH_PROOF` regression. Stage 170 IPC-FINAL is **ACCEPTED** — the
recv-v2 IPC surface is frozen. Stage 171 (SCHED-TIMEOUT) hardens the scheduler
timeout/deadline behavior on top of this baseline.

### 7.1.7 Stage 171 — SCHED-TIMEOUT (scheduler timeout/deadline hardening)

**Stability/hardening stage before VM/COW and cap/CNode work.** Makes the
timeout/deadline behavior rank-clean, deterministic, and bounded-stack, with a
default-off diagnostic profile (`yarm.sched_timeout=1`, script `SCHED_TIMEOUT=1`,
marker `SCHED_TIMEOUT_ENABLED`). No syscall/IPC/service/image ABI change; no
D3/D5/VM/CNode live-wire; no SMP broadening; the D6/D2 knobs stay default-off.

**Timeout frontier (audit, Task B).** The deadline field `ipc_timeout_deadline`
is wired ONLY for `Blocked(EndpointReceive)` and `Blocked(EndpointSend)` — i.e.
blocking IPC recv and send timeouts (and, transitively, an `ipc_call` reply-wait,
which rides the recv-timeout path). `Futex`, `Join`, and `Poll` waits are
**indefinite by design** (no deadline field; woken only by their explicit wake
path), so `process_ipc_timeout_deadlines` deliberately filters to the two IPC
wait reasons and never touches a futex/join/poll waiter. There is no separate
sleep/nanosleep deadline path today. Exact lock/rank order of the single timeout
driver (`process_ipc_timeout_deadlines`, called each serviced timer tick from the
x86_64 timer-IRQ path in `fault_state.rs`): per batch, **task (rank 2)** mark
Runnable + clear deadline → **ipc (rank 3)** remove the tid from every waiter slot
(endpoint receive slot, sender-waiter queues, notification waiters) → **scheduler
(rank 1)** enqueue OUTSIDE the task/ipc locks. Locks are acquired/released per
phase, never nested; no lower-rank lock is taken while a higher-rank lock is held.

**Bounded-stack chunked scan (Task F).** `process_ipc_timeout_deadlines` no longer
allocates the historical `[None; MAX_TASKS]` (= `[None; 512]` of `Option<ThreadId>`,
~8 KiB) scratch array on every timer-tick trap frame. It now processes expirations
in fixed `TIMEOUT_SCAN_CHUNK` (32) batches — an O(CHUNK) ≈ 512-byte frame
regardless of `MAX_TASKS` — looping until a pass finds zero expirations (each woken
task clears its own deadline, so no task is selected twice and the loop
terminates). Behavior is equivalent: same total expired count, same waiter
clearance, each expired task enqueued exactly once. This removes the ~8 KiB
contributor to the Stage 165 deep-trap kernel-stack pressure. A `stage171_*` guard
rejects reintroducing a large timeout scratch array.

**No stranded waiters (Task D).** For each expired batch, every waiter slot
referencing a timed-out tid is cleared (receive slot, sparse sender-waiter queues
scanned slot-by-slot, notification waiters), then a within-`ipc_state_lock`
re-check confirms none of the batch tids remain — `SCHED_TIMEOUT_STRANDED_WAITER`
fires only on a clear-loop bug (never expected). No later receiver can consume a
timed-out sender (its queued waiter is nulled); no timed-out receiver stays
blocked (status → Runnable in phase 1); each is enqueued exactly once; no
duplicate wake, no orphaned waiter, no reply-cap / transfer-envelope leak. Stage
163P sender-wake ordering is untouched (the timeout path only removes waiters that
timed out; the wake/handoff coordination is unchanged).

**Idle with pending timeout (Task E).** The scheduler idle branch emits (knob-gated
+ rate-limited to `SCHED_IDLE_MARKER_BUDGET`=8 to avoid UART flooding)
`SCHED_IDLE_PENDING_TIMEOUT earliest=…` + `SCHED_IDLE_TIMEOUT_SAFE earliest=…`
when a deadline is armed, else `SCHED_IDLE_NO_PENDING_TIMEOUT`. On x86_64 idle is
`hlt`, which wakes on the periodic timer IRQ that drives
`process_ipc_timeout_deadlines`, so a pending timeout is **guaranteed to fire** —
the CPU never enters an indefinite halt that would strand a deadline. AArch64 `wfi`
is equivalent. **RISC-V** uses the raw trap path with a deferred/coalesced timer
(`record_timer_tick`); its IPC-timeout firing is serviced on the next handled timer
tick, which remains a documented known characteristic (not changed here — no
RISC-V behavior change). x86_64 `-smp 1` is the primary QEMU acceptance target.

**Diagnostic markers (Task C).** Knob-gated in `process_ipc_timeout_deadlines`,
emitted only when a timeout actually fires (quiet ticks produce nothing):
`SCHED_TIMEOUT_SCAN_BEGIN`, `SCHED_TIMEOUT_EXPIRED tid=… kind=recv|send`,
`SCHED_TIMEOUT_TASK_WAKE_BEGIN`, `…_RUNQUEUE_ENQUEUE tid=…`, `…_TASK_WAKE_DONE`,
`…_NO_STRANDED_WAITERS`, `…_SCAN_DONE`. (`kind` is only ever `recv`/`send` — the
only wait reasons with a deadline; there is no `reply`/`futex`/`sleep` timeout.)

**`SCHED_TIMEOUT=1` acceptance profile (Task G).** Requires `SCHED_TIMEOUT_ENABLED`
and the deterministic idle-entry markers; if any timeout fires, requires the full
phase sequence and exactly-once wake (`SCHED_TIMEOUT_EXPIRED` count ==
`SCHED_TIMEOUT_RUNQUEUE_ENQUEUE` count). Fails on `SCHED_TIMEOUT_STRANDED_WAITER`,
an idle `PENDING` without `SAFE`, `BLOCKED_WOULDBLOCK_FATAL`, `CapabilityFull`,
`TaskTableFull`, and the fatal breadcrumbs `^!Fv`/`^!BNv`/`DOUBLE_FAULT`/`TRIPLE`/
`PANIC`/`FATAL`. Handled COW page faults (`PAGE_FAULT` + `PAGE_FAULT_HANDLED_COW`)
are NOT fatal. Guarded by `stage171_sched_timeout`. Invariants unchanged
(SYSCALL_COUNT=31, VARIANT_COUNT=23, x86_64 MAX_ADDRESS_SPACES=32).

Acceptance (user QEMU): (1) `SCHED_TIMEOUT=1 QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh`;
(2) `IPC_FINAL=1 QEMU_SMP=1 scripts/qemu-ipc-recv-v2-oracle-smoke.sh x86_64`;
(3) `D2_RECV_GENUINE=1 QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh`;
(4) `D2_SEND_GENUINE=1 QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh`;
(5) `QEMU_SMP=1 YARM_IPC_RECV_PROOF_SENDER_WAKE=1 scripts/qemu-ipc-recv-v2-oracle-smoke.sh x86_64`;
(6) normal `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh`; (7) `D6_SWITCH_A=1 …`;
(8) `TIMEOUT_SECS=300 D6_SWITCH_PROOF=1 …`; optionally a RISC-V timeout smoke.

**Stage 171B (SCHED_TIMEOUT smoke fault-gate fix).** The first `SCHED_TIMEOUT=1`
QEMU run reached service baseline and emitted `SCHED_TIMEOUT_ENABLED` +
`SCHED_IDLE_NO_PENDING_TIMEOUT`, but the wrapper **falsely failed** with
`PAGE_FAULT without PAGE_FAULT_HANDLED_COW`. The two page-fault groups in the log
(tid=1 and tid=10008) were both **handled COW faults** — each emits many benign
`PAGE_FAULT_*` diagnostic lines (`PAGE_FAULT_ENTRY`, `…_HW_REGS`, `…_FRAME_WORDS`,
`…_FRAME_DECODE`, `…_HW_PTE_WALK`, `…_RAW`, `…_X86_ERROR`, `…_CR3_COMPARE`) BEFORE
the final `PAGE_FAULT_HANDLED_COW`. The old gate treated the presence of ANY
`PAGE_FAULT` token (without a whole-log `PAGE_FAULT_HANDLED_COW`) as fatal, which
is wrong. Stage 171B **narrows the page-fault gate to explicit unhandled/fatal
markers only** — the kernel's `PAGE_FAULT_UNHANDLED tid=… addr=…`
(`fault_state.rs`), plus `PAGE_FAULT_FATAL` / `PAGE_FAULT_NOT_HANDLED` defensively
— and never fails on benign `PAGE_FAULT_*` diagnostics. Handled COW/DEMAND faults
(`PAGE_FAULT_HANDLED_COW` / `PAGE_FAULT_HANDLED_DEMAND`) are accepted. The same
latent bug in the IPC-FINAL oracle gate (`qemu-ipc-recv-v2-oracle-smoke.sh`)
received the identical fix. All real Stage 171 gates are preserved
(`SCHED_TIMEOUT_ENABLED`, idle diagnostics, `SCHED_TIMEOUT_STRANDED_WAITER`,
expired-without-enqueue / duplicate-wake, `BLOCKED_WOULDBLOCK_FATAL` /
`CapabilityFull` / `TaskTableFull`, `^!Fv`/`^!BNv`/`DOUBLE_FAULT`/`TRIPLE`/`PANIC`/
`FATAL`, and the proof/switch-a mode isolation). No kernel runtime change; guarded
by `stage171b_fault_gate`. **PENDING user QEMU re-run.**

**ACCEPTED (user QEMU, 2026).** After the Stage 171B gate fix, the `SCHED_TIMEOUT=1`
x86_64 run reached service baseline, emitted `SCHED_TIMEOUT_ENABLED` + idle
diagnostics, reported `[ok] SCHED-TIMEOUT: timeout/deadline hardening diagnostics
clean` and `x86_64 boot markers detected`, and the fatal grep was empty. Stage 171
SCHED-TIMEOUT is **ACCEPTED** (Stage 171B changed only the smoke fault gate — no
kernel runtime change). Stage 172 (VM-COW) is the next frontier.

### 7.1.8 Stage 172 — VM-COW (VM/COW/page-table split + hardening)

**Next kernel-unlocking frontier after IPC-FINAL and SCHED-TIMEOUT.** Splits and
instruments the VM/COW/page-table/frame/fork path with a default-off diagnostic
profile (`yarm.vm_cow=1`, script `VM_COW=1`, marker `VM_COW_ENABLED`). This is an
audit + instrumentation + hardening-verification stage: the existing COW/fork/map
paths are ALREADY transactional (each failure rolls back), so **no VM runtime
behavior changes** — the markers only expose the phase boundaries.

**Explicitly NOT done in Stage 172:** no D2/D6 change; no IPC ABI change; no
syscall count change (SYSCALL_COUNT=31, VARIANT_COUNT=23, x86_64
MAX_ADDRESS_SPACES=32); no CNode/cap overhaul; no SMP shootdown live-wire (prep
markers only); no AArch64/RISC-V D6 switch unlock (the knob is arch-neutral
diagnostics / no-op behavior); no RPi5 change; IPC-FINAL / D2 recv/send / Stage
163P / Stage 171 all preserved.

**Audit (Task B).** Paths audited and confirmed already transactional:
- **COW write fault** (`try_handle_cow_fault`, memory_state.rs): only WRITE faults
  to COW pages copy (fault_state.rs gates on `FaultAccess::Write`); read faults
  never copy. Phases: metadata (resolve mapping) → frame alloc
  (`alloc_anonymous_memory_object` + `resolve_memory_object_phys`) → COW copy
  (`copy_frame_contents_for_cow`) → PT update (`map_user_page_in_asid_raw`, writable)
  → clear COW mark. **Rollback:** every failure path revokes the freshly-allocated
  MemoryObject cap (no frame/cap leak). An already-writable stale-COW page just
  clears the mark. Lock/rank: capability (4) → vm (5) → memory (6), sequential.
- **Fork COW clone** (`clone_user_address_space_cow`): preflight rejects
  over-capacity BEFORE any mutation; snapshots parent runs (never iterates the
  live table); maps each run read-only/shared into the child; write-protects each
  writable parent run IN PLACE (no split); marks COW in parent + child; read-only
  parent runs that already carry a COW mark are inherited shared. **Rollback:**
  `rollback_cow_clone` destroys the partial child and restores every
  write-protected parent run + clears its COW marks (parent left byte-identical).
- **VM map** (`handle_vm_map`, syscall/vm.rs): per-page frame alloc + map with
  `rollback_anon_map` reclaiming caps/frames for the mapped prefix on any failure.
- **VM unmap** (`unmap_user_page` / `unmap_user_page_in_asid`): unmap PTE → clear
  COW → note removed → reclaim frame → `request_live_asid_shootdown` (TLB).
- Remaining global-KernelState-held VM mutations are classified metadata /
  page-table / frame-alloc / COW-refcount / TLB in the per-function comments; a
  genuine out-of-global-lock relocation of the VM domain is a later stage (this
  stage only instruments and verifies rollback).

**Markers (Task C/D/E/F).** All default-off behind `vm_cow`:
- COW fault: `VM_COW_FAULT_BEGIN`, `VM_COW_PHASE_METADATA`,
  `VM_COW_PHASE_FRAME_ALLOC`, `VM_COW_PHASE_PT_UPDATE`, `VM_COW_PHASE_TLB_FLUSH`,
  `VM_COW_ROLLBACK_BEGIN`/`VM_COW_ROLLBACK_DONE`, `VM_COW_DONE`,
  `VM_COW_FAIL reason=…`.
- Map/unmap: `VM_MAP_PHASE_METADATA`/`_FRAME_ALLOC`/`_PT_UPDATE`,
  `VM_MAP_ROLLBACK_OK`, `VM_UNMAP_PHASE_METADATA`/`_PT_UPDATE`, `VM_UNMAP_TLB_FLUSH`.
- Fork: `VM_COW_FORK_BEGIN`, `VM_COW_FORK_CHILD_MAP`,
  `VM_COW_FORK_PARENT_WRITE_PROTECT`, `VM_COW_FORK_REFCOUNT_OK`,
  `VM_COW_FORK_ROLLBACK_OK`, `VM_COW_FORK_DONE`.
- TLB prep (Task F, NO real SMP shootdown yet): `VM_TLB_LOCAL_FLUSH`,
  `VM_TLB_SHOOTDOWN_DEFERRED reason=smp_not_live`, `VM_TLB_SHOOTDOWN_PREP_DONE`.
  **Later SMP shootdown needs:** target-ASID tracking, a per-CPU active-ASID
  bitmap, an IPI ACK path, a rank-clean/lock-free shootdown wait, and
  timeout/failure handling — none live-wired here.

**COW correctness (Task E).** Only write faults copy; read faults do not; the
shared page stays shared until first write; the writable bit is restored only on
the private copy; parent/child isolation holds after write; error paths return a
normal syscall/fault result, not a fatal trap. `PAGE_FAULT_HANDLED_COW` /
`PAGE_FAULT_HANDLED_DEMAND` are preserved; the Stage 163P fork sender-wake oracle
is unaffected (the markers are gated; the workload is auto-enabled under `VM_COW=1`
to exercise fork COW clone + COW write faults deterministically).

**`VM_COW=1` acceptance profile (Task G).** Requires `VM_COW_ENABLED`; checks the
COW/fork phase sequence when it occurs; fails on `VM_COW_FAIL`,
`VM_MAP_ROLLBACK_FAIL`, `VM_UNMAP_ROLLBACK_FAIL`, `VM_COW_REFCOUNT_UNDERFLOW`,
`VM_COW_WRITABLE_SHARED_ALIAS`, `VM_COW_CHILD_ASID_LEAK`, `PAGE_FAULT_UNHANDLED` /
`PAGE_FAULT_FATAL` / `PAGE_FAULT_NOT_HANDLED`, `^!Fv`/`^!BNv`/`DOUBLE_FAULT`/
`TRIPLE`/`PANIC`/`FATAL`, `CapabilityFull`, `TaskTableFull`. Handled COW/DEMAND
faults are accepted. Mode isolation forces `VM_COW` off under proof/switch-a.
Guarded by `stage172_vm_cow`.

Acceptance (user QEMU): (1) `VM_COW=1` smoke; (2) `IPC_FINAL=1` oracle; (3)
`D2_RECV_GENUINE=1`; (4) `D2_SEND_GENUINE=1`; (5) `SCHED_TIMEOUT=1`; (6) normal
smoke; (7) `D6_SWITCH_A=1`; (8) 5-min `D6_SWITCH_PROOF=1`; (9) Stage 163P
sender-wake oracle.

**ACCEPTED (user QEMU, 2026).** The first `VM_COW=1` run failed on a **stale
x86_64 build artifact** (the boot image predated the VM-COW instrumentation, so
`VM_COW_ENABLED` never appeared). After rebuilding with
`scripts/build-qemu-x86_64-artifacts.sh`, the `VM_COW=1` profile passed: service
baseline reached, `VM_COW_ENABLED` present, a COW write fault observed, the
`VM_COW_PHASE_METADATA` / `VM_COW_PHASE_TLB_FLUSH` / `VM_COW_DONE` phase sequence
present, diagnostics clean, and none of the fatal markers
(`VM_COW_FAIL`/rollback-fail/refcount-underflow/writable-shared-alias/
child-ASID-leak/fatal breadcrumb) present. Stage 172 VM-COW is **ACCEPTED** — the
markers changed no VM/COW/page-table/fork behavior. Stage 173 (CAP-CNODE) is the
next frontier.

### 7.1.9 Stage 173 — CAP-CNODE (capability/CNode budget, materialize, release, transfer, rollback audit + diagnostics)

**Next kernel-unlocking frontier after VM-COW.** Instruments the
capability/CNode lifecycle — cap lookup, slot reserve, mint/materialize, cap
transfer, reply-cap one-shot, release/revoke, CNode-budget/full rollback, and
object refcount/generation/stale checks — with a default-off diagnostic profile
(`yarm.cap_cnode=1`, script `CAP_CNODE=1`, marker `CAP_CNODE_ENABLED`). This is an
audit + instrumentation + hardening-verification stage: the existing cap/CNode
paths are ALREADY transactional (each mint/transfer/materialize failure rolls back
with correct `cap_refcount`/generation discipline), so **no capability/CNode
runtime behavior changes** — the markers only expose the phase boundaries. This is
**instrumentation only**; **no real bug** was found in the audit.

**Explicitly NOT done in Stage 173:** no syscall count change (SYSCALL_COUNT=31,
Syscall::VARIANT_COUNT=23, x86_64 MAX_ADDRESS_SPACES=32); no cap-encoding ABI
change; no rights broadening; no D2/D6/IPC-FINAL/VM-COW/SCHED-TIMEOUT behavior
change; no D3/D5 live-wire; no real SMP CNode/shootdown broadening; no
AArch64/RISC-V D6 switch unlock (the knob is arch-neutral diagnostics / no-op
behavior); no RPi5 change.

**Lock/rank story.** Capability/CNode is **rank 4** in the lock ordering
(`doc/KERNEL_LOCKING.md`). The audited operations acquire no lower-rank lock while
conceptually holding the cap lock; in the current tree they run under the single
global `SpinLock<KernelState>` (the cap/CNode domain has not been relocated out of
the global lock — that is a later stage). This stage documents the rank-4 position
and current global-lock-mediated state honestly; it does not split the cap domain
out of the global lock.

**Audit (Task B).** Paths audited and confirmed already transactional — no bug
fixed, diagnostics only:
- **Cap lookup** (`capability_for_cnode_local`, `resolve_capability_for_task`):
  index + generation checked; a stale generation resolves to `None` (rejected).
- **Slot reserve + mint/materialize** (`mint_capability_in_cnode`): reserves cnode
  space first (`RESERVE_FAIL reason=full` on a full CNode — clean error, no partial
  state), then mints; on mint failure the reservation is not consumed
  (`MATERIALIZE_FAIL`). `cap_refcount` is bumped exactly once on the object.
- **Cap transfer materialization** (`materialize_received_message_cap` /
  `materialize_received_transfer_cap`, ipc_recv_core.rs): reserve → materialize →
  done; a receiver-full / missing-right / stale / internal failure rolls back
  (`TRANSFER_ROLLBACK_BEGIN`/`_OK`, `TRANSFER_FAIL reason=…`) leaving no minted slot.
- **Reply-cap one-shot** (`fast_revoke_reply_cap_in_cnode`): a reply cap is
  materialized once and consumed once; a second consume is refused
  (`REPLY_DOUBLE_CONSUME_BLOCKED`) — no double-free, no refcount touch.
- **Materialize rollback** (`rollback_materialized_recv_cap`, transfer_state.rs):
  the inverse of the materialize mint (reply → `fast_revoke_reply_cap_in_cnode` +
  clear `waiter_cap_id`; transfer → `revoke_capability_in_cnode` decrement +
  reclaim). A cleared slot yields `MATERIALIZE_ROLLBACK_OK`; a residual slot would
  yield `ROLLBACK_LEAK` (never observed).
- **Release/revoke + on-exit sweep** (`revoke_capability_in_cnode`, `exit_task`):
  revoke decrements `cap_refcount` once and reclaims the object if unreferenced;
  the revoked slot's generation is bumped so a stale cap no longer resolves; a
  double release fails cleanly (no underflow). Task exit sweeps live reply caps
  (`REVOKE_ON_EXIT` / `REVOKE_ON_EXIT_OK`) — the sweep itself is UNCHANGED.

**Markers (Task C/D).** All default-off behind `cap_cnode_enabled()`:
- Lookup: `CAP_CNODE_LOOKUP_BEGIN`/`_OK`/`_FAIL reason=invalid|stale_generation|missing_right|wrong_kind`.
- Reserve: `CAP_CNODE_RESERVE_BEGIN`/`_OK`/`_FAIL reason=full`.
- Materialize/mint: `CAP_CNODE_MATERIALIZE_BEGIN`, `CAP_CNODE_REF_INC`,
  `CAP_CNODE_SLOT_INSTALL`, `CAP_CNODE_MATERIALIZE_OK`,
  `CAP_CNODE_MATERIALIZE_ROLLBACK_BEGIN`/`_OK`, `CAP_CNODE_MATERIALIZE_FAIL`.
- Transfer: `CAP_CNODE_TRANSFER_BEGIN`, `_RESERVE_OK`, `_MATERIALIZE_OK`,
  `_ROLLBACK_BEGIN`/`_OK`, `_FAIL reason=receiver_full|missing_right|stale|internal`,
  `_DONE`.
- Reply: `CAP_CNODE_REPLY_MATERIALIZE_BEGIN`/`_OK`,
  `CAP_CNODE_REPLY_CONSUME_BEGIN`/`_OK`, `CAP_CNODE_REPLY_DOUBLE_CONSUME_BLOCKED`.
- Release/revoke: `CAP_CNODE_RELEASE_BEGIN`, `_SLOT_CLEAR`, `_REF_DEC`, `_OK`,
  `_FAIL reason=invalid|stale|underflow`, `CAP_CNODE_REVOKE_ON_EXIT`/`_OK`.
- Invariants (fatal in smoke): `CAP_CNODE_INVARIANT_OK`,
  `CAP_CNODE_REFCOUNT_UNDERFLOW`, `CAP_CNODE_SLOT_LEAK`,
  `CAP_CNODE_STALE_CAP_ACCEPTED`, `CAP_CNODE_RIGHTS_ESCALATION`,
  `CAP_CNODE_ROLLBACK_LEAK`.

**Workload (Task E).** A default-off, deterministic, bounded **one-shot** cap/CNode
lifecycle proof (`maybe_run_cap_cnode_proof`) is hooked in the arch-neutral timer
path and runs once when a real user task (tid != 0) with a CNode is current. It
reserves + materializes exactly one scratch MemoryObject cap, looks it up (rights
subset — no escalation), revokes it, confirms the stale cap no longer resolves,
confirms a double release fails without underflow, and verifies the object returns
to baseline (`INVARIANT_OK`) — consuming no net slots and changing no service
state. The reply/transfer production markers fire naturally from the boot's spawn
IPC (reply caps + a cap transfer occur every boot under IPC-FINAL); the hot
mint/lookup success paths are NOT instrumented (only their error/rollback edges),
to bound marker volume.

**`CAP_CNODE=1` acceptance profile (Task F/G).** Requires `CAP_CNODE_ENABLED`,
`CAP_CNODE_LOOKUP_OK`, `CAP_CNODE_RESERVE_OK`, one of
`CAP_CNODE_MATERIALIZE_OK`/`CAP_CNODE_TRANSFER_MATERIALIZE_OK`, one of
`CAP_CNODE_RELEASE_OK`/`CAP_CNODE_REVOKE_ON_EXIT_OK`, and `CAP_CNODE_INVARIANT_OK`;
fails hard on `CAP_CNODE_REFCOUNT_UNDERFLOW`, `CAP_CNODE_SLOT_LEAK`,
`CAP_CNODE_STALE_CAP_ACCEPTED`, `CAP_CNODE_RIGHTS_ESCALATION`,
`CAP_CNODE_ROLLBACK_LEAK`, `CAP_CNODE_MATERIALIZE_FAIL`, a `CAP_CNODE_TRANSFER_FAIL`
without a matching `CAP_CNODE_TRANSFER_ROLLBACK_OK`, `CapabilityFull`,
`TaskTableFull`, `BLOCKED_WOULDBLOCK_FATAL`, and the fatal breadcrumbs
(`^!Fv`/`^!BNv`/`DOUBLE_FAULT`/`TRIPLE`/`PANIC`/`FATAL`,
`PAGE_FAULT_UNHANDLED`/`PAGE_FAULT_FATAL`/`PAGE_FAULT_NOT_HANDLED`). Handled
COW/DEMAND page faults remain accepted. Mode isolation forces `CAP_CNODE` off under
`D6_SWITCH_PROOF` / `D6_SWITCH_A`; `CAP_CNODE` is standalone (does not enable a
D6/D2 mode and is not auto-enabled by the IPC proof workloads). Guarded by
`stage173_cap_cnode`.

Acceptance (user QEMU): (1) `CAP_CNODE=1` smoke (primary); (2) `VM_COW=1` smoke;
(3) `IPC_FINAL=1` oracle; (4) `D2_RECV_GENUINE=1`; (5) `D2_SEND_GENUINE=1`; (6)
`SCHED_TIMEOUT=1`; (7) normal smoke; (8) `D6_SWITCH_A=1`; (9) 5-min
`D6_SWITCH_PROOF=1`; (10) Stage 163P sender-wake oracle.

**ACCEPTED (user QEMU, 2026).** The primary `CAP_CNODE=1` run passed:
`CAP_CNODE_ENABLED`, `CAP_CNODE_LOOKUP_OK`, `CAP_CNODE_RESERVE_OK`,
`CAP_CNODE_MATERIALIZE_OK`, `CAP_CNODE_RELEASE_OK`, and `CAP_CNODE_INVARIANT_OK`
present; the deterministic one-shot proof completed `CAP_CNODE_PROOF_DONE
result=ok`; the stale lookup and the double-release were rejected; and none of the
invariant-violation markers (`CAP_CNODE_REFCOUNT_UNDERFLOW`, `CAP_CNODE_SLOT_LEAK`,
`CAP_CNODE_STALE_CAP_ACCEPTED`, `CAP_CNODE_RIGHTS_ESCALATION`,
`CAP_CNODE_ROLLBACK_LEAK`) appeared. Stage 173 CAP-CNODE is **ACCEPTED** — it was
instrumentation-only (the cap/CNode lifecycle was already transactional). The one
follow-up was **Stage 173B**, a smoke-script false positive: the `D2_SEND_GENUINE=1`
regression tripped on a generic `PAGE_FAULT` token emitted by the sender-wake
workload's handled COW faults (`PAGE_FAULT_ENTRY … PAGE_FAULT_HANDLED_COW`, no
`PAGE_FAULT_UNHANDLED`/`_FATAL`/`_NOT_HANDLED` and no crash breadcrumb). The
`D2_SEND_GENUINE` fatal-after-send gate was narrowed to line-anchored crash
breadcrumbs (`^!Fv`/`^!BNv`/`DOUBLE_FAULT`/`TRIPLE`/`PANIC`/`FATAL`) plus the
explicit unhandled/fatal page-fault markers only — generic `PAGE_FAULT` is no
longer a fatal token, matching the Stage 171B/VM-COW narrowing. No kernel/runtime
change; all real `D2_SEND_GENUINE` gates (required marker set, committed send-path
`DISPATCH_DEFERRED` + `NO_INLOCK_DISPATCH`, no in-lock `switch_required` fallback,
the three Stage 163P sender-wake markers, `BLOCKED_WOULDBLOCK_FATAL`/`CapabilityFull`/
`TaskTableFull`) are preserved. Stage 174 (FAULT-DELIVERY) is the next frontier.

### 7.1.10 Stage 174 — FAULT-DELIVERY (kernel-fault → supervisor delivery + fault-channel lifecycle audit + diagnostics)

**Next kernel-unlocking frontier after CAP-CNODE.** Audits and instruments the
kernel-fault → supervisor delivery path and fault-channel lifecycle — fault
classification, supervisor fault-endpoint routing, direct blocked-recv completion,
queued delivery, faulting-task stop/cleanup, and supervisor restart/rebind — behind
a default-off arch-neutral diagnostic profile (`yarm.fault_delivery=1`, script
`FAULT_DELIVERY=1`, marker `FAULT_DELIVERY_ENABLED`). This is an audit +
instrumentation + hardening-verification stage: the delivery path is ALREADY
correct (handled faults never deliver; a blocked supervisor completes directly with
no queued duplicate; an idle supervisor queues exactly one message consumed exactly
once; kernel faults stay fatal), so **no fault/IPC runtime behavior changes** — the
markers only expose the phase boundaries. This is **instrumentation only**; **no
real bug** was found (the paths are pinned by pre-existing tests
`page_fault_report_falls_back_to_supervisor_endpoint_when_no_fault_handler` and
`kernel_fault_report_completes_blocked_supervisor_recv_v2_without_stranding_queue`).

**Explicitly NOT done in Stage 174:** no syscall count change (SYSCALL_COUNT=31,
Syscall::VARIANT_COUNT=23, x86_64 MAX_ADDRESS_SPACES=32); no fault-message ABI
change (`SupervisorFaultReportWire` unchanged — the `decode` helper was merely
un-gated from `#[cfg(test)]` for the proof's round-trip check); no D2/D6/IPC-FINAL
behavior change; no VM/COW behavior change; no CNode/cap behavior change; no SMP
fault-IPI broadening; no AArch64/RISC-V D6 switch unlock (the knob is arch-neutral
diagnostics / no-op behavior); no RPi5 change.

**Audit (Task B).** Paths audited and confirmed already correct — no bug fixed,
diagnostics only:
1. **Handled page fault** (`handle_trap` PageFault arm, fault_state.rs): only WRITE
   faults route to COW; COW/demand handled faults emit `PAGE_FAULT_HANDLED_COW` /
   `PAGE_FAULT_HANDLED_DEMAND` and `return Ok(())` BEFORE any supervisor delivery
   (`FAULT_DELIVERY_CLASSIFY_HANDLED kind=cow|demand`). Not delivered.
2. **Unhandled user fault** (`fault_current_task_with_fault` →
   `emit_fault_report_for_fault`): classify user-unhandled, build the
   `SupervisorFaultReportWire` (faulting tid/addr/access), route to
   `fault_handler_endpoint` else `supervisor_endpoint`, deliver, then stop the task
   per policy (`FaultPolicy::NotifyAndContinue` returns without stopping).
3. **Kernel fault/fatal** (`handle_trap` Unknown arm): stays FATAL
   (`FAULT_DELIVERY_CLASSIFY_KERNEL_FATAL`), never reclassified as a
   supervisor-deliverable user fault.
4. **Supervisor already blocked** on the fault endpoint: `complete_blocked_recv_for_waiter`
   completes the recv inline and clears the waiter — nothing is left queued
   (`FAULT_DELIVERY_DIRECT_RECV_DONE`; a residual queued/​waiter count would emit
   `FAULT_DELIVERY_DUPLICATE_MSG` / `FAULT_DELIVERY_ORPHANED_WAITER`).
5. **Supervisor not blocked**: `send_message_to_endpoint_and_wake` queues exactly
   one message (`FAULT_DELIVERY_QUEUE_OK`); a later recv consumes it exactly once.
6. **Supervisor restart** (`restart_task`): the restart token is validated first, so
   the fault channel (endpoint index/generation) stays valid across the restart; the
   task rebinds to the same channel with no stale sender/reply cap or orphaned waiter
   (`FAULT_DELIVERY_RESTART_TOKEN_OK` / `_CHANNEL_REBIND_OK` / `_SUPERVISOR_RESTART_OK`).
7. **Exiting faulting task** (`exit_task`): `clear_ipc_waiters_for_tid` sweeps the
   task's queued/waiting IPC references so no dangling fault-channel reference
   remains (`FAULT_DELIVERY_TASK_CLEANUP_OK`).
8. **Repeated fault from same task**: each unhandled fault enqueues one bounded
   message into the fixed-depth endpoint queue (`QUEUE_FULL` on overflow — no
   unbounded leak); duplicates are neither coalesced nor stranded (current policy:
   one message per delivery, bounded by endpoint depth).

**Lock/rank story.** Fault delivery routes through the endpoint/IPC domain (rank 3)
and, for direct completion, the task domain (rank 2 < 3) — the same order as normal
IPC send; capability materialization (rank 4) is only touched for reply/transfer
caps, not fault reports. No lower-rank lock is taken while a higher-rank lock is
held. The domain remains under the single global `SpinLock<KernelState>` (no
relocation out of the global lock — a later stage).

**Markers (Task C/D).** All default-off behind `fault_delivery_enabled()`:
classification (`FAULT_DELIVERY_CLASSIFY_BEGIN`/`_HANDLED`/`_USER_UNHANDLED`/`_KERNEL_FATAL`),
message build (`_MSG_BUILD_BEGIN`/`_OK`/`_FAIL`), endpoint lookup
(`_ENDPOINT_LOOKUP_BEGIN`/`_OK`/`_FAIL`), direct recv (`_DIRECT_RECV_BEGIN`/`_WRITEBACK_OK`/`_DONE`),
queue (`_QUEUE_BEGIN`/`_OK`/`_FULL`, `_DEQUEUE_BEGIN`/`_OK`), task state
(`_TASK_STOP_BEGIN`/`_OK`, `_TASK_CLEANUP_BEGIN`/`_OK`), restart
(`_SUPERVISOR_RESTART_BEGIN`/`_OK`, `_CHANNEL_REBIND_OK`, `_RESTART_TOKEN_OK`), and
invariants (`_INVARIANT_OK`, `_STRANDED_QUEUE`, `_DUPLICATE_MSG`, `_ORPHANED_WAITER`,
`_STALE_SUPERVISOR`, `_BAD_SENDER`, `_WRITEBACK_FAIL`, `_QUEUE_LEAK`).

**Workload (Task E).** A default-off, deterministic, bounded **one-shot**
fault-delivery proof (`maybe_run_fault_delivery_proof`) is hooked in the
arch-neutral timer path and runs once when a real user task (tid != 0) with a CNode
is current. It is **self-contained**: it creates a SCRATCH endpoint (never the real
supervisor channel), classifies a synthetic user-unhandled fault, builds a real
`SupervisorFaultReportWire`, queues exactly one message, dequeues it directly from
the endpoint queue (`Endpoint::recv` — it never blocks or dispatches the live
current task), verifies the report round-trips and the queue is empty
(`FAULT_DELIVERY_INVARIANT_OK`), then tears down (revokes the scratch caps + frees
the endpoint slot) — consuming no net slots and disturbing no real service. The live
classify markers additionally fire naturally on the boot's handled COW faults; the
direct-recv / task-stop / restart markers fire on the live path when a real
supervisor fault occurs.

**`FAULT_DELIVERY=1` acceptance profile (Task F/G).** Requires `FAULT_DELIVERY_ENABLED`,
`FAULT_DELIVERY_CLASSIFY_USER_UNHANDLED`, `FAULT_DELIVERY_MSG_BUILD_OK`, one of
`FAULT_DELIVERY_DIRECT_RECV_DONE`/`FAULT_DELIVERY_DEQUEUE_OK`, and
`FAULT_DELIVERY_INVARIANT_OK`; if a task-stop began it must complete
(`_TASK_STOP_OK`). Fails hard on `FAULT_DELIVERY_STRANDED_QUEUE`,
`_DUPLICATE_MSG`, `_ORPHANED_WAITER`, `_STALE_SUPERVISOR`, `_BAD_SENDER`,
`_WRITEBACK_FAIL`, `_QUEUE_LEAK`, `CapabilityFull`, `TaskTableFull`,
`BLOCKED_WOULDBLOCK_FATAL`, the fatal breadcrumbs
(`^!Fv`/`^!BNv`/`DOUBLE_FAULT`/`TRIPLE`/`PANIC`/`FATAL`), `PAGE_FAULT_FATAL`/
`PAGE_FAULT_NOT_HANDLED`, and a `PAGE_FAULT_UNHANDLED` that escaped WITHOUT a
fault-delivery classification. A generic `PAGE_FAULT` is NOT fatal, and handled
COW/DEMAND faults remain accepted (same as Stage 171B/173B). Mode isolation forces
`FAULT_DELIVERY` off under `D6_SWITCH_PROOF` / `D6_SWITCH_A`; it is standalone (does
not enable a D6/D2 mode and is not auto-enabled by the IPC proof workloads). Guarded
by `stage174_fault_delivery`.

Acceptance (user QEMU): (1) `FAULT_DELIVERY=1` smoke (primary); (2)
`D2_SEND_GENUINE=1`; (3) `CAP_CNODE=1`; (4) `IPC_FINAL=1` oracle; (5) `VM_COW=1`;
(6) `SCHED_TIMEOUT=1`; (7) `D2_RECV_GENUINE=1`; (8) normal smoke; (9)
`D6_SWITCH_A=1`; (10) 5-min `D6_SWITCH_PROOF=1`; (11) Stage 163P sender-wake
oracle.

**ACCEPTED (user QEMU, 2026).** The full matrix passed. The primary
`FAULT_DELIVERY=1` run reached the service baseline with `FAULT_DELIVERY_ENABLED`,
`FAULT_DELIVERY_CLASSIFY_USER_UNHANDLED`, `FAULT_DELIVERY_MSG_BUILD_OK`,
`FAULT_DELIVERY_DEQUEUE_OK`, and `FAULT_DELIVERY_INVARIANT_OK` present, the
one-shot self-contained proof completing cleanly, and none of the
invariant-violation markers (`FAULT_DELIVERY_STRANDED_QUEUE`/`_DUPLICATE_MSG`/
`_ORPHANED_WAITER`/`_STALE_SUPERVISOR`/`_BAD_SENDER`/`_WRITEBACK_FAIL`/`_QUEUE_LEAK`)
or fatal breadcrumbs present; handled COW faults were accepted. The D2_SEND /
CAP_CNODE / IPC_FINAL / VM_COW / SCHED_TIMEOUT / D2_RECV / normal / D6_SWITCH_A /
5-min D6_SWITCH_PROOF / sender-wake regressions all passed. Stage 174 FAULT-DELIVERY
is **ACCEPTED** — it was instrumentation-only (the fault-delivery path was already
correct). Stage 175 (SPAWN-LIFECYCLE) is the next frontier.

### 7.1.11 Stage 175 — SPAWN-LIFECYCLE (spawn / image-loading / lifecycle metadata audit + diagnostics)

**Next kernel-unlocking frontier after FAULT-DELIVERY.** Audits and instruments the
spawn / image-loading / lifecycle-metadata path — the init → process_manager spawn
request handling, PM image-id resolution + initramfs/VFS image loading, ELF
parse/load/zero-copy grant, process/thread creation metadata, CNode/bootstrap-cap
setup, address-space creation + rollback, kernel task-table/TCB lifecycle, and the
service startup sequence — behind a default-off arch-neutral diagnostic profile
(`yarm.spawn_lifecycle=1`, script `SPAWN_LIFECYCLE=1`, marker
`SPAWN_LIFECYCLE_ENABLED`). This is an audit + instrumentation stage: the spawn path
is ALREADY transactional (each ELF-parse / image-missing / CNode-full / VM-full /
task-table-full / cap-materialization failure rolls back), and **PM policy stays in
userspace**, so **no spawn/PM runtime behavior changes** — the markers only expose
the phase boundaries. This is **instrumentation only**; **no real bug** was found.

**Explicitly NOT done in Stage 175:** no syscall count change (SYSCALL_COUNT=31,
Syscall::VARIANT_COUNT=23, x86_64 MAX_ADDRESS_SPACES=32); no syscall/IPC ABI change;
no image-id change; no service ABI change; no PM policy change (policy stays in
userspace); no D2/D6/IPC-FINAL/VM-COW/SCHED-TIMEOUT/CAP-CNODE/FAULT-DELIVERY behavior
change; no D3/D5 live-wire; no SMP change; no AArch64/RISC-V D6 switch unlock (the
knob is arch-neutral diagnostics / no-op behavior); no RPi5 change.

**Audit (paths).** All confirmed already transactional — no bug fixed, diagnostics
only:
- **Spawn request handling** (`SpawnV5` / `SpawnFromInitramfsFile`): the kernel
  validates the request, resolves the image, parses+loads the ELF, creates the
  address space + TCB + CNode, materializes bootstrap caps, and marks the thread
  runnable — with a single rollback path that unwinds every partial resource
  (address space, TCB, caps) on any failure.
- **Image resolution + loading**: PM resolves an image id to an initramfs/VFS image;
  a missing image is a clean typed error (no partial task).
- **ELF parse/load/zero-copy grant**: parse validates headers before any mapping;
  the zero-copy grant maps initramfs-backed frames read-only/shared; a parse or map
  failure rolls back the mapped prefix.
- **Address-space / TCB / CNode / bootstrap-cap setup**: each is created in order and
  torn down in reverse on failure (address space destroyed, TCB freed, caps revoked).
- **Service startup sequence**: `initramfs_srv`, `devfs_srv`, `vfs_server`,
  `driver_manager`, `blkcache_srv`, `virtio_blk_srv`, optional `ramfs`/`ext4`
  servers — started in a fixed dependency order that the smoke's service-baseline
  markers already gate.
- **Zombie/exit/restart cleanup**: a spawn that later exits/restarts routes through
  the Stage 173/174 cap-revoke + fault-cleanup sweeps (no zombie/cap/aspace/TCB leak).

**Markers.** All default-off behind `spawn_lifecycle_enabled()`:
`SPAWN_LIFECYCLE_REQUEST_BEGIN`, `_IMAGE_RESOLVE_OK`/`_FAIL`, `_ELF_PARSE_BEGIN`/`_OK`,
`_ELF_LOAD_BEGIN`/`_OK`, `_ZC_LOAD_OK`, `_ASPACE_CREATE_OK`, `_TCB_ALLOC_OK`,
`_CNODE_SETUP_OK`, `_BOOTSTRAP_CAPS_OK`, `_THREAD_READY`, `_PROCESS_READY`,
`_SERVICE_READY`, `_ROLLBACK_BEGIN`/`_OK`, `_INVARIANT_OK`; failure markers
`_ROLLBACK_LEAK`, `_ZOMBIE_LEAK`, `_CAP_LEAK`, `_ASPACE_LEAK`, `_TCB_LEAK`,
`_DUPLICATE_TID`, `_BAD_IMAGE_ID`, `_SERVICE_ORDER_VIOLATION`.

**Stage 175B — `_DUPLICATE_TID` false-positive fix.** The first `SPAWN_LIFECYCLE=1`
run reached the service baseline with successful spawn/invariant markers but tripped
because `SPAWN_LIFECYCLE_DUPLICATE_TID` was emitted for the bootstrap tasks tid=2/3/1
immediately before their own `TCB_ALLOC_OK` / `PROCESS_READY` / `INVARIANT_OK` (the
later 10000+ service tids never did). The cause was an instrumentation false
positive: a *pre-register presence scan* flagged the legitimately pre-reserved
bootstrap TCB slot as a duplicate. The fix removes the pre-register scan and keeps
`_DUPLICATE_TID` gated solely on the *post-register* `tcb_count > 1` invariant — a
true second live TCB for the same tid. Instrumentation-only; no runtime spawn
behavior change, and the smoke still hard-fails on a real `_DUPLICATE_TID`. Guarded
by `stage175b_duplicate_tid_gate`.

**`SPAWN_LIFECYCLE=1` acceptance profile.** Requires `SPAWN_LIFECYCLE_ENABLED`, at
least one successful spawn path, the service baseline, and (when exercised) the
rollback/invariant diagnostics; fails hard on the failure markers plus
`CapabilityFull`, `TaskTableFull`, `PAGE_FAULT_UNHANDLED`/`_FATAL`/`_NOT_HANDLED`,
and the fatal breadcrumbs (`^!Fv`/`^!BNv`/`DOUBLE_FAULT`/`TRIPLE`/`PANIC`/`FATAL`).
Handled COW/DEMAND faults remain accepted. Mode isolation forces `SPAWN_LIFECYCLE`
off under `D6_SWITCH_PROOF` / `D6_SWITCH_A`; it is standalone (does not enable a
D6/D2 mode). Guarded by `stage175_spawn_lifecycle`.

Acceptance (user QEMU): (1) `SPAWN_LIFECYCLE=1` smoke (primary); (2)
`FAULT_DELIVERY=1`; (3) `CAP_CNODE=1`; (4) `VM_COW=1`; (5) `SCHED_TIMEOUT=1`; (6)
`IPC_FINAL=1` oracle; (7) `D2_RECV_GENUINE=1`; (8) `D2_SEND_GENUINE=1`; (9) normal
smoke; (10) `D6_SWITCH_A=1`; (11) 5-min `D6_SWITCH_PROOF=1`; (12) Stage 163P
sender-wake oracle.

**PRIMARY ACCEPTED (user QEMU, 2026).** The primary `SPAWN_LIFECYCLE=1` run reached
the service baseline with the successful spawn phase markers (`REQUEST_BEGIN` →
`IMAGE_RESOLVE_OK` → `ELF_PARSE_OK` → `ELF_LOAD_OK`/`ZC_LOAD_OK` →
`ASPACE_CREATE_OK`/`TCB_ALLOC_OK`/`CNODE_SETUP_OK`/`BOOTSTRAP_CAPS_OK` →
`THREAD_READY` → `PROCESS_READY` → `SERVICE_READY`), the one-shot rollback proof
completing (`ROLLBACK_BEGIN`/`_OK`, `INVARIANT_OK`), and — after the **Stage 175B**
fix — no `SPAWN_LIFECYCLE_DUPLICATE_TID` false positive for the bootstrap tids, no
leak markers, and no fatal breadcrumbs. Stage 175/175B SPAWN-LIFECYCLE primary is
**ACCEPTED**; it was instrumentation-only (the spawn path was already transactional
and PM policy stays in userspace). The full regression matrix (rows 2–12) is
**PENDING user QEMU re-run**. Stage 176 (GLOBAL-STATE) is the next frontier.

### 7.1.12 Stage 176 — GLOBAL-STATE (remaining direct global-KernelState mutation audit + rank discipline + diagnostics)

**Next kernel-unlocking frontier after SPAWN-LIFECYCLE.** Audits the remaining
direct global `KernelState` mutation sites and enforces the owner/helper/rank
discipline, behind a default-off arch-neutral diagnostic profile
(`yarm.global_state=1`, script `GLOBAL_STATE=1`, marker `GLOBAL_STATE_ENABLED`).
This is a **conservative** audit + instrumentation stage: it classifies every
remaining global-root site, verifies the lock-rank ordering and the no-guard-held
invariants at runtime, and documents the high-risk sites left unchanged with a
follow-up target — so **no runtime behavior changes**. This is **instrumentation
only**; **no real bug** was found.

**Explicitly NOT done in Stage 176:** no syscall count change (SYSCALL_COUNT=31,
Syscall::VARIANT_COUNT=23, x86_64 MAX_ADDRESS_SPACES=32); no syscall/IPC/service/
image ABI change; no PM policy or userspace service-order change; no deletion of the
`D6_SWITCH_A`/`D6_SWITCH_PROOF`/`D6_GENUINE`/`D2_RECV_GENUINE`/`D2_SEND_GENUINE`
fallback paths; no D3/D5 live-wire; no SMP broadening; no RPi5 change; no AArch64/
RISC-V D6 switch unlock (the knob is arch-neutral diagnostics / no-op behavior).

**Lock-rank discipline.** The lock domains are ranked scheduler (1) → task (2) →
IPC (3) → capability (4) → VM (5) → memory/page-table (6). The audit verifies this
ordering is monotonic and that no new nested inversion is introduced; the runtime
proof re-checks the ordering (`GLOBAL_STATE_RANK_ORDER_OK`) and that no global guard
is held across a user-memory copy, an IPC writeback, or `switch_frames`
(`GLOBAL_STATE_NO_LEAKED_GLOBAL_GUARD`).

**Direct global-root site classification.** The remaining direct global
`KernelState` roots are classified and documented:
- **Legitimate trap-entry orchestration root** (`handle_trap_entry_shared` /
  `with_cpu`): the single authoritative entry that owns the global guard for the
  trap; it drops the guard before `switch_frames` on the single-CPU x86_64/AArch64
  production paths (Stage 117) and before the out-of-lock D6-GENUINE / D2 dispatch
  drains. **Allowed** (root orchestration).
- **Already-decomposed owner/helper calls**: the `with_scheduler_split_mut` (rank 1),
  `with_task_tcbs_split_mut` (rank 2), `with_ipc_split_mut` (rank 3),
  `with_vm_user_spaces_split_mut` (rank 5), `with_memory_split_mut` (rank 6) seams.
  **Owner/helper OK**.
- **Temporary compatibility/fallback sites**: the in-lock D6/D2 dispatch fallbacks
  (`reason=switch_required` / multi-CPU / no-trap-drainer) that remain under the
  global lock until the SMP-ready lock-free shootdown/IPI design lands. **Allowed
  with reason**; follow-up target: SMP-ready D2-GENUINE / D3 / D5 / cross-arch D6.
- No **unauthorized** direct field mutation outside the approved owner/helper
  functions was found.

**Markers.** All default-off behind `global_state_enabled()`:
`GLOBAL_STATE_ENABLED`, `GLOBAL_STATE_AUDIT_BEGIN`,
`GLOBAL_STATE_SITE_CLASSIFIED kind=…`, `GLOBAL_STATE_OWNER_HELPER_OK`,
`GLOBAL_STATE_DIRECT_SITE_ALLOWED reason=…`, `GLOBAL_STATE_DIRECT_SITE_REJECTED`,
`GLOBAL_STATE_RANK_ORDER_OK`/`_FAIL`, `GLOBAL_STATE_NO_LEAKED_GLOBAL_GUARD`,
`GLOBAL_STATE_SEAM_INVARIANT_OK`, `GLOBAL_STATE_INVARIANT_OK`,
`GLOBAL_STATE_PROOF_DONE result=ok`; failure markers
`GLOBAL_STATE_DIRECT_MUTATION_LEAK`, `_RANK_INVERSION`,
`_GUARD_HELD_ACROSS_USER_COPY`, `_GUARD_HELD_ACROSS_SWITCH`,
`_GUARD_HELD_ACROSS_IPC_WRITEBACK`, `_OWNER_HELPER_BYPASS`, `_UNCLASSIFIED_SITE`,
`_INVARIANT_FAIL`.

**Workload.** A default-off, deterministic **one-shot** global-state audit
(`maybe_run_global_state_audit`) hooked in the arch-neutral timer path runs once
when a real user task is current. It emits the site classifications, re-checks the
lock-rank ordering is monotonic (rank inversion → `RANK_ORDER_FAIL`), confirms no
global guard is leaked at the audit point, and emits the seam/overall invariants —
touching no service state.

**`GLOBAL_STATE=1` acceptance profile.** Requires `GLOBAL_STATE_ENABLED`, the
owner/helper + direct-site-allowed classifications, `GLOBAL_STATE_RANK_ORDER_OK`,
`GLOBAL_STATE_NO_LEAKED_GLOBAL_GUARD`, `GLOBAL_STATE_INVARIANT_OK`, and
`GLOBAL_STATE_PROOF_DONE`; fails hard on every `GLOBAL_STATE_*` failure marker plus
`CapabilityFull`, `TaskTableFull`, `BLOCKED_WOULDBLOCK_FATAL`,
`PAGE_FAULT_UNHANDLED`/`_FATAL`/`_NOT_HANDLED`, and the fatal breadcrumbs
(`^!Fv`/`^!BNv`/`DOUBLE_FAULT`/`TRIPLE`/`PANIC`/`FATAL`). Handled COW/DEMAND faults
remain accepted. Mode isolation forces `GLOBAL_STATE` off under `D6_SWITCH_PROOF` /
`D6_SWITCH_A`; it is standalone. Guarded by `stage176_global_state`.

**Deferred follow-ups.** The temporary in-lock D2/D6 dispatch fallbacks and the
rank-5/6 VM/memory seams' broader out-of-global-lock relocation are deferred to the
SMP-ready lock-free shootdown/IPI design (D2-GENUINE full live-wire, D3, D5, and
cross-arch D6 switch unlock). None are live-wired here.

Acceptance (user QEMU): (1) `GLOBAL_STATE=1` smoke (primary); (2)
`SPAWN_LIFECYCLE=1`; (3) `FAULT_DELIVERY=1`; (4) `CAP_CNODE=1`; (5) `VM_COW=1`; (6)
`SCHED_TIMEOUT=1`; (7) `IPC_FINAL=1` oracle; (8) `D2_RECV_GENUINE=1`; (9)
`D2_SEND_GENUINE=1`; (10) normal smoke; (11) `D6_SWITCH_A=1`; (12) 5-min
`D6_SWITCH_PROOF=1`; (13) Stage 163P sender-wake oracle.

**PRIMARY ACCEPTED (user QEMU, 2026).** The primary `GLOBAL_STATE=1` run reached the
service baseline with `GLOBAL_STATE_ENABLED`, the site classifications
(`OWNER_HELPER_OK` + `DIRECT_SITE_ALLOWED`), `RANK_ORDER_OK`,
`NO_LEAKED_GLOBAL_GUARD`, `INVARIANT_OK`, and `PROOF_DONE result=ok`, and none of the
`GLOBAL_STATE_*` failure markers or fatal breadcrumbs. Stage 176 GLOBAL-STATE primary
is **ACCEPTED**; it was instrumentation-only (no unauthorized direct global mutation
site was found). The full regression matrix (rows 2–13) is **PENDING user QEMU
re-run**. Stage 177 (SMP-READY) is the next frontier.

### 7.1.13 Stage 177 — SMP-READY (x86_64 SMP audit + AP bring-up / per-CPU / remote-wake readiness diagnostics)

**Next kernel-unlocking frontier after GLOBAL-STATE.** Audits and instruments x86_64
SMP readiness — AP bring-up, per-CPU scheduler/idle state, and remote-wake/IPI
readiness — behind a default-off arch-neutral diagnostic profile
(`yarm.smp_ready=1`, script `SMP_READY=1`, marker `SMP_READY_ENABLED`). This landed
as **Option B (audit/refactor complete, explicit fallback markers, no behavior
change)** — see the honesty note below. It is **instrumentation only**; **no real
bug** was found.

**x86_64 SMP audit result.** The audit found the SMP module is ALREADY in good shape:
- **Trampoline/Rust split already done** (Stage 108, AI_AGENT_RULES §5.2): the AP
  16/32/64-bit startup assembly + trampoline-page encoding live in the sibling
  `smp_trampoline` module; `smp.rs` keeps only the Rust bring-up (LAPIC INIT-SIPI
  sequencing, handoff construction, AP online/park accounting). **No further split
  was needed** and none was forced (splitting a boot-critical path without QEMU
  evidence would be reckless).
- **AP bring-up state:** the BSP sends INIT-SIPI-SIPI; the AP reaches the trampoline
  (`X86_AP_TRAMPOLINE_REACHED`), publishes Rust-online (`X86_AP_ENTER_RUST`), gets a
  per-CPU record + env scaffold, then **parks in a `cli`/`hlt` loop**
  (`X86_AP_RUST_PARK reason=no_ap_scheduler_yet`). Per-CPU **TSS/IDT/GS are honestly
  DEFERRED** (the AP shares the trampoline-inherited GDT while IRQ-masked — safe for
  a parked AP, NOT a production per-CPU env; no fake readiness).
- **Per-CPU scheduler/idle state:** each CPU has a unique kernel/AP stack
  (`ap_stack_top` is strictly increasing in CPU id — verified by the audit); the
  **production scheduler stays BSP-only** (`online_cpus=1`, `scheduler_aps=0`). APs
  are never admitted to the run queue and run no user tasks.
- **Timer/IPI/remote-wake readiness:** the boot-CPU timer is live; **remote-wake and
  IPI-driven scheduling are NOT live** — they are recorded as honest deferrals
  (`SMP_READY_REMOTE_WAKE_DEFERRED reason=smp_not_live|ipi_not_live`,
  `SMP_READY_IPI_DEFERRED reason=not_live`). The success markers
  (`SMP_READY_REMOTE_WAKE_OK`/`_IPI_SEND_OK`/`_IPI_RECV_OK`) are gated on the
  never-today `online > 1` path and never fire in this stage.

**What is accepted now vs deferred.** Accepted: the `-smp 1` baseline is fully
preserved; the opt-in `SMP_READY` profile audits the boot CPU + per-CPU invariants
and, under `-smp 2/4`, exercises the real AP bring-up to the parked-online state with
honest mirror markers. Deferred (later stages): admitting APs to the production
scheduler, a real per-CPU TSS/IDT/GS, and live IPI/remote-wake — the whole
lock-free-shootdown/IPI design. **This stage does NOT broaden production SMP.**

**Relationship to D6/D2 and GLOBAL-STATE.** `D6_SWITCH_A`/`D6_GENUINE`/`D2_RECV`/
`D2_SEND` remain **x86_64 `-smp 1` only**; nothing here broadens them. The audit
re-checks the same lock-domain rank ordering as Stage 176 GLOBAL-STATE
(`SMP_READY_RANK_ORDER_OK`) and the no-leaked-global-guard invariant
(`SMP_READY_GLOBAL_STATE_OK`).

**Markers.** All default-off behind `smp_ready_enabled()`: `SMP_READY_ENABLED`,
`SMP_READY_AUDIT_BEGIN`, `SMP_READY_BOOT_CPU_OK`, AP mirror
(`_AP_TRAMPOLINE_BEGIN`/`_AP_ENTRY_OK`/`_AP_STACK_OK`/`_AP_GDT_IDT_OK`/`_AP_TSS_OK`/
`_AP_ONLINE`/`_AP_IDLE_OK`/`_AP_FALLBACK reason=…`), per-CPU
(`_PERCPU_CURRENT_OK`/`_PERCPU_ASID_OK`/`_PERCPU_STACK_UNIQUE_OK`/`_PERCPU_NO_CLOBBER_OK`),
scheduler (`_SCHED_ONLINE_BEGIN`/`_OK`, `_RUNQUEUE_LOCAL_OK`,
`_REMOTE_WAKE_BEGIN`/`_DEFERRED`, `_IDLE_WITH_RUNNABLE_SAFE`), IPI/timer
(`_IPI_DEFERRED`, `_TIMER_CPU_OK`; success `_REMOTE_WAKE_OK`/`_IPI_SEND_OK`/`_IPI_RECV_OK`
gated on `online>1`), invariants (`_GLOBAL_STATE_OK`, `_RANK_ORDER_OK`,
`_INVARIANT_OK`, `_PROOF_DONE result=ok`); failure markers `_AP_BOOT_FAIL`,
`_AP_STACK_ALIAS`, `_AP_TSS_BAD`, `_PERCPU_CLOBBER`, `_CURRENT_TID_MISMATCH`,
`_ASID_MISMATCH`, `_REMOTE_WAKE_LOST`, `_IPI_LOST`, `_RUNQUEUE_CORRUPT`,
`_GLOBAL_GUARD_LEAK`, `_RANK_INVERSION`, `_INVARIANT_FAIL`.

**`SMP_READY=1` acceptance profile.** The normal smoke stays `-smp 1`; only the
`SMP_READY` profile raises `QEMU_SMP` to `SMP_READY_CPUS` (default 2, optionally 4).
Requires `SMP_READY_ENABLED`, `SMP_READY_BOOT_CPU_OK`, either an AP
online/idle OR an explicit `SMP_READY_AP_FALLBACK reason=…`, `SMP_READY_RANK_ORDER_OK`,
`SMP_READY_GLOBAL_STATE_OK`, `SMP_READY_INVARIANT_OK`, and `SMP_READY_PROOF_DONE`;
fails hard on every `SMP_READY_*` failure marker plus `CapabilityFull`,
`TaskTableFull`, `BLOCKED_WOULDBLOCK_FATAL`, `PAGE_FAULT_UNHANDLED`/`_FATAL`/
`_NOT_HANDLED`, and the fatal breadcrumbs. Handled COW/DEMAND faults remain accepted.
Mode isolation forces `SMP_READY` off under `D6_SWITCH_PROOF` / `D6_SWITCH_A` (keeping
those regressions `-smp 1`). Guarded by `stage177_smp_ready`.

Acceptance (user QEMU): primary `SMP_READY=1 SMP_READY_CPUS=2 QEMU_SMP=2` smoke
(optionally `SMP_READY_CPUS=4 QEMU_SMP=4` if `-smp 2` is stable); regression matrix:
`GLOBAL_STATE=1`, `SPAWN_LIFECYCLE=1`, `FAULT_DELIVERY=1`, `CAP_CNODE=1`, `VM_COW=1`,
`SCHED_TIMEOUT=1`, `IPC_FINAL=1` oracle, `D2_RECV_GENUINE=1`, `D2_SEND_GENUINE=1`,
normal `-smp 1` smoke, `D6_SWITCH_A=1`, 5-min `D6_SWITCH_PROOF=1`, Stage 163P
sender-wake oracle.

**ACCEPTED (user QEMU, 2026).** Both `SMP_READY=1 QEMU_SMP=2` and `SMP_READY=1
QEMU_SMP=4` passed and reached the x86_64 service baseline, with
`SMP_READY_ENABLED`, `SMP_READY_BOOT_CPU_OK`, `SMP_READY_RANK_ORDER_OK`,
`SMP_READY_GLOBAL_STATE_OK`, `SMP_READY_INVARIANT_OK`, and `SMP_READY_PROOF_DONE`
present; the smoke confirmed AP online/idle (or explicit fallback) with APs
parked / BSP-only and no fatal breadcrumbs. Stage 177 SMP-READY is **ACCEPTED** —
meaning **SMP audit/readiness only, not full production SMP scheduling**: remote
wake / IPI remain deferred, and the D2/D6 unlocked paths remain x86_64 `-smp 1`
only. Stage 178 (CROSS-ARCH-D6) is the next frontier.

### 7.1.14 Stage 178 — CROSS-ARCH-D6 (AArch64/RISC-V D6 restore-path audit + diagnostics)

**Next kernel-unlocking frontier after SMP-READY.** Audits and instruments the
AArch64 and RISC-V user-task restore / trap-return / dispatch / lock-drop readiness
for a D6-style global-lock-unlock path, behind a default-off arch-neutral diagnostic
profile (`yarm.cross_arch_d6=1`, script `CROSS_ARCH_D6=1`, marker
`CROSS_ARCH_D6_ENABLED`). This is a **conservative audit + diagnostic** stage:
AArch64/RISC-V do NOT copy the x86_64 kernel switch-frame model — their correct D6
model is **restore the selected user trapframe after the global lock is dropped**
(exception-return / `sret`), which is audited and instrumented but **not
live-wired** here. So **no runtime behavior changes**. This is **instrumentation
only**; **no real bug** was found. AArch64/RISC-V D6 live-restore is **DEFERRED**
(explicit fallback markers, no fake success).

**Why x86_64 `switch_frames` is not copied blindly.** x86_64 D6 (Stage 117/166/167)
drops the global lock and resumes via a kernel switch-frame / trampoline
(`switch_frames` + first-resume re-acquire) because its trap entry stashes a kernel
continuation. AArch64 and RISC-V resume user tasks by **restoring a user trapframe
and executing `eret` / `sret`** from the exception vector — there is no kernel
switch-frame to trampoline through. Forcing the x86_64 model onto them would be
incorrect; the D6 unlock model for these arches is "drop the global guard, then
restore the chosen incoming user trapframe (ELR/SPSR/SP + TTBR0/ASID for AArch64;
sepc/sstatus/sp + satp/ASID for RISC-V) and exception-return", which this stage
documents and instruments.

**AArch64 audit + model.** Trap entry/exit (`arch/aarch64`), the SVC/raw syscall
return, trapframe writeback ordering, and the user restore
(ELR_EL1 / SPSR_EL1 / SP_EL0 + TTBR0/ASID) are audited. Classification: the user
trapframe restore + TTBR0/ASID switch is **safe to OBSERVE** (the audit reads the
incoming task's restore state read-only); a **global-lock-dropped live restore is
DEFERRED** — the D6 lock-drop-before-`eret` relocation needs its own multi-CPU-safe
proof + smoke before it may go live. Model marker: `trapframe_eret`.

**RISC-V audit + model.** Trap entry/exit (`arch/riscv64`), the syscall return,
trapframe writeback (gated on `task_switched || ecall`, per Stage 163P), and the
user restore (sepc / sstatus / sp + satp/ASID) are audited. Classification: same as
AArch64 — **safe to OBSERVE**, **live restore DEFERRED**. Model marker:
`trapframe_sret`.

**Diagnostic proof.** Behind `cross_arch_d6_enabled()`, a one-shot per-arch audit
(`maybe_run_cross_arch_d6_audit`) runs when a real user task is current. It records
the arch model, confirms the global guard is NOT held at the observe point, reads
the incoming task's trapframe/ASID/current-tid restore state (read-only), verifies
current_tid/active_asid consistency and that no scheduler queue is double-advanced,
then emits the arch restore-readiness markers and an explicit **DEFERRED** for the
live lock-dropped restore (no fake `RESTORE_DONE` on a deferred arch). On x86_64 the
audit records `model=switch_frames` and defers to the already-accepted D6 path
(observe-only; it does not touch D6_SWITCH_A/D6_GENUINE).

**Markers.** All default-off behind `cross_arch_d6_enabled()`: `CROSS_ARCH_D6_ENABLED`,
`_AUDIT_BEGIN arch=…`, `_ARCH_MODEL arch=… model=…`, `_GLOBAL_DROPPED arch=…`,
`_RESTORE_CANDIDATE`/`_RESTORE_ENTER`/`_RESTORE_DONE`, `_FALLBACK arch=… reason=…`,
`_INVARIANT_OK`, `_PROOF_DONE arch=… result=ok`; AArch64 `_AARCH64_ELR_OK`/`_SPSR_OK`/
`_SP_OK`/`_TTBR0_ASID_OK`/`_ERET_READY`/`_DEFERRED`; RISC-V `_RISCV_SEPC_OK`/`_SSTATUS_OK`/
`_SP_OK`/`_SATP_ASID_OK`/`_SRET_READY`/`_DEFERRED`; failure markers
`_GLOBAL_GUARD_HELD`, `_BAD_TRAPFRAME`, `_BAD_ASID`, `_CURRENT_TID_MISMATCH`,
`_DOUBLE_DISPATCH`, `_RESTORE_FAIL`, `_UNSUPPORTED_MODEL`, `_INVARIANT_FAIL`.

**`CROSS_ARCH_D6=1` acceptance profile.** Requires `CROSS_ARCH_D6_ENABLED` and either
(`RESTORE_DONE` + `INVARIANT_OK` + `PROOF_DONE`) or (an explicit `FALLBACK`/`DEFERRED`
reason + `INVARIANT_OK` + `PROOF_DONE`); on AArch64/RISC-V today the honest path is
the DEFERRED branch. Fails hard on every `CROSS_ARCH_D6_*` failure marker plus
`CapabilityFull`, `TaskTableFull`, `BLOCKED_WOULDBLOCK_FATAL`,
`PAGE_FAULT_UNHANDLED`/`_FATAL`/`_NOT_HANDLED`, and the fatal breadcrumbs. Handled
COW/DEMAND faults remain accepted. On x86_64 core smoke, `CROSS_ARCH_D6` is forced off
under `D6_SWITCH_PROOF`/`D6_SWITCH_A` and does not disturb the accepted x86_64 D6
paths. Guarded by `stage178_cross_arch_d6`.

Acceptance (user QEMU): primary AArch64
`CROSS_ARCH_D6=1 QEMU_SMP=4 ./scripts/qemu-aarch64-core-smoke.sh`; primary RISC-V
`CROSS_ARCH_D6=1 QEMU_SMP=1 ./scripts/qemu-riscv64-core-smoke.sh`; plus the x86_64
regression matrix (SMP_READY / GLOBAL_STATE / SPAWN_LIFECYCLE / FAULT_DELIVERY /
CAP_CNODE / VM_COW / SCHED_TIMEOUT / IPC_FINAL / D2_RECV / D2_SEND / normal /
D6_SWITCH_A / 5-min D6_SWITCH_PROOF / sender-wake oracle). AArch64/RISC-V D6 live
restore is NOT claimed live — it is audited + deferred; live-wiring is a later stage
with its own multi-CPU proof.

**PARTIAL (user QEMU, 2026).** The AArch64 and RISC-V manual runs with
`yarm.cross_arch_d6=1` reached the service baseline and emitted `CROSS_ARCH_D6_ENABLED`
with no fatal breadcrumbs, but the one-shot audit markers
(`CROSS_ARCH_D6_AUDIT_BEGIN` / `_ARCH_MODEL` / the arch restore markers /
`_INVARIANT_OK` / `_PROOF_DONE`) were **absent** — the cmdline parse/apply works and
services boot, but the audit hook was placed only in the timer-interrupt path, whose
`tid != 0` gate is not satisfied on the AArch64/RISC-V timer tick (the tick fires with
the kernel/idle context current). This is an **instrumentation hook-placement bug, not
a D6 restore-path failure**. Fixed in **Stage 178B**: the read-only audit is
additionally invoked from the arch-neutral `Trap::Syscall` handling (which always runs
with the syscalling user task current on all three arches); the one-shot latch keeps it
a single audit. Behavior unchanged; AArch64/RISC-V live restore stays DEFERRED. Guarded
by `stage178b_cross_arch_d6_hook`.

**ACCEPTED (user QEMU, 2026).** After the Stage 178B hook fix, both arches emit the
full audit path. AArch64 (`CROSS_ARCH_D6=1 QEMU_SMP=4`) reached the service baseline
and emitted the complete `trapframe_eret` path (`ENABLED`, `AUDIT_BEGIN`, `ARCH_MODEL`,
`GLOBAL_DROPPED`, `RESTORE_CANDIDATE`, ELR/SPSR/SP/TTBR0-ASID/ERET readiness,
`AARCH64_DEFERRED reason=live_lock_drop_restore_needs_multicpu_proof`, `FALLBACK
reason=deferred_live_restore`, `INVARIANT_OK`, `PROOF_DONE result=ok`); RISC-V
(`CROSS_ARCH_D6=1 QEMU_SMP=1`) emitted the complete `trapframe_sret` path
(SEPC/SSTATUS/SP/SATP-ASID/SRET readiness + `RISCV_DEFERRED` + `FALLBACK` +
`INVARIANT_OK` + `PROOF_DONE result=ok`); the x86_64 regression matrix stayed green.
Stage 178/178B CROSS-ARCH-D6 is **ACCEPTED** — cross-arch D6 readiness boundaries
accepted; the AArch64/RISC-V live global-lock-dropped restore remains **DEFERRED**;
x86_64 D6 paths untouched. Stage 179 (D3-FULL) is the next frontier.

### 7.1.15 Stage 179 — D3-FULL (VM anonymous map/unmap two-phase, rank-clean, local-flush-live / remote-shootdown-prepped)

**Next kernel-unlocking frontier after CROSS-ARCH-D6.** Finishes the D3 VM anonymous
map/unmap path as an explicit two-phase, rank-clean sequence with local TLB flush live
and remote TLB shootdown honestly **prepped/deferred** (never a fake SMP-shootdown
claim), behind a default-off knob (`yarm.d3_full=1`, script `D3_FULL=1`, marker
`D3_FULL_ENABLED`).

**Live vs deferred (honesty).** The production `VmAnonMap`/`VmUnmap` syscall path is
**unchanged** — Stage 172 VM-COW already confirmed it is transactional (per-page frame
alloc + map with `rollback_anon_map`, unmap → COW-clear → reclaim → local flush). What
D3-FULL adds is (1) the explicit two-phase `D3_*` phase/rollback/TLB markers and (2) a
**self-contained one-shot proof** that drives the REAL VM primitives
(`create_user_address_space` → `alloc_anonymous_memory_object` →
`map_user_page_in_asid_with_caps` → `unmap_user_page_in_asid` →
`destroy_user_address_space_by_asid`) on a **scratch** address space + scratch memory
object, verifying the full map→commit→unmap→reclaim cycle with leak checks. So the
map/unmap primitives are **genuinely exercised live** (not mocked), but no production
VM ABI changed and no net resource is consumed. **Local ASID flush is live**
(`request_live_asid_shootdown` performs the local invalidation; in BSP-only its remote
target set is empty); **remote shootdown is DEFERRED** (`reason=smp_not_live` in
BSP-only) with an explicit ACK-model-ready prep marker but **no IPI and no ACK wait** —
matching the Stage 177 APs-parked/BSP-only reality.

**Rank order & lock scopes.** Phase 0 validate (no lock). Phase A reserve: address-space
metadata (vm rank 5) + anon MemoryObject cap mint (capability rank 4). Phase B: frame
alloc + PTE install (memory rank 6) via the vm-domain primitive. Phase C commit: local
TLB flush outside any nested lock; remote shootdown request publishes an immutable
target ASID/range and **holds no lock while (not) waiting** — the BSP-only path returns
before any wait. The audit re-checks the documented rank monotonicity
(`D3_VM_RANK_ORDER_OK`).

**Rollback rules.** Map: a Phase-B PTE-update failure unmaps the installed prefix, frees
frames, revokes the reserved cap, and undoes the VM-metadata reservation
(`D3_VM_ANON_ROLLBACK_*`); a Phase-A failure tears down the partial reservation. Unmap:
failure before PTE removal mutates nothing; after PTE removal the path completes cleanup
to a consistent unmapped state (COW cleared, frame/MO/refcount reclaimed) — no stale
writable shared alias, no COW-refcount underflow, no leaked frame. The proof asserts all
leak-check invariants (`D3_VM_NO_FRAME_LEAK`/`_CAP_LEAK`/`_METADATA_LEAK`/`_STALE_PTE`/
`_COW_UNDERFLOW`/`_WRITABLE_SHARED_ALIAS`).

**Markers.** All default-off behind `d3_full_enabled()`: map
(`D3_VM_ANON_MAP_BEGIN`/`_VALIDATE_OK`/`_PHASE_RESERVE_*`/`_PHASE_FRAME_ALLOC_*`/
`_FRAME_ALLOC_OK`/`_PHASE_PT_UPDATE_*`/`_PT_UPDATE_OK`/`_PHASE_COMMIT_*`/`_COMMIT_OK`/
`_DONE`), rollback (`_ROLLBACK_BEGIN`/`_UNMAP_PREFIX_OK`/`_FREE_FRAMES_OK`/`_CAPS_OK`/
`_METADATA_OK`/`_ROLLBACK_OK`/`_FAIL`), unmap (`D3_VM_UNMAP_*`), TLB
(`D3_TLB_LOCAL_FLUSH_*`, `D3_TLB_SHOOTDOWN_PREP_*`, `D3_TLB_SHOOTDOWN_DEFERRED`,
`D3_TLB_ACK_MODEL_READY`, `D3_TLB_ACK_WAIT_DEFERRED`), and invariants (`D3_VM_NO_*`,
`D3_VM_RANK_ORDER_OK`, `D3_VM_INVARIANT_OK`, `D3_VM_PROOF_DONE result=ok`); failure
markers `D3_VM_FRAME_LEAK`/`_CAP_LEAK`/`_METADATA_LEAK`/`_STALE_PTE`/`_COW_UNDERFLOW`/
`_WRITABLE_SHARED_ALIAS`/`_RANK_INVERSION`/`_ROLLBACK_FAIL`, `D3_TLB_LOCAL_FLUSH_FAIL`,
`D3_TLB_SHOOTDOWN_UNSAFE_WAIT`, `D3_VM_INVARIANT_FAIL`.

**`D3_FULL=1` acceptance profile.** Requires `D3_FULL_ENABLED`, the successful
`D3_VM_ANON_*` map sequence, the `D3_VM_UNMAP_*` sequence, `D3_TLB_LOCAL_FLUSH_OK`,
`D3_TLB_SHOOTDOWN_PREP_OK` or `D3_TLB_SHOOTDOWN_DEFERRED`, `D3_VM_INVARIANT_OK`, and
`D3_VM_PROOF_DONE result=ok`; fails hard on every D3 failure marker plus
`CapabilityFull`, `TaskTableFull`, `BLOCKED_WOULDBLOCK_FATAL`,
`PAGE_FAULT_UNHANDLED`/`_FATAL`/`_NOT_HANDLED`, and the fatal breadcrumbs. Handled
COW/DEMAND faults remain accepted. Mode isolation forces `D3_FULL` off under
`D6_SWITCH_PROOF` / `D6_SWITCH_A`. Guarded by `stage179_d3_full`.

**Explicitly NOT done in Stage 179:** no syscall number / ABI change; no service/image/
PM-policy change; no D2/D5/D6 live-wire change; no CNode overhaul; no SMP scheduler
broadening; **no real remote TLB shootdown** (local flush live, remote prepped/deferred,
no IPI/ACK); no RPi5 change; AArch64/RISC-V cross-build clean (x86_64 primary). Counts
unchanged: SYSCALL_COUNT=31, Syscall::VARIANT_COUNT=23, x86_64 MAX_ADDRESS_SPACES=32.

Acceptance (user QEMU): primary
`TIMEOUT_SECS=120 D3_FULL=1 QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh`; regression
matrix (CROSS_ARCH_D6 aarch64/riscv, SMP_READY, GLOBAL_STATE, SPAWN_LIFECYCLE,
FAULT_DELIVERY, CAP_CNODE, VM_COW, SCHED_TIMEOUT, IPC_FINAL, D2_RECV, D2_SEND, normal,
D6_SWITCH_A, 5-min D6_SWITCH_PROOF, sender-wake oracle).

**ACCEPTED (user QEMU, 2026).** The `D3_FULL=1` x86_64 run reached the service
baseline and exercised the real VM primitives on a scratch ASID/object
(`create_user_address_space` → `alloc_anonymous_memory_object` →
`map_user_page_in_asid_with_caps` → local TLB flush → `unmap_user_page_in_asid` →
reclaim/destroy) with the full two-phase map/unmap marker sequence, the remote
shootdown explicitly prep/deferred (`D3_TLB_SHOOTDOWN_DEFERRED reason=smp_not_live`,
`D3_TLB_ACK_WAIT_DEFERRED reason=smp_not_live`), and all leak/invariant checks green
(`D3_VM_NO_FRAME_LEAK`/`_NO_CAP_LEAK`/`_NO_METADATA_LEAK`/`_NO_STALE_PTE`/
`_NO_COW_UNDERFLOW`/`_NO_WRITABLE_SHARED_ALIAS`/`RANK_ORDER_OK`/`INVARIANT_OK`/
`PROOF_DONE result=ok`); the x86_64 regression matrix stayed green. Stage 179 D3-FULL
is **ACCEPTED** — the D3 map/unmap primitives are genuinely exercised live in the
scratch proof, the production `VmAnonMap`/`VmUnmap` syscall ABI is unchanged, local TLB
flush is live, and remote shootdown/ACK is deferred. Stage 180 (CI-PROFILES) is the
next frontier.

### 7.1.16 Stage 180 — CI-PROFILES (repeatable profile runner + fatal-marker policy)

**Reliability/tooling milestone after D3-FULL — not a kernel behavior stage.** Turns the
accepted manual smoke/oracle profiles from Stages 163P and 166–179 into repeatable,
documented profiles behind a single runner (`scripts/run-ci-profiles.sh`) with
consistent logs, marker gates, mode isolation, and a shared fatal-marker policy. **No
kernel runtime behavior changed**; no accepted gate weakened. No script bug required a
fix beyond the shared-helper additions (the per-stage gates were already correct after
Stages 171B/173B/175B/178B).

**Profile inventory.** Core: `x86_64-core` (`-smp 1`), `aarch64-core`, `riscv64-core`.
Oracle: `sender-wake` (Stage 163P), `ipc-final` (`IPC_FINAL=1`). Kernel-unlocking /
diagnostics (x86_64 `-smp 1` unless noted): `d6-switch-proof` (5 min), `d6-switch-a`,
`d6-genuine`, `d2-recv`, `d2-send`, `sched-timeout`, `vm-cow`, `cap-cnode`,
`fault-delivery`, `spawn-lifecycle`, `global-state`, `smp-ready` (`SMP_READY_CPUS=2`),
`smp-ready-4` (`SMP_READY_CPUS=4`, extended), `cross-arch-d6-aarch64`,
`cross-arch-d6-riscv64`, `d3-full`. Each profile is a default timeout of 120 s (300 s
for `d6-switch-proof`), the documented QEMU SMP setting (`-smp 1` except SMP_READY /
CROSS_ARCH_D6 aarch64 = 4), the arch build artifacts, a deterministic log path, and the
per-profile marker gate implemented in its underlying smoke script.

**Groups.** `quick` = x86_64-core + sender-wake + d2-recv + d2-send + d3-full;
`full` = every core/oracle/diagnostic profile at `-smp` defaults + 5-min d6-switch-proof
+ both cross-arch-d6; `extended` = full + smp-ready-4 + optional fs/strict profiles.
`list` prints all profiles; `--dry-run` prints commands without launching QEMU;
`--keep-going` continues past failures; `--logs-dir` fixes the wrapper+QEMU log
location; `--timeout` overrides the default; `--build` rebuilds artifacts first. The
runner calls the existing smoke scripts (no logic duplication) and prints a final
PASS/FAIL/SKIP summary table, exiting nonzero if any required profile fails.

**Shared fatal-marker policy** (`qemu-smoke-common.sh` helpers, additive — existing
scripts unchanged): `log_has_fatal_breadcrumb` (`^!Fv`/`^!BNv`/`DOUBLE_FAULT`/`TRIPLE`/
`PANIC`/`FATAL`), `log_has_unhandled_page_fault` (`PAGE_FAULT_UNHANDLED`/`_FATAL`/
`_NOT_HANDLED` only — the benign `PAGE_FAULT_HW_REGS`/`_FRAME_WORDS`/`_FRAME_DECODE`/
`_HW_PTE_WALK`/`_RAW`/`_X86_ERROR`/`_CR3_COMPARE`/`_ENTRY` and handled
`_HANDLED_COW`/`_HANDLED_DEMAND` are NOT fatal), and `log_has_profile_failure`.

**Acceptance-rule reminders** (also added to `doc/AI_AGENT_RULES.md`): a stage is not
accepted without QEMU/user evidence; the `*_ENABLED` marker alone is never acceptance
(the invariant/proof-done markers are required); handled `PAGE_FAULT_*` diagnostics are
not fatal unless the explicit unhandled/fatal markers appear; default-off knobs are
isolated under `D6_SWITCH_PROOF`/`D6_SWITCH_A`; counts stay SYSCALL_COUNT=31,
VARIANT_COUNT=23, x86_64 MAX_ADDRESS_SPACES=32.

**What remains deferred after Stage 180:** Stage 178 live AArch64/RISC-V global-lock-
dropped restore; real SMP scheduler / IPI remote wake; real remote TLB shootdown ACK;
optional GitHub-Actions QEMU jobs (kept local/manual — the runner is local-first;
`--dry-run` is CI-safe).

Guarded by `stage180_ci_profiles`. Acceptance (user):
`scripts/run-ci-profiles.sh quick --build --logs-dir logs/stage180-quick` (primary),
`full` / `extended` optional/manual.

**ACCEPTED (user, 2026).** `scripts/run-ci-profiles.sh quick --build` passed:
`x86_64-core PASS`, `sender-wake PASS`, `d2-recv PASS`, `d2-send PASS`, `d3-full PASS`;
the explicit hard-failure grep returned no real fatal markers (the broad
FAIL/SKIP/PAGE_FAULT grep matched only benign diagnostics — `target_asid_unavailable`
probes, disabled-FAT `SKIPPED`, `*_FAIL count=0`, handled COW). Stage 180 CI-PROFILES
is **ACCEPTED** — the runner + shared fatal-marker policy are the repeatable proof
harness going forward. Stage 181 (GRADUATE-KNOBS / BIG-BANG UNLOCK) is the next
frontier.

### 7.1.17 Stage 181 — GRADUATE-KNOBS / BIG-BANG UNLOCK (accepted x86_64 -smp1 seams default-on)

**Production-behavior graduation — no longer proof-only.** The accepted x86_64
`-smp 1` unlock seams (D2-RECV-GENUINE, D2-SEND-GENUINE, D6-GENUINE/D6-SWITCH-A
out-of-global-lock dispatch/switch) stop being permanent opt-in experiments: an
umbrella knob `yarm.unlock_graduated` (script `UNLOCK_GRADUATED=1`, marker
`UNLOCK_GRADUATED_ENABLED`) enables them **together, by default, on x86_64 single-CPU
boots**, with a temporary emergency opt-out (`yarm.unlock_graduated=0`) that restores
the conservative per-stage-off behavior. The old per-stage knobs
(`d6_genuine`/`d2_recv_genuine`/`d2_send_genuine`) are retained as escape/debug
overrides for this stage only; **Stage 182 removes/hard-disables the obsolete
fallbacks** after this proves stable.

**Default policy.** On x86_64 with a single online CPU: `unlock_graduated` defaults to
**true** (absent knob ⇒ graduated). On x86_64 with SMP > 1: defaults **false**
(`UNLOCK_GRADUATED_DEFERRED reason=smp_not_live`) — the accepted seams are `-smp 1`
only, and the runtime D6/D2 eligibility already excludes multi-CPU, so the seam gates
fall back to the conservative in-lock path there. On AArch64/RISC-V: defaults false
(`UNLOCK_GRADUATED_DEFERRED reason=cross_arch_live_restore_deferred`) — cross-arch live
D6 restore stays deferred (Stage 178). `yarm.unlock_graduated=0` forces the old
conservative path everywhere (emergency rollback); `=1` explicitly requests graduation.
Isolation: when `D6_SWITCH_PROOF` or `D6_SWITCH_A` is set, the umbrella does NOT
graduate D6 (those proof modes own the switch path).

**Paths graduated together (x86_64 -smp1):**
- **D2-RECV-GENUINE** — blocking recv uses the accepted out-of-global-lock dispatch
  (`PHASE_TASK_BLOCK` → `PHASE_IPC_LOCK` → `PHASE_DISPATCH` → `DISPATCH_DEFERRED` →
  `NO_INLOCK_DISPATCH` → global dropped → reverify → out-of-lock dispatch → restore)
  by default; an unexpected in-lock fallback emits `UNLOCK_GRADUATED_FALLBACK
  path=d2_recv reason=…` and fails the graduated smoke.
- **D2-SEND-GENUINE** — blocking send publishes the waiter before deferral (Stage 163P
  sender-wake ordering preserved), out-of-lock by default; unexpected fallback fails.
- **D6-GENUINE / D6-SWITCH-A** — the accepted x86_64 `-smp 1` global-lock-dropped
  dispatch/switch runs by default where the readiness checks pass; `D6_SWITCH_PROOF`
  is never disturbed; not enabled under SMP > 1.
- **D3** — Stage 179 confirmed the production `VmAnonMap`/`VmUnmap` path is **already**
  the accepted two-phase transactional path (validate → reserve → frame-alloc →
  PT-update → commit → local flush; unmap snapshot → PT-remove → COW-clear → reclaim →
  commit → local flush). Stage 181 does **NOT** change that ABI; it re-runs the
  Stage 179 self-contained D3 proof as the graduation evidence (`UNLOCK_GRADUATED_D3_OK`)
  and states explicitly that **production D3 is already transactional (no new
  graduation wiring / no ABI change)**. Remote shootdown stays prep/deferred
  `reason=smp_not_live` — no fake IPI/ACK.

**Still diagnostic-only (NOT default-enabled):** `VM_COW`, `CAP_CNODE`,
`FAULT_DELIVERY`, `SPAWN_LIFECYCLE`, `GLOBAL_STATE`, `SMP_READY`, `CROSS_ARCH_D6`
remain default-off audit knobs (they carry no additional production seam beyond what is
already always-on); `SCHED_TIMEOUT`'s chunked scan is already the always-on baseline
and `SCHED_TIMEOUT=1` stays a diagnostic marker profile.

**Markers.** `UNLOCK_GRADUATED_ENABLED`, `_BEGIN arch=x86_64 smp=1`,
`_PATH_ENABLED path=d2_recv|d2_send|d6|d3`, `_D2_RECV_OK`/`_D2_SEND_OK`/`_D6_OK`/`_D3_OK`,
`_INVARIANT_OK`, `_DONE result=ok`; fallback `_FALLBACK path=… reason=…`, `_DEFERRED
reason=smp_not_live|cross_arch_live_restore_deferred|unsupported_arch`; failure
`_UNEXPECTED_INLOCK_DISPATCH`, `_DOUBLE_DISPATCH`, `_RESTORE_FAIL`, `_D3_ROLLBACK_FAIL`,
`_D3_LEAK`, `_INVARIANT_FAIL`.

**Emergency opt-out.** `yarm.unlock_graduated=0` (script `UNLOCK_GRADUATED=0`) forces
every accepted seam gate off and boots the conservative path — proven by a dedicated
opt-out smoke. Retained for Stage 181 only.

**Explicitly NOT done / deferred:** no syscall/ABI/service/image/PM-policy change; no
SMP scheduler broadening (SMP > 1 defers); no AArch64/RISC-V live D6 restore; no real
remote TLB shootdown ACK; fallback code is NOT deleted (Stage 182). Counts unchanged:
SYSCALL_COUNT=31, Syscall::VARIANT_COUNT=23, x86_64 MAX_ADDRESS_SPACES=32.

Guarded by `stage181_graduate_knobs`. Acceptance (user QEMU): primary
`UNLOCK_GRADUATED=1 QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh`; normal default-on
`QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh`; emergency opt-out
`UNLOCK_GRADUATED=0 QEMU_SMP=1 …`; `run-ci-profiles.sh quick`; regression matrix
(legacy D2/D3 profiles, D6_SWITCH_A, 5-min D6_SWITCH_PROOF, SMP_READY, CROSS_ARCH_D6
aarch64/riscv, normal aarch64/riscv).

**PARTIAL (user QEMU, 2026).** The explicit `UNLOCK_GRADUATED=1 QEMU_SMP=1` run reached
the service baseline with the full graduated marker set (`UNLOCK_GRADUATED_ENABLED`,
`_D2_RECV_OK`, `_D2_SEND_OK`, `_D6_OK`, `_D3_OK`, `_INVARIANT_OK`) and NO unexpected
fallback / double-dispatch / restore-fail / D3 leak; the runtime log showed
D2-RECV-GENUINE out-of-global-lock dispatch and D6-GENUINE mutating dispatch active by
default. The normal default-on smoke also passed (`graduated by default (result=ok)`).
`run-ci-profiles.sh quick` was blocked only at the `sender-wake` profile: the oracle
recorded the queued-split + rollback proof markers but not
`IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE` / `IPC_RECV_V2_SENDER_WAKE_ORDER_OK`
(`have_seq=0 have_kern=0`) — the sender-wake workload did not run. Fixed in **Stage
181B** (profile plumbing, not a kernel regression).

**Stage 181B — sender-wake runner/oracle plumbing.** Root cause diagnosis: the
runner→oracle→core-smoke chain was verified to correctly deliver the sub-knob — the
`sender-wake` profile passes `YARM_IPC_RECV_PROOF_SENDER_WAKE=1`, the oracle exports
`IPC_RECV_PROOF_SENDER_WAKE=1`, and the x86_64 core smoke appends
`yarm.ipc_recv_proof_sender_wake=1` (confirmed by probing the built cmdline). To make
the failure mode UNAMBIGUOUS and deterministic, the oracle now hard-checks the
authoritative kernel marker `YARM_IPC_RECV_PROOF_SENDER_WAKE_SET enabled=true` before
the sequence check: if it is absent the oracle fails immediately with a clear
"sub-knob did not reach the kernel — runner/oracle plumbing bug" message instead of the
confusing downstream "workload did not run". Kernel behavior is **unchanged** and the
Stage 181 graduation is **preserved** — the sender-wake oracle still boots under the
graduated default-on D2/D6 paths (no `unlock_graduated=0`), proving Stage 163P
sender-wake coexists with the graduated seams. Guarded by `stage181b_sender_wake_plumbing`.
Counts unchanged. **Stage 181 remains PENDING full acceptance** until
`run-ci-profiles.sh quick` (incl. sender-wake) is green under QEMU.

**Stage 181C — fork `Internal` under graduated default-on (PARTIAL).** With the
181B plumbing hard-check in place, a decisive QEMU bisect (user, 2026) isolated a
REAL graduation regression — NOT plumbing:

- Explicit graduated core smoke (`UNLOCK_GRADUATED=1`) passes; normal default-on boot
  passes; no `UNLOCK_GRADUATED_FALLBACK` / `UNEXPECTED_INLOCK_DISPATCH` /
  `DOUBLE_DISPATCH` / `RESTORE_FAIL` / D3 leak markers observed.
- **Graduated** sender-wake oracle FAILS: the workload starts, fills the endpoint,
  reaches `fork`, and `fork` returns `IPC_RECV_PROOF_SENDER_WAKE_FORK_FAILED code=255
  meaning=Internal` — the sequence/order markers never appear.
- **Emergency opt-out** (`UNLOCK_GRADUATED=0`) sender-wake oracle PASSES:
  `IPC_RECV_V2_SENDER_WAKE_ORDER_OK` + `IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`.

Therefore the graduated default-on path makes `fork` return `SyscallError::Internal`
during the sender-wake proof. The conservative path is unaffected. The fix must keep
graduation on (no forcing `unlock_graduated=0` in the sender-wake profile) — Stage 182
fallback deletion still comes after full Stage 181 acceptance.

Instrumentation + triage added this stage so the exact seam and reason are surfaced
on the next run (the syscall ABI is unchanged — `Internal`/err=255 is preserved; the
*cause* is now logged, not hidden):

- `handle_fork` emits `FORK_COW_BEGIN` / `FORK_COW_DONE` and, on failure, a normalized
  `FORK_COW_FAIL reason=<asid_full|cow_capacity|task_full|cap_full|current_tid|…>`
  mapped from the terminal `KernelError` (proof-gated; no normal-boot noise). The
  existing `FORK_PROOF_*` per-phase markers already pinpoint the failing phase.
- The oracle triages a non-completing sender-wake run into WORKLOAD ABSENT vs.
  WORKLOAD STARTED-but-FORK-FAILED (printing the fork code + nearest `FORK_COW_FAIL`
  reason) vs. send/order-marker-missing, instead of the opaque "sequence marker absent".
- The graduated one-shot proof's D3 scratch check now leak-checks the **aspace cap**
  (minted into the caller's cnode by `create_user_address_space`) in addition to the
  mem cap / MO / live aspace slot, closing a gap in the "no net ASID/cap/frame/MO leak"
  guarantee (a stale aspace-cap slot would shrink the caller cnode budget every
  graduated boot and could starve the sender-wake fork's child-cnode setup).

The per-seam bisect modes are already supported by invocation (the oracle passes the
`UNLOCK_GRADUATED` / `D2_RECV_GENUINE` / `D2_SEND_GENUINE` / `D6_GENUINE` env through
to the core smoke, which appends the matching `yarm.*` cmdline knobs), e.g.
`UNLOCK_GRADUATED=0 D6_GENUINE=1 YARM_IPC_RECV_PROOF_SENDER_WAKE=1
scripts/qemu-ipc-recv-v2-oracle-smoke.sh x86_64`. Guarded by `stage181c_fork_internal`.
Counts unchanged. **Stage 181 remains PARTIAL (user QEMU, 2026)** until the graduated
sender-wake oracle passes; the added instrumentation makes the failing seam + reason
deterministic on the next QEMU run.

**Stage 181C (cont.) — QEMU pinned the failure to fork child-register (PARTIAL).**
The next graduated-mode QEMU run reported:

```
FORK_PROOF_ALLOC_CHILD_CAPACITY step=register reason=CapabilityFull live_tasks=12
  max_tasks=512 reserved_cnode_slots=6152 max_total_cnode_slots=262144
FORK_COW_FAIL reason=cap_full kernel_error=CapabilityFull syscall_code=255
```
ASID/VMA capacity are fine (`asid_used=11/32`, `parent_used=80/128`, `child_used=4`).

Key inference: `reserved_cnode_slots=6152` is `≈ 12 live tasks × 512` default slots and
sits at **2.3 % of `max_total_cnode_slots=262144`** — so the register `CapabilityFull`
is **NOT** the aggregate slot budget. Tracing `ensure_cnode_space_with_slots`, the
budget check passes and a free `cnode_spaces` slot exists, so the `CapabilityFull`
comes from `CapabilitySpace::try_with_slots(512)` — i.e. the child's cnode-slot `Vec`
backing allocation (`Vec::try_reserve_exact`) returns `AllocFailed`. That allocation is
served by the kernel slab heap, whose backing pages are drawn from the **PT frame
pool** (`global_allocator` uses `alloc_pt_frame`) — the *same* pool every user
page-table hierarchy uses. So the true mechanism is **PT-pool / kernel-heap
exhaustion**, surfaced as `CapabilityFull` → `Internal`.

Audit results this stage (ruling out false leads): the aspace/cnode primitives are
sound — `remove_asid` → `free_table_hierarchy` returns the *entire* page-table
hierarchy (root + intermediates) to the PT pool, and the emergency-opt-out path
exercises the identical create/destroy primitives yet passes, so the D3 scratch
destroy and normal teardown do not leak. The `d2_send_genuine` fill path only adds
diagnostic markers + dispatch deferral (the 8 sender-waiters are enqueued identically
under opt-out), so it adds no per-send allocation. The leak therefore lives in the
graduated default-on runtime footprint reaching the fork, which the added
instrumentation now attributes on the next run:

- `thread_state.rs` fork-register failure now emits `FORK_PROOF_ALLOC_CHILD_POOL`
  (`pt_pool_free_frames`, requested child slots, live cnode count) plus a per-owner
  `FORK_PROOF_ALLOC_CHILD_CNODE_OWNER id/reserved/occupied` breakdown — answering
  "who holds the slots/pool" directly.
- `frame_allocator::pt_pool_free_frames()` exposes the exhausted pool's headroom.
- `handle_fork` `FORK_COW_BEGIN` now records `pt_pool_free_frames` at fork entry.
- The graduated one-shot proof snapshots the PT pool before/after
  (`UNLOCK_GRADUATED_POOL_BEFORE/AFTER`) and flags a net-negative delta as
  `UNLOCK_GRADUATED_POOL_LEAK` — making the graduated path a deterministic pool-leak
  self-check on its own boot (not just via the later fork).
- The oracle triage prints the pool headroom, the per-owner cnode breakdown, and any
  `UNLOCK_GRADUATED_POOL_LEAK` when fork fails, distinguishing PT-pool exhaustion from
  a genuine slot-budget overflow.

Graduation is preserved (no opt-out forced); emergency opt-out retained; no ABI or
count change.

**Stage 181C — ROOT CAUSE FOUND + FIXED (revoke-scratch cache retained by the proof).**
The instrumented graduated run pinned it exactly:

```
UNLOCK_GRADUATED_POOL_BEFORE pt_pool_free_frames=186
UNLOCK_GRADUATED_POOL_AFTER  pt_pool_free_frames=172 before=186
UNLOCK_GRADUATED_POOL_LEAK   pt_pool_frames_leaked=14
...
FORK_COW_BEGIN parent_tid=1 pt_pool_free_frames=24
FORK_PROOF_ALLOC_CHILD_POOL child_class=SystemServer child_requested_slots=512 pt_pool_free_frames=2
FORK_COW_FAIL reason=cap_full kernel_error=CapabilityFull syscall_code=255
```
Opt-out passes (`FORK_COW_BEGIN ... pt_pool_free_frames=35`, `FORK_COW_DONE`).

Root cause: `CapabilitySpace::revoke()` lazily builds a `RevokeScratch` working set
sized to the cspace capacity and **caches it** (`revoke_scratch_cache = Some(..)`).
For the init cnode (512 slots) that scratch is four Vecs — `child_heads` (8 KB),
`next_sibling` (8 KB), `stack` (4 KB), `marked` — i.e. **≈12–14 pages** taken from the
PT frame pool that backs the kernel slab heap, and it stays **resident** (cached). The
graduated one-shot proof's D3 scratch check revokes its two throwaway caps
(`mem_cap`, `aspace_cap`) on the *current* (init) cnode, so it triggers that cache
build **before** the sender-wake fork — permanently stealing exactly the PT-pool
headroom the fork's 512-slot child cnode `Vec` needs (`try_reserve_exact` →
`AllocFailed` → `CapabilityFull` → `Internal`). The emergency-opt-out path never runs
the proof, so init's revoke-scratch cache is not built early and the fork keeps its
headroom — precisely matching the 186→172 (−14) proof delta and the 35→24 fork-entry
gap between opt-out and graduated.

Fix (targeted, no behavior/ABI/count change): after the scratch check's revokes, drop
the revoke-scratch cache it triggered on that cnode
(`KernelState::drop_revoke_scratch_cache_for_cnode` →
`CapabilitySpace::drop_revoke_scratch_cache`), returning those pages to the PT pool and
emitting `UNLOCK_GRADUATED_D3_SCRATCH_CACHE_DROPPED`. The cache is a pure performance
optimization rebuilt on demand by the next real revoke, so correctness is unchanged and
the graduated path now matches the opt-out path's pre-fork pool headroom. The
`UNLOCK_GRADUATED_POOL_BEFORE/AFTER/LEAK` self-check stays as a regression guard. Nothing
else weakened: graduation still on, oracle unweakened, PT pool / `MAX_ADDRESS_SPACES` /
cnode-slot / task limits unchanged, `AllocFailed`/`CapabilityFull` still surfaced.
Guarded by `stage181c_fork_internal` + `drop_revoke_scratch_cache_releases_and_rebuilds`.

**Stage 181C — functional regression FIXED; residual 3-frame guard delta under
investigation (PARTIAL).** The confirming graduated QEMU run showed the functional
fix holds and the big leak is gone, but the guard still fires on a small residual:

```
UNLOCK_GRADUATED_POOL_BEFORE pt_pool_free_frames=186
UNLOCK_GRADUATED_D3_SCRATCH_CACHE_DROPPED cnode=2 dropped=true
UNLOCK_GRADUATED_POOL_AFTER  pt_pool_free_frames=183 before=186
UNLOCK_GRADUATED_POOL_LEAK   pt_pool_frames_leaked=3
FORK_COW_BEGIN parent_tid=1 pt_pool_free_frames=35
FORK_COW_DONE  child_tid=10008
IPC_RECV_V2_SENDER_WAKE_ORDER_OK / IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE  (PASS)
```
Opt-out passes with identical fork-entry headroom (35). The 14→0 revoke-scratch-cache
leak is fixed and sender-wake passes under graduated default-on; a residual **3**
PT-pool frames remain net-consumed inside the one-shot proof.

Analysis: the scratch address space is net-zero on PT frames — `remove_asid` →
`free_table_hierarchy` returns the root + all intermediate page-table frames
synchronously at destroy, and opt-out exercises the same primitives. `drop_revoke_
scratch_cache` returns the three *large-alloc* RevokeScratch Vecs (`child_heads`,
`next_sibling`, `stack`) directly to the PT pool, but `RevokeScratch.marked`
(`Vec<bool>`, 512 B) is a *small-slab* allocation that leaves a reusable warm page,
and `map`/`unmap`/`destroy` internals touch small-slab classes too. The residual 3 are
therefore most likely **reusable slab warm pages** (the slab keeps one warm empty page
per size class the proof first touched), which `pt_pool_free_frames` counts as
unavailable even though a later allocation of that class reuses them.

To attribute it precisely (not guess), the scratch check now emits **proof-gated
per-step PT-pool snapshots** `UNLOCK_GRADUATED_D3_STEP step=<entry|after_create_aspace|
after_alloc_mo|after_map|after_unmap|after_revoke_mem_cap|after_destroy_aspace|
after_revoke_aspace_cap|after_drop_revoke_scratch> pt_pool_free_frames=N`, and the
oracle surfaces the residual `UNLOCK_GRADUATED_POOL_LEAK` + the per-step trace as an
advisory when it still fires. The kernel guard is **NOT** weakened or silenced.

The per-step trace localized the residual **decisively** (not warm-page noise):

```
entry                186
after_create_aspace  185   (-1 root PT frame)
after_alloc_mo       185
after_map            182   (-3 PDPT/PD/PT intermediates)
after_unmap          182
after_revoke_mem_cap 168   (-14  ← full revoke(mem_cap) builds RevokeScratch)
after_destroy_aspace 172   (+4  root+intermediates returned — VM teardown clean)
after_revoke_aspace  172   (+0  cache HIT)
after_drop_scratch   183   (+11 cache drop returns the large Vecs; 3 unrecovered)
```
So the residual 3 are the part of the `revoke(mem_cap)` RevokeScratch build that
`drop_revoke_scratch_cache` does NOT return (the small-slab `marked` Vec + rounding) —
i.e. a **removable full-revoke side effect**, not VM/COW/fork and not an irreducible
warm-page floor.

**Stage 181C — residual eliminated at source (childless-leaf cap delete).** The scratch
caps `mem_cap`/`aspace_cap` are freshly minted **childless leaves** never delegated
(`map_user_page_in_asid_with_caps` only resolves `mem_cap` to a phys addr — it derives
no child cap). Full `revoke` on a leaf still lazily builds + caches the whole
`RevokeScratch` derivation-tree working set (≈12 pages). Fix: add
`CapabilitySpace::delete_if_leaf` (verifies no in-cspace derived children, clears the one
slot + bumps its generation, **allocation-free** — no RevokeScratch) and
`KernelState::delete_leaf_capability_in_cnode` (also checks no cross-process delegated
descendants, then preserves EVERY object-teardown side effect of
`revoke_capability_in_cnode`: delegation-link removal, transfer-mapping revocation,
MemoryObject refcount/reclaim, Notification destroy). It **transparently falls back to
full `revoke`** for any non-leaf, so recursive-revoke semantics are untouched. The D3
scratch proof now releases both scratch caps (and the alloc-fail rollback) via the leaf
path, so `after_revoke_mem_cap` no longer drops by 14 and there is no residual to drop
(`UNLOCK_GRADUATED_D3_SCRATCH_CACHE_DROPPED ... dropped=false mem_leaf=true
aspace_leaf=true`). Expected: `UNLOCK_GRADUATED_POOL_AFTER == BEFORE`, no
`UNLOCK_GRADUATED_POOL_LEAK`, sender-wake + opt-out both green. The full revoke path is
unchanged; the guard is **NOT** weakened; no PT-pool/cnode/task/MAX_ADDRESS_SPACES limit
changed; no ABI/count change.

**Stage 181C — leaf-delete cut the leak 3→2; residual traced to an allocation-free
teardown and eliminated.** The leaf-delete run took the leak from 3 to **2** frames and
the per-step trace isolated the last residual to `after_delete_mem_cap` (182→180) with
`aspace_cap` flat and NO RevokeScratch built (`dropped=false mem_leaf=true`). Sub-step
audit of `delete_leaf_capability_in_cnode`'s side effects found the culprit was NOT the
MemoryObject reclaim (a single `free_frame` on a fixed-array slot — allocation-free) but
the **delegation bookkeeping**: `collect_delegated_descendants` allocated a `Box`-cloned
snapshot of the 2048-entry links array plus two `Vec::with_capacity(64)` worklists, and
`remove_delegation_links_for` allocated more `Box` clones. Their small worklist Vecs
warmed PT-pool-backed slab pages on the FIRST delete (`mem_cap`), reused by the second
(`aspace_cap`) — exactly the −2-then-flat pattern.

Fix: the leaf path needs only "is this cap delegated AT ALL?", so replace
`collect_delegated_descendants` with a new **allocation-free** `has_any_delegated_child`
(scans the links array in place under one lock, buffering the rare `source_cap`-numeric
matches on a 16-entry stack array and resolving their owning pid outside the lock;
conservative — returns `true` ⇒ full revoke — on the improbable overflow), and SKIP
`remove_delegation_links_for` entirely on the leaf path (a leaf provably has no source
links, so it would remove nothing). The remaining teardown (`revoke_active_transfer_
mappings_for_cap` fixed-array scan, `adjust_memory_object_cap_refcount`,
`reclaim_memory_object_if_unreferenced` single `free_frame`, notification destroy no-op)
is allocation-free. Proof-gated `UNLOCK_GRADUATED_D3_LEAFDEL step=<...> pt_pool_free_
frames=N` sub-steps were added to confirm each side effect is now flat.

Non-leaf caps still route through full `revoke` (which uses the transitive
`collect_delegated_descendants` unchanged), so semantics are identical. Guard NOT
weakened; no warm-page floor; no PT-pool/cnode/task/MAX_ADDRESS_SPACES change; no
ABI/count change. Guarded by `delete_if_leaf_releases_leaf_without_building_scratch`,
`stage181c_graduated_proof_releases_scratch_caps_as_leaves`,
`stage181c_leaf_delete_delegation_check_is_allocation_free`. **Stage 181 acceptance pends
the confirming graduated QEMU run**: expected `UNLOCK_GRADUATED_POOL_AFTER == BEFORE`, no
`UNLOCK_GRADUATED_POOL_LEAK`, `FORK_COW_DONE`, `IPC_RECV_V2_SENDER_WAKE_ORDER_OK`,
`IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`; opt-out still green.

4. **D2-GENUINE — D2 blocking-recv waiter-publish seam fully live-wired.** With the
   global lock no longer spanning `switch_frames` (D6-GENUINE), relocate the D2
   `block_current_on_receive_with_deadline` call boundary ahead of
   `SharedKernel::with_cpu` so that `with_scheduler_split_mut` (rank 1) and
   `with_task_tcbs_split_mut` (rank 2) are called without the outer global-lock
   borrow. Delete the helper-only fences for those two seams in the same PR.
   Gated on D6-GENUINE.

5. **D2-B — D2 send-blocking split.** Full send-side blocking split; no behavior change
   from current enqueue path. Gated on D2-GENUINE.

6. **D6-SWITCH-CROSSARCH — extend D6-SWITCH-A to AArch64 and RISC-V.** AArch64 uses
   the same `switch_frames` shape; RISC-V uses its own trap-frame switch. Both must
   show the unlocked markers in their core smokes before per-CPU runqueue lock sharding
   (D6-full) can be unblocked. Gated on D6-SWITCH-A.

7. **D3-FULL — full `VmAnonMap` two-phase live.** Requires lock-free
   `await_tlb_shootdown_ack` design and multi-CPU smoke proof. Gated on D6-SWITCH-A
   (for per-CPU environment) and D6-SWITCH-CROSSARCH (for multi-CPU smoke readiness).

8. **D1-D5-AUDIT — cap-slot / lock-ordering audit.** D4 is mechanically complete
   (Stage 152). Any remaining unlocking work in the cap/IPC space is the D1/D5
   cap-slot/lock-ordering audit (§5.1), not a mechanical module move. This is
   concurrent with D6-GENUINE / D2-GENUINE but must not introduce new seam callers
   without the helper-only fence rule (§6.6, §8).

### 7.1.18 Stage 182 — REMOVE-FALLBACKS (graduated seams are the only production path)

**Stage 181 is ACCEPTED** (graduated sender-wake reaches `FORK_COW_DONE` +
`IPC_RECV_V2_SENDER_WAKE_ORDER_OK` + `IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`; opt-out
passed; `UNLOCK_GRADUATED_POOL_AFTER == BEFORE`; no `UNLOCK_GRADUATED_POOL_LEAK`;
allocation-free leaf-delete). Stage 182 **removes** the obsolete production fallback
paths + knobs that Stage 181's graduation made redundant on x86_64 `-smp 1` — it deletes
them, it does not hard-disable them.

**What was removed (deleted, not dormant):**

- The runtime seam-toggle plumbing: `UNLOCK_GRADUATED_ENABLED` / `D6_GENUINE_ENABLED` /
  `D2_RECV_GENUINE_ENABLED` / `D2_SEND_GENUINE_ENABLED` `AtomicBool`s and their
  `set_*_enabled()` setters. The gate accessors (`d6_genuine_enabled()` etc.) are now
  **compile-time `cfg!(target_arch = "x86_64")` constants** (graduated on x86_64 unless a
  category-D D6-switch diagnostic owns the switch path). There is no local boolean a
  knob/env/flag can flip back to the old path.
- The `yarm.unlock_graduated` umbrella knob **including its `=0` emergency opt-out** that
  ran the old global-lock production path, and the per-seam SELECTOR knobs
  `yarm.d6_genuine` / `yarm.d2_recv_genuine` / `yarm.d2_send_genuine`. They are still
  *recognized* only to emit `UNLOCK_FALLBACK_KNOB_OBSOLETE knob=<...> action=ignored` — a
  stale boot line can never re-enable the fallback.
- The dead `else { UNLOCK_GRADUATED_FALLBACK / UNLOCK_GRADUATED_UNEXPECTED_INLOCK_DISPATCH }`
  branches in the one-shot proof (the seams can no longer be off), replaced by a
  `debug_assert!` on the compile-time seam invariant + the positive `PATH_ENABLED`/`*_OK`
  evidence markers.
- Scripts: the `unlock-optout` runner profile is deleted; the smoke no longer appends any
  obsolete seam knob and its acceptance block is a **negative test** — it asserts the old
  `emergency_optout` deferral / `UNLOCK_GRADUATED_FALLBACK` / `UNEXPECTED_INLOCK_DISPATCH`
  markers never fire and requires the graduated verdict (`UNLOCK_GRADUATED_DONE
  result=ok`). The oracle asserts the same absences on the graduated sender-wake run.

**What was preserved (NOT a production fallback):**

- The **eligibility-based in-lock path** (`single_cpu` runtime guard + `cfg(not(x86_64))`)
  remains the sole path for SMP>1 (Stage 183 SMP-LIVE) and AArch64/RISC-V (Stage 184
  CROSS-ARCH-LIVE). It is reached by hardware/topology, not by a knob — it is the future
  live-unlock surface, not an obsolete production fallback.
- The D6-SWITCH-PROOF / D6-SWITCH-A diagnostics (category D) and the per-seam
  D2/D6 marker-check env selectors in the smoke stay as diagnostics that prove the
  graduated seam markers appear; they no longer append any kernel knob and select no path.
- **All Stage 181 guards**: `UNLOCK_GRADUATED_POOL_LEAK`, the sender-wake oracle checks,
  the allocation-free leaf-delete (`delete_if_leaf` + `has_any_delegated_child`), and full
  recursive `revoke` for real non-leaf caps. No warm-page allowance; no PT-pool /
  cnode-slot / task-limit / `MAX_ADDRESS_SPACES` increase; no ABI/count change
  (`SYSCALL_COUNT=31`, `Syscall::VARIANT_COUNT=23`, x86_64 `MAX_ADDRESS_SPACES=32`).

Runtime behavior on every path is **identical** to accepted Stage 181 — only the ability
to toggle back to the fallback is gone. Guarded by `stage182_remove_fallbacks` +
updated `stage181_graduate_knobs` / `stage16{7,8,9}` / `stage170` negative source guards.

### 7.1.19 Stage 183 — SMP-LIVE (x86_64 unlocked seams under real `-smp >1`)

**Stage 182 is ACCEPTED** (graduated core smoke `UNLOCK_GRADUATED_DONE result=ok`;
sender-wake `FORK_COW_DONE` + `IPC_RECV_V2_SENDER_WAKE_ORDER_OK` +
`IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`; no fallback / opt-out / in-lock-dispatch /
pool-leak). Stage 183 makes the accepted x86_64 unlocked seams live under **real SMP**
(`-smp 2/4`) instead of only `-smp 1`.

**Pivotal current-state finding (the gating blocker).** On x86_64 the APs are started
via the trampoline but **park in a `cli/hlt` loop** — `X86_AP_RUST_PARK cpu=N
reason=no_ap_scheduler_yet`; they are *not admitted to the production scheduler* (GS-base
MSR write, per-CPU TSS, and local-APIC timer are all deliberately deferred; see
`arch/x86_64/smp.rs`). Consequently `online_cpu_count()` is **1 even under `-smp N`**, so
the graduated seams' `single_cpu` eligibility (`online_cpu_count() <= 1`) is always true:
they run single-CPU-eligible on the BSP and the SMP in-lock path never engages because
there is no concurrent scheduler CPU to race. **There is no live SMP scheduling yet.** So
the real Stage 183 work is **AP scheduler admission** (category B: IPI/remote-wake +
per-CPU bring-up), then the concurrent-safety proof (category C: TLB shootdown/ACK). This
is a large low-level effort whose acceptance is entirely QEMU-SMP runs.

**SMP blocker inventory + classification.**

- `exec_state.rs:~1903` (D6), `ipc_state.rs:~667` (D2-recv), `ipc_state.rs:~757` (D2-send):
  `let single_cpu = self.online_cpu_count() <= 1;` gates the out-of-lock slice → in-lock
  under SMP. **Category D (topology guard)** — correct as-is; relax *only after* AP
  admission + invariants proven. Not a knob.
- `orchestrator_state.rs:~841`: the graduated proof defers at `online > 1`
  (`reason=smp_not_live`). **Category D**.
- `arch/x86_64/smp.rs`: APs park, not scheduler-admitted (`no_ap_scheduler_yet`, GS/TSS/
  APIC-timer deferred). **Category B — the primary blocker**: AP scheduler admission +
  IPI-driven remote wake.
- TLB shootdown request/wait/ACK (`request_live_asid_shootdown`,
  `execute_tlb_shootdown_wait_plan`, `acknowledge_shootdown`) exist but are exercised only
  in the single-CPU fast path today. **Category C** — must be proven once APs are live.
- AArch64/RISC-V in-lock. **Category F — Stage 184**, out of scope here.

**Increment 2 — AP scheduler *idle* admission (GS-initialized, interrupt-masked idle).**
Host QEMU confirmed increment 1 (`-smp 2/4`: present=N online=1, APs parked, BSP graduated
path green, no fallback). Increment 2 admits the APs out of the bare park loop into a
**GS-initialized, interrupt-masked Rust idle loop** — idle-live only, no D2/D6 SMP seams.

- Internal proof path (NOT a user knob): `smp_trampoline::AP_IDLE_ADMIT_PROOF` (compile-time
  const). `-smp 1` has no APs, so production is unchanged.
- The AP entry stays **100% inline asm** (the AP runs on the bootstrap PML4 — text + low
  identity only, no `.bss`/`.data`/MMIO, CR4 has only PAE so no SSE). The BSP passes the
  per-CPU record base through the (low, identity-mapped) `ApHandoff.percpu_record_ptr`, so
  the AP can `wrmsr IA32_GS_BASE` + `rdmsr` readback-verify it **without any higher-half
  access**, then publish its admit stage into the low `ready_word` and enter `cli/hlt`.
- **TSS / local-APIC / APIC-timer stay DEFERRED** (`X86_AP_TSS_DEFERRED` /
  `X86_AP_LAPIC_DEFERRED` / `X86_AP_LAPIC_TIMER_DEFERRED`) — they need a switch to the full
  kernel CR3 + a per-CPU GDT TSS descriptor + LAPIC MMIO, which is the next increment. The
  idle loop is interrupt-masked, so none are required for safe idle.
- The BSP is the single serial writer (no AP log garbling): it polls the AP's admit stage
  and emits `X86_AP_SCHED_ADMIT_BEGIN` / `X86_AP_GS_OK`|`X86_AP_GS_BAD` /
  `X86_AP_TSS_DEFERRED` / `X86_AP_LAPIC_DEFERRED` / `X86_AP_LAPIC_TIMER_DEFERRED` /
  `X86_AP_IDLE_ENTER` / `X86_AP_SCHED_ADMIT_DONE` (or `X86_AP_SCHED_ADMIT_FAIL` /
  `X86_AP_IDLE_FAIL` on timeout).
- **`ap_idle_live` is a SEPARATE count from `online_cpu_count()`**: these APs idle with
  interrupts masked and are **NOT admitted to the production scheduler** (`bring_up_cpu` is
  still BSP-only), so `online` stays 1, `single_cpu` stays true, and **no task is ever
  enqueued onto an idle-only AP**. The SMP audit reports `X86_SMP_AP_IDLE_LIVE present=N
  online=1 ap_idle_live=M` + `X86_SMP_UNLOCK_DONE result=ap_idle_live`, with the next
  blocker `category=B reason=ap_full_scheduler_admission_required`.
- Smoke (`-smp >1`) requires the admission markers + `result=ap_idle_live`, forbids
  `X86_AP_GS_BAD` / `X86_AP_IDLE_FAIL` / `X86_AP_SCHED_ADMIT_FAIL` and any premature
  `X86_SMP_APS_ADMITTED`. Guarded by `stage183_ap_idle_admit`.

Risk/limitation: the AP bring-up path is only validatable on the host's `-smp 2/4` QEMU
(none in the dev env). It is default-on for SMP boots so `smp2-core` exercises it directly;
a fault would surface as an admit-stage timeout (`X86_AP_SCHED_ADMIT_FAIL`) rather than a
silent hang, since the BSP poll is bounded. D2/D6 SMP-live remains gated behind full
scheduler admission (next increments); `single_cpu` is NOT relaxed.

**Increment 2 diagnostic — deterministic AP breadcrumb ⇄ stage trace.** The observed
host symptom is that the AP reaches the `@` (Rust-entered) COM1 breadcrumb but never
publishes a terminal admit stage, so the BSP times out. The interleaved post-SIPI serial
bytes (`g P h 4 A 3 C e L S R @ …`) are **intentional breadcrumbs, not corruption**; each
maps to an exact asm block. To turn that stream into a deterministic trace and name the
*first failing transition* without guessing:

- A dedicated **stage word** at `ApHandoff` offset 48 (`ap_stage`, distinct from the coarse
  `ready_word` at offset 32) is written by the AP entry asm **before every risky action**,
  paired with a distinct breadcrumb byte: `@`/`AP_STAGE_RUST_ENTERED(10)` → `H`/handoff
  loaded(11) → `V`/validated(12) → `W`/before-wrmsr(13) → `w`/after-wrmsr(14) → `r`/
  after-rdmsr(15) → `O`/`gs_verified(16)` (or `B`/`gs_mismatch(254)`, or `!`/`handoff_null
  (253)`) → `I`/before-hlt(17) → `Z`/`idle(18)`. The full byte↔stage↔asm-block table (mapped
  from the actual asm, incl. preconditions and "failure-after-this-byte" meaning) lives as a
  comment block in `smp_trampoline.rs`.
- The failing transition is instrumented at exactly the seams that can fault on an AP with
  no IDT: loading + **validating** the handoff pointer (null → stage 253, skip wrmsr),
  `wrmsr IA32_GS_BASE`, `rdmsr` readback, the GS-readback compare, and entering the first
  `hlt`. `dx` is reloaded to `0x3F8` after each MSR op (wrmsr/rdmsr clobber `rdx`).
- On admit-poll timeout the BSP now reads offset 48 and reports
  `X86_AP_SCHED_ADMIT_FAIL cpu=<id> reason=timeout last_stage=<name> last_stage_raw=<hex>`
  (via `ap_stage_word_low_virt` + `ap_stage_name`), so the next `smp2-core` run names the
  last stage the AP reached — the first missing stage then drives the targeted admission fix.
- Layout hardening: the trampoline handoff reservation grew `.zero 40` → `.zero 56` to hold
  the full 56-byte `ApHandoff`, and a compile-time `offset_of!` guard locks `ready_word@32`,
  `percpu_record_ptr@40`, `ap_stage@48` (the offsets the asm hardcodes). The AP path stays
  ultra-minimal: no Rust calls / higher-half `.bss`/`.data` / LAPIC MMIO / TSS / AP timer /
  scheduler-runnable AP / D2/D6 seam. Guarded by `stage183_ap_stage_trace_is_deterministic`.

**Increment 2 `@ → H` ROOT CAUSE (found by disassembly + reference audit, host-pinned to
the first rdi dereference).** Two independent defects, both fixed, both verified in the
built ELF's disassembly:

1. **Trampoline misassembly (the deterministic killer):** `add rdi, AP_OFF_HANDOFF`
   assembled as `add rdi, QWORD PTR ds:0x140` — GAS Intel syntax treats a bare
   symbol-difference `.set` as a **memory operand**, so the AP added the 8 bytes at
   physical `0x140` (BIOS IVT vector 0x50 area — SeaBIOS junk) instead of the constant.
   `rdi` was corrupt **before** `jmp rax`; the first store through it in the Rust entry
   faulted (no AP IDT → triple fault → silence after `@`). Fixed to
   `add rdi, OFFSET AP_OFF_HANDOFF` (immediate; same idiom as
   `movabs rax, OFFSET yarm_x86_64_ap_entry`). Post-fix disassembly:
   `48 81 c7 40 01 00 00 = add rdi, 0x140`. Pure-numeric `.set`s
   (`mov rbx, AP_TRAMPOLINE_BASE` → `mov rbx, 0x7000`) and bracketed uses were never
   affected. Increment 1 never tripped this because the parked AP never dereferenced rdi.
2. **Formal UB in the entry (per the Rust reference, rule `asm.rules.reg-not-input`):**
   the entry read `rdi` inside a regular `asm!` block without `in("rdi") handoff_ptr` —
   "any registers not specified as inputs will contain an undefined value on entry to
   the assembly code"; `let _ = handoff_ptr;` binds nothing at the register level.
   `yarm_x86_64_ap_entry` is now **`#[unsafe(naked)]` + `naked_asm!`** (reference: no
   compiler prologue/epilogue; "the assembly code may assume that the call stack and
   register state are valid on entry as per the signature and calling convention"), so
   `rdi == handoff_ptr` is guaranteed at the first instruction. This is the same
   trampoline→naked-entry transfer Redox's kernel uses (`kstart_ap`:
   `#[unsafe(naked)]` + `naked_asm!` reading `[rdi + offset_of!(KernelArgsAp, ...)]`
   after its trampoline's `mov rdi, [trampoline.args_ptr]; jmp rax`). Verified: the
   entry's first instruction in the ELF is `cli` — no prologue.

   Determinism hardening: the trampoline now publishes `AP_STAGE_RUST_JUMP` (9) through
   the **absolute** low stage-word address (not rdi) + a `>` breadcrumb immediately
   before `jmp rax`, so even a broken register handoff or bad jump target can never
   again stop the trace silently after `@` — the BSP timeout would name
   `last_stage=rust_jump`. Guarded by `stage183_ap_entry_naked_abi_and_offset_fix`.

**Increment 2 ACCEPTED (user QEMU, 2026).** `smp2-core`: `X86_AP_GS_OK cpu=1`,
`X86_AP_IDLE_ENTER cpu=1`, `X86_AP_SCHED_ADMIT_DONE cpu=1`, `X86_SMP_AP_IDLE_LIVE
present=2 online=1 ap_idle_live=1`, `X86_SMP_UNLOCK_DONE result=ap_idle_live`.
`smp4-core`: same per cpu=1/2/3 with `present=4 online=1 ap_idle_live=3`. No
`X86_AP_GS_BAD` / `X86_AP_SCHED_ADMIT_FAIL` / `X86_AP_IDLE_FAIL` /
`X86_SMP_ONLINE_ACCOUNTING_BAD` / fallback / emergency_optout.

**Increment 3 — AP scheduler-admission PREREQUISITES (per-CPU runtime env, still
idle-only).** Moves from "idle-live with interrupts masked" toward "enough per-CPU
runtime environment to become scheduler-online later". All AP-side steps stay in the
naked entry asm (no AP Rust calls — the target has no SSE-disabling features, so
compiled Rust remains unsafe on the AP's PAE-only CR4); each publishes a breadcrumb +
stage word, results are AP-written into the handoff/per-CPU record and GRADED by the
BSP (single serial writer):

- **Kernel CR3 (controlled transition):** the BSP passes `ApHandoff.kernel_cr3` (the
  same full kernel root the BSP runs on — it maps kernel text, low identity, `.bss`,
  LAPIC MMIO); the AP reloads CR3 (`'K'/19 → 'k'/20`), then proves the space live by
  storing the `AP_ENV_CANARY` (0x0183C0DE) into its per-CPU record **via gs:** (higher-
  half `.bss` write + GS-relative addressing in one instruction). Markers:
  `X86_AP_KERNEL_CR3_BEGIN/OK/FAIL cpu=<id> [reason=reload_flag_missing|
  bss_canary_missing]`. Prepare-time `YARM_SMP_AP_ENV_MAP_CHECK` walk-checks the
  per-CPU/GDT/TSS/LAPIC VAs under that root and refuses SIPI if the `.bss` VAs are
  unmapped (the AP would triple-fault).
- **Per-AP GDT/TSS:** `descriptor_tables::prepare_ap_descriptor_tables` builds one GDT +
  one TSS per CPU in `.bss` — BOOT_GDT selector layout (0x08 kernel code / 0x10 kernel
  data / 0x28 TSS) so the AP CONVERGES on the production kernel selectors; TSS `rsp0` =
  the AP's own stack top; ISTs stay 0 (only consumed via IDT gates — the AP IDT
  increment wires real per-AP IST stacks before any interrupt can fire). The AP does
  `lgdt [rdi+64]` (GDTR image in the handoff), reloads SS/DS/ES=0x10, far-returns to
  CS=0x08 (`'D'/21`), then `ltr 0x28` (`'T'/22`). The BSP grades via the AP-set env
  flags AND the **TSS BUSY bit ltr wrote into that AP's GDT** (read back from `.bss`).
  Markers: `X86_AP_GDT_LOCAL_OK`, `X86_AP_TSS_OK cpu=<id> rsp0=… busy=1` /
  `X86_AP_TSS_BAD reason=gdt_not_loaded|ltr_flag_missing|busy_bit_not_set`.
- **LAPIC access:** the AP reads ITS OWN LAPIC ID register (`[lapic_id_reg_va]`, VA
  passed after the map check; 0 ⇒ skip) under the kernel CR3 (`'l'/23`) and publishes
  the id; the BSP compares with the cpu's APIC id. Markers: `X86_AP_LAPIC_OK cpu=<id>
  apic_id=<n>` / `X86_AP_LAPIC_BAD reason=read_skipped_or_unmapped|id_mismatch`.
- **AP timer policy: DEFERRED** — `X86_AP_LAPIC_TIMER_DEFERRED cpu=<id>
  reason=no_ap_idt_interrupts_masked` (a timer before the AP IDT exists would
  triple-fault on the first tick; interrupts stay masked).
- **Idle task/context:** BSP records idle METADATA (entry/stack/CR3 +
  `IDLE_TASK_META_SET`) in the per-CPU record before SIPI — a reserved, validated
  description, NOT an enqueued task; the AP publishes its live idle `rsp` via gs:
  (`'y'/24`), validated against the stack bounds. Markers: `X86_AP_IDLE_TASK_READY
  cpu=<id> entry=… stack=… enqueued=0`, `X86_AP_IDLE_CONTEXT_OK cpu=<id> rsp=…` (or
  `X86_AP_IDLE_CONTEXT_BAD`).
- **Conservative counts:** all prerequisites proven ⇒ `X86_AP_SCHED_PREREQ_OK` +
  `ap_env_ready` (a SEPARATE `AP_ENV_READY_COUNT`, between `ap_idle_live` and the
  future `scheduler_online`). `online_cpu_count()` STAYS 1 — `bring_up_cpu` is never
  called for APs, `single_cpu` stays true, no D2/D6 SMP seams. The audit adds
  `X86_SMP_AP_ENV_READY present=N online=1 ap_idle_live=M ap_env_ready=K` and appends
  `ap_env_ready=K` to `X86_SMP_UNLOCK_DONE result=ap_idle_live …`.
- Record-init race fix: `init_record_for_ap` + idle metadata + TSS pointer moved to
  `prepare_trampoline_for_cpu` (BEFORE SIPI); `emit_ap_percpu_scaffold` is now
  read-only (the AP concurrently writes canary/rsp into the same record via gs:).
  ApHandoff grew 56→96 bytes (`.zero 96` + compile-time `offset_of!` guard). Smoke
  requires the new OK/DEFERRED markers and forbids `X86_AP_KERNEL_CR3_FAIL` /
  `X86_AP_TSS_BAD` / `X86_AP_LAPIC_BAD` / `X86_AP_IDLE_CONTEXT_BAD` /
  `X86_AP_SCHED_PREREQ_INCOMPLETE`. Guarded by `stage183_inc3_ap_env_prereqs`.
  Acceptance: `scripts/run-ci-profiles.sh smp2-core` + `smp4-core`.

**Increment 3 ACCEPTED (user QEMU, 2026).** `smp2-core`: `X86_AP_KERNEL_CR3_OK cpu=1`,
`X86_AP_GDT_LOCAL_OK cpu=1`, `X86_AP_TSS_OK cpu=1 busy=1`, `X86_AP_LAPIC_OK cpu=1
apic_id=1`, `X86_AP_IDLE_CONTEXT_OK cpu=1`, `X86_AP_SCHED_PREREQ_OK cpu=1`,
`X86_SMP_AP_ENV_READY present=2 online=1 ap_idle_live=1 ap_env_ready=1`; `smp4-core`
same for cpus 1/2/3. No CR3/TSS/LAPIC failure markers; online stayed 1.

**Increment 4 — 183.4 AP INTERRUPT-SAFE IDLE (Task A).** The APs prove they can take
one controlled interrupt without triple fault, and that any unexpected vector parks
deterministically instead of triple-faulting:

- **AP-safe IDT (dedicated, NOT the shared kernel BOOT_IDT).** The kernel IDT's gates
  enter the full Rust trap path (global KernelState, logging, compiled-Rust SSE) — not
  AP-safe. `descriptor_tables::prepare_ap_idt` builds one shared AP IDT in `.bss`:
  every vector points at a 16-byte catch-all stub (`.rept 256`, `push vec; jmp common`)
  that records (vector+1) into `PerCpuRecord.irq_unexpected_vec` via gs: and PARKS
  (cli/hlt, no iretq); `AP_IRQ_SMOKE_VECTOR` (0xF0) gets the real handler:
  `irq_hit_count += 1`, `irq_hit_vector = 0xF0`, LAPIC EOI, `iretq` — all pure asm,
  register-preserving. Gates: CS=0x08 (per-AP GDT kernel code), dpl=0, ist=0. The AP
  loads it via `lidt [rdi+96]` (IDTR image in the handoff; env flag `IDT_LOADED`).
- **IST policy: not_required, validated.** Every gate uses ist=0 — the AP never leaves
  its known-good idle stack (no user mode, no stack switch, no nesting; interrupts are
  enabled ONLY inside the controlled sti;hlt window), so the interrupted rsp is always
  valid. The BSP validates no gate names an IST slot (`ap_idt_any_ist_nonzero` →
  `X86_AP_IST_BAD reason=gate_ist_nonzero_without_per_ap_stacks`; else `X86_AP_IST_OK
  mode=not_required`). Real per-AP IST stacks land with scheduler-online (183.5).
- **CR4 sync.** The AP mirrors the BSP's CR4 (`mov cr4 = handoff.bsp_cr4`; env flag
  `CR4_SYNCED`, markers `X86_AP_CR4_SYNC_OK/FAIL`) — control-state convergence
  (PGE/OSFXSR/…), the prerequisite for any future compiled-Rust execution on APs
  (the target spec has no SSE-disabling features).
- **Controlled interrupt smoke (one IPI, no scheduler tick).** After the env steps the
  AP publishes `ready_word=3` and waits in the race-free `sti; hlt` pair (sti's
  interrupt shadow defers delivery until hlt has begun — an IPI sent any time after
  ready_word=3 either wakes hlt or was already handled; no lost wake), re-checking
  `gs:[96]` under cli. The BSP sends EXACTLY ONE fixed IPI (`X86_IPI_REMOTE_WAKE_SEND
  from=0 to=<cpu> vector=0xf0`), then grades: handler hit (`X86_IPI_REMOTE_WAKE_RECV`),
  AP resumed to the idle path (stage 28→17→18, `X86_IPI_REMOTE_WAKE_ACK`), then after a
  settle window requires hit_count==1 and no unexpected vector →
  `X86_AP_INTERRUPT_SMOKE_OK vector=0xf0` (failure grades: `no_handler_hit`,
  `dup_delivery`, `unexpected_vector`, `no_resume_after_handler`, `idt_not_loaded`).
  `X86_AP_IDLE_ENTER` is now emitted only after the stage word confirms the AP
  re-reached the permanent interrupt-masked idle loop (18).
- New AP stages: `'c'`/25 cr4_synced, `'i'`/26 idt_loaded, `'u'`/27 irq_smoke_wait,
  `'v'`/28 irq_smoke_done; ApHandoff grew 96→120 (`idtr_image@96`, `bsp_cr4@112`,
  `.zero 120` + `offset_of!` guard); prepare-time map check extended with the IDT +
  stub-text VAs (refuses SIPI if unmapped). `ap_interrupt_ready` is a new SEPARATE
  count; the audit adds `X86_SMP_AP_INTERRUPT_READY present=N online=1 ap_idle_live=M
  ap_env_ready=K ap_interrupt_ready=J` and the blocker becomes
  `category=B reason=ap_scheduler_online_admission_required`.
- **NOT graduated here (hard gates preserved):** scheduler-online (`bring_up_cpu` for
  APs, `online>1`), periodic timer ticks, D6 SMP dispatch, D2 SMP sender-wake, live
  TLB shootdown ACK — 183.5/183.6, each gated on the previous marker set passing on
  the host. `single_cpu` untouched; no fallback knobs. Runner gains `smp6-core` /
  `smp6-sender-wake` (CPU-count-only profiles). Guarded by
  `stage183_inc4_ap_interrupt_safe_idle`.
  Acceptance: `scripts/run-ci-profiles.sh smp2-core` + `smp4-core` (+ optional
  `smp6-core`), plus `smp2-sender-wake`/`smp4-sender-wake` for the BSP graduated path.

**183.4 host failure ROOT CAUSE + fix (deterministic: every AP
`X86_AP_INTERRUPT_SMOKE_FAIL reason=no_handler_hit`, `last_stage=irq_smoke_wait`).**
The AP sat correctly in the race-free `sti;hlt` window — the IPI was dropped BEFORE
delivery. Per the Intel SDM (state after INIT), the AP's local APIC resets to
`SVR=0xFF` with **bit 8 (APIC software enable) CLEAR**: a software-disabled LAPIC
accepts only INIT/SIPI/NMI/SMI and silently drops fixed IPIs (QEMU implements
exactly this). That is why INIT/SIPI bring-up worked while vector 0xF0 never arrived
— and why `X86_AP_LAPIC_OK` (an MMIO *read* of the APIC id) could pass while
delivery was impossible. Fix (all before the smoke window):

- **AP-side LAPIC interrupt-delivery readiness** (`'n'`/stage 29, env flag
  `LAPIC_SW_ENABLED`): write `SVR=0x1FF` (software enable | spurious vector 0xFF —
  parked by the catch-all stub if it ever fires), `TPR=0` (accept all priority
  classes), write-clear `ESR`; publish all three readbacks through the handoff
  (`svr_out@120`/`tpr_out@124`/`esr_out@128`; ApHandoff 120→136, `.zero 136` +
  guard). BSP grades: `X86_AP_LAPIC_ENABLE_BEGIN`, `X86_AP_LAPIC_SVR_OK value=0x1ff`,
  `X86_AP_LAPIC_TPR_OK value=0x0`, `X86_AP_LAPIC_ESR_OK value=0x0`,
  `X86_AP_LAPIC_INTERRUPT_READY` (or `X86_AP_LAPIC_INTERRUPT_BAD
  reason=enable_flag_missing|svr_sw_enable_clear|tpr_masking|esr_nonzero`).
- **BSP-side instrumented fixed-IPI send**: `X86_IPI_FIXED_SEND_BEGIN … mode=physical`,
  `X86_IPI_FIXED_ICR_WRITTEN to=N high=0x<apic<<24> low=0x000000f0`,
  `X86_IPI_FIXED_DELIVERY_IDLE`, `X86_IPI_FIXED_ESR from=0 before=… after=…` (BSP ESR
  write-latch read before/after), `X86_IPI_FIXED_SEND_DONE` /
  `X86_IPI_FIXED_SEND_FAIL reason=delivery_status_stuck|esr_nonzero`.
- **Smoke-vector DESCRIPTOR check** (not just the IDT base):
  `ap_idt_smoke_vector_report` verifies present + interrupt gate (0xE) + selector
  0x08 + ist 0 + offset == the smoke stub's linked VA →
  `X86_AP_IDT_VECTOR_OK cpu=N vector=0xf0 selector=0x08 ist=0 type=0xe` /
  `X86_AP_IDT_VECTOR_BAD reason=descriptor_mismatch …`.
- **No fake success**: the smoke still hard-fails deterministically
  (`no_handler_hit` / `idle_reentry_timeout last_stage=irq_smoke_wait`) and
  `ap_interrupt_ready` still requires the actual delivery proof
  (`X86_IPI_REMOTE_WAKE_RECV`/`ACK` + `X86_AP_INTERRUPT_SMOKE_OK`), never readiness
  alone. All hard gates unchanged (no scheduler-online / `bring_up_cpu` /
  `single_cpu` relax / D2/D6 seams / TLB-ACK proof). Smoke additionally requires the
  readiness/vector/send markers and forbids `X86_AP_LAPIC_INTERRUPT_BAD` /
  `X86_AP_IDT_VECTOR_BAD` / `X86_IPI_FIXED_SEND_FAIL`. Guarded by
  `stage183_inc4_fix_lapic_sw_enable_for_ipi_delivery`.

**183.4 ACCEPTED (user QEMU, 2026).** Per AP: `X86_AP_LAPIC_SVR_OK value=0x1ff`,
`X86_AP_LAPIC_TPR_OK value=0x0`, `X86_AP_LAPIC_ESR_OK value=0x0`,
`X86_AP_IDT_VECTOR_OK vector=0xf0 selector=0x08 ist=0 type=0xe`,
`X86_IPI_REMOTE_WAKE_RECV`/`ACK`, `X86_AP_INTERRUPT_SMOKE_OK vector=0xf0`;
summaries `X86_SMP_AP_INTERRUPT_READY present=2/4 … ap_interrupt_ready=1/3`. No
LAPIC/IDT/SEND/SMOKE failure markers, no admit fail, no fallback/optout.

**183.5 — AP SCHEDULER-ONLINE + REMOTE WAKE.** APs become `online_cpu_count()`
members with real scheduler-owned idle and a per-AP exactly-one remote-wake proof —
while task placement on them stays gated and the D2/D6 out-of-lock SMP seams stay
unproven-and-gated for 183.6:

- **Sequencing (the key safety decision):** the admission runs ONE-SHOT from the SMP
  audit only AFTER the graduated one-shot proof emitted its verdict
  (`unlock_graduated_proof_completed()`; the audit returns without latching until
  then). The accepted graduated evidence therefore still executes on the BSP with
  `online == 1` — its out-of-lock seam slices require the single-CPU topology until
  183.6 — and the smoke's unconditional `UNLOCK_GRADUATED_DONE result=ok` gate keeps
  holding under `-smp 2/4`. Boot-time `X86_SMP_STARTUP online_cpus=1` /
  `X86_SMP_OBSERVATION_OK scheduler_aps=0` remain the boot-time truth.
- **Wake-only online (stranding is impossible by construction):** `SmpScheduler`
  gains a `wake_only` bitmap — such CPUs are online for accounting/wake but
  `enqueue_balanced` skips them and explicit `enqueue_on_with_priority` is DENIED
  (`SCHED_ENQUEUE_DENIED_WAKE_ONLY`, forbidden in smoke). Without this, any
  runtime spawn without affinity could balance onto an AP run queue that no
  dispatcher drains. The AP is marked wake-only BEFORE `bring_up_cpu` (no placement
  window). 183.6+ clears the bit per CPU when the AP dispatcher lands. NOT a knob.
- **Scheduler-owned idle task:** current = tid 0 — the scheduler's EXISTING idle
  placeholder convention (`dispatch_next` already switches away from tid 0 when
  real work arrives, so the representation is forward-correct for the 183.6 AP
  dispatcher). `install_ap_idle_current` installs it on the online wake-only AP;
  the AP's live body is the new MANAGED interruptible idle loop (stage 30
  `sched_idle` / 31 `sched_wake_reenter`, breadcrumbs `'q'`/`'z'`): `sti;hlt`,
  wake-capable via vector 0xF1, publishes `wake_reenter_out` ([handoff+132]) on
  every observed wake and RETURNS TO IDLE — no bare unmanaged cli/hlt as the final
  scheduler-online state (degraded GS_BAD/env-skip paths still park masked).
- **Remote wake proof (per AP, after online):** dedicated vector 0xF1
  (`yarm_ap_remote_wake_stub`: gs:[108] `remote_wake_count` += 1, EOI, iretq).
  BSP sends EXACTLY ONE (`X86_IPI_REMOTE_WAKE_SEND from=0 to=N vector=0xf1`), then
  grades: handler ran (`RECV`), idle re-entered with the re-enter count bumped and
  stage back at 30 (`ACK` + `X86_AP_SCHED_IDLE_REENTER`), settle, wake delta == 1
  (else `D6_SMP_DUP_WAKE_FAIL`), no missing wake (else `D6_SMP_LOST_WAKE_FAIL`),
  and the idle current still coherent (`current_tid_on(ap) == idle tid`) →
  `D6_SMP_REMOTE_WAKE_OK cpu=N` (+ separate `ap_remote_wake_ok_count`).
- **Marker flow per AP:** `X86_AP_IDLE_TASK_CREATE_BEGIN` →
  `X86_AP_IDLE_TASK_READY tid=0 stack entry` → `X86_AP_SCHED_ONLINE_BEGIN` →
  `X86_AP_IDLE_TASK_ACTIVE tid=0` → `X86_AP_SCHED_ONLINE_OK` →
  `X86_AP_SCHED_IDLE_ENTER tid=0` → wake trio → `D6_SMP_REMOTE_WAKE_OK`; then
  `X86_SMP_ONLINE_READY present=N online=N`. Audit: `X86_SMP_AP_SCHED_ONLINE …
  remote_wake_ok=K`, `X86_SMP_PLACEMENT_GATED cpus=0x<bitmap>
  reason=ap_dispatcher_not_wired`, blocker `category=C
  reason=d2_d6_smp_seams_unproven`, `X86_SMP_UNLOCK_DONE result=aps_online …`.
- **D2/D6 seams under `single_cpu == false`:** the untouched topology gates route
  the graduated seams onto their conservative in-lock slice
  (`D6_GENUINE_MUT_DISPATCH_FALLBACK reason=multi_cpu` / `D2_*_GENUINE_FALLBACK
  reason=multi_cpu`) — the designed Category-D behavior, NOT an old fallback knob.
  The strict out-of-lock gates (d2-recv/d2-send profiles, IPC_FINAL) remain
  `-smp 1` profiles; 183.6 proves the out-of-lock slices under SMP and graduates
  them. Guarded by `stage183_inc5_ap_scheduler_online_and_remote_wake`.
  Acceptance: `scripts/run-ci-profiles.sh smp2-core` + `smp4-core`
  (+ optional `smp6-core`).
- **183.5 host failure #1 ROOT CAUSE + fix (every AP:
  `X86_AP_INTERRUPT_SMOKE_FAIL reason=no_resume_after_handler` with RECV present).**
  The fixed IPI was delivered and the handler ran (RECV proves `irq_hit_count`
  bumped) — the failure was the BSP's resume grading, not the AP. The old
  `resumed` poll accepted stages `28|17|18`; the 183.5 managed-idle tail made 28 a
  microseconds-wide transient and removed 17/18 from the smoke-OK path (terminal
  is now 30/31), while the BSP first spent milliseconds printing the RECV marker
  through the QEMU UART — so the poll deterministically started after 28 was gone
  and could never match. Fix: **grade from persistent state, never transients** —
  the AP's post-`hlt`, handler-confirmed path now writes a PERSISTENT ACK
  (`PerCpuRecord.irq_ack = 1` via `gs:[116]`, stage 36 `irq_ack_written` after
  stage 35 `irq_resumed` written immediately after every `hlt` return) and the
  BSP polls `irq_ack == 1`. The 0xF0 handler additionally publishes sub-stages
  via `gs:[112]` (`32 irq_handler_enter` → `33 irq_handler_eoi` → `34
  irq_handler_iret`), and every `X86_AP_INTERRUPT_SMOKE_FAIL` now reports
  `last_stage=… last_stage_raw=… irq_stage=…` so a future failure names the exact
  handler/resume transition. Stub audit (ELF-verified): interrupt gate 0xE,
  CS=0x08, ist=0, no error code for vector 0xF0, `push rax`-preserving, LAPIC EOI
  before `iretq`, terminal `iretq`, no park-path contamination. Honest summary:
  `X86_SMP_AP_INTERRUPT_READY` is only emitted with a nonzero count; a zero count
  emits `X86_SMP_AP_INTERRUPT_NOT_READY` (forbidden in smoke). Scheduler-online
  stays gated on the smoke verdict — with the smoke failing, the run correctly
  refused `bring_up_cpu`/online>1. Guarded by
  `stage183_inc5_fix_persistent_smoke_ack`.
- **183.5 host failure #2 ROOT CAUSE + fix (kernel `#PF CR2=0x7170`,
  `RIP ∈ ap_scheduler_online_admission`, right after `X86_AP_SCHED_ONLINE_OK`).**
  Symbolization pinned the crash to the sched-idle stage poll: `rdx = 0x140`
  (= `handoff_off`) with displacement `0x7030` = `0x7000 + handoff_off + 0x30` —
  exactly `ap_stage_word_low_virt(handoff_off)`, the LOW identity-mapped
  trampoline VA. That alias is mapped only under the boot CR3; the admission
  runs post-graduated-proof inside a trap on the CURRENT TASK address space,
  where low `0x7000` is unmapped → deterministic page fault at the first low-VA
  read. (All earlier polls of the same VAs ran at boot time on the boot CR3 —
  which is why 183.1–183.4 never tripped this.) Fix: the AP now MIRRORS its
  sched-idle stage (`gs:[120]` → `PerCpuRecord.sched_stage`) and wake-reenter
  count (`gs:[124]` → `wake_reenter_mirror`) into the per-CPU record — kernel
  `.bss`, mapped on every address space — and the admission polls ONLY those
  mirrors. Before any poll it validates the pointer against the LIVE CR3
  (high-half + `debug_root_maps_virt`) and emits
  `X86_AP_SCHED_IDLE_POLL_PTR_OK cpu=N ptr=0x…` (or `…_PTR_BAD cpu=N ptr=0x…
  reason=low_or_unmapped` and skips, instead of faulting; PTR_BAD forbidden in
  smoke). The low-VA readers remain boot-CR3-only diagnostics. The persistent
  smoke-ACK path (fix #1) is untouched and host-proven
  (`X86_AP_INTERRUPT_SMOKE_OK` observed). Guarded by the extended
  `stage183_inc5_ap_scheduler_online_and_remote_wake` (admission body must
  contain no low-virt reader calls).
- **Retired-ASID safety under `online = N` (found by audit, fixed pre-host):**
  `destroy_user_address_space_by_asid` retired ASIDs pending on the FULL
  `online_cpu_bitmap()`, but nothing drains a wake-only AP's cross-CPU work queue
  (only the BSP trap path drains its own) — every post-admission task destroy
  would leak a retired-ASID slot and after 32 destroys every teardown returns
  `VmError::Full`. Fix: both shootdown-target computations
  (`live_cpu_bitmap_for_asid`, destroy's `pending_cpu_bitmap`) now exclude
  wake-only CPUs — architecturally sound because a wake-only AP runs no
  dispatcher, never loads a user CR3, and never touches user VAs, so it cannot
  hold translations for any user ASID. 183.6 re-includes each CPU when its
  dispatcher lands and the REAL remote shootdown IPI + AP-side drain are wired
  (that, plus the D2/D6 out-of-lock SMP proof, is exactly the
  `category=C reason=d2_d6_smp_seams_unproven` blocker).

**183.5 ACCEPTED (user QEMU, 2026).** `smp2`: `X86_SMP_ONLINE_READY present=2
online=2`, `D6_SMP_REMOTE_WAKE_OK cpu=1`, `X86_SMP_UNLOCK_DONE result=aps_online
present=2 online=2 remote_wake_ok=1`. `smp4`: same for cpu=1/2/3, `online=4
remote_wake_ok=3`. Interrupt smoke + persistent ACK + low-alias poll fix +
sched-idle reenter all passed; no bad markers.

**183.6 — REAL SMP SEAMS (D2/D6 out-of-lock under `online = N` + real cross-CPU TLB
shootdown ACK).**

- **D2/D6 out-of-lock dispatch under real SMP — the single-DISPATCHER predicate.**
  The topology gate moves from `online_cpu_count() <= 1` to
  `dispatching_cpu_count() <= 1`, where `dispatching = online & !wake_only`. This is
  the correct predicate: the accepted out-of-lock deferred-dispatch slice is safe
  when only ONE CPU dispatches user tasks, and wake-only APs (183.5) dispatch
  nothing (no dispatcher runs on them; the scheduler denies task placement,
  `SCHED_ENQUEUE_DENIED_WAKE_ONLY`). So under `online = N` with every AP wake-only
  the predicate stays 1 and the seams keep their accepted single-CPU out-of-lock
  path — no in-lock `multi_cpu` fallback. Under `online = 1` it is identical to the
  prior value. It stays pure topology (a derivation of the online + wake-only
  bitmaps), NOT a knob. The D6 genuine path emits `D6_SMP_DISPATCH_BEGIN` /
  `D6_SMP_DISPATCH_OK` when it relocates a queue-advancing dispatch out of the lock
  while `online > 1`; the blocking sender-wake workload drives it, and the oracle
  requires it + `IPC_RECV_V2_SENDER_WAKE_ORDER_OK` /
  `IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE` under `-smp >1`. Clearing an AP's
  wake-only bit (a future AP-dispatcher increment) raises the count and re-gates the
  seams automatically.
- **Real cross-CPU TLB shootdown ACK — no simulation.** A lock-free per-CPU
  shootdown mailbox lives in the (always-mapped) `PerCpuRecord`: `tlb_req_gen@128`,
  `tlb_ack_gen@132`, `tlb_req_va@136` (single writer per field per direction, so no
  lock and no `KernelState` access from the AP). The AP's managed sched-idle loop,
  on every wake, services it: if `req_gen != ack_gen` it executes the invalidation
  locally (`invlpg [tlb_req_va]`, or a CR3 reload when `va == 0`) and then advances
  `ack_gen = req_gen` — a genuine remote acknowledgement. The BSP driver
  `smp_tlb_shootdown_cpus` posts the request (VA before gen, so the AP always reads
  the matching VA), sends the wake IPI (vector 0xF1 — the same one the idle loop
  already services), and waits (bounded) for the ACK, emitting
  `X86_TLB_SHOOTDOWN_SEND` / `X86_TLB_SHOOTDOWN_ACK` (or `X86_TLB_REMOTE_ACK_TIMEOUT`
  on failure — forbidden in smoke, never a hang). The 183.6 one-shot
  `ap_tlb_shootdown_proof` runs a full round-trip against every online AP for the
  COW context (a representative write-protect VA) and the VM_UNMAP context (full
  flush), emitting `X86_TLB_SHOOTDOWN_DONE` + `COW_SMP_TLB_ACK_OK` /
  `VM_UNMAP_SMP_TLB_ACK_OK`. Because wake-only APs idle on the kernel CR3 and hold
  no user ASID, invalidating any VA on them is correct-and-conservative
  (over-invalidation is always safe) while the ACK is real; precise per-ASID
  targeting on an AP arrives when that AP runs user tasks (its wake-only bit
  cleared, re-joining `live_cpu_bitmap_for_asid`).
- **Terminal verdict.** The audit drives the TLB proof after admission and emits
  `X86_SMP_UNLOCK_DONE result=smp_seams_ok present=N online=N remote_wake_ok=K
  tlb_ack=1` when both TLB contexts acked; otherwise
  `X86_SMP_UNLOCK_BLOCKER category=C reason=tlb_shootdown_ack_unproven` +
  `result=aps_online … tlb_ack=0` (honest, never faked). `PerCpuRecord` grew
  128→192 bytes for the mailbox (stride only; not a limit/ABI constant). No user
  knobs, no fallback selectors, no scheduler placement on APs, counts unchanged.
  Guarded by `stage183_inc6_real_smp_seams`. Acceptance:
  `scripts/run-ci-profiles.sh smp2-core` + `smp2-sender-wake` + `smp4-core` +
  `smp4-sender-wake`.

**Stage 183 ACCEPTED CAVEAT (must not be overclaimed).** x86_64 SMP is accepted
with APs **online but WAKE-ONLY**: they are `online_cpu_count()` members that idle
in a scheduler-owned interruptible loop, receive real remote wakes, and acknowledge
real TLB shootdowns — but they **run no dispatcher and execute NO user tasks**. Task
placement on them is denied (`SCHED_ENQUEUE_DENIED_WAKE_ONLY`). Stage 183 therefore
does **NOT** prove multi-dispatcher user scheduling; `dispatching_cpu_count` stays 1
(BSP-only). Multi-dispatcher / user-tasks-on-APs is a **later** milestone (after
Stage 185 global-lock retirement or a dedicated AP-dispatch stage), NOT part of
Stage 183 or 184.

### 7.1.20 Stage 184 — CROSS-ARCH-LIVE (accepted graduated paths, honest per-arch topology)

Bring the accepted graduated kernel paths live across architectures **without**
weakening the x86_64 Stage 181–183 results, and **without** faking SMP bring-up on
architectures that do not have it.

- **x86_64: regression only.** All Stage 183 markers stay required
  (`X86_SMP_ONLINE_READY`, `D6_SMP_REMOTE_WAKE_OK`, `D6_SMP_DISPATCH_OK`,
  `IPC_RECV_V2_SENDER_WAKE_ORDER_OK`, `IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`,
  `COW_SMP_TLB_ACK_OK`, `VM_UNMAP_SMP_TLB_ACK_OK`,
  `X86_SMP_UNLOCK_DONE result=smp_seams_ok`).
- **Generalized topology predicate.** The Stage 183.6 `dispatching_cpu_count =
  online_cpu_count − wake_only_cpu_count` is arch-generic. The out-of-lock D2/D6/D3
  slice is eligible when `dispatching_cpu_count ≤ 1` (single dispatcher), NOT merely
  `online_cpu_count ≤ 1`. On arches without AP scheduler-online this naturally
  collapses to the single-dispatcher case.
- **The correctness invariants are already arch-generic and live.** No IPC-state
  lock across the user-memory copy, sender-wake ordering, no lost/dup wake, no
  fallback branch — these hold on AArch64/RISC-V today. The **only** x86-specific
  piece is the *out-of-lock dispatch relocation*, which rides the x86 trap-entry
  drain. So Stage 184 does not blindly copy x86 SMP: it attests the honest per-arch
  reality with a `mode` field.
- **Default-on cross-arch live audit** (`maybe_run_cross_arch_live_audit`, one-shot,
  real-user-task gated, no knob, runs on every arch). It emits:
  `CROSS_ARCH_TOPOLOGY_BEGIN` / `CROSS_ARCH_DISPATCHING_CPUS arch online wake_only
  dispatching` / `CROSS_ARCH_TOPOLOGY_OK arch reason=single_dispatcher` (or
  `_BLOCKED`); `CROSS_ARCH_D2_RECV_OK` / `_D2_SEND_OK` / `_D6_OK` / `_D3_OK` each with
  `mode=out_of_lock` on x86_64 or `mode=in_lock_single_dispatcher` on aarch64/riscv;
  `CROSS_ARCH_SYSCALL_PARITY_OK arch tid` (the audit runs from the syscall trap path
  with a live user task that returned to userspace — direct runtime evidence the
  Stage 81A parity holds); and `CROSS_ARCH_LIVE_DONE arch result=ok mode`. Honest
  framing: `in_lock_single_dispatcher` means the graduated path runs **in-lock**
  (correct, single-dispatcher-safe) — it is NOT the removed global-lock fallback, and
  it is NOT an x86-style out-of-lock relocation.
- **No fake remote TLB ACK off x86.** AArch64/RISC-V are single-dispatcher with no AP
  user execution, so no remote CPU caches a user translation to acknowledge; the
  audit emits the honest topology marker and makes **no** `*_TLB_ACK_OK` claim for
  those arches. Only x86_64 (with real online APs holding the kernel CR3 and acking
  via the per-CPU mailbox) emits the TLB-ACK markers.
- **Stage 184 explicit NON-GOALS:** not Stage 185 global-lock retirement; not
  multi-dispatcher scheduling; not AP user-task scheduling; **not fake remote TLB ACK
  on architectures without real remote holders.** No fallback knobs, no emergency
  opt-out, no ABI/count/limit changes (`SYSCALL_COUNT=31`, `VARIANT_COUNT=23`,
  x86_64 `MAX_ADDRESS_SPACES=32`). Guarded by `stage184_cross_arch_live`. Acceptance:
  `scripts/run-ci-profiles.sh {x86_64-core,smp2-core,smp2-sender-wake,smp4-core,
  smp4-sender-wake,aarch64-core,riscv64-core}`.

**What remains for Stage 185 (and beyond).** Global-lock retirement (the graduated
seams still take the global `KernelState` lock for the in-lock authoritative
dispatch; retiring it is Stage 185), then multi-dispatcher user scheduling / AP
user-task execution (a dedicated later stage that clears an AP's wake-only bit and
raises `dispatching_cpu_count` above 1, at which point the same predicate re-gates
the seams and the per-ASID-precise TLB targeting on APs becomes meaningful).

**Stage 185 (GLOBAL-LOCK-RETIRE) — honest status.** The Stage 185 pass established
that the global `SpinLock<KernelState>` (`with`/`with_cpu`) is still the
authoritative *live-runtime* serialization for the single-dispatcher model and is
**not an obsolete crutch**: the lock-free split path is a whitelist-only scaffold
(3 syscall sub-cases), and every other live syscall/IPC/scheduler/capability/
VM/fault path runs inside the global lock by design. Fully retiring it is the
per-subsystem rewrite `doc/KERNEL_LOCKING.md` §"Current status" disclaims, and
Stage 185 is explicitly *not a rewrite stage* — so it did **not** retire the
global lock from live runtime. It instead (a) inventoried + classified every
global-lock site, (b) confirmed no obsolete fallbacks remain (Stage 182 removed
them), (c) confined + guarded the sole boot-only raw `&mut KernelState` escape
(`borrow_kernel_for_boot`, `stage185_boot_only_global_borrow_confined`), and
(d) recorded the lock-rank rules in `doc/KERNEL_LOCKING.md §0`. Full retirement
is deferred to future per-subsystem increments, coupled to the multi-dispatcher
work above. See `doc/KERNEL_LOCKING.md §0` for the classified inventory.

**Stage 185BC (CAP / OBJECT-STORE / REPLY-CAP DECOMPOSITION) — audited, HARD-STOPPED,
no code change.** A mega pass was attempted to split the capability/object-store
(185B) and reply-cap/cap-transfer (185C) slices out of the global lock. The audit
found **no safe new slice for either part** and stopped per its own hard-stop rule
(no risky half-conversion):

- *185C reply-cap / cap-transfer — BLOCKED (needs IPC decomposition first).*
  `ReplyCapRecord` and `TransferEnvelope` are **IPC-subsystem state** in
  `IpcSubsystem` under `ipc_state_lock` (rank 3, see `doc/CAPABILITY_MODEL.md`
  §5/§6/§8). Reply caps are created (`create_reply_cap_for_caller`), consumed
  (`ipc_reply`), and revoked (`revoke_reply_caps_for_caller`/`_replier`, incl.
  task-exit/restart cleanup) **inside the IPC send/recv/call/reply path**, which
  runs under the global lock. Transfer envelopes are taken/materialized on the
  recv-delivery path via the mandatory two-phase pattern. Decomposing any of this
  out of the global lock requires broad IPC endpoint/waiter decomposition first —
  the explicit hard-stop condition.
- *185B capability/object-store basic split — no standalone slice.* There is **no
  standalone capability syscall** (no CapLookup/CapDelete/CapRevoke in the 23-variant
  `Syscall` enum). `resolve_capability_for_task` (read-only lookup) and the
  leaf-revoke helpers are internal, called from ~26 sites all inside global-lock-held
  IPC/spawn/VM/exit handlers; leaf release/refcount runs on the TransferRelease /
  IPC-reply / task-exit paths (IPC-domain, rank 3). The one standalone cnode-domain
  operation that *was* safely splittable — `ControlPlaneSetCnodeSlots` — is already
  split (Stage 29, `syscall_split.rs` whitelist). Adding any new lock-free cap path
  would mean decomposing the IPC/spawn/VM callers, widening scope.

**Recommended smaller next stage:** do the IPC-domain decomposition first as its own
stage (endpoint/waiter/reply-cap/transfer-envelope under `ipc_state_lock` with the
two-phase materialization moved after IPC-unlock), *then* revisit reply-cap and
cap-transfer split as follow-on increments. No `CAP_SPLIT_*` / `REPLY_CAP_SPLIT_*` /
`CAP_TRANSFER_SPLIT_*` markers were introduced — emitting split-success markers for
paths still handled by the legacy global runtime would be dishonest.

**Stage 185B (IPC-DOMAIN-DECOMPOSITION-PREP) — audited, HARD-STOPPED, no code change.**
The follow-on IPC-domain decomposition was attempted and stopped per its own hard-stop
rule (real IPC decomposition must not require scheduler-wide decomposition first).
Two findings:

- *The two-phase `ipc_state_lock` structure the previous note asked for ALREADY
  EXISTS.* `ipc_reply` / recv-delivery / `create_reply_cap_for_caller` /
  `take_transfer_envelope` already own their IPC-local state under `ipc_state_lock`
  (rank 3) and perform cap materialization (rank 4), user-memory copy, and scheduler
  wake **only after dropping `ipc_state_lock`** ("Phase 3/5 outside all locks"). The
  reply-cap one-shot / `StaleCapability` / `WrongObject` / `MissingRight` /
  `CapabilityFull` invariants are already enforced and heavily tested. So there is no
  un-converted two-phase work to add — the discipline is in place.
- *Genuine IPC decomposition — removing the global lock from IPC ops — is BLOCKED on
  scheduler decomposition.* `ipc_send/recv/call/reply` intrinsically perform scheduler
  **block/wake** as integral steps (`apply_scheduler_wake_plan`, `block_current`,
  `enqueue_woken_task` — ~37 scheduler-mutation call sites across the IPC files). The
  IPC-local sub-operations the task named as "safe" candidates (reply-cap table
  lookup/mark-consumed, transfer-envelope validate/take) are **not standalone
  syscalls** — they occur only inside those enclosing IPC ops, which need the
  scheduler wake/block. Running any IPC syscall without the global lock therefore
  requires the scheduler block/wake state mutation to be lock-free-safe first — the
  explicit hard-stop condition. No `IPC_SPLIT_*` markers were introduced (they would
  be dishonest for paths still under the global lock).

**Corrected dependency ordering:** the 185BC note above proposed "IPC decomposition
first, then reply-cap/cap-transfer". The 185B audit shows the real prerequisite is one
level deeper: **scheduler-domain decomposition** (make `TaskStatus` block/wake +
runqueue enqueue/dequeue safe outside the global lock) must come first, *then* IPC
send/recv/call/reply can leave the global lock, *then* reply-cap/cap-transfer split
becomes possible. Recommended next stage: `Stage 185S SCHEDULER-BLOCK-WAKE-DECOMP` —
move the block/wake state mutation onto `scheduler_state` (rank 1) / `task_state_lock`
(rank 2) with a documented two-phase (compute wake plan under lock, apply after
release), reusing the existing `compute_wake_plan_for_tid` / `apply_scheduler_wake_plan`
split that the D2/D6 dispatch relocation already established.

**Stage 185S (SCHEDULER-BLOCK-WAKE-DECOMP) — audited, HARD-STOPPED, no code change.**
The audit found the premise WRONG in the codebase's favour: **the scheduler block/wake
mechanics are ALREADY subsystem-lock-decomposed and two-phase.** `apply_scheduler_
wake_plan` is called at ~10 production sites, every one documented and structured to run
the wake *after* dropping `ipc_state_lock` / all IPC/cap/VM/memory domain locks
("Phase 3/4/5 outside all locks"); the wake itself (`wake_tid_to_runnable`) transitions
`TaskStatus` via `with_tcbs_mut` (`task_state_lock`, rank 2) and enqueues via
`enqueue_woken_task` (`scheduler_state`, rank 1), sequentially and non-nested, holding
neither lock across a user-memory copy or a cap materialization; `block_current` mutates
only scheduler-internal state under the scheduler lock. So the "move block/wake onto
scheduler/task locks with a two-phase structure" work this stage asked for is **already
implemented** — there is nothing un-converted to move.

The only remaining coupling of block/wake to the global lock is that its ~10 callers are
IPC syscalls (`ipc_send/recv/call/reply`) that hold the global lock. Removing the global
lock from the wake therefore means removing it from those IPC ops and relocating the
sender/receiver wake out of the global lock (the D6-style *stash then apply after the
global lock drops* pattern) **without perturbing the sender-wake ORDERING oracle**
(`IPC_RECV_V2_SENDER_WAKE_ORDER_OK`, "sender-wake stays before writeback and after the
receiver commits metadata"). That is IPC endpoint / sender-wake decomposition — the
explicit hard-stop condition for this stage. No `SCHED_SPLIT_*` markers were introduced
(they would be dishonest for paths still under the global lock).

**Corrected dependency picture (final).** The chain is not a clean stack (cap ← IPC ←
scheduler-block/wake). Block/wake is already decomposed; IPC and its sender-wake are
mutually entangled with the global lock. The real next increment is a **vertical
co-decomposition of a single IPC operation**: pick one op (e.g. `ipc_reply`), move it out
of the global lock end-to-end using the proven D6 stash relocation for its sender-wake,
and validate against the full sender-wake QEMU oracle (x86_64 `smp2/smp4-sender-wake`)
before touching any other IPC op. Recommended next stage:
`Stage 185V IPC-REPLY-VERTICAL-DECOMP` (one op, stash-relocated wake, oracle-gated) —
a larger, riskier, single-operation slice, not another horizontal layer.

**Stage 185V (IPC-REPLY-VERTICAL-DECOMP) — audited, HARD-STOPPED, no code change.**
The `ipc_reply` vertical slice was attempted and stopped: it cannot be moved out of the
global lock in one pass without broad endpoint/waiter + capability decomposition (the
hard-stop condition). Findings:

- *`ipc_reply` is already impeccably phased.* Phase 1 snapshots the waiter under
  `ipc_state_lock` (released immediately), Phase 3 (`complete_blocked_recv_for_waiter`)
  does the user copy "outside all locks", Phase 4 clears the waiter under
  `ipc_state_lock`, Phase 5 wakes via `apply_scheduler_wake_plan` outside all locks; the
  reply cap is consumed exactly once (`ipc.reply_caps[slot] = None`), and `ipc_state_lock`
  is never held across copy / cap materialization / wake. There is no phasing defect to
  fix — the two-phase discipline the stage asked for already exists.
- *The only coupling to the global lock is that `ipc_reply` accesses state through
  `&mut KernelState` (from `with_cpu`).* It touches four subsystems via `self.*`: ipc-state
  (7 ops), capability/cnode (6 ops: `fast_revoke_reply_cap_in_cnode`, `current_task_cnode`,
  `resolve_send_cap_task_local`), task-state (`with_tcbs`), scheduler
  (`apply_scheduler_wake_plan`).
- *The infrastructure to run those subsystems lock-free does not exist.* Split-mut helpers
  exist **only for the scheduler** (`with_scheduler_split_mut`); there is no
  `with_ipc_state_split_mut` / `with_task_state_split_mut` / `with_cnode_split_mut` /
  `with_capability_split_mut` (0 in tree). And `complete_blocked_recv_for_waiter` — the
  user-copy/waiter-completion `ipc_reply` uses — takes `&mut KernelState` and is **shared
  by 7 call sites** across send/recv/call/reply/fault delivery. Making it global-lock-free
  converts it for every IPC op = broad endpoint/waiter decomposition. The split-dispatch
  path services only narrow single-subsystem ops ("never blocks, yields, schedules, or
  copies user memory"), so `ipc_reply` (multi-subsystem, user-copy, wake) cannot ride it.
  Granting `ipc_reply` a raw `&mut KernelState` without the global lock is forbidden
  (reintroduces the boot-only escape into live runtime, guarded by
  `stage185_boot_only_global_borrow_confined`) and unsound on SMP (races wake-only APs;
  the global lock is what serializes them). No `IPC_REPLY_SPLIT_*` markers were introduced.

**Series conclusion (185BC → 185B → 185S → 185V, four honest stops).** Global-lock
retirement cannot proceed by peeling one operation or one subsystem at a time: the IPC ops
share wide `&mut KernelState` machinery (delivery/waiter/cnode/scheduler), and the only
per-subsystem split-mut infrastructure that exists is the scheduler's. Retirement requires
**building the per-subsystem split-mut layer (ipc-state, task-state, cnode/capability)
first, as pure infrastructure**, before any IPC op can leave the global lock — a large,
multi-stage effort. Moreover, on the accepted single-dispatcher model the global lock is
**uncontended** (one dispatcher), so retirement yields **no functional benefit until the
deferred multi-dispatcher work lands**. Honest recommendation: **defer global-lock
retirement** and either (a) prioritize the multi-dispatcher enablement that would make it
beneficial and give the sender-wake oracle a real concurrency surface to prove against, or
(b) if retirement is still desired, run it as an explicit multi-stage
`Stage 186 SPLIT-MUT-INFRA` track that builds `with_ipc_state_split_mut` /
`with_task_state_split_mut` / cnode split-mut helpers + guards first, then revisits
`ipc_reply`. Either way it is not a single-pass vertical slice.

**Stage 186A (SPLIT-MUT-INFRA) — DONE (infrastructure only, no live conversion).**
The four honest 185-series stops recommended building the per-subsystem split-mut
layer before any IPC op can leave the global lock. On audit, that layer **already
existed** for ranks 1 (scheduler), 2 (task/TCB), 3 (IPC), 5 (VM), 6 (memory) from
Stage 108/115 — the only gap was the **rank-4 capability domain**. Stage 186A closes
it: `capability_split_mut_ptrs_from_raw` (projector) + `with_capability_state_split_mut`
(`SharedKernel` seam), exposing ONLY `&mut CapabilitySubsystem` (CNode spaces,
`process_cnodes`, `delegated_capability_links`), `M2_SEAM_HELPER_ONLY`, **no live
caller**. The per-domain seam set (ranks 1–6) is now complete — see
`doc/KERNEL_LOCKING.md §0.1` for the table. **No live syscall/op was migrated**; the
`with`/`with_cpu` legacy boundary remains authoritative for every runtime path;
full global-lock retirement remains deferred; APs remain online but wake-only.
Guarded by `stage186a_capability_split_mut_infra` (seam exists, is narrow, never
exposes `&mut KernelState`, is not wired live, rank-4 two-phase contract documented).

Roadmap (updated): **186A split-mut infra (done)** → 186B `ipc_reply` vertical
conversion onto the seams (stash-relocated wake, sender-wake-oracle-gated on real
QEMU) → 186C `ipc_send`/`recv`/`call` → 186D reply-cap/cap-transfer → 186E
VM/COW/fork → 186F fault-report delivery → 187 AP user scheduling / multi-dispatcher
→ 188 final live-runtime global-lock removal. Note the earlier honest finding stands:
on the single-dispatcher model the global lock is uncontended, so 186B+ yield no
functional benefit until 187 — sequencing 187 earlier is a legitimate alternative.

**Stage 186B (IPC-REPLY-VERTICAL-CONVERSION) — audited, HARD-STOPPED, no code change.**
With the 186A seams in hand, `ipc_reply` conversion was attempted and stopped: the
split-mut seams are necessary but **not sufficient**, because `ipc_reply` sits on top
of two broad shared subsystems that Stage 186B may not convert. `ipc_reply`'s IPC
reply-cap phase (`with_ipc_split_mut`) and the cnode fast-revoke
(`with_capability_state_split_mut`) ARE seam-expressible — but its **delivery** to the
blocked caller (`complete_blocked_recv_for_waiter`) requires:

- **User-memory copy** (`copy_to_user` ×2 for payload+meta; and `copy_from_user` to
  marshal the reply message). `copy_to_user` resolves the target ASID's page tables and
  calls `validate_user_access_for_asid(.., write=true)`, whose write path triggers
  **COW fault-in** (allocates a frame via the memory allocator, rewrites the PTE). There
  is no seam-based user-copy helper; building one that soundly handles the write-fault/COW
  case is VM/COW-domain work — **explicitly out of Stage 186B scope** ("do not convert
  VM/fork/COW/futex").
- **Cap materialization** (`materialize_received_message_cap_routed`, `ipc_recv_core.rs`),
  the shared D1/D4/D5 cap-transfer engine with **13 call sites** across all IPC delivery
  paths. Converting it is broad cap-transfer decomposition — **explicitly out of Stage
  186B scope** ("do not convert broad cap-transfer paths") — and being shared, it cannot
  be converted "for `ipc_reply` only".

Even the narrow "enqueue-to-endpoint, no blocked-recv-v2 waiter" sub-case still needs
`copy_from_user` to marshal the reply payload from the replier's address space, so no
seam-only sub-case exists. No `IPC_REPLY_SPLIT_*` markers were introduced (they would be
dishonest for a path still on the legacy boundary).

**Corrected roadmap ordering.** `ipc_reply` (186B) sits ABOVE the VM user-copy path and
the cap-transfer engine, so those are **prerequisites**, not successors. The roadmap
`186B → 186D(cap-transfer) → 186E(VM)` is inverted for this slice. Do **186E-prereq**
(a VM/memory-seam user-memory copy helper — `copy_to_user`/`copy_from_user` via
`with_vm_user_spaces_split_mut` + `with_memory_split_mut`, COW-fault-safe) and
**186D-prereq** (seam-based `materialize_received_message_cap_routed` on the capability
+ IPC seams) **first**; only then can `ipc_reply` be converted end-to-end. Standing
caveat unchanged: none of this yields functional benefit until 187 (multi-dispatcher),
so prioritizing 187 remains the honest alternative.

**Stage 186E-prereq (VM-USER-COPY-SEAM) — DONE (infrastructure only, no live conversion).**
The 186B stop identified two blockers for `ipc_reply`: (1) user-memory copy and (2) the
shared cap-transfer engine. This stage removes the **first** blocker as reusable
infrastructure. On audit, the legacy user-copy path
(`copy_to_user`/`copy_from_user`/`validate_user_access_for_asid`) turned out to touch
**only** the rank-5 VM (`with_user_spaces`) and rank-6 memory (`with_memory_state` /
direct-phys) domains, and to perform **no COW fault-in** (`validate_user_access_for_asid`
returns `UserMemoryFault` on a non-writable/unmapped target — it never faults a page in;
the 186B "COW-during-copy" fear was incorrect). So the seam is cleanly buildable. Added,
in `boot/user_memory_state.rs` on `SharedKernel`:

- `validate_user_access_for_asid_split` (rank-5 VM seam),
- `copy_from_user_split`, `copy_to_user_split` (rank-5 VM validate + rank-6 memory / direct
  phys byte access).

They never form a broad `&mut KernelState` and never take the IPC (rank 3), capability
(rank 4), task (rank 2), or scheduler (rank 1) locks, and they preserve byte-identical
error semantics (`UserMemoryFault`/`InvalidAsid` never hidden). `M2_SEAM_HELPER_ONLY` — NOT
wired into any live IPC/syscall path. **It does not by itself retire the global lock from
IPC.** Guarded by `stage186e_vm_user_copy_seam` (seam callable + rejects unknown-ASID /
unmapped page with real faults; uses only VM/memory seams; no `&mut KernelState`; not wired
live). The added helpers are dead/uncalled in the release build → zero behavior change
(riscv64 core smoke green: `RISCV_PM_STARTUP_CAPS_OK`, `CROSS_ARCH_LIVE_DONE result=ok`).

**Remaining blocker for `ipc_reply` (186B):** the shared cap-transfer materialization engine
`materialize_received_message_cap_routed` (13 call sites) still has no seam form —
`Stage 186D-prereq CAP-TRANSFER-ENGINE-SEAM` is next. Only after **both** the VM user-copy
seam (this stage) **and** the cap-transfer seam exist can `ipc_reply` be converted
end-to-end. Full global-lock retirement remains deferred; APs remain online but wake-only;
and the standing caveat holds — no functional benefit until 187 (multi-dispatcher).

**Stage 186D-prereq (CAP-TRANSFER-ENGINE-SEAM) — audited, HARD-STOPPED, no runtime
conversion.** Goal was a seam-based cap-transfer materialization helper (via the rank-4
`with_capability_state_split_mut` seam added in 186A) that `ipc_reply` could later use
without a broad `&mut KernelState` and without holding `ipc_state_lock` during
capability/cnode materialization. On audit, the materialize path is **not cap-only** — a
single "materialize a received transfer/reply cap" spans **four** subsystems, and the
`with_capability_state_split_mut` seam structurally hands out only `&mut CapabilitySubsystem`
(it cannot reach task, ipc, or memory). Subsystem-touch inventory
(`cap_transfer_split.rs` → `capability_state.rs` / `capability_lifecycle_state.rs` /
`cnode_state.rs` / `memory_lifecycle_state.rs`):

| Step (transfer-cap arm) | Primitive | Subsystem(s) / rank |
|---|---|---|
| resolve source cnode | `task_cnode` → `with_task_then_capability` | task (2) **+** capability (4), fused |
| resolve source cap | `resolve_capability_for_task` → `capability_object_live` | capability (4); for `Endpoint`/`Notification` objects reads `with_ipc_state` → **IPC (3)** |
| mint into dest cnode | `mint_capability_in_cnode` → `ensure_cnode_space` + cnode mint | capability (4) |
| refcount the object | `mint_capability_in_cnode` → `adjust_memory_object_cap_refcount(+1)` | **memory (6)**, in the *same* critical section as the mint |
| record delegation | `record_delegated_capability_link` | capability (4) |
| link-full rollback | `fast_revoke_reply_cap_in_cnode` + `adjust_memory_object_cap_refcount(-1)` + `reclaim_memory_object_if_unreferenced` | capability (4) **+** memory (6) |

Two blockers make this a genuine hard stop, not a mechanical wrap:

1. **capability↔memory fusion in the mint (reclaim atomicity window).**
   `mint_capability_in_cnode` installs the cnode slot (rank 4) and increments the memory
   object's `cap_refcount` (rank 6) inside one global-lock critical section. Expressing this
   through a cap-only seam means dropping rank 4 and re-taking rank 6 between the two — which
   opens a window where a concurrent `reclaim_memory_object_if_unreferenced` can free an
   object that already has a cnode slot pointing at it (`cap_refcount` still 0). Closing that
   window requires a new cross-subsystem capability↔memory atomicity protocol, i.e. exactly
   the broad co-decomposition this stage is told to stop before.

2. **capability→IPC rank inversion in the reply arm (already flagged in D5).** The reply-cap
   arm additionally needs `try_set_reply_cap_waiter_cap` (IPC rank 3) *after* the rank-4
   mint (`cap_transfer_split.rs` Phase B'). A cap-only seam cannot perform that rank-3
   record-set; re-acquiring IPC after capability is the documented rank inversion.

Because minting a *reply* object (`CapObject::Reply`) is a no-op for
`adjust_memory_object_cap_refcount` (it early-returns on non-`MemoryObject`/`DmaRegion`),
one might imagine a reply-only cap seam — but blocker (2) still applies to it, and the
whole *point* of transfer caps is to move `MemoryObject`/`DmaRegion` authority, where
blocker (1) is unavoidable. So neither arm is cleanly seam-able through
`with_capability_state_split_mut` alone. Disposition: **`CAP_TRANSFER_SEAM_DEFERRED`** —
documented here, **not** emitted as a live marker on any legacy global-lock path (that would
dishonestly claim a split that does not exist). No runtime code changed. Pinned by
`stage186d_cap_transfer_engine_seam_entanglement` (source-grep guard asserting the fusion
points remain, so no future edit silently "wraps" them and overclaims a seam). The next real
move is a joint capability↔memory decomposition (a `Stage 186D` proper) that gives the
mint+refcount a shared atomicity discipline; only then does a cap-transfer seam become
honest.

**Stage 186D-proper (CAPABILITY-MEMORY-MINT-ATOMICITY) — DONE (infrastructure only, no
live conversion).** This stage removes the mint/refcount atomicity blocker that 186D-prereq
identified, as reusable seam-only infrastructure. Under the global lock,
`mint_capability_in_cnode` installs a cnode slot (rank 4) and bumps the referenced
memory-object `cap_refcount` (rank 6) in ONE critical section, so nothing observes a
half-state. A future cap-transfer *seam* runs under per-domain marker locks, which makes the
slot-install → refcount-bump window real: a concurrent `reclaim_memory_object_if_unreferenced`
in that window could free an object a freshly-published slot already references
(use-after-free). This stage builds the atomicity discipline for that mint.

Added `mint_capability_with_memory_ref_split` on `SharedKernel` (in
`boot/cap_memory_mint_split.rs`), plus its private phase helpers
`bump_memory_ref_for_mint_split` (rank 6), `install_cnode_slot_for_mint_split` (rank 4), and
`rollback_memory_ref_for_mint_split` (rank 6).

**Atomicity model — Model A ("pre-bump then install"):**

1. Phase 1 (rank 6, `with_memory_split_mut`): validate the memory-object is still live and
   increment its `cap_refcount` by one — the object is now protected against reclaim *before
   any cnode slot can reference it*. Non-memory-backed objects (`Reply`/`Endpoint`/…) are a
   no-op (their liveness is the caller's / IPC's concern, out of scope).
2. Phase 2 (rank 4, `with_capability_state_split_mut`): publish the capability into an
   existing destination cnode slot, minting a fresh **receiver-local** `CapId`.
3. Rollback: if Phase 2 fails (`CapabilityFull`, or absent cnode space → `TaskMissing`), drop
   the Phase-1 bump so a failed mint leaks nothing.

**Rollback proof / invariants.** At every instant a published slot exists, the object's
`cap_refcount` already protects it → no reclaim race. The only transient is a briefly
over-counted refcount when Phase 2 fails — harmless (can only delay a reclaim, never cause a
premature free) and always rolled back → increment/decrement stay symmetric (exactly one
each; the existing `revoke_capability_in_cnode` decrements the one this mint added).

**Lock-scope / rank proof.** The two critical sections are **disjoint** — Phase 1 releases
the memory lock before Phase 2 acquires the capability lock — so despite acquiring rank 6
before rank 4, the helper never holds two subsystem locks at once and cannot be part of any
lock-ordering cycle (deadlock-free). It never forms a broad `&mut KernelState`, never
acquires `ipc_state_lock` (rank 3), so it materializes no cap under the IPC lock and
introduces no cap→IPC rank inversion.

**Capability-authority proof.** The helper takes an already-formed `Capability` (object +
rights), not a foreign `CapId`, and publishes a freshly-minted receiver-local `CapId` via
cspace `.mint()` — it never echoes a sender-local CapId as authority. Rights derivation /
`WrongObject` / `MissingRight` happen upstream (before the mint) and pass through unchanged;
`StaleCapability` (dead memory object), `CapabilityFull` (full cspace), and `TaskMissing`
(absent cnode space) are all returned as real errors, never hidden as success.

`M2_SEAM_HELPER_ONLY` — **not wired into `ipc_reply`, `ipc_send`/`recv`/`call`, or the
cap-transfer materialization path.** It **does not by itself** convert any live path or
retire the global lock, and it **does not solve** the reply-cap IPC rank-inversion blocker
(the reply arm still records waiter-cap metadata into IPC rank 3 after the rank-4 mint — that
remains deferred). The added helper is dead/uncalled in the release build → zero behavior
change. Guarded by `stage186d_proper_cap_memory_mint_atomicity` (functional: success bumps
once + revoke balances, install failure rolls back, stale object rejected before publish;
source: no broad `&mut KernelState`, no IPC lock, only memory+capability seams, rollback
present, real errors, fresh receiver-local cap, not wired live, docs don't overclaim).

**Remaining `ipc_reply` blockers.** (a) The cap-transfer materialization *seam* still needs
to be built on top of this helper (composing envelope-consume with the atomic mint), and
(b) the reply-cap arm's IPC rank inversion (`set_reply_cap_waiter_cap` after mint) is still
unsolved. Full global-lock retirement remains deferred; APs remain online but wake-only; no
functional benefit until 187 (multi-dispatcher).

**Stage 186D2 (CAP-TRANSFER-MATERIALIZATION-SEAM-FIRST-SLICE) — DONE (infrastructure only,
no live conversion).** Builds the **first seam-based** cap-transfer materialization on top of
the Stage 186D-proper atomic mint. The existing live D1/D5 path
(`materialize_received_message_cap_routed` → `materialize_split_transfer_cap_equivalent`)
factors the work into typed phase functions but still runs under the **global lock**
(`grant_task_to_task_with_rights` on `&mut KernelState`). This slice adds a genuinely
seam-based materializer that touches only the rank-4 capability seam and rank-6 memory seam
(via the atomic mint) — never `ipc_state_lock`, never a broad `&mut KernelState`.

Added, in `boot/cap_transfer_materialize_split.rs` on `SharedKernel`:

- `TransferCapSnapshot { receiver_cnode, object, rights }` — a plain, IPC-lock-free snapshot
  captured AFTER the transfer envelope was consumed under `ipc_state_lock` and the source cap
  resolved to `(object, rights)`. It carries **object identity + rights**, never a
  sender-local `CapId` (local CapIds are not transferable authority).
- `materialize_received_cap_snapshot_split(snapshot) -> Result<CapId, _>` — mints the ordinary
  object cap into the receiver's cnode via `mint_capability_with_memory_ref_split`
  (pre-bump / publish / rollback), returning a fresh receiver-local `CapId`.
- `materialize_received_message_cap_routed_split(snapshot) -> Result<CapTransferMaterializeOutcome, _>`
  — routes ordinary object caps to the atomic mint (`Materialized(CapId)`) and **reply-cap
  objects to an explicit `DeferredReplyCap`** (reason `reply_cap_ipc_rank_inversion`).

**Snapshot / envelope-boundary proof.** The one-shot envelope is consumed under IPC (rank 3)
*before* the snapshot exists; this seam takes the snapshot by value and never sees, re-takes,
or reuses an envelope — one-shot semantics preserved at the interface boundary.

**Atomic-mint usage proof.** The seam goes *through* `mint_capability_with_memory_ref_split`
and never bypasses it (no direct cspace `.mint()`, no direct capability/memory seam) — so a
cap is never published without memory-object refcount protection, and the 186D-proper rollback
is preserved. Guarded functionally (memory transfer bumps `cap_refcount` +1; full cnode →
`CapabilityFull` with the bump rolled back).

**Lock-scope / rank proof.** No `ipc_state_lock`, no broad `&mut KernelState`, no cap→IPC
rank inversion — the seam holds only the atomic mint's disjoint rank-6 then rank-4 sections.

**Capability-authority proof.** Snapshot carries object + rights, never a sender-local CapId;
the receiver-local CapId is freshly minted. `StaleCapability` / `CapabilityFull` /
`TaskMissing` are real errors; `WrongObject` / `MissingRight` are produced upstream (source
resolution / rights derivation) and never converted to `Ok`.

**Reply-cap deferral proof.** Reply objects route to `DeferredReplyCap`
(`reply_cap_ipc_rank_inversion`); no reply-cap seam success marker is emitted, and no cnode
slot is published for a deferred reply.

`M2_SEAM_HELPER_ONLY` — **not wired** into `materialize_received_message_cap_routed`,
`ipc_reply`, `ipc_send`/`recv`/`call`, or any live delivery path. It **does not by itself**
convert any live path or retire the global lock. **It is not yet a live-equivalent of
`grant_task_to_task_with_rights`:** it materializes the cap (object + rights + fresh
receiver-local CapId + atomic refcount) but does **not** yet record the source→dest
**delegation link** the legacy grant records for revocation propagation. That delegation-link
record is a rank-4-only follow-on slice; until it lands, this helper MUST NOT be wired into
the live delivery path. Guarded by `stage186d2_cap_transfer_materialize_seam_first_slice`
(functional: ordinary transfer mints fresh local cap via atomic mint, identity/rights
preserved, stale rejected before publish, full cnode → CapabilityFull + rollback, reply
deferred not faked; source: no broad `&mut KernelState`, no IPC lock, uses atomic mint / no
bypass, no sender-local CapId authority, reply arm not faked, real errors, not wired live,
docs don't overclaim). Dead/uncalled in release → zero behavior change.

**Remaining `ipc_reply` blockers.** (a) the delegation-link follow-on for a live-equivalent
transfer materialize, and (b) the reply-cap IPC rank inversion
(`set_reply_cap_waiter_cap` after mint) — both still deferred. Full global-lock retirement
remains deferred; APs remain online but wake-only; no functional benefit until 187
(multi-dispatcher).

**Stage 186D3 (CAP-TRANSFER-DELEGATION-LINK-SEAM) — DONE (infrastructure only, no live
conversion).** Closes the 186D2 live-equivalence gap for **ordinary** transferred caps: the
legacy grant (`grant_task_to_task_with_rights`) records a sender→receiver **delegation link**
so revoking the sender's source cap propagates to the derived receiver cap; without it a
seam-materialized cap would be an orphan the revoke tree can't reach. On audit the delegation
link is **pure capability-domain (rank 4)** metadata
(`CapabilitySubsystem::delegated_capability_links`, written by
`record_delegated_capability_link`) — no IPC/task/memory lock — so it is a clean seam
candidate.

Delegation-link inventory:

| Concern | Where / how | Classification |
|---|---|---|
| link record | `record_delegated_capability_link` → `with_capability_state_mut` (rank 4) | `delegation_link_seam_candidate` |
| link data | `DelegatedCapabilityLink { source_tid, source_cap, dest_tid, dest_cap }` | capability-domain only |
| revoke dependency | `collect_delegated_descendants` + `revoke_capability_direct_in_process_cnode` read the links | preserved (revoke path unchanged) |
| atomic w/ publish? | must be consistent with the mint: on record failure the mint must be undone | rollback via atomic-mint inverse |
| refcount interaction | undo = clear slot (rank 4) then drop `cap_refcount` + reclaim (rank 6) | mirror of the mint install order |

Added, in `boot/cap_transfer_delegation_split.rs` on `SharedKernel`:

- `TransferCapDelegation { source_tid, source_cap, dest_tid }` — the source's **bookkeeping
  identity** (the revoke edge). `source_cap` is recorded only, **never** resolved-to-mint /
  treated as receiver authority.
- `record_cap_delegation_link_split` — records the link under the rank-4 capability seam
  (byte-for-byte the legacy table write; idempotent; `CapabilityFull` when the table is full).
- `materialize_received_cap_snapshot_with_delegation_split` — the seam **live-equivalent** of
  the legacy grant for ordinary caps: mints via the Stage 186D2 seam (atomic mint), then
  records the link (when `source_tid != dest_tid`, mirroring legacy); on record failure rolls
  the mint back.
- `materialize_received_message_cap_routed_with_delegation_split` — routes ordinary caps
  through the above and reply objects to `DeferredReplyCap` (never delegated, never faked).

Plus, in `boot/cap_memory_mint_split.rs`, `rollback_minted_cap_split` — the inverse of the
atomic mint: clear the receiver cnode slot (rank 4) THEN drop `cap_refcount` + reclaim
(rank 6).

**Delegation atomicity / rollback model.** (1) mint (atomic: pre-bump refcount, publish slot);
(2) record link (rank 4); (3) on record failure, `rollback_minted_cap_split` undoes the mint.
Teardown clears the slot **before** dropping the refcount (mirror of the mint's install order),
so no live slot ever references an object whose refcount was already dropped — no reclaim
race, no stale cnode slot (the cspace bumps the slot generation, invalidating the stale
`CapId`), no stale delegation edge (the link is never recorded on the failure path).
**Success ⇒ receiver slot + refcount + delegation metadata all consistent; failure ⇒ nothing
left behind** (no published cap, no refcount leak).

**Lock-scope / rank proof.** No `ipc_state_lock`, no broad `&mut KernelState`, no cap→IPC
rank inversion; the seam holds only disjoint rank-4 / rank-6 sections. **Capability-authority
proof:** the snapshot carries object + rights (authority) and the delegation carries
`source_cap` only as a recorded edge; the seam never resolves the source cap to mint and never
echoes a sender-local CapId as receiver authority. **Reply-cap deferral proof:** reply objects
route to `DeferredReplyCap` (`reply_cap_ipc_rank_inversion`); no reply-cap success marker; reply
caps are never delegated (they are one-shot, outside the delegation tree).

`M2_SEAM_HELPER_ONLY` — **not wired** into `materialize_received_message_cap_routed`,
`ipc_reply`, `ipc_send`/`recv`/`call`, or any live delivery path. It **does not by itself**
convert any live path or retire the global lock. It brings ordinary cap-transfer
materialization to seam **live-equivalence** (mint + delegation link + rollback). Guarded by
`stage186d3_cap_transfer_delegation_link_seam` (functional: ordinary transfer records a link
observed by revoke propagation; full link-table → CapabilityFull with cap + refcount rolled
back; same-task records no link but mints; reply deferred; stale rejected; source: no broad
`&mut KernelState`, no IPC lock, uses atomic mint + records links only / no bypass, no
sender-local CapId authority, delegation-failure rollback present, reply arm not faked, real
errors, rollback clears slot before refcount, not wired live, docs don't overclaim).
Dead/uncalled in release → zero behavior change.

**Remaining `ipc_reply` blockers.** The reply-cap arm's IPC rank inversion
(`set_reply_cap_waiter_cap` after mint) is still unsolved, and live wiring of the ordinary
transfer path (auditing every recv/delivery call site) is a separate future stage. `ipc_reply`
conversion and full global-lock retirement remain deferred; APs remain online but wake-only;
no functional benefit until 187 (multi-dispatcher).

**Stage 186D4 (ORDINARY-CAP-TRANSFER-LIVE-WIRING) — audited, HARD-STOPPED, no runtime
conversion.** Goal was to wire the ordinary cap-transfer path to the seam live-equivalent
helper (`materialize_received_message_cap_routed_with_delegation_split`). On audit this
**cannot be done safely** in the current architecture without the broad IPC/dispatch
decomposition this stage is told to stop before. No runtime code changed.

*Materialization call-site inventory* (`materialize_received_message_cap_routed`, the D1/D5
router in `ipc_recv_core.rs`):

| Call site | File / fn | Class |
|---|---|---|
| blocked-waiter delivery | `syscall.rs:474` `complete_blocked_recv_for_waiter(kernel: &mut KernelState)` | `defer_needs_broad_ipc_decomposition` |
| queued split-recv | `syscall.rs:872` `try_split_recv_queued_plain_with_snapshot_locked(kernel: &mut KernelState)` | `defer_needs_broad_ipc_decomposition` |
| reply-cap arm (both above) | routed inside `materialize_received_message_cap_routed` D5 | `defer_reply_cap_ipc_rank_inversion` |
| 11 remaining call sites | `syscall.rs` (4722/4756/4789/4824/4874/5001/5140/5184/5366) + equivalence tests | `test_only` |

So there are exactly **two live call sites**, both in `&mut KernelState` functions invoked
inside a `with`/`with_cpu` closure that holds the global `SpinLock<KernelState>` for the whole
body (`runtime.rs:266` `let mut guard = self.state.lock(); … f(&mut guard)`).

*Exact blocker.* The seam helpers are methods on `SharedKernel` that derive
`&mut CapabilitySubsystem` / `&mut MemorySubsystem` from `self.state.data_ptr()`. At both live
sites the body already holds a `&mut KernelState` (the global-lock guard) aliasing the *same*
`KernelState`. Forming the seam's `&mut Subsystem` from `data_ptr()` while that `&mut
KernelState` is live is **mutable aliasing = undefined behavior** (stacked/tree borrows).
Calling the seam safely requires dropping the `&mut KernelState` / global lock *before*
materialize — i.e. splitting the recv critical section (consume envelope under lock → drop
lock → seam-materialize). That is "convert `ipc_recv` out of the global lock" / broad IPC
decomposition (explicitly forbidden here), and it would also reorder the
copy-to-user → meta-encode → sender-wake sequence that `IPC_RECV_V2_SENDER_WAKE_ORDER_OK`
pins.

*Safe subset.* None that constitutes real seam wiring. A `&mut KernelState` re-implementation
of mint+link+rollback callable from the live path would be byte-identical to the existing
legacy `grant_task_to_task_with_rights` (already mint+link+rollback) — relabeling the legacy
global-lock path and emitting `CAP_TRANSFER_LIVE_SEAM_*` markers on it would be dishonest
(forbidden). So no `CAP_TRANSFER_LIVE_SEAM_*` success marker is emitted; the ordinary-transfer
seam stays `M2_SEAM_HELPER_ONLY`. This is the same architectural wall Stage 115 documented for
the IPC seam ("cannot be moved outside `with_cpu` until … is restructured").

*Recommended next step.* Stage 187 (multi-dispatcher) is the prerequisite: once the recv
delivery boundary releases the global lock before materialization (consume envelope under
`ipc_state_lock`, drop it, then materialize at `SharedKernel` level), the ordinary-transfer
seam (186D2/186D3) becomes wireable exactly as designed — the by-value snapshot + delegation +
rollback are already built for that boundary. Reply-cap materialization additionally still
needs the `reply_cap_ipc_rank_inversion` solution.

Pinned by `stage186d4_ordinary_cap_transfer_live_wiring_hard_stop` (the two live sites remain
`&mut KernelState` on the legacy router; the seam is not called from any live recv/syscall
file; no `CAP_TRANSFER_LIVE_SEAM_*` marker exists in `src/`). Reply-cap materialization,
`ipc_reply` conversion, and full global-lock retirement all remain deferred.

**Stage 187A (IPC-RECV-DELIVERY-BOUNDARY-SPLIT) — DONE (LIVE boundary split, queued-split
recv path).** Targets the recv/delivery boundary aliasing problem found by 186D4: the seams
derive `&mut Subsystem` from `data_ptr()` and therefore cannot run while the `with`/`with_cpu`
broad `&mut KernelState` is live. This stage splits the **queued-split recv delivery** (the
`SharedKernel::try_split_ipc_recv_queued_plain_into_frame` path — the one live site with a
`SharedKernel`-level frame) into:

- **Phase A** (`try_split_recv_queued_plain_with_snapshot_locked`, inside `with_cpu`, broad
  borrow live, byte-identical to pre-187A): plan → rank-3 dequeue → **legacy** cap
  materialization (`materialize_received_message_cap_routed`, reply-cap arm unchanged /
  deferred there) → deferred sender wake (§56 order:
  `IPC_RECV_V2_SENDER_WAKE_ORDER_OK … phase=before_writeback`) → ret2 commit → kernel-register
  writeback completes here. For a user-ASID receiver Phase A returns a **by-value**
  `RecvBoundaryUserCopySnapshot` (`RecvQueuedSplitPhaseA::PendingUserCopy`) instead of
  copying: owned fields only (asid, receiver tid, message, writeback plan, receiver-local
  rollback cap) — no borrows, no sender-local CapId as authority. **No seam call happens
  inside the closure** (pinned by guard).
- **Broad borrow dropped** (`with_cpu` returns; markers `IPC_RECV_BOUNDARY_SNAPSHOT_OK`,
  `IPC_RECV_BOUNDARY_GLOBAL_DROPPED_OK`).
- **Phase B** (`recv_core::execute_user_asid_plain{,_v2}_writeback_boundary`, taking
  `&SharedKernel`): the user copy runs through the **Stage 186E `copy_to_user_split` seam**
  (rank-5 VM + rank-6 memory marker locks only) — the **first live seam call on the recv
  path**. No `ipc_state_lock`, no capability lock, no broad borrow. §55 meta-first ordering
  and all failure modes byte-identical (`IPC_RECV_BOUNDARY_USER_COPY_SEAM_OK`).
- **Phase C** (`SharedKernel::complete_recv_boundary_user_copy`): frame commit
  (`IPC_RECV_BOUNDARY_SPLIT_DONE result=ok`) and §58 failure handling — cap rollback
  (`rollback_materialized_recv_cap`) and user-fault recording via brief `with_cpu`
  re-entries (no seam inside those closures). Undersized → rollback + `InvalidArgs`; v2 meta
  fault → rollback + `PageFault`; payload fault → fault record + `Ok` (no rollback) — exactly
  the legacy in-lock semantics; the message is consumed in every case (one-shot preserved,
  no envelope/message reuse).

*Ordering proof.* The externally observable §56/§58 sequence is unchanged: materialize →
sender wake → user writeback → user-visible return. The wake still fires in Phase A **before**
the writeback (which moved later, out of the lock) — `phase=before_writeback` remains
literally true; the queued-split markers that belong to the writeback
(`YARM_RECV_CORE_LIVE kind=user_plain{,_v2}`, `IPC_RECV_V2_META_QUEUED_SPLIT_OK`,
`IPC_RECV_V2_ROLLBACK_OK site=queued_split_*`) moved WITH the writeback to runtime.rs Phase C
— same live path, same meaning (guard `stage159bcd_target_markers_are_kernel_emitted`
re-homed to accept either kernel emission site and now requires a literal emission, not a
comment).

*Aliasing proof.* Phase B/C run only after the `with_cpu` closure returns, so no broad
`&mut KernelState` exists when `copy_to_user_split` derives its `&mut Subsystem` from
`data_ptr()`. The Phase C rollback/fault re-entries take the global lock fresh and call no
seam inside. Pinned by `stage187a_no_seam_call_inside_with_cpu_closure` and
`stage187a_boundary_executors_use_copy_seam_without_ipc`.

*Recv/delivery boundary inventory* (Stage 187A):

| Path | Class |
|---|---|
| queued-split user-ASID writeback (plain + v2) | `recv_boundary_split_candidate` → **LIVE split (this stage)** |
| queued-split kernel-register arm | completes in Phase A (no user copy — nothing to split) |
| queued-split cap materialization | stays legacy in Phase A (`defer_needs_broad_ipc_decomposition` for the seam form; the boundary now exists for a follow-on to move it to Phase B before a wake re-entry) |
| reply-cap arm (D5 inside the router) | `defer_reply_cap_ipc_rank_inversion` (unchanged) |
| `complete_blocked_recv_for_waiter` (blocked-waiter delivery, 7 call sites inside `with`-closures in ipc_state.rs/ipc.rs/fault_state.rs) | `defer_needs_broad_ipc_decomposition` (requires waiter-state rewrite; untouched) |
| timeout cleanup / fault delivery / call-reply path | `legacy_authoritative_runtime` (untouched) |

*What this stage does NOT do.* It **does not enable multi-dispatcher**/AP user scheduling
(APs remain wake-only); it **does not fully retire the global lock** (Phase A still runs
under it, and every other IPC path is unchanged); it **does not solve reply-cap**
materialization (the rank-inversion blocker stands; reply caps still materialize via the
legacy router in Phase A and are never routed through any seam); ordinary cap-transfer
**seam** wiring is still deferred — but it depends on exactly this boundary split and can now
be built as a follow-on (Phase B materialize via 186D2/186D3 before a Phase C wake re-entry).

Guarded by `stage187a_ipc_recv_delivery_boundary_split` (snapshot by-value / no sender-local
authority; no seam in the Phase A closure; boundary executors on `&SharedKernel` + copy seam
+ no IPC lock; §56 wake-before-writeback and §58 materialize-before-wake order pinned in
source; no forbidden markers; end-to-end functional: user recv delivered via the boundary,
one-shot consumption, undersized → real `InvalidArgs`; kernel receiver completes in Phase A).

**Stage 187B (ORDINARY-CAP-TRANSFER-SEAM-LIVE-ON-RECV-BOUNDARY) — DONE (LIVE, ordinary
cap-transfer materialization moved onto the 186D2/186D3 seam for the 187A queued-split
boundary).** This is the **first live use of the cap-transfer materialization + delegation
seam** (`M2_SEAM_LIVE_187B_CAP_TRANSFER`). It converts ordinary (non-reply, non-shared-region)
transferred caps delivered to a **user** receiver on the queued-split recv boundary from the
legacy in-lock router to the post-boundary seam.

*Reachability.* A userspace `ipc_send` of a `FLAG_CAP_TRANSFER` message to a buffered endpoint
with no blocked waiter **enqueues** it (Stage 4E: `ipc_try_send_queued_plain_endpoint_only` →
`Enqueued`, envelope stashed unbound), so a later `ipc_recv` receives it through the
queued-split path — this is the live path 187B converts. (Reply caps and cap transfers to a
*blocked* receiver go through `complete_blocked_recv_for_waiter`, which is out of scope.)

*Queued-recv cap-transfer inventory:*

| Step | Where | Class |
|---|---|---|
| envelope consume (`take_transfer_envelope`, rank 3) | Phase A, `phase_a_snapshot_ordinary_transfer` | `ordinary_cap_transfer_live_on_boundary` |
| ordinary object materialize | Phase B seam (`materialize_received_message_cap_routed_with_delegation_split`) | `ordinary_cap_transfer_live_on_boundary` |
| delegation-link record | inside the seam (186D3) | `ordinary_cap_transfer_live_on_boundary` |
| sender wake | Phase B, after mint, before writeback | `ordinary_cap_transfer_live_on_boundary` |
| return metadata (`encode_transfer_cap_ret`) | Phase B, after mint | `ordinary_cap_transfer_live_on_boundary` |
| user writeback | Phase B/C, 186E seam (from 187A) | `ordinary_cap_transfer_live_on_boundary` |
| reply-cap materialization | legacy Phase-A router (unchanged) | `defer_reply_cap_ipc_rank_inversion` |
| shared-region (`OPCODE_SHARED_MEM`) transfer | legacy Phase-A router (mapping obligation) | `legacy_authoritative_runtime` |
| kernel-register receiver cap transfer | legacy Phase-A router (no boundary to cross) | `legacy_authoritative_runtime` |
| blocked-waiter cap delivery | `complete_blocked_recv_for_waiter` | `defer_blocked_waiter_delivery` |

*Implemented.* `RecvQueuedSplitPhaseA::PendingOrdinaryCapUserCopy` +
`RecvBoundaryOrdinaryCapSnapshot` (by-value: `receiver_cnode`, `object`, `rights`,
`source_tid`, `source_cap`, `wake_tid`, `asid`, `receiver_tid`, `msg`, `writeback` — **no
`&mut KernelState`, no borrows, no sender-local CapId as authority**; `source_cap` is the
delegation parent edge only). Phase A helper `phase_a_snapshot_ordinary_transfer` consumes the
envelope once + resolves object/rights/cnode (no mint). Phase B/C
`SharedKernel::complete_recv_boundary_ordinary_cap`: seam mint → `encode_transfer_cap_ret` →
deferred wake → 186E copy → §58 rollback via `rollback_materialized_recv_cap`.

*Exact ordinary live path converted.* Non-shared-region `FLAG_CAP_TRANSFER`/`_PLAIN` (non-reply)
to a `UserMemory`/`UserMemoryV2` receiver on `try_split_ipc_recv_queued_plain_into_frame`.

*Deferred reply-cap path + blocker.* Reply caps (`FLAG_REPLY_CAP`) stay on the legacy Phase-A
router — the `reply_cap_ipc_rank_inversion` blocker (post-mint IPC rank-3 waiter record) is
unchanged; **reply-cap materialization remains deferred**.

*Snapshot / envelope one-shot proof.* The envelope is taken exactly once in Phase A
(`take_transfer_envelope`, rank 3) to build the by-value snapshot; the seam materializes from
the snapshot with no envelope access; a second recv finds nothing (functional test). A mint or
copy failure leaves the envelope consumed — identical to the legacy arm.

*Atomic-mint / delegation / rollback proof.* The seam routes through the Stage 186D-proper
atomic mint (functional test: object `cap_refcount` +1) and records the delegation link
(functional test: revoking the source cap removes the delegated receiver cap). A writeback
failure rolls the cap back via `rollback_materialized_recv_cap` (revoke + delegation removal +
refcount drop) — the same §58 rollback the legacy path used.

*Aliasing proof.* The seam mint runs only in `complete_recv_boundary_ordinary_cap`, i.e. AFTER
the `with_cpu` closure returned; Phase A (`syscall.rs`, under the borrow) calls no seam
(guard-pinned: the seam symbol appears only in `runtime.rs`, never in `syscall.rs`). The wake
and rollback use brief `with_cpu` re-entries that call no seam.

*Sender-wake ordering proof.* Order is materialize → wake → writeback: the boundary method
emits `CAP_TRANSFER_BOUNDARY_SEAM_ATOMIC_MINT_OK` before
`IPC_RECV_V2_SENDER_WAKE_ORDER_OK … phase=before_writeback` before the
`complete_recv_boundary_user_copy` writeback (guard-pinned by source position). Plain and
reply/kernel paths are byte-unchanged from 187A, so `IPC_RECV_V2_SENDER_WAKE_ORDER_OK`,
`IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`, and `IPC_RECV_V2_META_QUEUED_SPLIT_OK` are
untouched.

*Markers.* `CAP_TRANSFER_BOUNDARY_SEAM_BEGIN/SNAPSHOT_OK/ATOMIC_MINT_OK/DELEGATION_OK/DONE
result=ok` on the converted ordinary path only — never for reply caps or legacy paths.

`M2_SEAM_LIVE_187B_CAP_TRANSFER`. Scope honesty: this stage **does not enable multi-dispatcher**
/AP user scheduling; **does not fully retire the global lock** (Phase A still runs under it,
and reply/shared/kernel/blocked-waiter cap paths are unchanged); **reply-cap materialization
remains deferred** (rank inversion); and it does not convert `ipc_reply`, `ipc_send`,
`ipc_call`, or blocked-waiter delivery. Guarded by
`stage187b_ordinary_cap_transfer_seam_live_on_recv_boundary` (functional: queued mem-object +
endpoint cap transfers delivered via the boundary seam, atomic refcount, delegation/revoke
propagation, one-shot; source: Phase-A-consumes-only-no-mint, seam-only-post-boundary,
boundary-method-no-IPC-lock, materialize→wake→writeback order, reply/shared excluded, docs
honest).

**Stage 187C (IPC-REPLY-RETRY-AFTER-BOUNDARY-SEAMS) — audited, HARD-STOPPED, no runtime
conversion.** Retried the `ipc_reply` conversion with the 187A/187B boundary infrastructure now
available. On audit `ipc_reply` **cannot** be boundary-split within this stage's scope. No
runtime code changed.

*`ipc_reply` phase inventory* (`boot/ipc_state.rs::ipc_reply`):

| Phase | Work | Class |
|---|---|---|
| reply-cap resolve + right check | `resolve_send_cap_task_local` (rank 4) + SEND right | `legacy_authoritative_runtime` (no seam) |
| reply-cap consume (one-shot) | `with_ipc_state_mut { reply_caps[slot] = None }` (rank 3) | `reply_cap_consume_candidate` — but calls no seam |
| replier/caller cnode revoke | `fast_revoke_reply_cap_in_cnode` (rank 4) | `legacy_authoritative_runtime` (no seam) |
| reply delivery to a blocked caller | `complete_blocked_recv_for_waiter` (copy_to_user + `materialize_received_message_cap_routed`, all under the broad `&mut KernelState`) | `defer_blocked_waiter_delivery` |
| reply enqueue to a non-blocked caller | `endpoint.send(msg)` + waiter snapshot (rank 3) | `legacy_authoritative_runtime` — no copy/materialize here; the caller's later recv is already 187A/187B-split |
| reply-with-reply-cap materialization | D5 arm inside the router (`materialize_split_reply_cap_equivalent`) | `defer_reply_cap_ipc_rank_inversion` |
| caller wake | `apply_scheduler_wake_plan` (rank 1) | `legacy_authoritative_runtime` (no seam) |

*Exact blocker.* Every **seam-eligible** step in `ipc_reply` (the reply payload copy to the
caller, and any cap materialization the reply carries) lives inside
`complete_blocked_recv_for_waiter` — the **shared** blocked-waiter delivery path with **6
production call sites** (reply: `ipc_state.rs:1787/2258/2333`; send: `ipc.rs:445/931`; fault:
`fault_state.rs:456`) that runs the copy + materialize under the broad `&mut KernelState`.
Stage 187A/187B split the **queued** recv path (`try_split_recv_queued_plain_with_snapshot_locked`),
**not** this blocked-waiter path. Converting `ipc_reply` to post-boundary seams therefore
requires boundary-splitting `complete_blocked_recv_for_waiter` — **broad blocked-waiter
delivery decomposition**, explicitly out of scope here — and forking it for the reply case
only would duplicate the accepted helper and risk diverging its copy-failure / rollback /
sender-wake semantics (a "half-converted path"). Additionally, a reply that carries a reply
cap hits the D5 arm's unsolved **`reply_cap_ipc_rank_inversion`** (mint at capability rank 4,
then `set_reply_cap_waiter_cap` at IPC rank 3).

*Safe subset.* None that constitutes a real conversion. The reply-cap **consume** and the
cnode revokes are rank-3 IPC + rank-4 capability operations that are already correct under the
broad borrow; they call **no** seam and gain nothing from a boundary. The **enqueue** path
performs no user copy or cap materialization inside `ipc_reply` at all — the caller's
subsequent `ipc_recv` is where a queued reply's payload/cap is delivered, and that recv is
already boundary-split by 187A/187B. So there is nothing in `ipc_reply` to move onto a seam
without splitting the shared blocked-waiter delivery.

*Unsafe subset.* (1) reply delivery to a blocked caller via `complete_blocked_recv_for_waiter`
(blocked-waiter decomposition, forbidden); (2) reply-with-cap materialization
(`reply_cap_ipc_rank_inversion`, unsolved).

*Recommended next step.* **Stage 187D** — boundary-split the shared blocked-waiter delivery
`complete_blocked_recv_for_waiter` the way 187A split the queued path (Phase A: snapshot the
waiter ASID / payload / cap-transfer input under the lock; drop the borrow; Phase B: 186E copy
+ 187B cap seam; Phase C: wake), carefully preserving the semantics shared across its
reply/send/fault call sites and the sender-wake oracle. Then a **focused reply-cap
rank-inversion stage** for the D5 arm. Only after both can `ipc_reply`'s cap-carrying replies
be fully seam-converted. No runtime code changed; no `IPC_REPLY_BOUNDARY_*` marker is emitted
(that would dishonestly mark the legacy path). Pinned by `stage187c_ipc_reply_retry_hard_stop`
(ipc_reply still delivers via the shared broad-`&mut KernelState` blocked-waiter path; ipc_reply
calls no post-boundary seam; the D5 rank inversion is still present; no boundary marker in
`src/`). Broader IPC conversion, multi-dispatcher, and full global-lock retirement remain
deferred.

**Stage 187D (BLOCKED-WAITER-DELIVERY-BOUNDARY-SPLIT) — audited, HARD-STOPPED, no runtime
conversion.** Targeted the shared blocked-waiter delivery engine
`complete_blocked_recv_for_waiter` that 187C identified. On audit it **cannot** be
boundary-split within this stage's scope. No runtime code changed.

*Blocked-waiter delivery inventory* (production call sites of `complete_blocked_recv_for_waiter`):

| Call site | Enclosing `&mut KernelState` handler | Class |
|---|---|---|
| `syscall/ipc.rs:445` | `handle_ipc_send` (send → blocked recv-v2 waiter) | `blocked_waiter_boundary_candidate` (but no SharedKernel owner) |
| `syscall/ipc.rs:931` | `handle_ipc_call` | `blocked_waiter_boundary_candidate` (no owner) |
| `boot/ipc_state.rs:1787` | `ipc_reply` (reply → blocked caller) | `blocked_waiter_boundary_candidate` (no owner) |
| `boot/ipc_state.rs:2258/2333` | `ipc_send_with_optional_deadline` | `blocked_waiter_boundary_candidate` (no owner) |
| `boot/fault_state.rs:456` | `emit_fault_report_for_fault` (fault delivery) | `legacy_authoritative_runtime` (extra fault semantics) |
| reply-cap arm inside the router | D5 (`materialize_split_reply_cap_equivalent`) | `defer_reply_cap_ipc_rank_inversion` |
| user-copy (payload+meta), waiter GPR clear, wake | inside the helper | seam-eligible, but see blocker |

The helper's seam-eligible shape is identical to the 187A queued path (payload
`copy_to_user` → `materialize_received_message_cap_routed` → meta `copy_to_user` →
clear waiter GPRs → wake), so the *helper itself* could be refactored into a Phase-A
snapshot builder.

*Exact blocker (architectural).* All 6 call sites are `&mut KernelState` **syscall handlers**
(send / call / reply / deadline-send / fault) invoked from inside the **single main-dispatch
`with_cpu` closure** (`arch/trap_entry.rs`: `shared.with_cpu(cpu, |kernel| … dispatch …)`).
`complete_blocked_recv_for_waiter` sits ~4–5 `&mut KernelState` frames deep in that closure.
Running its Phase B/C (186E copy + 186D2/186D3 mint) requires `&SharedKernel` with the broad
borrow **dropped** — but no call site has `&SharedKernel`, and the borrow is only released when
the *entire* `dispatch()` closure returns to the SharedKernel-level trap entry. Contrast 187A:
`try_split_ipc_recv_queued_plain_into_frame` is a **dedicated SharedKernel-level pre-dispatch
fast path** (routed by `try_split_dispatch_into_frame` for NR2 only) that owns its own
`with_cpu`, so Phase A runs inside and Phase B/C after. Blocked-waiter delivery has **no such
SharedKernel-level owner** — `try_split_dispatch_into_frame` routes only `IpcRecv` and `VmBrk`;
send/reply/call fall straight through to the global-lock `dispatch()`.

*Safe subset.* Mechanically, `complete_blocked_recv_for_waiter` could be split into a pure
Phase-A snapshot builder — but that is **inert infrastructure with no live caller able to run
Phase B/C** (no real conversion), so this stage adds none (dead infra would masquerade as
progress).

*Unsafe subset.* Making Phase B/C run at the SharedKernel level requires either (1) a
**dispatch-return "pending post-boundary work" channel** threaded through every syscall handler
and the dispatcher, or (2) **forking each caller** (send/reply/call/deadline-send/fault) into a
dedicated SharedKernel-level pre-dispatch split that duplicates its resolution logic (endpoint
resolve, sender-waiter queue handling, reply-cap consume/revoke, fault semantics). Both are
**broad IPC endpoint/waiter + dispatch decomposition**, explicitly out of scope and named in
the hard-stop condition. The reply-with-cap sub-case additionally hits the unsolved
`reply_cap_ipc_rank_inversion`.

*Recommended next step.* This is genuinely the **multi-dispatcher / dispatch-boundary
restructuring** the roadmap defers to Stage 188+: give the main dispatch a typed
"pending post-boundary delivery" return channel so any handler can hand a blocked-waiter
delivery snapshot back to the SharedKernel level, where a single shared Phase-B/C executor
(reusing the 186E + 186D2/186D3 + 187A/187B machinery) finishes it. Only then can
`complete_blocked_recv_for_waiter` be split and `ipc_reply` (187C) be converted. No runtime
code changed; no `BLOCKED_WAITER_BOUNDARY_*` marker is emitted (that would dishonestly mark the
legacy path). Pinned by `stage187d_blocked_waiter_delivery_hard_stop` (the helper stays broad
`&mut KernelState` and copies/materializes under the borrow; all 6 call sites stay
`&mut KernelState` handlers; no send/reply/call pre-dispatch split exists; no boundary marker
in `src/`). Reply-cap materialization, broader IPC conversion, multi-dispatcher, and full
global-lock retirement all remain deferred.

**Stage 188A (DISPATCH-RETURN-DELIVERY-CHANNEL) — DONE (infrastructure only, inert).** Builds
the prerequisite 187D identified: a typed, by-value channel by which a syscall/IPC handler
running under the broad `with_cpu` / `&mut KernelState` borrow can hand *post-boundary work*
back to runtime, which executes it **after** the borrow is dropped through `&SharedKernel`
seams. This is the architectural mechanism that will let the shared blocked-waiter delivery
(and later `ipc_reply`) be split without threading pending work through every handler
signature.

*Dispatch-return inventory:*

| Element | Where | Class |
|---|---|---|
| post-`with_cpu` execution point | `arch/trap_entry.rs` (after `let inner_result = inner_result?;`, alongside the D2/D6 drains) | `dispatch_return_channel_candidate` — **used** |
| per-CPU stash idiom | Stage 117 `PerCpuSwitchPlanStash` / `DISPATCH_SWITCH_PLAN_STASH` | `dispatch_return_channel_candidate` — **mirrored** |
| 187A/187B runtime Phase B/C bridge | `runtime.rs` (`complete_recv_boundary_*`) | reused pattern |
| the 6 blocked-waiter call sites | send/call/reply/deadline/fault handlers | `post_boundary_work_candidate` — **not wired** (future) |
| reply-cap D5 arm | router | `defer_needs...` / `reply_cap_ipc_rank_inversion` |

*Implemented.* `crate::kernel::dispatch_post_work::DispatchPostWork` (`None` +
`BlockedWaiterPlainDelivery(BlockedWaiterPlainDeliverySnapshot)`) — **by value only**: no
`&mut KernelState`, no borrows, and no `CapId` (the wired variant is a *plain, no-cap* delivery,
so it carries no sender-local authority). Per-CPU stash `PerCpuDispatchPostWorkStash` /
`DISPATCH_POST_WORK_STASH` (mirrors the Stage 117 stash exactly; local-CPU trap path, IRQs
disabled). Runtime execution point `SharedKernel::drain_dispatch_post_work(cpu)` +
`execute_dispatch_post_work` — wired into `handle_trap_entry_shared` right after the broad
borrow drops. The `BlockedWaiterPlainDelivery` executor copies payload+meta to the waiter's
ASID through the **186E `copy_to_user_split` seam** (Phase B), then clears the waiter's return
registers (shared `KernelState::clear_blocked_recv_return_regs`, extracted byte-identically from
`complete_blocked_recv_for_waiter`) and wakes it (Phase C, a brief `with_cpu` re-entry with no
seam).

*Exact live path converted.* **None.** This stage is **infrastructure only**: no live handler
stashes work, so on every production trap the stash is empty and the drain is a no-op — **zero
runtime behavior change** (the drain emits only a one-shot `DISPATCH_RETURN_CHANNEL_READY
mode=helper_only`). The `BlockedWaiterPlainDelivery` executor is complete and **unit-tested
end-to-end** (stash → drain → 186E seam copy verified in user memory) but is produced by
nothing live; a future stage wires the blocked-waiter call sites to produce it.

*Aliasing / by-value / seam-execution proofs.* The drain runs only after `with_cpu` returns
(guard-pinned by source position), so no broad `&mut KernelState` is live when
`copy_to_user_split` derives its `&mut Subsystem` from `data_ptr()`. The enum/snapshot are
by-value (guard: no `KernelState`, no `&`, no `CapId`). The executor uses only the 186E seam and
no `ipc_state_lock`; its Phase-C `with_cpu` closure calls no seam (guard-pinned). No production
handler stashes work (guard: `DISPATCH_POST_WORK_STASH` absent from the syscall/IPC handler
files).

*Scope honesty.* This stage **does not enable AP user-task scheduling**, **does not fully
retire the global lock**, does not convert any IPC syscall, and does not solve the reply-cap
IPC rank inversion (`reply_cap_ipc_rank_inversion` — reply-cap materialization still blocked).
It **enables** future blocked-waiter delivery splitting and later `ipc_reply` conversion by
providing the dispatch-return execution point those need. Guarded by
`stage188a_dispatch_return_delivery_channel` (by-value enum; executor seam + no IPC lock;
drain after `with_cpu`; no live producer; no forbidden markers; empty-stash no-op; end-to-end
executor delivery via the seam; docs honest).

**Stage 188B (BLOCKED-WAITER-PLAIN-DELIVERY-LIVE) — DONE (first live producer, plain path
only).** Wires the *simplest* blocked-waiter delivery producer to the Stage 188A
dispatch-return channel, flipping it from `mode=helper_only` to `mode=live`. The chosen
producer is the `ipc_reply` recv-v2-blocked branch: a plain reply to a caller blocked in
recv-v2, where the message carries **no transferred cap and no reply cap**. Nothing else is
converted.

*Exact live path converted.* `ipc_reply` (`boot/ipc_state.rs`), recv-v2-blocked branch only,
plain messages only. Before the legacy `complete_blocked_recv_for_waiter` copy, the branch now
calls `crate::kernel::syscall::produce_blocked_waiter_plain_delivery(self, waiter_tid, endpoint_idx,
&msg)`:

- **`Ok(true)`** — a plain reply with a trap-entry drainer active: Phase A ran under the broad
  borrow (consumed `blocked_recv_state`, resolved the waiter ASID, stripped the inline opcode
  prefix, encoded the 40-byte recv-v2 meta with `cap_id = NO_TRANSFER_CAP`, and **pre-validated**
  both user buffers writable with *no copy*), stashed a by-value
  `DispatchPostWork::BlockedWaiterPlainDelivery`, and returned. The endpoint slot-clear + wake are
  **deferred to the executor** (Phase C). `ipc_reply` records the split delivery and returns `Ok`.
- **`Ok(false)`** — not plain (cap-transfer / reply-cap / `transferred_cap().is_some()`), **or no
  trap-entry drainer active** (direct/kernel-internal caller): falls through to the unchanged
  legacy `complete_blocked_recv_for_waiter` path. Zero behavior change on those paths.
- **`Err(..)`** — a buffer pre-validation failure (unmapped / non-writable waiter buffer):
  returned synchronously as `KernelError::UserMemoryFault`, byte-for-byte matching the legacy
  helper (which consumes the blocked state, then faults). **No stash, no wake.**

The executor (`runtime.rs`, `execute_dispatch_post_work` `BlockedWaiterPlainDelivery` arm) runs
after the broad borrow drops: two `copy_to_user_split` calls (payload then meta) through the
**186E seam** (Phase B), then a brief `with_cpu` re-entry (Phase C) that clears the waiter's
return registers, clears the endpoint receiver-waiter slot
(`ipc_clear_plain_receiver_waiter_only`), and applies the one-shot wake
(`apply_scheduler_wake_plan`). It emits `DISPATCH_POST_WORK_USER_COPY_OK`,
`IPC_RECV_V2_META_BLOCKED_WAITER_OK`, and `DISPATCH_POST_WORK_DONE kind=blocked_waiter_plain
result=ok`.

*Failure-semantics preservation.* The Phase-A pre-validation (a VM page-table walk with **no
copy**) makes the deferred copy infallible on the supported single-CPU user-scheduling config:
nothing runs between Phase A (inside `with_cpu`) and the drain (right after `with_cpu` returns)
that could change the waiter's mapping. So a delivery that would have faulted still faults
**synchronously** with the same `UserMemoryFault`, and a delivery that would have succeeded still
delivers exactly once. `WrongObject` / `StaleCapability` / `MissingRight` / `CapabilityFull` /
`UserMemoryFault` remain real errors on this path.

*Wake-once / ordering proofs.* The producer consumes `blocked_recv_state` exactly once and only
when it will stash (gated on `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu]`, so no orphaned stash
without a drainer). The executor performs the single slot-clear + wake in Phase C. The
`IPC_RECV_V2_META_BLOCKED_WAITER_OK`, `IPC_RECV_V2_SENDER_WAKE_ORDER_OK`, and
`IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE` markers are preserved. No lost wake, no duplicate
wake, no orphaned waiter.

*Scope honesty.* **cap-transfer blocked-waiter delivery remains deferred** — any message with a
transferred cap or reply cap stays on the legacy path (`Ok(false)`), because cap materialization
under the IPC rank is still blocked by `reply_cap_ipc_rank_inversion` (reply-cap materialization
still cannot run without broad IPC decomposition). This stage converts **only** the plain,
no-cap reply; it does **not** convert send/call/deadline/fault blocked-waiter delivery, does
**not** touch the multi-dispatcher, **does not enable AP user-task scheduling**, and **does not
fully retire the global lock**. It is the first live use of the 188A channel, nothing more.
Guarded by `stage188b_blocked_waiter_plain_delivery_live` (plain producer stashes + executor
delivers via the seam and wakes once; cap/reply message → `Ok(false)`; no drainer → `Ok(false)`;
unmapped buffer → synchronous `Err`, no stash; producer calls no seam under the broad borrow;
snapshot by-value; executor Phase C slot-clear + wake + meta marker; docs honest).

**Stage 188C (BLOCKED-WAITER-ORDINARY-CAP-DELIVERY-LIVE) — DONE (second live producer,
ordinary cap-transfer path only).**
Stage 188C wires ordinary cap-transfer blocked-waiter delivery through dispatch-return.
The `ipc_reply` recv-v2-blocked branch now defers a
single ORDINARY (non-reply, non-shared-region) transferred cap — an endpoint / notification /
memory-object / DMA-region object cap — instead of materializing + copying it under the broad
borrow. It is the cap-carrying sibling of the 188B plain producer and the first live wiring of
the Stage 186D2/186D3 cap-transfer seam onto the **blocked-waiter** path (187B wired it onto the
queued-recv path).

*Exact live path converted.* `ipc_reply` (`boot/ipc_state.rs`), recv-v2-blocked branch, ordinary
single cap transfers only. After the 188B plain producer returns `Ok(false)`, the branch calls
`produce_blocked_waiter_ordinary_cap_delivery(self, waiter_tid, endpoint_idx, &msg)`:

- **`Ok(true)`** — an ordinary cap transfer with a drainer active: Phase A (under the broad
  borrow) consumes `blocked_recv_state`, **pre-validates the payload buffer** writable (no copy —
  a fault here leaves the transfer envelope UNconsumed, matching the legacy payload-copy-before-
  materialize order), consumes the transfer envelope **once** and resolves the source object +
  rights + receiver cnode (`phase_a_snapshot_ordinary_transfer` — no mint, no seam),
  **pre-validates the meta buffer**, and stashes a by-value `BlockedWaiterOrdinaryCapDelivery`.
  Slot-clear + wake are deferred to the executor.
- **`Ok(false)`** — not an ordinary cap transfer (plain, reply cap, shared region, or no
  transferred cap), or no trap-entry drainer: falls through to the unchanged legacy
  `complete_blocked_recv_for_waiter` path.
- **`Err(..)`** — a real Phase-A error (undersized/unmapped buffer, missing/dead envelope,
  source-cap resolution) returned synchronously as `KernelError::UserMemoryFault`, with the SAME
  envelope disposition the legacy path produces on each fault.

The executor (`runtime.rs`, `execute_blocked_waiter_ordinary_cap_delivery`) runs after the broad
borrow drops: materializes the receiver-local cap through
`materialize_received_message_cap_routed_with_delegation_split` (Stage 186D2 atomic mint + 186D3
delegation link + rollback-on-delegation-failure), encodes the recv-v2 meta with that **fresh
receiver-local CapId** (`cap_id = local_cap`, `recv_meta_flags = TRANSFERRED_CAP`), copies
payload+meta through the **186E seam**, and — on a user-copy fault — rolls the freshly-minted cap
all the way back (`rollback_materialized_recv_cap`) so nothing leaks. Phase C clears the return
regs + receiver-waiter slot and wakes the waiter once. Markers:
`DISPATCH_POST_WORK_CAP_TRANSFER_SEAM_OK`, `DISPATCH_POST_WORK_USER_COPY_OK`,
`IPC_RECV_V2_META_BLOCKED_WAITER_OK`, `DISPATCH_POST_WORK_DONE kind=blocked_waiter_ordinary_cap
result=ok`; on a real failure, the honest `DISPATCH_POST_WORK_FAIL kind=blocked_waiter_ordinary_cap
reason=<materialize|user_copy|unexpected_reply_object>` (absent from a healthy boot log).

*Authority / envelope / wake proofs.* The transfer envelope is consumed exactly once in Phase A
(one-shot). The receiver-local CapId is minted **fresh** by the seam; the source `(source_tid,
source_cap)` is carried ONLY as the delegation-link parent edge (bookkeeping) and is **never
resolved-to-mint, never receiver authority** — a revoke of the source removes the delegated
receiver cap through that link. Phase-A pre-validation keeps the deferred copy infallible on the
supported single-CPU config, preserving failure semantics and wake-once ordering; the
`IPC_RECV_V2_META_BLOCKED_WAITER_OK` marker is preserved. `WrongObject` / `StaleCapability` /
`MissingRight` / `CapabilityFull` / `UserMemoryFault` remain real errors.

*Scope honesty.* reply-cap materialization remains deferred (`reply_cap_ipc_rank_inversion`):
any reply-cap message returns `Ok(false)` and stays legacy — reply objects route to the seam's
`DeferredReplyCap` and are never materialized here. Shared-region transfers (mapping obligation),
fault delivery, and the broader `ipc_send`/`ipc_call`/`ipc_reply` conversion remain deferred
unless explicitly converted. This stage converts only the ordinary single cap transfer on the
`ipc_reply` blocked-waiter path; it does not enable AP user-task scheduling and does not fully
retire the global lock. Guarded by `stage188c_blocked_waiter_ordinary_cap_delivery_live`
(ordinary producer stashes + executor materializes via the seam, delivers, and wakes once;
receiver gets a fresh receiver-local CapId, not the sender's; delegation link recorded; source
revoke removes the delegated cap; link-table-full → materialize rollback, no refcount leak;
reply/plain/shared/no-drainer → `Ok(false)`; missing envelope → synchronous `Err`, no stash;
producer calls no seam/copy under the broad borrow; snapshot by-value; executor uses the
cap-transfer + 186E seams + rollback; FAIL only on error paths; docs honest).

**Stage 188D (REPLY-CAP-RANK-INVERSION-SEAM) — DONE (rank-inversion solved at the seam;
infrastructure + dormant ipc_reply slice).**
Stage 188D builds the reply-cap rank-inversion seam — the smallest safe mechanism that finally
retires the long-standing `reply_cap_ipc_rank_inversion` blocker for the dispatch-return channel.

*The blocker.* Reply-cap materialization records the receiver-local reply CapId into the
reply-cap registry, which lives under `ipc_state_lock` (rank 3) — *below* the capability rank
(rank 4) where the cap is minted. Under the global lock the mint→record sequence is one critical
section; a seam split makes the window real, and doing it naively would hold the rank-4 lock while
taking the rank-3 lock (the inversion).

*The solution — phase separation, not nested acquisition.* The seam runs three **disjoint**
critical sections, each acquiring and fully releasing one subsystem lock before the next:
- **Phase A (broad borrow, producer):** `produce_blocked_waiter_reply_cap_delivery` takes the
  reply-cap transfer envelope ONCE (`phase_a_take_reply_envelope` — validates a live `Reply`
  object, resolves the receiver cnode), pre-validates the user buffers (no copy), ensures the
  receiver cnode space, and stashes a by-value `BlockedWaiterReplyCapDelivery` snapshot carrying
  only the reply object's registry coordinates `(reply_index, reply_generation)` — **no
  sender-local CapId as authority**. No mint, no IPC record, no seam.
- **Phase B (rank 4, executor):** mint the receiver-local reply cap via the Stage 186D-proper
  `mint_capability_with_memory_ref_split` (rank-4-only for a `Reply` object — no memory refcount).
  **No IPC lock held.**
- **Phase C (rank 3, executor):** record the receiver-local CapId into the reply-cap registry via
  the new `SharedKernel::try_record_reply_waiter_cap_split` (`with_ipc_split_mut` — IPC lock only,
  the exact rank-3 half of the D5 split). A stale record (generation mismatch / slot reused in the
  mint→record window) rolls the rank-4 mint back (`rollback_minted_cap_split`); a post-record
  user-copy fault additionally clears the recorded waiter-cap (`clear_reply_waiter_cap_split`).

The rank-4 mint fully releases its lock before the rank-3 record acquires `ipc_state_lock`, so the
seam **never holds the capability lock while taking the IPC lock**, never mints a cap under
`ipc_state_lock`, and never records IPC metadata under a broad `&mut KernelState` borrow. Markers:
`REPLY_CAP_RANK_SEAM_BEGIN` / `REPLY_CAP_RANK_SEAM_MINT_OK` / `REPLY_CAP_RANK_SEAM_IPC_RECORD_OK` /
`REPLY_CAP_RANK_SEAM_ROLLBACK_OK` / `REPLY_CAP_RANK_SEAM_DONE result=ok`, and the honest
`REPLY_CAP_RANK_SEAM_FAIL reason=<mint|stale_record|user_copy>` on real failures (absent from a
healthy boot log).

*Live-slice honesty.* The one reply-cap→blocked-waiter delivery path in a real boot is `ipc_call`
to a blocked server (all 53 boot reply-cap deliveries originate from `IPC_CALL_REPLY_CAP_CREATE`),
and wiring a live producer into `ipc_call` is out of Stage 188D scope (the broader
`ipc_send`/`ipc_call` conversion is deliberately untouched). The producer is therefore wired into
the sanctioned dispatch-return site (`ipc_reply`); a reply carrying a reply cap does not occur in
the current boot, so the seam is **exercised end-to-end by unit tests**, not by boot traffic —
`REPLY_CAP_RANK_SEAM_DONE` does not appear in a standard smoke, and that is correct. The rank
inversion is now **solved at the seam level** with no broad IPC decomposition; live wiring into
`ipc_call` is the explicit deferred next step.

*Scope honesty.* This stage converts reply-cap materialization only; it does **not** touch
shared-region transfer, fault delivery, or the broader `ipc_send`/`ipc_call` delivery paths, and
it **does not enable AP user-task scheduling** and **does not fully retire the global lock**. No
ABI/count change (`SYSCALL_COUNT=31`, `Syscall::VARIANT_COUNT=23`, x86_64 `MAX_ADDRESS_SPACES=32`).
Guarded by `stage188d_reply_cap_rank_inversion_seam` (producer takes envelope + validates + stashes
with no mint/record/seam under the broad borrow; executor mints BEFORE recording with rollback on
stale record / copy fault; rank-3 record seam holds only the IPC lock; snapshot by-value with no
CapId authority; reply cap consumed once; stale/wrong-object/missing-buffer stay real errors; exit
cleanup still revokes reply caps; docs honest).

**Stage 188E (IPC-CALL-REPLY-CAP-BLOCKED-WAITER-LIVE) — DONE (188D seam goes live on its real
path).** Stage 188D built the reply-cap rank-inversion seam and proved it by unit tests, but left
it dormant because the real reply-cap→blocked-waiter path flows through `ipc_call`, not
`ipc_reply`. Stage 188E wires that live: `handle_ipc_call`, on delivery to a blocked recv-v2
receiver, now calls `produce_blocked_waiter_reply_cap_delivery` **before** the legacy
`complete_blocked_recv_for_waiter`. On `Ok(true)` the caller's (sender's) frame is set to Ok and
the split is accounted; the receiver's mint + record + payload/meta copy + slot-clear + wake all
run in the dispatch-return executor after the broad borrow drops. `Ok(false)` (no trap-entry
drainer) and any non-reply-cap message keep the unchanged legacy path.

*Exact live path converted.* `handle_ipc_call` (`syscall/ipc.rs`), the recv-v2 blocked-receiver
branch, reply-cap messages only. Phase A (broad borrow) takes the reply-cap transfer envelope once
and stashes a by-value `BlockedWaiterReplyCapDelivery`; the 188D executor runs Phase B (rank-4
mint via `mint_capability_with_memory_ref_split`, no IPC lock) → Phase C (rank-3 record via
`try_record_reply_waiter_cap_split`, disjoint) → 186E copy → clear + wake. This makes
`REPLY_CAP_RANK_SEAM_DONE result=ok` and `DISPATCH_POST_WORK_DONE kind=blocked_waiter_reply_cap
result=ok` **live in a real boot**, alongside the 188B/188C plain and ordinary-cap DONE markers,
with the sender-wake ordering (`IPC_RECV_V2_SENDER_WAKE_ORDER_OK` /
`IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`) preserved.

*Envelope / one-shot / rollback.* The transfer envelope is consumed once in Phase A; on a producer
Phase-A fault before the take the legacy `take_transfer_envelope` cleanup consumes it, and after the
take that cleanup is a harmless no-op — the same envelope disposition the legacy arm produces. The
executor rolls the mint back on a stale record or user-copy fault (and clears the recorded
waiter-cap on a post-record fault), so no reply cap, refcount, or waiter-cap record is orphaned. On
the supported single-CPU config the Phase-A pre-validation keeps the deferred copy infallible, so
the deferred delivery cannot leave the caller with an accepted send but a lost wake.

*Scope honesty.* This stage converts **only** the `ipc_call` reply-cap blocked-waiter delivery; it
does not touch shared-region transfer, fault delivery, or unrelated `ipc_send`/`ipc_reply` paths,
does not enable AP user-task scheduling, and does not fully retire the global lock. No ABI/count
change (`SYSCALL_COUNT=31`, `Syscall::VARIANT_COUNT=23`, x86_64 `MAX_ADDRESS_SPACES=32`). Guarded by
`stage188e_ipc_call_reply_cap_blocked_waiter_live` (full IpcCall syscall to a blocked receiver
stashes reply-cap work and the drain mints + records + delivers a fresh receiver-local reply cap
and wakes the receiver; handle_ipc_call tries the producer before the legacy path, on the recv-v2
blocked branch only).

**Stage 188F (IPC-REPLY-BOUNDARY-LIVE-RETRY) — DONE (ipc_reply boundary split, superseding the
187C hard stop).** Stage 187C hard-stopped the `ipc_reply` conversion because its blocked-waiter
delivery blockers (cap-transfer seam aliasing, reply-cap rank inversion) were unsolved. Stages
188B/188C/188D/188E solved them; 188F retries and lands the `ipc_reply` boundary split.

*What changed.* The `ipc_reply` blocked recv-v2 delivery — which the 188B/188C/188D producers were
already wired into as three sequential matches — is now dispatched through a single boundary helper,
`try_ipc_reply_boundary_split`, that tries the plain → ordinary-cap → reply-cap producers in order
and emits the `IPC_REPLY_BOUNDARY_*` marker family
(`IPC_REPLY_BOUNDARY_SPLIT_BEGIN` / `IPC_REPLY_BOUNDARY_REPLY_CAP_CONSUME_OK` /
`IPC_REPLY_BOUNDARY_POST_WORK_STASH_OK kind=<plain|ordinary_cap|reply_cap>` /
`IPC_REPLY_BOUNDARY_SPLIT_DONE result=ok` / `IPC_REPLY_BOUNDARY_SPLIT_DEFERRED reason=…` /
`IPC_REPLY_BOUNDARY_SPLIT_FAIL reason=…`). This **does not duplicate** any 188B/188C/188D helper —
the helper reuses the existing producers; the seam mint + 186E user copy + slot-clear + wake still
run only in the trap-entry drain after the broad borrow drops. `ipc_reply` itself calls no seam
(guard-pinned).

*Phase A / one-shot.* Under the broad borrow `ipc_reply` already validates + consumes the replier's
reply-cap record once (`reply_caps[slot] = None`) and fast-revokes the caller/replier cnode slots,
exactly as the legacy path requires; the boundary helper then runs each producer's Phase A (consume
blocked state + any transfer/reply envelope once, pre-validate buffers, stash a by-value
`DispatchPostWork`). No seam / user-copy / cap-materialization runs under `ipc_state_lock` or the
broad borrow. Shared-region replies and the no-drainer case return `Ok(false)` →
`IPC_REPLY_BOUNDARY_SPLIT_DEFERRED` → the unchanged legacy `complete_blocked_recv_for_waiter` path.
The delivery inherits the 188B/188C/188D rollback (mint/refcount/delegation/waiter-cap) and
wake-once semantics; sender-wake ordering is preserved.

*Live markers.* Because the boot's blocked-recv-v2 replies flow through `ipc_reply`, this makes
`IPC_REPLY_BOUNDARY_SPLIT_DONE result=ok` live alongside the 41 plain and 10 ordinary-cap
`DISPATCH_POST_WORK_DONE` deliveries already produced by this path.

*Scope honesty.* Converts **only** the `ipc_reply` blocked recv-v2 delivery; shared-region transfer,
fault delivery, and unrelated `ipc_send`/`ipc_call` conversions remain deferred; **does not enable
AP user-task scheduling** and **does not fully retire the global lock**. No ABI/count change
(`SYSCALL_COUNT=31`, `Syscall::VARIANT_COUNT=23`, x86_64 `MAX_ADDRESS_SPACES=32`). Guarded by
`stage188f_ipc_reply_boundary_live` (a real ipc_reply to a blocked recv-v2 caller stashes
dispatch-return work, consumes the reply-cap record once, and the drain delivers + wakes; the helper
tries all three producers with no seam in Phase A; SPLIT_DONE is honest, only on a stash; the
boundary markers live only in `ipc_state.rs`).

**Increment 1 (Task 6.A — establish the SMP baseline + audit, no guard flip).**

- `run-ci-profiles.sh`: new `smp2-core` / `smp2-sender-wake` / `smp4-core` /
  `smp4-sender-wake` profiles (`x86_64 2|4 …`); the x86_64 core smoke now honors
  `QEMU_SMP=${QEMU_SMP:-1}` so `-smp 2/4` boots can be driven (not a production knob — it
  only selects the QEMU CPU topology).
- `orchestrator_state.rs`: `maybe_run_x86_smp_unlock_audit` — a one-shot, **read-only**
  audit (x86_64, `present>1` gated; silent on `-smp 1`) that emits `X86_SMP_UNLOCK_BEGIN`,
  the online-accounting check, and the honest verdict: `X86_SMP_AP_PARKED … reason=
  no_ap_scheduler_yet` + `X86_SMP_UNLOCK_BLOCKER category=B reason=
  ap_scheduler_admission_required` + `X86_SMP_NO_INLOCK_FALLBACK` + `X86_SMP_UNLOCK_DONE
  result=deferred reason=aps_not_admitted`. It flips **no** topology guard and mutates no
  scheduler state; when AP admission later lands (`online>1`) it emits
  `X86_SMP_APS_ADMITTED` + `result=aps_live` and the per-seam SMP invariant proof runs.
- Smoke/oracle: under `-smp >1` they require the `X86_SMP_UNLOCK_DONE` verdict and forbid
  `UNLOCK_GRADUATED_FALLBACK` / `UNEXPECTED_INLOCK_DISPATCH` / `emergency_optout` /
  `X86_SMP_ONLINE_ACCOUNTING_BAD` / `X86_TLB_REMOTE_ACK_TIMEOUT` / `D6_SMP_LOST_WAKE_FAIL`
  / `D6_SMP_DUP_WAKE_FAIL`.

**x86_64 SMP invariants to enforce once APs are admitted (the next increments, B→G):**
scheduler — no run-queue mutation without the rank-1 scheduler lock; no lost/duplicate
remote wake; no stale current-task after remote wake; no cross-CPU access to another CPU's
current task; IPC — endpoint waiter-queue ops atomic under the IPC lock; sender-wake stays
*before writeback* and after the receiver commits metadata; reply-cap one-shot; cap
transfer receiver-local; VM/TLB — page-table changes to a running ASID issue remote
shootdowns and wait for ACK before frame/PT reuse; COW write-protect visible on all CPUs;
no PT frame freed before remote ACK.

**Deliberately NOT done this increment (and why):** the `single_cpu` guard is **not**
relaxed and AP admission is **not** attempted — both require concurrent-execution proof
that is only obtainable from the `-smp 2/4` QEMU runs, and flipping them blind would risk
unverifiable data races / lost wakeups / use-after-free of page tables and would violate
"do not fake SMP success". No production fallback knob, opt-out, or global-lock fallback
was reintroduced; the in-lock path stays reachable only by topology (SMP>1 / non-x86_64).

**Guards preserved:** `UNLOCK_GRADUATED_POOL_LEAK`, sender-wake oracle checks,
allocation-free leaf-delete (`delete_if_leaf` + `has_any_delegated_child`), full recursive
`revoke` for non-leaf caps, the Stage 182 no-fallback guarantee. No PT-pool / cnode-slot /
task-limit / `MAX_ADDRESS_SPACES` change; no ABI/count change (`SYSCALL_COUNT=31`,
`Syscall::VARIANT_COUNT=23`, x86_64 `MAX_ADDRESS_SPACES=32`). Guarded by
`stage183_smp_live`. **Stage 184 is AArch64/RISC-V — not this stage.**

QEMU targets (run on the user's host — no QEMU in the dev environment):
`scripts/run-ci-profiles.sh smp2-core|smp2-sender-wake|smp4-core|smp4-sender-wake`, or
`QEMU_SMP=2|4 scripts/qemu-x86_64-core-smoke.sh` and
`QEMU_SMP=2|4 YARM_IPC_RECV_PROOF_SENDER_WAKE=1 scripts/qemu-ipc-recv-v2-oracle-smoke.sh x86_64`.
Expected this increment: boots `-smp N`, `X86_SMP_AP_PARKED` + `X86_SMP_UNLOCK_DONE
result=deferred reason=aps_not_admitted`, sender-wake still completes on the BSP, no
fallback/SMP-error marker. AP admission + the guard relax follow in the next increments.

#### 7.1.20.1 Stage 184 follow-up — two post-acceptance fixes (no invariant change)

Two defects surfaced after Stage 184 acceptance; both are fixed without weakening any
Stage 181–184 invariant (no fallback knob, no fake SMP / remote TLB ACK, no
ABI/count/limit change; `SYSCALL_COUNT=31`, `VARIANT_COUNT=23`, x86_64
`MAX_ADDRESS_SPACES=32` unchanged).

- **Issue A — x86_64 SMP-LIVE oracle drift.** The `-smp >1` smoke still required the
  obsolete `X86_SMP_AP_ENV_READY` / `X86_SMP_AP_INTERRUPT_READY` markers. After 183.5/183.6
  the audit emits those two only on the `online==1` branch; under `online>1` (every
  smp2/smp4 profile) it emits `X86_SMP_AP_SCHED_ONLINE` (carrying `ap_env_ready=`/
  `ap_interrupt_ready=` as inline fields). The required-marker loop in
  `scripts/qemu-x86_64-core-smoke.sh` now requires `X86_SMP_AP_SCHED_ONLINE` (already
  present) and no longer the stale pair; the whole 183.6 chain (`X86_SMP_UNLOCK_DONE
  result=smp_seams_ok`, `COW_SMP_TLB_ACK_OK`, `VM_UNMAP_SMP_TLB_ACK_OK`) and all
  forbidden-marker checks are unchanged. Guard tests `stage183_ap_admission_markers_present`
  / `stage183_inc4_ap_interrupt_safe_idle` track the modern marker.

- **Issue B — RISC-V userspace startup handoff.** On riscv64, `process_manager` (tid 3)
  reached userspace with a **zeroed** startup register hand-off
  (`STARTUP_INSTALL_FINAL task_id=0 … slots_len=0` → `PM_STARTUP_CAPS request_recv=0` →
  `PM_NO_RECV_CAP`), so it could not receive Init's SpawnV5, Init blocked, and the whole
  service chain stalled, ending in `RISCV_TRAP_UNHANDLED … reason=trap_from_s_mode`.
  Root cause: in the RISC-V trap bridge (`yarm_riscv64_trap_bridge`), the fresh-task
  ABI-register write-back (`frame.regs[A0..A5] = tframe.arg(..)`) fed a chain the optimizer
  proved dead — `frame` is reloaded only through the *raw* `frame_ptr` in the extern
  `yarm_riscv64_trap_return` asm, an access LLVM does not model as observing the
  `&mut frame`/`&mut tframe` stores, so it eliminated the whole
  `apply_user_context → tframe.args → frame.regs` chain and the task resumed with
  `a0..a5 = 0`. This only bit tasks resumed via the trap-frame write-back (PM); tasks
  entered via the direct sret (supervisor) or the kernel-context switch (init) were fine.
  The fix keeps the write-back's ABI-register loads observed by emitting the required
  `RISCV_STARTUP_ARGS` startup-cap marker over `tframe.arg(0..5)` immediately before the
  stores. New riscv64-gated attestations: `RISCV_STARTUP_ARGS` (trap bridge),
  `RISCV_STARTUP_CAPS_INSTALL_BEGIN/OK/BAD` (`yarm-user-rt` install path),
  `RISCV_PM_STARTUP_CAPS_OK/BAD` (`process_manager`). Result: PM boots with real caps
  (`request_recv`/`reply_recv` non-zero, `PM_BLOCKING_RECV_LOOP`), the full service chain
  spawns (`INITRAMFS_SRV_ENTRY … VIRTIO_BLK_SRV_ENTRY`), and the boot reaches the clean
  `RISCV_KERNEL_IDLE_WAITING_FOR_IO` idle with **no** S-mode trap. The change is
  `#[cfg(… target_arch = "riscv64")]`-gated; x86_64/AArch64 boot logs are byte-identical.
  `scripts/qemu-riscv64-core-smoke.sh` now requires the new markers + the full
  `DRIVER_MANAGER/BLKCACHE/VIRTIO_BLK` chain and rejects `PM_NO_RECV_CAP` /
  `*_INSTALL_BAD` / `*_CAPS_BAD` / `RISCV_TRAP_UNHANDLED` / `reason=trap_from_s_mode`.

QEMU acceptance: `QEMU_SMP=2 SMP_READY=1 scripts/qemu-x86_64-core-smoke.sh` (Issue A) and
`scripts/qemu-riscv64-core-smoke.sh` (Issue B); AArch64/x86_64 cross-arch markers preserved.

### 7.1.21 Stage 194 — CROSS-ARCH GLOBAL-LOCK RETIREMENT PORTABILITY AUDIT (audit/design only)

Audit-only stage: determines exactly how the x86_64-proven retirement mechanisms port to
AArch64 and RISC-V. **Nothing is enabled.** No out-of-lock retirement is activated on
AArch64 or RISC-V; `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE` is not flipped true on either;
`SYSCALL_COUNT=32`, `VARIANT_COUNT=23`, x86_64 `MAX_ADDRESS_SPACES=32` unchanged; the
compiled runtime is behaviorally unchanged (guard tests only, no runtime source touched).

#### Mechanism → portability classification

| Mechanism | x86_64 | AArch64 support | AArch64 missing | RISC-V support | RISC-V missing | genericization | classification |
|---|---|---|---|---|---|---|---|
| DebugLog | live | shared path + ABI-import hook (oracle-gated) | de-gate ABI import + finalize | none (raw trap path) | route trap through shared entry | make ABI import unconditional | generic_with_arch_restore_hook (no restore needed) |
| ~~InitramfsReadChunk~~ | **removed (Stage 197A)** — NR 27 syscall + split class deleted; zero-copy grant (NR 28/29) is the sole ELF-load path | — | — | — | — | — |
| FutexWake | live | same as DebugLog | same | same | same | same | generic_with_arch_restore_hook |
| D2 recv/send dispatch drain | live | shared path; restore hook exists | de-gate x86-only drain body + EL0-return proof | none | shared path + post-`sret` drain | de-gate `#[cfg(x86_64)]` drain block | x86_64_trap_entry_specific |
| FutexWait dispatch drain | live | as D2 | as D2 | none | as D2 | as D2 | x86_64_trap_entry_specific |
| Yield dispatch drain | live | as D2 | as D2 | none | as D2 | as D2 | x86_64_trap_entry_specific |
| IpcSendPlain → blocked recv-v2 | live | **ALREADY LIVE on shared path** | none | none | shared path prerequisite | none (drain already generic) | generic_with_arch_restore_hook |
| IpcSendOrdinaryCap → blocked | live | **ALREADY LIVE** (`class=IpcSendOrdinaryCap result=ok` observed on AArch64 core smoke) | none | none | as above | none | generic_with_arch_restore_hook |
| IpcSendReplyCap → blocked | live | ALREADY LIVE on shared path (fires when boot exercises it) | none | none | as above | none | generic_with_arch_restore_hook |
| IpcSendPlainEnqueue (no waiter) | live | rank-4 enqueue; no deferred work | ABI import to reach it live | none | shared path prerequisite | none | fully_generic_now (no drain at all) |
| IpcSendOrdinaryCapEnqueue (no waiter) | live | as PlainEnqueue | as PlainEnqueue | none | as PlainEnqueue | none | fully_generic_now |

Generic-vs-arch boundary: the split dispatcher (`try_split_dispatch_into_frame`), the
IpcSend boundary drain (`drain_dispatch_post_work`/`execute_dispatch_post_work`, ASID-based
`copy_to_user_split`), and the endpoint enqueue seams are already generic. Arch-specific:
(a) the decoded syscall ABI in the trap frame, (b) the return-path finalize
(`finalize_split_handled_syscall`), (c) the queue-advancing drain bodies + the post-switch
register/address-space restore (`post_switch_restore_arch_thread_state`).

#### TLB/ASID/SMP prerequisites

- x86_64: CR3(/PCID) switch on `d2_*_switch_incoming_asid`; local invalidate on switch;
  remote shootdown wired (`X86_TLB_SHOOTDOWN_*`). Only VM/SMP classes need remote shootdown.
- AArch64: TTBR0_EL1 + ASID switch; `TLBI` + `DSB ISH`/`ISB` maintenance already used at boot;
  a local ASID switch is sufficient for the recommended first slices; remote `TLBI` (broadcast)
  is required only for VM/SMP classes, which are NOT in scope.
- RISC-V: SATP(+ASID) switch on `sret`; `SFENCE.VMA` discipline; the shared-path prerequisite
  (routing through `handle_trap_entry_shared`) must land before ANY drain.
- SMP: recommended first slices (DebugLog/InitramfsReadChunk/*Enqueue) require NO cross-CPU
  dispatch and NO remote shootdown, so current AArch64/RISC-V SMP maturity does not block them.

#### Stage 195A — AArch64 DebugLog LIVE (DONE)

DebugLog (NR 15) is now a live AArch64 split-dispatch retirement class. The ABI import +
finalize are de-gated **selectively for DebugLog**: `pre_split_import_syscall_abi` peeks the
raw `x8` and imports the decoded ABI only when it is `SYSCALL_DEBUG_LOG_NR` (or the oracle knob
is on), so DebugLog is the ONLY newly-eligible class — every other syscall keeps `nr=0` and
takes the unchanged global-lock path. `finalize_split_handled_syscall` mirrors that selectivity.
Live markers on a normal AArch64 boot: `AARCH64_SPLIT_ABI_IMPORT_OK nr=15`,
`YARM_LOCK_SPLIT_DISPATCH arch=aarch64 nr=15`,
`GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=DebugLog result=ok`,
`AARCH64_SPLIT_FINALIZE_OK nr=15 result=ok`. Success/error registers are byte-identical to the
legacy `handle_debug_log` path. Queue-advancing classes stay `#[cfg(target_arch = "x86_64")]`;
x86_64 marker text is byte-identical (arch tag is empty off AArch64); RISC-V stays inert
(active flag force-false). See §4.6 of `doc/ARCH_AARCH64.md`.

#### Stage 195B — AArch64 InitramfsReadChunk LIVE (DONE — superseded by Stage 197A removal)

> **Historical.** Stage 197A removed the NR 27 `InitramfsReadChunk` syscall and its
> split-dispatch retirement entirely (the zero-copy MemoryObject grant is now the sole ELF-load
> path). The AArch64 pre-lock split gate is now `NR 15 || NR 10 (FutexWake) || oracle`. The
> record below is retained for history; the AArch64 return-path (`+4`/re-save) bug fix it drove
> is preserved and now guards every return-consuming split class (e.g. FutexWake).

InitramfsReadChunk (NR 27) is the second live AArch64 split-dispatch class. The selective
ABI-import gate is extended to `NR 15 || NR 27 || oracle` (the finalize gate mirrors it), so
InitramfsReadChunk reaches `try_split_initramfs_read_chunk_into_frame`. Only the **success
path** is retired: the helper returns `None` (unchanged global-lock fallback) for every
access-gate / arg / not-found / unwritable-destination / ASID-unavailable case, keeping
`MissingRight` / `InvalidArgs` / `UserMemoryFault` canonical. The destination write is a
two-pass validated copy (`copy_slice_to_user_asid_split_write`), so a fault leaves **zero**
bytes written (no partial user write). Immutable initramfs/CPIO data only; no mutation, no
alloc, no cap mint, no scheduler/IPC change, no TTBR0/ASID switch. Live markers:
`AARCH64_SPLIT_ABI_IMPORT_OK nr=27`, `YARM_LOCK_SPLIT_DISPATCH arch=aarch64 nr=27`,
`GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=InitramfsReadChunk result=ok`,
`AARCH64_SPLIT_FINALIZE_OK nr=27 result=ok`. DebugLog byte-for-byte preserved; x86_64 marker
text byte-identical; RISC-V inert. `CreateInitramfsFileSliceMo` (NR 28) stays global-lock-only.
See §4.6 of `doc/ARCH_AARCH64.md`.

#### Stage 195C — AArch64 FutexWake LIVE + live oracle (DONE)

FutexWake is the third live AArch64 split-dispatch class. **FutexWake is NR 10** — the Stage
195C task text's "NR11" is INCORRECT (NR 11 is `SpawnThread`; NR 9 is `FutexWait`). The
selective ABI-import gate is extended to `NR 15 || NR 27 || NR 10 || oracle` (finalize mirrors
it), so FutexWake reaches `try_split_futex_wake_into_frame`. The caller never task-switches; the
split only mutates waiter/run-queue state via the two-seam `futex_wake_split_mut` (Phase A
rank-2 task seam scans `Blocked(Futex)` → `Runnable`; Phase B rank-1 scheduler seam enqueues
each waiter once). Ineligible cases (invalid addr) return `None` → unchanged global-lock
fallback (canonical error). `FutexWait` (NR 9) stays global-lock-only — it BLOCKS the caller.
Live markers: `AARCH64_SPLIT_ABI_IMPORT_OK nr=10`, `YARM_LOCK_SPLIT_DISPATCH arch=aarch64 nr=10`,
`FUTEX_WAKE_SPLIT_BEGIN/DONE arch=aarch64 result=ok woke=N`,
`GLOBAL_LOCK_RETIRE_CLASS_BEGIN/DONE arch=aarch64 class=FutexWake result=ok`,
`AARCH64_SPLIT_FINALIZE_OK nr=10 result=ok`. DebugLog / InitramfsReadChunk byte-for-byte
preserved; x86_64 marker text byte-identical; RISC-V inert.

**Live oracle (default-off, `yarm.aarch64_futex_wake_oracle=1`).** Proves ACTUAL waiter
mutation, not just marker emission. init spawns a child thread, then blocks on a handshake futex
to hand the CPU to the never-run child — AArch64 fresh-dispatches it through the block/dispatch
path (the same one that first enters the control-plane servers into user mode; `yield` cannot
fresh-dispatch a never-run thread because its `kernel_context` is uninitialized). The child
wakes init (split FutexWake) and then blocks on the oracle word through the **legacy global-lock**
`FutexWait`; init resumes and wakes the child through the **split** path. The kernel's returned
wake COUNT is the authoritative signal (NOT timing): `first_wake=1` (the sole waiter → `Runnable`,
enqueued exactly once), then `second_wake=0` (no waiter remains). Proof marker:
`AARCH64_FUTEX_WAKE_LIVE_ORACLE_DONE result=ok first_wake=1 second_wake=0`. The oracle boots
single-CPU (`FUTEX_WAKE_ORACLE=1` forces `QEMU_SMP=1` in `qemu-aarch64-core-smoke.sh`): AArch64
dispatches user tasks on the BSP only and the freshly-spawned waiter is enqueued balanced, so on
SMP>1 it can land on a wake-only AP and never run. This is a single-dispatcher proof; FutexWake
*enablement* is independent of CPU count. See §4.6 of `doc/ARCH_AARCH64.md`.

#### Stage 195D — AArch64 BSP dispatch affinity (DONE) + FutexWait queue-advancing drain (foundation)

**BSP dispatch affinity (landed).** The AArch64 AP bring-up (`start_secondary_cpus`) now marks
every AP **wake-only before onlining it** and installs the idle current — mirroring the x86_64
183.5 pattern. The AArch64 AP runs no user dispatcher (its main loop only drains cross-CPU work
and `wfe`s), so this aligns the scheduler model with reality: `dispatching = online - wake_only`
collapses to 1, balanced/explicit user-task placement routes to the BSP (reusing the existing
`least_loaded_online_cpu` skip + `enqueue_on_with_priority` deny), and no unpinned user task
(e.g. a `SpawnThread` child) can strand on a non-dispatching AP. No new lock; AP kernel/interrupt
work preserved; the wake-only AP is correctly excluded from user-ASID TLB shootdowns (it never
loads a user TTBR0). Markers: `AARCH64_BSP_DISPATCH_AFFINITY_ACTIVE result=ok`,
`AARCH64_USER_TASK_PLACEMENT_OK tid=<tid> cpu=0`, `AARCH64_WAKE_ONLY_AP_QUEUE_REJECTED
tid=<tid> cpu=<ap>` (real rejections only). Result: AArch64 `-smp 2` now attests
`CROSS_ARCH_TOPOLOGY_OK reason=single_dispatcher` (was `TOPOLOGY_BLOCKED
reason=multi_dispatcher_unproven`), and the Stage 195C FutexWake oracle passes under **both**
`-smp 1` and `-smp 2`.

**FutexWait queue-advancing drain (foundation; live port deferred).** The x86_64 192A model
defers the FutexWait *dispatch* out of the broad lock: in-lock it publishes `Blocked(Futex)`,
clears `current`, records a one-shot per-CPU deferral, and skips the in-lock dispatch; the
trap-entry drain (after the global guard drops) re-verifies the waiter is still `Blocked(Futex)`,
runs the authoritative `dispatch_next_on`, marks the incoming task Running, and does the arch
restore. On AArch64 the arch building blocks already exist and are arch-neutral or have an
aarch64 arm: `futex_wait_reverify_blocked`, `futex_wait_dispatch_step_mut`,
`d6_genuine_mark_running_via_task_seam`, `d2_recv_switch_incoming_asid` (→ `hal.switch_address_space`,
TTBR0_EL1/ASID + TLBI/DSB/ISB), and `post_switch_restore_arch_thread_state` →
`restore_arch_thread_state_post_switch` (EL0 SPSR/ELR/GPR frame restore).

**First blocking return-path invariant.** The AArch64 in-lock handler
(`arch/aarch64/trap.rs::handle_trap_entry_with_fault_bookkeeping_mode`) is structured so that a
syscall which leaves `current == None|Some(0)` hits `idle_no_eret_loop()` **inside** the global
`with_cpu` guard (it never `eret`s; it `wfe`-spins waiting for an interrupt). The 192A deferral
leaves `current == None` on purpose (dispatch relocated out of lock), so on AArch64 it would spin
in the idle loop **before** `with_cpu` returns and the out-of-lock drain can run — a hang inside
the lock. Enabling FutexWait therefore requires first teaching the aarch64 handler to bypass
`idle_no_eret_loop()` (and the in-lock restore) specifically when a FutexWait deferral is pending
for this CPU, then returning cleanly so `handle_trap_entry_shared`'s drain performs the
dispatch+restore. That handler change is a real return-path edit (not a flag flip) and is the
first prerequisite; it is deferred rather than half-landed so the proven in-lock FutexWait path
(which the 195C oracle exercises) stays intact. **FutexWait is NOT enabled and emits no
`GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=FutexWait`.** Yield stays a separate future
slice (its re-enqueue proof is independent). See §4.7 of `doc/ARCH_AARCH64.md`.

#### Stage 196G — RISC-V Yield (NR 0) DEFAULT-ON out-of-lock retirement (DONE)

RISC-V promotes the 196D re-enqueue + switch machinery into a **DEFAULT-ON Yield
(NR 0) retirement class** — the preempt sibling of FutexWait, reusing the generic
`YIELD_DISPATCH_*` deferral + `preempt_reenqueue_current_cpu` + `yield_dispatch_step_mut`
(the same x86_64 192B / AArch64 195G seams) + the 196D–196F SATP/sfence/frame switch
(§9.8 of `doc/ARCH_RISCV64.md`). The 196D foundation oracle stays default-off + separate.

- **Yield vs FutexWait:** the caller stays Runnable, re-enqueued once, and ALWAYS has an
  incoming (another task or itself) — NO idle outcome, NO `Err(Internal)` sentinel.
  No-incoming after publication is a real `RISCV_YIELD_DISPATCH_FAIL reason=no_incoming`.
- **Default-on eligibility:** trap drain active + single dispatcher + BSP + no pending
  Yield/FutexWait/foundation deferral. No knob, no consume latch. First exercise emits
  `RISCV_YIELD_RETIRE_DEFAULT_ON result=ok`; retires every eligible Yield (not one-shot).
- **In-lock publish:** `preempt_reenqueue_current_cpu` (Runnable + enqueue-once at FIFO
  tail + clear current) → record generic Yield deferral → skip in-lock dispatch. Markers
  `RISCV_YIELD_DISPATCH_{DEFER_BEGIN,REENQUEUE_OK}`; failure clears + legacy fallback
  (never both/neither current+queued).
- **Handler bypass:** independent of FutexWait + 196D; requires a real Yield deferral
  (`RISCV_YIELD_HANDLER_BYPASS_{BEGIN,DONE}`).
- **Post-lock drain:** `yield_reverify_ready` (lock-dropped proof + current still cleared)
  → dequeue → current → Running → fresh `with_cpu`: real `write_satp`+`sfence.vma`+
  `restore_arch_thread_state` → `sret`. Markers `RISCV_YIELD_DISPATCH_{LOCK_DROPPED_OK,
  REVERIFY_OK,DEQUEUE_OK,CURRENT_SET_OK,RUNNING_OK,SATP_OK,SFENCE_OK,FRAME_OK,SRET_ARMED,
  DONE result=ok}` + `GLOBAL_LOCK_RETIRE_CLASS_{BEGIN,DONE} arch=riscv64 class=Yield
  result=ok`. No x86 CR3 / AArch64 TTBR0. state_changed → decline.
- **Oracles (default-off):** `yarm.riscv64_yield_two_task_oracle` (slot-5 = 5): A yields
  → switch to B → B blocks (default-on FutexWait) → A resumes once
  (`RISCV_YIELD_TWO_TASK_ORACLE_DONE result=ok outgoing_resumed=1`).
  `yarm.riscv64_yield_lone_task_oracle` (slot-5 = 6): the only task yields and
  self-redispatches (`RISCV_YIELD_LONE_TASK_ORACLE_DONE result=ok redispatched_self=1`),
  repeats bounded Yields (`..._REPEAT_OK`) proving non-one-shot, never idles.
- **Excluded:** NR27/D2/IpcSend/VM/spawn/fork/cap-mint/ReapFaultedTask/AP-user; NR 0 not
  in the pre-lock split gate. DebugLog + FutexWake + FutexWait live; foundation + FutexWait
  switch/idle oracles green; counts unchanged (32/23); no new kernel lock; 2 MiB
  trap-stack preserved. **RISC-V now matches the x86_64/AArch64 first cohort: DebugLog +
  FutexWake + FutexWait + Yield.** Next: **197** first-cohort cross-arch seal.

#### Stage 196F — RISC-V FutexWait DEFAULT-ON + post-lock IDLE seal (DONE)

RISC-V FutexWait (NR 9) retirement is now **DEFAULT-ON** for eligible traps (no
oracle knob, no one-shot consume latch in the kernel), and the post-lock drain
gained a genuine **no-incoming IDLE outcome** (§9.7 of `doc/ARCH_RISCV64.md`). The
196E switch chain is byte-preserved.

- **Default-on eligibility:** `futex_wait_current` publishes when trap drain active
  + single dispatcher + BSP + no FutexWait/196D deferral pending. Removed: the
  `runnable_count_on_cpu > 0` gate, `armed()`, and the `try_consume` latch. First
  exercise emits the one-shot `RISCV_FUTEX_WAIT_RETIRE_DEFAULT_ON result=ok`.
- **Switch outcome (unchanged 196E):** reverify Blocked → dequeue → current →
  Running → real `write_satp`+`sfence.vma`+`restore_arch_thread_state` → `sret`
  (`..._DONE result=ok`).
- **Idle outcome (new):** no incoming + caller still Blocked ⇒
  `RISCV_FUTEX_WAIT_DISPATCH_NO_INCOMING` → `POST_LOCK_IDLE_BEGIN` → `with_cpu`
  re-acquire confirms current None (`POST_LOCK_IDLE_LOCK_DROPPED_OK`) → clear
  deferral → `..._DONE result=idle` → `GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64
  class=FutexWait result=ok` → `POST_LOCK_IDLE_ENTERED`. NO frame restored, NO
  `sret`. **Stage 197B (typed idle):** the drain returns the explicit typed
  `RiscvTrapEntryOutcome::EnterKernelIdle { reason: FutexWaitNoIncoming }` (a SUCCESS,
  not an `Err(Internal)` sentinel); the bridge matches that variant, asserts the
  `current == None|Some(0)` invariant, emits `RISCV_TYPED_IDLE_OUTCOME result=ok`
  + `RISCV_KERNEL_IDLE_WAITING_FOR_IO` + timer/PLIC safe-point + `riscv_trap_halt` wfi.
  `current == None` is an invariant check, NOT the control-flow discriminator. The
  active flag was cleared before the drain (never true across the idle handoff / wfi).
  No second idle implementation.
- **Race preserved:** FutexWake race → `..._DEFERRED reason=state_changed` (clear +
  decline). Genuine errors still propagate as `Err` (never idle-washed).
- **Workload knobs (default-off):** `yarm.riscv64_futex_wait_oracle` (slot-5 = 3,
  switch workload, now under the default-on mechanism) +
  `yarm.riscv64_futex_wait_idle_oracle` (slot-5 = 4, last-task idle workload →
  kernel attestation `RISCV_FUTEX_WAIT_IDLE_ORACLE_DONE result=ok lock_dropped=1
  current_none=1 outgoing_blocked=1`).
- **Excluded:** Yield/NR27/D2/IpcSend/VM/spawn/fork/cap-mint/ReapFaultedTask/AP-user;
  NR 9 not in the pre-lock split gate. DebugLog + FutexWake live; foundation oracle
  green; counts unchanged (32/23); no new kernel lock; 2 MiB trap-stack preserved.
  A normal core boot may show zero FutexWait markers (mechanism is source/test-proven
  default-on). Next: **196G** RISC-V Yield retirement.

#### Stage 196E — RISC-V FutexWait (NR 9) queue-advancing RETIREMENT — live controlled oracle (DONE)

RISC-V enables the **first genuine off-global-lock syscall retirement that
context-switches the BLOCKING caller** — FutexWait (NR 9) — behind a **default-off,
ONE-SHOT controlled oracle** (§9.6 of `doc/ARCH_RISCV64.md`). A live retirement
PROOF, not the default-on seal (Stage 196F). No-incoming stays on the legacy path.

- **Reused generic seams** (no duplication): the in-lock publish + per-CPU
  `FUTEX_WAIT_DISPATCH_*` deferral + `futex_wait_reverify_blocked` +
  `futex_wait_dispatch_step_mut` (both un-gated to RISC-V) are the SAME x86_64
  (192A) / AArch64 (195E/F) seams; the arch restore reuses the 196D machinery
  (`cr3_for_asid` + `map_kernel_shared_into_asid` + `write_satp` +
  `restore_arch_thread_state`).
- **Default-off, one-shot** (`yarm.riscv64_futex_wait_oracle=1`): a compare_exchange
  CONSUMED latch retires exactly ONE eligible FutexWait; later calls (incl. the
  child's park) stay legacy. `armed` = knob-on AND !consumed.
- **Eligibility (MANDATORY incoming exists):** armed + shared trap drain + single
  dispatcher + BSP + no FutexWait/196D deferral pending + `runnable_count_on_cpu > 0`.
  No incoming ⇒ `RISCV_FUTEX_WAIT_RETIRE_DEFERRED reason=no_incoming`, no consume, no
  publish, legacy path (not a failure).
- **In-lock publish:** claim one-shot, `futex_wait_dispatch_try_defer`, skip in-lock
  dispatch (`return Ok(true)`). Markers `RISCV_FUTEX_WAIT_DISPATCH_{DEFER_BEGIN,
  BLOCK_PUBLISH_OK}`; publish failure → clear + `..._FALLBACK reason=defer_failed` →
  legacy (never Blocked-and-current).
- **Handler bypass:** `post_lock_bypass = foundation_bypass || futex_wait_bypass`;
  the FutexWait arm requires a real `futex_wait_dispatch_is_deferred` deferral,
  emits `RISCV_FUTEX_WAIT_HANDLER_BYPASS_{BEGIN,DONE}`, independent of the unchanged
  196D foundation bypass.
- **Post-lock drain:** `futex_wait_reverify_blocked` (rank-2 re-acquire proves the
  broad guard dropped → `LOCK_DROPPED_OK`, AND confirms still `Blocked(Futex)`) →
  dequeue B (rank-1) → set current → mark Running (rank-2) → fresh `with_cpu`: real
  `write_satp` (`csrw satp` + `sfence.vma`) + `restore_arch_thread_state` → `sret`.
  Markers `RISCV_FUTEX_WAIT_DISPATCH_{DRAIN_BEGIN,LOCK_DROPPED_OK,REVERIFY_OK,
  DEQUEUE_OK,CURRENT_SET_OK,RUNNING_OK,SATP_OK,SFENCE_OK,FRAME_OK,SRET_ARMED,DONE}` +
  `GLOBAL_LOCK_RETIRE_CLASS_{BEGIN,DONE} arch=riscv64 class=FutexWait result=ok`. NO
  x86 CR3 / AArch64 TTBR0. Honest race/failure: `..._DEFERRED reason=state_changed`
  (FutexWake raced) / `..._FAIL reason=no_incoming` (never fabricated).
- **Live oracle** (slot-5=3): A spawns B, blocks on FutexWait → switch to B; B emits
  `RISCV_FUTEX_WAIT_INCOMING_USER_OK`, wakes A via split FutexWake (count 1), parks
  legacy; A resumes once → `RISCV_FUTEX_WAIT_USER_RETURN_OK tid=<A> wake_count=1` +
  `RISCV_FUTEX_WAIT_LIVE_ORACLE_DONE result=ok blocked_tid=<A> dispatched_tid=<B>
  wake_count=1`.
- **Excluded / not yet claimed:** default-on FutexWait + post-lock idle (196F); NR 9
  NOT in the pre-lock split gate; Yield/NR27/D2/IpcSend/VM/spawn/fork/cap-mint/
  ReapFaultedTask/AP-user-dispatch. DebugLog + FutexWake live; 196D foundation oracle
  green; counts unchanged (32/23); no new kernel lock; 2 MiB trap-stack preserved.
  Next: **196F** — default-on FutexWait + post-lock idle/no-incoming.

#### Stage 196D — RISC-V queue-advancing context-switch drain FOUNDATION (DONE)

RISC-V proves a **genuine post-lock context switch** end-to-end — outgoing
userspace task A → in-lock publish → broad lock drops → post-lock drain dequeues
incoming B → B's SATP/ASID activated with the required `sfence.vma` → B's saved
frame restored → `sret` enters B (§9.5 of `doc/ARCH_RISCV64.md`). It enables
**ZERO new syscall retirement classes**: a separate default-off one-shot deferral
that reuses Yield (NR 0) only as a trigger and emits no Yield/FutexWait retirement
marker.

- **In-lock publish** (default-off, one-shot, BSP + single-dispatcher + trap-path
  gated) in `yield_current`: `riscv_queue_switch_foundation_try_defer` claims the
  one-shot, re-enqueues the outgoing task Runnable once + clears `current` via
  `preempt_reenqueue_current_cpu`, records the per-CPU deferral, SKIPS the in-lock
  dispatch (`return Ok(())`). Re-enqueue failure → clear + fall through to legacy
  dispatch. Markers `RISCV_QUEUE_SWITCH_FOUNDATION_{PUBLISH_BEGIN,REENQUEUE_OK}`.
- **Handler-return bypass** requires a real per-CPU deferral: after the broad-lock
  handler returns, the canonical in-lock restore is skipped only when
  `riscv_queue_switch_foundation_is_deferred` holds (no `current` to restore),
  returning cleanly from the bounded `with_cpu` phase. Inert for normal syscalls.
  Marker `RISCV_QUEUE_SWITCH_FOUNDATION_HANDLER_RETURN_OK`.
- **Post-lock drain** (broad guard genuinely dropped): `yield_reverify_ready`
  re-acquires the scheduler seam through the SharedKernel (impossible under a held
  guard → deadlock) → `LOCK_DROPPED_OK`; dequeue B via rank-1
  `yield_dispatch_step_mut` (`DEQUEUE_OK`), set B `current` (`CURRENT_SET_OK`),
  mark B Running via rank-2 `d6_genuine_mark_running_via_task_seam` (`RUNNING_OK`).
- **Real SATP/`sfence.vma` + frame restore** in a fresh bounded `with_cpu`:
  `cr3_for_asid` + `map_kernel_shared_into_asid` + `write_satp` (`csrw satp` THEN
  `sfence.vma x0, x0` — real hardware, not markers; NO x86 CR3 / AArch64 TTBR0),
  then `restore_arch_thread_state` restores B's sepc/sstatus/GPR frame; the bridge
  `sret`s into B. Markers `..._{DRAIN_BEGIN,SATP_OK,SFENCE_OK,FRAME_OK,SRET_ARMED,
  DRAIN_DONE}`. No-incoming / state-changed → honest
  `RISCV_QUEUE_SWITCH_FOUNDATION_FAIL reason=…` (never a fabricated idle/success).
- **Live oracle** (`yarm.riscv64_queue_switch_foundation_oracle=1`, default-off;
  init slot-5=2): init A spawns B, yields; B runs in userspace
  (`..._INCOMING_USER_OK tid=<btid>`) then parks (legacy NR 9 re-dispatches A); A
  resumes → `RISCV_QUEUE_SWITCH_FOUNDATION_ORACLE_DONE result=ok outgoing=<A>
  incoming=<B> outgoing_resumed=1` (full round trip).
- **Excluded:** FutexWait/Yield/InitramfsReadChunk(NR27)/D2/IpcSend/VM/spawn/fork/
  cap-mint/ReapFaultedTask (0 `class=<retirement>` markers, 0
  `RISCV_{FUTEX_WAIT,YIELD}_DISPATCH_*`). DebugLog + FutexWake stay live; counts
  unchanged (32/23); no new kernel lock; RISC-V AP user dispatch still gated; the
  switch drain adds only a bounded re-acquire on the existing trap stack — 2 MiB
  fix + TODO preserved. Next: **196E** RISC-V FutexWait retirement (context-switch
  the blocking caller on this foundation).

#### Stage 196C — RISC-V FutexWake (NR 10) split-dispatch retirement + live oracle (DONE)

RISC-V enables its **second** split-dispatch class, **FutexWake (NR 10)** — the
first RISC-V class that mutates kernel state off the global lock (§9.4 of
`doc/ARCH_RISCV64.md`).

- **Gate extended** to `nr == DEBUG_LOG_NR || nr == FUTEX_WAKE_NR`; markers
  nr-templated. FutexWait (NR 9) stays global-lock-only. Every other syscall
  still falls through to the broad-lock handler exactly once.
- **Reused 191B seam** `try_split_futex_wake_into_frame` →
  `futex_wake_split_mut`: Phase A (rank-2 task seam) flips matching
  `Blocked(Futex)→Runnable` into a bounded buffer; Phase B (rank-1 scheduler
  seam) enqueues each once. No broad `&mut KernelState`, no new lock, no nested
  task+scheduler lock; waiter state changes once, enqueue once, **caller stays
  `current`** (no context-switch, no SATP switch, no post-lock drain).
- **Same-task sret parity:** handled FutexWake returns EARLY (before the active
  flag / `with_cpu`); the helper writes the wake count via `set_ok(woke,0,0)`,
  and the bridge's same-task write-back finalizes (`sepc+4` once, `sstatus`
  preserved, a0 = wake count). Markers `FUTEX_WAKE_SPLIT_{BEGIN,DONE}
  arch=riscv64 result=ok woke=<n>`, `GLOBAL_LOCK_RETIRE_CLASS_{BEGIN,DONE}
  arch=riscv64 class=FutexWake result=ok`.
- **Live oracle** (`yarm.riscv64_futex_wake_oracle=1`, default-off; init slot-5=1):
  child blocks legacy NR 9; parent uses the authoritative handshake futex to know
  the child is `Blocked(Futex)`, wakes via split NR 10 (count 1), wakes again
  (count 0), child resumes once. `RISCV_FUTEX_WAKE_LIVE_ORACLE_DONE result=ok
  first_wake=1 second_wake=0 waiter_tid=<tid>` + userspace
  `RISCV_FUTEX_WAKE_USER_RETURN_OK first_wake=1 second_wake=0`.
- **Excluded:** FutexWait/Yield/InitramfsReadChunk(NR27)/D2/IpcSend/VM/spawn/fork/
  cap-mint/ReapFaultedTask (0 `class=<other>` retirement, 0
  `RISCV_{FUTEX_WAIT,YIELD}_DISPATCH_*`). DebugLog stays live; foundation markers +
  post-lock oracle preserved; counts unchanged (32/23); 2 MiB trap-stack fix +
  TODO preserved. Next: **196D** RISC-V queue-advancing foundation.

#### Stage 196B — RISC-V DebugLog (NR 15) split-dispatch retirement (DONE)

RISC-V enables **exactly one** split-dispatch retirement class: **DebugLog (NR
15)** — its first and only off-global-lock class (§9.3 of `doc/ARCH_RISCV64.md`).

- **Selective NR-15 gate.** The bridge imports the ABI (a7→nr, a0..a5→args); the
  shared wrapper's Phase 1 gates the split dispatcher behind
  `frame.syscall_num() == SYSCALL_DEBUG_LOG_NR`, so `try_split_dispatch_into_frame`
  (which also knows FutexWake/IpcRecv/VmBrk/InitramfsReadChunk/ControlPlaneSetCnodeSlots)
  can never service another class on RISC-V. Non-DebugLog syscalls fall through
  to the unchanged broad-lock handler exactly once.
- **Pure read.** Reuses `try_split_debug_log_into_frame` (bounded
  `current_tid_authoritative` + `copy_from_user_asid_split_read` + `USER_LOG` +
  `set_ok(0,0,0)`): no broad `&mut KernelState`, no scheduler/cap/IPC/switch
  mutation, no post-lock deferred work; UserMemoryFault stays canonical.
- **Same-task sret parity.** Handled DebugLog returns EARLY (before the active
  flag / `with_cpu`), so no drain is owed and nothing is left true across `sret`.
  The bridge's same-task ecall write-back finalizes: `sepc+4` once (bridge is the
  sole pre-advance; split adds none), `sstatus` preserved, a0/a1/a2 = `set_ok`
  lanes. No AArch64 ELR-over-advance analogue.
- **Markers:** `RISCV_SPLIT_ABI_IMPORT_OK nr=15`, `YARM_LOCK_SPLIT_DISPATCH
  arch=riscv64 nr=15 cpu=0 result=ok`, `GLOBAL_LOCK_RETIRE_CLASS_{BEGIN,DONE}
  arch=riscv64 class=DebugLog result=ok`, `RISCV_SPLIT_FINALIZE_OK nr=15
  result=ok` (kernel, one-shot), and `RISCV_DEBUGLOG_SPLIT_USER_RETURN_OK`
  (init userspace, after `INIT_RUN_ENTER` — proves userspace resumes after the
  split return).
- **Excluded:** FutexWake/FutexWait/Yield/InitramfsReadChunk(NR27)/D2/IpcSend/
  VM/spawn/fork/cap-mint/ReapFaultedTask all stay global-lock-only (0
  `class=<other>` retirement markers, 0 `RISCV_{FUTEX_WAIT,YIELD}_DISPATCH_*`).
  Shared-wrapper foundation markers + default-off post-lock oracle preserved.
  `SYSCALL_COUNT`/`VARIANT_COUNT` unchanged (32/23). 2 MiB trap-stack fix
  preserved with a bounded measurement TODO. Next: **FutexWake** (196C).

#### Stage 196A — RISC-V shared trap-path + post-lock drain foundation (DONE)

RISC-V now enters a **real shared trap path**, contract-equivalent to
`handle_trap_entry_shared`, while enabling **zero** retirement classes — the
Stage 196 step-1 prerequisite (§7.1.21 plan / `doc/ARCH_RISCV64.md` §9.2).

- **Old raw path retired.** Boot used `Bootstrap::init_static()` + a persistent
  raw `&'static mut KernelState` pointer (`install_riscv_trap_kernel_state` /
  `trap_kernel_state_mut`); the bridge held that `&mut` across the whole trap
  and called `handle_trap_entry` directly with no post-`with_cpu` drainer.
- **New shared path.** `run_with_prepared_kernel` owns `KernelState` through a
  boot-constructed `SharedKernel` (`init_shared_static` + `borrow_kernel_for_boot`,
  the Stage-2N pattern) and installs a `SharedKernel` pointer
  (`install_riscv_trap_shared_kernel`). The bridge routes through
  `arch/riscv64/trap.rs::handle_riscv_trap_entry_shared`: **pre-lock** declines
  every RISC-V syscall (no `try_split_dispatch_into_frame`); **broad-lock**
  (`with_cpu`) sets `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu]=true` and runs the
  **unchanged** canonical `handle_trap_entry_with_fault_bookkeeping_mode`;
  **post-lock** clears the flag and runs `drain_dispatch_post_work` after the
  guard drops. Every remaining bridge kernel read (current-tid, asid) is a
  bounded `with_cpu` — no persistent raw `&mut KernelState` escapes.
- **Active-flag ownership** is centralized in the wrapper (§9.2 markers,
  one-shot latched). The §9.1 force-false is **retired**: a real drainer now
  exists, so the deferred blocked-waiter path is valid.
- **Genuine drain proof** (default-off `yarm.riscv64_post_lock_foundation_oracle=1`):
  publish token in-lock → drop guard → consume post-lock via a **real `with_cpu`
  re-acquire** (deadlocks if still held) → `sret` to the same task. Markers
  `RISCV_POST_LOCK_FOUNDATION_ORACLE_{PUBLISH_OK,LOCK_DROPPED_OK,DRAIN_OK,
  USER_RETURN_OK,DONE result=ok}`. No scheduler/cap/user/switch mutation.
- **Part 5 restore foundation:** RISC-V `post_switch_restore_arch_thread_state`
  is no longer a silent no-op — it delegates to the documented
  `restore_arch_thread_state_post_switch` (frame sepc/sstatus/GPR/TLS restore);
  still uncalled in production (no switch class enabled), paired at call time
  with the bridge's incoming-asid SATP activation + `sfence.vma`.
- **Trap-stack fix:** the shared wrapper's canary flag exposed a pre-existing
  latent overflow — the 16 KiB `RISCV_TRAP_STACK` was too small for the deepest
  syscall dispatch (256 KiB–1 MiB), silently clobbering adjacent `.bss`. Bumped
  to 2 MiB (a real RISC-V correctness fix).
- **Zero retirement:** 0 `YARM_LOCK_SPLIT_DISPATCH arch=riscv64`, 0
  `GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64`, 0 `RISCV_FUTEX_WAIT_DISPATCH_*` /
  `RISCV_YIELD_DISPATCH_*`. `SYSCALL_COUNT`/`VARIANT_COUNT` unchanged (32/23);
  `ReapFaultedTask` excluded; NR 27 deprecation preserved. Recommended next
  class: **DebugLog only** (Stage 196B). Full RISC-V server chain still reaches
  `RISCV_KERNEL_IDLE_WAITING_FOR_IO` with no S-mode fault. x86_64 / AArch64
  unchanged.

#### Stage 195G — AArch64 Yield out-of-lock queue-advancing dispatch DEFAULT-ON (DONE)

AArch64 **Yield (NR 0)** is now the fourth live queue-advancing out-of-lock class, **default-on**,
reusing the FutexWait infrastructure. As the *preempt* sibling, publication is re-enqueue-only:
`yield_current` sets the caller Runnable and re-enqueues it exactly once at the queue tail via
`preempt_reenqueue_current_cpu` (192B seam) + clears `current`, records the one-shot deferral,
skips the in-lock `on_preempt` dispatch (eligible on the BSP + shared drain + single-dispatcher +
not-already-deferred; re-enqueue failure leaves `current` untouched → legacy fallback). A
Yield-specific handler bypass (parallel to FutexWait; `post_lock_bypass = futex_wait_bypass ||
yield_bypass`) returns cleanly so the post-lock Yield drain runs: `yield_reverify_ready` →
`yield_dispatch_step_mut` (rank-1 dequeue of the FIFO head — another task, or the caller itself
when alone; **always an incoming, NO idle outcome**) → mark Running → AArch64 TTBR0/ASID + EL0
frame restore (same hooks as FutexWait, **no CR3**). Markers:
`AARCH64_YIELD_DISPATCH_{DEFER_BEGIN,REENQUEUE_OK,REVERIFY_OK,DEQUEUE_OK,CURRENT_SET_OK,RUNNING_OK,
TTBR0_OK,FRAME_OK,DONE result=ok}`, `AARCH64_YIELD_HANDLER_BYPASS_BEGIN/DONE`,
`AARCH64_YIELD_RETIRE_DEFAULT_ON result=ok`, `GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64
class=Yield result=ok`. Oracles (default-off): two-task
`AARCH64_YIELD_TWO_TASK_ORACLE_DONE result=ok outgoing=<A> incoming=<B>` and lone-task
`AARCH64_YIELD_LONE_TASK_ORACLE_DONE result=ok tid=<A> redispatched_self=1`, both under `-smp 1`
and `-smp 2`. `yield_reverify_ready` + `yield_dispatch_step_mut` un-gated to AArch64. D2 recv/send
drains stay inactive; AP user dispatch not enabled; NR 27 deprecation TODO preserved. See §4.10 of
`doc/ARCH_AARCH64.md`.

#### Stage 195F — AArch64 FutexWait DEFAULT-ON + post-lock idle seal (DONE)

The AArch64 FutexWait out-of-lock retirement is now the **default production path** (no enable
knob) for eligible traps: eligibility = `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE` + `dispatching_cpu_count()
<= 1` + BSP + not-already-deferred (the 195E `runnable_count_on_cpu > 0` requirement is REMOVED).
The post-lock drain has two successful outcomes: **Switch** (byte-identical to 195E) and **Idle**
(no incoming → outgoing stays `Blocked(Futex)`, `current` None, deferral cleared, NO frame
restored, enter the real BSP idle loop `enter_post_lock_idle`→`idle_no_eret_loop` AFTER the broad
`with_cpu` lock is released — never while holding it). Idle markers:
`AARCH64_FUTEX_WAIT_DISPATCH_NO_INCOMING`, `..._POST_LOCK_IDLE_BEGIN`,
`..._POST_LOCK_IDLE_LOCK_DROPPED_OK` (a real re-acquire of the released broad lock), `..._DISPATCH_DONE
result=idle`, `..._POST_LOCK_IDLE_ENTERED` + `class=FutexWait result=ok`. Default-on attestation:
`AARCH64_FUTEX_WAIT_RETIRE_DEFAULT_ON result=ok`. IRQs are not permanently masked (`wfi` wakes on a
pending interrupt into the normal trap path; `current == None` ⇒ no stale frame). Legacy in-lock
fallback retained only for genuinely ineligible traps. Idle oracle (default-off,
`yarm.aarch64_futex_wait_idle_oracle`): `AARCH64_FUTEX_WAIT_IDLE_ORACLE_DONE result=ok lock_dropped=1
current_none=1`, then WFI idle to timeout. Yield inert; NR 27 deprecation TODO preserved. See §4.9
of `doc/ARCH_AARCH64.md`.

#### Stage 195E — AArch64 FutexWait queue-advancing drain LIVE (DONE)

The 195D blocker is resolved and the AArch64 **FutexWait (NR 9)** queue-advancing dispatch is
live out of the broad lock under **SMP=1 and SMP=2**, porting the x86_64 192A model with **no
CR3 logic**. Default-off behind `yarm.aarch64_futex_wait_oracle=1`. (1) **Handler bypass**
(`arch/aarch64/trap.rs`): when `futex_wait_dispatch_is_deferred(cpu)` and `current` is None/idle,
skip `idle_no_eret_loop()` + the in-lock restore and return cleanly (markers
`AARCH64_FUTEX_WAIT_HANDLER_BYPASS_BEGIN/DONE`); any other None/idle keeps the idle loop. (2)
**In-lock deferral** (`futex_wait_current`): BSP + shared-drain-active + `dispatching_cpu_count()
<= 1` (195D) + another-runnable-present + not-already-deferred → publish `Blocked(Futex)`, clear
`current`, one-shot defer, skip in-lock dispatch; else legacy fallback. (3) **Post-lock drain**
(`handle_trap_entry_shared`): generic reverify/dequeue/current/mark-Running seams (un-gated to
AArch64) + AArch64 arch hooks — TTBR0/ASID via `hal.switch_address_space` (DSB/ISB/TLBI) + EL0
frame via `restore_arch_thread_state_post_switch`; `state_changed` race declined, deferral cleared
on every path. Emits `GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=FutexWait result=ok`. Live
oracle: `AARCH64_FUTEX_WAIT_LIVE_ORACLE_DONE result=ok blocked_tid=<A> dispatched_tid=<B>
wake_count=1` (A blocks via NR 9 → drain dispatches B → B wakes A via NR 10 → A resumes once).
Yield stays inert. See §4.8 of `doc/ARCH_AARCH64.md`.

#### Stage 195 — AArch64 first live retirement (implementation plan)

1. **[195A DONE]** De-gate `pre_split_import_syscall_abi` + `finalize_split_handled_syscall`
   selectively for DebugLog so a normal boot carries the decoded ABI for NR 15 only
   (byte-identical fallback for every other syscall). Proven by the AArch64 core smoke:
   non-eligible syscalls still fall back.
2. **[195A DONE]** Enable **DebugLog** first (pure read, no switch, no drain, no address-space
   change): `GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=DebugLog result=ok` on AArch64
   with the server chain intact. **[195B DONE]** **InitramfsReadChunk** success path (read-only
   user-copy, two-pass validated write). **[195C DONE]** **FutexWake** (NR 10, waiter/run-queue
   mutation, no caller task-switch) + a default-off live oracle proving real waiter mutation
   (first_wake=1, second_wake=0). **Next:** **IpcSendPlainEnqueue** (rank-4 enqueue, no drain).
3. Do NOT enable D2/FutexWait/Yield: those need the de-gated drain body + an EL0-return frame
   restore proof (`restore_arch_thread_state_post_switch` under the drain), deferred to a later
   stage.
4. Acceptance: AArch64 core + full server chain green; the enabled class marker present;
   `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE` still only true within `handle_trap_entry_shared`.

#### Stage 196 — RISC-V shared-path prerequisite + first live retirement (implementation plan)

1. Prerequisite (no retirement yet): route the RISC-V trap bridge through a shared wrapper
   (`handle_trap_entry_shared` or an equivalent that takes `&SharedKernel` and drains after the
   borrow drops), preserving the existing SATP/ASID `sret` restore. Prove the full server chain
   still reaches `RISCV_KERNEL_IDLE_WAITING_FOR_IO` with no S-mode trap and no stash-without-drain.
2. Only after the drain exists: allow the RISC-V path to own `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE`
   (set true within the shared wrapper, cleared after) instead of force-false.
3. Enable **DebugLog** first, then **InitramfsReadChunk**, then **IpcSendPlainEnqueue**.
4. Do NOT enable queue-advancing classes until a post-`sret` context-switch drain +
   `SFENCE.VMA`/SATP restore proof exists.

#### Stage 197 — cross-arch seal acceptance gates

- Per-arch: `GLOBAL_LOCK_RETIRE_CLASS_DONE class=<enabled>` present ONLY for the classes that
  arch has actually enabled; zero for the rest.
- `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE` provably owned per-CPU by the shared entry on every arch
  that enables a drain; force-false on any arch still lacking one.
- Full userspace server chains pass on x86_64, AArch64, RISC-V.
- No fake remote TLB/TLBI/SFENCE acknowledgment; no ReapFaultedTask split; ABI/counts unchanged.
- Docs updated to state precisely which classes are live per arch — no blanket "cross-arch
  retirement" claim.

**Current reality (194):** the IpcSend *boundary* family already runs live on AArch64 (it rides
the generic drain on the shared path — `class=IpcSendOrdinaryCap result=ok` is observed on the
AArch64 core smoke, pre-existing and not enabled by 194). AArch64's split-dispatch +
queue-advancing classes, and ALL of RISC-V's classes, remain inert / global-lock-only; both
full userspace server chains pass; no additional class is enabled by a flag flip; a real drain +
return-path proof is mandatory before 195 (AArch64 split-dispatch) and 196 (RISC-V) land.

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

**Ready to resume global kernel unlocking: yes. Current frontier: D6-SWITCH.**

Stage 163P is accepted on x86_64, AArch64, and RISC-V (§5.1.9.22). The
fork/sender-wake oracle is now a **regression gate**, not the development
frontier. The Cycle 13 review re-establishes D6-SWITCH as the blocking
gate for D2-GENUINE and D6-GENUINE.

The plumbing is in place: Stages 117–129 + the default-off `D6_SWITCH_PROOF`
harness (Stages 120–132 + 139) proved the unlocked `switch_frames` path on
x86_64 under a controlled pair. The next concrete step is **D6-SWITCH-SMOKE**
(§7.1.5 item 1): move that gate from the proof knob into the default x86_64
core smoke so that `D6_SWITCH_FRAMES_ENTER_UNLOCKED` and
`D6_SWITCH_FRAMES_RETURNED_UNLOCKED` appear without `D6_SWITCH_PROOF=1`.
Once that lands, **D6-SWITCH-A** (§7.1.5 item 2) completes the Outcome A
live-wire: wire `initialize_thread_kernel_switch_frame` into the production
boot path for the supervisor (tid=2), delete the `with_scheduler_split_mut`
helper-only fence, and confirm `D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH` in
smoke.

**Vocabulary reminder (preserved across all stages):**

- **Outcome B** — preparatory phase split, no genuine seam call; helper-only
  fence on `with_scheduler_split_mut` / `with_task_tcbs_split_mut` stays.
- **Outcome A** — genuine seam live-wire; helper-only fence deleted in the
  same PR; proof markers appear in the default core smoke.

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
- **Strict oracle marker rules (sender-wake regression oracle).**
  1. **Never count `[info] absent : MARKER` as success.** A log line
     reporting a marker absent is not evidence the marker fired. Only a
     direct, positive match of the marker string counts.
  2. **x86_64 order marker must match `^IPC_RECV_V2_SENDER_WAKE_ORDER_OK`**
     (anchored at the start of the line). A substring match anywhere in a
     line (e.g. in a diagnostic summary) is not sufficient; the kernel
     emits this marker only on the live recv-v2 split path.
  3. **Sequence-done marker must match
     `USER_LOG .*msg=IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`** — the
     `USER_LOG` prefix confirms it was emitted by userspace (init task), not
     by a kernel diagnostic or a grep of a summary line.
- **Sender-wake workload preservation rules.**
  1. **Do not replace the Stage 163P cooperative non-blocking E2 probe /
     `yield_now()` loop with a blocking deadline recv.** The cooperative
     design (`ipc_recv_with_deadline(e2_recv, 0)` + `yield_now()` between
     probes) is what allows the parent to hand the CPU to the child without
     depending on a timer interrupt. A blocking deadline recv re-introduces
     the `BLOCKED_WOULDBLOCK_FATAL` race that Stage 163P fixed. Pinned by
     the `stage163p_e2_poll_is_nonblocking` and related guards.
  2. **Do not use `enqueue_task` for fork proof child placement unless
     remote wake / IPI is proven.** Stage 163N fixed this by switching to
     `enqueue_woken_task`, which places the child on the same CPU as the
     fork-calling parent, eliminating the need for cross-CPU IPIs. Reverting
     to `enqueue_task` reintroduces the AArch64 multi-CPU deadlock where the
     child lands on a remote CPU and no IPI is sent. Pinned by
     `stage163n_fork_child_enqueued_with_woken_task`.
  3. **RISC-V same-task non-syscall traps must not mirror stale TCB GPR
     snapshots over live hardware frame.** The Stage 163P fix gates TCB
     writeback strictly on `task_switched || ecall`; the non-syscall
     (non-ecall, same-task) branch preserves the full hardware frame without
     overwriting it from the TCB snapshot. Reverting this causes the fork
     child to read a stale `a0` (from the parent's pre-fork TCB snapshot)
     instead of the kernel-written child `a0=0`. Pinned by
     `stage163p_riscv_non_syscall_branch_preserves_full_hw_frame` and
     `stage163p_riscv_writeback_gated_on_switch_or_ecall`.

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
