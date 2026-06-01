<!-- SPDX-License-Identifier: Apache-2.0 -->

# Capability Rights Width Audit

This note records the audit of widening `CapRights` from `u8` to `u16`.
It is intentionally documentation-only: the current ABI and kernel behavior keep
`CapRights(u8)` unchanged.

## Current allocation

`CapRights` is defined in `crates/yarm-kernel/src/capability.rs` as a private
`u8` newtype and is re-exported through the kernel and userspace runtime
facades. All eight bits are currently assigned:

| Bit | Mask | Right |
|-----|------|-------|
| 0 | `0x01` | `READ` |
| 1 | `0x02` | `WRITE` |
| 2 | `0x04` | `MAP` |
| 3 | `0x08` | `SEND` |
| 4 | `0x10` | `RECEIVE` |
| 5 | `0x20` | `SCHEDULE` |
| 6 | `0x40` | `SIGNAL` |
| 7 | `0x80` | `WAIT` |

There is no unused in-band bit in the current representation. A ninth right
cannot be represented by the current `u8` bitset.

## Kernel storage impact

A width change is mechanically small at the type definition, but it changes the
layout of kernel capability storage:

- `Capability { object: CapObject, rights: CapRights }` stores the rights value.
- `CapEntry` embeds `Capability` plus a parent `CapId`.
- `CapSlot` embeds `Option<CapEntry>` plus a generation.
- `CapabilitySpace` stores all `CapSlot`s in allocator-backed CNode slot arrays.

These types are not declared `repr(C)` and are not copied directly to userspace,
but widening can still affect memory footprint, enum niche use, padding, cache
behavior, and tests that compare extracted/re-exported type sizes.

`TransferEnvelope` and `ReplyCapRecord` do **not** store a standalone rights
field today. Transfer materialization re-resolves the source capability and
grants rights from the current `Capability`; reply-cap records store `CapObject`
and `CapId` state rather than a serialized rights mask.

## ABI-visible surface found

The current public IPC/syscall ABI mostly exposes **capability IDs**, not raw
capability-right masks:

- syscall arguments and returns carry `CapId` values in integer registers;
- recv-v2 metadata is 40 bytes and carries status/opcode/message flags,
  payload length, receiver-local transferred `CapId`, recv-meta flags, and
  sender/status lanes, but no rights mask;
- `Message` carries a transferred `CapId` (`u64`) plus message flags/opcode and
  payload bytes, but no rights mask;
- startup slots, SpawnV5, VFS grant replies, and driver-manager grant replies
  carry cap IDs as `u64`/`u32`-style scalar values or transferred caps, not
  rights masks;
- `yarm-ipc-abi` service protocols encode cap IDs and operation flags, not
  `CapRights` bitsets.

The main ABI-visible `CapRights` exposure is the Rust API surface: `CapRights`
is re-exported from `yarm-user-rt` and from the kernel extraction bridge, and
`tests/extraction_bridge_tests.rs` asserts that extracted/re-exported
`CapRights` has the same size as `yarm_kernel::capability::CapRights`. Changing
it to `u16` is therefore observable to Rust users and compatibility tests even
if the syscall wire format keeps passing only cap IDs.

## Layout-sensitive structures and tests

Known layout- or size-sensitive items affected or requiring review if rights are
widened:

- `crates/yarm-kernel/src/capability.rs`
  - `CapRights` representation and `bits()` return type;
  - `Capability`, `CapEntry`, `CapSlot`, `CapabilitySpace` footprint;
  - capability unit tests for bit operations and bridge assumptions.
- `src/kernel/capabilities.rs`
  - `Capability::rights_bits()` currently returns `u8`;
  - all grant/derive/has-right call sites use `CapRights` directly.
- `tests/extraction_bridge_tests.rs`
  - explicitly compares `size_of::<CapRights>()` across kernel extraction
    boundaries.
- `crates/yarm-user-rt/src/lib.rs`
  - re-exports `CapRights`, so width changes are visible to userspace Rust
    crates.
- Any future serialized structure that adds a rights field must choose an
  explicit width and must not rely on Rust layout.

No current recv-v2 metadata, `Message`, `SpawnV5CapArgs`, startup slots,
VFS `FileGrantRo*`, or driver-manager grant payload inspected in this audit
stores raw `CapRights` bits.

## ABI versioning impact

Widening `CapRights` everywhere is **not safe as an unqualified ABI v10 change**.
Even if the register-level syscall ABI and IPC message metadata do not serialize
rights bits today, the public Rust-facing ABI exposes `CapRights` as a type and
contains size bridge tests. A full `u16` widening would require at least a staged
compatibility plan and should be treated as ABI v11 if any public/userspace API
or serialized protocol begins accepting or returning rights masks wider than
`u8`.

Under the current ABI v10 policy, adding a ninth public/user-visible right by
widening an existing rights field would be an incompatible public struct/flag
meaning change. It should not land silently inside v10.

## Migration options

### A. Widen `CapRights` to `u16` everywhere now

Not recommended for v10. This is the simplest code change but creates observable
Rust API/layout changes and requires a full audit of every cap-bearing type,
bridge test, and userspace crate. If chosen, treat it as ABI v11 work.

### B. Widen kernel-internal rights to `u16`, keep v10 ABI fields packed as `u8`

Possible as a staged migration, but only if all public packing/unpacking paths
remain explicit and reject/strip extension bits at v10 boundaries. This would
need a new internal/external conversion layer and tests proving no v10 wire path
emits the high byte.

### C. Keep `u8` for v10 and reserve widening for ABI v11

Recommended. The rights budget is exhausted, and documentation should treat the
next right as ABI-planning work rather than a trivial bit addition.

### D. Add a second extension-rights field in a future ABI

Viable for ABI v11 if preserving the low-byte meaning is important. This avoids
renumbering existing rights bits but still requires new struct/protocol layout
and explicit fallback behavior for v10 peers.

## Recommendation

Choose **C** for v10: keep `CapRights(u8)`, document that the rights bit budget
is exhausted, and require an ABI v11/staged migration before adding a ninth
right. If a new right becomes urgent, prototype option **B** internally only
with strict v10 boundary tests, or design option **D** as the ABI v11 wire
format.
