<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM Phase Gates and Service-Domain Readiness

> **Ownership rule.** This doc consolidates the live phase-gate contracts for
> Drivers (Phase 2), Networking (Phase 3), UI (Phase 4), Structural Boundary
> (Phase 5), Shared-IPC (Phase 7), the per-phase readiness matrix, and the
> kernel-mechanism boundary snapshot. CI scripts pin literal tokens in this
> doc; do not rename headings without updating
> `scripts/check-roadmap-readiness.sh` +
> `scripts/check-boundary-milestone-freeze.sh`.
>
> Live per-arch status lives in `doc/STATUS.md`. The boundary milestone
> spec lives in `doc/MICROKERNEL_BOUNDARY.md`. Service-domain ownership
> rules are gated by `scripts/check-service-domain-ownership.sh` (see
> `doc/PROCESS_AND_SPAWN.md` §9).

---

## 1. Kernel / runtime boundary snapshot

### Kernel ownership (current)

Kernel and low-level runtime own:

- Scheduling and dispatch mechanisms.
- IPC / notification mechanisms.
- Capability enforcement / mechanisms.
- Trap / IRQ routing mechanisms.
- VM / address-space and bootstrap mechanisms.

### Not kernel-owned anymore

- Supervisor protocol encoding / decoding policy.
- VFS policy / backend / service layer.
- Driver-manager service logic.
- Substantial process-manager policy / orchestration logic.

These are workspace service-crate responsibilities.

### Current crate boundary model

- Mechanism types: `crates/yarm-kernel`.
- Shared ABI contracts: `crates/yarm-ipc-abi`.
- Shared service helpers: `crates/yarm-srv-common`.
- Service policy / orchestration: extracted `yarm-*-servers` crates.

### Current enforcement / gates

- `scripts/check-crate-graph-boundary.py`
- `scripts/check-service-arch-boundary.sh`
- `scripts/phase5-boundary-gates.sh`

PR-BND-6 pass C landed; boundary milestone freeze status is recorded in
`doc/MICROKERNEL_BOUNDARY.md`.

No kernel-side doc blocker is currently tracked; active work is primarily
service-crate reliability hardening while preserving the
mechanism / policy split.

---

## 2. Server roadmap (post-extraction)

### Service domain ownership

- Control plane: `crates/yarm-control-plane-servers`
- Drivers: `crates/yarm-driver-servers`
- Filesystems: `crates/yarm-fs-servers`
- Networking: `crates/yarm-network-servers`
- UI: `crates/yarm-ui-servers`
- Compatibility / personality: `crates/yarm-compat-servers`
- Shared service helper / runtime: `crates/yarm-srv-common`

### Current maturity focus

1. Keep boundary and runtime-entrypoint parity gates green.
2. Harden cross-service ABI stability via `yarm-ipc-abi` + deterministic
   tests.
3. Continue service reliability work (recovery / restart paths) in service
   crates, not kernel.

### Canonical contributor gates

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

### What is no longer true

- Service domain ownership under `src/services/*`.
- Root crate as the primary service bin owner.

## Architecture follow-up status (frozen)

The post-extraction architecture is frozen. No further structural
refactor is planned for the service-domain boundary; subsequent work is
service-reliability and ABI hardening only.

## Architecture follow-up addenda

- 2026-06-15: Pass 4 documentation consolidation — phase contract docs
  merged into `doc/PHASE_GATES.md`. Phase 2 readiness wiring (`fault
  gate: wired to compat-gates workflow`, `delegation gate: wired to
  compat-gates workflow`) preserved verbatim; CI tokens
  (`phase2-driver-gates`, `phase3-network-gates`, `phase4-ui-gates`,
  `phase4-ui-smoke-marker`, `phase5-boundary-gates`) preserved verbatim
  in the per-phase contract sections below and in §6.

---

## 3. Phase Readiness Matrix

Current phase gates by extracted crate ownership.

### Phase 1 — Filesystem server readiness

- Contracts: `doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md` (canonical).
- Gate command:

```bash
scripts/phase5-boundary-gates.sh --fs-runtime-entrypoint
```

