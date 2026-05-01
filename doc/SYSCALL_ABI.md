<!-- SPDX-License-Identifier: Apache-2.0 -->

# YARM Syscall ABI v10 (Frozen Contract)

- ABI Version: `10`
- Syscall count: `15`

## Syscall numbers

- `0`: `Yield`
- `1`: `IpcSend`
- `2`: `IpcRecv`
- `3`: `VmMap` (YARM-native VM map syscall, capability-targeted)
- `4`: `TransferRelease` (release a recv auto-mapped shared-memory transfer)
- `5`: `IpcRecvTimeout` (bounded non-blocking receive with scheduler-yield retry budget)
- `6`: `IpcCall` (send with kernel-minted ephemeral reply-cap transfer)
- `7`: `IpcReply` (consume reply-cap and send reply to bound caller endpoint)
- `8`: `ControlPlaneSetCnodeSlots` (control-plane cnode slot-capacity resize by target process id)
- `9`: `FutexWait` (`arg0=addr`, `arg1=expected`, `arg2=observed`)
- `10`: `FutexWake` (`arg0=addr`, `arg1=max_wake`)
- `11`: `SpawnThread` (`arg0=tls_base`, `arg1=user_stack_top`, `arg2=user_entry`)
- `12`: `Fork` (fork current process with CoW; return child tid in parent)
- `13`: `VmAnonMap` (reserved; currently returns `InvalidArgs`)
- `14`: `VmBrk` (reserved; currently returns `InvalidArgs`)

## Syscalls `9..14` status

- `9` `FutexWait`: exposed and wired.
- `10` `FutexWake`: exposed and wired.
- `11` `SpawnThread`: exposed and wired.
- `12` `Fork`: exposed and wired.
- `13` `VmAnonMap`: reserved syscall number; current implementation is a stub returning `InvalidArgs`.
- `14` `VmBrk`: reserved syscall number; current implementation is a stub returning `InvalidArgs`.

## Futex safety contract

- `FutexWait`/`FutexWake` validate the futex address as a userspace `u32` word (4 bytes) before acting.
- Kernel/high-half, non-user, and unmapped addresses are rejected with user-memory fault/error mapping rather than being treated as trusted kernel pointers.

## Fork contract (current behavior)

- Child is created via CoW address-space clone and resumes from copied parent user context.
- Child return register (`arg0` / arch return register, e.g. `x0`) is set to `0`.
- Child preserves parent `arg1..arg5`.
- Parent `Fork` syscall return is child TID (`ret0`).
- If child inherits a TLS pointer, TLS-restore pending is enqueued for child.
- Child robust-futex state is initialized empty (no inherited robust-futex head/list record).

## CoW staged limitation and exhaustion behavior

- CoW tracking is fixed-size and staged:
  - `hosted-dev`: `MAX_COW_PAGES = 1024`
  - freestanding (`not(feature = "hosted-dev")`): `MAX_COW_PAGES = 256`
- CoW table exhaustion returns `MemoryObjectFull`.
- Failed CoW clone/fork now cleans up partial child clone state by destroying the in-progress child ASID/mappings and clearing child CoW records before returning error.
- Dynamic or physical-frame-bounded CoW tracking remains future work.

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

### `ControlPlaneSetCnodeSlots` argument layout

- `args[0]`: target process id (`pid != 0`)
- `args[1]`: requested cnode slot capacity (kernel clamps to `[1, MAX_CAPABILITIES_PER_CSPACE_HARD]`)
- `args[2..5]`: reserved (must be `0`)

`ControlPlaneSetCnodeSlots` semantics:

- Caller policy:
  - `TaskClass::SystemServer`: may resize any target process cnode.
  - non-system-server caller: may only resize its own process cnode.
- Existing process cnode:
  - resized in-place if present.
- Missing process cnode:
  - kernel creates process cnode binding with requested capacity (subject to policy and global slot budget).
