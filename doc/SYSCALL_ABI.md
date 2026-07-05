<!-- SPDX-License-Identifier: Apache-2.0 -->

# YARM Syscall ABI v10 (Frozen Contract)

- ABI Version: `10`
- Public syscall count: `16` (`0..=15`)
- Kernel dispatch table count: `32` (`SYSCALL_COUNT`, slots `0..=31`)

## Public ABI v10 syscall numbers

`SYSCALL_COUNT` in the kernel is the dispatch-table size, not the public ABI
count. The current public ABI v10 surface is the contiguous user-callable range
`0..=15` (**16 slots**). Some public slots still enforce capability or control
plane policy and can return `MissingRight`; that does not make them private
kernel-extension slots.

| Nr | Name | Public ABI status |
|----|------|-------------------|
| `0` | `Yield` | public |
| `1` | `IpcSend` | public, endpoint capability/right checked |
| `2` | `IpcRecv` | public, endpoint capability/right checked |
| `3` | `VmMap` | public, capability-targeted YARM-native VM map syscall |
| `4` | `TransferRelease` | public, releases a recv auto-mapped shared-memory transfer |
| `5` | `IpcRecvTimeout` | public, bounded non-blocking receive with scheduler-yield retry budget |
| `6` | `IpcCall` | public, send with kernel-minted ephemeral reply-cap transfer |
| `7` | `IpcReply` | public, consumes a reply-cap and sends reply to the bound caller endpoint |
| `8` | `ControlPlaneSetCnodeSlots` | public control-plane ABI; policy-gated by `control_plane_set_process_cnode_slots`, non-authorized callers receive `MissingRight` |
| `9` | `FutexWait` | public (`arg0=addr`, `arg1=expected`, `arg2=observed`) |
| `10` | `FutexWake` | public (`arg0=addr`, `arg1=max_wake`) |
| `11` | `SpawnThread` | public (`arg0=tls_base`, `arg1=user_stack_top`, `arg2=user_entry`) |
| `12` | `Fork` | public, forks current process with CoW and returns child tid in parent |
| `13` | `VmAnonMap` | public, wired anonymous page mapping syscall |
| `14` | `VmBrk` | public staged syscall; query, grow, and page-granular shrink are supported |
| `15` | `DebugLog` | public debug logging syscall |

## ABI slot status matrix

Status terms used by this matrix:

- **public v10**: part of the stable user-callable syscall ABI for version `10`.
- **privileged extension**: implemented dispatch-table slot outside the public v10
  range; intended for PM/SystemServer/bootstrap plumbing and not counted in the
  public ABI surface.
- **reserved gap**: not assigned and not dispatched in v10. Calls currently fail
  with `InvalidNumber`; assignment to a public user ABI requires a future ABI
  version as described below.
- **deprecated**: still dispatched for compatibility, but discouraged, with a
  documented replacement and removal plan. **No public v10 syscall is currently
  deprecated.**
- **removed/invalid**: not dispatched by this kernel ABI.

