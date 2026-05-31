<!-- SPDX-License-Identifier: Apache-2.0 -->

# YARM Syscall ABI v10 (Frozen Contract)

- ABI Version: `10`
- Syscall count: `16`

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
- `14`: `VmBrk` (staged: query, grow, and page-granular shrink supported)
- `27`: `InitramfsReadChunk` (**PM-only / privileged**, Phase 2A/2B bootstrap bridge — see below)

## Syscalls `9..14` status

- `9` `FutexWait`: exposed and wired.
- `10` `FutexWake`: exposed and wired.
- `11` `SpawnThread`: exposed and wired.
- `12` `Fork`: exposed and wired.
- `13` `VmAnonMap`: reserved syscall number; current implementation is a stub returning `InvalidArgs`.
- `14` `VmBrk`: staged syscall; query, grow, and page-granular shrink are supported.

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
- `args[0] > 0`: set-break request.
  - Validates userspace range (must be below kernel-space split).
  - If no bounds exist yet, non-query requests are rejected (`InvalidArgs`) to avoid creating an empty `[base,end)` heap window.
  - Rejects requests below `base`.
  - Growth (`requested >= current_end`) only updates the byte-granular `brk_end`; heap pages continue to be mapped lazily on demand faults.
  - Shrink (`requested < current_end`) updates `brk_end` after page-granular unmap bookkeeping succeeds. The unmap window is `align_up(requested, PAGE_SIZE)..align_up(current_end, PAGE_SIZE)`, so the partially used page containing `requested` is preserved and lazy/unmapped pages in the shrink range are skipped safely.
- Initial bounds are installed for the first user task at ELF boot/startup using:
  - `heap_base = page_align_up(max(PT_LOAD.p_vaddr + PT_LOAD.p_memsz))`
  - `set_task_brk_bounds(leader_tid, heap_base, heap_base)`
- Growth relies on existing demand-page-fault behavior in `[brk_base, brk_end)` to allocate/map heap pages lazily when touched.
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

`IpcCall` runtime semantics (current contract):

- `IpcCall` performs request send/queue only.
- Reply payload is **not** consumed from syscall return registers.
- Caller must receive replies explicitly via `IpcRecv`/`IpcRecvTimeout` (recv-v2 out-meta path).
- Old inline reply-consumption behavior is obsolete and must not be assumed.

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

## `IpcRecv` / `IpcRecvTimeout` recv-v2 out-meta contract (current)

- Return register success/error only:
  - success: `ret0 == 0`
  - error: syscall error code path
- Recv metadata is returned only through `IpcRecvMetaV2` out pointer memory.
- No recv metadata lanes in `ret0..ret2` (and no `ret5` dependency).
- Reply payloads are delivered unchanged (no opcode-prefix stripping for replies).
- Legacy inline request-prefix stripping applies only to request-framed inline messages.

`IpcRecvMetaV2` encoded layout used by current tests/documented contract:

- bytes `8..10`: opcode (`u16`, LE)
- bytes `12..16`: payload length (`u32`, LE)
- bytes `16..24`: cap field (receiver-local cap id when capability materialization applies; otherwise sentinel)
- bytes `24..32`: recv-meta flags
- bytes `32..40`: sender tid/status lane (where applicable)

Notes:

- metadata consumers must read opcode/flags/payload length/cap info from out-meta, not from return registers.
- metadata bytes are ABI data; test-only hosted-dev memory notes are not part of this ABI.

Blocked recv-v2 completion:

- Portable `BlockedRecvState` is stored in task state when recv-v2 blocks.
- Delivery-time completion copies payload into saved user receive buffer.
- Delivery-time completion copies `IpcRecvMetaV2` into saved out-meta pointer.
- Completion is used for both send-delivery and reply-delivery waiter paths.
- Message is consumed exactly once (no enqueue duplication after waiter completion).
- No syscall re-exec model and no userspace recv retry workaround.

Received-cap materialization:

- Reply/transfer capabilities are materialized into receiver-local cap IDs before writing meta.
- Raw Reply object handles are not exposed directly to userspace.
- One-shot Reply objects must be materialized once per delivered message.
- Manually embedding raw cap-like values in message transfer fields is invalid; materialization requires legitimate kernel transfer-handle flow from call/send.
- The same delivered message cannot be received again to materialize a second capability.
- Reply caps are one-shot when consumed through `IpcReply`.

## recv-v2/reply-cap regression coverage

- `recv_v2_blocked_waiter_direct_delivery_consumes_exactly_once`
  - blocks regressions to syscall replay/duplicate queueing after blocked delivery completion.
- `ipc_reply_wakes_blocked_recv_v2_waiter_without_duplicate_enqueue`
  - blocks regressions where `IpcReply` fails to complete blocked waiters or leaves duplicate replies queued.
- `recv_v2_reports_metadata_only_via_out_meta_and_preserves_plain_reply_payload`
  - blocks regressions that leak recv metadata into return lanes or mutate plain-reply payload bytes.
- `recv_v2_materializes_reply_cap_once_per_message`
  - blocks regressions that allow invalid transfer-handle use, duplicate materialization, or second-receive rematerialization.

## Hosted-dev test harness note (non-ABI)

- Sparse hosted-dev user-memory backing guarantees readback only for bytes actually written.
- recv-v2 tests should read back actual payload length, not full receive-buffer capacity.
- user copy-path tests must use mapped user virtual addresses; host stack pointers are invalid for syscall/blocked-recv user pointers.

