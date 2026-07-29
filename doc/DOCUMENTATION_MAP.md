<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM Documentation Ownership Map

This file lists the canonical owner of each documentation topic. **New
fragmented milestone / context / audit / status / PR-plan files are
forbidden.** Future docs must update the canonical owner file, not create new
fragments. The reviewer for any PR that touches `doc/` should reject new
fragment files unless the canonical owner explicitly does not exist.

## Canonical owners

| Topic | Canonical doc(s) |
|-------|------------------|
| Kernel unlocking (canonical stages 199C–205D, roadmap, status) | **`doc/KERNEL_UNLOCKING.md`** §0 |
| Kernel-unlock audit (broad-lock census, per-arch syscall/path matrix, stage evidence, blockers) | **`doc/KERNEL_UNLOCK_AUDIT.md`** |
| Kernel locking architecture (lock-rank design, domains, invariants) + current broad-lock census | `doc/KERNEL_LOCKING.md` (§0 census) |
| IPC reply caps, shared regions, direct IPC, reply timeout, server death | `doc/IPC.md` §8 |
| Accepted global-lock retirement seals | `doc/PROJECT_HISTORY.md` |
| Supervisor runtime state / audit / PM-restart contracts | `doc/supervisor-runtime-state.md`, `doc/supervisor-audit.md`, `doc/supervisor-pm-restart-contract.md`, `doc/process-manager-restart-contract.md` |
| Driver layering / driver-manager PM spawn contract | `doc/driver-layering-audit.md`, `doc/driver-manager-pm-spawn-contract.md` |
| Boot (boot flow, command line, memory layout, QEMU runbook) | **`doc/BOOT.md`** |
| Architecture — AArch64 | **`doc/ARCH_AARCH64.md`** |
| Architecture — x86_64 | **`doc/ARCH_X86_64.md`** |
| Architecture — RISC-V64 | **`doc/ARCH_RISCV64.md`** |
| RPi5 bring-up | **`doc/RPI5_BRINGUP.md`** |
| IPC (send/recv, shared-memory fastpath, fragmentation, throughput) | **`doc/IPC.md`** |
| VFS (request loop, shared-I/O contract, mapper requirements, Proc/VFS codec freeze) | **`doc/VFS.md`** |
| Filesystem and storage (RAMFS/initramfs/devfs/FAT/ext4 + block + smoke tokens) | **`doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md`** |
| Networking (netmgr/DHCP/DNS/TCPIP/socket/virtio-net) | **`doc/NETWORKING.md`** |
| Capabilities (rights, domains, cspace access, lock-rank/two-phase/transfer rules) | **`doc/CAPABILITY_MODEL.md`** |
| Process / spawn (PM contract, TID allocation, init server boot, startup slots, control plane) | **`doc/PROCESS_AND_SPAWN.md`** |
| Phase gates (Phase 2/3/4 contracts, server roadmap, kernel-status milestones, phase readiness matrix) | **`doc/PHASE_GATES.md`** |
| Service manifest format | `doc/SERVICE_MANIFEST.md` |
| Project history (closed phases / milestones / checklists) | **`doc/PROJECT_HISTORY.md`** |
| Roadmap (current direction) | `doc/ROADMAP.md` |
| Project status / maturity | **`doc/STATUS.md`** |
| Agent rules (capability/spawn/zero-copy/smoke policy + source-licensing header + server-runtime boundary) | **`doc/AI_AGENT_RULES.md`** |
| Agent-facing entry point (short pointer for tools that look for `AGENTS.md` by convention) | `doc/AGENTS.md` (points at `doc/AI_AGENT_RULES.md`) |
| Kernel test rules (per-rule unit-test guard rails) | `doc/KERNEL_TEST_RULES.md` |
| Driver-server delegation ABI (`DRIVER_OP_*`) | `doc/DRIVER_PROTOCOL.md` |
| HAL conformance + per-ISA platform-layout audit | `doc/HAL_CONFORMANCE.md` |
| Kernel global allocator (slab + large-page) | `doc/KERNEL_GLOBAL_ALLOCATOR.md` |
| Kernel multithreading design (TCB, futex, `spawn_user_thread`, TLS) | `doc/KERNEL_MULTITHREADING_DESIGN.md` |
| Kernel scaling profile (fixed-array capacities, `hosted-dev` vs non-hosted) | `doc/KERNEL_SCALING_PROFILE.md` |
| Signal policy (non-goal stance + revisit prerequisites) | `doc/SIGNAL_POLICY.md` |
| TLB invalidation policy (per-arch + hosted vs production) | `doc/TLB_INVALIDATION_POLICY.md` |
| libc / Linux / musl POSIX compatibility (ABI freeze, dispatcher table, mapping matrix) | **`doc/LIBC_AND_LINUX_COMPAT.md`** |
| Global unlocking readiness audit | `doc/KERNEL_UNLOCKING.md` §7.1 (single source of truth — do not restate elsewhere) |

