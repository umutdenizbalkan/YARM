<!-- SPDX-License-Identifier: Apache-2.0 -->

# YARM Syscall ABI v10 (Frozen Contract)

- ABI Version: `10`
- Syscall count: `22`

## Syscall numbers

- `0`: `Yield`
- `1`: **Reserved (legacy `IpcSend`, removed from active dispatch)**
- `2`: **Reserved (legacy `IpcRecv`, removed from active dispatch)**
- `3`: `VmMap` (YARM-native VM map syscall, capability-targeted)
- `4`: `TransferRelease` (release a recv auto-mapped shared-memory transfer)
- `5`: **Reserved (legacy `IpcRecvTimeout`, removed from active dispatch)**
- `6`: **Reserved (legacy `IpcCall`, removed from active dispatch)**
- `7`: **Reserved (legacy `IpcReply`, removed from active dispatch)**
- `8`: `ControlPlaneSetCnodeSlots` (control-plane cnode slot-capacity resize by target process id)
- `9`: `FutexWait` (`arg0=addr`, `arg1=expected`, `arg2=observed`)
- `10`: `FutexWake` (`arg0=addr`, `arg1=max_wake`)
- `11`: `SpawnThread` (`arg0=tls_base`, `arg1=user_stack_top`, `arg2=user_entry`)
- `12`: `Fork` (fork current process with CoW; return child tid in parent)
- `13`: `VmAnonMap` (staged anonymous MemoryObject allocation+mapping for service buffers)
- `14`: `VmBrk` (staged: query + grow supported, shrink unsupported)
- `19`: `VmUnmap` (staged producer-local anonymous mapping cleanup for current task/ASID)
- `20`: `CapRelease` (staged producer-local capability revoke/drop in current task cnode)
- `21`: `DebugSerialWrite` (**diagnostic-only**, writes one raw serial byte)

## Syscalls `9..14` status

- `9` `FutexWait`: exposed and wired.
- `10` `FutexWake`: exposed and wired.
- `11` `SpawnThread`: exposed and wired.
- `12` `Fork`: exposed and wired.
- `13` `VmAnonMap`: staged syscall; caller-specified non-zero page-aligned VA, READ|WRITE anonymous map, returns mapped base/rounded length/MemoryObject cap.
- `14` `VmBrk`: staged syscall; query and grow are supported, shrink is currently unsupported.

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

## `VmBrk` staged contract (syscall `14`)

- Current implementation is intentionally minimal and **per-task**.
- Staged ownership gate: only thread-group leader may issue `VmBrk`; non-leader threads are rejected.
- `args[0] == 0`: query current break.
  - Returns current `brk_end` when bounds exist for current tid.
  - Returns `0` when no brk bounds are set yet.
- `args[0] > 0`: grow request.
  - Validates userspace range (must be below kernel-space split).
  - If no bounds exist yet, grow is rejected (`InvalidArgs`) to avoid creating an empty `[base,end)` heap window.
  - If bounds exist, only grows (`requested >= current_end`), and rejects requests below `base`.
  - Shrink (`requested < current_end`) is currently unsupported and rejected.
- Initial bounds are installed for the first user task at ELF boot/startup using:
  - `heap_base = page_align_up(max(PT_LOAD.p_vaddr + PT_LOAD.p_memsz))`
  - `set_task_brk_bounds(leader_tid, heap_base, heap_base)`
- Growth currently relies on existing demand-page-fault behavior in `[brk_base, brk_end)` to allocate/map heap pages lazily when touched.
- Fork staging rule: child process leader copies parent leader brk bounds (base/end).
- Spawn-thread staging rule: spawned threads do not receive independent copied brk bounds.
- Future work: move from tid-keyed staging toward process-wide brk ownership/model.

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

### `VmAnonMap` argument layout

- `args[0]`: target user virtual address (must be non-zero and page-aligned)
- `args[1]`: requested mapping length in bytes (`>0`, rounded up to page size)
- `args[2]`: protection flags bitmask (`READ=0x1`, `WRITE=0x2`, `EXEC=0x4`)
  - staged policy: requires `READ|WRITE`; `EXEC` is rejected
