# YARM AI Agent Rules: Capability, Spawn, and Zero-Copy Constraints

**Scope:** Rules for AI agents (Claude, Codex, or similar) working on YARM kernel,
IPC, server, or build-script code. These rules encode invariants proven through
Phase 2A → 2B → 3A → 3B. Violating them risks silent correctness regressions that
are hard to diagnose from logs alone.

---

## 1. Capability Rules

### 1.1 Never encode local cap IDs in payload bytes as authority

Cap IDs (slot numbers) are local to a specific cnode/task cspace. Embedding a cap
ID in an IPC message payload and treating it as transferable authority is wrong:
the receiving task's cspace is independent, and the same integer may refer to a
different object (or no object) there.

**Correct:** use the kernel's IPC cap-transfer path (see §1.3).

### 1.2 Cap IDs are cspace-local

A cap ID returned by `create_*`, `grant_*`, or `materialize_*` is valid only within
the cnode of the task that received it. Never assume a cap ID received in a payload
refers to a cap in your own cspace without an explicit materialization step.

### 1.3 Authority transfer must use the real IPC transferred-cap path

The only sanctioned way to transfer a capability between tasks is:

1. Sender: set `FLAG_CAP_TRANSFER_PLAIN` in the IPC flags word and place the local
   cap ID in the designated transfer field of the message.
2. Kernel: stashes the cap on the pending IPC and strips it from the sender's cspace.
3. Receiver: the cap is materialized into the receiver's cspace; available via
   `received.transferred_cap`.

Do not encode cap IDs in `payload[0..4]` or similar and expect them to work.

### 1.4 Use FLAG_CAP_TRANSFER_PLAIN for reply-with-cap

`FLAG_CAP_TRANSFER_PLAIN = 1 << 2`

Use this flag when replying with a transferred cap via `ipc_reply`. It does **not**
strip an opcode prefix from the payload.

**Do not use** the older `FLAG_CAP_TRANSFER` (without `_PLAIN`) for plain replies:
it triggers opcode-prefix stripping, which corrupts the payload when the reply body
does not start with an opcode word.

### 1.5 Reply caps are one-shot and non-delegatable

A reply capability created by `ipc_call` is consumed exactly once by `ipc_reply`.
It cannot be delegated to another task, stored in a cnode for later use, or used to
send additional messages. Attempting a second reply on the same cap returns
`StaleCapability`.

### 1.6 Reply cap cleanup uses fast revoke

When a reply cap is no longer needed (timeout, cancellation), use the fast-revoke
path (`IPC_FAST_REVOKE`). Do not traverse the general revocation/delegation graph
for reply cap cleanup — it is disproportionately expensive and is not designed for
single-shot reply caps.

### 1.7 Capability errors are fatal unless explicitly specified otherwise

The following errors on a cap operation indicate a programming bug or a corrupt
system state and must **not** be silently ignored or retried:

- `MissingRight` — caller lacks the required right for this operation
- `WrongObject` — cap refers to the wrong kernel object type
- `StaleCapability` — cap has been consumed or revoked
- `MaterializeFailed` — cap could not be installed into the receiver cspace

The only permitted recovery is explicit fallback logic documented in the relevant
milestone (e.g., Phase 3A falls back to Phase 2B on `Unsupported` from
`VFS_OP_FILE_GRANT_RO`). All other capability errors must propagate as hard failures.

---

## 2. Startup / Slot Rules

### 2.1 Do not casually change startup cap slots

The startup cap layout for each service is fixed and documented in
`doc/PROCESS_AND_SPAWN.md`. Changing a slot number requires updating every
consumer of that slot and verifying the change in both AArch64 and x86_64 smoke.

### 2.2 image_id 7/8/9 late services use zeroed extra caps

`sbin/driver_manager` (image_id 7), `sbin/blkcache_srv` (image_id 8), and
`sbin/virtio_blk_srv` (image_id 9) are spawned with zeroed extra caps in the
current Phase 3B implementation. Do not reintroduce a `vfs_recv_cap` in slot 13
or any other slot unless a real consumer is implemented and the slot layout is
re-validated end-to-end.

### 2.3 startup_args[0] must be the final task TID before first entry

The first word of the startup args array passed to a newly spawned task must be
its own task ID (TID). This is set by the kernel before the task's first instruction
executes. Do not overwrite `startup_args[0]` from userspace after spawn.

### 2.4 Startup slots live below stack_top; do not assert SP == stack_top

The startup args and cap slots are placed on the initial stack below the nominal
`stack_top` address. At first entry, SP points below `stack_top`, not at it. Do
not add an assertion `SP == stack_top` in entry stubs.

### 2.5 x86_64 first-entry stack must satisfy SysV ABI alignment

At the point of the first user instruction on x86_64, the stack pointer must be
16-byte aligned minus 8 bytes (as if a `call` instruction just pushed a return
address). Violating this causes SSE/AVX faults in functions that use aligned loads.

---

## 3. Fallback Rules

### 3.1 Phase 3A/3B fallback to Phase 2B is allowed only on Unsupported

PM may fall back from `VFS_OP_FILE_GRANT_RO` (Phase 3A/3B path) to
`VFS_OP_READ_BULK` (Phase 2B path) **only** if the VFS reply carries opcode
`Unsupported` (meaning the running kernel or VFS does not implement the operation).

This allows a Phase 3A/3B PM binary to boot on an older kernel that lacks syscall
nr=28 or nr=29.

### 3.2 Hard errors on the grant-RO path are fatal — do not silently fall back

The following responses from `VFS_OP_FILE_GRANT_RO` must **never** trigger a silent
fallback to Phase 2B. They indicate a real system error:

- `NotFound` — the file does not exist in the CPIO archive
- `PermissionDenied` / `MissingRight` — access control violation
- `PageFault` — memory error during cap materialization
- `OOM` / `CapabilityFull` — resource exhaustion
- `WrongObject` — MemoryObject kind mismatch
- `StaleCapability` — cap was already consumed
- `MaterializeFailed` — cap could not be installed in PM's cspace

Silently falling back to bulk-read in these cases hides real bugs and defeats the
Phase 3B acceptance criteria.

### 3.3 Do not silently fall back to 112-byte inline READ

The Phase 2A inline-read path (112-byte `OPCODE_INLINE` IPC payload) is an
emergency fallback for kernels that do not support `READ_BULK`. It must not be
triggered in the Phase 3B freeze. If `PM_VFS_READ_BULK_PHASE2A_BEGIN` appears in
a smoke log, it is a regression.

### 3.4 Syscall 27 is deprecated after Phase 3B — do not remove yet

Syscall nr=27 `InitramfsReadChunk` (both the self-ASID and cross-ASID paths) is
still present in the kernel for Phase 2B fallback compatibility. It must not be
removed until the long-run gate passes (see `doc/PROJECT_HISTORY.md`).

---

## 4. Zero-Copy Rules

### 4.1 Required alignment condition

Zero-copy mapping from an `InitramfsFileSlice` MemoryObject is only valid when:

```
(initrd_phys_base + file_initrd_offset + elf_page_offset_within_file) % PAGE_SIZE == 0
```

where `PAGE_SIZE = 4096`. This requires both:
- The CPIO archive places the ELF file's data at a 4096-byte boundary
  (`file_initrd_offset % PAGE_SIZE == 0`), and
