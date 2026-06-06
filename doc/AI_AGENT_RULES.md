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
