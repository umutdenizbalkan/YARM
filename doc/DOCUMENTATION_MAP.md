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
| Kernel unlocking (decomposition, milestones, status, audits) | **`doc/KERNEL_UNLOCKING.md`** |
| Kernel locking architecture (lock-rank design, domains, invariants) | `doc/KERNEL_LOCKING.md` |
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
| Agent rules (capability/spawn/zero-copy/smoke policy) | `doc/AI_AGENT_RULES.md` |
| Kernel test rules (per-rule unit-test guard rails) | `doc/KERNEL_TEST_RULES.md` |
| Cross-cutting agent-facing reference | `doc/AGENTS.md` |

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

## Validation

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
