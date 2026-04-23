<!-- SPDX-License-Identifier: Apache-2.0 -->

# YARM Roadmap (Current)

This roadmap documents current direction after server extraction and boundary cleanup.

## Completed structural milestones (already reflected in code)

- Workspace-owned service domains (control-plane/driver/fs/network/ui/compat)
- Removal of `src/services/`
- Root-crate service ownership cleanup
- Crate-graph boundary gating and phase boundary scripts
- Shared ABI ownership in `crates/yarm-ipc-abi`

## Active roadmap priorities

1. **Kernel mechanism stability**
   - maintain mechanism-only kernel boundary
   - preserve trap/IPC/scheduler/capability invariants and tests

2. **Service reliability in workspace crates**
   - init/control-plane progression and recovery robustness
   - deterministic behavior under restart/fault paths

3. **ABI/runtime contract hardening**
   - keep supervisor/socket/VFS/process ABI contracts stable in `yarm-ipc-abi`
   - preserve service helper consistency through `yarm-srv-common`

4. **Gate-driven contributor workflow**
   - boundary gate stability (`phase5-boundary-gates`)
   - runtime-entrypoint parity checks per service domain

## Current architecture guardrails

- Policy logic remains outside kernel.
- Service extraction is not being reversed.
- Compatibility/socket routing remains binding-backed through shared ABI ownership.