## Authoring rule

Future kernel-unlocking / boot / IPC / VFS / FS / networking / capability /
process documentation MUST update the canonical owner file from the table
above. Do not create new milestone / status / next-context / audit / PR-plan
fragment files.

If a topic has no canonical owner yet, add a new top-level doc and register
it here in the same PR. The owner file name should reflect the topic, not a
stage number.

If a fragment must be created (rare, e.g. a temporary working note that will
be deleted before merge), it must:

1. Live under `doc/.work/` (which should be gitignored or pruned at merge),
   **not** in `doc/`.
2. Carry an explicit "delete-by" stage and PR number at the top.

## Consolidation Pass 6 (kernel-unlock audit + de-fragmentation)

Pass 6 deleted **39** per-stage report files (`doc/STAGE_*.md`) after migrating every
unique contract, accepted seal, known defect, commit evidence and unresolved item into a
retained canonical document. Git history holds the full narrative; the active
documentation describes the current system and the roadmap.

### Deleted-fragment inventory and destination mapping

| Deleted fragment | Unique content preserved in |
|------------------|------------------------------|
| `STAGE_198B1_REPORT.md` | `doc/IPC.md` §8.6 (ordinary-cap copy/delegation semantics; retirement-seal isolation model); `doc/PROJECT_HISTORY.md` (build-integrity seal) |
| `STAGE_198C_REPLY_CAP_AUDIT.md` | `doc/IPC.md` §8.1 (reply-cap semantics, one-shot, aliases); `doc/PROJECT_HISTORY.md` (reply-cap direct negative seal) |
| `STAGE_198D1_QUEUED_REPLY_CAP_AUDIT.md` | `doc/IPC.md` §8.1 (queued envelope redesign; queued reply-cap enqueue unsupported) |
| `STAGE_198E1_SHARED_REGION_AUDIT.md` | `doc/IPC.md` §8.2; `doc/PROJECT_HISTORY.md` (hosted audit seal) |
| `STAGE_198E2A_SHARED_REGION_DIRECT.md` | `doc/IPC.md` §8.2 (transaction states, rollback order); `doc/PROJECT_HISTORY.md` |
| `STAGE_198E2A1_SHARED_REGION_TXN_RACE.md` | `doc/IPC.md` §8.2 (protocol A executor-owned cleanup, generation-bearing teardown); `doc/PROJECT_HISTORY.md` |
| `STAGE_198E2B_SHARED_REGION_ENQUEUE.md` | `doc/IPC.md` §8.2 (enqueue class hosted-only, zero live cells); `doc/PROJECT_HISTORY.md` |
| `STAGE_198E3_SHARED_REGION_LIVE.md` | `doc/IPC.md` §8.2; `doc/PROJECT_HISTORY.md` |
| `STAGE_198E3C1_SHARED_REGION_USERSPACE_CONTRACT.md` | `doc/IPC.md` §8.2 — **verbatim**: large-transfer `IpcSend` ABI table, `ENCODED_LEN` = 16 bytes, `Message::MAX_PAYLOAD` = 128 selector. `include_str!` pin repointed to `doc/IPC.md` |
| `STAGE_199A1_IPCCALL_DIRECT_AUDIT.md` | `doc/IPC.md` §8.3. `include_str!` pin repointed to `doc/IPC.md` |
| `STAGE_199A2A_OFFLOCK_INCARNATION.md` | `doc/IPC.md` §8.3 — **verbatim**: achieved incarnation seal and the `result=deferred` off-lock seal with its full reason. `include_str!` pin repointed to `doc/IPC.md` |
| `STAGE_199A2B1_OFFLOCK_FOUNDATIONS.md`, `STAGE_199A2B2_REQUEST_SUBSTRATE.md`, `STAGE_199A2B2C_OFFLOCK_SEAMS.md` | `doc/IPC.md` §8.3 (reserve → commit → cancel transaction shape); `doc/KERNEL_UNLOCKING.md` §0 (canonical 199C) |
| `STAGE_199A2D1_DIRECT_IPC_RACE_MODEL.md` | `doc/IPC.md` §8.3 (race outcomes) and §8.6 (reply-delivery ordering, single-slot ack boundary, overwrite fuse, multi-pair prerequisite) |
| `STAGE_199A2D2A_SMP_REQUEST.md`, `…2B_AP_DISPATCH.md`, `…2C1_AP_GENERIC_RETURN.md`, `…2C2A_AP_SAVED_RETURN.md`, `…2C2B_CROSS_CPU_NR6.md`, `…2C2B1_RECV_V2_SERVER_BLOCK.md`, `…2C2B2_CROSS_CPU_REQUEST.md`, `…2C2B3_AP_USER_CONSUME.md`, `…2C2C_CROSS_CPU_REPLY.md`, `…2C2_RECV_V2_CONTINUATION.md` | `doc/PROJECT_HISTORY.md` (earned AP / cross-CPU seals, and the superseded `result=blocked` refusals); `doc/ARCH_X86_64.md` §6.1 (live-proof status) |
| `STAGE_199A2D3_X86_DIRECT_IPC_FREEZE.md` | `doc/PROJECT_HISTORY.md` (`STAGE_199_X86_DIRECT_IPC_FINAL_SEAL` verbatim); `doc/ARCH_X86_64.md` §6.1 |
| `STAGE_200A_REPLY_TERMINAL_OWNERSHIP.md` | `doc/IPC.md` §8.4 (terminal ownership); `doc/PROJECT_HISTORY.md` |
| `STAGE_200B_DEADLINE_TOKEN.md` | `doc/IPC.md` §8.4 (deadline tokens, arm/fire/cancel, reuse safety); `doc/PROJECT_HISTORY.md` |
| `STAGE_200C1_REPLY_TIMEOUT_TRANSACTION.md` | `doc/IPC.md` §8.4 (completion transaction) |
| `STAGE_200C2A_REPLY_TIMEOUT_X86_LIVE.md`, `STAGE_200C2B_REPLY_TIMEOUT_X86_RETIREMENT.md` | `doc/IPC.md` §8.4; `doc/PROJECT_HISTORY.md` (`scan_broad_lock=0`, class retirement) |
| `STAGE_200D2B1B_SERVER_DEATH_LIVENESS_FOUNDATION.md` | `doc/IPC.md` §8.5 (nine transitions, fifteen literals, 24 races, nine guards); `doc/PROJECT_HISTORY.md` (`live_cells=0` by design) |
| `STAGE_200D2B1C_ARCH_RETURN_LIVE_READINESS.md` | `doc/ARCH_AARCH64.md` §6.1 and `doc/ARCH_RISCV64.md` §11.1 (post-drain disposition consumption); `doc/PROJECT_HISTORY.md` (readiness seal) |
| `STAGE_200D2B1D_X86_SERVER_DIES_LIVE_ATTEMPT.md`, `…D2_…`, `…D4_…` | `doc/IPC.md` §8.5 (both live defects, including `LINK_LEAK` in full); `doc/STATUS.md` §0 |
| `STAGE_200D2B1D5_DISPATCH_INVARIANT_DIAGNOSIS.md` | `doc/ARCH_X86_64.md` §6.1 (the violated invariant and why it is x86_64-specific) |
| `STAGE_200D2B1D5A_X86_POST_DRAIN_OWNER_REVALIDATION.md`, `STAGE_200D2B1D5B_OWNER_REVALIDATION_RESTORE_CONTRACT.md` | `doc/ARCH_X86_64.md` §6.1 (`OwnerRevalidation` / `OwnerCommit`, rollback, `TaskMissing` silent-success hole, unreachable `FailClosed` backstop) |

