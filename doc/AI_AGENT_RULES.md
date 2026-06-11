# YARM AI Agent Rules: Capability, Spawn, and Zero-Copy Constraints

**Scope:** Rules for AI agents (Claude, Codex, or similar) working on YARM kernel,
IPC, server, or build-script code. These rules encode invariants proven through
Phase 2A → 2B → 3A → 3B. Violating them risks silent correctness regressions that
are hard to diagnose from logs alone.

---

## 1. Capability Rules

### 1.1 Never encode local cap IDs in payload bytes as authority

Cap IDs (slot numbers) are local to a specific cnode/task cspace. Embedding a cap
ID in an IPC message payload and treating it as transferable authority is wrong:
the receiving task's cspace is independent, and the same integer may refer to a
different object (or no object) there.

**Correct:** use the kernel's IPC cap-transfer path (see §1.3).

### 1.2 Cap IDs are cspace-local

A cap ID returned by `create_*`, `grant_*`, or `materialize_*` is valid only within
the cnode of the task that received it. Never assume a cap ID received in a payload
refers to a cap in your own cspace without an explicit materialization step.

### 1.3 Authority transfer must use the real IPC transferred-cap path

The only sanctioned way to transfer a capability between tasks is:

1. Sender: set `FLAG_CAP_TRANSFER_PLAIN` in the IPC flags word and place the local
   cap ID in the designated transfer field of the message.
2. Kernel: stashes the cap on the pending IPC and strips it from the sender's cspace.
3. Receiver: the cap is materialized into the receiver's cspace; available via
   `received.transferred_cap`.

Do not encode cap IDs in `payload[0..4]` or similar and expect them to work.

### 1.4 Use FLAG_CAP_TRANSFER_PLAIN for reply-with-cap

`FLAG_CAP_TRANSFER_PLAIN = 1 << 2`

Use this flag when replying with a transferred cap via `ipc_reply`. It does **not**
strip an opcode prefix from the payload.

**Do not use** the older `FLAG_CAP_TRANSFER` (without `_PLAIN`) for plain replies:
it triggers opcode-prefix stripping, which corrupts the payload when the reply body
does not start with an opcode word.

### 1.5 Reply caps are one-shot and non-delegatable

A reply capability created by `ipc_call` is consumed exactly once by `ipc_reply`.
It cannot be delegated to another task, stored in a cnode for later use, or used to
send additional messages. Attempting a second reply on the same cap returns
`StaleCapability`.

### 1.6 Reply cap cleanup uses fast revoke

When a reply cap is no longer needed (timeout, cancellation), use the fast-revoke
path (`IPC_FAST_REVOKE`). Do not traverse the general revocation/delegation graph
for reply cap cleanup — it is disproportionately expensive and is not designed for
single-shot reply caps.

### 1.7 Capability errors are fatal unless explicitly specified otherwise

The following errors on a cap operation indicate a programming bug or a corrupt
system state and must **not** be silently ignored or retried:

- `MissingRight` — caller lacks the required right for this operation
- `WrongObject` — cap refers to the wrong kernel object type
- `StaleCapability` — cap has been consumed or revoked
- `MaterializeFailed` — cap could not be installed into the receiver cspace

The only permitted recovery is explicit fallback logic documented in the relevant
milestone (e.g., Phase 3A falls back to Phase 2B on `Unsupported` from
`VFS_OP_FILE_GRANT_RO`). All other capability errors must propagate as hard failures.

---

## 2. Startup / Slot Rules

### 2.1 Do not casually change startup cap slots

The startup cap layout for each service is fixed and documented in
`doc/INIT_SERVER_BOOT_CONTRACT.md`. Changing a slot number requires updating every
consumer of that slot and verifying the change in both AArch64 and x86_64 smoke.

### 2.2 image_id 7/8/9 late services use zeroed extra caps

