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
on the shared path. **The pre-lock split-dispatch classes (`DebugLog`, `FutexWake`,
`InitramfsReadChunk`) and the queue-advancing classes (`D2`, `FutexWait`, `Yield`) remain inert
/ global-lock-only on AArch64** — nothing there is enabled by flipping a flag. This split
(boundary family already generic-and-live; split-dispatch + queue-advancing still arch-blocked)
is the core Stage 194 finding.

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
- **Still inert on AArch64 (unchanged this stage):** `FutexWake`, `InitramfsReadChunk` (split
  classes not yet de-gated), and the queue-advancing classes (`D2`/`FutexWait`/`Yield`), which
  remain `#[cfg(target_arch = "x86_64")]` and await a de-gated drain body + an EL0-return-frame
  restore proof.
- **Next AArch64 slices (Stage 195B+):** `InitramfsReadChunk` success path, then
  `IpcSendPlainEnqueue` (rank-4 enqueue, no drain).
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
