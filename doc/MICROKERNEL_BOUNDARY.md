<!-- SPDX-License-Identifier: Apache-2.0 -->

# Microkernel Boundary Contract (Current)

This document describes the **current** boundary after server extraction and root-crate cleanup.

## Kernel-side mechanism ownership

Owned by kernel/root + `yarm-kernel` mechanism crates:

- scheduler and CPU/task dispatch mechanisms
- IPC transport and notification mechanisms
- capability rights enforcement/mechanisms
- trap/IRQ normalization and routing mechanisms
- virtual memory/address-space primitives
- bootstrap/runtime mechanism plumbing

## User-space service policy ownership

Owned by workspace server crates:

- control-plane service policy (`yarm-control-plane-servers`)
- filesystems and VFS service policy (`yarm-fs-servers`)
- driver-manager + driver service policy (`yarm-control-plane-servers`, `yarm-driver-servers`)
- network policy/services (`yarm-network-servers`)
- UI service policy (`yarm-ui-servers`)
- POSIX personality translation policy (`yarm-compat-servers`)

## Current repository layout implications

- `src/services/` is removed.
- Root crate is not the service container.
- Extracted service bins are owned by workspace server crates.

## ABI ownership

Shared ABI contracts are owned in `crates/yarm-ipc-abi` (including supervisor and socket ABI families).
Service-side helper/runtime glue is owned in `crates/yarm-srv-common`.

## Enforcements and gates

Primary structural checks:

- `scripts/check-crate-graph-boundary.py`
- `scripts/check-service-arch-boundary.sh`
- `scripts/phase5-boundary-gates.sh`

Milestone freeze/assertion checks:

- `scripts/check-boundary-milestone-freeze.sh`

## Boundary milestone status

✅ **COMPLETE** — PR-BND-6 pass C landed; the boundary milestone freeze gate now tracks this moved document at `doc/MICROKERNEL_BOUNDARY.md`.

## Current rule of thumb

- If behavior is policy/translation/orchestration, it belongs in workspace service crates.
- If behavior is mechanism/resource enforcement, it belongs in kernel/mechanism crates.
