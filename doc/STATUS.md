<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM Current Status

> **Live state only.** This file does not narrate milestones. It says
> what is currently working on each architecture and per-service domain,
> and links the next-target details to the canonical owner doc. For
> closed-milestone history, see `doc/PROJECT_HISTORY.md`. For ownership
> and authoring rules, see `doc/DOCUMENTATION_MAP.md`.

---

## 0. Kernel-unlock frontier — current verified state

**Broad-lock census verified at commit `757993b6`, tree `1118b61b`. Live-cell evidence at
commit `f5669cb55325ac58aba6a15207a89c95ad8cad3d`, tree
`e2fd0b5c7a82dc6c8c422d5c6db242473533a9a6`.**
Full evidence: `doc/KERNEL_UNLOCK_AUDIT.md`. Canonical stage ladder and roadmap:
`doc/KERNEL_UNLOCKING.md` §0.

### Broad-lock position

| Metric | Value |
|--------|-------|
| Production `SharedKernel::with_cpu` callsites | **41** |
| Production broad `SharedKernel::with` callsites | **10** |
| **Total production broad-lock acquisition sites** | **51** |
| Ungated off-lock syscall classes | **5** on x86_64 (NR 15, 10, 8, 2-narrow, 14-narrow); **2** on AArch64 (NR 15, 10); **2** on RISC-V (NR 15, 10) |
| Proof-gated off-lock classes (default **OFF**) | NR 6 `IpcCall`, NR 7 `IpcReply` — all three architectures |
| Off-lock authoritative dispatch | **x86_64 only** (`d6_genuine_enabled()` is compile-time false elsewhere) |

### Hosted validation (re-executed, not inherited)

| Command | Result |
|---------|--------|
| `cargo test --lib -- --test-threads=1` | ✅ 3729 passed, 0 failed, 2 ignored |
| `cargo test --tests -- --test-threads=1` | ✅ 3881 passed (3729 lib + 152 integration), 0 failed |
| `cargo test --tests --features ipc-reply-timeout-oracle-core -- --test-threads=1` | ✅ 4045 passed, 0 failed |
| `cargo test --lib` (default parallel harness) | ⚠️ **completes, 0 aborts** — 58–71 logical shared-state assertion failures remain |
| all 13 repository gate scripts | ✅ **13 of 13 pass** |
| `cargo check` — x86_64 / AArch64 / RISC-V bare-metal `kernel_boot` | ✅ clean |
| `cargo check` — x86_64 / AArch64 / RISC-V freestanding `crash_test_srv` | ✅ clean |
| `cargo fmt --check`, `git diff --check` | ✅ clean |

The parallel memory corruption (three cross-test aliasing bugs) is **fixed**; see
`doc/KERNEL_TEST_RULES.md` Rule H1. What remains is process-global test contention in the
hosted corpus: **test-infrastructure debt, not canonical Stage 205C work.** It may precede
or support 205C (which is a long-running torture of the running kernel) but closes no part
of it.

### Canonical stage position

Stage definitions are owner-supplied and authoritative
(`doc/KERNEL_UNLOCKING.md` §0). **A historical stage carrying the same number does not
complete the canonical stage.**

| Phase | Complete | Partial foundation | Open |
|-------|----------|--------------------|------|
| 2 — IPC (199C–199G) | 0 of 5 | 199D, 199E | 199C, 199F, 199G |
| 3 — Capability (200A–200D) | 0 of 4 | 200A, 200C | 200B, 200D |
| 4 — VM (201A–201G) | 0 of 7 | 201B, 201F | 201A, 201C, 201D, 201E, 201G |
| 5 — Lifecycle (202A–202F) | 0 of 6 | 202D | 202A, 202B, 202C, 202E, 202F |
| 6 — Timer/IRQ/sched (203A–203D) | 0 of 4 | 203A, 203C, 203D | 203B |
| 7 — Monolith removal (204A–204E) | **1 of 5** (204A) | 204B | 204C, 204D, 204E |
| 8 — Seal (205A–205D) | 0 of 4 | 205A | 205B, 205C, 205D |
| **Total** | **1 of 35** | 12 | 22 |