| Slot/range | Name | Status | Caller class | Notes |
|------------|------|--------|--------------|-------|
| `0` | `Yield` | public v10 | any user task | Stable public syscall. |
| `1` | `IpcSend` | public v10 | any user task with endpoint rights | Capability/right checks apply. |
| `2` | `IpcRecv` | public v10 | any user task with endpoint rights | Capability/right checks apply. |
| `3` | `VmMap` | public v10 | any user task with mapping capability | Capability-targeted VM mapping. |
| `4` | `TransferRelease` | public v10 | transfer receiver | Releases auto-mapped shared-memory transfer. |
| `5` | `IpcRecvTimeout` | public v10 | any user task with endpoint rights | Bounded receive/probe variant. |
| `6` | `IpcCall` | public v10 | any user task with endpoint rights | Request send with kernel-minted reply-cap transfer. |
| `7` | `IpcReply` | public v10 | reply-cap holder | Consumes one-shot reply capability. |
| `8` | `ControlPlaneSetCnodeSlots` | public v10 | policy-gated user/control-plane callers | Public ABI slot; kernel policy may return `MissingRight`. |
| `9` | `FutexWait` | public v10 | any user task | Futex address validation applies. |
| `10` | `FutexWake` | public v10 | any user task | Futex address validation applies. |
| `11` | `SpawnThread` | public v10 | any user task | Spawns a thread in the current process/thread group. |
| `12` | `Fork` | public v10 | any user task | CoW process fork. |
| `13` | `VmAnonMap` | public v10 | any user task | Wired anonymous mapping syscall; not reserved or deprecated. |
| `14` | `VmBrk` | public v10 | thread-group leader | Staged per-task brk contract. |
| `15` | `DebugLog` | public v10 | any user task | Debug logging aid; semantics are best-effort. |
| `16..=22` | — | reserved gap | none | `Syscall::decode` rejects these with `InvalidNumber`; unavailable until explicitly assigned in a future ABI. |
| `23` | `SpawnProcess` | privileged extension | privileged/bootstrap control-plane use | Implemented kernel dispatch slot; not part of public v10 count. |
| `24` | `SpawnProcessFromUserBuf` | privileged extension | privileged/control-plane staging use | Implemented kernel dispatch slot; not part of public v10 count. |
| `25` | — | reserved gap | none | `Syscall::decode` rejects this with `InvalidNumber`; unavailable until explicitly assigned in a future ABI. |
| `26` | `SpawnFromInitramfsFile` | privileged extension | PM/VFS-backed spawn path | Implemented kernel dispatch slot; not part of public v10 count. |
| `27` | `InitramfsReadChunk` | privileged extension | SystemServer only | Phase 2A/2B bootstrap bridge; not part of public v10 count. |
| `28` | `CreateInitramfsFileSliceMo` | privileged extension | SystemServer only | Phase 3A initramfs MemoryObject helper; not part of public v10 count. |
| `29` | `SpawnFromMemoryObject` | privileged extension | PM TID `3` only | Phase 3A zero-copy spawn helper; not part of public v10 count. |
| `30` | `RecvSharedV3` | non-blocking recv extension | any user task | Stage 42+43: non-blocking `recv_shared_v3` (NR 30); `timeout_ticks=0` only; no mapped receive. See `KERNEL_LOCKING.md §58`. |
| `31` | `ReapFaultedTask` | privileged PM restart-cleanup extension | PM TID `3` only | SUP-L7K-A: reaps an old terminal Faulted/Exited/Dead task after PM has successfully spawned and recorded a restart replacement. `arg0=target_tid`; self-target returns `InvalidArgs`; non-PM returns `MissingRight`; Running/Runnable/Blocked targets return `WrongObject`; missing targets are treated as already gone and return success. Uses existing `mark_task_dead` cleanup; not a kill-running-task syscall. |
| `32+` | — | removed/invalid | none | Outside `SYSCALL_COUNT = 32`; not dispatched. |

## ABI versioning and deprecation policy

The v10 contract is frozen for the public ABI slots listed above. Freezing means
call numbers and incompatible user-visible semantics do not change inside v10;
it does not prevent compatible implementation fixes or documentation updates.

Changes that require an ABI v11 bump include:

- renumbering any public syscall;
- changing public syscall argument layout, return layout, error behavior, or
  side effects incompatibly;
- assigning a reserved public gap (`16..=22` or `25`) to a user-visible public
  syscall;
- changing public struct layout, encoded metadata bytes, flag bits, sentinel
  values, or existing flag meanings incompatibly;
- removing or making invalid a public v10 syscall that is still documented as
  callable.

Changes allowed within v10 include:

- bug fixes that preserve the documented public behavior;
- documentation clarifications and reserved-slot documentation;
- adding or refining privileged extension slots outside the public v10 range,
  provided they do not change existing public syscall behavior;
- tightening access checks for privileged-only extension syscalls;
- compatible additions that do not change existing public argument/return
  layouts, existing flag meanings, or existing success/error contracts.

Reserved/deprecated terminology:

- **Reserved** means not callable and invalid in the current ABI; a reserved slot
  may be assigned only by an explicit future ABI decision.
- **Deprecated** means callable for compatibility but discouraged; deprecation
  requires this document to name the replacement, first-deprecated ABI version,
  and planned removal gate.
- **Removed/invalid** means not dispatched by this kernel ABI.

The public tail slots `13..=15` are assigned and dispatched in v10:
`13` is `VmAnonMap`, `14` is `VmBrk`, and `15` is `DebugLog`.
They are not reserved gaps and are covered by the public ABI v10 matrix above.

## Reserved and gap slots

- `16..=22`: reserved/unassigned in ABI v10. `Syscall::decode` rejects these
  numbers with `InvalidNumber`.
- `25`: reserved/unassigned in ABI v10. `Syscall::decode` rejects this number
  with `InvalidNumber`.