`sbin/driver_manager` (image_id 7), `sbin/blkcache_srv` (image_id 8), and
`sbin/virtio_blk_srv` (image_id 9) are spawned with zeroed extra caps in the
current Phase 3B implementation. Do not reintroduce a `vfs_recv_cap` in slot 13
or any other slot unless a real consumer is implemented and the slot layout is
re-validated end-to-end.

### 2.3 startup_args[0] must be the final task TID before first entry

The first word of the startup args array passed to a newly spawned task must be
its own task ID (TID). This is set by the kernel before the task's first instruction
executes. Do not overwrite `startup_args[0]` from userspace after spawn.

### 2.4 Startup slots live below stack_top; do not assert SP == stack_top

The startup args and cap slots are placed on the initial stack below the nominal
`stack_top` address. At first entry, SP points below `stack_top`, not at it. Do
not add an assertion `SP == stack_top` in entry stubs.

### 2.5 x86_64 first-entry stack must satisfy SysV ABI alignment

At the point of the first user instruction on x86_64, the stack pointer must be
16-byte aligned minus 8 bytes (as if a `call` instruction just pushed a return
address). Violating this causes SSE/AVX faults in functions that use aligned loads.

---

## 3. Fallback Rules

### 3.1 Phase 3A/3B fallback to Phase 2B is allowed only on Unsupported

PM may fall back from `VFS_OP_FILE_GRANT_RO` (Phase 3A/3B path) to
`VFS_OP_READ_BULK` (Phase 2B path) **only** if the VFS reply carries opcode
`Unsupported` (meaning the running kernel or VFS does not implement the operation).

This allows a Phase 3A/3B PM binary to boot on an older kernel that lacks syscall
nr=28 or nr=29.

### 3.2 Hard errors on the grant-RO path are fatal — do not silently fall back

The following responses from `VFS_OP_FILE_GRANT_RO` must **never** trigger a silent
fallback to Phase 2B. They indicate a real system error:

- `NotFound` — the file does not exist in the CPIO archive
- `PermissionDenied` / `MissingRight` — access control violation
- `PageFault` — memory error during cap materialization
- `OOM` / `CapabilityFull` — resource exhaustion
- `WrongObject` — MemoryObject kind mismatch
- `StaleCapability` — cap was already consumed
- `MaterializeFailed` — cap could not be installed in PM's cspace

Silently falling back to bulk-read in these cases hides real bugs and defeats the
Phase 3B acceptance criteria.

### 3.3 Do not silently fall back to 112-byte inline READ

The Phase 2A inline-read path (112-byte `OPCODE_INLINE` IPC payload) is an
emergency fallback for kernels that do not support `READ_BULK`. It must not be
triggered in the Phase 3B freeze. If `PM_VFS_READ_BULK_PHASE2A_BEGIN` appears in
a smoke log, it is a regression.

### 3.4 Syscall 27 is deprecated after Phase 3B — do not remove yet

Syscall nr=27 `InitramfsReadChunk` (both the self-ASID and cross-ASID paths) is
still present in the kernel for Phase 2B fallback compatibility. It must not be
removed until the long-run gate passes (see `doc/PHASE3B_MILESTONE.md §Deprecated`).

---

## 4. Zero-Copy Rules

### 4.1 Required alignment condition

Zero-copy mapping from an `InitramfsFileSlice` MemoryObject is only valid when:

```
(initrd_phys_base + file_initrd_offset + elf_page_offset_within_file) % PAGE_SIZE == 0
```

where `PAGE_SIZE = 4096`. This requires both:
- The CPIO archive places the ELF file's data at a 4096-byte boundary
  (`file_initrd_offset % PAGE_SIZE == 0`), and
- The ELF's PT_LOAD segments have `p_offset ≡ p_vaddr (mod PAGE_SIZE)` so that
  each page within the segment also lands on a page boundary in the archive.

### 4.2 Only full non-writable pages may be mapped zero-copy

Pages in a PT_LOAD segment with the W (writable) flag must always be copied to a
fresh anonymous physical frame, never mapped from the initramfs backing.