**No canonical stage in Phases 2–6 or 8 is complete.** The one complete stage, 204A
(broad-lock callsite census), is documentation rather than lock retirement: 51 callsites
classified as 0 boot-only, 3 test-only, 2 obsolete, 46 runtime-required, 0 undocumented.

> **Arithmetic correction.** An earlier revision reported *1 of 34* with 11 partials. Phase 7
> was the only row written without an `N of M` denominator, and the totals silently counted it
> as four stages. **The dropped stage was `204B` (decompose `KernelState` ownership), the sole
> Phase-7 partial**, which is why both the total (34 → **35**) and the partial count
> (11 → **12**) were low by exactly one. All 35 stages were, and remain, individually
> documented and classified; only the summary arithmetic was wrong. `204B` is classified
> **partial foundation**: the eleven ranked domain locks and the `*_split_mut` / `*_split_read`
> seam set already exist, but `with_cpu` still forms a broad `&mut KernelState`, so the
> container still serializes the kernel.


The historical stages labelled 200A/200B/200C (terminal ownership, deadline token,
reply-timeout transaction) are IPC timeout work belonging to canonical **199E**. They
contribute nothing to canonical 200A–200C, which are the **capability** stages and have
essentially no production wiring — every capability seam is `M2_SEAM_HELPER_ONLY`.

### Live cells earned

| Programme | Cells | Seal / canonical stage served |
|-----------|-------|-------------------------------|
| Stage 198F combined retirement (first cohort 12 + supported `IpcSend` 18) | **30** | `STAGE_198F_COMPLETE_RETIREMENT_SEAL … total_live_cells=30 result=ok`; pre-199C groundwork, 199C delivery, 200C shared-region |
| Reply-timeout matrix | **6** | `STAGE_200_IPC_REPLY_TIMEOUT_MATRIX_SEAL`, commit `72a4ebf`; 199E (one quarter of the stage) |
| `ExitCurrentTask` NR 16 | **2 of 3** | x86_64 `0b5e98f`, AArch64; 202D (one sub-path; RISC-V unearned) |
| **Server death (`ServerDies`) — x86_64** | **1 of 3** | **`STAGE_200D2B1C_X86_64_SERVER_DIES_SEAL`, commit `f5669cb5`; canonical 199D server-crash-cleanup increment** |
| **Accepted total (production-path)** | **39** | 30 + 6 + 2 + 1 |
| Direct IPC NR 6 / NR 7 (x86_64, SMP=2) | 6, **knob-gated** | `STAGE_199_X86_DIRECT_IPC_FINAL_SEAL … result=ok`; proves the 199D mechanism, **not** the production path |

> **On the total.** There is no aggregate live-cell counter anywhere in the tree; the only
> in-tree aggregate is Stage 198F's `total_live_cells=30`. The figure above is computed from
> the seals listed and counts only **production-path** cells. Including the six knob-gated
> Stage 199 functional cells gives **45**. A previously-quoted figure of **43** matches
> neither: it requires counting the six knob-gated Stage 199 cells *and* excluding the two
> `ExitCurrentTask` cells. Recorded here as **39** with the arithmetic visible rather than
> asserting an unverifiable total — if the intended policy is to count knob-gated cells, the
> number is 45 and this row should say so.

### x86_64 ServerDies cell — evidence

Exact commit `f5669cb55325ac58aba6a15207a89c95ad8cad3d`, tree
`e2fd0b5c7a82dc6c8c422d5c6db242473533a9a6`. One fresh boot, 14215 lines, **zero
`result=fail`**, all eighteen forbidden markers absent, one boot banner.

* Scoped vector `[1, 1, 1, 1, 1, 1, 1, 1, 1]`, `result_before_enqueue=1`.
* Quiescent system balance `created=54 closed=54 live_links=0` — the same 54 that was
  previously reported as a leak.
* `EXIT_TASK_OWNER_REVALIDATED … prepared=idle committed=replacement next_tid=1 advances=1`
  — `revalidate_idle_owner_after_drains` executed in QEMU for the first time — and
  `EXIT_TASK_COMMON_EPILOGUE_OWNER … owner=replacement frame_committed=1`.
* `TERMINAL_CLAIM terminal=PeerDeath result=won` → `USER_VALIDATED result=ServerDied code=10`;
  survivor and health attested.