### Retained despite matching a fragment shape

| Retained | Reason |
|----------|--------|
| `doc/FIRST_COHORT_RETIREMENT_SEAL.md`, `doc/SECOND_COHORT_RETIREMENT_SEAL.md`, `doc/SECOND_COHORT_PLAIN_SEAL.md`, `doc/SECOND_COHORT_ORDINARY_CAP_SEAL.md` | **Accepted seals**, not stage narratives. Each carries an authoritative 3×N implementation matrix and is pinned by many `include_str!` assertions in the hosted corpus. |
| `doc/supervisor-*.md`, `doc/process-manager-restart-contract.md`, `doc/driver-*.md`, `doc/pm-restart-live-*.md` | **Deferred deletion.** These are pinned by `include_str!` from *production crate sources* (`crates/yarm-control-plane-servers/src/control_plane/mod.rs`, `crates/yarm-driver-servers/src/lib.rs`). Retiring them requires editing production files, which the Pass 6 change explicitly did not do. Their unique content has **not** yet been proven redundant, so they must not be deleted on name alone. |

### Known-broken references pre-dating Pass 6 (not introduced here)

* `scripts/check-contract-doc-enforcement.sh` greps `doc/ABI_CONTRACT_FREEZE.md`, which
  does not exist — that gate cannot pass.