### 4.3 Writable, BSS, and partial pages must copy

| Page type | Action |
|-----------|--------|
| W segment page | Alloc fresh frame + copy file data |
| BSS page (`va >= p_vaddr + p_filesz`) | Alloc fresh zeroed frame |
| Partial head page (`va < p_vaddr`) | Alloc fresh frame + copy |
| Partial tail page (`va + PAGE > p_vaddr + p_filesz`) | Alloc fresh frame + copy |

### 4.4 W+X segments are rejected

Any PT_LOAD segment with both W and X flags set must be rejected before any page
mapping is attempted. The ZC loader must emit `ZC_PAGE reason=wx_rejected` and
return a hard error, not silently skip the segment.

### 4.5 Use the aligned CPIO packer for boot-service payloads

The standard `cpio` tool aligns file data to 4 bytes only. Every QEMU CPIO
archive containing ELF payloads must use `scripts/pack-initramfs-aligned.py` (or
`common_create_initramfs_aligned`). The rule applies to `/init`, every `/sbin/*`
ELF, and any ELF added later, not only services currently loaded zero-copy.

Verify one `ALIGN_PROOF ... alignment_mod=0 aligned=true` line per ELF. Missing
Python, a missing packer, or any unaligned ELF is a hard packing error.

### 4.6 Do not give child tasks MemoryObject caps

MemoryObject caps created via nr=28 must not be transferred to spawned child tasks.
Only PM (TID=3) may hold and consume MemoryObject caps for spawn. After syscall nr=29
returns, the MemoryObject cap in PM's cspace should be revoked (the kernel does this
automatically upon successful spawn).

---

## 5. Architecture Rules

### 5.1 x86_64 smoke stays at -smp 1

`scripts/qemu-x86_64-core-smoke.sh` hardcodes `QEMU_SMP=1`. This line must not be
changed to allow SMP. x86_64 SMP is out of scope until the trampoline assembly is
separated from `src/arch/x86_64/smp.rs`.

### 5.2 x86_64 SMP TODO

Before enabling x86_64 SMP smoke: split the AP trampoline assembly stub from the
Rust SMP initialization logic in `src/arch/x86_64/smp.rs`. The current file mixes
low-level 16-bit/32-bit trampoline code with Rust AP bringup. A clean split is
required before SMP can be tested in CI.

### 5.3 AArch64 user ELFs must keep 4 KiB LOAD alignment

`targets/aarch64-yarm-user-none.json` must retain:
```json
"-zmax-page-size=0x1000",
"-zcommon-page-size=0x1000"
```
Removing these flags restores 64 KiB PT_LOAD alignment and breaks zero-copy loading
for all AArch64 late services.

### 5.4 Kernel page size remains 4 KiB

The kernel uses `PAGE_SIZE = 4096` on all supported architectures. Do not change
this constant. It affects the ZC feasibility check, the CPIO packer alignment target,
and the ELF LOAD alignment requirement simultaneously.

---

## 6. What Not to Do (Hard Constraints)

The following changes require an explicit Phase N+1 plan and are never acceptable
as incidental modifications:

| Prohibited action | Reason |
|-------------------|--------|
| Change syscall ABI (nr, arg layout, return layout) | Breaks all existing server binaries |
| Change SpawnV5 ABI | Breaks init_server and all spawn paths |
| Remove syscall 27 prematurely | Breaks Phase 2B fallback |
| Remove Phase 2B fallback from PM | Breaks old-kernel compatibility |
| Enable `VFS_READ_SHARED_REPLY_ENABLED` | Not validated; may cause IPC state corruption |
| Increase heap sizes | Violates memory budget; may cause OOM on constrained targets |
| Change kernel page size | Cascading breakage across ZC, CPIO, and ELF alignment |
| Change x86_64 SMP in smoke | x86_64 SMP not validated |
| Change service startup cap layout | Breaks slot assumptions in all service entry stubs |
| Map writable file-backed pages | Security invariant: initramfs data is read-only |
| Give child tasks MemoryObject caps | Violates isolation; child must not see initrd phys addrs |
| Use FLAG_CAP_TRANSFER for plain replies | Causes opcode-prefix stripping on reply body |
| Silently fall back on hard cap errors | Hides real bugs; violates Phase 3B acceptance criteria |

