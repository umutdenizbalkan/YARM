<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM Project History

> **This file is historical and not the live source for ABI / policy / status.**
> For live state see `doc/STATUS.md`. For the canonical owners of each topic
> see `doc/DOCUMENTATION_MAP.md`. ABI invariants live in `doc/SYSCALL_ABI.md`,
> `doc/KERNEL_UNLOCKING.md` §3, and per-domain contract docs (RAMFS, VFS, etc).
> This file exists only to preserve milestone outcomes, exit-gate decisions,
> and PR-plan conclusions for future reviewers who need the context.

This file is chronological and read-only in spirit: do not add new design,
ABI, or status content here. Add new live content to the canonical owner doc,
and only summarize a closed milestone here when it is genuinely done.

---

## Chronological milestone log (high level)

| Phase / Milestone | Closed (as recorded) | One-line outcome |
|-------------------|----------------------|------------------|
| Phase 0 — IPC baseline gates | early stages | Five named conformance tests pinned (round-trip, IRQ-notification routing, shared-mem cap-transfer, receiver auto-map, transfer-release unmap-revoke) |
| Phase 1 — Inline payload + framing policy | early stages | `Message::MAX_PAYLOAD = 128` frozen; medium payloads use fragmentation, large payloads use shared-memory descriptor |
| Phase 2A — Direct syscall bridge | superseded by Phase 2B | PM called `nr=27` directly, bypassing VFS, for emergency CPIO loading; retained only as `InvalidArgs`/`Unsupported` fallback |
| Phase 2B — VFS-mediated bulk read | frozen 2026-05-27 | Bulk read of image_id 7/8/9 via VFS → initramfs_srv → kernel cross-ASID copy into PM's 4 KiB transfer buffer |
| Phase 3A — InitramfsFileSlice MemoryObject cap grant | impl complete (pre-Phase 3B) | Full IPC path PM→VFS→initramfs_srv→nr=28→cap→nr=29→ZC loader; `zc_pages=0` because alignment pre-conditions were not yet met |
| Phase 3B — Page-aligned zero-copy ELF loading | runtime-proven 2026-05-27 | 4 KiB ELF LOAD alignment + page-aligned CPIO payloads → `zc_pages > 0` for image_id 7/8/9 (`driver_manager`, `blkcache_srv`, `virtio_blk_srv`); proven on AArch64 QEMU and x86_64 QEMU `-smp 1` |
| Phase 4 — Call/Reply Capability | implemented (model, syscalls, single-use, replay reject) | Reply-cap IPC kernel model (`CapObject::Reply`, bounded reply-cap record table, generation-protected slot identity, single-use, revoke-on-use); current authoritative ABI lives in `doc/SYSCALL_ABI.md` |
| Phase 6 — Service migration + deprecation | exit-gate report drafted | Core control-plane services (VFS, supervisor, init, process_manager) migrated to typed/budgeted control-plane request/reply helper model; legacy `kernel.ipc_recv` guardrail active; soft sunset 2026-06-30 / hard sunset 2026-09-30 (as recorded by the original exit-gate report) |
| P2.8 / P2.9 — Page-table + frame-allocator scaling | ✅ closed | Non-hosted TLB invalidation hooks (AArch64 + RISC-V); scalable frame storage replacing `MAX_TRACKED_FRAMES`; contiguous-alloc fast path; long-run fragmentation/throughput tests |
| P2.10 — Page-table + frame-allocator production hardening | ✅ closed | Strict ISA smoke jobs (x86_64 / AArch64 / RISC-V64) merge-blocking; non-hosted invalidation correctness sign-off; per-ISA test logs archived |
| TID Allocation Policy cleanup | ✅ closed (4 phases) | `TidAllocationPolicy` + cursor abstraction; gap-accounting telemetry (`dynamic_tid_allocations`, `dynamic_tid_wraps`, `gap_floor_repairs`); CI dynamic-floor enforcement |
| Freestanding Service Isolation PR plan | retired (folded into live flow) | Non-hosted PVH module discovery + initramfs executable manifest + ELF validation + initramfs-launched `init_server` in a dedicated user AS |
| Init-server initramfs-boot PR board | retired (folded into live flow) | PRs that moved x86_64 first-user from synthetic syscall/yield to real initramfs ELF launch in a dedicated user AS |
| Server Runtime Boundary / POSIX / VFS refactor | ✅ closed | `yarm-server-runtime` no longer a root-crate re-export bridge; userspace runtime in `yarm-user-rt`; startup-slot ABI in `doc/INIT_SERVER_BOOT_CONTRACT.md` |
| Optional FS Milestone 1 | ✅ declared (Stage 100) | All userspace FS servers built/staged/tested; RAMFS fully writable proof; ext4 read-only live; FAT profile-ready and disabled; strict optional-FS smoke scripts for x86_64 + AArch64 |
| Kernel Unlocking Milestone 1 (Stage 106) | ✅ declared (2026-06-12) | D1 / D2 / D5 live splits; D3.1 + D6.1 first live wires (Stage 107); see `doc/KERNEL_UNLOCKING.md` |
| Kernel Unlocking Milestone 2 Pass 1 (Stage 108) | ✅ infrastructure landed | SharedKernel split-mut seams (ranks 1/2/5/6), `yarm.loglevel=` knob, x86_64 SMP trampoline split |
| Kernel Unlocking Milestone 2 Pass 2 (Stage 109) | ✅ outcome A | x86_64 AP enters Rust and parks; scheduler stays BSP-only; AP per-CPU env remains the next blocker |