* `scripts/qemu-ipc-recv-v2-oracle-smoke.sh` names `doc/IPC_RECV_V2_ORACLE.md` in a
  comment; the file does not exist.
* `doc/ROADMAP.md` was referenced by `DOCUMENTATION_MAP.md` and `STATUS.md` but has never
  existed in this tree. The kernel-unlock roadmap is `doc/KERNEL_UNLOCKING.md` §0.

Filenames appearing in the historical Pass 1–5 logs below are records of *already deleted*
documents, not live links.

## Validation

**`tests/doc_fragmentation_guard.rs` enforces this file's rules.** It fails the build when
a new `doc/STAGE_*.md` (or other per-stage / duplicate-status / readiness / checklist /
PR-plan shape) appears without explicit approval. Approval requires **both** an entry in
`APPROVED_FRAGMENTS` in that test **and** a row in this file — so a new fragment cannot
merge by accident, and cannot merge without recording who owns the topic. The guard also
asserts the canonical owner docs exist and that `doc/KERNEL_UNLOCK_AUDIT.md` states the
exact commit and tree it was taken against.

The canonical-owner expectations above are pinned by source-grep tests:

- `kernel::syscall::tests::*_milestone_doc_exists*` and the audit-scan tests
  in `src/kernel/syscall.rs` reference `doc/KERNEL_UNLOCKING.md`. Changing
  the canonical owner file name requires updating those `include_str!`
  paths.
- `tests/rpi5_stage1_scope.rs::rpi5_high_half_scaffold_is_explicit_and_non_default`
  references `doc/RPI5_BRINGUP.md` and pins the two phrases
  `"This scaffold does not install TTBR1"` and
  `"only then install a user root in TTBR0"` verbatim — do not reflow.

## Consolidation passes

The primary kernel-unlocking consolidation landed in Pass 1. The
secondary clusters all landed in Passes 2–4; this section is a
historical log.

### TODO §1 — Project history / status — DONE (Pass 3)

Pass 3 created `doc/PROJECT_HISTORY.md` (chronological closed-milestone
log + per-phase outcome detail) and `doc/STATUS.md` (live per-arch /
per-service / documentation-ownership / next-steps snapshot).

Deleted in the same pass: `P2_8_P2_9_CHECKLIST.md`, `P2_10_CHECKLIST.md`,
`PHASE0_IPC_BASELINE_GATES.md`, `PHASE1_PAYLOAD_POLICY.md`,
`PHASE2B_MILESTONE.md`, `PHASE3A_MILESTONE.md`, `PHASE3B_MILESTONE.md`,
`PHASE4_CALL_REPLY_CAP_PLAN.md`, `PHASE6_EXIT_GATE_REPORT.md`,
`PHASE6_SERVICE_MIGRATION_MATRIX.md`, `OPTIONAL_FS_MILESTONE_1.md`,
`FREESTANDING_SERVICE_ISOLATION_PR_PLAN.md`,
`INIT_SERVER_INITRAMFS_BOOT_PR_BOARD.md`, `TID_ALLOCATION_POLICY_PR_PLAN.md`,
`SERVER_RUNTIME_REFACTOR_STATUS.md`, `USERSPACE_SERVER_MATURITY.md`,
`USERSPACE_SERVER_BINARIES.md`.