- `args[3]`: reserved (must be `0`)
- `args[4]`: reserved (must be `0`)
- `args[5]`: reserved (must be `0`)

`VmAnonMap` semantics (staged):

- allocates an anonymous `MemoryObject` sized to the rounded mapping length;
- rejects overlap: every page in `[base, base+rounded_len)` must be currently unmapped in the caller ASID;
- maps it into the caller's current user ASID at the provided base;
- returns mapping + transfer-cap tuple in return registers:
  - `ret0 = mapped_base_va`
  - `ret1 = mapped_len_rounded_to_page`
  - `ret2 = memory_object_cap_id` (caller-local cap, suitable for IPC transfer).

### `VmUnmap` argument layout

- `args[0]`: target user virtual base address (non-zero, page-aligned)
- `args[1]`: unmap length in bytes (`>0`, rounded up to page size)
- `args[2..5]`: reserved (must be `0`)

`VmUnmap` semantics (staged):

- unmaps each page in `[base, base+rounded_len)` from current task's current ASID;
- returns `InvalidArgs` if any page in the requested range is already unmapped;
- producer-local lifecycle primitive for cleanup after staged `VmAnonMap` usage.

### `CapRelease` argument layout

- `args[0]`: capability id in current task cnode
- `args[1..5]`: reserved (must be `0`)

`CapRelease` semantics (staged):

- revokes/drops the specified cap in the current task cnode;
- invalid/stale/non-present cap returns `InvalidCapability`;
- producer-local lifecycle primitive; distinct from receiver-side `TransferRelease`.

### `DebugSerialWrite` argument layout (diagnostic-only)

- Syscall number: `21`
- `args[0]`: byte lane; kernel writes `args[0] & 0xff` as exactly one serial byte
- `args[1..5]`: reserved (must be `0`)

`DebugSerialWrite` semantics:

- Writes exactly one byte to the architecture serial debug console path.
- Returns success with status in `ret0`:
  - `ret0=1`: byte emitted
  - `ret0=0`: debug serial disabled (no-op)
  - `ret1=0`, `ret2=0`
- Returns `InvalidArgs` when any reserved argument is non-zero.
- In non-debug kernel builds, syscall 21 remains present and returns success as a no-op.
- Performs no userspace pointer dereference and does not read/write userspace memory.

### `DebugSerialWriteBuf` argument layout (diagnostic-only)

- Syscall number: `22`
- `args[0]`: userspace pointer to marker bytes
- `args[1]`: marker byte length (`1..=256`)
- `args[2..5]`: reserved (must be `0`)

`DebugSerialWriteBuf` semantics:

- Validates user pointer range as userspace-accessible.
- Copies bytes from userspace into a fixed kernel buffer (max 256 bytes).
- Emits the full copied buffer through the active architecture debug console backend.
- Returns success with status in `ret0`:
  - `ret0=1`: buffer emitted/submitted
  - `ret0=0`: debug serial disabled or backend unavailable (non-fatal no-op)
  - `ret1=0`, `ret2=0`
- Returns `InvalidArgs` when pointer is null, length is zero/oversize, reserved arguments are non-zero, or pointer range is invalid.
- Diagnostic-only ABI, not a general logging/data plane syscall.

Temporary gating policy:

- Byte emission is enabled only in debug kernel builds (`cfg(debug_assertions)`).
- In non-debug builds it is intentionally a no-op-success path so diagnostic marker calls never become a fatal dependency.

### `SpawnThread` argument layout and runtime contract

- Syscall number: `11`
- `args[0]`: `tls_base`
- `args[1]`: `user_stack_top` (**must be 16-byte aligned**)
- `args[2]`: `user_entry`
- `args[3..5]`: reserved (must be `0`)

`SpawnThread` semantics:

- Parent/current thread id is derived by the kernel (not passed by userspace).
- New thread starts at `user_entry` with initial SP=`user_stack_top`.
- Initial user arg register lanes (`arg0..arg5`) are zeroed.
- TLS base is installed and TLS restore is marked pending for first resume/application.
- Returning from `user_entry` is currently undefined/unsupported unless userspace takes an explicit exit path.

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
- `10`: `BufferTooSmall`
- `255`: `Internal`

## Per-ISA shape source of truth

- Trap/syscall argument lane count is sourced from `crate::arch::syscall_abi` (`TRAPFRAME_ARG_REGS`).
- `src/kernel/syscall.rs` and `src/kernel/trapframe.rs` include compile-time assertions to keep ABI lane mapping synchronized with the selected ISA profile.


## Shared-reply adoption checklist

- See `doc/IPC_V2_SHARED_REPLY_ADOPTION.md` for service migration/adoption guidance and rollout checks.

## IPC ABI v2 status and semantics

- `IpcRegisterBlockV2` is active for v2 IPC syscalls and uses `inline_words: [u64; 8]` (up to 64-byte inline payloads).
- Active v2 syscall numbers:
  - `SYSCALL_IPC_SEND_V2_NR = 15`
  - `SYSCALL_IPC_RECV_V2_NR = 16`
  - `SYSCALL_IPC_CALL_V2_NR = 17`
  - `SYSCALL_IPC_REPLY_V2_NR = 18`
- Lane contract for v2 syscalls:
  - `arg0`: user pointer to `IpcRegisterBlockV2`
  - `arg1`: block size (`IPC_ABI_V2_BLOCK_SIZE`)
  - `arg2..arg5`: reserved for ABI compatibility
- Message opcode carriage contract (no block layout changes):
  - `IPC_SEND_V2`: request opcode is carried in `aux0` (`aux1` must be `0`).
  - `IPC_REPLY_V2`: reply opcode is carried in `aux0` (`aux1` must be `0`).
  - `IPC_CALL_V2`: request opcode is carried in `aux1`; `aux0` remains reply-receive endpoint cap.
  - `IPC_RECV_V2` success return: `ret_status` carries received `Message.opcode`.
  - `IPC_CALL_V2` success return: `ret_status` carries reply `Message.opcode`.

### `IPC_CALL_V2` behavior

### IPC v2 stage-1 shared-reply metadata convention (scaffolding)

- This is an ABI **convention** layer on top of existing `IPC_REPLY_V2` capability-transfer behavior.
- Server path:
  - reply using `IPC_REPLY_V2`;
  - set `IPC_V2_FLAG_TRANSFER_CAP`;
  - transfer-cap should be a MemoryObject-cap-like object suitable for explicit later mapping by the caller;
  - reply payload should carry `IpcV2SharedReplyMeta` bytes.
- Caller path:
  - receives `ret_transfer_cap` as the receiver-local transferred capability id;
  - decodes `IpcV2SharedReplyMeta` from reply payload;
  - maps transferred capability explicitly via VM/map syscalls later.
- Stage-1 intentionally has **no automatic mapping** in IPC syscall handling.
- Safety caveats:
  - mutability depends on transferred-cap rights and aliasing policy (`READ_ONLY` vs writable intent);
  - revocation/lifetime races remain possible and must be handled by caller map/use failure paths.
- Current kernel-policy baseline:
  - `REPLY_V2` enforces MemoryObject-only transfer-cap kind **when** reply payload decodes as `IpcV2SharedReplyMeta`;
  - `REPLY_V2` enforces `IpcV2SharedReplyMeta.offset + len <= MemoryObject.len` for valid shared-reply metadata payloads;
  - non-shared payloads preserve generic transfer-cap behavior (existing capability-existence validation);
  - services adopting shared replies should transfer MemoryObject-like caps only by policy;
  - map-time checks are still required for liveness/rights/revocation at use time;
  - read-only shared-reply behavior requires rights attenuation at cap creation/delegation time.