- The ELF's PT_LOAD segments have `p_offset ≡ p_vaddr (mod PAGE_SIZE)` so that
  each page within the segment also lands on a page boundary in the archive.

### 4.2 Only full non-writable pages may be mapped zero-copy

Pages in a PT_LOAD segment with the W (writable) flag must always be copied to a
fresh anonymous physical frame, never mapped from the initramfs backing.

### 4.3 Writable, BSS, and partial pages must copy

| Page type | Action |
|-----------|--------|
| W segment page | Alloc fresh frame + copy file data |
| BSS page (`va >= p_vaddr + p_filesz`) | Alloc fresh zeroed frame |
| Partial head page (`va < p_vaddr`) | Alloc fresh frame + copy |
| Partial tail page (`va + PAGE > p_vaddr + p_filesz`) | Alloc fresh frame + copy |

### 4.4 W+X segments are rejected

Any PT_LOAD segment with both W and X flags set must be rejected before any page
mapping is attempted. The ZC loader must emit `ZC_PAGE reason=wx_rejected` and
return a hard error, not silently skip the segment.

### 4.5 Use the aligned CPIO packer for boot-service payloads

The standard `cpio` tool aligns file data to 4 bytes only. Every QEMU CPIO
archive containing ELF payloads must use `scripts/pack-initramfs-aligned.py` (or
`common_create_initramfs_aligned`). The rule applies to `/init`, every `/sbin/*`
ELF, and any ELF added later, not only services currently loaded zero-copy.

Verify one `ALIGN_PROOF ... alignment_mod=0 aligned=true` line per ELF. Missing
Python, a missing packer, or any unaligned ELF is a hard packing error.

### 4.6 Do not give child tasks MemoryObject caps

MemoryObject caps created via nr=28 must not be transferred to spawned child tasks.
Only PM (TID=3) may hold and consume MemoryObject caps for spawn. After syscall nr=29
returns, the MemoryObject cap in PM's cspace should be revoked (the kernel does this
automatically upon successful spawn).

---

## 5. Architecture Rules

### 5.1 x86_64 smoke defaults to -smp 1 (Stage 183 admits an explicit override)

`scripts/qemu-x86_64-core-smoke.sh` **defaults** to `-smp 1` via
`QEMU_SMP=${QEMU_SMP:-1}`. Through Stage 182 this was a hard `QEMU_SMP=1` pin. **Stage 183
(SMP-LIVE)** lifts the pin: the smoke now honors a caller-provided `QEMU_SMP` so the
`smp2-*/smp4-*` SMP-LIVE profiles can drive `-smp >1`, while `-smp 1` stays the default.
This selects only the QEMU CPU topology — it is NOT a production fallback knob, and the
graduated seams remain the only x86_64 production path. Do not add a runtime knob that
selects the old global-lock fallback.

**Stage 183/184 accepted status — DO NOT OVERCLAIM.** x86_64 SMP is accepted with APs
**online but WAKE-ONLY**: they idle in a scheduler-owned interruptible loop, take real
remote wakes, and acknowledge real TLB shootdowns, but they **run no dispatcher and
execute NO user tasks** (`dispatching_cpu_count` stays 1; task placement on APs is
denied). Stage 183 does NOT prove multi-dispatcher user scheduling. Stage 184
(CROSS-ARCH-LIVE) attests the accepted graduated D2/D6/D3 correctness + syscall parity
per arch under the single-dispatcher topology, with `mode=out_of_lock` on x86_64 and
`mode=in_lock_single_dispatcher` on AArch64/RISC-V (the in-lock path is the graduated
one, NOT the removed fallback). Stage 184 explicitly does NOT: retire the global lock
(Stage 185), enable multi-dispatcher scheduling, let APs run user tasks, or fake remote
TLB ACK on arches without real remote translation holders. When editing SMP/cross-arch
code or docs, preserve these caveats; do not describe APs as running user tasks or claim
multi-dispatcher scheduling.

**Stage 185 (GLOBAL-LOCK-RETIRE) status — DO NOT OVERCLAIM.** The global
`SpinLock<KernelState>` (`SharedKernel::with` / `with_cpu`) is **still the
authoritative live-runtime serialization** for the accepted single-dispatcher
model — it was **not** retired from live runtime in Stage 185 (which is *not a
rewrite stage*). The lock-free split path is a whitelist-only scaffold; every
other live syscall/IPC/scheduler/capability/VM/fault path runs inside the global
lock by design. Do not describe the global lock as "retired" or live paths as
"lock-free". The sole boot-only raw `&mut KernelState` escape,
`SharedKernel::borrow_kernel_for_boot`, is `pub(crate) unsafe`, used only during
bootstrap ELF load on the boot CPU from `arch/{x86_64,aarch64}/boot.rs`, and must
never appear on a live-runtime dispatch path (guarded by
`stage185_boot_only_global_borrow_confined`). See `doc/KERNEL_LOCKING.md §0` for
the classified inventory and the deferred per-subsystem retirement plan.

**Stage 186A (SPLIT-MUT-INFRA) status — infrastructure only, DO NOT OVERCLAIM.**
The per-domain split-mut seam set (`with_*_split_mut`, exposing only
`&mut <Subsystem>` never `&mut KernelState`) is now complete for ranks 1–6 — Stage
186A added the missing rank-4 `with_capability_state_split_mut` seam. These seams
are `M2_SEAM_HELPER_ONLY`: **no live syscall/IPC/cap/VM path was migrated onto
them** (only the pre-existing VmBrk-shrink and D6-dispatch callers use any seam).
Do **not** describe Stage 186A as retiring the global lock, converting `ipc_reply`,
or making any IPC/cap path "lock-free" — it did none of those. When adding a seam
or a future migration, keep the rank order (`doc/CAPABILITY_MODEL.md §3`): a seam
of rank N must not be entered while holding a lock of rank ≥ N, and cap
materialization (rank 4) must never run under `ipc_state_lock` (rank 3). See
`doc/KERNEL_LOCKING.md §0.1` for the seam table.

**Stage 186E-prereq (VM-USER-COPY-SEAM) status — infrastructure only, DO NOT
OVERCLAIM.** `SharedKernel` now has seam-based user-memory copy helpers
(`copy_to_user_split` / `copy_from_user_split` / `validate_user_access_for_asid_split`,
in `boot/user_memory_state.rs`) built on the VM (rank 5) + memory (rank 6) seams.
They never form a broad `&mut KernelState` and never take the IPC/cap/task/scheduler
locks, and — like the legacy copy path — perform **no COW fault-in** (non-writable ⇒
`UserMemoryFault`). They are `M2_SEAM_HELPER_ONLY`: **not wired into `ipc_reply` or any
live path.** Do not describe Stage 186E-prereq as converting `ipc_reply` or retiring
the global lock — it did neither. `ipc_reply` conversion still additionally needs a
seam form of the cap-transfer engine (`materialize_received_message_cap_routed`), which
does not yet exist. See `doc/KERNEL_LOCKING.md §0.2`.