### Phase 2 — Driver server readiness

- Contracts: this doc §4, `doc/LIBC_ABI_X86_64_NONE.md`.
- Gate command:

```bash
scripts/phase2-driver-gates.sh
```

- CI token: `phase2-driver-gates`.

### Phase 3 — Network server readiness

- Contracts: this doc §5, `doc/NETWORKING.md` (per-service ABIs).
- Gate commands:

```bash
scripts/phase5-boundary-gates.sh --network-runtime-entrypoint
scripts/phase3-4-strict-runtime-gate.sh
```

- CI token: `phase3-network-gates`.

### Phase 4 — UI server readiness

- Contracts: this doc §6.
- Gate commands:

```bash
scripts/phase4-ui-gates.sh
scripts/phase5-boundary-gates.sh --ui-runtime-entrypoint
```

- CI tokens: `phase4-ui-gates`, `phase4-ui-smoke-marker`.

### Phase 5 — Structural boundary / type gates

```bash
scripts/phase5-boundary-gates.sh
```

Runs crate-graph / source-shape / freeze checks and extracted-server
compile checks. CI token: `phase5-boundary-gates`.

### Phase 7 — Shared IPC hardening

```bash
scripts/phase7-shared-ipc-gates.sh
```

See `doc/IPC.md` §5 for the migration ownership and gate expectations.

---

## 4. Phase 2 — Device Driver Servers

Minimum invariants for Phase-2 driver services (`irqmux`, `uart`,
`virtio_blk`, `virtio_net`, `virtio_gpu`, `input`).

### Delegation contract — delegation gate: wired to compat-gates workflow

- Driver runtime capability bundles are delegated only through validated
  service-role edges.
- Bundles are composed of IRQ and DMA primitives, with optional IOVA-space
  attachment.
- Delegation must be deterministic for the same input plan and service
  graph.

### Fault / restart contract — fault gate: wired to compat-gates workflow

- Driver restart requires a valid restart token.
- Driver runtime caps are revoked on restart and faulted teardown.
- Restart denial escalation is reported at class-configured thresholds.

### DMA / IOVA contract

- IOVA windows must be page-aligned and non-zero sized.
- Validation fails when no IOVA space is attached or window constraints
  are violated.
- Detaching an IOVA space immediately invalidates DMA-window validation.

### Service-level deterministic counters

- `irqmux.srv`: routed vs dropped IRQ accounting.
- `uart.srv`: tx / rx byte counters.
- `virtio_net.srv`: tx / rx packet counters.
- `virtio_gpu.srv`: mode-set and frame-commit counters.
- `input.srv`: accepted vs dropped input events.

### CI gate mapping (Phase 2 — `phase2-driver-gates`)

Transfer-cap ABI prerequisite:

- Kernel IPC syscall ABI is frozen at v3.
- Transfer-cap send requires a known waiting receiver (`WouldBlock`
  otherwise).
- Transfer metadata is an envelope handle (not a raw source capability
  id).
- Reference: `doc/LIBC_ABI_X86_64_NONE.md`.

Tests required to pass in `.github/workflows/compat-gates.yml` under
`phase2-driver-gates` (executed via `scripts/phase2-driver-gates.sh`):

- `kernel::boot::tests::delegate_driver_bundle_checked_enforces_service_role_edges`
- `kernel::boot::tests::restart_denial_escalates_to_supervisor_every_threshold`
- `kernel::boot::tests::driver_restart_revokes_runtime_caps`
- `kernel::syscall::tests::transfer_send_without_waiter_returns_would_block`

### Backpressure and queueing

- `uart.srv` enforces TX queue limits with deterministic drop
  accounting.
- `virtio_net.srv` enforces TX queue limits with deterministic drop
  accounting and completion-based recovery.
- `input.srv` enforces queue limits and deterministic overflow handling.
- `virtio_gpu.srv` rejects frame commit before mode-set and reports
  deterministic rejection counters.
- `irqmux.srv` supports per-line masking and deterministic masked / drop
  routing semantics.

---

