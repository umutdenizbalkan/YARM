# Phase 3B Milestone: Page-Aligned Zero-Copy ELF Loading

**Status:** ✅ Runtime-proven on AArch64 QEMU and x86_64 QEMU (-smp 1)
**Branch:** `claude/elegant-cannon-gF61P`
**Date:** 2026-05-27

---

## Overview

Phase 3B makes the zero-copy ELF loader (`load_elf_with_mo_zero_copy`) deliver
`zc_pages > 0` for image_id 7/8/9 (sbin/driver_manager, sbin/blkcache_srv,
sbin/virtio_blk_srv). Phase 3A proved the full MemoryObject grant/spawn IPC path
but still reported `zc_pages=0` because two physical-alignment pre-conditions were
not yet met. Phase 3B satisfies both.

---

## Phase History

| Phase | Name | Key Deliverable | zc_pages |
|-------|------|----------------|---------|
| 2A | Direct syscall bridge | PM calls nr=27 directly, bypassing VFS | N/A (bulk copy) |
| 2B | VFS-mediated bulk read | PM reads via VFS → initramfs_srv → nr=27 cross-ASID copy | N/A (bulk copy) |
| 3A | MemoryObject cap grant | Full IPC path: PM→VFS→initramfs_srv→nr=28→cap→nr=29→ZC loader | 0 (alignment not yet met) |
| 3B | Aligned ZC | 4 KiB ELF LOAD alignment + page-aligned CPIO payloads | > 0 ✅ |

---

## Phase 2A: Direct Syscall Bootstrap Bridge (Superseded)

PM called syscall nr=27 (`InitramfsReadChunk`, `arg5=0`) directly, bypassing VFS
entirely. This read chunks into PM's own address space via a self-ASID copy.
Retained as an `InvalidArgs`/`Unsupported` fallback only. `PM_VFS_READ_BULK_PHASE2A_BEGIN`
count must be 0 in the Phase 3B freeze.

---

## Phase 2B: VFS-Mediated Transfer-Buffer Bulk Read (Superseded for 7/8/9)

