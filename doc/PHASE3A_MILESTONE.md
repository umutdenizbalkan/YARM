# Phase 3A Milestone: InitramfsFileSlice MemoryObject Cap Grant

**Status:** 🔧 Implementation complete — pending QEMU runtime confirmation  
**Branch:** `claude/vigilant-allen-pqENU`  
**Date:** 2026-05-27

---

## Overview

Phase 3A extends the microkernel with the infrastructure needed for true zero-copy ELF loading from the boot initramfs CPIO archive. PM (Process Manager, TID=3) now attempts to spawn image_id 7/8/9 via a MemoryObject capability grant instead of the Phase 2B transfer-buffer copy.

**Hard constraints preserved (unchanged from Phase 2B):**
- VFS_READ_SHARED_REPLY_ENABLED NOT enabled
- Phase 2B transfer-buffer path NOT removed
- Syscall 27 NOT removed
- SpawnV5 ABI NOT changed
- No heap size increase
- No generic writable shared memory
- No child MemoryObject caps
- Only PM may spawn from MemoryObject caps
- File-backed MemoryObject mappings are read-only in Phase 3A

---

## Architecture

### New Kernel Objects

**`MemoryObjectKind`** (`src/kernel/boot/defs.rs`):
```rust
pub(crate) enum MemoryObjectKind {
    Anonymous,
    InitramfsFileSlice { initrd_offset: u64, file_len: u64 },
}
```

InitramfsFileSlice MemoryObjects are backed by a read-only slice of the boot initrd mapping. They have `READ | MAP` rights (no WRITE).

### New Syscalls

| Nr | Name | Access | Description |
|----|------|--------|-------------|
| 28 | `CreateInitramfsFileSliceMo` | SystemServer only | Create read-only MO for named CPIO file |
| 29 | `SpawnFromMemoryObject` | PM (TID=3) only | Spawn process from InitramfsFileSlice MO cap |

### New VFS Operation

**`VFS_OP_FILE_GRANT_RO = 25`** — PM sends this to VFS with an open fd; VFS routes to the backend (initramfs_srv) which calls syscall nr=28 and replies with a transferred MemoryObject cap.

---

## IPC Call Graph (Phase 3A)

```
PM (TID=3)                     VFS (TID=4)             initramfs_srv (TID=5)
    │                               │                           │
    │── VFS_OP_OPENAT ─────────────▶│── VFS_OP_OPENAT ─────────▶│
    │◀── fd ────────────────────────│◀── fd ────────────────────│
    │                               │                           │
    │── VFS_OP_FILE_GRANT_RO(fd) ──▶│── VFS_OP_FILE_GRANT_RO ──▶│
    │   (FileGrantRoArgs{fd})        │   (forwarded)             │── nr=28 (CreateInitramfsFileSliceMo)
    │                               │                           │   ↓ returns (mo_id, cap_id)
    │                               │◀── reply+cap ─────────────│
    │◀── reply+cap ─────────────────│   FLAG_CAP_TRANSFER       │
    │   FLAG_CAP_TRANSFER            │   (cap transparently      │
    │   transferred_cap=mo_cap       │    forwarded by ipc_reply)│
    │                               │                           │
    │── nr=29 (SpawnFromMemoryObject) ──▶ kernel                │
    │   (image_id, mo_cap, ...)     │                           │
    │◀── (tid, caller_cap, spawner_cap) ─────────────────────── │
```

### Cap Transfer Path

The MemoryObject cap travels through the existing IPC cap-transfer machinery:
1. initramfs_srv: `ipc_reply(reply_cap, msg_with_FLAG_CAP_TRANSFER)` → kernel stashes cap
2. VFS: `ipc_recv_v2(backend_reply_recv_cap)` → materializes cap into VFS cspace → returned in `received.transferred_cap`
3. VFS: `ipc_reply(client_reply_cap, &response)` where `response` carries the cap via `FLAG_CAP_TRANSFER`
4. PM: `pm_vfs_call_full(...)` returns `(Message, Option<u32>)` with the cap

---

## Zero-Copy ELF Loader

**`load_elf_with_mo_zero_copy`** (`src/kernel/boot/exec_state.rs`):

1. Checks `zc_feasible = (initrd_phys_base + file_initrd_offset) & (PAGE_SIZE-1) == 0`
2. If NOT feasible (typical CPIO — headers cause misalignment): delegates to existing `load_elf_pt_load_segments`, returns `zc_pages=0`
3. If feasible: per-page ZC decision
   - RO + fully-in-file + page-aligned phys → map initrd phys directly (`zc_pages++`)
   - Writable/BSS/partial pages → alloc anon frame + copy (`copied_pages++`)

**Current behavior with CPIO:** CPIO file data offsets are NOT page-aligned (CPIO header + filename padding precedes the data). Therefore `zc_feasible=false` in practice, and `zc_pages=0` with all pages copied via the existing loader. The infrastructure is correct and will deliver true ZC when a page-aligned initramfs format is used.

---

## Phase 3A Boot Sequence (image_id 7)