- Errors:
  - `InvalidArgs` for `pid == 0` and malformed reserved-arg usage.
  - `MissingRight` when caller policy does not permit targeting the requested process.
  - `Internal` when kernel capacity/budget constraints reject the resize path.

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

## Phase 6 migration policy (ABI v10 window)

- **Timed wait migration target**: control-plane services should migrate blocking receive loops to `IpcRecvTimeout` with explicit tick budgets; indefinite waits are allowed only where watchdog/supervisor policy explicitly permits.
- **Request/reply migration target**: for standard RPC flows, use `IpcCall`/`IpcReply` single-use reply-cap semantics instead of maintaining ad-hoc reply endpoints.
- **Legacy choreography deprecation**: two-endpoint request/reply choreography is deprecated for new or updated core services during Phase 6. Existing deployments remain supported during the migration window.
  - **waiver ledger source**: `PHASE6_EXIT_GATE_REPORT.md`.
- **Shared-memory lifecycle requirement**: services receiving shared-memory auto-maps must complete `TransferRelease` as the release primitive; manual side-band cleanup protocols are deprecated.
- **Removal gate**: legacy request/reply choreography removal is deferred until all core control-plane services are migrated and Phase 6 exit criteria are met.
- **Current migration snapshot**: Phase 6 pass 2 migrates the VFS control-plane kernel-IPC roundtrip receive path to bounded timed receives (`IpcRecvTimeout`-equivalent kernel deadline receive), establishing the first service-level cut.
- **Current migration snapshot (pass 3)**: VFS migration path now uses an explicit receive-budget helper for timed queue draining, including zero-tick budget coverage for already-queued request/reply flow.
- **Current migration snapshot (pass 4)**: VFS migration includes a deprecation guardrail test that blocks regression to legacy blocking `IpcRecv` usage in the migrated control-plane roundtrip path.
- **Current migration snapshot (pass 5)**: VFS migration pass-2/3/4 bundle is validated together, with stabilized deprecation guardrail enforcement against legacy blocking `IpcRecv` regression.
- **Current migration snapshot (pass 6)**: Supervisor control-plane receive loop now uses budget-aware receive draining (nonblocking probe plus timed receive when possible), with guardrails against regression to legacy blocking `IpcRecv`.
- **Current migration snapshot (pass 7)**: control-plane module-level guardrails now assert migrated VFS/supervisor sources remain free of legacy blocking `IpcRecv` call-sites.
- **Current migration snapshot (pass 13)**: Process Manager kernel-IPC roundtrip now uses reply-cap call/reply choreography (`create_reply_cap_for_caller` + `ipc_reply`) instead of ad-hoc two-endpoint server reply sends.
- **Current migration snapshot (pass 14)**: VFS kernel-IPC roundtrip now uses reply-cap call/reply choreography (`create_reply_cap_for_caller` + `ipc_reply`) instead of ad-hoc two-endpoint server reply sends.
- **Current migration snapshot (pass 19)**: Phase 6 service migration/deprecation slices are complete at the service level; core control-plane services are recorded as migrated or dated-waived in `PHASE6_SERVICE_MIGRATION_MATRIX.md`, with final global sign-off held until remaining Phase 4 lifecycle closure.

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

## TODO / design note: inline IPC register payload expansion

- `IPC_REGISTER_WORDS` remains **2** for the current ABI generation.
- Raising inline register payload width to **8 words** (64 bytes on 64-bit targets) is **not** blocked by `Message::MAX_PAYLOAD`; that limit is currently **128** bytes.
- The real blocker is syscall/register ABI plumbing:
  - current syscall ABI provides **6** argument lanes and they are already allocated (`cap`, `ptr`, `len`, two inline lanes, transfer-cap lane);
  - `inline_payload_from_frame` currently extracts exactly **2** inline lanes from the trap frame.
- Moving to 8 inline words therefore requires either:
  - a syscall ABI v2 with expanded argument/register mapping, or
  - an alternate payload path that preserves existing syscall argument ABI compatibility.