**Pass 4 (2026-06-15) folded the six deferred CI-gated files into
`doc/PHASE_GATES.md`** and updated the gate scripts in the same pass:

- `doc/KERNEL_STATUS.md` → §1 of `doc/PHASE_GATES.md` (literal
  `PR-BND-6 pass C landed` preserved verbatim);
  `scripts/check-boundary-milestone-freeze.sh` now reads `doc/PHASE_GATES.md`.
- `doc/SERVER_ROADMAP.md` → §2 of `doc/PHASE_GATES.md` (frozen-section
  heading + dated addenda preserved verbatim);
  `scripts/check-roadmap-readiness.sh` now reads `doc/PHASE_GATES.md`.
- `doc/PHASE_READINESS_MATRIX.md` → §3 of `doc/PHASE_GATES.md`
  (all five CI tokens preserved verbatim: `phase2-driver-gates`,
  `phase3-network-gates`, `phase4-ui-gates`, `phase4-ui-smoke-marker`,
  `phase5-boundary-gates`).
- `doc/PHASE2_DRIVER_CONTRACT.md`, `doc/PHASE3_NETWORK_CONTRACT.md`,
  `doc/PHASE4_UI_CONTRACT.md` → §4/§5/§6 of `doc/PHASE_GATES.md`.

### TODO §2 — Boot / architecture — DONE (Pass 2)

Pass 2 consolidated all boot/arch fragments into:

- `doc/BOOT.md` (cmdline + memory layout + QEMU runbook)
- `doc/ARCH_AARCH64.md` (boot, IPC, VFS, PM, userspace)
- `doc/ARCH_X86_64.md` (PVH, AP Rust online, SMP fences)
- `doc/ARCH_RISCV64.md` (OpenSBI handoff, Sv39, U-mode, round-trip, services)
- `doc/RPI5_BRINGUP.md` (Stage 1 / HH-2 / HH-3 / HH-4 / HH-5)

Deleted in the same pass: `BOOT_COMMAND_LINE.md`, `BOOT_MEMORY_LAYOUT.md`,
`BOOT_QEMU_RUNBOOK.md`, `AARCH64_BOOT_BRINGUP_PR_PLAN.md`,
`AARCH64_IPC_VFS_PM_STATUS_2026_05.md`, `aarch64-initrd-init-elf-bringup.md`,
`aarch64-ipc-bootstrap-notes.md`, `x86_64_boot_path.md`,
`RISCV64_SMP_SECONDARY_RELEASE_AUDIT.md`, `rpi5-stage1.md`.

### TODO §3 — IPC / VFS / FS / networking / capability / process — DONE (Pass 4)

Pass 4 (2026-06-15) consolidated all six clusters into the seven
canonical owners marked **bold** in the table above. Source-grep
`include_str!` tests in
`crates/yarm-fs-servers/src/fs/ramfs/service.rs` and
`crates/yarm-fs-servers/src/fs/fat/service.rs` were repointed at
`doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md`. CI gate scripts
(`check-roadmap-readiness.sh`, `check-boundary-milestone-freeze.sh`,
`check-contract-doc-enforcement.sh`, `check-proc-vfs-codec-freeze.sh`,
`phase7-shared-ipc-gates.sh`) were updated atomically.

Deleted in Pass 4:

- **IPC cluster:** `SHARED_IPC_MIGRATION_GUIDE.md`,
  `SHARED_IPC_THROUGHPUT_GUIDE.md`,
  `IPC_SHARED_MEMORY_FASTPATH_PLAN.md`,
  `IPC_FRAGMENTATION_POLICY.md`, `IPC_IMPROVEMENT_PHASES.md`.
- **VFS cluster:** `VFS_REQUEST_LOOP_ABI.md`,
  `VFS_SHARED_IO_CONTRACT.md`,
  `VFS_SHARED_IO_MAPPER_REQUIREMENTS.md`,
  `PROC_VFS_CODEC_FREEZE.md`.
- **Filesystem / storage cluster:** `RAMFS_CONTRACT.md`,
  `RAMFS_SERVER_CONTRACT.md`, `INITRAMFS_CONTRACT.md`,
  `INITRAMFS_EXEC_MANIFEST_CONTRACT.md`, `DEVFS_CONTRACT.md`,
  `EXT4_SERVER_CONTRACT.md`, `FAT_SERVER_CONTRACT.md`,
  `STORAGE_SERVICE_CONTRACT.md`, `BLKCACHE_ABI.md`,
  `BLOCK_BACKEND_ABI.md`, `BLOCK_WRITE_CONTRACT.md`.