Full detail: `doc/IPC.md` §8.5.

### Immediate blockers

1. **AArch64 and RISC-V ServerDies live cells are unearned** — 1 of 3. The x86_64 cell is
   earned (`STAGE_200D2B1C_X86_64_SERVER_DIES_SEAL`, `f5669cb5`), which also **cleared the
   two blockers that used to head this list**: the `IPC_SERVER_DEATH_LINK_LEAK` accounting
   failure is resolved, and `revalidate_idle_owner_after_drains` has now executed in QEMU
   (`EXIT_TASK_OWNER_REVALIDATED … committed=replacement`).
2. **NR 6 / NR 7 off-lock direct IPC cannot be made production-default yet — two remaining
   correctness defects in the transaction body, not the gates.** The acknowledgement-store
   prerequisite *is* met (the bounded endpoint-indexed multi-pair store,
   `src/kernel/direct_ack_store.rs`, Stage 199D), **delivery conformance is now met**
   (defect A: the NR6 delivery projects through the one canonical receiver-visible
   projection in `src/kernel/syscall/ipc_recv_core.rs`, byte-identical to the legacy
   blocked-waiter delivery and proved by `stage199d_delivery_projection_differential` plus a
   re-earned live round trip), and the two enablement gates are mechanically easy to remove.
   **error disposition is now met** (defect B: one pure exhaustive mapping per direction in
   `src/kernel/direct_disposition.rs` — `Completed` / `DeclinedBeforeMutation` /
   `Failed(SyscallError)`, no wildcard arm, neither drain's `Result` discarded, with
   fault-injection and empirical legacy error-code parity for both copy faults), and
   **return-lane parity is now met** (defect C: the successful direct NR6/NR7 frame writes
   `ret2 = SYSCALL_NO_TRANSFER_CAP` through the shared encoder, byte-for-byte equal to a
   successful legacy `IpcCall` frame, attested live — `ret2=18446744073709551615 ret2_ok=1`).
   **No correctness defect remains**, and **mode eligibility + production counters have
   landed**: `src/kernel/direct_eligibility.rs` (pure exhaustive contract — `SEND` rights,
   current endpoint incarnation, `Buffered` only, `Synchronous` declines before mutation to
   the legacy rendezvous path; NR7 needs no mode) and `src/kernel/direct_ipc_counters.rs`
   (per-direction terminal buckets, ack lifecycle, occupancy high-watermark and every
   fail-closed fuse, balance proved live). What is left is the enablement step itself: remove
   the proof gate and the oracle endpoint confinement, then prove the flip live. The counters
   make that auditable — the current oracle boot shows 54 NR6 / 53 NR7 ordinary service-chain
   calls turned away by the confinement, exactly the population a flip would move. Until then
   both gates stay in place and no production default has changed. Full evidence is in
   `doc/KERNEL_UNLOCK_AUDIT.md` §6.1; see also `doc/IPC.md` §8.6.
3. **`d6_genuine_enabled()` is compile-time x86_64-only** — 203C blocked; AArch64 and
   RISC-V cannot retire any queue-advancing class.
4. **Every capability seam is `M2_SEAM_HELPER_ONLY`** — all of Phase 3 has zero production
   wiring.
5. **`FutexWait` off-lock seams landed helper-only** and were never wired.
6. **Reply-timeout scan is off-lock on x86_64 only**; `IpcSend` and `IpcCall` timeouts are
   untouched; the broad fallback `run_reply_timeout_completion` survives (199E).
7. **RISC-V `ExitCurrentTask` live cell** — kernel chain proven correct, runner bound
   corrected at `5488d8e`, re-run never executed (202D).
8. **Parallel `cargo test --lib` produces 58–71 shared-state assertion failures** — keeps
   every hosted claim single-threaded-only. Test-infrastructure debt; a prerequisite for
   using the hosted suite as a 205C harness, not 205C completion work.
9. **AArch64 re-acquires the broad lock on its split return path**
   (`src/arch/trap_entry.rs:1432`) — 204B/204E must localize it. 205A reports the cell; it
   is not where it gets retired.

---

## 1. Per-architecture status

### 1.1 AArch64 (QEMU virt — primary)

