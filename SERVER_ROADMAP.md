<!-- SPDX-License-Identifier: Apache-2.0 -->

# Server Roadmap (Current)

This roadmap reflects the **post-extraction** layout.

## Service domain ownership

- Control plane: `crates/yarm-control-plane-servers`
- Drivers: `crates/yarm-driver-servers`
- Filesystems: `crates/yarm-fs-servers`
- Networking: `crates/yarm-network-servers`
- UI: `crates/yarm-ui-servers`
- Compatibility/personality: `crates/yarm-compat-servers`
- Shared service helper/runtime: `crates/yarm-srv-common`

## Current maturity focus

1. Keep boundary and runtime-entrypoint parity gates green.
2. Harden cross-service ABI stability via `yarm-ipc-abi` + deterministic tests.
3. Continue service reliability work (recovery/restart paths) in service crates, not kernel.

## Canonical contributor gates

```bash
scripts/phase5-boundary-gates.sh
scripts/phase5-boundary-gates.sh --fs-runtime-entrypoint
scripts/phase5-boundary-gates.sh --driver-runtime-entrypoint
scripts/phase5-boundary-gates.sh --network-runtime-entrypoint
scripts/phase5-boundary-gates.sh --ui-runtime-entrypoint
```

Additional phase-specific checks:

```bash
scripts/phase2-driver-gates.sh
scripts/phase4-ui-gates.sh
scripts/phase3-4-strict-runtime-gate.sh
scripts/phase7-shared-ipc-gates.sh
```

## What is no longer true

- Service domain ownership under `src/services/*`.
- Root crate as the primary service bin owner.
