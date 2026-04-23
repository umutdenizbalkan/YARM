<!-- SPDX-License-Identifier: Apache-2.0 -->

# Phase Readiness Matrix (Current)

This matrix tracks current phase gates using extracted crate ownership.

## Phase 1 — Filesystem server readiness

- Contracts: `STORAGE_SERVICE_CONTRACT.md`, `DEVFS_CONTRACT.md`, `INITRAMFS_CONTRACT.md`, `RAMFS_CONTRACT.md`
- Gate command:

```bash
scripts/phase5-boundary-gates.sh --fs-runtime-entrypoint
```

## Phase 2 — Driver server readiness

- Contracts: `PHASE2_DRIVER_CONTRACT.md`, `LIBC_ABI_X86_64_NONE.md`
- Gate command:

```bash
scripts/phase2-driver-gates.sh
```

## Phase 3 — Network server readiness

- Contracts: `PHASE3_NETWORK_CONTRACT.md`
- Gate commands:

```bash
scripts/phase5-boundary-gates.sh --network-runtime-entrypoint
scripts/phase3-4-strict-runtime-gate.sh
```

## Phase 4 — UI server readiness

- Contracts: `PHASE4_UI_CONTRACT.md`
- Gate commands:

```bash
scripts/phase4-ui-gates.sh
scripts/phase5-boundary-gates.sh --ui-runtime-entrypoint
```

## Phase 5 — Structural boundary/type gates

```bash
scripts/phase5-boundary-gates.sh
```

This runs crate-graph/source-shape/freeze checks and extracted server compile checks.

## Phase 7 — Shared IPC hardening

```bash
scripts/phase7-shared-ipc-gates.sh
```
