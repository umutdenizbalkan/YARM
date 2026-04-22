# Linux is Obsolete.

There. Someone had to say it again.

---

<div align="center">

# YARM

**Yet Another Real-time Microkernel**

*A capability-based microkernel with workspace-owned user-space servers.*

</div>

## Current architecture snapshot

YARM is a `no_std` microkernel root crate plus a workspace of extracted server crates.

### Kernel/root responsibilities (`src/`)

- scheduling, IPC transport, trap/IRQ normalization, capabilities, VM/memory mechanisms
- bootstrap and architecture bring-up (`src/kernel/boot`, `src/arch`)
- runtime wiring and crate re-exports for extracted server crates

### Workspace server responsibilities (`crates/`)

- `yarm-control-plane-servers`: init/process-manager/vfs/supervisor/driver-manager service policy
- `yarm-driver-servers`: driver-domain servers
- `yarm-fs-servers`: filesystem/VFS backend services
- `yarm-network-servers`: networking services
- `yarm-ui-servers`: UI/display/compositor/shell services
- `yarm-compat-servers`: POSIX compatibility personality (`posix_compat`)
- `yarm-srv-common`: shared service-side helper/runtime utilities
- `yarm-ipc-abi`: shared ABI ownership (including supervisor and socket ABI)
- `yarm-kernel`: extracted kernel mechanism types
- `yarm-server-runtime`: runtime entry wrappers for extracted server bins
- `yarm-runtime-tools`: runtime tooling/smoke binaries

`src/services/` has been removed; services are workspace-owned.

## Boundary model (current)

- **Kernel = mechanism**
- **Servers = policy**

Examples of policy now outside kernel:

- supervisor protocol encoding/decoding
- VFS policy/backend/service layer
- driver-manager service logic
- significant process-manager lifecycle/policy logic

## Compatibility layer (current)

`posix_compat` lives in `crates/yarm-compat-servers` and routes syscall personality behavior through service bindings.
Socket `socket/connect/sendto` compatibility paths use shared socket ABI ownership in `crates/yarm-ipc-abi` plus binding-backed dispatch.

## Contributor checks (most used)

### Structural boundary gates

```bash
scripts/phase5-boundary-gates.sh
```

### Runtime-entrypoint parity gates

```bash
scripts/phase5-boundary-gates.sh --fs-runtime-entrypoint
scripts/phase5-boundary-gates.sh --driver-runtime-entrypoint
scripts/phase5-boundary-gates.sh --network-runtime-entrypoint
scripts/phase5-boundary-gates.sh --ui-runtime-entrypoint
```

### Additional phase gates

```bash
scripts/phase2-driver-gates.sh
scripts/phase4-ui-gates.sh
scripts/phase3-4-strict-runtime-gate.sh
scripts/phase7-shared-ipc-gates.sh
```

## Status

YARM is active development software with extracted workspace-owned service domains and crate-graph boundary enforcement.
See:

- `MICROKERNEL_BOUNDARY.md`
- `ARCH_SPLIT.md`
- `KERNEL_STATUS.md`
- `USERSPACE_SERVER_MATURITY.md`

## License

Apache-2.0.