| Item | Status |
|------|--------|
| Core service-chain spawns | ✅ initramfs_srv / devfs_srv / vfs_server / driver_manager / blkcache_srv / virtio_blk_srv (tids 10000–10005) |
| Strict core-smoke gate | ✅ ordered progression: `_start` → `prepare_arch_boot` → `vbar_el1_ready` → `mmu_enabled` → `run_with_prepared_kernel` → `YARM_BOOT_OK` → `YARM_INIT_START`/`_DONE` |
| Timer / scheduler tick | ✅ `YARM_TIMER_IRQ_DELIVERED` / `YARM_TIMER_EOI_DONE` / `YARM_SCHED_TICK` |
| Optional FS strict smoke | ✅ RAMFS + ext4 live (`RAMFS_MOUNT_READY`, `EXT4_SRV_READY`, `VFS_MOUNT_REGISTER_*_OK`); FAT skipped (`server_disabled`) |
| Steady-state | Expected quiescent idle: `init_server` blocks on `init_alert_recv_ep` after `INIT_ALERT_WAIT_BEGIN`; `process_manager` blocks for more requests |
| SMP / PSCI | Deferred (post-bring-up baseline) |

See `doc/ARCH_AARCH64.md` for the per-PR boot history, IPC contract, PM
exec-load policy, and capability-materialization rules.

### 1.2 x86_64 (PVH — primary; `-smp 1` baseline)

| Item | Status |
|------|--------|
| Core-smoke gate (`QEMU_SMP=1`) | ✅ all 6 service entries exactly once; boot markers detected |
| Optional FS strict | ✅ RAMFS + ext4 live; FAT skipped |
| AP Rust online (`yarm.x86_ap_rust=1`) | ✅ Stage 109 outcome A — AP enters Rust and parks |
| Production scheduler | BSP only; `online_cpu_count()` stays at 1; AP `started_secondary` reported separately |
| Off-lock authoritative dispatch (D6-genuine) | ✅ **production, no opt-out** — `d6_genuine_enabled()` is compile-time true on x86_64 unless a D6 switch diagnostic owns the switch path; the D2-send / D2-recv / FutexWait / Yield / D6 drains all run with the broad guard dropped |
| D6 switch proof harness | 🧪 default-off diagnostic only (`yarm.d6_switch_proof=1` / `D6_SWITCH_PROOF=1`); mutually exclusive with the production D6-genuine path. Historical bring-up detail (Stages 120–132) is in `doc/PROJECT_HISTORY.md` |
| AP scheduler participation | 🧪 **proof-gated live** — `X86_AP_GENERIC_RETURN_SEAL`, `X86_AP_SAVED_RETURN_SEAL`, `X86_AP_RECV_V2_BLOCK_SEAL` and the cross-CPU NR6/NR7 seals all earned live at SMP=2 under default-off knobs; the **production** scheduler is still BSP-only (`online_cpu_count()` == 1) |
| Timer interrupts on APs | ❌ not enabled on the production path |
| Restore-owner selection | ⚠️ resolved **in-lock** (identity-coherent) and revalidated after the drains by `revalidate_idle_owner_after_drains`; that revalidation is **hosted-proven only, never run in QEMU** |

See `doc/ARCH_X86_64.md` for the safety fences, AP marker sequence, BT2
LAPIC timer discipline, and the ordered next-target list before AP
scheduling can be enabled.

### 1.3 RISC-V64 (OpenSBI / QEMU virt)

