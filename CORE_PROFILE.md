<!-- SPDX-License-Identifier: Apache-2.0 -->

# Core Profile Release Checklist

This checklist defines the **core-only** release profile for systems that do not ship Linux personality compatibility.

## Build profile

- Build/test command: `cargo test`
- Linux personality feature is disabled by default.
- No Linux personality server binary is required in this profile.

## Expected module boundaries

- Mechanism code under `src/kernel/*` remains Linux-policy agnostic.
- Linux compatibility translation stays in `src/linux_compat/*` and is feature-gated.
- Protocol modules (`process_abi`, `vfs_abi`) remain shared wire-contract modules and do not depend on Linux personality.

## Core deliverables

- Kernel mechanism layer (scheduler, VM, capability checks, IPC transport).
- Process manager and VFS services via protocol IPC contracts.
- Deterministic core simulation (`src/kernel/sim.rs`) without Linux dispatch coupling.
- Minimum runnable server profile smoke covering `init.srv`, `process_manager.srv`, `vfs.srv`, `supervisor.srv`, `devfs.srv`, and `initramfs.srv`.

## Gate criteria

- `cargo test` passes.
- `cargo test -q --bin core_profile_smoke` passes (core profile smoke path).
- No mandatory dependency on `linux-compat` feature.
- Core tests include deterministic simulation and protocol contract checks.
- Core smoke validates the minimum runnable profile path with `devfs` + `initramfs` mounted behind init orchestration.
