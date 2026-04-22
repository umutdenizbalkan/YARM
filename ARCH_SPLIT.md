<!-- SPDX-License-Identifier: Apache-2.0 -->

# Architecture Split (Current State)

This is the current split after service extraction.

## Root crate (`src/`)

- `src/kernel/*`: kernel mechanisms and bootstrap/runtime internals
- `src/arch/*`: ISA/platform bring-up and low-level architecture glue
- `src/runtime.rs`: runtime support glue
- root crate re-exports workspace server modules for integration wiring

## Extracted workspace crates

- `crates/yarm-kernel`: extracted kernel mechanism type families
- `crates/yarm-ipc-abi`: shared IPC ABI ownership
- `crates/yarm-srv-common`: shared service helper/runtime utilities
- `crates/yarm-server-runtime`: server-runtime wrappers
- `crates/yarm-control-plane-servers`
- `crates/yarm-driver-servers`
- `crates/yarm-fs-servers`
- `crates/yarm-network-servers`
- `crates/yarm-ui-servers`
- `crates/yarm-compat-servers`
- `crates/yarm-runtime-tools`

## Important outcome

`src/services/` no longer exists.
Service ownership is crate-local in workspace packages, not root `src/`.

## Boundary intent

- kernel crates: mechanism
- server crates: policy/orchestration/compat translation