---

## 7. Log Marker Reference (Phase 3B Baseline)

The following kernel/server log markers are part of the Phase 3B acceptance
contract. Smoke scripts check these; do not rename or remove them without updating
both the smoke scripts and this document.

| Marker | Source | Meaning |
|--------|--------|---------|
| `PM_VFS_GRANT_RO_BEGIN image_id=N` | PM | Before `VFS_OP_FILE_GRANT_RO` send |
| `VFS_FILE_GRANT_RO_FORWARD` | VFS | VFS routes to backend |
| `INITRAMFS_FILE_GRANT_RO_REPLY` | initramfs_srv | After nr=28 succeeds |
| `PM_VFS_GRANT_RO_RECEIVED image_id=N` | PM | MemoryObject cap received |
| `PM_SPAWN_FROM_MO_DONE image_id=N` | PM | nr=29 returned success |
| `ZC_FEASIBILITY image_id=N feasible=true/false` | kernel | ZC pre-check result |
| `ZC_SEG_BEGIN image_id=N seg=K` | kernel | Per-LOAD-segment entry |
| `ZC_PAGE image_id=N seg=K reason=...` | kernel | Per-page decision |
| `ZC_SEG_DONE image_id=N seg=K mapped=N copied=N` | kernel | Segment summary |
| `PM_ELF_ZC_DONE image_id=N zc_pages=M copied_pages=K` | kernel | Load complete |
| `PM_ELF_ZC_FAIL image_id=N reason=...` | PM/kernel | ZC load error (must be 0) |
| `INITRAMFS_SRV_ENTRY` | initramfs_srv | Service started (must be exactly once) |
| `DEVFS_SRV_ENTRY` | devfs_srv | Service started (must be exactly once) |
| `VFS_SRV_ENTRY` | vfs_server | Service started (must be exactly once) |
| `DRIVER_MANAGER_ENTRY` | driver_manager | Service started (must be exactly once) |
| `BLKCACHE_SRV_ENTRY` | blkcache_srv | Service started (must be exactly once) |
| `VIRTIO_BLK_SRV_ENTRY` | virtio_blk_srv | Service started (must be exactly once) |
| `DRIVER_MANAGER_READY` | driver_manager | Initialization complete (must be exactly once) |
| `BLKCACHE_SRV_READY` | blkcache_srv | Initialization complete (must be exactly once) |
| `VIRTIO_BLK_SRV_READY` | virtio_blk_srv | Initialization complete (must be exactly once) |

### Stage 88-91 optional-FS markers