For the canonical, currently-live state of each item see `doc/STATUS.md`.

---

## Phase outcomes — detail

### Phase 0 — IPC baseline gates

Five conformance tests pinned before any IPC refactor:

- `capability_checked_ipc_round_trip`
- `notification_irq_route_delivers_message_to_bound_endpoint`
- `syscall_send_large_payload_uses_shared_region_descriptor_with_cap_transfer`
- `syscall_recv_shared_mem_can_auto_map_into_receiver_when_requested`
- `syscall_transfer_release_unmaps_receiver_range_and_revokes_transfer_cap`

These pin endpoint round-trip semantics, IRQ-notification routing, shared-memory
transfer descriptor path, receiver auto-map contract, and transfer-release
unmap+revoke lifecycle. They remain in the live test suite.

### Phase 1 — Payload capacity and framing policy

- Frozen `Message::MAX_PAYLOAD = 128` bytes.
- Medium payloads (>128 B and <shared-memory threshold) use the fragmentation
  protocol (current canonical doc: `doc/IPC_FRAGMENTATION_POLICY.md`; pending
  consolidation into `doc/IPC.md`).
- Large payloads use the shared-memory descriptor path
  (`OPCODE_SHARED_MEM`).
- Original benchmark snapshot:
  - `inline64 = 94.96 ns/op`
  - `inline128 = 96.80 ns/op`
  - `shared_desc = 80.93 ns/op`
  - `simulated_2x128 = 193.61 ns/op`

### Phase 2A — Direct-syscall bootstrap bridge (superseded)

PM called `nr=27` (`InitramfsReadChunk`) **directly**, bypassing VFS:

```
PM (tid=3) --[nr=27, arg5=0]--> kernel --> CPIO lookup --> PM AS
```

`arg5=0` meant copy into the caller's own address space (self-ASID). Retained
as an `InvalidArgs`/`Unsupported` fallback only. `PM_VFS_READ_BULK_PHASE2A_BEGIN`
count must be `0` in the Phase 3B-and-later freeze.

### Phase 2B — VFS-mediated transfer-buffer bulk read (frozen 2026-05-27)

PM (tid=3) fetches each ELF binary for image_id 7/8/9 through VFS, which
routes the request to `initramfs_srv` (tid=5), which uses a kernel-assisted
cross-ASID copy to fill PM's 4 KiB stack transfer buffer directly. This is
still the recorded baseline for non-zero-copy bulk read; Phase 3B's
zero-copy path runs alongside.

### Phase 3A — InitramfsFileSlice MemoryObject cap grant

New kernel object kind:

```rust
pub(crate) enum MemoryObjectKind {
    Anonymous,
    InitramfsFileSlice { initrd_offset: u64, file_len: u64 },
}
```

Backed by a read-only slice of the boot initrd mapping; `READ | MAP` rights,
no `WRITE`. Hard constraints preserved: `VFS_READ_SHARED_REPLY_ENABLED` NOT
enabled; Phase 2B transfer-buffer path NOT removed; syscall 27 NOT removed;
SpawnV5 ABI NOT changed; no heap-size increase; no generic writable shared
memory; no child MemoryObject caps; only PM may spawn from MemoryObject caps;
file-backed MemoryObject mappings remain read-only.

Phase 3A reported `zc_pages=0` because two physical-alignment pre-conditions
were not yet met. Phase 3B closed that.

### Phase 3B — Page-aligned zero-copy ELF loading (proven 2026-05-27)