- `30`: `RecvSharedV3` — Stage 42+43 live non-blocking recv_shared_v3 extension (NR 30).
- `31`: `ReapFaultedTask` — SUP-L7K-A PM-only terminal-task cleanup extension.
- `32+`: outside the current kernel dispatch table and rejected with
  `InvalidNumber`.

## Privileged kernel extensions

The kernel currently declares `SYSCALL_COUNT = 32`, so dispatch-table slots
`0..=31` are in range. This is deliberately a different concept from the
public ABI count above: slots `23`, `24`, `26`, `27`, `28`, `29`, and `30` are
non-public kernel extensions used by PM, SystemServer, and bootstrap service
plumbing. They are documented here so integrators can distinguish reserved
holes from implemented privileged paths.

| Nr | Name | Caller restriction | Args | Returns | Failure / denial | Related docs |
|----|------|--------------------|------|---------|------------------|--------------|
| `23` | `SpawnProcess` | Privileged/bootstrap spawn extension intended for PM/control-plane bootstrap use. The current handler does not perform a class/TID gate before loading from the boot initrd. | `arg0=image_id`, `arg1=parent_pid`, `arg2=startup_args_ptr`, `arg3=startup_args_count`, `arg4..arg5` reserved/unused. | On success, `ret0=0`, `ret1=spawned_tid`, `ret2=service send cap` or packed spawner/parent send caps. | Invalid image IDs, malformed startup args, missing initrd entries, ELF/load failures, task/capacity exhaustion, or user-copy failures return the corresponding syscall error; no access-denial error is emitted by this handler today. | `doc/PROCESS_AND_SPAWN.md` bootstrap boundary |
| `24` | `SpawnProcessFromUserBuf` | Privileged staging extension intended for PM/control-plane use. The current handler does not perform a class/TID gate before copying the caller-supplied ELF buffer. | `arg0=image_id`, `arg1=elf_user_ptr`, `arg2=elf_len`, `arg3=parent_pid`, `arg4=startup_args_ptr`, `arg5=startup_args_count`. | On success, `ret0=0`, `ret1=spawned_tid`, `ret2=service send cap` or packed spawner/parent send caps. | `elf_user_ptr == 0`, `elf_len == 0`, `elf_len > 128 KiB`, invalid user memory, malformed ELF/startup args, or spawn/load/capacity failures return an error; no access-denial error is emitted by this handler today. | PM spawn staging/history |
| `26` | `SpawnFromInitramfsFile` | PM/VFS-backed spawn extension used by PM for image IDs `>= 4` through `pm_vfs_spawn_inline`. The current handler does not perform a class/TID gate before reading the named initramfs file. | `arg0=image_id`, `arg1=name_ptr`, `arg2=name_len`, `arg3=parent_pid`, `arg4=startup_args_ptr`, `arg5=startup_args_count`. | On success, `ret0=0`, `ret1=spawned_tid`, `ret2=service send cap` or packed spawner/parent send caps. | Empty/overlong names, invalid user memory/UTF-8, missing initrd entries, invalid image IDs, malformed ELF/startup args, or spawn/load/capacity failures return an error; no access-denial error is emitted by this handler today. | `doc/PROCESS_AND_SPAWN.md` (`pm_vfs_spawn_inline`) |
| `27` | `InitramfsReadChunk` | SystemServer-only Phase 2A/2B bootstrap bridge. Non-`TaskClass::SystemServer` callers receive `MissingRight`. `arg5` target writes are limited to `0` (self) or PM TID `3`; other targets receive `MissingRight`. | `arg0=name_ptr`, `arg1=name_len`, `arg2=offset`, `arg3=dst_ptr`, `arg4=max_len` (clamped to `4096`), `arg5=target_tid` (`0` self, `3` PM). | On success, `ret0=0`, `ret1=bytes_copied`, `ret2=0`. EOF returns success with `ret1=0`. | Non-SystemServer or invalid target receives `MissingRight`; invalid names/pointers/UTF-8/initrd access return errors; file-not-found returns `Internal` rather than EOF. | Phase 2A/2B bootstrap bridge notes below |
| `28` | `CreateInitramfsFileSliceMo` | SystemServer-only (`initramfs_srv`) Phase 3A bridge. Non-`TaskClass::SystemServer` callers receive `MissingRight`. | `arg0=name_ptr`, `arg1=name_len`, `arg2=flags` (reserved, must be `0`), `arg3..arg5` reserved/unused. | On success, `ret0=0`, `ret1=cap_id`, `ret2=file_len`. | Non-SystemServer receives `MissingRight`; empty/overlong names, nonzero flags, invalid user memory/UTF-8, missing/empty files, bounds failures, or MemoryObject/capability allocation failures return errors. | MemoryObject-backed initramfs spawn path |
| `29` | `SpawnFromMemoryObject` | PM-only Phase 3A zero-copy spawn path. Caller TID must be PM bootstrap TID `3`; other callers receive `MissingRight`. | `arg0=image_id`, `arg1=mo_cap`, `arg2=parent_pid`, `arg3=startup_args_ptr`, `arg4=startup_args_count`, `arg5` reserved/unused. | On success, `ret0=0`, `ret1=spawned_tid`, `ret2=service send cap` or packed spawner/parent send caps. | Non-PM callers receive `MissingRight`; invalid caps, wrong object type/kind, invalid initrd slice bounds, malformed ELF/startup args, or spawn/load/capacity failures return errors. | Phase 3A MemoryObject zero-copy spawn path |
| `30` | `RecvSharedV3` | Any user task with a RECEIVE-right capability. No class/TID gate. | `arg0=req_ptr` (ptr to `RecvSharedV3Request`, ≥64 bytes), `arg1=req_len`. Output written to `metadata_ptr` field inside the request struct. `timeout_ticks` must be 0 (non-blocking only). `map_intent`: `0x0`=no mapping, `0x1`=MAP_READ, `0x3`=MAP_READ\|MAP_WRITE; `0x2` (WRITE-only) is invalid. When `map_intent != 0`, `metadata_ptr` must be non-zero and `metadata_len >= V3_LIVE_OUTPUT_LEN`. | On success (message delivered): `ret0=V3_STATUS_OK(0)`, `ret1=sender_tid`, `ret2=0`; `RecvSharedV3Output` (80 bytes) written to `metadata_ptr`: `version=3`, `record_len=80`, `abi_version=10`, `result_status`, `sender_tid`, `message_len`, `message_flags`, `transferred_cap` (cap ID or `RECV_V3_NO_TRANSFER_CAP`). When `map_intent != 0`: `mapped_base` (offset 88), `actual_mapping_perm` (offset 104; `1`=RO, `3`=RW), `cleanup_token` (offset 112). FUTURE fields are 0. | `req_len < 64` → `InvalidArgs`; `timeout_ticks != 0` → `WouldBlock`; `map_intent=0x2` (WRITE-only) → `InvalidArgs`; unknown `map_intent` bits → `InvalidArgs`; `map_intent != 0` with `metadata_len < V3_LIVE_OUTPUT_LEN` → `InvalidArgs`; cap lacks `CAP_RIGHT_WRITE` but `map_intent=0x3` → `InvalidArgs`; invalid/wrong-type cap → `WrongObject`/`InvalidCapability`; empty queue → `WouldBlock`; cap materialization failure → `InvalidCapability`; user-copy fault to `metadata_ptr` → rollback + `InvalidArgs`. See `KERNEL_LOCKING.md §58`. | Stage 42+43+72 recv_shared_v3 |

