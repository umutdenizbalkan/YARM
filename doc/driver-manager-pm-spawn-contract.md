<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# Driver Manager ↔ Process Manager live spawn contract design

This document records the intended **future** contract between Driver Manager
(DM) and Process Manager (PM) for live driver spawning. It is a design note only:
DRS-6 does not add live spawning, hardware grants, capability minting, real MMIO,
live-DTB parsing, PM calls, or boot-chain changes. DRS-7 adds an inert
`DriverSpawnRequest` model that connects the mock pipeline to this contract, but
it still produces data only and does not call PM. DRS-8 adds an inert
PM-validation simulation over those records; validation is modeled for hosted
tests but is not executed by real PM and still has no PM/supervisor call path.
DRS-9 adds an inert PM accounting and rollback simulation over accepted validation
reports; reservations and rollback steps are descriptive only.

The current DRS models remain advisory mock policy/data models. They describe
what a safe future request would contain, but they do not transfer authority.

## 1. Authority boundary

The boundary is strict:

- Driver Manager **never creates processes**. It may decide that a driver should
  exist, but it cannot perform the mechanism of creating one.
- Driver Manager **never mints capabilities**. Any future capability delivered to
  a driver is minted by PM or another mechanism-owning authority that PM calls
  according to kernel policy.
- Driver Manager **never directly grants MMIO, IRQ, DMA, mailbox, PCIe BAR,
  pinmux, or clock authority**. It can request those resources and explain why
  they are needed.
- Driver Manager owns policy: hardware discovery input, driver matching,
  dependency ordering, health monitoring, crash/restart policy, `SpawnPlan`
  construction, and PM-facing `DriverSpawnRequest` construction.
- Process Manager owns mechanism: validation, address-space setup, resource
  accounting, capability minting, startup-cap delivery, process creation, and
  returning driver/process handles.
- PM is allowed to reject a DM request even when DM policy says a driver should
  start. PM rejection is authoritative for process/resource mechanism.

## 2. Data-flow pipeline

### Current fake/mock pipeline

The current hosted and mock-safe DRS path is:

1. Fake FDT or static inventory input.
2. Inert `PlatformInventory` / `DeviceRecord` data.
3. Sender-scoped read-only resource query replies.
4. Policy-only `SpawnPlan` entries.
5. Mock `SpawnAuthorityDecision` approvals or denials.
6. Mock `ResourceGrantBundle` descriptions.
7. DRS-7 inert `DriverSpawnRequestBundle` / `DriverSpawnRequest` records.
8. DRS-8 inert `PmSpawnValidationReport` records that simulate PM validation
   outcomes without invoking PM.
9. DRS-9 inert `PmSpawnAccountingReport` records that simulate PM reservations,
   commits, and rollback plans without touching live resources.

This pipeline is descriptive. It never calls PM or supervisor services, never
spawns a task, never grants resources, never transfers caps, and never touches
MMIO. The DRS-8 validation report and DRS-9 accounting report are audit artifacts
only; real PM remains the only future process-creation, address-space setup,
resource-accounting, rollback, cap-minting, grant, startup-cap-delivery, and
handle-return authority.

### Future live pipeline

The intended live path is:

1. Live DTB discovery or a platform service inventory supplies validated device
   records.
2. Driver Manager matches records to driver candidates and builds a `SpawnPlan`.
3. Driver Manager converts an eligible plan into a PM-facing
   `DriverSpawnRequest`.
4. PM authenticates the sender and validates the request against platform
   inventory, resource ownership, accounting limits, and isolation policy. DRS-8
   models this step inertly with a mock verified-DM-identity policy bit; payload
   or self-claimed identity is not trusted.
5. PM mints or obtains the needed capabilities and sets up the driver's address
   space.
6. PM creates the driver process.
7. PM delivers the stable startup-cap layout and startup arguments.
8. The driver registers with Driver Manager after initialization.
9. Driver Manager monitors health/status while PM owns process-liveness and death
   notification.
10. For restart, Driver Manager sends a restart request to PM; PM performs or
    denies the restart mechanism.

## 3. PM-facing `DriverSpawnRequest` shape

DRS-7 models this request shape with bounded inert Rust types near the
`driver_manager` policy code. The model is PM-facing data only: request IDs and
resource IDs are mock identifiers, not `CapId`s, process handles, or authority.
A future live `DriverSpawnRequest` should be versioned and explicit. A conceptual
shape is:

| Field | Purpose |
| --- | --- |
| `request_version` | Version of the DM↔PM request schema. |
| `requesting_driver_manager` | Kernel-authenticated DM identity; payload claims are not authority. |
| `driver_candidate` | Matched candidate name plus optional driver class metadata. |
| `image_id` / `binary_name` | PM-resolvable executable identity. |
| `device_record_id` | Stable inventory record being served. |
| `compatible` | Device compatible string that justified the match. |
| `device_class` | Coarse class such as UART, mailbox, GPIO, IRQ mux, or block. |
| `resource_requirements.mmio_ranges` | Required MMIO windows, with record-relative provenance. |
| `resource_requirements.irq_lines` | Required interrupt lines and routing-domain metadata. |
| `resource_requirements.dma_windows` | Required DMA windows and IOMMU constraints, if any. |
| `resource_requirements.mailbox_transport` | Future mailbox transport/cache/MMIO requirements. |
| `resource_requirements.pcie_bar` | Future PCIe BAR identity, size, and offset requirements. |
| `resource_requirements.pinmux` | Required pin ownership/function state, if policy supports it. |
| `resource_requirements.clock` | Required clock identity/rate constraints, if policy supports it. |
| `startup_cap_layout` | Requested startup slots and semantic labels. |
| `dependencies` | Driver/service dependencies that must exist first. |
| `restart_policy` | DM policy for crash handling and retry limits. |
| `security_label` / `isolation_policy` | Optional label, sandbox, or privilege profile. |
| `startup_timeout` / `health_expectation` | Expected registration and heartbeat deadlines. |
| `rollback_behavior` | Required cleanup semantics if setup or registration fails. |

Every resource field is a requirement, not a grant. DRS-7 copies/describes the
mock `ResourceGrantBundle` requirements into the inert request model so hosted
tests can inspect MMIO/IRQ/DMA/mailbox/BAR/pinmux/clock needs without granting
anything. PM must derive actual mechanism from validated platform state and
kernel policy in a later live stage.

## 4. Startup-cap layout proposal

The driver startup interface should use a stable, documented slot layout before
any implementation. DRS-7 adds `StartupCapRequirement` descriptors for this
layout, but they are descriptive only and do not mint, transfer, or install caps.
The following layout is descriptive only; every cap listed here is **future**
until PM implements and validates it.

| Slot | Future cap | Description |
| --- | --- | --- |
| 0 | Driver Manager control endpoint | Future endpoint for DM control messages. |
| 1 | Driver registration endpoint | Future endpoint used by the driver to register readiness and device identity. |
| 2 | Fault/restart endpoint | Future endpoint for fault reporting or restart coordination, if separate from slot 0. |
| 3..N | MMIO caps | Future caps for validated MMIO ranges, one per granted window or a packed descriptor. |
| N+1..M | IRQ / notification caps | Future interrupt delivery or notification caps after explicit IRQ routing validation. |
| M+1..P | DMA / IOMMU caps | Future DMA authority only after IOMMU/DMA policy exists. |
| P+1 | Mailbox transport cap | Future cap for a real mailbox transport, cache, and MMIO policy. |
| P+2 | Devfs registration cap | Future authority to publish device nodes, if devfs policy allows it. |
| P+3 | Logging/debug cap | Future debug/logging endpoint only when existing policy allows it. |

The layout must remain independent of payload-encoded local cap numbers. Cap IDs
are cspace-local; authority must be delivered through the real startup/IPC cap
mechanism selected by PM and kernel policy.

## 5. Sender identity and anti-spoofing

- PM must verify that a `DriverSpawnRequest` came from the authorized Driver
  Manager using kernel-provided sender metadata.
- Driver Manager must verify the driver identity on registration using
  kernel-provided sender metadata and PM-returned handles/labels, not a
  self-claimed payload TID.
- Payload-claimed TIDs, image names, or record IDs are diagnostic and routing
  hints only; they are never authority.
- All privileged requests use kernel-provided sender identity metadata.
- Missing sender metadata fails closed at both PM and DM boundaries.

## 6. Resource validation rules

PM validation must happen before any process is made runnable:

- Requested resources must exist in the validated platform inventory.
- Requested resources must match the selected `device_record_id` and compatible
  string; a driver cannot borrow unrelated resources by listing them.
- RP1 GPIO requires PCIe discovery, RP1 BAR identification, BAR sizing, and BAR
  grant policy before it can be spawned live.
- Mailbox requires a real mailbox transport policy, cache-maintenance policy,
  and MMIO policy before it can be spawned live.
- IRQ routing must be explicit: interrupt controller/domain, line, trigger,
  polarity, target, and delivery mechanism must be known.
