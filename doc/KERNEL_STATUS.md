<!-- SPDX-License-Identifier: Apache-2.0 -->

# Kernel Status (Current)

This file captures current kernel/runtime boundary reality.

## Kernel ownership (current)

Kernel and low-level runtime own:

- scheduling and dispatch mechanisms
- IPC/notification mechanisms
- capability enforcement/mechanisms
- trap/IRQ routing mechanisms
- VM/address-space and bootstrap mechanisms

## Not kernel-owned anymore

The kernel no longer owns service-policy domains such as:

- supervisor protocol encoding/decoding policy
- VFS policy/backend/service layer
- driver-manager service logic
- substantial process-manager policy/orchestration logic

These are workspace service-crate responsibilities.

## Current crate boundary model

- Mechanism types: `crates/yarm-kernel`
- Shared ABI contracts: `crates/yarm-ipc-abi`
- Shared service helpers: `crates/yarm-srv-common`
- Service policy/orchestration: extracted `yarm-*-servers` crates

## Current enforcement/gates

- `scripts/check-crate-graph-boundary.py`
- `scripts/check-service-arch-boundary.sh`
- `scripts/phase5-boundary-gates.sh`

PR-BND-6 pass C landed; boundary milestone freeze status is recorded in `doc/MICROKERNEL_BOUNDARY.md`.

## Current known limitation snapshot

No kernel-side doc blocker is currently tracked in this file; active work is primarily service-crate reliability hardening while preserving the mechanism/policy split.