## 5. Phase 3 — Networking

Minimum invariants for networking services (`netmgr`, `tcpip`, `dns`,
`dhcp`, `socket`). Full per-service ABI: `doc/NETWORKING.md`.

### Packet path

- Link bring-up (`netmgr`) precedes packet routing (`tcpip`).
- Routed and dropped packet counters are deterministic for a given event
  sequence.

### Name / lease

- DNS cache-hit vs upstream-query accounting is deterministic.
- DHCP lease-grant vs renewal accounting is deterministic.

### Socket adapter

- Socket open/close accounting must remain balanced for deterministic
  round-trips.
- Adapter behavior must remain transport-agnostic and not depend on
  FS / UI internals.

### IPC transfer-cap ABI prerequisite

- Kernel IPC syscall ABI is frozen at v3.
- Transfer-cap send requires a known waiting receiver (`WouldBlock`
  otherwise).
- Transfer metadata is an envelope handle, not a raw source capability
  id.
- Reference: `doc/LIBC_ABI_X86_64_NONE.md`.

### CI gate mapping (Phase 3 — `phase3-network-gates`)

- `kernel::syscall::tests::transfer_send_without_waiter_returns_would_block`
- `yarm_network_servers::network::netmgr::service::tests::netmgr_tracks_link_state_events`
- `yarm_network_servers::network::tcpip::service::tests::tcpip_deterministic_packet_path`
- `yarm_network_servers::network::dns::service::tests::dns_timeout_retry_is_reproducible`
- `yarm_network_servers::network::dhcp::service::tests::dhcp_lease_accounting_is_deterministic`
- `yarm_network_servers::network::socket::service::tests::socket_adapter_roundtrip_is_accounted`
- `yarm_network_servers::network::sim::tests::deterministic_network_bootstrap_flow_is_stable`
- `yarm_network_servers::network::sim::tests::link_flap_dhcp_rebind_and_socket_recovery_is_deterministic`

---

## 6. Phase 4 — UI

Minimum invariants for UI-facing services (`display`, `compositor`,
`shell`, and phase-linked `input`).

### Boot marker

- `display.srv` must emit a stable boot marker string:
  `[ui] boot-to-shell marker`.
- QEMU smoke validation must treat this marker as an accepted
  boot-success signal.

### Display

- Mode-set and frame-present counters are deterministic for a fixed
  event sequence.
- Frame-present checks are required in CI for Phase-4 readiness.

### Compositor replay

- Composition replay count is deterministic across repeated runs.
- Compositor behavior should remain independent from FS / network
  internals.

### Shell / session

- Shell session startup counter is deterministic and monotonic.
- Session-manager startup is validated by deterministic unit coverage.

### IPC transfer-cap ABI prerequisite

(Same as Phase 3 — reference `doc/LIBC_ABI_X86_64_NONE.md`.)

### CI gate mapping (Phase 4 — `phase4-ui-gates`, `phase4-ui-smoke-marker`)

- `kernel::syscall::tests::transfer_send_without_waiter_returns_would_block`
- `yarm_ui_servers::ui::display::service::tests::boot_marker_is_stable`
- `yarm_ui_servers::ui::display::service::tests::display_tracks_modeset_and_present`
- `yarm_ui_servers::ui::compositor::service::tests::compositor_replay_is_deterministic`
- `yarm_ui_servers::ui::shell::service::tests::shell_session_counter_increments`
- `phase4-ui-smoke-marker` job in `.github/workflows/compat-gates.yml`.

---

## 7. Authoring rule

Future phase-gate changes update **this file**. The five per-CI tokens
(`phase2-driver-gates`, `phase3-network-gates`, `phase4-ui-gates`,
`phase4-ui-smoke-marker`, `phase5-boundary-gates`) must remain literally
greppable in `.github/workflows/compat-gates.yml` and
`.github/workflows/core-qemu-smoke.yml`. Do **not** create new
`PHASE*_CONTRACT.md` / `KERNEL_STATUS.md` / `SERVER_ROADMAP.md` /
`PHASE_READINESS_MATRIX.md` fragment files.
