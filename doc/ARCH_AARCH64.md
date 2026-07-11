<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM AArch64

> **Ownership rule.** All AArch64-specific boot, trap, syscall, and
> userspace status documentation lives here. Generic boot flow lives in
> `doc/BOOT.md`. New AArch64 fragment files are forbidden; update this doc
> instead. See `doc/DOCUMENTATION_MAP.md`.

QEMU `virt` is the primary AArch64 development target. The boot path is
gate-hardened and the core service chain spawns to steady-state idle.

---

## 1. Status

### 1.1 QEMU virt — core smoke

Hardened progression markers required by the strict gate (ordered, not
marker-only presence):

```text
YARM_AARCH64_BOOT_MARKER stage=_start
YARM_AARCH64_BOOT_MARKER stage=prepare_arch_boot
YARM_AARCH64_BOOT_MARKER stage=vbar_el1_ready
YARM_AARCH64_BOOT_MARKER stage=mmu_enabled
YARM_AARCH64_BOOT_MARKER stage=run_with_prepared_kernel
YARM_BOOT_OK
YARM_INIT_START
YARM_INIT_DONE
```

Timer / runtime progression:

```text
YARM_TIMER_IRQ_DELIVERED
YARM_TIMER_EOI_DONE
YARM_SCHED_TICK
```

### 1.2 Core service chain

The system successfully spawns the bootstrap chain and the bootstrap FS
services:

| tid    | service          |
|--------|------------------|
| 10000  | initramfs_srv    |
| 10001  | devfs_srv        |
| 10002  | vfs_server       |
| 10003  | driver_manager   |
| 10004  | blkcache_srv     |
| 10005  | virtio_blk_srv   |

Cleared failure paths (formerly observed, now zero):

- `InvalidCapability`
- `StaleCapability`
- `ret0_nonzero_meta_unset`
- trap-handler fatal failures
- genuine capability `WrongObject` on the IPC delivery / spawn paths

### 1.2.1 Benign, cross-arch `SUPERVISOR_LIFECYCLE_QUERY_ERR err=WrongObject`

`USER_LOG tid=2 msg=SUPERVISOR_LIFECYCLE_QUERY_ERR tid=2 err=WrongObject`
appears **once** and is **not** a boot blocker. Root cause: after handoff the
supervisor issues a lifecycle *self*-query IPC to `process_manager`
(`query_lifecycle_via_process_manager`, `supervisor/service.rs`) for its own
tid to establish supervision metadata; that call's build/recv/decode failure
is folded into `KernelError::WrongObject`. The supervisor **handles it
gracefully** — it logs the line and proceeds straight into its event loop
(`SUPERVISOR_EVENT_LOOP_TICK` follows), so restart metadata is simply absent
(no restart-token source is wired). The FULL server chain (initramfs → devfs →
vfs → driver_manager → blkcache → virtio_blk, plus optional ramfs/ext4) loads
regardless.

This is a **uniform, cross-arch** condition: x86_64 and riscv64 emit the exact
same line and boot to completion. The x86_64 core smoke has no `WrongObject`
blocker at all, so it accepts it silently. The aarch64 core smoke keeps
`WrongObject` in its `BLOCKER_REGEX` (to catch genuine cap-object faults) but
excludes **only** this one benign self-query line via `BLOCKER_EXCLUDE_REGEX`
(`SUPERVISOR_LIFECYCLE_QUERY_ERR tid=[0-9]+ err=WrongObject`) — the log line is
preserved, not suppressed. This is not accepted debt: aarch64 reaches the same
server-chain parity as x86_64.

Steady-state after bootstrap is **expected quiescent idle**:

- `init_server` sends one spawn request to `process_manager` and blocks on
  `init_alert_recv_ep` after emitting `INIT_ALERT_WAIT_BEGIN cap=<...>`.
- `process_manager` is a long-lived server blocking for additional
  requests.

Repeated wake/sleep at the recv-wait PCs for `tid=1` and `tid=3` is
runtime behavior, not a crash loop.

### 1.3 Optional-FS

Strict optional-FS smoke passes on AArch64 with the canonical marker set
(see `doc/KERNEL_UNLOCKING.md` §3, "Optional-FS smoke markers"):

- `INIT_RAMFS_SPAWN_OK`, `RAMFS_SRV_ENTRY`, `RAMFS_MOUNT_READY`,
  `VFS_MOUNT_REGISTER_RAMFS_OK`
- `INIT_EXT4_SPAWN_OK`, `EXT4_SRV_ENTRY`, `EXT4_SRV_READY`,
  `VFS_MOUNT_REGISTER_EXT4_OK`
- `INIT_FAT_SPAWN_SKIPPED reason=server_disabled`

---

## 2. Boot path

The current AArch64 boot path is the result of a nine-PR sequence; the
brief reference is below. Each PR is now landed and the gate is strict.

