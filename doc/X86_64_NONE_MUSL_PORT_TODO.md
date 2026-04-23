<!-- SPDX-License-Identifier: Apache-2.0 -->

# musl sysdeps shim TODO (all ISAs, x86_64 boot target-first)

Status: **multi-ISA portability effort** with **x86_64 as the current boot target** (hosted Linux ABI path is not the primary target).

## Goal

Port user-space runtime behind an ISA-agnostic musl sysdeps shim that maps libc expectations to YARM microkernel mechanisms, then validate per-ISA runners. Current boot-first ISA: `x86_64`.

## Milestone 1 — Target + toolchain baseline

- [x] Add a custom target JSON for `x86_64-unknown-none` (code model, relocation model, panic strategy).
- [x] Add `.cargo/config.toml` target aliases for x86_64-none workflows (runner wiring is deferred until bootable image format is finalized).
- [x] Introduce build profile knobs for freestanding userspace (panic=abort, LTO optional).
- [ ] Verify `cargo build --target <x86_64-none-target>` works for `kernel_boot` and `init_server` without Linux ABI assumptions.
  - Current blocker: `core` is unavailable for this custom target in the current toolchain setup; next step is to wire `build-std`/`rust-src` strategy for the freestanding target.
  - Bootstrap script added: `scripts/build-x86_64-none-bootstrap.sh` to enforce toolchain/rust-src checks and run the `-Z build-std` flow once prerequisites exist.
  - Milestone 1 verification remains blocked in this environment because downloading `nightly`/`rust-src` from `static.rust-lang.org` fails (network tunnel limitation).

## Milestone 2 — ABI boundary contract for libc shim

**Audit status (2026-03-24): complete.**

Completed:
- [x] Freeze a tiny libc-facing kernel ABI surface (threads/TLS, memory mapping, clocks, process lifecycle, IPC-backed fd model).
- [x] Document syscall numbering + calling convention expected by shim stubs.
- [x] Define error mapping policy (`errno` conversion from kernel/service status codes).

- [x] Add compatibility tests that explicitly cover `EINTR` and timeout error mapping behavior at the shim boundary.
- [x] Add compatibility tests that explicitly assert partial I/O + invalid-handle semantics with errno-level expectations in the linux-compat shim surface.

## Milestone 3 — musl sysdeps shim (minimum viable)

**Audit status (2026-03-24): partially complete.**

Implemented bootstrap sysdeps pieces:
- [x] Implement memory primitives (`mmap`/`munmap` equivalent, brk/no-brk policy).
- [x] Implement thread primitives expected by musl (`clone`/TLS hooks or equivalent shim model).
- [x] Implement futex-like wait/wake bridge using kernel IPC/synchronization primitives.
- [x] Implement time/clock hooks (`clock_gettime`, `nanosleep`) via kernel timer-backed service hooks for deterministic bring-up.
- [x] Implement minimal file/socket facade over VFS/network services (bootstrap deterministic fd hooks in `linux_compat::sysdeps`; full service-backed semantics remain milestone-4+ integration work).
  - Mapping matrix artifact added: `MUSL_POSIX_IPC_MAPPING.md` (POSIX entry -> linux nr -> IPC opcode/service).

Not yet complete:
- [ ] Implement real musl entry/exit glue (`crt` startup + `__libc_start_main` integration path). Current code validates/parses startup vectors and runs a test main callback, but does not yet provide the full musl crt integration symbols.

## Milestone 4 — Service integration on x86_64

- [ ] Boot `init_server` + `procman_srv` + `vfs_srv` under x86_64 QEMU path.
- [ ] Add x86_64 smoke script parallel to current RISC-V smoke flow.
- [ ] Add strict boot markers for x86_64 (`YARM_BOOT_OK`, `YARM_INIT_DONE`, service health checks).
- [ ] Validate linux-compat server behavior when hosted ABI is absent (or explicitly gate it off).

## Milestone 5 — CI gates and migration hardening

- [ ] Add CI job matrix entry for x86_64 freestanding target build.
- [ ] Add deterministic contract tests that exercise shim boundary and `errno` mapping.
- [ ] Keep RISC-V smoke optional while x86_64 becomes primary developer path.
- [ ] Add regression suite for capability transfer + shared-memory descriptor paths under x86_64.

## Risks / watchpoints

- musl thread/TLS expectations can dominate complexity early.
- fd/socket semantics need clear ownership between libc shim and user-space servers.
- signal semantics should be explicitly unsupported or emulated deterministically.

## Suggested immediate execution order (next 2 weeks)

1. Target JSON + cargo config + successful x86_64 freestanding builds.
2. Written libc-kernel ABI contract + error mapping table.
3. Minimal startup + memory + clock sysdeps.
4. `init_server` smoke on x86_64 QEMU with strict marker checks.