**Stage 186D-prereq (CAP-TRANSFER-ENGINE-SEAM) status — audited, HARD-STOPPED, DO NOT
OVERCLAIM.** The cap-transfer materialization engine has **no** seam form and this stage
did **not** build one. On audit the materialize path is not cap-only: it spans task
(rank 2), IPC (rank 3), capability (rank 4), and memory (rank 6). `task_cnode` fuses
task+capability; `capability_object_live` reads IPC generations; `mint_capability_in_cnode`
bumps the memory `cap_refcount` (rank 6) in the same critical section as the cnode-slot
install (rank 4) — splitting opens a reclaim race; and the reply arm sets the waiter cap
under IPC (rank 3) after the rank-4 mint. The rank-4 `with_capability_state_split_mut`
seam cannot carry any of this. Do **not** describe 186D-prereq as building a cap-transfer
seam, converting `ipc_reply`, or retiring the lock — it did none of those. Disposition
`CAP_TRANSFER_SEAM_DEFERRED`; no `CAP_TRANSFER_SEAM_*` success marker may be emitted on the
legacy path. Pinned by `stage186d_cap_transfer_engine_seam_entanglement`. See
`doc/KERNEL_UNLOCKING.md` (Stage 186D-prereq) and `doc/KERNEL_LOCKING.md §0.3`.

**Stage 186D-proper (CAPABILITY-MEMORY-MINT-ATOMICITY) status — infrastructure only, DO NOT
OVERCLAIM.** `SharedKernel` now has `mint_capability_with_memory_ref_split` (in
`boot/cap_memory_mint_split.rs`) — an atomic cap↔memory mint that keeps a memory-object's
`cap_refcount` (rank 6) and a published cnode slot (rank 4) consistent via **Model A
(pre-bump then install, rollback on publish failure)**. It never forms a broad
`&mut KernelState`, never takes `ipc_state_lock` (no cap materialization under IPC, no
cap→IPC rank inversion), holds only one subsystem lock at a time (disjoint critical sections,
deadlock-free), takes an object+rights `Capability` (never echoes a sender-local CapId), and
returns `StaleCapability`/`CapabilityFull`/`TaskMissing` as real errors. It is
`M2_SEAM_HELPER_ONLY`: **not wired into `ipc_reply`, `ipc_send`/`recv`/`call`, or the
cap-transfer materialization path.** Do **not** describe it as converting `ipc_reply`,
building the cap-transfer seam, or retiring the lock — it did none of those, and it did
**not** solve the reply-cap IPC rank-inversion blocker (still deferred). It is the
atomic-mint building block a future cap-transfer seam sits on. Pinned by
`stage186d_proper_cap_memory_mint_atomicity`. See `doc/KERNEL_UNLOCKING.md` (Stage
186D-proper) and `doc/KERNEL_LOCKING.md §0.4`.

**Stage 186D2 (CAP-TRANSFER-MATERIALIZATION-SEAM-FIRST-SLICE) status — infrastructure only,
DO NOT OVERCLAIM.** `SharedKernel` now has the first seam-based cap-transfer materializer
(`materialize_received_cap_snapshot_split` / `materialize_received_message_cap_routed_split`,
in `boot/cap_transfer_materialize_split.rs`) built on the 186D-proper atomic mint. It takes a
plain IPC-lock-free `TransferCapSnapshot { receiver_cnode, object, rights }` (captured after
the envelope was consumed under `ipc_state_lock`) and mints an ordinary object cap into the
receiver cnode — no `ipc_state_lock`, no broad `&mut KernelState`, no cap→IPC rank inversion,
no sender-local CapId echoed as authority. Reply objects are explicitly deferred
(`DeferredReplyCap` / `reply_cap_ipc_rank_inversion`), never faked. It is
`M2_SEAM_HELPER_ONLY`: **not wired** into `materialize_received_message_cap_routed`,
`ipc_reply`, `ipc_send`/`recv`/`call`, or any live delivery path. Do **not** describe it as
converting a live path, retiring the lock, or as a drop-in for `grant_task_to_task_with_rights`
— it is **not yet a live-equivalent** (it does not yet record the source→dest delegation link
for revocation propagation; that rank-4 follow-on must land first), and it did **not** solve
the reply-cap IPC rank inversion. Pinned by
`stage186d2_cap_transfer_materialize_seam_first_slice`. See `doc/KERNEL_UNLOCKING.md` (Stage
186D2) and `doc/KERNEL_LOCKING.md §0.5`.

**Stage 186D3 (CAP-TRANSFER-DELEGATION-LINK-SEAM) status — infrastructure only, DO NOT
OVERCLAIM.** `SharedKernel` now records the sender→receiver delegation link via the rank-4
capability seam (`record_cap_delegation_link_split`) and materializes an ordinary transferred
cap seam **live-equivalently** (`materialize_received_cap_snapshot_with_delegation_split`:
atomic mint + delegation link, with `rollback_minted_cap_split` undoing the mint — clear slot
then drop refcount + reclaim — if the link record fails). No `ipc_state_lock`, no broad
`&mut KernelState`, no cap→IPC rank inversion; the delegation carries `source_cap` as a
recorded edge only, never as receiver authority. Reply objects stay `DeferredReplyCap`
(`reply_cap_ipc_rank_inversion`), never delegated. It is `M2_SEAM_HELPER_ONLY`: **not wired**
into `materialize_received_message_cap_routed`, `ipc_reply`, `ipc_send`/`recv`/`call`, or any
live delivery path. Do **not** describe it as converting a live path, retiring the lock, or
live-wiring cap transfer — live wiring (auditing every recv/delivery call site) is a separate
future stage, and it did **not** solve the reply-cap IPC rank inversion. Pinned by
`stage186d3_cap_transfer_delegation_link_seam`. See `doc/KERNEL_UNLOCKING.md` (Stage 186D3) and
`doc/KERNEL_LOCKING.md §0.6`.

**Stage 186D4 (ORDINARY-CAP-TRANSFER-LIVE-WIRING) status — HARD-STOPPED, DO NOT OVERCLAIM.**
Live-wiring the ordinary cap-transfer seam was audited and stopped: no runtime path was
converted. The two live materialization sites (`complete_blocked_recv_for_waiter`,
`try_split_recv_queued_plain_with_snapshot_locked`) run inside a `with`/`with_cpu` closure that
holds the global `SpinLock<KernelState>` and hands the body a `&mut KernelState`; the
`SharedKernel` seam derives `&mut Subsystem` from `self.state.data_ptr()`, so calling it there
would alias the live global-lock `&mut KernelState` — undefined behavior. Releasing the global
lock before materialize is broad IPC decomposition (Stage 187 multi-dispatcher), forbidden in
this stage. Do **not** describe the ordinary transfer seam as live-wired, and do **not** emit
any `CAP_TRANSFER_LIVE_SEAM_*` marker (that would dishonestly mark the legacy global-lock path).
A `&mut KernelState` re-implementation would just be the existing `grant_task_to_task_with_rights`
(mint+link+rollback) relabeled — not real seam wiring. The seam stays `M2_SEAM_HELPER_ONLY`.
Reply-cap materialization, `ipc_reply` conversion, and full global-lock retirement remain
deferred. Pinned by `stage186d4_ordinary_cap_transfer_live_wiring_hard_stop`. See
`doc/KERNEL_UNLOCKING.md` (Stage 186D4) and `doc/KERNEL_LOCKING.md §0.7`.