| PR | Scope | Acceptance |
|----|-------|------------|
| 1 | Early PL011 serial + deterministic boot markers in `_start`, `prepare_arch_boot`, `run_with_prepared_kernel`; `emit_panic` wired to serial | Core smoke captures the early marker sequence; panic emits marker+message |
| 2 | Exception-vector table + `VBAR_EL1` setup; EL2→EL1 drop when booting from EL2; minimal sync/IRQ trap entry/return ABI; `CPACR_EL1.FPEN` before any FP/NEON use | Deliberate exception reaches handler and returns/halts predictably |
| 3 | DTB parsing (RAM layout, initramfs bounds, GIC base/config, timer properties); parsed values feed `prepare_arch_boot` and IRQ setup | Boot log shows parsed memory/IRQ base values |
| 4 | EL1 page tables for kernel text/data/bss/stack + direct-map; MAIR/TCR/TTBR/SCTLR with MMU-enable maintenance (`TLBI VMALLE1`, `IC IALLU`, `DSB ISH`, `ISB`) | Kernel reaches `YARM_BOOT_OK` with MMU on; no early translation faults |
| 5 | GIC init + IRQ ack/EOI; BSP timer-deadline programming; timer IRQ hooked into scheduler tick | Timer/IRQ/EOI/tick markers appear in smoke |
| 6 | Trapframe save/restore contract; syscall ABI / TLS restore; context-switch correctness | Trap/syscall unit tests pass with non-stub behavior |
| 7 | `bootstrap_first_user_task` + `enter_dispatched_user_task_if_available` with EL0 handoff via `eret`; initial cap-space bootstrap | Boot reaches user-mode handoff path without panic/hang |
| 8 | Initramfs-backed `init_server` launch reusing the manifest/loader | `YARM_INIT_START` / `YARM_INIT_DONE` observed |
| 9 | Gate hardening: ordered-progression checks + timer markers in strict mode | AArch64 strict gate green; no flaky exemptions |

PR10 (SMP/PSCI bring-up scaffold) remains a deferred follow-up:
PSCI CPU-onlining scaffold + topology integration; single-core fallback
must remain stable when SMP is disabled/unavailable.

### 2.1 Boot register handoff

- QEMU `virt` supplies the FDT pointer in `x0`. The entry path preserves
  it across the EL2→EL1 drop and passes it to `yarm_kernel_main`.
- The existing `parse_boot_dtb` parses memory, initrd, CPU, GIC, and
  PSCI metadata.
- The shared `crate::arch::fdt::chosen_bootargs` extracts the raw
  `bootargs` property; storage handles the optional trailing NUL and
  truncation.

### 2.2 Boot QEMU command

Boot QEMU with **`yarm-aarch64.bin`** (raw), not `yarm-aarch64.elf`. The
raw image path is required for correct DTB `x0` handoff during this boot
flow.

```sh
scripts/build-qemu-aarch64-artifacts.sh
QEMU_SMOKE_STRICT=1 scripts/qemu-aarch64-core-smoke.sh
QEMU_SMOKE_STRICT=1 scripts/qemu-aarch64-optional-fs-smoke.sh
```

By default the runner launches QEMU **without** `-append`; override with
`KERNEL_CMDLINE=...` if needed (see `doc/BOOT.md` §4.2).

---

## 3. Userspace bring-up notes

### 3.1 `/init` is an ELF, not a shell script

Artifact staging copies the built server ELF directly to `/init` and
marks it executable.

### 3.2 User linker base

AArch64 userspace uses a dedicated user linker script at `0x00400000`.
Kernel and user binaries must **not** share the kernel link base on
AArch64.

### 3.3 ELF PT_LOAD permission contract (W^X)

User page permissions are derived from ELF `PT_LOAD` `p_flags`
(`PF_R`/`PF_W`/`PF_X`) on a per-page basis. Overlapping `PT_LOAD`
segments are combined per page before mapping (`p_flags` OR across
overlapping load segments). Mapping policy:

| Segment kind | Final flags |
|--------------|-------------|
| Text / code | `RX` (`PF_R|PF_X`) |
| Data / BSS  | `RW` (`PF_R|PF_W`) |
| Read-only   | `RO` (`PF_R`) |

Conservative compatibility for uncommon flags:

- `PF_W` without `PF_R` maps as `RW` (no write-only user mappings).
- `PF_X` without `PF_R` maps as `RX` (no execute-only user mappings).

`PF_W|PF_X` `PT_LOAD` pages are **rejected** with
`KernelError::WrongObject` (no user W+X mappings). Boot trace
`ELF_MAP_PAGE_PERMS` reflects the final computed page flags.

### 3.4 EL1→EL0 handoff and syscall-return discipline

- Do **not** keep critical `ELR_EL1`/`SP_EL0`/`SPSR_EL1` values only in
  caller-saved registers across marker/logging calls.
- Write critical system registers immediately after loading/preserving
  their values.

### 3.5 Cooperative yield

`yield_current` handles same-task/no-peer yields safely: a cooperative
yield with only one runnable user task returns to the same task cleanly
and does not poison scheduler/current-task state.