Portability boundary:

- Generic kernel recv completion logic is ISA-neutral (no x0/rax/a0/ELR/SVC assumptions).
- Architecture code maps abstract syscall success/error to ISA return registers and resume semantics only.

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

- IPC receive metadata now distinguishes `reply_cap` (for `ipc_reply`) from `transferred_cap` (application-transferred object capability).
- **IPC_RECV_V2 metadata is returned only via the out-meta pointer** (`IpcRecvMetaV2` memory write).
- `x0` is only syscall success/error for recv; metadata is not returned via `x0/x1/x2/x5`.
- Recv metadata flags in `IpcRecvMetaV2.recv_meta_flags`:
  - `RECV_META_REPLY_CAP = 1<<0`
  - `RECV_META_TRANSFERRED_CAP = 1<<1`
- Cap-kind decode must use `recv_meta_flags`; no opcode/payload heuristic inference is valid.
- Reply payloads are never opcode-prefix stripped.
- Legacy 2-byte opcode-prefix stripping applies only to inline request-framed messages (`OPCODE_INLINE` request path), exactly once.
- Current limitation: `ipc_call` carries a kernel reply capability; simultaneous application cap transfer in the same call message is not yet supported by syscall ABI v2.

---

## Syscall `27`: `InitramfsReadChunk` (PM-only, Phase 2A/2B Bootstrap Bridge)

**Access gate**: `TaskClass::SystemServer` only.  Any other caller receives `MissingRight` and the
kernel logs `INITRAMFS_READ_CHUNK_DENIED tid=<tid> name=<name>`.

**Purpose**: Temporary bootstrap bridge that lets the Process Manager (PM) bulk-copy CPIO file
data before a proper VFS-mediated file-transfer path is available.  This syscall bridges the
gap between the single-page initramfs-copy prototype (Phase 2A) and the full shared-4KiB
transfer-buffer VFS path (Phase 2B).  It will be superseded by `MemoryObject` page-cap grants
in Phase 3.

### Argument layout

| Register | Field        | Description                                                                 |
|----------|--------------|-----------------------------------------------------------------------------|
| `arg0`   | `name_ptr`   | User-space pointer to the CPIO entry name byte slice                        |
| `arg1`   | `name_len`   | Length in bytes of the CPIO entry name                                      |
| `arg2`   | `offset`     | Byte offset within the file (absolute; 0 = start of file)                  |
| `arg3`   | `dst_ptr`    | Destination user-space VA.  Phase 2A: caller's own VA.  Phase 2B: PM's VA. |
| `arg4`   | `max_len`    | Maximum bytes to copy (clamped to 4096 by kernel)                          |
| `arg5`   | `target_tid` | **Phase 2A**: `0` (copy to caller's ASID). **Phase 2B**: `3` (PM_BOOTSTRAP_TID — copy to PM's ASID). Any other value → `MissingRight`. |

### Return values

| Return    | Meaning                                             |
|-----------|-----------------------------------------------------|
| `ret0`    | `0` on success, non-zero error code on failure      |
| `ret1`    | Number of bytes actually copied (0 at EOF)          |

### Errors

| Error            | Meaning                                                          |
|------------------|------------------------------------------------------------------|
| `MissingRight`   | Caller is not `TaskClass::SystemServer`, or `target_tid` is not 0 or `PM_BOOTSTRAP_TID` |
| `Internal`       | File not found in CPIO archive                                   |
| `InvalidArgs`    | Kernel does not have a boot CPIO loaded (bridge unavailable)     |
| `PageFault`      | `dst_ptr` or the target ASID memory access failed               |

### Phase 2A behavior (`arg5 = 0`)

- Kernel finds the named file in the boot CPIO and copies `min(max_len, remaining)` bytes
  to `dst_ptr` in the **caller's** address space.
- Returns `Ok(0)` when `offset >= file_len` (EOF; file exists).
- Returns `Err(Internal)` when the file is not present in CPIO.

### Phase 2B extension (`arg5 = PM_BOOTSTRAP_TID = 3`)

- Used by `initramfs_srv` when servicing `VFS_OP_READ_BULK` requests forwarded from PM.
- Kernel performs a **cross-ASID copy**: finds the named CPIO entry and writes
  `min(max_len, remaining)` bytes to `dst_ptr` in **PM's** address space (ASID of TID 3).
- PM passes its stack `bulk_buf[4096]` VA as `BulkReadArgs.dst_ptr` in the IPC message.
- After the IPC round-trip completes, PM reads the filled data from `bulk_buf`.
- This is a temporary kernel-mediated transfer primitive.  The missing primitive for
  zero-copy is a `MemoryObject` page-cap grant (Phase 3 target).

### Lifecycle / removal gate

- **Phase 2A / Phase 2B**: active, PM-only.
- **Phase 3 target**: replace with `MemoryObject` page-cap grant; remove `arg5=PM_TID` extension.
- **Removal condition**: VFS bulk-read path uses page-cap zero-copy and does not need kernel
  cross-ASID copy mediation.

### Syscall trace gates (hot-path, default `false`)

- `INITRAMFS_READ_CHUNK_TRACE` in `src/kernel/syscall.rs` — per-chunk success/EOF log.
- `PM_VFS_BULK_READ_CHUNK_TRACE` in PM service — Phase 2A per-chunk PM log.
- `PM_VFS_BULK_READ_TRANSFER_CHUNK_TRACE` in PM service — Phase 2B per-chunk PM log.