**Stage 187A (IPC-RECV-DELIVERY-BOUNDARY-SPLIT) status — LIVE boundary split, DO NOT
OVERCLAIM.** The queued-split recv delivery path now performs its user-space writeback
AFTER the `with_cpu` broad `&mut KernelState` is dropped, through the Stage 186E
`copy_to_user_split` seam (`M2_SEAM_LIVE_187A_RECV_BOUNDARY`). Phase A (under the lock,
byte-identical): dequeue + legacy cap materialization + sender wake (§56 order preserved —
wake still before writeback) + by-value `RecvBoundaryUserCopySnapshot`; Phase B: seam copy;
Phase C: frame commit + §58 rollback/fault via brief re-entries. Scope honesty: this stage
does not enable multi-dispatcher/AP user scheduling, does not fully retire the global lock,
does not solve reply-cap materialization (legacy router in Phase A, never seam-routed), does
not convert `ipc_send`/`ipc_call`/`ipc_reply` or blocked-waiter delivery
(`complete_blocked_recv_for_waiter` — still `defer_needs_broad_ipc_decomposition`), and does
not wire the 186D2/186D3 cap-transfer seam (that follow-on now depends only on this
boundary). Do not call any `data_ptr()`-derived seam inside the Phase A closure or the Phase
C re-entry closures — pinned by `stage187a_ipc_recv_delivery_boundary_split`. Relocated
markers: `YARM_RECV_CORE_LIVE kind=user_plain{,_v2}`, `IPC_RECV_V2_META_QUEUED_SPLIT_OK`,
and the queued-split `IPC_RECV_V2_ROLLBACK_OK` sites moved WITH the writeback to runtime.rs
Phase C (same live path; `stage159bcd_target_markers_are_kernel_emitted` re-homed to require
a literal kernel emission in either file). See `doc/KERNEL_UNLOCKING.md` (Stage 187A) and
`doc/KERNEL_LOCKING.md §0.8`.

**Stage 187B (ORDINARY-CAP-TRANSFER-SEAM-LIVE-ON-RECV-BOUNDARY) status — LIVE, DO NOT
OVERCLAIM.** Ordinary (non-reply, non-shared-region) transferred caps received by a user task
on the 187A queued-split boundary are now materialized through the 186D2/186D3 cap-transfer
seam — the first live use of that seam (`M2_SEAM_LIVE_187B_CAP_TRANSFER`). Phase A
(`phase_a_snapshot_ordinary_transfer`, under `with_cpu`) consumes the transfer envelope once
and snapshots object/rights/cnode + delegation identity by value (no mint, no seam;
`source_cap` is the delegation edge only, never receiver authority). Phase B/C
(`SharedKernel::complete_recv_boundary_ordinary_cap`, after the borrow drops): seam mint
(atomic cap↔memory mint + delegation link) → commit receiver-local CapId → deferred sender
wake → 186E user copy → §58 rollback via `rollback_materialized_recv_cap`. Order preserved:
materialize → wake → writeback. Scope honesty: does **not** enable multi-dispatcher/AP user
scheduling, does **not** fully retire the global lock, **reply-cap materialization remains
deferred** (rank inversion — reply caps stay on the legacy in-lock router), and it does not
convert `ipc_reply`/`ipc_send`/`ipc_call`/blocked-waiter delivery (shared-region and
kernel-register cap transfers also stay legacy). The seam must be called ONLY in `runtime.rs`
post-boundary, never in `syscall.rs` Phase A (186D4 aliasing). Markers
`CAP_TRANSFER_BOUNDARY_SEAM_*` fire only on the converted ordinary path. Pinned by
`stage187b_ordinary_cap_transfer_seam_live_on_recv_boundary`. See `doc/KERNEL_UNLOCKING.md`
(Stage 187B) and `doc/KERNEL_LOCKING.md §0.9`.

**Stage 187C (IPC-REPLY-RETRY-AFTER-BOUNDARY-SEAMS) status — HARD-STOPPED, DO NOT OVERCLAIM.**
Retrying the `ipc_reply` conversion was audited and stopped: no runtime path was converted.
`ipc_reply`'s only seam-eligible work (reply payload copy + any cap materialization to the
caller) lives inside `complete_blocked_recv_for_waiter` — the shared blocked-waiter delivery
path (6 production call sites: reply/send/fault) that runs the copy + materialize under the
broad `&mut KernelState`. 187A/187B split the **queued** recv path, not this blocked-waiter
path. Converting `ipc_reply` needs (a) boundary-splitting the shared blocked-waiter delivery
(broad decomposition — out of scope) and (b) for reply-with-cap, the unsolved
`reply_cap_ipc_rank_inversion`. Do **not** describe `ipc_reply` as boundary-split or emit any
`IPC_REPLY_BOUNDARY_*` marker; do not fork `complete_blocked_recv_for_waiter` for reply only
(half-converted path). The reply-cap consume/revoke/enqueue/wake call no seam and need no
boundary. Recommended next step: Stage 187D (split the shared blocked-waiter delivery) then a
focused reply-cap rank-inversion stage. Broader IPC conversion, multi-dispatcher, and full
global-lock retirement remain deferred. Pinned by `stage187c_ipc_reply_retry_hard_stop`. See
`doc/KERNEL_UNLOCKING.md` (Stage 187C) and `doc/KERNEL_LOCKING.md §0.10`.

**Stage 187D (BLOCKED-WAITER-DELIVERY-BOUNDARY-SPLIT) status — HARD-STOPPED, DO NOT OVERCLAIM.**
Splitting `complete_blocked_recv_for_waiter` was audited and stopped: no runtime path was
converted. The helper's seam-eligible shape matches the 187A queued path, but all 6 production
call sites are `&mut KernelState` syscall handlers (`handle_ipc_send`, `handle_ipc_call`,
`ipc_reply`, `ipc_send_with_optional_deadline`, `emit_fault_report_for_fault`) buried inside the
single main-dispatch `with_cpu` closure — none has `&SharedKernel`, and the broad borrow only
drops when the whole `dispatch()` closure returns. Unlike 187A's dedicated pre-dispatch recv
fast path, blocked-waiter delivery has **no SharedKernel-level owner** (`try_split_dispatch_into_frame`
routes only `IpcRecv`/`VmBrk`). Running Phase B/C on the seams needs a dispatch-return
"pending post-boundary work" channel through every handler, or per-caller pre-dispatch forks
(wholesale send/reply/call/fault duplication) — broad IPC/dispatch decomposition, out of scope;
plus the reply-with-cap `reply_cap_ipc_rank_inversion`. Do **not** split the helper into inert
Phase-A-only infra (no live Phase B/C caller — dead infra that masquerades as progress); do
**not** emit any `BLOCKED_WAITER_BOUNDARY_*` marker. Recommended next step: the Stage 188+
multi-dispatcher / dispatch-boundary restructuring (a typed dispatch-return delivery channel).
Reply-cap materialization, broader IPC conversion, multi-dispatcher, and full global-lock
retirement remain deferred. Pinned by `stage187d_blocked_waiter_delivery_hard_stop`. See
`doc/KERNEL_UNLOCKING.md` (Stage 187D) and `doc/KERNEL_LOCKING.md §0.11`.