### 3.6 Validation state

Repeated AArch64 `yield` syscalls work in QEMU with `/init` as a real ELF
and return-path ELR continuity preserved. ELF loader remains
intentionally minimal; uncommon binaries requiring writable+executable
`PT_LOAD` mappings are rejected by policy.

---

## 4. IPC and userspace status

### 4.1 Finalized IPC semantics

The following IPC architecture is **finalized** and is no longer in
unstable bring-up mode. The recv-v2 + reply-cap + blocked-waiter model is
coherent and portable.

1. **`ipc_call` is send/queue only.** No inline syscall reply
   consumption; callers consume replies via `ipc_recv_v2` or timeout
   variants.

2. **`ipc_recv_v2` ABI contract.**
   - `ret0` carries syscall success/error only.
   - All metadata in `IpcRecvMetaV2` (out-meta only).
   - No reply metadata in return lanes.
   - No inline reply prefix stripping for plain replies.
   - Reply payloads are unchanged.

3. **Portable blocked recv-v2 completion.** Generic `BlockedRecvState`
   stored in task state; tracks recv cap, payload ptr/len, meta ptr/len,
   and recv ABI variant. Delivery-time completion copies payload + the
   40-byte recv-v2 meta and resumes the waiter with syscall success. No
   ISA-specific logic in generic IPC/syscall code.

4. **Syscall replay removed.** Old blocked-recv replay model gone — no
   stale `x0` leakage, no userspace retry workaround. Waiter
   wake/complete occurs at delivery time.

5. **Reply-cap materialization stabilized.** One-shot reply objects are
   consumed once; reply objects materialized once, receiver-local only;
   raw reply handles never exposed to userspace.

6. **`ipc_reply` waiter-completion parity.** `ipc_reply` completes
   blocked waiters directly; no duplicate enqueue on the reply path.

7. **PM SpawnV5 reply-path stabilization.** Fixed: reply-prefix
   stripping corruption, nonblocking recv race, shared reply-endpoint
   cross-contamination, stale `ret0` wake behavior, duplicate reply-cap
   materialization.

### 4.2 Capability materialization rules

- Receiver-local cap IDs only.
- Reply/transfer caps materialized **before** meta write.
- Raw reply handles never exposed.
- Manually embedding raw cap-like values into message transfer fields is
  invalid; materialization requires legitimate call/send transfer flow.
- One delivered message materializes at most one receiver-local cap;
  replay/rematerialization from that same message is not possible.
- Reply caps remain one-shot.

### 4.3 Portability boundary

| Generic / portable | Architecture-specific |
|--------------------|-----------------------|
| Blocked recv state | Register mapping only (`x0` / `rax` / `a0`) |
| Delivery-time completion | ISA-specific resume semantics only |
| Payload/meta copy | |
| Capability materialization | |
| Waiter lifecycle | |
| Abstract syscall completion | |

No ISA-specific assumptions are introduced in generic IPC or syscall
logic.

### 4.4 Regression coverage

- `recv_v2_blocked_waiter_direct_delivery_consumes_exactly_once` —
  protects against blocked-waiter replay/duplicate-enqueue regressions
  and validates delivery-time payload/meta copy to waiter user buffers.
- `ipc_reply_wakes_blocked_recv_v2_waiter_without_duplicate_enqueue` —
  protects reply-path parity so `ipc_reply` completes blocked recv-v2
  waiters directly without queue duplication or stale-cap reuse
  behavior.
- `recv_v2_reports_metadata_only_via_out_meta_and_preserves_plain_reply_payload`
  — protects recv-v2 out-meta-only ABI (no metadata-in-register lanes)
  and plain-reply payload integrity (no prefix stripping/mutation).
- `recv_v2_materializes_reply_cap_once_per_message` — protects one-shot
  receiver-local cap materialization semantics: exactly one materialized
  cap per delivered message and no second receive/rematerialization.

### 4.5 PM exec-load source policy

- Image IDs `1..=3` (bootstrap-critical): PM keeps the direct kernel
  spawn path — must be reachable before VFS is guaranteed.
- Image IDs `4..=6` (`initramfs_srv`, `devfs_srv`, `vfs_server`): PM
  keeps direct-initrd/bootstrap spawn so those services come up before
  VFS-backed executable loading is available.
- Image IDs `7..=9` (`driver_manager`, `blkcache_srv`,
  `virtio_blk_srv`): after the bootstrap VFS chain is live, PM uses
  canonical initramfs paths and VFS `STATX → OPENAT → READ* → CLOSE`,
  then spawns via `spawn_process_from_user_buf`.
- PM performs VFS-backed `7..=9` loads only when explicitly given a
  `vfs_server` request SEND cap (passed from init in SpawnV5 service
  caps slot 0). Missing cap → truthful spawn failure.
- PM nested/outbound VFS calls use a dedicated PM-owned reply RECEIVE
  cap in startup slot 2 (`process_manager_reply_recv_cap`). This cap is
  a separate endpoint created during boot wiring; distinct from PM's
  main request receive endpoint (startup slot 17 /
  `pm_request_recv_cap`).