- DMA requires an IOMMU/DMA policy, allowed windows, ownership/accounting rules,
  and revocation semantics.
- Unknown, deferred, disabled, malformed, or unsupported devices cannot be
  spawned live.
- PM may reject otherwise valid resources if process, memory, cap-table, IRQ, or
  accounting limits would be exceeded.

## 7. Failure and rollback semantics

- PM validates all requested resources before spawning or making a task runnable.
- If capability minting or transfer setup fails, PM rolls back any caps minted for
  the attempted spawn.
- If address-space setup fails, PM destroys the partial process/address space and
  releases associated accounting.
- If process creation succeeds but startup-cap delivery fails, PM tears down the
  process before reporting failure to DM.
- If the driver starts but fails to register before the startup timeout, Driver
  Manager marks the driver failed and may later request a PM-mediated restart.
- If the driver crashes, PM reports death/liveness state to Driver Manager.
- Driver Manager decides whether policy permits restart, but PM performs the
  restart mechanism and may deny it for accounting, resource, or policy reasons.
- Driver Manager cannot directly restart a driver by spawning a process.

## 8. Health monitoring

- A driver registers with Driver Manager after startup and identifies the device
  record it serves.
- Driver Manager tracks status, registration deadline, heartbeat, crash count,
  dependency health, and restart policy state.
- PM owns process liveness and death notification because PM owns process
  creation and accounting.
- Restart requests flow Driver Manager → PM.
- PM may deny restart requests based on resource exhaustion, accounting limits,
  revoked resources, dependency loss, or isolation policy.

## 9. Raspberry Pi 5 notes

- PL011 can become the first live candidate once live MMIO and IRQ grant paths,
  clock/divisor policy, pinmux policy, and PM startup-cap delivery exist.
- RP1 GPIO remains blocked on PCIe/RP1 BAR discovery, BAR validation, interrupt
  model, and a capability-granted MMIO path. RP1 GPIO remains BAR-relative and
  must not be treated as direct BCM2712 MMIO.
- Mailbox remains blocked until real transport, cache-maintenance, bus-address,
  alignment, and MMIO policy exist.
- `irqmux_srv` remains blocked on a concrete interrupt-routing model for GIC/RP1
  interrupt domains and delivery caps/notifications.
- Block drivers remain blocked on SD/eMMC, xHCI/USB, PCIe/RP1, and real storage
  backend work.

## 10. Non-goals for DRS-6

DRS-6 explicitly does not add:

- live driver spawning;
- MMIO, IRQ, DMA, mailbox, PCIe BAR, pinmux, or clock grants;
- capability minting or transfer changes;
- live boot-DTB parsing;
- kernel ABI, syscall ABI, IPC ABI, cap-logic, scheduler, VM, trap-entry, RPi5
  boot, or init-bootstrap changes;
- PM or supervisor-service calls;
- live use of the inert DRS-7 request records, DRS-8 validation reports, or
  DRS-9 accounting reports as process-creation/accounting authority;
- service-manifest behavior changes.

Driver Manager remains advisory only in this stage.

## 11. Integration with DRS mock models

The existing DRS models map to the future contract as follows:

- `PlatformInventory` feeds request construction by describing candidate device
  records and their inert resources.
- `SpawnPlan` says what should exist according to DM policy and dependency
  ordering.
- `SpawnAuthorityDecision` represents mock PM approval/denial for tests; it is
  not a PM response and carries no authority.
- `ResourceGrantBundle` becomes the descriptive source for DRS-7
  `DriverSpawnRequest.resource_requirements`; it does not grant anything.
- DRS-7 `DriverSpawnRequestBundle` is the inert PM-facing projection of the
  inventory/plan/authority/grant-bundle pipeline. Approved PL011 entries may be
  `ReadyForPmValidation`; deferred, denied, unknown, and already-running entries
  remain inert records and are not spawn requests.
- DRS-8 `PmSpawnValidationReport` simulates whether PM would accept, reject,
  defer, mark unsupported, or no-op each inert request after checking mock
  verified DM identity, version, image policy, startup-cap layout, inventory
  resource matching, and conservative resource blockers.
- DRS-9 `PmSpawnAccountingReport` simulates descriptive PM reservations, commit
  outcomes, and reverse-order rollback plans for injected partial failures.
  Future live implementation must perform all real accounting and rollback inside
  PM, not Driver Manager.
- No mock model itself grants authority, mints caps, calls PM, creates a task, or
  touches MMIO. A possible next step is health/restart request simulation or a
  live-spawn API design review, still without live implementation.
