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

## 1. Live status (Milestone 1 declared, Milestone 2 Pass 2, Stage 110 sentinel cleanup)

| Item | Status | Live since | Notes |
|------|--------|-----------|-------|
| **D1** transfer-cap recv (non-reply, non-shared-region) | **LIVE** | Stage 104 | router → `materialize_split_transfer_cap_equivalent`; telemetry `d1_split_materializations` |
| **D2** endpoint blocking-recv waiter publish | **LIVE** | Stage 106 | `publish_recv_waiter_live`; telemetry `d2_recv_waiter_publishes`, `d2_publish_race_unwinds` |
| **D3.1** `vm_brk_shrink_two_phase` (`D3_LIVE_SPLIT`) | **LIVE** | Stage 107 | first VM/memory split wire; rest of D3 deferred (see §6) |
| **D4** `syscall/{debug,initramfs}.rs` | **PARTIAL** | Stage 102 | rest of `syscall/dispatch.rs`, `syscall/ipc.rs`, `syscall/ipc_recv_core.rs`, `syscall/mm.rs`, `syscall/cap.rs`, `syscall/sched.rs`, `syscall/process.rs`, `syscall/recv_shared_v3.rs` pending (§7) |
| **D5** reply-cap recv (non-shared-region) | **LIVE** | Stage 105 | fallible record-set + mint rollback on stale; telemetry `d5_split_reply_materializations`, `d5_split_reply_rollbacks` |
| **D6.1** `local_dispatch_step_split` (`D6_LIVE_SPLIT`) | **LIVE** | Stage 107 | scheduler-seam first wire; per-CPU lock sharding deferred (§9) |
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