- VFS errors surface as spawn failures; PM does not silently mask
  failures.
- `VFS_READ_SHARED_REPLY_ENABLED` remains disabled in this phase.
- `initramfs_srv` must be backed by real boot CPIO bytes for VFS-backed
  `7..=9` exec to work.
- Transitional bridge: when direct boot-CPIO bytes are not mapped into
  `initramfs_srv`, it may import known initramfs files via syscall
  `ReadInitramfsFile` (`nr=25`) into a userspace-owned cache and serve
  VFS from that cache. This bridge is temporary and does not change PM
  policy.
- Long-term target: capability-scoped read-only initrd
  memory-object/cap handoff to `initramfs_srv`; `ReadInitramfsFile`
  should not become the final architecture for routine service loading.
- In runtime placeholder mode
  (`INITRAMFS_BACKEND_SOURCE source=placeholder`), late exec paths
  (`/initramfs/sbin/driver_manager`, `.../blkcache_srv`,
  `.../virtio_blk_srv`) are rejected truthfully (`Unsupported`) and are
  **not** treated as successful stat/open/read sources.

### 4.6 Global-lock retirement portability (Stage 194 audit)

**Empirical reality (QEMU virt core smoke):** the AArch64 IpcSend *boundary* family already
runs live — `GLOBAL_LOCK_RETIRE_CLASS_DONE class=IpcSendOrdinaryCap result=ok` is observed on
a normal AArch64 boot (the server chain naturally sends an ordinary cap to a blocked recv-v2
receiver), because that class's drain is the GENERIC `drain_dispatch_post_work` and AArch64 is
on the shared path. **The pre-lock split-dispatch classes `DebugLog` (NR 15, Stage 195A),
`InitramfsReadChunk` (NR 27, Stage 195B), and `FutexWake` (NR 10, Stage 195C) are now LIVE on
AArch64** via the selective ABI-import gate (see below); the queue-advancing classes (`D2`,
`FutexWait`, `Yield`) remain inert / global-lock-only on AArch64. At the time of the Stage 194
audit none of the split-dispatch classes were enabled by flipping a flag — each required the
selective de-gating described below. This split (boundary family already generic-and-live;
queue-advancing still arch-blocked) is the core Stage 194 finding.

- **AArch64 IS on the shared, drain-capable trap path.** The primary vector handler
  (`arch/aarch64/boot.rs`) marshals the EL1 exception frame into a portable `TrapFrame`
  and calls `dispatch_trap_entry_with_shared_kernel` → `handle_trap_entry_shared`, exactly
  like x86_64. It therefore owns `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu]` correctly (set
  before `with_cpu`, cleared after) and runs the generic `drain_dispatch_post_work`.
- **Correction to earlier Stage 194 wording (do NOT read the active-flag rules as
  "AArch64 never sets the flag").** AArch64 already owns the generic shared post-lock drain
  and DOES set `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE` true while that drain is guaranteed to
  run — that is exactly why the generic IpcSend boundary deliveries already complete live on
  AArch64. Only the queue-advancing, x86_64-only retirement classes (`D2`/`FutexWait`/`Yield`)
  remain inactive on AArch64; those are the paths still awaiting a de-gated drain body + an
  EL0-return-frame restore proof. Setting the active flag is not the blocker; the missing
  queue-advancing drain body is.
- **The arch restore hook exists:** `restore_arch_thread_state_post_switch` (the AArch64
  arm of `post_switch_restore_arch_thread_state`).
- **DebugLog is now LIVE on AArch64 (Stage 195A).** The ABI import
  (`pre_split_import_syscall_abi`) and handled-syscall finalize
  (`finalize_split_handled_syscall`) are de-gated **selectively for DebugLog (NR 15)**: the
  import peeks the raw `x8` and imports the decoded ABI only when it is DebugLog (or when the
  oracle knob is on), so DebugLog reaches `try_split_dispatch_into_frame` and every other
  syscall keeps `nr=0` and falls back to the unchanged global-lock path. A normal AArch64 boot
  now emits `AARCH64_SPLIT_ABI_IMPORT_OK nr=15`, `YARM_LOCK_SPLIT_DISPATCH arch=aarch64 nr=15`,
  `GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=DebugLog result=ok`, and
  `AARCH64_SPLIT_FINALIZE_OK nr=15 result=ok`. The DebugLog user-copy seam runs lock-free
  (`copy_from_user_asid_split_read`); the finalize's brief `with_cpu` is the arch return-path
  restore (export x0..x5 + advance past the SVC), not the seam. Success/error registers are
  byte-identical to the legacy `handle_debug_log` path (same `set_ok(0,0,0)` / `set_err`, same
  `export_syscall_result_to_user_gprs`).