| Marker | Source | Meaning |
|--------|--------|---------|
| `INIT_PM_RECV_DRAIN_BEGIN` | init | Before draining pm_recv of stale replies |
| `INIT_PM_RECV_DRAIN_DONE count=N` | init | After drain; N replies discarded |
| `INIT_RAMFS_SPAWN_BEGIN` | init | Before spawning ramfs_srv (image_id=11) |
| `INIT_RAMFS_SPAWN_OK child_tid=N` | init | ramfs_srv spawned successfully |
| `INIT_RAMFS_SPAWN_FAIL` | init | ramfs_srv spawn failed (error) |
| `RAMFS_SRV_ENTRY` | bin/ramfs_srv | ramfs_srv binary entry point reached |
| `RAMFS_MOUNT_READY` | ramfs/service | RAMFS service loop ready; mount registered |
| `VFS_MOUNT_REGISTER_RAMFS_OK` | init | VFS accepted RAMFS mount registration |
| `INIT_EXT4_SPAWN_BEGIN` | init | Before spawning ext4_srv (image_id=12) |
| `INIT_EXT4_SPAWN_OK child_tid=N` | init | ext4_srv spawned successfully |
| `INIT_EXT4_SPAWN_FAIL` | init | ext4_srv spawn failed (error) |
| `EXT4_SRV_ENTRY` | bin/ext4_srv | ext4_srv binary entry point reached |
| `EXT4_SRV_READY` | ext4/service | ext4_srv service loop ready |
| `VFS_MOUNT_REGISTER_EXT4_OK` | init | VFS accepted EXT4 mount registration |
| `INIT_FAT_SPAWN_BEGIN` | init | Before spawning fat_srv (image_id=10) |
| `INIT_FAT_SPAWN_SKIPPED reason=profile_disabled` | init | FAT skipped: no virtio_blk in profile |
| `INIT_FAT_SPAWN_SKIPPED reason=server_disabled` | init | FAT skipped: INIT_SPAWN_FAT_SRV=false |
| `INIT_FAT_SPAWN_OK child_tid=N` | init | fat_srv spawned (only if virtio_blk present) |
| `FAT_MOUNT_READY` | fat/service | FAT service loop ready; mount registered |
| `VFS_MOUNT_REGISTER_FAT_OK` | init | VFS accepted FAT mount registration |

---

## 8. Reply-Endpoint Hygiene Rule

### 8.1 Do not reuse a shared reply endpoint across protocol phases without consuming all prior replies

A shared reply endpoint (e.g., `pm_recv`) must have all pending replies consumed or explicitly
drained before it is used for a new round of protocol traffic. A timeout/deadline-0 receive
(`ipc_recv_with_deadline(ep, 0)`) is a poll that returns `None` if no message is pending at that
instant. It does NOT guarantee the endpoint is clean — a reply arriving milliseconds later will
be misinterpreted as the next operation's reply.

**Correct pattern for shared endpoints:**
1. If a service helper sends a non-blocking poll to a shared endpoint, drain the endpoint
   exhaustively before starting the next protocol phase.
2. The `INIT_PM_RECV_DRAIN_BEGIN` / `INIT_PM_RECV_DRAIN_DONE` pattern in `init/service.rs` is
   the reference implementation.

**Correct pattern for per-operation replies:**
Use a dedicated reply-recv cap (not the shared endpoint) for protocol phases that need a clean
reply window. The ext4 and FAT mount-register helpers each use their own `reply_recv_cap`
argument, not `pm_recv`.

**Forbidden:** Reusing `pm_recv` for a VFS mount-register reply without draining it first.
This was the root cause of the "stale 32-byte blkcache reply misinterpreted as 16-byte SpawnV5
reply" bug fixed in commit 234aed2.

### 8.2 Deadline-0 receives on dedicated caps are safe

A deadline-0 receive on a dedicated per-operation reply cap is safe: only the current
operation's reply can arrive on that cap. The drain pattern is only required for shared
endpoints.

---

## 9. Initramfs Path Table Rule

Every server staged in the CPIO archive must be:
1. Present in the CPIO with 4 KiB-aligned file data (`scripts/pack-initramfs-aligned.py`).
2. Registered in `spawn_image_path_for_image_id()` so the kernel path table can map
   image_id → CPIO path.
3. Registered in `InitramfsBackend` inode/path table (`archive.rs`) so VFS can open/statx
   it at `/initramfs/sbin/<name>`.
4. Present in the `from_cpio_newc()` match arm so that live CPIO images update the synthetic
   inode's `file_len`.
5. Covered by an `openat_path`/`statx_path` test through the `InitramfsBackend` API.

**The ext4_srv regression (fixed in commit 690951b):** `sbin/ext4_srv` was staged in CPIO and
registered in the kernel path table, but the `InitramfsBackend` inode table had only 13 slots
— `ext4_srv` was missing. VFS returned `NotFound` when PM tried to open
`/initramfs/sbin/ext4_srv`, causing `PM_ELF_ZC_FAIL` on every ext4_srv spawn attempt.