`block_current_on_receive_with_deadline`: scheduler block (rank 1) → TCB
Blocked + deadline staging (rank 2) → **atomic queue-recheck + publish**
(rank 3, `publish_recv_waiter_live`) → dispatch. `QueueNonEmpty` outcome
drives the no-lost-wakeup unwind (`wake_tid_to_runnable` + return so the
caller's Phase-2 dequeue drains the raced message). The notification-recv
blocking path and all sender-side blocking remain canonical.

### D3 (VmAnonMap / VmBrk two-phase)

- **Phase 2 shootdown precedes Phase 3 reclaim** inside
  `execute_tlb_shootdown_wait_plan` (structural, UAF-load-bearing).
- **D3.1 live wire (Stage 107):** `vm_brk_shrink_two_phase` runs the brk
  shrink via the rank-5 vm seam + rank-6 memory seam.
- Remaining D3 (`VmAnonMap` live) is **gated**: requires lock-free
  `await_tlb_shootdown_ack` for multi-CPU + x86_64 SMP smoke approval.

### D6 (scheduler)

- **D6.1 live wire (Stage 107):** `local_dispatch_step_split` routes the
  local-CPU dispatch step through the typed helper for telemetry and future
  SharedKernel-seam wrapping.
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

### 6.6 Stage 108 split-mut seams (helper-only)

`with_scheduler_split_mut` (rank 1), `with_task_tcbs_split_mut` (rank 2),
`with_vm_user_spaces_split_mut` (rank 5), `with_memory_split_mut` (rank 6)
— labels `M2_SEAM_HELPER_ONLY` + `FALLBACK_GLOBAL_LOCK`. Live-wiring any of
them requires its own PR + MUST_SMOKE run + deletion of the helper-only
fence in the same PR.

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

3. **D-NEXT-1 PR-A — D2 publish → task/scheduler seams.** Route
   `publish_recv_waiter_live` through `with_task_tcbs_split_mut` /
   `with_scheduler_split_mut` (§6.6), deleting the helper-only fence for
   those two seams in the same PR. Smoke-gated.
4. **D-NEXT-1 PR-B — D3 shrink → vm/memory seams.** Route
   `vm_brk_shrink_two_phase` through `with_vm_user_spaces_split_mut` /
   `with_memory_split_mut`, deleting the helper-only fence for those two
   seams in the same PR. Smoke-gated.
5. **D-NEXT-1 PR-C — D6 dispatch → scheduler seam.** Route
   `local_dispatch_step_split` through `with_scheduler_split_mut`,
   deleting its helper-only fence in the same PR. Smoke-gated.
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
| D2 (endpoint blocking-recv waiter publish) | **live** | `publish_recv_waiter_live`; telemetry `d2_recv_waiter_publishes` / `d2_publish_race_unwinds` (must be 0). Stage 106. |
| D3.1 (`vm_brk_shrink_two_phase`) | **live** | `D3_LIVE_SPLIT`. Stage 107. |
| D3 rest (full `VmAnonMap` two-phase live) | **deferred** | plan types are consumed inside the still-global-locked `handle_vm_anon_map`; gated on lock-free `await_tlb_shootdown_ack`. |
| D4 (`syscall.rs` decomposition) | **partial** | `syscall/{debug,initramfs}.rs` landed; `syscall/recv_shared_v3.rs` is the next split target; the rest of §5.1 is pending mechanical moves. |
| D5 (reply-cap recv, non-shared-region) | **live** | fallible record-set + mint rollback on stale; telemetry `d5_split_reply_materializations` / `d5_split_reply_rollbacks`. Stage 105. |
| D6.1 (`local_dispatch_step_split`) | **live** | `D6_LIVE_SPLIT`; per-CPU lock sharding deferred until x86_64 AP scheduler-online. Stage 107. |
| D7 (MUST_SMOKE policy) | **enforced** | see `AI_AGENT_RULES.md` §13. Stage 101. |
| Stage 108 split-mut seams (rank 1/2/5/6) | **helper-only** | `M2_SEAM_HELPER_ONLY` + `FALLBACK_GLOBAL_LOCK`. Live-wiring any requires its own PR + MUST_SMOKE + helper-fence deletion in the same PR. |
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
landed in Stage 110 (§1) and are no longer pending. The next targets, in
order:

1. **D-NEXT-1 PR-A/B/C — route Stage 106/107 typed helpers through Stage
   108 seams**, one PR per helper, each smoke-gated: D2 publish →
   task/scheduler seams; D3 shrink → vm/memory seams; D6 dispatch →
   scheduler seam. Each PR deletes its `M2_SEAM_HELPER_ONLY` /
   `FALLBACK_GLOBAL_LOCK` fence atomically.
2. **D4 step 1 — `syscall/recv_shared_v3.rs` extraction**, then
   `syscall/process.rs`, then the remaining modules listed in §5.1.
3. **D-NEXT-2 — x86_64 AP per-CPU environment → scheduler-online.**
   Per-CPU GDT/IDT/TSS + GS base + AP-safe printk + `bring_up_cpu(cpu)`,
   behind a default-off knob; then `-smp ≥ 2` smoke acceptance. Still
   high priority — it unblocks per-CPU runqueue lock sharding (D6) and
   the lock-free `await_tlb_shootdown_ack` design (D3) — but does not
   bypass items 1–2 above.

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

D7-A and D7-B (§1 Stage 110, §7 items 1–2) are complete: the stale
`NOT SMOKE-ACCEPTED` sentinels are gone and `D2_PUBLISH_RACE_UNWIND` is
now a hard smoke-script reject on every architecture. All required
per-arch acceptance smokes pass: `qemu-x86_64-core-smoke.sh`,
`qemu-aarch64-core-smoke.sh`, `qemu-riscv64-smoke-matrix.sh`
(`--smp 1/2/3/4`), plus the strict optional-FS smokes on x86_64 and
AArch64. The next unlocking pass is the seam-routing work (§7.1.5 item 1)
— **not** the x86_64 AP per-CPU environment, which stays gated/concurrent
(§7 item 7) until the seam PRs and D4 step 1 land.

**Exact next Claude prompt recommendation:**

> Kernel unlocking Pass: D-NEXT-1 PR-A — route the Stage 106 D2 publish
> helper (`publish_recv_waiter_live`) through the Stage 108
> `with_task_tcbs_split_mut` / `with_scheduler_split_mut` seams (§6.6),
> deleting the `M2_SEAM_HELPER_ONLY` fence for those two seams in the same
> PR. Hard rules: do not touch D1/D5 canonical fallbacks; do not start
> D-NEXT-1 PR-B (D3) or PR-C (D6) in this PR; do not start the x86_64 AP
> per-CPU environment (D-NEXT-2) in this PR; preserve all required
> per-arch smokes (x86_64 core + optional-FS strict, AArch64 core +
> optional-FS strict, RISC-V64 matrix) including the `D2_PUBLISH_RACE_UNWIND`
> reject added in Stage 110; do not rely on Debug-level markers; do not
> leave a new `NOT SMOKE-ACCEPTED` sentinel behind without actually
> running the required smokes first. Deliverables: implementation,
> MUST_SMOKE run, deletion of the two seams' helper-only fence, audit
> note in `doc/KERNEL_UNLOCKING.md` §1/§7/§7.1.

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
- **D3/D6 fences.** No `with_vm_split_mut` / `with_memory_split_mut` calls
  may be added without the lock-free `await_tlb_shootdown_ack` design and
  multi-CPU smoke. No per-CPU scheduler lock types until the x86_64 SMP
  trampoline split has landed and D2/D3 are smoke-stable.
  `entering_tid` / `exiting_tid` remain Class F (authoritative read only).
- **Stage 108 seam rule.** The four split-mut seams are `M2_SEAM_HELPER_ONLY`.
  Live-wiring requires its own PR + MUST_SMOKE run + deletion of the
  helper-only fence in the same PR.
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