- **InitramfsReadChunk is now LIVE on AArch64 (Stage 195B).** The selective ABI-import gate is
  extended to `NR 15 || NR 27 || oracle`, so InitramfsReadChunk (NR 27) reaches
  `try_split_initramfs_read_chunk_into_frame`. Only the **success path** is retired: the helper
  returns `None` (unchanged global-lock fallback) for every access-gate / arg / not-found /
  unwritable-destination / ASID-unavailable case, so `MissingRight` / `InvalidArgs` /
  `UserMemoryFault` stay canonical. The destination copy is a **two-pass validated write**
  (`copy_slice_to_user_asid_split_write` validates every destination page before writing any
  byte), so a fault leaves **zero** bytes written and falls back with no mutation — no partial
  user write. Immutable initramfs/CPIO data only; no structural mutation, no allocation, no cap
  mint, no scheduler/IPC mutation, no TTBR0/ASID switch. Live markers:
  `AARCH64_SPLIT_ABI_IMPORT_OK nr=27`, `YARM_LOCK_SPLIT_DISPATCH arch=aarch64 nr=27`,
  `GLOBAL_LOCK_RETIRE_CLASS_DONE arch=aarch64 class=InitramfsReadChunk result=ok`,
  `AARCH64_SPLIT_FINALIZE_OK nr=27 result=ok`. DebugLog behavior is byte-for-byte unchanged.