`load_elf_with_mo_zero_copy` delivers `zc_pages > 0` for image_id 7/8/9
(`sbin/driver_manager`, `sbin/blkcache_srv`, `sbin/virtio_blk_srv`) on
AArch64 QEMU and x86_64 QEMU `-smp 1`. The CPIO packer's mandatory
4096-byte `ALIGN_PROOF` checks are part of this contract; see
`doc/BOOT.md` §1.

### Phase 4 — Call/Reply capability (implemented)

Implementation slices:

1. ✅ Kernel object-model extension: `CapObject::Reply` with
   generation-protected slot identity; bounded in-kernel reply-cap record
   table with owner/caller/endpoint binding metadata.
2. ✅ `IpcCall` mints an ephemeral reply capability; bound to caller and
   invocation context; single-use; revoke-on-use; deterministic rejection
   of replay/use-after-consume.

The current authoritative ABI lives in `doc/SYSCALL_ABI.md`; the recv-v2 +
reply-cap + blocked-waiter contract is documented in `doc/ARCH_AARCH64.md`
§4 (the portable AArch64 reference) and is enforced by the regression set
in `doc/ARCH_AARCH64.md` §4.4.

### Phase 6 — Service migration + deprecation

Per the original exit-gate report:

- ✅ Control-plane legacy blocking-receive guardrail (`kernel.ipc_recv`)
  active.
- ✅ Control-plane exit-gate migration bundle canary active.
- ✅ PM kernel-IPC round-trip uses reply-cap call/reply helper path.
- ✅ VFS kernel-IPC round-trip uses reply-cap call/reply helper path.
- ✅ Supervisor non-RPC / event-driven flows covered by dated
  non-applicability waiver; request/reply-like status-query path supports
  reply-cap compatibility + helper entrypoint.
- ✅ Init orchestration path covered by dated non-applicability waiver
  (not a dedicated kernel request/reply loop service).

Dated checkpoints recorded:

- **Soft sunset checkpoint:** 2026-06-30 — all core control-plane services
  have timed-receive migration + source guardrails.
- **Hard sunset target:** 2026-09-30 — no legacy ad-hoc two-endpoint
  request/reply choreography in core control-plane services unless
  explicitly documented as dated waiver with owner and closure plan.

Service migration matrix (frozen at closure):

| Service | Owning crate path | Current receive/reply model | Status |
|---------|-------------------|------------------------------|--------|
| VFS control-plane service | `crates/yarm-control-plane-servers/src/control_plane/vfs/service.rs` | typed/budgeted control-plane request/reply helper | ✅ migrated |
| Supervisor service | `crates/yarm-control-plane-servers/src/control_plane/supervisor/service.rs` | fault/control queue handling + reply paths | ✅ migrated |
| Init service | `crates/yarm-control-plane-servers/src/control_plane/init/service.rs` | orchestration-focused (not a dedicated request loop) | ✅ migrated |
| Process Manager service | `crates/yarm-control-plane-servers/src/control_plane/process_manager/service.rs` | typed/budgeted control-plane request/reply helper | ✅ migrated |

Exit criteria still apply as live invariants (see `doc/STATUS.md`):

- No reintroduction of legacy monolithic service paths under `src/services/*`.
- Boundary gates remain green (`phase5-boundary-gates`).
- Service ownership remains crate-local in extracted server crates.

### P2.8 / P2.9 — Page-table + frame-allocator scaling

- ✅ Non-hosted TLB invalidation hooks for AArch64 and RISC-V (page and
  ASID).
- ✅ Architecture-targeted smoke validation in CI QEMU lanes.
- ✅ Explicit doc note for hosted-dev no-op behavior — see
  `doc/TLB_INVALIDATION_POLICY.md` (current canonical owner).
- ✅ Replaced single fixed bitmap capacity (`MAX_TRACKED_FRAMES`) with
  scalable storage.
- ✅ Fast-path contiguous-allocation data structure.
- ✅ Long-run fragmentation / throughput tests.

### P2.10 — Page-table + frame-allocator production hardening

- ✅ Strict ISA smoke jobs for `x86_64`, `aarch64`, `riscv64` triggered
  automatically by PRs touching `src/arch/**`, `src/kernel/vm.rs`,
  `src/kernel/frame_allocator.rs`, or boot/memory init paths; branch
  protection requires the strict lanes; logs archived as artifacts.
- ✅ Non-hosted invalidation correctness tests per ISA with required
  ordering/barrier semantics; per-ISA sign-off artifact in CI outputs.

### TID Allocation Policy cleanup

Four phases, all closed:

1. ✅ Policy floor + gap enforcement — `INITIAL_DYNAMIC_TID` hard lower
   bound; cursor normalization; floor + wrap regression tests.