**Stage 188A (DISPATCH-RETURN-DELIVERY-CHANNEL) status — infrastructure only, DO NOT
OVERCLAIM.** A typed by-value dispatch-return channel (`crate::kernel::dispatch_post_work::DispatchPostWork`:
`None` + `BlockedWaiterPlainDelivery`) lets a handler under the broad `with_cpu` /
`&mut KernelState` borrow stash post-boundary work into a per-CPU `DISPATCH_POST_WORK_STASH`
(mirroring the Stage 117 `PerCpuSwitchPlanStash`); `SharedKernel::drain_dispatch_post_work(cpu)`
executes it in `handle_trap_entry_shared` **after** the broad borrow drops, through the 186E
copy seam. The enum is by-value (no `&mut KernelState`, no borrows, no `CapId`). **No live
handler stashes work** — the channel is inert (one-shot `DISPATCH_RETURN_CHANNEL_READY
mode=helper_only`; zero behavior change); the `BlockedWaiterPlainDelivery` executor is
unit-tested but produced by nothing live. Do **not** describe 188A as converting any IPC path,
enabling AP user-task scheduling, retiring the global lock, or solving the reply-cap rank
inversion (`reply_cap_ipc_rank_inversion` still blocks reply-cap materialization). Do **not**
stash work from a production handler until a focused follow-on wires + proves a specific
blocked-waiter slice. `KernelState::clear_blocked_recv_return_regs` was extracted
byte-identically from `complete_blocked_recv_for_waiter` (shared by the legacy path and the
executor). Pinned by `stage188a_dispatch_return_delivery_channel`. See `doc/KERNEL_UNLOCKING.md`
(Stage 188A) and `doc/KERNEL_LOCKING.md §0.12`.

### 5.2 x86_64 SMP TODO

Before enabling x86_64 SMP smoke: split the AP trampoline assembly stub from the
Rust SMP initialization logic in `src/arch/x86_64/smp.rs`. The current file mixes
low-level 16-bit/32-bit trampoline code with Rust AP bringup. A clean split is
required before SMP can be tested in CI.

### 5.3 AArch64 user ELFs must keep 4 KiB LOAD alignment

`targets/aarch64-yarm-user-none.json` must retain:
```json
"-zmax-page-size=0x1000",
"-zcommon-page-size=0x1000"
```
Removing these flags restores 64 KiB PT_LOAD alignment and breaks zero-copy loading
for all AArch64 late services.

### 5.4 Kernel page size remains 4 KiB

The kernel uses `PAGE_SIZE = 4096` on all supported architectures. Do not change
this constant. It affects the ZC feasibility check, the CPIO packer alignment target,
and the ELF LOAD alignment requirement simultaneously.

---

## 6. What Not to Do (Hard Constraints)

The following changes require an explicit Phase N+1 plan and are never acceptable
as incidental modifications:

| Prohibited action | Reason |
|-------------------|--------|
| Change syscall ABI (nr, arg layout, return layout) | Breaks all existing server binaries |
| Change SpawnV5 ABI | Breaks init_server and all spawn paths |
| Remove syscall 27 prematurely | Breaks Phase 2B fallback |
| Remove Phase 2B fallback from PM | Breaks old-kernel compatibility |
| Enable `VFS_READ_SHARED_REPLY_ENABLED` | Not validated; may cause IPC state corruption |
| Increase heap sizes | Violates memory budget; may cause OOM on constrained targets |
| Change kernel page size | Cascading breakage across ZC, CPIO, and ELF alignment |
| Change x86_64 SMP in smoke | x86_64 SMP not validated |
| Change service startup cap layout | Breaks slot assumptions in all service entry stubs |
| Map writable file-backed pages | Security invariant: initramfs data is read-only |
| Give child tasks MemoryObject caps | Violates isolation; child must not see initrd phys addrs |
| Use FLAG_CAP_TRANSFER for plain replies | Causes opcode-prefix stripping on reply body |
| Silently fall back on hard cap errors | Hides real bugs; violates Phase 3B acceptance criteria |

---

## 7. Log Marker Reference (Phase 3B Baseline)

The following kernel/server log markers are part of the Phase 3B acceptance
contract. Smoke scripts check these; do not rename or remove them without updating
both the smoke scripts and this document.

| Marker | Source | Meaning |
|--------|--------|---------|
| `PM_VFS_GRANT_RO_BEGIN image_id=N` | PM | Before `VFS_OP_FILE_GRANT_RO` send |
| `VFS_FILE_GRANT_RO_FORWARD` | VFS | VFS routes to backend |
| `INITRAMFS_FILE_GRANT_RO_REPLY` | initramfs_srv | After nr=28 succeeds |
| `PM_VFS_GRANT_RO_RECEIVED image_id=N` | PM | MemoryObject cap received |
| `PM_SPAWN_FROM_MO_DONE image_id=N` | PM | nr=29 returned success |
| `ZC_FEASIBILITY image_id=N feasible=true/false` | kernel | ZC pre-check result |
| `ZC_SEG_BEGIN image_id=N seg=K` | kernel | Per-LOAD-segment entry |
| `ZC_PAGE image_id=N seg=K reason=...` | kernel | Per-page decision |
| `ZC_SEG_DONE image_id=N seg=K mapped=N copied=N` | kernel | Segment summary |
| `PM_ELF_ZC_DONE image_id=N zc_pages=M copied_pages=K` | kernel | Load complete |
| `PM_ELF_ZC_FAIL image_id=N reason=...` | PM/kernel | ZC load error (must be 0) |
| `INITRAMFS_SRV_ENTRY` | initramfs_srv | Service started (must be exactly once) |
| `DEVFS_SRV_ENTRY` | devfs_srv | Service started (must be exactly once) |
| `VFS_SRV_ENTRY` | vfs_server | Service started (must be exactly once) |
| `DRIVER_MANAGER_ENTRY` | driver_manager | Service started (must be exactly once) |
| `BLKCACHE_SRV_ENTRY` | blkcache_srv | Service started (must be exactly once) |
| `VIRTIO_BLK_SRV_ENTRY` | virtio_blk_srv | Service started (must be exactly once) |
| `DRIVER_MANAGER_READY` | driver_manager | Initialization complete (must be exactly once) |
| `BLKCACHE_SRV_READY` | blkcache_srv | Initialization complete (must be exactly once) |
| `VIRTIO_BLK_SRV_READY` | virtio_blk_srv | Initialization complete (must be exactly once) |

### Stage 88-91 optional-FS markers

| Marker | Source | Meaning |
|--------|--------|---------|
| `INIT_PM_RECV_DRAIN_BEGIN` | init | Before draining pm_recv of stale replies |
| `INIT_PM_RECV_DRAIN_DONE count=N` | init | After drain; N replies discarded |
| `INIT_RAMFS_SPAWN_BEGIN` | init | Before spawning ramfs_srv (image_id=11) |
| `INIT_RAMFS_SPAWN_OK child_tid=N` | init | ramfs_srv spawned successfully |
| `INIT_RAMFS_SPAWN_FAIL` | init | ramfs_srv spawn failed (error) |
| `RAMFS_SRV_ENTRY` | bin/ramfs_srv | ramfs_srv binary entry point reached |
| `RAMFS_MOUNT_READY` | ramfs/service | RAMFS service loop ready; mount registered |
| `VFS_MOUNT_REGISTER_RAMFS_OK` | init | VFS accepted RAMFS mount registration |
| `INIT_EXT4_SPAWN_BEGIN` | init | Before spawning ext4_srv (image_id=12) |
| `INIT_EXT4_SPAWN_OK child_tid=N` | init | ext4_srv spawned successfully |
| `INIT_EXT4_SPAWN_FAIL` | init | ext4_srv spawn failed (error) |
| `EXT4_SRV_ENTRY` | bin/ext4_srv | ext4_srv binary entry point reached |
| `EXT4_SRV_READY` | ext4/service | ext4_srv service loop ready |
| `VFS_MOUNT_REGISTER_EXT4_OK` | init | VFS accepted EXT4 mount registration |
| `INIT_FAT_SPAWN_BEGIN` | init | Before spawning fat_srv (image_id=10) |
| `INIT_FAT_SPAWN_SKIPPED reason=profile_disabled` | init | FAT skipped: no virtio_blk in profile |
| `INIT_FAT_SPAWN_SKIPPED reason=server_disabled` | init | FAT skipped: INIT_SPAWN_FAT_SRV=false |
| `INIT_FAT_SPAWN_OK child_tid=N` | init | fat_srv spawned (only if virtio_blk present) |
| `FAT_MOUNT_READY` | fat/service | FAT service loop ready; mount registered |
| `VFS_MOUNT_REGISTER_FAT_OK` | init | VFS accepted FAT mount registration |

