<!--
SPDX-License-Identifier: Apache-2.0
Copyright 2026 Umut Deniz Balkan
-->

# IRQMUX Userspace Contract (IRQMUX-2)

## Scope and boundary

IRQMUX defines a **userspace-only** interrupt-routing and authorization model.
It does not connect hardware interrupt controllers, kernel trap paths,
scheduler events, or syscall behavior to the service. The only event source in
this phase is the explicit fake/test dispatch model.

The service owns bounded route and grant-policy state. A route identifies a
logical IRQ line and vector, trigger mode and polarity, an authorized owner, an
optional userspace-local target ID, and independent enabled and masked gates.
A target ID and grant ID are placeholders for future endpoint/capability-backed
objects. Neither is interpreted as a kernel capability, and IRQMUX does not send
an IPC interrupt event to the target.

## ABI version 2

IRQMUX ABI version 2 adds explicit authorization operations and owner identity
to the IRQMUX-1 route operations. Requests and responses are fixed-size,
little-endian payloads with zero-checked reserved fields.

| Operation | Purpose |
| --- | --- |
| `AUTHORIZE_GRANT` | Install or rotate a driver-manager-produced userspace grant descriptor. |
| `REVOKE_GRANT` | Remove a matching live grant and make its route inert. |
| `REGISTER_LINE` | Register the exact line/vector/configuration named by a grant. |
| `UNREGISTER_LINE` | Remove the route and its authorization record. |
| `BIND_DRIVER` / `UNBIND_DRIVER` | Change the owner route's service-local target. |
| `ENABLE` / `DISABLE` | Control the owner route's administrative delivery gate. |
| `MASK` / `UNMASK` | Control the owner route's interrupt mask gate. |
| `ACK` | Validate owner acknowledgement; hardware EOI remains deferred. |
| `INJECT_TEST_IRQ` | Exercise fake dispatch without claiming hardware delivery. |
| `GET_STATUS` | Return route, owner, authorization, target, and gate state. |

In addition to the IRQMUX-1 statuses, version 2 defines `Unauthorized`,
`GrantNotFound`, `GrantStale`, `GrantMismatch`, and `RightsMissing`. Unknown
rights bits and malformed/reserved fields are rejected as `BadRequest`.

## Driver-manager IRQ grant audit

The IRQMUX-2 userspace integration audit classifies the existing driver-manager
model as follows:

- **A — driver identity available:** driver manager registers and addresses a
  driver by task ID; IRQMUX uses that value as the placeholder `driver_id`.
- **B — IRQ grant exists but lacks an IRQMUX token:** the live `GRANT_IRQ`
  request identifies a driver and IRQ line and returns a transferred IRQ
  capability, but it does not return an IRQMUX grant ID, generation, vector,
  trigger mode, polarity, or rights mask.
- **C — existing grant identity is not sufficient:** the line and transferred
  capability are enough for the current driver-manager operation, but not for
  IRQMUX's explicit line/vector/configuration ownership checks.
- **D — IRQMUX therefore keeps its own authorization table:** the bounded table
  stores the opaque grant key, exact interrupt configuration, and rights until
  protected capability-backed authorization is integrated.
- **E — live driver-manager behavior remains unchanged:** IRQMUX-2 adds only the
  shared descriptor-construction helper and contract tests; it does not alter
  `REGISTER`, `GRANT_IRQ`, `GRANT_DMA`, `RESTARTED`, spawning, or boot policy.
- **F — no docs-only blocker:** the userspace placeholder model is executable
  and tested, while authenticating the grant authority and validating kernel
  IRQ capabilities are explicitly deferred integration work.

## Grant descriptor and driver-manager relationship

The shared `IrqGrantDescriptor` contains:

- opaque `grant_id`;
- `driver_id`, currently corresponding to driver manager's registered task ID;
- nonzero `generation`;
- IRQ line and vector;
- edge/level trigger mode and high/low polarity;
- a rights bitmask.

The current live driver-manager `GRANT_IRQ` path still receives a driver task ID
and IRQ line, asks its runtime adapter to mint/grant an IRQ capability, and
returns the line plus the transferred capability. IRQMUX-2 deliberately does
not change that live behavior. The driver ABI provides a helper for constructing
the userspace IRQMUX descriptor so driver-manager-side code and IRQMUX share one
wire model, but no live boot path invokes the helper yet.