| Item | Status |
|------|--------|
| OpenSBI handoff | ✅ a0 (hartid) + a1 (DTB) preserved; `mv a0, s1` fix applied |
| Secondary hart park (`--smp 2/3/4`) | ✅ live-verified; boot hart never parked; parked list is the topology bitmap minus the boot hart |
| SMP topology + nonzero boot hart | ✅ binary-FDT `/cpus` walk yields `present_cpus=N`, `present_bitmap=0x{1,3,7,f}`; nonzero OpenSBI boot hart correctly selected (commit 271ac73) |
| Monotonic cmdline capture | ✅ once-guarded; `RISCV_CMDLINE_CAPTURE_ONCE`; `RISCV_CMDLINE_PRESERVED reason=missing_dtb_after_valid` |
| DTB RAM / initrd staging | ✅ `crate::arch::fdt::memory_reg` + `chosen_initrd`; firmware / DTB / initrd reserved |
| Bootstrap | ✅ 16 MiB boot stack; `Bootstrap::init_static`; real RAM staged before allocator init |
| Early S-mode trap diagnostic | ✅ `RISCV_EARLY_TRAP` + `RISCV_BOOTSTRAP_TRAP_STEP` |
| Sv39 kernel-shared gigapage | ✅ root[2] over `[0x8000_0000, 0xC000_0000)` with `V \| R \| W \| X \| G \| A \| D`; idempotent installer |
| Page-table write-through + zero-on-alloc | ✅ MMU walks physical frames, intermediates with `U=0` (Sv39 spec compliance) |
| Real S-mode → U-mode `sret` | ✅ `RISCV_ENTER_USER_SRET tid=2`; first trap `from_u=1 spp=0` |
| Syscall round-trip | ✅ full `RiscvTrapFrame` save/restore; `+4` ecall PC advance via TCB snapshot; task-switch arg seeding; S-mode-fault fail-closed halt |
| Core service chain | ✅ initramfs / devfs / vfs / ramfs / ext4 reached; `RAMFS_MOUNT_READY`; `EXT4_SRV_READY`; `VFS_MOUNT_REGISTER_*_OK` |
| Terminal state | ✅ `RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked` (event-driven idle, no timer/IRQ scope) |
| Regular smoke target (`--smp 1/2/3/4`) | ✅ `scripts/qemu-riscv64-core-smoke.sh` + `scripts/qemu-riscv64-smoke-matrix.sh` enforce the full per-N marker contract on QEMU virt + OpenSBI |
| Ready for global kernel-unlocking smoke matrix | ✅ **Ready: yes** — see `doc/ARCH_RISCV64.md` §13.5; the regular core smoke is RISC-V's per-arch gate, treated the same way as x86_64 / AArch64 core smokes |
| Timer audit scaffold | ✅ `RISCV_TIMER_AUDIT_BEGIN` + `RISCV_TIMER_AUDIT_DONE sbi_time=… boot_hart=… trap_bridge_reentrant=… feature=…`; canonical deferred reasons pinned by the smoke gate (`timer_irq_feature_disabled`, `trap_bridge_reentrancy_not_ready`, `sbi_time_ext_unavailable`, `stie_audit_pending`, `not_boot_hart`) |
| Timer interrupt (live) | ⏸ deferred — accepted as `RISCV_TIMER_DEFERRED reason=timer_irq_feature_disabled`; next pass enables S-mode timer (`stimecmp` + `sstatus.SIE=1` + `mideleg` STI) and flips the gate to live-required |
| PLIC threshold write under active satp | ✅ skipped + reported as `RISCV_PLIC_DEFERRED reason=plic_mmio_unmapped_under_active_satp` (PLIC MMIO is outside the kernel-shared gigapage; raw write would fault) |
| External IRQ enable | ⏸ deferred — `RISCV_EXTIRQ_DEFERRED reason=no_safe_source`; UART0 (sid=10) is the marked candidate, no source enabled in this pass |
| SMP scheduler | ⏸ off — `RISCV_SCHEDULER_BSP_ONLY online_cpus=1 reason=riscv_smp_scheduler_not_enabled`; `online_cpus` stays at 1 until RISC-V SMP scheduling lands |

See `doc/ARCH_RISCV64.md` for the full marker sequence, ABI mapping, and
SMP blocker list.

### 1.4 Raspberry Pi 5 (diagnostic only — not production)

| Stage | Status |
|-------|--------|
| Stage 1 UART / DTB / MMU / allocator / read-only timer + GIC | ✅ live diagnostic |
| Stage 2A–2D | ✅ live diagnostic; EL0 entry deferred at Stage 2D (`ttbr_split_not_ready`) |
| HH-2 (TTBR split, MMU on, branch to high alias) | ✅ live diagnostic; non-default `rpi5-highhalf` feature |
| HH-3 (high-linked Rust continuation) | ✅ live diagnostic |
| HH-4 (low-identity retirement) | ✅ live diagnostic |
| HH-5 (real userspace) | ❌ DEFERRED — `RPI5_HH5_DEFERRED reason=high_half_initrd_allocator_bridge_not_ready` |