## Syscalls `9..14` status

- `9` `FutexWait`: exposed and wired.
- `10` `FutexWake`: exposed and wired.
- `11` `SpawnThread`: exposed and wired.
- `12` `Fork`: exposed and wired.
- `13` `VmAnonMap`: exposed and wired; maps anonymous pages at a caller-selected page-aligned virtual address.
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

### `VmAnonMap` argument layout

- Syscall number: `13`
- `args[0]`: reserved for future use; portable callers should pass `0`
- `args[1]`: virtual address (page-aligned)
- `args[2]`: mapping length in bytes (rounded up to page size)
- `args[3]`: protection flags bitmask (`READ=0x1`, `WRITE=0x2`, `EXEC=0x4`)
- `args[4]`: reserved for future use; portable callers should pass `0`
- `args[5]`: reserved for future use; portable callers should pass `0`

`VmAnonMap` semantics:

- The syscall is public v10 and wired; it is not reserved or deprecated.
- The kernel validates the `(addr, len, prot)` triple, rejects zero length and
  non-page-aligned addresses, rounds length up to page size, allocates anonymous
  MemoryObjects page-by-page, and maps them into the current address space.
- Success returns `ret0=addr`, `ret1=rounded_length`, `ret2=0`.
- Partial mapping failures roll back physical mappings already installed for the
  requested range; any already allocated MemoryObject cap slots are reclaimed
  with the task cspace on exit.

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
- **Medium payload policy** (`129..=1024` bytes): fragmentation protocol (see `doc/IPC.md` §2).
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
  - **waiver ledger source**: `doc/PROJECT_HISTORY.md`.
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
- **Current migration snapshot (pass 19)**: Phase 6 service migration/deprecation slices are complete at the service level; core control-plane services are recorded as migrated or dated-waived in `doc/PROJECT_HISTORY.md`, with final global sign-off held until remaining Phase 4 lifecycle closure.

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