The grant token is **not a kernel capability**, is not cryptographic, and does
not prove that the sender possesses the transferred IRQ capability. Likewise,
`AUTHORIZE_GRANT` and `REVOKE_GRANT` do not yet authenticate a privileged sender.
A future integration must bind these operations to driver-manager authority and
validate a real kernel-issued IRQ capability or equivalent protected object.

## Rights and route ownership

The rights bits are:

- `REGISTER`: register or unregister the route;
- `BIND`: bind or unbind a target;
- `ENABLE`: enable or disable the route;
- `MASK`: mask or unmask the route;
- `ACK`: acknowledge the route.

IRQMUX keeps fixed-capacity, no-heap tables of 32 grants and 32 routes. Only one
live grant may authorize a given IRQ line. A grant must exist before route
registration. Registration supplies an owner key and exact line, vector,
trigger, and polarity; every field must match the authorized descriptor.

A registered route stores the grant key as its owner. Bind/unbind,
enable/disable, mask/unmask, acknowledge, and unregister requests must present
the matching grant ID, driver ID, generation, line, and required right. A wrong
driver or route identity returns `GrantMismatch`; an older generation returns
`GrantStale`; a missing right returns `RightsMissing`; and an absent grant
returns `GrantNotFound`.

An identical duplicate authorization is idempotent. A higher generation may
rotate the same grant subject (same grant ID, driver, line, vector, trigger, and
polarity), including a changed rights mask. Rotation clears the target, disables
and masks the route, and updates its owner generation before further control is
allowed. A lower or equal non-identical generation is stale.

## Registration and revocation policy

A newly registered route is disabled, masked, and unbound. The normal setup
sequence is:

1. Driver manager constructs and authorizes a grant descriptor.
2. The driver registers the exact granted route.
3. The owner binds its target.
4. The owner enables the route.
5. The owner unmasks the route.

Owner-initiated `UNREGISTER_LINE` removes both the route and its authorization
record. Administrative `REVOKE_GRANT` removes the authorization but retains the
route as inert audit/configuration state: its target is cleared, it is disabled,
and it is masked. Without a newly authorized matching grant, subsequent owner
control requests fail with `GrantNotFound`.

## Fake dispatch model

Fake dispatch remains independent of caller authorization because it represents
a future interrupt event entering an already-configured route, not a route
control operation. It evaluates state in this order:

1. Unregistered line -> `Unregistered`.
2. Disabled route -> `Disabled`.
3. Masked route -> `Masked`.
4. Missing target -> `NoTarget`.
5. Otherwise -> `Delivered { line, target }`.

`Delivered` is an internal test result only. It is not an IPC send, kernel
notification, hardware acknowledgement, or end-of-interrupt operation.
Unauthorized configuration is never installed, and revocation forces the route
back to the disabled/masked/unbound gates.

## Service process behavior

`irqmux_srv` emits binary and service entry markers, constructs empty route and
grant tables, and remains resident. If a receive endpoint exists, it accepts
fixed-size IRQMUX control messages and replies with encoded status. Unknown
operations are rejected cleanly. If no endpoint exists, it yields while
remaining resident.

No runtime spawn ordering or policy is defined or changed by this contract.

## Architecture neutrality

The contract intentionally avoids architecture-specific interrupt-controller
data. Future hardware integration may translate authorized routes to:

- x86_64 APIC, MSI, or MSI-X programming;
- AArch64 GIC routing and acknowledgement;
- RISC-V PLIC or IMSIC routing and completion.

Those backends must preserve the userspace ownership contract while keeping
controller-specific details outside this ABI.

## Deferred integration

The following work remains explicitly deferred:

- authenticating driver manager as the grant-authority sender;
- validating and binding real kernel IRQ capabilities to grant descriptors;
- kernel-to-userspace IRQ event delivery;
- endpoint/capability target validation and IPC notification;
- interrupt-controller route, mask, and unmask programming;
- hardware acknowledgement/EOI ordering, especially for level-triggered IRQs;
- event coalescing, backpressure, fairness, and lost-interrupt accounting;
- architecture-specific APIC/MSI/MSI-X, GIC, PLIC, or IMSIC adapters.
