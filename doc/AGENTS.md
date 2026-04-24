## Licensing

All new source files must begin with the following header, before any other content including `#![no_std]`:
```
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan
```

Do not omit this header. Do not add any other license text.

## Architecture / Boundary Rules

- `yarm-server-runtime` must remain a narrow userspace server runtime boundary.
- It may export only intentional server-facing surfaces such as:
  - `ipc_abi`
  - `user_rt`
  - freestanding allocator installer
  - startup slot installer/helpers
- It must never depend on or re-export the root `yarm` crate.
- It must never expose `KernelState`, `Bootstrap`, `TrapFrame`, `ProcessManager`, `kernel::boot`, or other kernel-internal surfaces.
- Do not use `yarm-server-runtime` as a compatibility bridge for server crates.
- If a server needs a new runtime surface, add the smallest explicit userspace-facing API instead of glob re-exporting kernel internals.