## Design note: inline IPC register payload expansion

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

## Syscall `27`: `InitramfsReadChunk` (SystemServer-only Phase 2A/2B Bootstrap Bridge)

**Access gate**: `TaskClass::SystemServer` only.  Any other caller receives `MissingRight` and the
kernel logs `INITRAMFS_READ_CHUNK_DENIED tid=<tid> name=<name>`.

**Purpose**: Temporary bootstrap bridge called by `TaskClass::SystemServer` initramfs/VFS
plumbing to bulk-copy CPIO file data for itself (`arg5=0`) or into PM (`arg5=3`) before the
page-cap zero-copy path fully replaces this transfer primitive.  This syscall bridges the gap
between the single-page initramfs-copy prototype (Phase 2A) and the full shared-4KiB
transfer-buffer VFS path (Phase 2B), while the MemoryObject-based spawn helpers occupy slots
`28` and `29`.

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
- This is a temporary kernel-mediated transfer primitive.  The MemoryObject-backed
  zero-copy spawn helpers exist at slots `28` and `29`; this bridge remains documented
  only for the older bulk-read copy path until that path no longer needs cross-ASID copy mediation.

### Lifecycle / removal gate

- **Phase 2A / Phase 2B**: active, SystemServer-only; `arg5=3` targets PM memory for the Phase 2B bridge.
- **Phase 3 target**: retire this bridge after the MemoryObject/page-cap path no longer needs `arg5=PM_TID` cross-ASID copy mediation.
- **Removal condition**: VFS bulk-read path uses page-cap zero-copy and does not need kernel
  cross-ASID copy mediation.

### Syscall trace gates (hot-path, default `false`)

- `INITRAMFS_READ_CHUNK_TRACE` in `src/kernel/syscall.rs` — per-chunk success/EOF log.
- `PM_VFS_BULK_READ_CHUNK_TRACE` in PM service — Phase 2A per-chunk PM log.
- `PM_VFS_BULK_READ_TRANSFER_CHUNK_TRACE` in PM service — Phase 2B per-chunk PM log.

---

## Stage 81A: Syscall error parity across AArch64 / x86_64 / RISC-V

Prior to Stage 81A, any `SyscallError` from `dispatch_syscall` propagated via `?` out of
`handle_trap` as `TrapHandleError::Syscall(...)`. All three arch entry points treat
`Err(TrapHandleError)` as a fatal kernel halt (AArch64: WFE spin; x86_64: `halt_forever()`; RISC-V:
`?` bubble to bare-metal context). A normal user error such as `InvalidArgs` or `MissingRight` would
lock up the CPU rather than being returned to userspace.

**Fix (Stage 81A):** `handle_trap`'s `Trap::Syscall` arm now calls
`trapframe.set_err(e.code())` on error and returns `Ok(())`. The error is encoded into the trap
frame's `error` field and written back to the user return registers by the arch return path.

`SyscallError::from_code(code: usize) -> SyscallError` was added as a const reverse-mapping helper
(mirrors the `#[repr(usize)]` discriminants; unknown codes map to `Internal`).

Kernel-internal wrappers that call `handle_trap` synthetically and need to observe policy-denial
results (e.g. `control_plane_set_process_cnode_slots_via_syscall`) read `frame.error_code()` after
dispatch and re-raise `Err(TrapHandleError::Syscall(SyscallError::from_code(code)))`.

**Invariants preserved:** syscall numbers, `SYSCALL_COUNT`, `SpawnV5` ABI, Phase2B/Phase3B
semantics all unchanged.

---

## Stage 81B: Spawn image path table extended to optional-FS image IDs

`spawn_image_path_for_image_id()` extended with entries for:

| image_id | CPIO path |
|----------|-----------|
| `10` | `sbin/fat_srv` |
| `11` | `sbin/ramfs_srv` |
| `12` | `sbin/ext4_srv` |

IDs ≥ 13 remain `None` (returns `InvalidArgs` from Phase 2B / Phase 3A spawn paths).

`INIT_SPAWN_OPTIONAL_FS_SERVERS` remains `false` in all core profiles — no live spawning added.
CPIO staging of optional-FS binaries is unchanged from Stage 80.
