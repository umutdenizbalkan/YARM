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
| Boot (boot flow, command line, memory layout, QEMU runbook) | `doc/BOOT.md` (to be consolidated; see TODO §2) |
| Architecture — AArch64 | `doc/ARCH_AARCH64.md` (to be consolidated; see TODO §2) |
| Architecture — x86_64 | `doc/ARCH_X86_64.md` (to be consolidated; see TODO §2) |
| Architecture — RISC-V64 | `doc/ARCH_RISCV64.md` (to be consolidated; see TODO §2) |
| RPi5 bring-up | `doc/RPI5_BRINGUP.md` (to be consolidated; see TODO §2) |
| IPC (send/recv, shared-memory fastpath, fragmentation, throughput) | `doc/IPC.md` (to be consolidated; see TODO §3) |
| VFS (request loop, shared-I/O contract, mapper requirements) | `doc/VFS.md` (to be consolidated; see TODO §3) |
| Filesystem and storage (RAMFS/initramfs/devfs/FAT/ext4 + block) | `doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md` (to be consolidated; see TODO §3) |
| Networking (netmgr/DHCP/DNS/TCPIP/socket/virtio-net) | `doc/NETWORKING.md` (to be consolidated; see TODO §3) |
| Capabilities (rights, domains, cspace access) | `doc/CAPABILITY_MODEL.md` (to be consolidated; see TODO §3) |
| Process / spawn (PM contract, TID allocation, control plane) | `doc/PROCESS_AND_SPAWN.md` (to be consolidated; see TODO §3) |
| Service manifest format | `doc/SERVICE_MANIFEST.md` |
| Project history (closed phases / milestones / checklists) | `doc/PROJECT_HISTORY.md` (to be created; see TODO §1) |
| Roadmap (current direction) | `doc/ROADMAP.md` |
| Project status / maturity | `doc/STATUS.md` (to be consolidated; see TODO §1) |
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

## Outstanding consolidation TODOs

The primary kernel-unlocking consolidation landed in this pass. The
secondary clusters listed below remain open and should be tackled one
cluster at a time:

### TODO §1 — Project history / status

- Migrate closed phase / milestone / checklist files into a new
  `doc/PROJECT_HISTORY.md`, then delete the originals:
  - `doc/P2_8_P2_9_CHECKLIST.md`
  - `doc/P2_10_CHECKLIST.md`
  - `doc/PHASE0_IPC_BASELINE_GATES.md`
  - `doc/PHASE1_PAYLOAD_POLICY.md`
  - `doc/PHASE2B_MILESTONE.md`
  - `doc/PHASE2_DRIVER_CONTRACT.md`
  - `doc/PHASE3A_MILESTONE.md`
  - `doc/PHASE3B_MILESTONE.md`
  - `doc/PHASE3_NETWORK_CONTRACT.md`
  - `doc/PHASE4_CALL_REPLY_CAP_PLAN.md`
  - `doc/PHASE4_UI_CONTRACT.md`
  - `doc/PHASE6_EXIT_GATE_REPORT.md`
  - `doc/PHASE6_SERVICE_MIGRATION_MATRIX.md`
  - `doc/OPTIONAL_FS_MILESTONE_1.md`
  - `doc/FREESTANDING_SERVICE_ISOLATION_PR_PLAN.md`
  - `doc/INIT_SERVER_INITRAMFS_BOOT_PR_BOARD.md`
  - `doc/TID_ALLOCATION_POLICY_PR_PLAN.md`
- Consolidate status / maturity / readiness into `doc/STATUS.md`, then
  delete:
  - `doc/SERVER_ROADMAP.md`
  - `doc/SERVER_RUNTIME_REFACTOR_STATUS.md`
  - `doc/USERSPACE_SERVER_MATURITY.md`
  - `doc/USERSPACE_SERVER_BINARIES.md`
  - `doc/PHASE_READINESS_MATRIX.md`
  - `doc/KERNEL_STATUS.md`

### TODO §2 — Boot / architecture

- Create `doc/BOOT.md` consolidating boot cmdline, memory layout, QEMU
  runbook. Delete after migration:
  - `doc/BOOT_COMMAND_LINE.md`
  - `doc/BOOT_MEMORY_LAYOUT.md`
  - `doc/BOOT_QEMU_RUNBOOK.md`
