# Kernel Global Allocator

This repository uses a kernel global allocator for non-hosted builds (`not(feature = "hosted-dev")`) to support `no_std + alloc` runtime allocation needs.

## Scope and usage

- The allocator is exported as `KERNEL_GLOBAL_ALLOCATOR` from `src/kernel/global_allocator.rs`.
- It is installed with `#[global_allocator]` in:
  - `src/bin/kernel_boot.rs`
  - `crates/yarm-server-runtime/src/lib.rs`

## High-level design

The allocator is a two-path design:

1. **Small allocations (slab path)**
   - Size classes: **16, 32, 64, 128, 256, 512, 1024, 2048** bytes.
   - Routing uses `max(layout.size(), layout.align())`.
   - Requests at or below 2048 route to slab classes.

2. **Large allocations (page path)**
   - Requests above 2048 (or otherwise not class-routable) use contiguous page allocations.
   - Large allocations use a compact header for deallocation bookkeeping.

## Slab metadata

Each slab page stores in-page metadata:

- class index and object size
- object capacity
- intrusive free-list head
- `used` count
- allocation bitmap (for detectability of invalid/double frees)
- `next_page_phys` link in the per-class page list

There is no dedicated metadata page per small allocation.

## Reclamation policy

- Fully empty slab pages may be reclaimed back to the frame allocator.
- Policy keeps **one warm empty page per class**.
- If a class has more than one fully empty page, extra empty pages are unlinked/reclaimed.

## SMP/locking discipline

- **Per-size-class lock** (`SpinLockIrq<u64>`) protects each class page-list head and page metadata mutations.
- **Separate large-path lock** protects large-header lifecycle operations.
- **No nested allocator locks** are taken (class-lock and large-lock paths are disjoint).
- Lock hold may span frame allocator calls to avoid reclaim/list races.

## IRQ safety assumptions

- Allocator locks use `SpinLockIrq`, which disables local IRQs while held.
- This prevents same-CPU IRQ preemption deadlocks on the same allocator lock.

## Correctness invariants

- Alignment routing and returned-pointer alignment follow class/large-path constraints.
- Slab alloc/free maintains free-list + bitmap consistency.
- Reclamation/unlink is performed under class lock.
- Large dealloc validates large-header magic and page count.

## Known limitations

- Invalid free detection is best-effort (magic/shape/bitmap checks), not full provenance protection.
- Reclamation performs linear list scans within a class.
- Hosted-dev tests are model/interleaving based; true multicore race execution is not exercised in the hosted unit-test harness.

## Related files

- `src/kernel/global_allocator.rs`
- `src/kernel/frame_allocator.rs`
- `src/bin/kernel_boot.rs`
- `crates/yarm-server-runtime/src/lib.rs`
