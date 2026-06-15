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
| IPC (send/recv, shared-memory fastpath, fragmentation, throughput) | `doc/IPC.md` (to be consolidated; see TODO §3) |
| VFS (request loop, shared-I/O contract, mapper requirements) | `doc/VFS.md` (to be consolidated; see TODO §3) |
| Filesystem and storage (RAMFS/initramfs/devfs/FAT/ext4 + block) | `doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md` (to be consolidated; see TODO §3) |
| Networking (netmgr/DHCP/DNS/TCPIP/socket/virtio-net) | `doc/NETWORKING.md` (to be consolidated; see TODO §3) |
| Capabilities (rights, domains, cspace access) | `doc/CAPABILITY_MODEL.md` (to be consolidated; see TODO §3) |
| Process / spawn (PM contract, TID allocation, control plane) | `doc/PROCESS_AND_SPAWN.md` (to be consolidated; see TODO §3) |
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

## Outstanding consolidation TODOs

The primary kernel-unlocking consolidation landed in this pass. The
secondary clusters listed below remain open and should be tackled one
cluster at a time:

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

**Deferred to Pass 4** (live CI gate scripts pin specific file names
and content; deletion would break gate checks):

- `doc/KERNEL_STATUS.md` — `scripts/check-boundary-milestone-freeze.sh`
  requires the literal `PR-BND-6 pass C landed` string in this file.
- `doc/SERVER_ROADMAP.md` — `scripts/check-roadmap-readiness.sh`
  enforces frozen-section + dated-addenda + gate-wiring text in this
  file.
- `doc/PHASE_READINESS_MATRIX.md` — `scripts/check-roadmap-readiness.sh`
  enforces specific CI-token strings (`phase2-driver-gates`,
  `phase3-network-gates`, `phase4-ui-gates`, `phase4-ui-smoke-marker`,
  `phase5-boundary-gates`).
- `doc/PHASE2_DRIVER_CONTRACT.md`, `doc/PHASE3_NETWORK_CONTRACT.md`,
  `doc/PHASE4_UI_CONTRACT.md` — `scripts/check-roadmap-readiness.sh`
  requires the files to exist as the phase contracts.

These six files will be consolidated alongside the IPC / VFS / FS /
networking / capability / process clusters in Pass 4, where the same PR
can update the gate scripts to point at the new canonical owners
(`doc/STATUS.md` + per-domain contract docs).

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