- **Networking cluster:** `NETWORK_STACK_INTEGRATION.md`,
  `NETMGR_CONTRACT.md`, `DHCP_SERVER_CONTRACT.md`,
  `DNS_SERVER_CONTRACT.md`, `TCPIP_SERVER_CONTRACT.md`,
  `SOCKET_SERVER_CONTRACT.md`, `VIRTIO_NET_CONTRACT.md`,
  `PHASE3_NETWORK_CONTRACT.md`.
- **Capability cluster:** `CAPABILITY_DOMAIN_RULES.md`,
  `CAPABILITY_RIGHTS_AUDIT.md`, `KERNEL_CSPACE_ACCESS_POLICY.md`.
- **Process / spawn cluster:** `CONTROL_PLANE_BOUNDARIES.md`,
  `PM_SPAWN_CONTRACT.md`, `TID_ALLOCATION_CONTRACT.md`,
  `INIT_SERVER_BOOT_CONTRACT.md`.
- **Phase-gates cluster (CI-gated):** `KERNEL_STATUS.md`,
  `SERVER_ROADMAP.md`, `PHASE_READINESS_MATRIX.md`,
  `PHASE2_DRIVER_CONTRACT.md`, `PHASE4_UI_CONTRACT.md`.

ABI values, opcodes, syscall numbers, struct offsets, image IDs, smoke
markers, and startup slot counts are preserved verbatim in the
canonical owners. No runtime code behavior was changed.

### TODO §4 — libc / Linux / POSIX compatibility cluster — DONE (Pass 5, 2026-06-16)

Pass 5 consolidated the three pre-existing libc/Linux/POSIX docs into
the single canonical `doc/LIBC_AND_LINUX_COMPAT.md` and updated the
remaining references atomically.

Deleted in the same pass:

- `doc/LIBC_ABI_X86_64_NONE.md`
- `doc/LINUX_COMPAT.md`
- `doc/MUSL_POSIX_IPC_MAPPING.md`

References updated in: `doc/NETWORKING.md`, `doc/PHASE_GATES.md`,
`doc/X86_64_NONE_MUSL_PORT_TODO.md`.

### TODO §5 — Agent rules + boundary rules — DONE (Pass 5, 2026-06-16)

Pass 5 merged the licensing-header rule (§15) and the
`yarm-server-runtime` boundary rules (§16) from the former `AGENTS.md`
into `doc/AI_AGENT_RULES.md`, and reduced `doc/AGENTS.md` to a short
pointer that preserves the `AGENTS.md` filename convention for
external tools. No source-grep tests changed (`AI_AGENT_RULES.md` is
the only one referenced by `src/kernel/syscall.rs::tests::*`).

### TODO §6 — Audited but kept as canonical (Pass 5, 2026-06-16)

The following docs were audited for freshness, found current, and
retained as canonical owners with explicit "Canonical: yes" notes at
the top:

- `doc/DRIVER_PROTOCOL.md` — driver-server delegation ABI.
- `doc/HAL_CONFORMANCE.md` — HAL contract surface + per-ISA platform
  layout audit (RISC-V row mirrors `doc/ARCH_RISCV64.md` §13).
- `doc/KERNEL_GLOBAL_ALLOCATOR.md` — slab + large-page allocator
  (orthogonal to `KERNEL_SCALING_PROFILE.md`).
- `doc/KERNEL_MULTITHREADING_DESIGN.md` — kernel-side thread
  mechanism (current after RISC-V BSP-only + x86_64 AP per-CPU
  scaffold landed; signal/pthread policy explicitly deferred).
- `doc/KERNEL_SCALING_PROFILE.md` — `hosted-dev` vs non-hosted
  capacities; dynamic-CNode status.
- `doc/SIGNAL_POLICY.md` — explicit non-goal + revisit prerequisites.
- `doc/TLB_INVALIDATION_POLICY.md` — per-arch invalidation contract;
  hosted no-op rationale.

No content of these files was modified other than the top-of-file
canonical-status block.