PM reads each ELF via `VFS_OP_READ_BULK (24)`, VFS routes to initramfs_srv which
calls nr=27 with `arg5=PM_TID=3` (cross-ASID kernel copy into PM's `bulk_buf`).
Data path: CPIO slice → kernel stack → PM's 4 KiB stack buffer → PM heap accumulator.
**Not zero-copy.** Two full memcpys per chunk. Retained as the Phase 3A/3B fallback
on `Unsupported` response from `VFS_OP_FILE_GRANT_RO`. `PM_VFS_READ_BULK_DONE`
count for image_id 7/8/9 must be 0 in the Phase 3B freeze.

---

## Phase 3A: MemoryObject Cap Grant + ZC Loader (Proven Infra)

Phase 3A implemented the complete IPC infrastructure:

1. New kernel object `MemoryObjectKind::InitramfsFileSlice { initrd_offset, file_len }`.
2. Syscall nr=28 `CreateInitramfsFileSliceMo` — initramfs_srv creates a read-only
   MemoryObject cap for a named CPIO file.
3. Syscall nr=29 `SpawnFromMemoryObject` — PM spawns from a MemoryObject cap; kernel
   calls `load_elf_with_mo_zero_copy`.
4. `VFS_OP_FILE_GRANT_RO (25)` — PM→VFS→initramfs_srv IPC sequence transferring the
   MemoryObject cap via `FLAG_CAP_TRANSFER_PLAIN`.
5. `load_elf_with_mo_zero_copy` — per-page ZC decision engine.

**Phase 3A runtime proof:** `PM_ELF_ZC_DONE image_id=7/8/9` appeared exactly once
each, `PM_ELF_ZC_FAIL=0`, Phase 2B fallback=0 — **but zc_pages=0** because
`ZC_FEASIBILITY feasible=false` due to CPIO alignment.

---

## Phase 3B: Why zc_pages Was 0 After Phase 3A

The ZC feasibility check in `load_elf_with_mo_zero_copy` requires:

```
(initrd_phys_base + file_initrd_offset) % PAGE_SIZE == 0
```

There were two reasons this failed:

### Reason 1: CPIO newc aligns file data to 4 bytes, not 4096

The standard `cpio` tool writes:
```
[110-byte header][filename\0][pad to 4][file data][pad to 4]
```
File data starts at an arbitrary 4-byte boundary. The logged diagnostics showed:
```
image_id=7 file_off=0x39584 offset_in_page=1412  feasible=false
image_id=8 file_off=0xa3e8  offset_in_page=1000  feasible=false
image_id=9 file_off=0x2505c offset_in_page=92    feasible=false
```

### Reason 2: AArch64 user ELFs had PT_LOAD Align=0x10000 (64 KiB)

LLD defaults to `max-page-size=0x10000` for AArch64. This meant the ELF on-disk
`p_offset` was only aligned to 64 KiB, not 4 KiB, so even if the CPIO was fixed,
the per-page physical offset within a segment would not always be page-aligned.

The feasibility condition also requires per-page alignment:
```
(initrd_phys_of_page) % PAGE_SIZE == 0
```
where `initrd_phys_of_page = initrd_phys_base + file_initrd_offset + (va - p_vaddr + p_offset)`.
With 64 KiB alignment, gaps between segments are 64 KiB multiples, violating 4 KiB alignment
for intermediate pages.

---

## Phase 3B Fixes

### Fix A: Detailed ZC Decision Diagnostics (`load_elf_with_mo_zero_copy`)

Added `image_id: u64` parameter and per-function/segment/page diagnostics:

```
ZC_FEASIBILITY image_id=N initrd_phys_base=0x... file_initrd_offset=0x... feasible=true/false
ZC_FALLBACK    image_id=N reason=cpio_file_data_unaligned        ← only on !feasible
ZC_SEG_BEGIN   image_id=N seg=K p_offset=0x... p_vaddr=0x... p_filesz=N p_memsz=N p_flags=0x...
ZC_PAGE        image_id=N seg=K va=0x... reason=<one of seven values>
ZC_SEG_DONE    image_id=N seg=K mapped_pages=N copied_pages=N
```

Seven page-decision reasons:

| Reason | Condition | Action |
|--------|-----------|--------|
| `full_page_zc_ok` | RO, fully in file data, phys page-aligned | Map zero-copy |
| `writable_segment_copy` | W flag set | Alloc + copy |
| `partial_head_copy` | `va < p_vaddr` | Alloc + copy |
| `partial_tail_copy` | `va + PAGE < p_vaddr + p_filesz` crosses file end | Alloc + copy |
| `bss_copy` | `p_filesz=0` or `va >= p_vaddr + p_filesz` | Alloc anonymous zero |
| `elf_offset_unaligned` | phys not page-aligned | Alloc + copy |
| `wx_rejected` | W+X segment | Rejected before flag extraction |

### Fix B: 4 KiB PT_LOAD Alignment for AArch64 User ELFs

Added to `targets/aarch64-yarm-user-none.json`:
```json
"pre-link-args": {
  "gnu-lld": [
    "-Ttargets/aarch64-yarm-user-none.ld",
    "-zmax-page-size=0x1000",
    "-zcommon-page-size=0x1000"
  ]
}
```

Before: `LOAD Align 0x10000` (64 KiB inter-segment padding)
After:  `LOAD Align 0x1000`  (4 KiB inter-segment padding, matches kernel PAGE_SIZE)

Kernel target (`targets/aarch64-yarm-none.json`) and x86_64 targets unchanged.

### Fix C: Page-Aligned CPIO Packer

`scripts/pack-initramfs-aligned.py` is a Python 3 CPIO newc packer that inserts
zero-data padding entries (`._padNNNN\x00`, 10-byte name) before every ELF file
so its file data lands at a 4096-byte boundary. Additional non-ELF paths may still
be requested with `--align`.

Usage: `pack-initramfs-aligned.py <rootfs_dir> <output.cpio> [--align <additional-path>]...`

Padding math: given H = `round_up(110 + namesize_of_next_entry, 4)` (header+name
overhead of the next real entry):
```python
target_data_pos = round_up(current_pos + H, PAGE_ALIGN)
target_pos_after_pad = target_data_pos - H
needed = target_pos_after_pad - current_pos
data_size = needed - PAD_HEADER_OVERHEAD   # PAD_HEADER_OVERHEAD = 120 bytes
```

Prints `ALIGN_PROOF path=<p> data_offset=<N> alignment_mod=<N> aligned=<true|false>`
to stderr for every ELF. It exits nonzero if any aligned payload fails validation.

`common_create_initramfs_aligned()` in `scripts/lib/build-qemu-artifacts-common.sh`
uses automatic ELF detection rather than a fixed late-service list. Missing Python
or a missing packer is a hard error; the QEMU path does not fall back to an
unaligned archive.

The x86_64, AArch64, and RISC-V QEMU artifact scripts all call
`common_create_initramfs_aligned`.

---

## Runtime IPC Path (Phase 3A/3B combined)

```
PM (TID=3)                     VFS (TID=4)             initramfs_srv (TID=5)
    │                               │                           │
    │── VFS_OP_OPENAT ─────────────▶│── VFS_OP_OPENAT ─────────▶│
    │◀── fd ────────────────────────│◀── fd ────────────────────│
    │                               │                           │
    │── VFS_OP_FILE_GRANT_RO(fd) ──▶│── VFS_OP_FILE_GRANT_RO ──▶│
    │   (FileGrantRoArgs{fd})        │   (forwarded)             │── nr=28 CreateInitramfsFileSliceMo
    │                               │                           │       → (mo_id, cap_id)
    │                               │◀── ipc_reply + cap ───────│
    │                               │   FLAG_CAP_TRANSFER_PLAIN  │
    │◀── ipc_reply + cap ───────────│
    │   FLAG_CAP_TRANSFER_PLAIN      │
    │   transferred_cap=mo_cap       │
    │                               │
    │── nr=29 SpawnFromMemoryObject ─────────────▶ kernel
    │   (image_id, mo_cap, ...)
    │          kernel: resolve MO cap → InitramfsFileSlice
    │          kernel: load_elf_with_mo_zero_copy(image_id, ...)
    │          kernel: per-page ZC decision (map or copy)
    │◀── (tid, caller_cap, spawner_cap) ──────────
    │
    PM logs: PM_ELF_ZC_DONE image_id=N zc_pages=M copied_pages=K
```

### Cap Transfer Detail

1. initramfs_srv: `ipc_reply(reply_cap, msg with FLAG_CAP_TRANSFER_PLAIN)` — kernel
   stashes the MemoryObject cap.
2. VFS receives in `ipc_recv_v2` → materializes cap into VFS cspace →
   `received.transferred_cap = Some(local_cap)`.
3. VFS: `ipc_reply(client_reply_cap, &response with FLAG_CAP_TRANSFER_PLAIN)` — kernel
   relays to PM.
4. PM: receives `transferred_cap` from `pm_vfs_call_full(...)`.

`FLAG_CAP_TRANSFER_PLAIN = 1 << 2` — avoids the opcode-stripping behaviour of the
older `FLAG_CAP_TRANSFER` path.

---

## Zero-Copy Safety Rules

These are hard kernel invariants enforced in `load_elf_with_mo_zero_copy`:

1. **Only full, non-writable PT_LOAD pages may be mapped zero-copy.** Read-only pages
   from the initramfs backing are mapped directly via the MemoryObject physical base.

2. **Writable segments (W flag) always copy.** Every W page gets a fresh anonymous
   physical frame and the file data (or BSS zeros) is copied in.

3. **BSS region (p_memsz > p_filesz) is anonymous zero.** Pages beyond `p_filesz`
   are allocated as fresh zeroed frames, never backed by initramfs data.

4. **Partial head/tail pages copy.** The first page of a LOAD segment (`va < p_vaddr`)
   and the last page where file data ends mid-page (`va + PAGE > p_vaddr + p_filesz`)
   are copied, never mapped from the archive.

5. **W+X segments are rejected.** Any segment with both W and X flags causes an error
   before page-flag extraction — no mapping is attempted.

6. **Child tasks do not receive MemoryObject caps.** Only PM (TID=3) may call syscall
   nr=29 `SpawnFromMemoryObject`. Spawned child tasks receive only the standard startup
   cap layout; no MemoryObject cap is delegated to them.

---

## Runtime Acceptance Proof

### Cross-architecture summary (2026-05-27)

| Metric | AArch64 | x86_64 (-smp 1) |
|--------|---------|-----------------|
| ZC done | 3 | 3 |
| ZC nonzero pages | 3 | 3 |
| ZC fail | 0 | 0 |
| Phase 2B fallback | 0 | 0 |
| Full-page ZC pages | 8 | 8 |

### x86_64 per-image detail

| image_id | binary | zc_pages | copied_pages |
|----------|--------|----------|--------------|
| 7 | sbin/driver_manager | 2 | 2 |
| 8 | sbin/blkcache_srv | 4 | 2 |
| 9 | sbin/virtio_blk_srv | 2 | 2 |

### Acceptance markers (Phase 3B freeze)

| Marker | Required |
|--------|----------|
| `PM_ELF_ZC_DONE image_id=7` | exactly once |
| `PM_ELF_ZC_DONE image_id=8` | exactly once |
| `PM_ELF_ZC_DONE image_id=9` | exactly once |
| `PM_ELF_ZC_DONE.*zc_pages=[1-9]` | 3 matches (all three images) |
| `PM_ELF_ZC_FAIL` | 0 |
| `PM_VFS_READ_BULK_DONE image_id=(7\|8\|9)` | 0 |
| `PM_VFS_READ_BULK_PHASE2A_BEGIN` | 0 |
| `INITRAMFS_SRV_ENTRY` | exactly once |
| `DEVFS_SRV_ENTRY` | exactly once |
| `VFS_SRV_ENTRY` | exactly once |
| `DRIVER_MANAGER_ENTRY` | exactly once |
| `BLKCACHE_SRV_ENTRY` | exactly once |
| `VIRTIO_BLK_SRV_ENTRY` | exactly once |
| `DRIVER_MANAGER_READY` | exactly once |
| `BLKCACHE_SRV_READY` | exactly once |
| `VIRTIO_BLK_SRV_READY` | exactly once |
| BAD / BOOT BLOCKERS | empty |

---

## CPIO Alignment Proof

Build log output after `common_create_initramfs_aligned`:
```
[initramfs-pack] ALIGN_PROOF path=sbin/driver_manager  data_offset=N*4096  aligned=true
[initramfs-pack] ALIGN_PROOF path=sbin/blkcache_srv    data_offset=N*4096  aligned=true
[initramfs-pack] ALIGN_PROOF path=sbin/virtio_blk_srv  data_offset=N*4096  aligned=true
[initramfs-pack] [ok] packed N entries, archive size=... bytes
[ok] aligned initramfs archive created: build-{arch}/initramfs-core.cpio
```

---

## Deprecated: Syscall 27 (Phase 2 Bridge)

Syscall nr=27 `InitramfsReadChunk` with `arg5=PM_TID=3` (cross-ASID copy path) is
the Phase 2B bootstrap bridge. It is **not yet removed** because:

1. The Phase 3A/3B fallback path uses it when `VFS_OP_FILE_GRANT_RO` returns
   `Unsupported` (old-kernel compatibility).
2. A long-run gate is required before removal: all image_id 7/8/9 spawns must show
   `PM_ELF_ZC_DONE zc_pages>0` across a full AArch64 + x86_64 CI matrix with no
   regressions in a sustained multi-boot run.

**Removal gate:** `PM_ELF_ZC_DONE zc_pages>0` for image_id 7/8/9 on both arches,
sustained across the CI long-run gate, with `PM_VFS_READ_BULK_PHASE2A_BEGIN=0` and
`PM_VFS_READ_BULK_DONE image_id=(7|8|9)=0`.

---

## x86_64 SMP Status

x86_64 smoke is `-smp 1` only. x86_64 SMP remains out of scope.

SMP TODO: split trampoline assembly from Rust SMP logic in `src/arch/x86_64/smp.rs`
before enabling multi-CPU smoke on x86_64.

---

## Files Changed (Phase 3B)

| File | Change |
|------|--------|
| `src/kernel/boot/exec_state.rs` | `load_elf_with_mo_zero_copy`: `image_id` param + full ZC diagnostics |
| `src/kernel/syscall.rs` | Call site: pass `image_id` to `load_elf_with_mo_zero_copy` |
| `targets/aarch64-yarm-user-none.json` | Add `-zmax-page-size=0x1000 -zcommon-page-size=0x1000` |
| `scripts/pack-initramfs-aligned.py` | New: Python CPIO packer with 4096-byte alignment |
| `scripts/lib/build-qemu-artifacts-common.sh` | Add `common_create_initramfs_aligned()` |
| `scripts/build-qemu-aarch64-artifacts.sh` | Use `common_create_initramfs_aligned` |
| `scripts/build-qemu-x86_64-artifacts.sh` | Use `common_create_initramfs_aligned` |
| `scripts/qemu-aarch64-core-smoke.sh` | Phase 3B freeze checks (zc_pages>0, bulk=0) |
| `scripts/qemu-x86_64-core-smoke.sh` | Phase 3B freeze checks (zc_pages>0, bulk=0, READY markers) |
| `doc/PHASE3B_MILESTONE.md` | This document |
| `doc/AI_AGENT_RULES.md` | Agent constraints for capability/spawn/ZC work |
