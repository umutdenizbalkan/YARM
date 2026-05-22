<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# BLOCK_BACKEND_ABI

This ABI is for **blkcache -> block-driver backend** communication only.
It is separate from the frontend blkcache ABI used by filesystem-facing clients.

- `Message.opcode` carries operation (`QUERY_STATE`, `READ`, `WRITE`, `FLUSH`, `GET_GEOM`).
- Payload carries only metadata/descriptor fields (no bulk data transfer).
- SG entries are `(mem_cap, offset, length, flags)` and never raw physical addresses.

Shared-memory mapping, DMA/IOMMU mapping, and zero-copy transport are future work.
Current `virtio_blk_srv` behavior is truthful stub behavior:

- `QUERY_STATE` => `EAGAIN` with logical/physical block size 512.
- `GET_GEOM` => `EAGAIN` while hardware remains not ready.
- `READ` / `WRITE` / `FLUSH` => `ENOSYS`.

No fake I/O success is allowed.