Current next blocker: build the high-half initrd / allocator bridge so
HH-5 can consume the existing Stage 2C loader without violating HH-4's
no-low-VA contract.

See `doc/RPI5_BRINGUP.md` for the full Stage 1A → HH-5 sequence and the
hardware artifact-build commands.

---

## 2. Per-service status

### 2.1 Bootstrap chain (image IDs 1–3)

| tid | service | status |
|-----|---------|--------|
| 1 | `init_server` | ✅ live; reaches steady-state event-driven idle on every arch with U-mode |
| 2 | `supervisor` | ✅ live; handoff banner emitted; control / fault / control-send caps present |
| 3 | `process_manager` | ✅ live; SpawnV5 path proven; PM-private reply RECEIVE cap in startup slot 2 |

Slots 0..17 are documented in `doc/PROCESS_AND_SPAWN.md` (slot 12
is PM-private for PM↔VFS subcalls).

### 2.2 Bootstrap FS chain (image IDs 4–6)

| tid (typical) | service | status |
|---------------|---------|--------|
| 10000 | `initramfs_srv` | ✅ live; `INITRAMFS_BACKEND_SOURCE source=cpio` populated from boot CPIO bytes |
| 10001 | `devfs_srv` | ✅ live; console / null FDs registered; `DEVFS_SRV_RESIDENT_WAIT_BEGIN` |
| 10002 | `vfs_server` | ✅ live; `VFS_MOUNT_TABLE_READY`; routes initramfs + devfs sends |

### 2.3 Optional FS / storage (image IDs 7–12)

| Image ID | Service | Status |
|----------|---------|--------|
| 7 | `driver_manager` | ✅ live; spawned via VFS-backed `STATX → OPENAT → READ* → CLOSE` after init passes a `vfs_server` request SEND cap (SpawnV5 service caps slot 0) |
| 8 | `blkcache_srv` | ✅ live |
| 9 | `virtio_blk_srv` | ✅ live |
| 10 | `fat_srv` | Profile-ready; **disabled by default** (`INIT_FAT_SPAWN_SKIPPED reason=server_disabled`); see `doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md` (FAT server section) for activation blockers |
| 11 | `ramfs_srv` | ✅ live; fully writable; mounted at `/ram` |
| 12 | `ext4_srv` | ✅ live; read-only; mounted at `/ext4` (writes report `Unsupported`) |

The optional-FS strict smoke pins these markers per arch — see
`doc/KERNEL_UNLOCKING.md` §3 ("Optional-FS smoke markers"). Do not
rename or remove them without updating both smoke scripts.

### 2.4 Networking

Service domain crate exists (`crates/yarm-network-servers`) with
contracts consolidated into `doc/NETWORKING.md` (Pass 4). Not part
of the core boot smoke.

### 2.5 UI

Service domain crate exists (`crates/yarm-ui-servers`). Not part of the
core boot smoke. Current contracts live in `doc/PHASE_GATES.md`
(Phase 4 UI contract section; gated by `scripts/check-roadmap-readiness.sh`).

---

## 3. Current crate / domain boundary

Kernel and low-level runtime own:

- scheduling and dispatch mechanisms;
- IPC / notification mechanisms;
- capability enforcement / mechanisms;
- trap / IRQ routing mechanisms;
- VM / address-space and bootstrap mechanisms.

Userspace service domains own service policy (extracted workspace crates):

| Domain | Crate path |
|--------|------------|
| Control plane | `crates/yarm-control-plane-servers` |
| Drivers | `crates/yarm-driver-servers` |
| Filesystems | `crates/yarm-fs-servers` |
| Networking | `crates/yarm-network-servers` |
| UI | `crates/yarm-ui-servers` |
| Compatibility | `crates/yarm-compat-servers` |
| Shared service helper/runtime | `crates/yarm-srv-common` |

The root `yarm` crate is no longer the monolithic service owner.
Boundary checks enforce crate-graph and source-shape constraints:

```sh
scripts/check-crate-graph-boundary.py
scripts/phase5-boundary-gates.sh
scripts/phase5-boundary-gates.sh --fs-runtime-entrypoint
scripts/phase5-boundary-gates.sh --driver-runtime-entrypoint
scripts/phase5-boundary-gates.sh --network-runtime-entrypoint
scripts/phase5-boundary-gates.sh --ui-runtime-entrypoint
```

`yarm-server-runtime` is a narrow server-runtime boundary; see
`doc/AI_AGENT_RULES.md` §16 for the export-surface contract.

---

## 4. Documentation ownership status

| Topic | Canonical owner | Status |
|-------|-----------------|--------|
| Kernel unlocking | `doc/KERNEL_UNLOCKING.md` | ✅ Pass 1 (canonical) |
| Kernel locking | `doc/KERNEL_LOCKING.md` | ✅ (existing canonical) |
| Boot | `doc/BOOT.md` | ✅ Pass 2 (canonical) |
| Arch — AArch64 | `doc/ARCH_AARCH64.md` | ✅ Pass 2 (canonical) |
| Arch — x86_64 | `doc/ARCH_X86_64.md` | ✅ Pass 2 (canonical) |
| Arch — RISC-V64 | `doc/ARCH_RISCV64.md` | ✅ Pass 2 (canonical) |
| RPi5 | `doc/RPI5_BRINGUP.md` | ✅ Pass 2 (canonical) |
| Project history | `doc/PROJECT_HISTORY.md` | ✅ Pass 3 (this pass) |
| Current status | `doc/STATUS.md` | ✅ Pass 3 (this file) |
| IPC | `doc/IPC.md` | ✅ Pass 4 (canonical) |
| VFS | `doc/VFS.md` | ✅ Pass 4 (canonical) |
| Filesystem / storage | `doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md` | ✅ Pass 4 (canonical) |
| Networking | `doc/NETWORKING.md` | ✅ Pass 4 (canonical) |
| Capabilities | `doc/CAPABILITY_MODEL.md` | ✅ Pass 4 (canonical) |
| Process / spawn | `doc/PROCESS_AND_SPAWN.md` | ✅ Pass 4 (canonical) |
| Phase gates (Phase 2/3/4 contracts, roadmap, kernel-status milestones) | `doc/PHASE_GATES.md` | ✅ Pass 4 (canonical) |
| Service manifest | `doc/SERVICE_MANIFEST.md` | ✅ (existing canonical) |
| Kernel-unlock audit (census / matrix / stage table / roadmap) | `doc/KERNEL_UNLOCK_AUDIT.md` | ✅ Pass 6 (canonical) |
| Roadmap (current direction) | `doc/KERNEL_UNLOCKING.md` §0 | ✅ Pass 6 — the former `doc/ROADMAP.md` never existed in this tree; the kernel-unlock roadmap is canonical |
| Agent rules (capability/spawn/zero-copy/smoke + source-licensing header §15 + server-runtime boundary §16) | `doc/AI_AGENT_RULES.md` | ✅ Pass 5 (canonical; absorbed `AGENTS.md` body 2026-06-16) |
| libc / Linux / musl POSIX compatibility | `doc/LIBC_AND_LINUX_COMPAT.md` | ✅ Pass 5 (canonical; merged `LIBC_ABI_X86_64_NONE.md` + `LINUX_COMPAT.md` + `MUSL_POSIX_IPC_MAPPING.md` 2026-06-16) |
| Global unlocking readiness audit | `doc/KERNEL_UNLOCKING.md` §7.1 | ✅ Pass 5 (single source of truth) |
| Kernel test rules | `doc/KERNEL_TEST_RULES.md` | ✅ (existing canonical) |
| Agent-facing entry point (external-tool convention `AGENTS.md`) | `doc/AGENTS.md` | ✅ Pass 5 (short pointer to `doc/AI_AGENT_RULES.md`) |

---

## 5. Current top next steps

The four highest-impact items, in order of unlock value:

1. **RISC-V S-mode timer interrupt (live path) + smoke-gate tightening.**
   The regular RISC-V core smoke now passes live across `--smp 1/2/3/4`
   on the deferred branch (timer / PLIC / external IRQ all reported with
   explicit `reason=` markers). Next, enable `stimecmp` via the SBI Timer
   extension, set `sstatus.SIE=1`, delegate `STI` in `mideleg`, then
   flip the smoke gate's `RISCV_TIMER_SMOKE_OK|RISCV_TIMER_DEFERRED`
   accept-regex from "either" to "live required". PLIC + external-IRQ
   follow the same flip; once both land, queue RISC-V into the global
   kernel-unlocking smoke policy and unblock RISC-V SMP scheduling so
   `online_cpus` can climb past 1. See `doc/ARCH_RISCV64.md` §10–11.

2. **Kernel unlocking — canonical Stage 199D.**
   The broad `SpinLock<KernelState>` still has **51** production acquisition sites (§0).
   The ServerDies reverse-link accounting failure that used to head this list is
   **resolved** (`doc/IPC.md` §8.5): the transition counters now describe exactly one armed
   ServerDies transaction and the leak invariant moved to system-wide link totals, so there
   is no hard `result=fail` left in the tree. Two follow-ons, of different kinds: the
   **x86_64 ServerDies live cell** (a runner act — one clean boot earns the first cell and
   finally exercises `revalidate_idle_owner_after_drains`, which has never run in QEMU), and
   the **smallest production change**, flipping `ipccall_direct_proof_enabled()` to the
   production default on x86_64 so the landed off-lock NR 6 / NR 7 transaction is actually
   taken by a normal boot. Neither completes 199D. See `doc/KERNEL_UNLOCKING.md` §0 and
   `doc/KERNEL_UNLOCK_AUDIT.md` §6.

3. **RPi5 HH-5 — high-half initrd / allocator bridge.** Build the bridge
   so HH-5 can consume the existing Stage 2C loader without violating
   HH-4's no-low-VA contract; then enter EL0 via the real ERET path. See
   `doc/RPI5_BRINGUP.md` §12–13.

4. **Documentation consolidation Pass 4 — completed 2026-06-15.** Six
   ABI-sensitive clusters (IPC, VFS, FS/storage, networking, capabilities,
   process/spawn) and the six CI-gated phase docs were consolidated into
   seven canonical owners (`doc/IPC.md`, `doc/VFS.md`,
   `doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md`, `doc/NETWORKING.md`,
   `doc/CAPABILITY_MODEL.md`, `doc/PROCESS_AND_SPAWN.md`,
   `doc/PHASE_GATES.md`). CI gate scripts were updated atomically. See
   `doc/DOCUMENTATION_MAP.md`.

---

## 6. Frozen boundaries (one-line reminders)

The full invariant list lives in `doc/KERNEL_UNLOCKING.md` §3. Headlines:

- SpawnV5 ABI (16-byte reply, argument layout) — frozen.
- Image IDs 7–12 — frozen.
- `SYSCALL_COUNT = 32` (dispatch-table size); public ABI surface is `0..=16`
  after `ExitCurrentTask` (NR 16) landed — see `doc/SYSCALL_ABI.md`.
- `STARTUP_SLOT_COUNT = 18`.
- `RecvSharedV3` ABI offsets — frozen.
- Optional-FS smoke markers (`INIT_RAMFS_SPAWN_OK`, `RAMFS_MOUNT_READY`,
  `VFS_MOUNT_REGISTER_RAMFS_OK`, `INIT_EXT4_SPAWN_OK`, `EXT4_SRV_ENTRY`,
  `EXT4_SRV_READY`, `VFS_MOUNT_REGISTER_EXT4_OK`,
  `INIT_FAT_SPAWN_SKIPPED reason=server_disabled`) — do not rename or
  remove.
- No `ipc_recv_with_deadline(_, 0)` in required-reply paths.
- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false`.
- VM / TLB two-phase ordering (PTE removal → TLB shootdown → reclaim).
- Boundary gates (`phase5-boundary-gates`) remain green.
- No service-policy logic in the kernel; no reintroduction of
  `src/services/*`.

---

## 7. Authoring rule

Do **not** turn this file into a milestone diary. Append a row to
`doc/PROJECT_HISTORY.md` for a closed milestone; update the rows above
to reflect the new live state; link the next-target details to the
canonical owner doc. New status / next-context / audit / PR-plan
fragment files are forbidden — see `doc/DOCUMENTATION_MAP.md`.