---

## 8. Reply-Endpoint Hygiene Rule

### 8.1 Do not reuse a shared reply endpoint across protocol phases without consuming all prior replies

A shared reply endpoint (e.g., `pm_recv`) must have all pending replies consumed or explicitly
drained before it is used for a new round of protocol traffic. A timeout/deadline-0 receive
(`ipc_recv_with_deadline(ep, 0)`) is a poll that returns `None` if no message is pending at that
instant. It does NOT guarantee the endpoint is clean — a reply arriving milliseconds later will
be misinterpreted as the next operation's reply.

**Correct pattern for shared endpoints:**
1. If a service helper sends a non-blocking poll to a shared endpoint, drain the endpoint
   exhaustively before starting the next protocol phase.
2. The `INIT_PM_RECV_DRAIN_BEGIN` / `INIT_PM_RECV_DRAIN_DONE` pattern in `init/service.rs` is
   the reference implementation.

**Correct pattern for per-operation replies:**
Use a dedicated reply-recv cap (not the shared endpoint) for protocol phases that need a clean
reply window. The ext4 and FAT mount-register helpers each use their own `reply_recv_cap`
argument, not `pm_recv`.

**Forbidden:** Reusing `pm_recv` for a VFS mount-register reply without draining it first.
This was the root cause of the "stale 32-byte blkcache reply misinterpreted as 16-byte SpawnV5
reply" bug fixed in commit 234aed2.

### 8.2 Deadline-0 receives on dedicated caps are safe

A deadline-0 receive on a dedicated per-operation reply cap is safe: only the current
operation's reply can arrive on that cap. The drain pattern is only required for shared
endpoints.

---

## 9. Initramfs Path Table Rule

Every server staged in the CPIO archive must be:
1. Present in the CPIO with 4 KiB-aligned file data (`scripts/pack-initramfs-aligned.py`).
2. Registered in `spawn_image_path_for_image_id()` so the kernel path table can map
   image_id → CPIO path.
3. Registered in `InitramfsBackend` inode/path table (`archive.rs`) so VFS can open/statx
   it at `/initramfs/sbin/<name>`.
4. Present in the `from_cpio_newc()` match arm so that live CPIO images update the synthetic
   inode's `file_len`.
5. Covered by an `openat_path`/`statx_path` test through the `InitramfsBackend` API.

**The ext4_srv regression (fixed in commit 690951b):** `sbin/ext4_srv` was staged in CPIO and
registered in the kernel path table, but the `InitramfsBackend` inode table had only 13 slots
— `ext4_srv` was missing. VFS returned `NotFound` when PM tried to open
`/initramfs/sbin/ext4_srv`, causing `PM_ELF_ZC_FAIL` on every ext4_srv spawn attempt.

When adding a new sbin server: bump `MAX_INITRAMFS_INODES`, add the inode, add the
`from_cpio_newc` match arm, and add a path test.

---

## 10. VFS Client Blocking-Receive Rule (Stage 92)

### 10.1 vfs_client.rs IPC helpers MUST use blocking `ipc_recv_v2`

All four IPC helpers in `crates/yarm-user-rt/src/vfs_client.rs`
(`vfs_statx`, `vfs_openat`, `vfs_read`, `vfs_close`) MUST use `ipc_recv_v2(reply_recv_cap)`
(blocking) to consume the VFS server's reply.

**Forbidden:** Using `ipc_recv_with_deadline(reply_recv_cap, 0)` (non-blocking poll)
in any of these helpers.

**Root cause (Stage 92 AArch64 wrong-sender race):**
On AArch64, VFS scheduling is slower than on x86_64. When init calls `vfs_statx` or
`vfs_openat` with a deadline-0 receive, VFS has not yet replied. The helper returns
`Err(NoReply)` but VFS's reply is still in flight and arrives later — at the shared
`pm_recv` endpoint (`E_init_reply`). The pre-spawn drain loop (also using deadline-0)
misses these delayed replies if they arrive after the loop completes. The next
`ipc_recv_v2` call inside `spawn_v5_cap` then receives 1–3 of those 8-byte VFS replies
before the real 16-byte SpawnV5 reply, logging `INIT_SPAWN_V5_WRONG_SENDER_REPLY` ×1–3.

**Fix:** Replace `ipc_recv_with_deadline(reply_recv_cap, 0)` with `ipc_recv_v2(reply_recv_cap)`
and change the match arm from `Ok(Some(ref r)) => decode_reply_u64(r)` to
`Ok(Some(ref received)) => decode_reply_u64(&received.message)` (since `ipc_recv_v2`
returns `ReceivedMessage`, not `Message` directly).

### 10.2 spawn_v5_cap wrong-sender drain loop is defense-in-depth only

The drain loop added in Stage 91 (logs `INIT_SPAWN_V5_WRONG_SENDER_REPLY`) must remain
as defense-in-depth. With the Stage 92 fix applied, this loop should fire 0 times in a
clean run. Do NOT remove the loop.

### 10.3 Smoke script strict mode must enforce count=0

Both `scripts/qemu-aarch64-optional-fs-smoke.sh` and `scripts/qemu-x86_64-optional-fs-smoke.sh`
must check `INIT_SPAWN_V5_WRONG_SENDER_REPLY` count and fail when `QEMU_SMOKE_STRICT=1`
and count > 0. This is the acceptance criterion for Stage 92.

---

## 11. Official FS Profile Matrix (Stage 93)

### 11.1 Profile definitions

| Profile | RAMFS | ext4 | FAT | Block device | Default? |
|---------|-------|------|-----|--------------|----------|
| `core` | disabled | disabled | disabled | none | no |
| `optional-fs` | ✓ live | ✓ read-only | disabled | none | **YES** |
| `fat-block` | ✓ live | ✓ read-only | ✓ read-only | virtio-blk required | no |
| `full-fs-experimental` | ✓ | ✓ | ✓ (future) | virtio-blk | future |

**Current default is `optional-fs`:** RAMFS + ext4 live, FAT disabled.

### 11.2 Gate constants per profile

| Constant | optional-fs | fat-block |
|----------|-------------|-----------|
| `INIT_SPAWN_RAMFS_SRV` | `true` | `true` |
| `INIT_SPAWN_FAT_SRV` | **`false`** | `true` |
| `INIT_SPAWN_EXT4_SRV` | `true` | `true` |
| `VFS_FAT_LIVE_MOUNT_ENABLED` | **`false`** | `true` |
| `VFS_FAT_SHARED_IO_ENABLED` | **`false`** | **`false`** (until read proven) |
| `VFS_EXT4_LIVE_MOUNT_ENABLED` | `true` | `true` |
| `VFS_RAMFS_LIVE_MOUNT_ENABLED` | `true` | `true` |