When adding a new sbin server: bump `MAX_INITRAMFS_INODES`, add the inode, add the
`from_cpio_newc` match arm, and add a path test.

---

## 10. VFS Client Blocking-Receive Rule (Stage 92)

### 10.1 vfs_client.rs IPC helpers MUST use blocking `ipc_recv_v2`

All four IPC helpers in `crates/yarm-user-rt/src/vfs_client.rs`
(`vfs_statx`, `vfs_openat`, `vfs_read`, `vfs_close`) MUST use `ipc_recv_v2(reply_recv_cap)`
(blocking) to consume the VFS server's reply.

**Forbidden:** Using `ipc_recv_with_deadline(reply_recv_cap, 0)` (non-blocking poll)
in any of these helpers.

**Root cause (Stage 92 AArch64 wrong-sender race):**
On AArch64, VFS scheduling is slower than on x86_64. When init calls `vfs_statx` or
`vfs_openat` with a deadline-0 receive, VFS has not yet replied. The helper returns
`Err(NoReply)` but VFS's reply is still in flight and arrives later — at the shared
`pm_recv` endpoint (`E_init_reply`). The pre-spawn drain loop (also using deadline-0)
misses these delayed replies if they arrive after the loop completes. The next
`ipc_recv_v2` call inside `spawn_v5_cap` then receives 1–3 of those 8-byte VFS replies
before the real 16-byte SpawnV5 reply, logging `INIT_SPAWN_V5_WRONG_SENDER_REPLY` ×1–3.

**Fix:** Replace `ipc_recv_with_deadline(reply_recv_cap, 0)` with `ipc_recv_v2(reply_recv_cap)`
and change the match arm from `Ok(Some(ref r)) => decode_reply_u64(r)` to
`Ok(Some(ref received)) => decode_reply_u64(&received.message)` (since `ipc_recv_v2`
returns `ReceivedMessage`, not `Message` directly).

### 10.2 spawn_v5_cap wrong-sender drain loop is defense-in-depth only

The drain loop added in Stage 91 (logs `INIT_SPAWN_V5_WRONG_SENDER_REPLY`) must remain
as defense-in-depth. With the Stage 92 fix applied, this loop should fire 0 times in a
clean run. Do NOT remove the loop.

### 10.3 Smoke script strict mode must enforce count=0

Both `scripts/qemu-aarch64-optional-fs-smoke.sh` and `scripts/qemu-x86_64-optional-fs-smoke.sh`
must check `INIT_SPAWN_V5_WRONG_SENDER_REPLY` count and fail when `QEMU_SMOKE_STRICT=1`
and count > 0. This is the acceptance criterion for Stage 92.

---

## 11. Official FS Profile Matrix (Stage 93)

### 11.1 Profile definitions

| Profile | RAMFS | ext4 | FAT | Block device | Default? |
|---------|-------|------|-----|--------------|----------|
| `core` | disabled | disabled | disabled | none | no |
| `optional-fs` | ✓ live | ✓ read-only | disabled | none | **YES** |
| `fat-block` | ✓ live | ✓ read-only | ✓ read-only | virtio-blk required | no |
| `full-fs-experimental` | ✓ | ✓ | ✓ (future) | virtio-blk | future |

**Current default is `optional-fs`:** RAMFS + ext4 live, FAT disabled.

### 11.2 Gate constants per profile

| Constant | optional-fs | fat-block |
|----------|-------------|-----------|
| `INIT_SPAWN_RAMFS_SRV` | `true` | `true` |
| `INIT_SPAWN_FAT_SRV` | **`false`** | `true` |
| `INIT_SPAWN_EXT4_SRV` | `true` | `true` |
| `VFS_FAT_LIVE_MOUNT_ENABLED` | **`false`** | `true` |
| `VFS_FAT_SHARED_IO_ENABLED` | **`false`** | **`false`** (until read proven) |
| `VFS_EXT4_LIVE_MOUNT_ENABLED` | `true` | `true` |
| `VFS_RAMFS_LIVE_MOUNT_ENABLED` | `true` | `true` |