- `IPC_CALL_V2` is a combined operation: **send request + wait for reply** in one syscall.
- Userspace must pass reply receive endpoint cap in `IpcRegisterBlockV2.aux0`.
- On success, the reply is returned through `ret_*` fields and inline payload lanes in the same `IpcRegisterBlockV2` written back to userspace.
- Do **not** call `IPC_RECV_V2` after a successful `IPC_CALL_V2` for the same request; the reply has already been consumed by `IPC_CALL_V2`.

### Current reply-size limitation

- Inline fastpath remains unchanged: replies up to **64 bytes** are returned inline via `inline_words`.
- Stage 1 large-reply copyout support for `IPC_RECV_V2` and `IPC_CALL_V2`:
  - callers set `IPC_V2_FLAG_RECV_COPYOUT` to request copyout mode;
  - `aux1` is the userspace reply buffer pointer;
  - `len` is reply buffer capacity in bytes;
  - on success, kernel sets `IPC_V2_FLAG_RET_COPYOUT`, writes payload bytes to `aux1`, and reports actual reply size in `ret_len`;
  - no truncation is performed;
  - if `actual_reply_len > len` (capacity), returns `BufferTooSmall`.

- `yarm-user-rt` exposes additive wrappers `ipc_send_v2`, `ipc_recv_v2`, `ipc_call_v2`, and `ipc_reply_v2`.
- `IPC_RECV_V2` timeout contract: `aux0 = timeout_ticks`, `aux1 = 0` (reserved and must be zero).
- `IPC_RECV_V2` with `aux0 == 0` performs a nonblocking probe; `aux0 > 0` performs deadline receive using kernel timeout machinery.
- `yarm-user-rt::ipc_recv_v2_with_deadline` maps both `WouldBlock` and `TimedOut` to `Ok(None)` (matching v1 timeout wrapper behavior).
- `yarm-user-rt` additionally exposes additive transport scaffolding via `IpcTransportV2` + `SyscallIpcTransport` adapter and a `request_reply_v2(...)` helper for small typed control-plane call/reply decoding.
- Migration state: user-runtime IPC v1 API surface (`IpcTransport`, `ipc_send`, `ipc_recv`, `ipc_recv_with_deadline`, `ipc_call`) has been removed; v2 transport/wrappers are the default userspace IPC API.
- Supervisor runtime process-manager helper RPCs (restart-token query, supervised-task registration, execute-restart) now use `IpcTransportV2` + `request_reply_v2(...)`.
- Supervisor idle timeout wait path now uses v2 `IPC_RECV_V2` deadline receive (`recv_v2_with_deadline`); budgeted kernel-side control/fault polling remains unchanged.
- Supervisor runtime fault/control `recv_v2` path preserves explicit opcode-based routing (fault-report, task-exited, transfer-revoked, unknown-op ignore/log).
- Supervisor runtime v2 fault/control wire format is opcode-prefixed envelope (`u16 opcode` + payload) for explicit dispatch.
- Supervisor fault/control producers now emit opcode-prefixed payload envelopes:
  - fault report: `[opcode=FAULT_REPORT][17-byte fault wire]`
  - task exited: `[opcode=TASK_EXITED][TaskExitedEvent bytes]`
  - transfer revoked: `[opcode=TRANSFER_REVOKED][TransferRevokedEvent bytes]`
- Envelope format is now mandatory for supervisor runtime fault/control `recv_v2` dispatch.
- Legacy raw 17-byte fault payloads are no longer accepted by runtime decoder.
- Unknown-op behavior intentionally differs by path:
  - runtime loop logs/ignores unknown opcodes to keep supervisor progress/liveness;
  - `service_step`/test path returns `WrongObject` for stricter validation.
- POSIX-compat runtime `getpid` process-manager IPC path now uses `IpcTransportV2` request/reply (`request_reply_v2(...)`) rather than v1 transport send/recv choreography.
- Default user-runtime IPC path is v2 (`IpcTransportV2` and `ipc_*_v2` wrappers); legacy compatibility now exists only at kernel syscall ABI layer.
- Kernel v1 IPC syscall numbers are intentionally kept as reserved holes for ABI numbering stability; dispatch now returns deterministic legacy-removed errors for slots `1/2/5/6/7`.
