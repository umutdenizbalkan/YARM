# Phase 2B Milestone: VFS-Mediated Transfer-Buffer Bulk Read

**Status:** ✅ Runtime-proven on AArch64 QEMU (`virt`, Cortex-A72, 2-SMP, 1 GiB)  
**Frozen:** 2026-05-27

---

## Overview

Phase 2B completes the bulk-read path for loading kernel images (image_id 7/8/9) from
the initramfs CPIO archive. PM (Process Manager, TID=3) fetches each ELF binary through
VFS (Virtual File System service), which routes the request to initramfs_srv (TID=5), which
uses a kernel-assisted cross-ASID copy to fill PM's 4 KiB stack transfer buffer directly.

---

## Phase 2A: Direct Syscall Bootstrap Bridge (Superseded)

**Phase 2A** was the first working solution, implemented as an emergency bridge while the
VFS IPC routing was not yet wired for bulk reads. It remains in the codebase as a fallback.

### How it worked

PM called syscall `nr=27` (`InitramfsReadChunk`) **directly** from user space, bypassing VFS:

```
PM (TID=3) ──[nr=27, arg5=0]──▶ kernel ──▶ CPIO lookup ──▶ PM address space
```

- `arg5 = 0`: copy into **caller's own** address space (self-ASID)
- The syscall reads one ≤4096-byte chunk per call, advancing `offset` each iteration
- PM kept a per-chunk loop (`PM_VFS_READ_BULK_PHASE2A_BEGIN`) to accumulate reads

### Why it was temporary

Phase 2A bypasses the VFS mount table entirely. Initramfs_srv is the owner of all CPIO
data; PM having direct kernel access to the CPIO is an architectural shortcut that:

1. Couples PM to kernel CPIO layout — VFS abstraction is defeated
2. Requires PM to know CPIO names by convention, not by VFS open/read/close
3. Cannot generalize to block-device-backed filesystems (Phase 3+)

Phase 2A is retained as an `InvalidArgs` fallback only (in case an older kernel lacks nr=27
support). In the Phase 2B frozen state, PM_VFS_READ_BULK_PHASE2A_BEGIN count must be 0.

---

## Phase 2B: VFS-Mediated Transfer-Buffer Bulk Read

Phase 2B routes all bulk reads through the VFS service, restoring the correct architectural
layering while still using a kernel-assisted cross-ASID copy as the data transport.

### IPC call graph

```
PM (TID=3)                 VFS (TID=4)             initramfs_srv (TID=5)
    │                           │                           │
    │── VFS_OP_READ_BULK ──────▶│                           │
    │   BulkReadArgs {          │── VFS_OP_READ_BULK ──────▶│
    │     fd, requested_len,    │   (forwarded as-is)       │
    │     offset,               │                           │── nr=27 (arg5=PM_TID=3) ──▶ kernel
    │     dst_ptr = &bulk_buf   │                           │   cross-ASID copy into
    │   }                       │                           │   PM's bulk_buf at dst_ptr
    │                           │                           │
    │                           │◀── BulkReadReply ─────────│
    │◀── BulkReadReply ─────────│   { copied_len, eof }     │
    │   { copied_len, eof }     │                           │
```

### Data transport: kernel cross-ASID copy (nr=27, arg5=target_tid)