### 11.3 Forbidden patterns in smoke strict mode (`QEMU_SMOKE_STRICT=1`)

All profiles: `INIT_SPAWN_V5_WRONG_SENDER_REPLY`, `KSPAWN_EXTRA_CAP_DELEGATE_FAIL`,
`PM_VFS_SPAWN_FAIL`, `reason=bad_fd_decode`, `panic`.

`optional-fs` profile (FAT disabled): `INIT_FAT_SPAWN_OK` must be absent.

`fat-block` profile: `INIT_FAT_SPAWN_FAIL`, `FAT_MOUNT_FAILED`, `PM_ELF_ZC_FAIL image_id=10`.

### 11.4 IpcBlockDevice blocking-receive rule

`IpcBlockDevice::read_exact_at` and `write_sector` in `crates/yarm-fs-servers/src/fs/fat/fs.rs`
MUST use `ipc_recv_v2` (blocking). Using `ipc_recv_with_deadline(_, 0)` is a
scheduling race (same root cause as Stage 92's `vfs_client.rs` fix) and will cause
`FatError::Io` on any architecture where blkcache_srv hasn't replied within 0 ticks.

This was fixed in Stage 93 as part of FAT production groundwork.

---

## 12. Optional FS Milestone 1 (Stage 94/100)

### 12.1 Milestone declaration

**YARM Optional FS Milestone 1 is declared at Stage 100.**

The milestone is reached when:
- RAMFS + ext4 optional-FS baseline is clean (strict smoke passes).
- FAT is profile-ready but disabled, with exact blockers documented.
- No yarm-fs-servers or yarm-control-plane-servers failures.
- Docs/rules are current.
- Kernel unlocking handoff seed is written.

See `doc/PROJECT_HISTORY.md` for the full milestone record.

### 12.2 Filesystem work is paused after Milestone 1

Do NOT start new FS feature work after Stage 100 without opening a new dedicated
FS milestone. Permitted after Stage 100:
- Regressions only (RAMFS/ext4 existing behavior).
- Emergency FAT gate doc updates (no code changes).
- Smoke script fixes (not feature additions).

### 12.3 Fatal smoke greps must exclude nonfatal=true (Stage 94)

The `panic` grep in optional-FS smoke scripts must not match log lines that also
contain `nonfatal=true`. Those lines represent non-fatal diagnostic events, not
kernel or userspace panics.

**Correct pattern:**
```bash
# Two-stage: find panic lines, then exclude nonfatal ones
panic_count=$(tr '\r' '\n' <"$LOGFILE" | rg -ai "\bpanic\b" 2>/dev/null \
    | rg -avc "nonfatal=true" 2>/dev/null || echo 0)
```

**Forbidden:**
```bash
# Single-stage: matches nonfatal=true lines
panic_count=$(... | rg -ai -c "\bpanic\b" 2>/dev/null || echo 0)
```

### 12.4 Kernel unlocking handoff

**Canonical source of truth: `doc/KERNEL_UNLOCKING.md`.** New milestone /
context / audit / status fragment files for kernel unlocking are forbidden;
update the canonical doc instead. See `doc/DOCUMENTATION_MAP.md` for the
repo-wide ownership map.

The full invariant list, live-path / fallback tables, recent correctness
fixes, scaffold status, and remaining-work plan all live in
`doc/KERNEL_UNLOCKING.md`. A short summary of the invariants follows for
quick-reference; if these conflict with the canonical doc, the canonical
doc wins.

- SpawnV5 ABI (16-byte reply, argument layout)
- Image IDs 7–12 frozen
- SYSCALL_COUNT = 31, STARTUP_SLOT_COUNT = 18
- recv_shared_v3 ABI offsets
- Optional-FS smoke markers (RAMFS/ext4 expected; FAT skipped)
- No deadline-0 required replies in vfs_client.rs or IpcBlockDevice
- VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false

---

## 13. MUST_SMOKE Policy (Stage 101)

### 13.1 Scope — when smoke results are mandatory

Any stage/PR MUST include smoke results when its diff:

1. **live-wires a new split path** in `handle_trap_entry_shared`,
   `dispatch_trap_entry_with_shared_kernel`, `try_split_dispatch_into_frame`, or
   any equivalent trap/syscall entry seam (i.e. a new `Some(Ok(()))` /
   `Some(Err(..))` return on a code path previously returning `None`),
2. modifies **IPC dequeue, sender-waiter, receiver-waiter, timeout, wakeup, or
   reply-delivery logic** (`recv_core.rs`, `ipc.rs`, `ipc_state.rs`, any
   `complete_blocked_recv_for_waiter` /
   `ipc_try_recv_queued_with_cap_transfer` / `apply_split_sender_wake_plan` /
   `apply_split_receiver_wake_plan` callers),
3. changes `entering_tid` / `exiting_tid` / `task_switched` /
   `current_tid_authoritative` / `current_tid_split_read` behavior,
4. changes **trap/syscall result writeback** (`TrapFrame::set_ok`,
   `TrapFrame::set_err`, `encode_transfer_cap_ret`, pack-payload helpers),
5. changes **scheduler dispatch or block/wake** (`dispatch_next_task`,
   `block_current_cpu`, `enqueue_on_cpu`, runqueue ops, membership tables),
6. changes **VM/TLB shootdown** (`vm.rs` two-phase unmap, `DrainedMapping`,
   `execute_tlb_shootdown_wait_plan`, `unmap_range_two_phase`).

### 13.2 Minimum accepted smoke

The minimum accepted smoke for these classes is **x86_64 `-smp 1`** core smoke:

```bash
QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh
```

If the change touches IPC/scheduler/SMP-sensitive paths and x86_64 SMP is still
known unstable, `-smp 1` is the accepted floor until the x86_64 SMP trampoline /
assembly split in `src/arch/x86_64/smp.rs` is audited (see §5.2).

### 13.3 Optional-FS strict smoke remains a regression gate

If the diff touches **filesystem-facing boot behavior** (init service spawn
ordering, optional-FS profile gates, VFS mount registration, RAMFS/ext4/FAT
spawn paths, or the SpawnV5 path), the strict optional-FS smoke is required
**in addition** to the core smoke:

```bash
QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh
QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh
```

Required markers (see §7 and §12.3) must remain present; forbidden markers must
remain absent.

### 13.4 Pure scaffold / docs / source-label / audit stages

Stages whose diff is purely:
- documentation,
- source-comment validation labels (no behavior change),
- scaffold or plan types not yet live-wired,
- audit/test-only source-scan additions,

do not require smoke runs, but MUST state the no-behavior-change claim and the
source-scan test coverage explicitly in the stage summary.

### 13.5 Do not grep `fatal` naïvely

The `fatal` substring is produced by both real fatal events and by lines of the
form `nonfatal=true`. A naïve `rg fatal` over a smoke log produces false
positives. Use a two-stage filter that excludes `nonfatal=true`:

```bash
# CORRECT (Stage 94 pattern, extended to fatal lookups in general)
fatal_count=$(tr '\r' '\n' <"$LOGFILE" | rg -ai "\bfatal\b" 2>/dev/null \
    | rg -avc "nonfatal=true" 2>/dev/null || echo 0)
```

```bash
# FORBIDDEN — matches `nonfatal=true` lines as fatal
fatal_count=$(... | rg -ai -c "\bfatal\b" 2>/dev/null || echo 0)
```

The same rule applies to `panic` (already enforced by §12.3).

### 13.6 QEMU not available is an acceptable disclosure

If the build/test environment does not have QEMU available (e.g. minimal CI
containers, remote agent sandboxes), the stage summary MUST say so explicitly
("QEMU not available; smoke not run"). It is **not** acceptable to claim a
smoke result that was not actually executed.

### 13.7 Acceptance rules for default-off diagnostic knobs (Stage 180 CI-PROFILES)

The Stage 163P / 166–179 diagnostic profiles are collected into a single runner,
`scripts/run-ci-profiles.sh` (`list` / `quick` / `full` / `extended` / individual
profile names; `--dry-run`, `--keep-going`, `--logs-dir`, `--timeout`, `--build`).
The shared fatal-marker policy lives in `scripts/qemu-smoke-common.sh`
(`log_has_fatal_breadcrumb`, `log_has_unhandled_page_fault`,
`log_has_profile_failure`). The following acceptance rules are BINDING:

1. **No stage is "ACCEPTED" without QEMU/user evidence.** A green `cargo test` and
   `--dry-run` are necessary but never sufficient; record the actual QEMU markers.
2. **The `*_ENABLED` marker alone is NOT acceptance.** A profile is accepted only
   when its invariant + proof/done markers are present (e.g. `*_INVARIANT_OK` +
   `*_PROOF_DONE result=ok`), plus the profile-specific required sequence.
3. **Handled `PAGE_FAULT_*` diagnostics are NOT fatal.** Only
   `PAGE_FAULT_UNHANDLED` / `PAGE_FAULT_FATAL` / `PAGE_FAULT_NOT_HANDLED` are fatal
   page-fault markers (see `log_has_unhandled_page_fault`); the benign
   `PAGE_FAULT_ENTRY`/`_HW_REGS`/`_FRAME_WORDS`/`_FRAME_DECODE`/`_HW_PTE_WALK`/`_RAW`/
   `_X86_ERROR`/`_CR3_COMPARE` and the handled `_HANDLED_COW`/`_HANDLED_DEMAND` are
   not (Stage 171B/173B/175B/178B narrowing).
4. **Default-off knobs are isolated under `D6_SWITCH_PROOF` / `D6_SWITCH_A`.** The
   x86_64 core smoke forces every lower-risk diagnostic knob off under those two
   proof modes; `SMP_READY` raises `QEMU_SMP` only for its own profile (normal smoke
   stays `-smp 1`); `CROSS_ARCH_D6` does not disturb x86_64 D6 paths.
5. **Counts are frozen:** SYSCALL_COUNT=31, Syscall::VARIANT_COUNT=23, x86_64
   MAX_ADDRESS_SPACES=32. Any stage that would change these must justify it
   explicitly; the diagnostic/CI stages never do.

QEMU CI is **local/manual-first**: the runner is safe to invoke without QEMU via
`list` and `--dry-run` (CI-safe); real QEMU jobs, if added, must be
`workflow_dispatch`/nightly, never a mandatory PR gate.

---

## 14. Kernel Unlocking Live-Path Rules (Stage 104–106)

> **Canonical reference:** `doc/KERNEL_UNLOCKING.md` (live-paths and
> fallbacks, §2; live-path policy fences, §8). The subsections below are
> retained for the agent-facing quick-reference contract. If they conflict
> with the canonical doc, the canonical doc wins.

### 14.1 Live split paths and their gates

| Path | Live since | Gate |
|------|-----------|------|
| D1 transfer-cap recv materialization (`materialize_received_message_cap_routed`) | Stage 104 | smoke-accepted per local Pass-1/2 runs |
| D5 reply-cap recv materialization (Phase B' fallible record-set + mint rollback) | Stage 105 | same |
| D2 endpoint blocking-recv waiter publish (`publish_recv_waiter_live`) | Stage 106 | smoke-accepted (Milestone 1 declared 2026-06-12) |

Do not remove the canonical fallbacks: `materialize_received_message_cap`
must remain at its ≥4 call sites; the notification-recv blocking path stays
canonical; sender-waiter cap-transfer refills stay on the global lock.

### 14.2 Milestone declaration honesty rule

`doc/KERNEL_UNLOCKING.md` carries an explicit
`Milestone status` line. Only an environment that has actually executed the
smoke checklist may flip it to DECLARED, recording the run results in the
acceptance table. Declaring without smoke is a hard violation of §13.

### 14.3 D2-specific invariants

- `d2_publish_race_unwinds` MUST be 0 until the SharedKernel seam split
  lands. Treat any non-zero value in a smoke log as a stop-ship bug.
- The publish primitive preserves canonical overwrite semantics
  (`D2_RECV_WAITER_DISPLACED` is observability, not a behavior change).

### 14.4 D3/D6 fences

- D3: no `with_vm_split_mut` / `with_memory_split_mut` may be added without
  the lock-free `await_tlb_shootdown_ack` design and multi-CPU smoke.
  The shootdown-before-reclaim source order inside
  `execute_tlb_shootdown_wait_plan` is UAF-load-bearing.
- D6: no per-CPU scheduler lock types until the x86_64 SMP trampoline split
  lands and D2/D3 are smoke-stable. `entering_tid`/`exiting_tid` remain
  Class F (authoritative read only).

### 14.5 Stage 108 seam and knob rules

- The Stage 108 split-mut seams (`with_scheduler_split_mut`,
  `with_task_tcbs_split_mut`, `with_vm_user_spaces_split_mut`,
  `with_memory_split_mut`) are M2_SEAM_HELPER_ONLY. Live-wiring any of them
  requires its own PR + MUST_SMOKE run + deletion of the helper-only fence
  in the same PR.
- `yarm.loglevel=` may be used in verbose smoke runs; never change the
  production default (Info), and never rely on Debug-level markers in
  acceptance greps.
- §5.2 is satisfied: the trampoline asm lives in
  `arch/x86_64/smp_trampoline.rs`. §5.1 still stands: core smoke stays
  `-smp 1` until the AP per-CPU environment exists and an SMP smoke is
  genuinely accepted (no fake SMP acceptance).

---

## 15. Source-file licensing header (canonical)

All new source files must begin with the following header, before any
other content including `#![no_std]`:

```
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan
```

Do not omit this header. Do not add any other license text.

---

## 16. Server-runtime boundary rules

The `yarm-server-runtime` crate must remain a narrow userspace
server-runtime boundary.

- It may export only intentional server-facing surfaces such as:
  - `ipc_abi`
  - `user_rt`
  - freestanding allocator installer
  - startup slot installer / helpers
- It must never depend on or re-export the root `yarm` crate.
- It must never expose `KernelState`, `Bootstrap`, `TrapFrame`,
  `ProcessManager`, `kernel::boot`, or other kernel-internal surfaces.
- Do not use `yarm-server-runtime` as a compatibility bridge for server
  crates.
- If a server needs a new runtime surface, add the smallest explicit
  userspace-facing API instead of glob re-exporting kernel internals.
