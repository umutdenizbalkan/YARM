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

## `/init` identity (current boot flow)

- Current QEMU artifact scripts stage **`yarm-control-plane-servers` / `init_server`** as `/init`.
- The kernel loads `/init` from initramfs as the first user task.
- `initramfs_srv` is a separate filesystem server binary and is staged into initramfs as `/sbin/initramfs_srv` (launchable artifact only).
- `/init` remains `init_server`; `initramfs_srv` is **not launched yet** by current runtime orchestration.
- Future work for active initramfs IPC boot path is still: `init_server` orchestration + spawn/startup-cap passing.
- Artifact scripts verify initramfs includes `/init`, `/sbin/init_server`, and `/sbin/initramfs_srv` after CPIO creation.
- `yarm_user_rt::user_log!` is currently a no-op formatting macro and is not serial-visible by itself.
- Process-manager runtime spawn is still staged: non-test spawn now performs real boot-initrd image lookup + ELF parse validation at the runtime seam, then returns truthful `Unsupported` until kernel-backed task launch + startup-cap installation are connected.
- Startup-cap transport is transitioning away from slot-overload assumptions via explicit structured service startup-cap ABI (`ServiceStartupCapsV1`), while slot 11 remains compatibility-only debt.

## Initramfs orchestration/runtime truth (current)

- `SpawnV5` exists and carries structured `ServiceStartupCapsV1` payloads.
- `InitOrchestrationCapsV1` exists and uses dedicated startup slots (not slot-11 overload) for orchestration control channels.
- `init_server` sends `SpawnV5` over real IPC when process-manager caps and orchestration caps are present.
- `initramfs_srv` consumes structured startup caps and sends `INITRAMFS_READY` readiness signals.
- Readiness is health-gated: spawn success by itself is not sufficient.
- Boot marker validation script exists: `scripts/check-initramfs-ready-boot.sh`.
- Real QEMU boot validation is still pending when QEMU/artifacts are unavailable in the current environment.
- `VFS_READ_SHARED_REPLY_ENABLED` remains `false`.
- VFS routing for this path is **not enabled yet**.

### How to validate

```bash
scripts/build-qemu-x86_64-artifacts.sh
scripts/check-initramfs-ready-boot.sh
scripts/check-initramfs-ready-boot.sh --check-log scripts/testdata/initramfs-ready-pass.log
scripts/test-check-initramfs-ready-boot.sh
```

### Remaining blockers before VFS routing

- real boot marker validation in QEMU with staged artifacts
- service readiness stability under repeated boot/orchestration runs
- VFS routing table wiring / endpoint registration for the initramfs service path
- consumer-side shared-reply support rollout
- only after the above: enable `VFS_READ_SHARED_REPLY_ENABLED`

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

For a broader pre-push sweep, run `scripts/phase5-boundary-gates.sh` first, then the phase-specific gate scripts relevant to the crates you changed.
If you touch CI definitions or project automation policies, also run:

```bash
scripts/check-ci-workflow-enforcement.sh
```

## Status

YARM is active development software with extracted workspace-owned service domains and crate-graph boundary enforcement.
See:

- `doc/MICROKERNEL_BOUNDARY.md`
- `doc/ARCH_SPLIT.md`
- `doc/KERNEL_STATUS.md`
- `doc/USERSPACE_SERVER_MATURITY.md`

## License

Apache-2.0.
