<!--
SPDX-License-Identifier: Apache-2.0
Copyright 2026 Umut Deniz Balkan
-->

# IRQMUX Userspace Contract (IRQMUX-1)

## Scope and boundary

IRQMUX-1 defines a **userspace-only** interrupt-routing contract and a
fixed-capacity service state machine. It does not connect hardware interrupt
controllers, kernel trap paths, scheduler events, or syscall behavior to the
service. The only event source in this phase is the explicit fake/test dispatch
model described below.

The service owns route policy state. A route identifies a logical IRQ line and
vector, its trigger mode and polarity, an optional userspace-local target ID,
and independent enabled and masked gates. A target ID is only a placeholder for
a future endpoint/capability association; IRQMUX-1 does not interpret it as a
kernel capability and does not send an IPC event to it.

## ABI

The ABI is version 1 and uses fixed-size, little-endian payloads. Operations are:

| Operation | Purpose |
| --- | --- |
| `REGISTER_LINE` | Add a line/vector with edge or level trigger and high or low polarity. |
| `UNREGISTER_LINE` | Remove the route and all binding/gate state. |
| `BIND_DRIVER` | Associate a nonzero service-local target ID with a registered line. |
| `UNBIND_DRIVER` | Remove the current target association. |
| `ENABLE` / `DISABLE` | Control the route's administrative delivery gate. |
| `MASK` / `UNMASK` | Control the route's interrupt mask gate. |
| `ACK` | Validate acknowledgement of a registered line; hardware EOI is deferred. |
| `INJECT_TEST_IRQ` | Exercise the fake dispatch path without claiming hardware delivery. |
| `GET_STATUS` | Return route metadata and gate flags. |

Responses use `Ok`, `NotFound`, `AlreadyRegistered`, `Busy`, `Masked`,
`Disabled`, `BadRequest`, and `Unsupported`. Unknown live opcodes receive an
`Unsupported` response, while malformed fixed-size payloads receive
`BadRequest`.

## Route state machine

IRQMUX stores at most `MAX_IRQ_ROUTES` routes in a fixed array and requires no
heap allocation. A newly registered route is deliberately **disabled and
masked**, with no target. The expected setup sequence is:

1. Register the line and vector.
2. Bind a driver target.
3. Enable the route.
4. Unmask the route.

Disable and mask are independent. Unregister removes the whole route, including
its target association. Duplicate registration is rejected, and registration
returns `Busy` when the fixed table is full.

Fake dispatch evaluates state in this order:

1. Unregistered line -> `Unregistered`.
2. Disabled route -> `Disabled`.
3. Masked route -> `Masked`.
4. Missing target -> `NoTarget`.
5. Otherwise -> `Delivered { line, target }`.

`Delivered` is an internal result only. It does not represent an IPC send,
kernel notification, interrupt-controller acknowledgement, or end-of-interrupt
operation.

## Driver-manager relationship

The current driver manager already models driver registration and an IRQ grant:
it can request that an IRQ capability be minted for a line and granted to a
driver task. IRQMUX-1 does not change that behavior and does not consume those
capabilities.

A later userspace integration should define the authority check and message flow
between driver manager, IRQMUX, and the driver service. Conceptually, driver
manager will grant the relevant IRQ authority, then an authorized driver will
bind its IRQMUX route to a service endpoint or capability-backed target. The
service-local `u64` target in IRQMUX-1 reserves the routing concept without
prematurely defining that capability-transfer protocol.

## Architecture neutrality

The route contract intentionally avoids architecture-specific controller data.
Future hardware integration may translate routes to:

- x86_64 APIC, MSI, or MSI-X programming;
- AArch64 GIC routing and acknowledgement;
- RISC-V PLIC or IMSIC routing and completion.

Those backends must preserve the userspace contract while keeping
controller-specific details outside this ABI.

## Service process behavior

`irqmux_srv` emits explicit binary and service entry markers, constructs an
empty `IrqMuxService`, and remains resident. If a receive endpoint exists, it
accepts fixed-size IRQMUX control messages and replies with encoded route status.
Unknown operations are rejected cleanly. If no endpoint exists, it yields while
remaining resident.

The `INJECT_TEST_IRQ` request and direct `dispatch_fake_irq` method only drive
the internal state machine. No live request causes delivery to the bound target
in IRQMUX-1.

## Deferred integration

The following work is explicitly deferred:

- kernel-to-userspace IRQ event delivery;
- endpoint/capability target validation and IPC notification;
- driver-manager IRQ grant-to-bind authorization;
- interrupt-controller mask/unmask and route programming;
- hardware acknowledgement/EOI semantics, including level-triggered ordering;
- event coalescing, backpressure, fairness, and lost-interrupt accounting;
- architecture-specific APIC/MSI/MSI-X, GIC, PLIC, or IMSIC adapters.