The critical enabler is syscall `nr=27` (`InitramfsReadChunk`) with `arg5 = PM_TID (3)`.
When `arg5 != 0`, the kernel copies the CPIO data directly into the **target task's** address
space (PM's ASID) at the user VA carried in arg3 (`dst_ptr`).

This is **not zero-copy** — the data moves:
```
CPIO (in initramfs_srv ASID)  ──kernel nr=27──▶  PM's bulk_buf[] (in PM ASID)
```

Both a CPIO-to-kernel-stack copy and a kernel-stack-to-PM-page copy occur inside the syscall.
See [§ Why Not Zero-Copy](#why-not-zero-copy) below.

### Wire formats

**`BulkReadArgs`** (32 bytes, 4 × u64 LE):

| Offset | Field          | Description                             |
|--------|----------------|-----------------------------------------|
| 0      | `fd`           | VFS file descriptor (opened by PM)      |
| 8      | `requested_len`| Max bytes to copy this call (≤ 4096)    |
| 16     | `offset`       | File offset to read from                |
| 24     | `dst_ptr`      | PM's VA of `bulk_buf[4096]` on stack    |

**`BulkReadReply`** (12 bytes):

| Offset | Field         | Description                                   |
|--------|---------------|-----------------------------------------------|
| 0      | `copied_len`  | Bytes copied into PM's buffer (u64 LE)        |
| 8      | `eof`         | 1 if end-of-file reached (u8), else 0         |

**VFS opcode:** `VFS_OP_READ_BULK = 24`

### Access control

Syscall `nr=27` with `arg5 != 0` is restricted to `TaskClass::SystemServer` tasks only.
Non-system-server tasks receive `MissingRight` immediately (`INITRAMFS_READ_CHUNK_DENIED`
logged). Only initramfs_srv (a system server) may call it with `arg5 = PM_TID = 3`.

### Lifecycle of a single image load (image_id 7)

1. PM opens the ELF file via VFS: `VFS_OP_OPENAT /initramfs/sbin/virtio_blk` → fd
2. PM calls `pm_read_all_via_vfs_bulk(fd, image_id=7)`:
   - Allocates `bulk_buf[4096]` on its stack
   - Loops calling VFS with `VFS_OP_READ_BULK { fd, 4096, offset, &bulk_buf }`
   - Each iteration: VFS routes to initramfs_srv, which calls `nr=27 (arg5=3)` to fill `bulk_buf`
   - PM receives `BulkReadReply { copied_len, eof }`, appends `bulk_buf[..copied_len]` to its image accumulator
   - Loop terminates on `eof=true` or `copied_len=0`
3. PM logs `PM_VFS_READ_BULK_DONE image_id=7 total=<N> mode=vfs_transfer`
4. PM spawns the loaded ELF via `SpawnV5`

---

## Runtime Acceptance Proof (AArch64, 2026-05-27)

Confirmed from AArch64 QEMU smoke log:

```
[ok] Phase 2B: PM_VFS_READ_BULK_DONE image_id=7 mode=vfs_transfer count=1
[ok] Phase 2B: PM_VFS_READ_BULK_DONE image_id=8 mode=vfs_transfer count=1
[ok] Phase 2B: PM_VFS_READ_BULK_DONE image_id=9 mode=vfs_transfer count=1
[ok] Phase 2A bridge not triggered (PM_VFS_READ_BULK_PHASE2A_BEGIN=0)
[ok] no PM_VFS_READ_BULK_FAIL reason=not_found
[ok] PM_VFS_READ_BULK_DONE: mode=vfs_transfer=3 mode=phase2a_bridge=0 total=3
[ok] absent marker confirmed: VFS_FORWARD_BULK_READ     ← trace-gated
[ok] absent marker confirmed: VFS_ROUTE_BULK_REPLY      ← trace-gated
[ok] absent marker confirmed: INITRAMFS_READ_BULK       ← trace-gated
[ok] absent marker confirmed: INITRAMFS_READ_BULK_REPLY ← trace-gated
[ok] INITRAMFS_SRV_ENTRY=1 DEVFS_SRV_ENTRY=1 VFS_SRV_ENTRY=1
[ok] DRIVER_MANAGER_ENTRY=1 BLKCACHE_SRV_ENTRY=1 VIRTIO_BLK_SRV_ENTRY=1
[ok] DRIVER_MANAGER_READY=1 BLKCACHE_SRV_READY=1 VIRTIO_BLK_SRV_READY=1
[ok] BAD / BOOT BLOCKERS: empty
```

---

## Why Not Zero-Copy

Phase 2B is transfer-buffer bulk read, **not** zero-copy. Data still traverses:

1. CPIO byte slice in initramfs_srv ASID (read-only kernel mapping of initrd)
2. Kernel `copy_slice_to_task()` intermediate stack buffer
3. PM's `bulk_buf[4096]` stack buffer (via cross-ASID page-table walk + copy)
4. PM's heap-allocated `image_bytes` accumulator (another memcpy)

The reason zero-copy is not yet possible:

> **Missing primitive:** `MemoryObject` page-capability grant.
>
> Zero-copy would require initramfs_srv to grant PM a read-only capability to a specific
> physical page (or `MemoryObject`) containing the CPIO file data, which PM could then
> directly map into its address space. This requires:
>
> - A `MemoryObject` kernel object (wrapping a PFN or contiguous region)
> - A `GrantCap` / `DelegateCap` mechanism to transfer page rights across task boundaries
> - PM-side `mmap`-equivalent to accept the grant and map it into its ASID

These are Phase 3 deliverables.

---

## Trace Gates

Two `bool` constants control hot-path log verbosity. Both default to `false`.

| Constant | File | Controls |
|----------|------|----------|
| `VFS_BULK_READ_TRACE` | `vfs/service.rs` | `VFS_FORWARD_BULK_READ`, `VFS_ROUTE_BULK_REPLY` |
| `INITRAMFS_READ_BULK_TRACE` | `initramfs/service.rs` | `INITRAMFS_READ_BULK`, `INITRAMFS_READ_BULK_REPLY` |

**Always-on logs** (never gated, important for debugging failures):
`PM_VFS_READ_BULK_BEGIN`, `PM_VFS_READ_BULK_DONE`, `PM_VFS_READ_DONE`,
`PM_VFS_SPAWN_FROM_VFS_BYTES`, `PM_VFS_READ_BULK_FAIL`,
`VFS_BULK_READ_DENIED`, `INITRAMFS_READ_BULK_FAIL`, `INITRAMFS_READ_BULK_BAD_LEN`,
`INITRAMFS_READ_BULK_BAD_BUFFER`, `INITRAMFS_READ_CHUNK_DENIED`, `INITRAMFS_READ_CHUNK_NOT_FOUND`

---

## Phase 3: Remaining Work

Phase 3 will implement true zero-copy ELF loading:

1. **`MemoryObject` kernel object** — wraps a PFN range or `&'static [u8]` slice
2. **`GrantCap` syscall** — transfers a read-only `MemoryObject` capability from initramfs_srv → PM
3. **PM-side `accept_page_grant`** — maps the granted pages into PM's ASID (or reads directly from cap)
4. **Remove initramfs_write_to_pm_buf bridge** — syscall `nr=27` with `arg5 != 0` can be retired once Phase 3 is in place. The Phase 2A/2B code paths in PM (`pm_read_all_via_vfs_bulk`) should eventually be replaced by a single zero-copy `accept_grant` call per image.
5. **VFS_OP_MMAP_GRANT opcode** (tentative) — extends VFS protocol for capability transfer

### Removal gate for nr=27 (arg5 != 0)

Syscall `nr=27 InitramfsReadChunk` with `arg5 = PM_TID (3)` is a **Phase 2 bootstrap bridge**
and must be removed before any Phase 3 stabilisation. The gate condition is:

> Phase 3 `MemoryObject` page-cap grant is implemented, tested, and confirmed to replace
> the transfer-buffer path for all three image_ids (7, 8, 9) with `PM_VFS_READ_BULK_DONE
> mode=zero_copy` in the AArch64 QEMU smoke.

See `doc/SYSCALL_ABI.md § 27 InitramfsReadChunk` for the full access-gate specification.