2. ✅ Explicit allocation model + cursor abstraction — `TidAllocationPolicy`
   / cursor helper in `kernel::boot`; reserved/dynamic/wrap semantics
   explicit.
3. ✅ Gap accounting + diagnostics — `dynamic_tid_allocations`,
   `dynamic_tid_wraps`, `gap_floor_repairs` telemetry; structured
   boot-time diagnostics.
4. ✅ CI enforcement — fail if dynamic allocation returns a TID below
   floor; targeted kernel test suite for TID policy invariants.

Current canonical contract: `doc/TID_ALLOCATION_CONTRACT.md` (pending
consolidation into `doc/PROCESS_AND_SPAWN.md` per `doc/DOCUMENTATION_MAP.md`
TODO §3).

### Freestanding Service Isolation PR plan (retired)

Originally a four-phase track:

1. Non-hosted boot payload discovery + launch-manifest seed (PVH module
   metadata parse, deterministic telemetry for the discovered initramfs
   window).
2. Initramfs executable manifest contract (stable paths/metadata for core
   services; typed loader-manifest format and parser; contract tests for
   missing/corrupt manifest entries).
3. ELF image validation + mapping contract integration (bridge into
   `ElfImageInfo`; segment-level mapping plan; reject-on-invalid ABI/ELF
   layout before spawn).
4. `init_server` launched via initramfs (replace fixed entry constants).

Folded into the live boot flow — see `doc/BOOT.md` for the current
artifact contract.

### `init_server` from initramfs PR board (retired)

PR sequence that moved x86_64 first-user from a synthetic syscall/yield
loop to a real initramfs ELF launched into a dedicated user address
space. Folded into the live boot flow; current arch-specific status lives
in `doc/ARCH_X86_64.md` / `doc/ARCH_AARCH64.md` / `doc/ARCH_RISCV64.md`.

### Server runtime / POSIX / VFS refactor

Closed; the following live invariants survive:

- `yarm-server-runtime` does not act as a root `yarm` re-export bridge.
- Server crates consume server-facing runtime surfaces from
  `yarm-server-runtime`, not kernel internals from root `yarm`.
- Boundary enforcement remains in place via crate-graph checks
  (`scripts/check-crate-graph-boundary.py`).
- `yarm-user-rt` provides userspace IPC entry points (`ipc_send` /
  `ipc_recv`) plus arch-specific asm under `crates/yarm-user-rt/src/arch`;
  `IpcTransport` / `SyscallIpcTransport` are available.
- Startup-slot ABI lives in `doc/INIT_SERVER_BOOT_CONTRACT.md` (slots
  0..17 documented there; `STARTUP_SLOT_COUNT = 18`).

### Optional FS Milestone 1 (Stage 100)

Authoritative milestone record (frozen): all userspace FS servers built,
staged, tested; RAMFS fully writable proof; ext4 read-only live; FAT
profile-ready and disabled pending virtio-blk round-trip proof; strict
optional-FS smoke scripts for x86_64 and AArch64; no known yarm-fs-servers
or yarm-control-plane-servers failures.

Current FS status lives in `doc/STATUS.md`; the FS server contracts live
in their domain docs (RAMFS / FAT / ext4 / DEVFS / etc.) pending
consolidation into `doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md` per
`doc/DOCUMENTATION_MAP.md` TODO §3.

### Kernel Unlocking milestones (current line)

All current unlocking status — Milestone 1 declaration, Milestone 2 Pass
1 / Pass 2, directive status table, live paths and fallbacks, scaffold
status, recent correctness fixes, remaining work — lives in
`doc/KERNEL_UNLOCKING.md`. The pieces below are kept here only for the
chronology:

- Milestone 1 declared 2026-06-12 at Stage 106 with smoke acceptance on
  three runs (x86_64 `-smp 1` core, x86_64 optional-FS strict, AArch64
  optional-FS strict).
- Milestone 2 Pass 1 (Stage 108) landed SharedKernel split-mut seams
  (ranks 1/2/5/6), `yarm.loglevel=` knob, and the x86_64 SMP trampoline
  split, with zero live-path behavior change.
- Milestone 2 Pass 2 (Stage 109) made x86_64 APs enter Rust and park
  online; scheduler participation remains BSP-only; the exact remaining
  blocker is the AP per-CPU environment.

---

## Authoring rule

Do not add new design / ABI / status content here. Add new live content
to the canonical owner doc (see `doc/DOCUMENTATION_MAP.md`). Append a
new row to the chronological table above only when a milestone is
genuinely closed and you have already updated the canonical owner doc to
reflect the new live state.
