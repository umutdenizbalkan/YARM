<!-- SPDX-License-Identifier: Apache-2.0 -->

# YARM Syscall ABI v9 (Frozen Contract)

- ABI Version: `9`
- Syscall count: `8`

## Syscall numbers

- `0`: `Yield`
- `1`: `IpcSend`
- `2`: `IpcRecv`
- `3`: `VmMap` (YARM-native VM map syscall, capability-targeted)
- `4`: `TransferRelease` (release a recv auto-mapped shared-memory transfer)
- `5`: `IpcRecvTimeout` (bounded non-blocking receive with scheduler-yield retry budget)
- `6`: `IpcCall` (send with kernel-minted ephemeral reply-cap transfer)
- `7`: `IpcReply` (consume reply-cap and send reply to bound caller endpoint)

## Argument register layout (`args[0..]`)

- `args[0]`: endpoint capability id
- `args[1]`: user pointer (small copy path) or shared-region sender offset (grant path)
- `args[2]`: length
- `args[3]`: inline payload lane 0 (kernel/no-ASID path) and recv metadata lane 0
- `args[4]`: inline payload lane 1 (kernel/no-ASID path) and recv metadata lane 1
- `args[5]`: optional transfer capability id (`0` or `u64::MAX` => none)

### `IpcRecvTimeout` argument layout

- `args[0]`: receive endpoint capability id
- `args[1]`: user receive buffer pointer
- `args[2]`: user receive buffer length
- `args[3]`: timeout budget in scheduler ticks (`0` means immediate probe)
- `args[4]`: reserved (must be `0`)
- `args[5]`: reserved (must be `0`)

`IpcRecvTimeout` semantics:

- `args[3] == 0`: immediate probe, empty queue returns `WouldBlock`.
- `args[3] > 0`: kernel arms a timed wait-state and returns `TimedOut` when the deadline expires without a message.

### `IpcCall` argument layout

- `args[0]`: send endpoint capability id
- `args[1]`: payload pointer (user) or inline lane source selector (kernel/no-ASID)
- `args[2]`: payload length (must be `<= Message::MAX_PAYLOAD`)
- `args[3..4]`: inline payload lanes for kernel/no-ASID path
- `args[5]`: caller reply-receive endpoint capability id (kernel mints and transfers ephemeral reply cap)

### `IpcReply` argument layout

- `args[0]`: reply capability id (`CapObject::Reply` with `SEND` right)
- `args[1]`: payload pointer (user) or inline lane source selector (kernel/no-ASID)
- `args[2]`: payload length (must be `<= Message::MAX_PAYLOAD`)
- `args[3..4]`: inline payload lanes for kernel/no-ASID path
- `args[5]`: reserved (must be `0`)

### `VmMap` argument layout

- `args[0]`: address-space mapping capability id (`CapId`)
- `args[1]`: virtual address (page-aligned)
- `args[2]`: mapping length in bytes (rounded up to page size)
- `args[3]`: protection flags bitmask (`READ=0x1`, `WRITE=0x2`, `EXEC=0x4`)
- `args[4]`: reserved (must be `0`)
- `args[5]`: reserved (must be `0`)

### `TransferRelease` argument layout

- `args[0]`: receiver-local transferred capability id (`CapId`)
- `args[1]`: mapped virtual base address (page-aligned)
- `args[2]`: mapped length in bytes (rounded up to page size by kernel)
- `args[3..5]`: reserved (must be `0`)

`TransferRelease` fast path for steady-state recycle loops:

- `args[1]=0` and `args[2]=0` means “release via active mapping record”.
- Kernel looks up the receiver’s active transfer mapping for `args[0]` and unmaps/revokes using recorded bounds.

## IPC model details

- **Synchronous rendezvous-friendly path**: small payloads (up to register lanes) can be passed through register lanes without kernel-side user-buffer copying on kernel/no-ASID paths.
- **Capability transfer opportunity on each IPC**: send can optionally attach a capability; recv returns transferred cap id in `ret2`.
- **Inline payload freeze**: `Message::MAX_PAYLOAD` is frozen at **128 bytes** for the current ABI generation.
- **Medium payload policy** (`129..=1024` bytes): fragmentation protocol (see `IPC_FRAGMENTATION_POLICY.md`).
- **Large payload shared-memory path**: if send length exceeds `Message::MAX_PAYLOAD`, sender provides a transferable memory capability and kernel sends `OPCODE_SHARED_MEM` metadata (`SharedMemoryRegion { offset, len }`).
- **Shared-memory recv map intent (optional)**: for user-mode `IpcRecv` on `OPCODE_SHARED_MEM`, `args[4]` may carry map-intent bits (`READ=0x1`, `WRITE=0x2`); `0` keeps default read+write mapping intent. If `WRITE` is requested, transferred capability must also carry `WRITE`.
- **Shared-memory recv attenuation**: when map-intent omits `WRITE`, receiver-local transferred capability is attenuated to `READ|MAP`.

## Shared-memory contract freeze (Phase 6)

- For user tasks (tasks with a bound user ASID), `IpcRecv` for `OPCODE_SHARED_MEM` **requires** a non-zero mapping target pointer and sufficient length budget.
- Compatibility descriptor-only receive fallback has been removed for user-mode shared-memory receives.
- Receiver-side auto-map + active mapping tracking + `TransferRelease` lifecycle is the required migration path.

## Phase 6 migration policy (ABI v9 window)

- **Timed wait migration target**: control-plane services should migrate blocking receive loops to `IpcRecvTimeout` with explicit tick budgets; indefinite waits are allowed only where watchdog/supervisor policy explicitly permits.
- **Request/reply migration target**: for standard RPC flows, use `IpcCall`/`IpcReply` single-use reply-cap semantics instead of maintaining ad-hoc reply endpoints.
- **Legacy choreography deprecation**: two-endpoint request/reply choreography is deprecated for new or updated core services during Phase 6. Existing deployments remain supported during the migration window.
- **Shared-memory lifecycle requirement**: services receiving shared-memory auto-maps must complete `TransferRelease` as the release primitive; manual side-band cleanup protocols are deprecated.
- **Removal gate**: legacy request/reply choreography removal is deferred until all core control-plane services are migrated and Phase 6 exit criteria are met.

## Return layout

- `ret0`: status/value (sender tid for recv)
- `ret1`: auxiliary value (message length)
- `ret2`: transferred capability id (`u64::MAX` sentinel when none)
- `error`: syscall error code (`0` means success)

## Error codes

- `1`: `InvalidNumber`
- `2`: `InvalidArgs`
- `3`: `InvalidCapability`
- `4`: `MissingRight`
- `5`: `WrongObject`
- `6`: `QueueFull`
- `7`: `WouldBlock`
- `8`: `PageFault`
- `9`: `TimedOut`
- `255`: `Internal`

## Per-ISA shape source of truth

- Trap/syscall argument lane count and IPC inline register-lane width are sourced from `crate::arch::syscall_abi` (`TRAPFRAME_ARG_REGS`, `IPC_REGISTER_WORDS`).
- `src/kernel/syscall.rs` and `src/kernel/trapframe.rs` include compile-time assertions to keep ABI lane mapping synchronized with the selected ISA profile.