### 11.3 Forbidden patterns in smoke strict mode (`QEMU_SMOKE_STRICT=1`)

All profiles: `INIT_SPAWN_V5_WRONG_SENDER_REPLY`, `KSPAWN_EXTRA_CAP_DELEGATE_FAIL`,
`PM_VFS_SPAWN_FAIL`, `reason=bad_fd_decode`, `panic`.

`optional-fs` profile (FAT disabled): `INIT_FAT_SPAWN_OK` must be absent.

`fat-block` profile: `INIT_FAT_SPAWN_FAIL`, `FAT_MOUNT_FAILED`, `PM_ELF_ZC_FAIL image_id=10`.

### 11.4 IpcBlockDevice blocking-receive rule

`IpcBlockDevice::read_exact_at` and `write_sector` in `crates/yarm-fs-servers/src/fs/fat/fs.rs`
MUST use `ipc_recv_v2` (blocking). Using `ipc_recv_with_deadline(_, 0)` is a
scheduling race (same root cause as Stage 92's `vfs_client.rs` fix) and will cause
`FatError::Io` on any architecture where blkcache_srv hasn't replied within 0 ticks.

This was fixed in Stage 93 as part of FAT production groundwork.

---

## 12. Optional FS Milestone 1 (Stage 94/100)

### 12.1 Milestone declaration

**YARM Optional FS Milestone 1 is declared at Stage 100.**

The milestone is reached when:
- RAMFS + ext4 optional-FS baseline is clean (strict smoke passes).
- FAT is profile-ready but disabled, with exact blockers documented.
- No yarm-fs-servers or yarm-control-plane-servers failures.
- Docs/rules are current.
- Kernel unlocking handoff seed is written.

See `doc/OPTIONAL_FS_MILESTONE_1.md` for the full milestone record.

### 12.2 Filesystem work is paused after Milestone 1

Do NOT start new FS feature work after Stage 100 without opening a new dedicated
FS milestone. Permitted after Stage 100:
- Regressions only (RAMFS/ext4 existing behavior).
- Emergency FAT gate doc updates (no code changes).
- Smoke script fixes (not feature additions).

### 12.3 Fatal smoke greps must exclude nonfatal=true (Stage 94)

The `panic` grep in optional-FS smoke scripts must not match log lines that also
contain `nonfatal=true`. Those lines represent non-fatal diagnostic events, not
kernel or userspace panics.

**Correct pattern:**
```bash
# Two-stage: find panic lines, then exclude nonfatal ones
panic_count=$(tr '\r' '\n' <"$LOGFILE" | rg -ai "\bpanic\b" 2>/dev/null \
    | rg -avc "nonfatal=true" 2>/dev/null || echo 0)
```

**Forbidden:**
```bash
# Single-stage: matches nonfatal=true lines
panic_count=$(... | rg -ai -c "\bpanic\b" 2>/dev/null || echo 0)
```

### 12.4 Kernel unlocking handoff

See `doc/KERNEL_UNLOCKING_NEXT_CONTEXT.md` for the full handoff context.
Recommended next stage: **Stage 101 — Kernel unlocking restart / trap-syscall borrow audit**.

Key invariants that kernel unlocking must not break:
- SpawnV5 ABI (16-byte reply, argument layout)
- Image IDs 7–12 frozen
- SYSCALL_COUNT = 31, STARTUP_SLOT_COUNT = 18
- recv_shared_v3 ABI offsets
- Optional-FS smoke markers (RAMFS/ext4 expected; FAT skipped)
- No deadline-0 required replies in vfs_client.rs or IpcBlockDevice
- VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false