1. PM opens file: `VFS_OP_OPENAT /initramfs/sbin/driver_manager` → fd
2. PM sends `VFS_OP_FILE_GRANT_RO { fd }` to VFS
3. VFS routes to initramfs_srv via fd-table lookup
4. initramfs_srv calls `nr=28 CreateInitramfsFileSliceMo(cpio_name)` → (mo_id, cap_id)
5. initramfs_srv replies with `FLAG_CAP_TRANSFER` + cap_id + file_len
6. VFS receives cap → materializes into VFS cspace → relays with `FLAG_CAP_TRANSFER`
7. PM receives cap in `received.transferred_cap`
8. PM calls `nr=29 SpawnFromMemoryObject(image_id=7, mo_cap)`
9. Kernel resolves MO cap → checks `InitramfsFileSlice` kind → calls `load_elf_with_mo_zero_copy`
10. Kernel spawns task, returns `(tid, caller_cap, spawner_cap)`
11. PM logs `PM_ELF_ZC_DONE image_id=7 zc_pages=0 copied_pages=N`

---

## Required Log Markers

| Marker | Source | When emitted |
|--------|--------|--------------|
| `PM_VFS_GRANT_RO_BEGIN image_id=N` | PM service.rs | Before FILE_GRANT_RO send |
| `VFS_FILE_GRANT_RO_FORWARD fd=N target=...` | VFS service.rs | VFS routes to backend |
| `INITRAMFS_FILE_GRANT_RO_REPLY path=... len=... cap=...` | initramfs service.rs | After nr=28 succeeds |
| `PM_VFS_GRANT_RO_RECEIVED image_id=N len=M cap=C` | PM service.rs | After receiving MO cap |
| `PM_ELF_ZC_DONE image_id=N zc_pages=0 copied_pages=N` | kernel syscall.rs | After load_elf_with_mo_zero_copy |
| `PM_ELF_ZC_FAIL image_id=N reason=...` | PM/kernel | On error |

---

## Fallback Behavior

If `VFS_OP_FILE_GRANT_RO` returns opcode≠0 or no transferred cap (Unsupported):
- PM logs `PM_VFS_GRANT_RO_UNSUPPORTED image_id=N fallback=phase2b`
- Falls back to Phase 2B (VFS_OP_READ_BULK transfer-buffer path)
- Phase 2B emits `PM_VFS_READ_BULK_DONE mode=vfs_transfer`

Hard errors (NotFound, Malformed) → NO fallback.

---

## Smoke Acceptance Criteria

For each image_id 7, 8, 9:
- `PM_ELF_ZC_DONE image_id=N` appears exactly once (Phase 3A path), OR
- `PM_VFS_READ_BULK_DONE image_id=N mode=vfs_transfer` appears exactly once (Phase 2B fallback)

Global:
- No `IPC_RECV_CAP_MATERIALIZE_FAILED`
- No `CapabilityFull`
- BAD/BOOT BLOCKERS: empty
- All six ENTRY markers exactly once
- DRIVER_MANAGER_READY / BLKCACHE_SRV_READY / VIRTIO_BLK_SRV_READY exactly once

---

## Files Modified

| File | Change |
|------|--------|
| `src/kernel/boot/defs.rs` | Add `MemoryObjectKind` enum, `kind` field to `MemoryObject` |
| `src/kernel/boot/memory_state.rs` | Add `create_initramfs_file_slice_mo`, `normalize_initrd_phys_ptr_static` |
| `src/kernel/boot/exec_state.rs` | Add `count_elf_load_pages`, `load_elf_with_mo_zero_copy` |
| `src/kernel/syscall.rs` | Add syscalls nr=28, nr=29; SYSCALL_COUNT→30 |
| `crates/yarm-ipc-abi/src/vfs_abi.rs` | Add `VFS_OP_FILE_GRANT_RO=25`, `FileGrantRoArgs`, `FileGrantRoReply` |
| `crates/yarm-user-rt/src/lib.rs` | Add `create_initramfs_file_slice_mo`, `spawn_from_memory_object` wrappers |
| `crates/yarm-fs-servers/src/fs/initramfs/service.rs` | Add `VFS_OP_FILE_GRANT_RO` handler |
| `crates/yarm-control-plane-servers/src/control_plane/vfs/service.rs` | Add `VFS_OP_FILE_GRANT_RO` routing |
| `crates/yarm-control-plane-servers/src/control_plane/process_manager/service.rs` | Add Phase 3A path with Phase 2B fallback |
| `scripts/qemu-aarch64-core-smoke.sh` | Add Phase 3A acceptance checks |

---

## Phase 4: Remaining Work

Phase 4 will deliver true zero-copy (zc_pages > 0):
1. **Page-aligned CPIO format** or aligned file embedding (so `zc_feasible=true`)
2. **VFS_OP_MMAP_GRANT** (tentative) — extends VFS protocol for capability transfer at mmap level
3. **Remove initramfs_write_to_pm_buf bridge** — syscall nr=27 with `arg5 != 0` retired

The gate condition for nr=27 removal (arg5 != 0 path) remains:
> Phase 3A `InitramfsFileSlice` MemoryObject grant fully replaces transfer-buffer path for all image_ids with `PM_ELF_ZC_DONE zc_pages>0` in AArch64 QEMU smoke.