- Create `doc/ARCH_AARCH64.md` consolidating aarch64 bring-up + IPC/VFS/PM
  status. Delete after migration:
  - `doc/AARCH64_BOOT_BRINGUP_PR_PLAN.md`
  - `doc/AARCH64_IPC_VFS_PM_STATUS_2026_05.md`
  - `doc/aarch64-initrd-init-elf-bringup.md`
  - `doc/aarch64-ipc-bootstrap-notes.md`
- Create `doc/ARCH_X86_64.md` from `doc/x86_64_boot_path.md` and delete the
  original.
- Create `doc/ARCH_RISCV64.md` from `doc/RISCV64_SMP_SECONDARY_RELEASE_AUDIT.md`
  and the recent RISC-V Sv39 / round-trip work referenced in commit history;
  delete the original audit fragment.
- Create `doc/RPI5_BRINGUP.md` from `doc/rpi5-stage1.md`; delete the original.

### TODO §3 — IPC / VFS / FS / networking / capability / process

Each cluster is a multi-file merge that needs careful preservation of live
ABI offsets. Suggested PR sequence:

1. **IPC.** Merge `doc/SHARED_IPC_MIGRATION_GUIDE.md`,
   `doc/SHARED_IPC_THROUGHPUT_GUIDE.md`,
   `doc/IPC_SHARED_MEMORY_FASTPATH_PLAN.md`,
   `doc/IPC_FRAGMENTATION_POLICY.md`,
   `doc/IPC_IMPROVEMENT_PHASES.md` → `doc/IPC.md`.
2. **VFS.** Merge `doc/VFS_REQUEST_LOOP_ABI.md`,
   `doc/VFS_SHARED_IO_CONTRACT.md`,
   `doc/VFS_SHARED_IO_MAPPER_REQUIREMENTS.md`,
   `doc/PROC_VFS_CODEC_FREEZE.md` → `doc/VFS.md`.
3. **Filesystem / storage.** Merge
   `doc/RAMFS_CONTRACT.md`, `doc/RAMFS_SERVER_CONTRACT.md`,
   `doc/INITRAMFS_CONTRACT.md`, `doc/INITRAMFS_EXEC_MANIFEST_CONTRACT.md`,
   `doc/DEVFS_CONTRACT.md`, `doc/EXT4_SERVER_CONTRACT.md`,
   `doc/FAT_SERVER_CONTRACT.md`, `doc/STORAGE_SERVICE_CONTRACT.md`,
   `doc/BLKCACHE_ABI.md`, `doc/BLOCK_BACKEND_ABI.md`,
   `doc/BLOCK_WRITE_CONTRACT.md` → `doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md`.
4. **Networking.** Merge `doc/NETWORK_STACK_INTEGRATION.md`,
   `doc/NETMGR_CONTRACT.md`, `doc/DHCP_SERVER_CONTRACT.md`,
   `doc/DNS_SERVER_CONTRACT.md`, `doc/TCPIP_SERVER_CONTRACT.md`,
   `doc/SOCKET_SERVER_CONTRACT.md`, `doc/VIRTIO_NET_CONTRACT.md` →
   `doc/NETWORKING.md`.
5. **Capabilities.** Merge `doc/CAPABILITY_DOMAIN_RULES.md`,
   `doc/CAPABILITY_RIGHTS_AUDIT.md`,
   `doc/KERNEL_CSPACE_ACCESS_POLICY.md` → `doc/CAPABILITY_MODEL.md`.
6. **Process / spawn.** Merge `doc/CONTROL_PLANE_BOUNDARIES.md`,
   `doc/PM_SPAWN_CONTRACT.md`, `doc/TID_ALLOCATION_CONTRACT.md`,
   `doc/INIT_SERVER_BOOT_CONTRACT.md` → `doc/PROCESS_AND_SPAWN.md` (keep
   `INIT_SERVER_BOOT_CONTRACT.md`'s slot 0..17 definitions verbatim under a
   subsection; they are load-bearing ABI).

Each cluster PR must:

- Verify by grep that no `include_str!`, README, script, or `.github/`
  workflow references the file being deleted.
- Update any references to point at the new canonical owner.
- Run `git diff --check` and the source-grep tests pinned in
  `src/kernel/syscall.rs`.