- **Split return-value parity fix (Stage 195B).** Enabling a return-value-checking split class
  (InitramfsReadChunk's caller reads x0/x1) surfaced a latent AArch64 split-finalize bug that
  DebugLog had masked (it ignores its return): (a) the split resume PC used `ELR + 4`, but the
  synchronous-exception `ELR_EL1` for an `SVC` already points at the instruction AFTER the
  `SVC`, so the `+4` over-advanced by one instruction and skipped the caller's return-register
  load (`mov rN, x0`) — the resume PC is now raw `ELR` (no `+4`), matching the proven global
  non-IpcRecv path; (b) the finalize now resyncs `args[0..2]` to the exported `x0..x2` and
  re-saves the TCB AFTER export, so a preemption resume reads the return value rather than the
  original syscall args. Verified: PM's NR 27 self-probe returns `bytes=16` (was `Internal`),
  DebugLog still live, x86_64/RISC-V unaffected (the fix is in `arch/aarch64/trap.rs`).
- **FutexWake is now LIVE on AArch64 (Stage 195C).** The selective ABI-import gate is extended
  to `NR 15 || NR 27 || NR 10 || oracle`, so FutexWake reaches
  `try_split_futex_wake_into_frame`. **FutexWake is NR 10** (the Stage 195C task text's "NR11"
  is incorrect — NR 11 is `SpawnThread`, NR 9 is `FutexWait`). The caller never task-switches;
  the split only mutates waiter/run-queue state via the two-seam
  `futex_wake_split_mut` (Phase A rank-2 task seam scans `Blocked(Futex)` → `Runnable`; Phase B
  rank-1 scheduler seam enqueues each woken waiter once). Every ineligible case (invalid addr)
  returns `None` → unchanged global-lock fallback with the canonical error. Live markers:
  `AARCH64_SPLIT_ABI_IMPORT_OK nr=10`, `YARM_LOCK_SPLIT_DISPATCH arch=aarch64 nr=10`,
  `FUTEX_WAKE_SPLIT_BEGIN/DONE arch=aarch64 result=ok woke=N`,
  `GLOBAL_LOCK_RETIRE_CLASS_BEGIN/DONE arch=aarch64 class=FutexWake result=ok`,
  `AARCH64_SPLIT_FINALIZE_OK nr=10 result=ok`. DebugLog / InitramfsReadChunk behavior is
  byte-for-byte unchanged. `FutexWait` (NR 9) stays global-lock-only — it BLOCKS the caller.
- **AArch64 FutexWake live oracle (Stage 195C, default-off).** Under
  `yarm.aarch64_futex_wake_oracle=1` (which provisions init startup slot 5 as a sentinel), init
  runs a controlled parent/child proof of *actual waiter mutation*: init spawns a child thread;
  init blocks on a handshake futex to hand the CPU to the never-run child (AArch64 fresh-
  dispatches it through the block/dispatch path — the same one that first enters the control-
  plane servers into user mode; `yield` cannot fresh-dispatch a never-run thread because its
  `kernel_context` is uninitialized); the child wakes init (split FutexWake) and then blocks on
  the oracle word through the **legacy global-lock** `FutexWait`; init resumes and wakes the
  child through the **split** path — the kernel's returned wake COUNT is the authoritative proof
  (not timing): `first_wake=1` (the sole waiter → `Runnable`, enqueued once), then `second_wake=0`
  (no waiter remains). Proof marker: `AARCH64_FUTEX_WAKE_LIVE_ORACLE_DONE result=ok first_wake=1
  second_wake=0`. The oracle boots single-CPU (`QEMU_SMP=1`): AArch64 dispatches user tasks on
  the BSP only, and the freshly-spawned waiter is enqueued balanced, so on SMP>1 it can land on
  a wake-only AP and never run. This is a single-dispatcher proof; the FutexWake *enablement*
  itself is unaffected by CPU count.
- **Still inert on AArch64 (unchanged this stage):** the queue-advancing classes
  (`D2`/`FutexWait`/`Yield`), which remain `#[cfg(target_arch = "x86_64")]` and await a de-gated
  drain body + an EL0-return-frame restore proof. `CreateInitramfsFileSliceMo` (NR 28,
  cap-minting) stays global-lock-only.

### 4.7 BSP dispatch affinity (Stage 195D)

Before Stage 195D the AArch64 secondary bring-up (`start_secondary_cpus`) onlined each AP via
PSCI **without** marking it wake-only. The AArch64 AP main loop
(`yarm_aarch64_secondary_cpu_boot`) only drains cross-CPU work and `wfe`s — it runs **no user
dispatcher** — yet the scheduler saw it as an online, non-wake-only, empty (least-loaded) CPU.
Two consequences followed on `-smp 2`:

1. `CROSS_ARCH_DISPATCHING_CPUS arch=aarch64 online=2 wake_only=0 dispatching=2` →
   `CROSS_ARCH_TOPOLOGY_BLOCKED reason=multi_dispatcher_unproven` (never surfaced because the
   aarch64 core smoke's strict cross-arch block is gated behind a boot-shell marker AArch64
   does not emit at the idle terminal).
2. An **unpinned** runnable user task (BSP-pinned servers were unaffected) — e.g. the Stage 195C
   `SpawnThread` oracle child — could be balanced onto the AP's queue and **strand** forever,
   which is exactly why the 195C oracle needed `-smp 1`.

Stage 195D marks every AArch64 AP **wake-only before onlining it** (mirroring the x86_64 183.5
AP bring-up) and installs the scheduler-owned idle current. This reuses the existing wake-only
placement infrastructure — `enqueue_balanced` → `least_loaded_online_cpu` skips wake-only CPUs,
`enqueue_on_with_priority` denies direct placement (`SCHED_ENQUEUE_DENIED_WAKE_ONLY`), and
`dispatching = online - wake_only` collapses to 1. No new lock; AP kernel/interrupt/cross-CPU
work is preserved (the AP is not user-dispatching, so it is correctly excluded from user-ASID
TLB shootdowns — it never loads a user TTBR0). Markers:
`AARCH64_BSP_DISPATCH_AFFINITY_ACTIVE result=ok`,
`AARCH64_USER_TASK_PLACEMENT_OK tid=<tid> cpu=0` (every user placement lands on the BSP), and
`AARCH64_WAKE_ONLY_AP_QUEUE_REJECTED tid=<tid> cpu=<ap>` (only on a real prevented placement).
Result: AArch64 `-smp 2` now attests `CROSS_ARCH_TOPOLOGY_OK reason=single_dispatcher`, and the
Stage 195C FutexWake live oracle passes under **both** `-smp 1` and `-smp 2` (the SMP=1
requirement is retired).

The AArch64 queue-advancing **FutexWait** retirement (moving the dispatch phase out of the broad
lock, x86_64 192A model) is the next slice; its first blocking return-path invariant is the
in-lock `idle_no_eret_loop()` (`arch/aarch64/trap.rs`), which fires when the deferred
`futex_wait_current` leaves `current == None` and would spin **inside** the global lock before
the out-of-lock drain can run — see `doc/KERNEL_UNLOCKING.md` Stage 195D.
- **Next AArch64 slice:** `IpcSendPlainEnqueue` (rank-4 enqueue, no drain).

### 4.8 FutexWait queue-advancing drain — LIVE (Stage 195E)

Stage 195E makes the AArch64 **FutexWait (NR 9)** queue-advancing dispatch live out of the broad
lock, porting the proven x86_64 192A model **without any CR3 logic**. Default-off behind
`yarm.aarch64_futex_wait_oracle=1` (which enables the retirement and runs the live oracle); the
proven in-lock FutexWait path stays the production default. Three cooperating pieces:

1. **Handler bypass** (`arch/aarch64/trap.rs`). The 195D blocker — `idle_no_eret_loop()` firing
   inside `with_cpu` when `current == None` — is resolved by a **FutexWait-deferral-specific**
   bypass: when `futex_wait_dispatch_is_deferred(cpu)` is true and `current` is None/idle, the
   handler skips the idle loop **and** the in-lock restore and returns cleanly so
   `handle_trap_entry_shared` can run the post-lock drain. Any other None/idle case keeps the
   exact `idle_no_eret_loop()` behavior. Markers: `AARCH64_FUTEX_WAIT_HANDLER_BYPASS_BEGIN/DONE`.
   The blocked caller's context is still saved by the `task_switched` block (entering ≠ None).
2. **In-lock deferral** (`futex_wait_current`). Eligible only on the **BSP** with the shared trap
   drain active, `dispatching_cpu_count() <= 1` (195D BSP affinity guarantees this under SMP=2),
   **another runnable task present** (so the drain always has an incoming; otherwise fall back to
   the in-lock path which idles correctly), and no outstanding deferral. It publishes
   `Blocked(Futex)`, clears `current`, records the one-shot deferral, and skips the in-lock
   dispatch. Markers: `AARCH64_FUTEX_WAIT_DISPATCH_DEFER_BEGIN`, `..._BLOCK_PUBLISH_OK`.
3. **Post-lock drain** (`handle_trap_entry_shared`). Generic seams (deferral ownership, blocked
   reverify `futex_wait_reverify_blocked`, rank-1 dequeue/current `futex_wait_dispatch_step_mut`,
   mark Running `d6_genuine_mark_running_via_task_seam`, cleanup) + **AArch64 arch hooks**:
   TTBR0_EL1/ASID switch via the generic HAL `switch_address_space` (carrying the DSB/ISB/TLBI
   ordering) and EL0 SPSR/ELR/GPR frame restore via `restore_arch_thread_state_post_switch`.
   Markers: `AARCH64_FUTEX_WAIT_DISPATCH_{REVERIFY_OK,DEQUEUE_OK,CURRENT_SET_OK,RUNNING_OK,
   TTBR0_OK,FRAME_OK,DONE result=ok}` + `GLOBAL_LOCK_RETIRE_CLASS_BEGIN/DONE arch=aarch64
   class=FutexWait result=ok`. **Race:** if split `FutexWake` flips the waiter to `Runnable`
   before the drain, `futex_wait_reverify_blocked` fails → `AARCH64_FUTEX_WAIT_DISPATCH_DEFERRED
   reason=state_changed` (no stale/double dispatch, no lost waiter); the deferral is cleared on
   every path (success/decline).

**Live oracle** (`FUTEX_WAIT_ORACLE=1`): task A (init) blocks via NR 9 → handler bypass → the
drain dispatches task B (the spawned child) → B wakes A via split FutexWake (NR 10) → A resumes
once. Proof: `AARCH64_FUTEX_WAIT_LIVE_ORACLE_DONE result=ok blocked_tid=<A> dispatched_tid=<B>
wake_count=1`, proven under **both** `-smp 1` and `-smp 2` (SMP=2 is the acceptance target; the
195D BSP affinity keeps the freshly-spawned task B on the BSP dispatcher). Yield remains inert.

### 4.9 FutexWait DEFAULT-ON + post-lock idle seal (Stage 195F)

Stage 195F makes the FutexWait out-of-lock retirement the **default production path** on eligible
AArch64 traps — **no enable knob**. The 195E eligibility drops the `runnable_count_on_cpu > 0`
requirement; the post-lock drain now has **two successful outcomes**:

- **Switch** (byte-identical to 195E): an incoming runnable task exists → dequeue + restore it.
- **Idle** (new): no incoming runnable task → the outgoing caller stays `Blocked(Futex)`,
  `current` stays None, the deferral is cleared, **no frame is restored**, and the BSP enters the
  real idle loop (`enter_post_lock_idle` → the same `idle_no_eret_loop` `wfi` primitive) **AFTER
  the broad `with_cpu` lock is released** — never `idle_no_eret_loop()` while holding `with_cpu`.
  Markers: `AARCH64_FUTEX_WAIT_DISPATCH_NO_INCOMING`, `..._POST_LOCK_IDLE_BEGIN`,
  `..._POST_LOCK_IDLE_LOCK_DROPPED_OK` (a real re-acquire of the released broad lock, which would
  deadlock if still held), `..._DISPATCH_DONE result=idle`, `..._POST_LOCK_IDLE_ENTERED`, plus the
  `class=FutexWait result=ok` retirement. The eligible-trap attestation `AARCH64_FUTEX_WAIT_RETIRE_DEFAULT_ON
  result=ok` fires once at the first default-on deferral.

**Interrupt/idle correctness:** `DAIF` is left as the trap left it (IRQs are not permanently
masked); `wfi` wakes on a pending unmasked interrupt, which enters the normal AArch64 trap path
(freely re-acquiring `with_cpu` because it is released here) and either dispatches a now-runnable
task or returns to the `wfi`. `current == None`, so no stale userspace ELR/SPSR/frame is ever
returned. This reuses the proven BSP idle primitive — not a second idle policy. The legacy in-lock
`dispatch_next_task` fallback is retained ONLY for genuinely ineligible traps (no trap drainer,
multi-dispatcher, non-BSP, already-deferred). Idle oracle (default-off, `FUTEX_WAIT_IDLE_ORACLE=1`):
the last runnable user task blocks on a never-woken futex →
`AARCH64_FUTEX_WAIT_IDLE_ORACLE_DONE result=ok lock_dropped=1 current_none=1`, then QEMU idles
until the smoke timeout. Yield remains inert (until Stage 195G); AP user dispatch is not enabled.

### 4.10 Yield queue-advancing dispatch — DEFAULT-ON (Stage 195G)

Stage 195G makes AArch64 **Yield (NR 0)** the fourth live queue-advancing out-of-lock class,
**default-on** (no knob), reusing the FutexWait infrastructure. Yield is the *preempt* sibling of
FutexWait, so the publication differs: the caller is set **Runnable** and **re-enqueued exactly
once** at its priority queue tail (not Blocked), so there is **ALWAYS an incoming** task — another
runnable task, or the yielding caller itself when alone — and therefore **NO idle outcome**.

- **Handler bypass** (`arch/aarch64/trap.rs`): a Yield-deferral-specific bypass parallel to
  FutexWait — when `yield_dispatch_is_deferred(cpu)` and `current` is None/idle, skip
  `idle_no_eret_loop()` + the in-lock restore and return cleanly so the post-lock Yield drain
  runs. Any other None/idle keeps the exact idle behavior (`post_lock_bypass = futex_wait_bypass
  || yield_bypass`). Markers: `AARCH64_YIELD_HANDLER_BYPASS_BEGIN/DONE`.
- **Re-enqueue-only publication** (`yield_current`): default-on, eligible on the BSP with the
  shared trap drain active + `dispatching_cpu_count() <= 1` + not-already-deferred. Sets the
  caller Runnable, re-enqueues it once via `preempt_reenqueue_current_cpu` (the proven 192B
  seam) + clears `current`, records the one-shot deferral, skips the in-lock `on_preempt`
  dispatch. On re-enqueue failure `preempt_reenqueue_current_cpu` leaves `current` untouched, so
  the legacy in-lock `on_preempt_current_cpu` fallback runs cleanly. Markers:
  `AARCH64_YIELD_DISPATCH_DEFER_BEGIN`, `..._REENQUEUE_OK`; attestation
  `AARCH64_YIELD_RETIRE_DEFAULT_ON result=ok`.
- **Post-lock drain** (`handle_trap_entry_shared`): generic seams un-gated to AArch64
  (`yield_reverify_ready` — re-verify `current` still cleared; `yield_dispatch_step_mut` — rank-1
  dequeue of the FIFO head; `d6_genuine_mark_running_via_task_seam`) + the SAME AArch64 arch
  hooks as FutexWait — TTBR0_EL1/ASID via `switch_address_space` (DSB/ISB/TLBI) + EL0 frame via
  `post_switch_restore_arch_thread_state`. **No CR3.** A published Yield always has an incoming,
  so no-incoming is a genuine failure (never fires). Markers:
  `AARCH64_YIELD_DISPATCH_{REVERIFY_OK,DEQUEUE_OK,CURRENT_SET_OK,RUNNING_OK,TTBR0_OK,FRAME_OK,DONE
  result=ok}` + `GLOBAL_LOCK_RETIRE_CLASS_BEGIN/DONE arch=aarch64 class=Yield result=ok`.

Oracles (default-off): **two-task** (`YIELD_ORACLE=1`) — A yields, drain dispatches B, B runs and
blocks, A resumes: `AARCH64_YIELD_TWO_TASK_ORACLE_DONE result=ok outgoing=<A> incoming=<B>`;
**lone-task** (`YIELD_LONE_ORACLE=1`) — the sole runnable task yields and the drain re-dispatches
it (same-task, no idle): `AARCH64_YIELD_LONE_TASK_ORACLE_DONE result=ok tid=<A>
redispatched_self=1`. Both proven under `-smp 1` and `-smp 2`. D2 recv/send drains stay inactive;
AP user dispatch not enabled.
- **TLB/ASID:** local TTBR0_EL1 + ASID switch with the existing `TLBI`/`DSB ISH`/`ISB`
  maintenance is sufficient for the first slices; broadcast `TLBI` is only needed for
  VM/SMP classes, which are out of scope. QEMU virt and RPi5 share the same generic drain —
  no board-specific drain logic.

See `doc/KERNEL_UNLOCKING.md` §7.1.21 for the Stage 195/197 plans and seal gates.

---

## 5. Hosted-dev harness note

- Hosted-dev sparse user-memory backing guarantees readability only for
  bytes actually written by the kernel/user-memory path in tests.
- recv-v2 tests should read back the actual payload length written, not
  full receive-buffer capacity.
- Syscall / blocked-recv tests must pass mapped user virtual addresses
  as user pointers; host stack pointers are invalid for user copy paths.

---

## 6. Pointers to current smoke commands

```sh
scripts/build-qemu-aarch64-artifacts.sh
QEMU_SMOKE_STRICT=1 scripts/qemu-aarch64-core-smoke.sh
QEMU_SMOKE_STRICT=1 scripts/qemu-aarch64-optional-fs-smoke.sh
```

See `doc/BOOT.md` §4.2 for default `-append` policy and
`doc/KERNEL_UNLOCKING.md` for the optional-FS marker contract.

---

## 7. Authoring rule

Future AArch64 docs update **this file**. Cross-arch / generic boot docs
update `doc/BOOT.md`. Raspberry Pi 5 hardware bring-up updates
`doc/RPI5_BRINGUP.md`.
