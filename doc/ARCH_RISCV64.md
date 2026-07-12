<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM RISC-V64

> **Ownership rule.** All RISC-V64-specific boot, trap, syscall, AP/SMP,
> Sv39, U-mode, and userspace status documentation lives here. Generic
> boot flow lives in `doc/BOOT.md`. New RISC-V64 fragment files are
> forbidden; update this doc instead. See `doc/DOCUMENTATION_MAP.md`.

RISC-V64 development target is QEMU `virt` with OpenSBI. The boot path
is deterministic; Sv39 paging and a real S-mode → U-mode `sret`
round-trip are working; the core service chain reaches steady-state
event-driven idle. Secondary harts park before global init.

---

## 1. OpenSBI handoff

OpenSBI enters the boot hart in S-mode with:

| Register | Meaning |
|----------|---------|
| `a0` | hart ID |
| `a1` | FDT (DTB) physical pointer |

**Both registers must be preserved across early setup.** `_start`
stashes `a0` → `s0` and `a1` → `s1` immediately, then forwards `a1` →
`a0` into the Rust primary entry so the FDT is reachable for cmdline /
memory / initrd parsing. The primary hart ID is currently not consumed
by this boot path; CPU identity uses existing architecture mechanisms.

Previously `_start` directly called the one-argument `yarm_kernel_main`,
so the hart ID was misinterpreted as the FDT pointer. The fix performs
`mv a0, s1` (after stashing) immediately before that call. No SMP,
secondary-hart, interrupt, memory-map, or platform policy is changed by
this correction.

`prepare_arch_boot` now validates the actual FDT and captures
`/chosen/bootargs`.

---

## 2. Secondary hart park (before global init)

Only the bootstrap hart (id 0) continues into kernel bootstrap. Any
other hart that reaches the cold-boot entry parks in a safe `wfi` loop
**before** BSS use, allocator init, cmdline capture, or kernel
bootstrap.

On QEMU `virt` + OpenSBI, non-boot harts wait in firmware for an HSM
`hart_start`; the bootstrap path drives that start so each secondary
lands in `yarm_riscv64_secondary_boot`, emits
`RISCV_SECONDARY_HART_PARK hart=N`, and spins in `wfi`.

| Property | Status |
|----------|--------|
| Bootstrap-hart `_start` keeps the normal kernel path | ✓ |
| Secondary entry uses a dedicated park path (not `_start` / not `yarm_kernel_main`) | ✓ |
| Per-hart handoff pointer consumed from SBI `opaque` (a1 of the HSM-started hart) | ✓ |
| Secondary installs a local park trap vector and `csrc sstatus, SIE` (interrupts off) | ✓ |
| BSP logs whether each secondary reached the parked path | ✓ |
| Failures (e.g. no second hart on `-smp 1`) are logged non-fatally | ✓ |
| Secondaries marked scheduler-online | **No** (deliberate — not yet) |

### 2.1 QEMU `virt` hart-ID assumption

The release loop is intentionally limited to the QEMU `virt` / OpenSBI
profile and uses the conservative hart-ID range `0..8`, skipping
`BOOTSTRAP_CPU_ID` (`0`). This is a profile assumption, not a real DTB
CPU map. Failed `hart_start` calls (e.g. on single-hart `-smp 1`) are
logged and non-fatal so single-hart QEMU boot remains preserved.

`prepare_arch_boot()` locates a DTB blob but does **not** yet stage
parsed RISC-V CPU IDs for `Bootstrap::init()`. The RISC-V topology
helper still parses only text-fixture shapes such as
`/cpus { cpu@1 { }; }`, not binary FDT CPU nodes.

### 2.2 SBI HSM status

`probe_extension(SBI_EXT_HSM)` is checked first. If HSM is missing /
probing fails, the release hook logs the result and returns without
changing boot behavior. HSM is used only to start harts into the parked
secondary path. HSM status is **not** used to drive scheduler onlining.

### 2.3 Remaining blockers before real RISC-V SMP

1. Real FDT `/cpus` parsing that records firmware hart IDs separately
   from scheduler `CpuId` indices and stages the discovered topology
   before `Bootstrap::init()`.
2. A secondary handoff that includes root page-table / `satp` details
   and a shared-kernel pointer once secondaries are ready to run kernel
   work instead of parking.
3. Secondary-local initialization for `satp`, `sfence.vma`, trap
   vectors, interrupt/timer state, and per-CPU scheduler identity.
4. A scheduler/topology handshake where the BSP marks a CPU online only
   after the secondary has acknowledged complete local initialization.
5. QEMU `virt` gating based on parsed platform identity rather than only
   the current compile-time profile assumption.

### 2.4 Explicit non-goals (current staged path)

- Does **not** make RISC-V fully SMP-capable.
- Does **not** make parked harts scheduler-online.
- Does **not** run user tasks or kernel scheduler work on secondaries.
- Does **not** provide VisionFive 2 or other hardware-board hart-ID
  policy (deferred until real DTB / firmware parsing or an explicit
  board profile lands).

---

## 3. Monotonic cmdline capture

The cmdline capture path is **monotonic and capture-once**: the first
call wins and records the command line; a later re-entry that no longer
has a valid DTB pointer must NOT replace a valid cmdline with an empty
one.

Helper: `crate::kernel::boot_command_line::set_raw_cmdline_from_bytes_monotonic`.

Markers emitted by the once-guarded RISC-V capture:

```text
RISCV_BOOT_ENTRY hart=0 dtb=0x...
RISCV_BOOT_HART_SELECTED hart=0
RISCV_DTB_PTR value=0x9fe00000     # (or equivalent, per platform)
RISCV_CMDLINE_CAPTURE_ONCE len=N
RISCV_DTB_PARSE_FAILED reason=...  # one of the failure paths
RISCV_CMDLINE_PRESERVED reason=missing_dtb_after_valid
```

Pre-fix symptom: thousands of `YARM_BOOT_CMDLINE_CAPTURE arch=riscv64 ...
source=missing_dtb` repetitions appeared after the first valid capture,
because a pre-kernel fault re-entered the payload entry with a garbage
pointer. The capture-once guard + early trap vector together replaced
the silent loop with a single deterministic diagnostic.

---

## 4. DTB RAM and initrd staging

RISC-V64 stages the real RAM window from the DTB `/memory` node and
reserves firmware / DTB / initramfs before frame-allocator init. Without
this, the common fallback memory map seeds allocators with MMIO addresses
(QEMU virt UART region, **below RAM at `0x80000000`**) and the first
frame write store-faults (`scause=7`).

The shared `crate::arch::fdt` module exposes:

- `chosen_bootargs(bytes)` (already used by AArch64).
- `memory_reg(bytes)` — first `/memory` node's first `reg` pair as
  `(base, size)`, honoring root `#address-cells` / `#size-cells`
  (default 2/2 = QEMU virt).
- `chosen_initrd(bytes)` — `/chosen` `linux,initrd-start` /
  `linux,initrd-end` pair as `(start, end)` physical addresses (4- or
  8-byte big-endian integer property values).

`stage_riscv64_boot_memory` stages real RAM (`stage_detected_ram_for_bootstrap`),
reserves firmware (RAM base up to the kernel image), DTB blob, and
initrd window; it then `install_boot_initrd_bytes(...)` and
`install_boot_extra_reserved_ranges(...)`.

---

## 5. Bootstrap / static `KernelState` fixes

Three bugs in the early bootstrap chain were found via instrumentation
and fixed (all in the RISC-V boot path; nothing common changed):

1. **Boot stack 1000× too small.** RISC-V boot stack was 16 KiB while
   x86_64 / AArch64 use 16 MiB. The bare-metal `KernelState` is
   ≈4.27 MiB (arrays stored inline off `hosted-dev`) and `Bootstrap::init`
   holds several copies on the stack → overflow corrupted a saved return
   address so `ret` jumped to `~-2` (the original
   `sepc=0xfffffffffffffffe`). **Fix:** 16 MiB boot stack (`.bss`,
   `NOLOAD`).

2. **Frame allocator seeded from MMIO.** `NEXT_ANON_PHYS_BASE = 0x1000_0000`
   is the QEMU-virt UART, below RAM → first frame write faulted
   (`scause=7`, `stval=0x10000008`). **Fix:** stage real RAM from DTB
   `/memory`, reserve firmware / DTB / initramfs (see §4).

3. **Heap-boxing 4.27 MiB from the 1 MiB PT pool.** `Bootstrap::init()`
   (`init_boxed → Box::new`) OOM'd into a silent panic-loop. **Fix:**
   RISC-V now uses `Bootstrap::init_static()` (in-place `.bss`), like
   x86_64 / AArch64.

After these, RISC-V reaches `YARM_BOOT_OK`, `RISCV_KERNEL_BOOT_OK`,
`YARM_SUPERVISOR_TID2_SPAWNED`, `YARM_PM_TID3_SPAWNED`, `YARM_INIT_DONE`.

An early S-mode trap vector is installed by `_start` **before** any
kernel code so a fault in the boot path becomes a single deterministic
diagnostic (`RISCV_EARLY_TRAP scause=0x... sepc=0x... stval=0x...`)
instead of an invisible payload-reset loop. The reporter also prints
the named bootstrap step via `RISCV_BOOTSTRAP_TRAP_STEP name=...`.

---

## 6. Sv39 design — kernel-shared gigapage + user roots

RISC-V uses Sv39 paging with a **kernel-shared gigapage at root index 2**
covering `[0x8000_0000, 0xC000_0000)` — the entire RISC-V kernel link
range (text / rodata / data / bss, all stacks, the S-mode trap vector).

| Property | Value |
|----------|-------|
| `RISCV_KERNEL_SHARED_BASE` | `0x8000_0000` |
| `RISCV_KERNEL_SHARED_END` | `0xC000_0000` |
| PTE flags | `V|R|W|X|G|A|D` (S-mode only; **no U** bit) |
| Install fn | `map_kernel_shared_into_asid(asid)` (idempotent) |

The gigapage is installed into **every user address-space root** so the
kernel and trap vector keep executing across a `satp` switch into a user
page table. U-mode cannot reach kernel memory (no U bit on the leaf).

User ELF mappings (R / W / X + U) live at low VAs (≤ `0x4000_0000`),
unaffected by the gigapage at index 2.

### 6.1 Page-table fixes (load-bearing)

Three bugs in the page-table module were found and fixed when paging
was first enabled:

1. **PTEs were written to the software shadow only.** The MMU walks the
   physical frame. Added `store_pte_to_frame` at every write site
   (intermediate, leaf, unmap) plus `zero_pt_frame` on alloc so the
   hardware never walks stale memory.

2. **Non-leaf PTEs had the USER bit.** Per the Sv39 spec, U/A/D/G on
   non-leaf PTEs are reserved and "must be cleared by software for
   forward compatibility." QEMU treats U=1 on an intermediate as a bad
   leaf → instruction page fault on the very first user fetch even with
   a correct leaf. `table_flags_from_page_flags` now returns `VALID`
   only.

3. **No kernel-shared mapping.** A `csrw satp` into a user-only PT
   would unmap the kernel's next fetch (silent death). Fixed by the
   gigapage above.

### 6.2 Console — no UART MMIO mapping required

The RISC-V console uses **SBI ecall** (M-mode `console_putchar`), not
direct UART MMIO. No UART mapping is required in user roots while
debugging the U-mode entry path; the `RISCV_SV39_MAP_UART` marker is
emitted with `va=0x0 pa=0x0 note=sbi_console_no_mmio_needed`.

---

## 7. Real U-mode `sret`

`yarm_riscv64_enter_user` performs the real `sret` into U-mode:

1. Loads the per-task `satp` (kernel-shared gigapage already installed).
2. Sets `sscratch` to a kernel trap stack.
3. Installs the S-mode trap vector via `csrw stvec`.
4. Programs `sstatus`: clears `SPP` (bit 8) so sret returns to U-mode
   and `SPIE` (bit 5) so interrupts stay disabled across the
   transition (no timer / IRQ path yet); sets `SUM` (bit 18) so the
   kernel can touch U-pages from S-mode for trap bookkeeping if/when we
   resume there. Atomic CSR clear/set (`csrc` / `csrs`) is used, not
   read-modify-write.
5. Loads user GPRs (`a0..a5`, `tp`), user `sp`, sets `sepc` to user
   entry, executes `sfence.vma; sret`.

The trap reporter decodes `sstatus.SPP` at trap time to verify the trap
was taken from U-mode (proves the `sret` actually landed in U). Markers
include `from_u=1 spp=0` on first arrival.

---

## 8. Syscall round-trip status

`yarm_riscv64_trap_vector` saves a full `RiscvTrapFrame` (all 31 GPRs
except x0, plus `sepc` / `sstatus` / `scause` / `stval`) on the kernel
trap stack; the Rust bridge dispatches through the existing
`crate::arch::riscv64::trap::handle_trap_entry` Rust path; the trap
return tail restores GPRs and CSRs and executes `sret`.

| Aspect | Mapping |
|--------|---------|
| Syscall number | `a7` |
| Args | `a0..a5` |
| `sepc` advance for ecall | `+4` (RISC-V `ecall` does not auto-advance) |
| Return — same task, ecall | `a0=ret0`, `a1=ret1`, `a2=ret2`, `a3=error` (mirrors AArch64) |
| Return — task switch (freshly-spawned task) | `a0..a5` seeded from `tframe.arg(0..5)`; `user_gprs` are zero on first run |

The bridge activates the resumed task's `satp` (with kernel-shared
gigapage ensured present) so `sret` lands in the right user page table.

### 8.1 Markers (steady-state)

First round-trip emits the full required marker set:

```text
RISCV_TRAP_ENTER scause=0x... sepc=0x... stval=0x... sstatus=0x... spp=0 from_u=1 user_sp=0x...
RISCV_TRAP_SAVE_BEGIN tid=... scause=0x... sepc=0x... stval=0x...
RISCV_TRAP_SAVE_DONE tid=... scause=0x... sepc=0x... stval=0x...
RISCV_FIRST_USER_TRAP scause=0x... sepc=0x... stval=0x...
RISCV_FIRST_USER_SYSCALL nr=...
RISCV_SYSCALL_DECODE nr=... a0=0x... a1=0x... a2=0x... a3=0x... a4=0x... a5=0x...
RISCV_TRAP_HANDLE_BEGIN tid=... nr=...
RISCV_TRAP_HANDLE_DONE status=ok ret0=0x... ret1=0x... ret2=0x... err=0x...
RISCV_TRAP_RESTORE_BEGIN tid=...
RISCV_TRAP_RETURN_SRET tid=... pc=0x... sp=0x...
RISCV_LIVEEEEEEE
RISCV_SYSCALL_ROUNDTRIP_OK nr=...
RISCV_USER_RESUMED tid=... pc=0x...
```

Subsequent round-trips drop to one concise log line so the boot log
stays readable.

### 8.2 Fail-closed paths

- A trap taken from S-mode (`sstatus.SPP=1`) is a kernel fault and
  produces `RISCV_TRAP_UNHANDLED scause=... reason=trap_from_s_mode`
  followed by a halt — no silent retry.
- `RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task
  all_services_blocked` is the **correct terminal state** when all user
  tasks are blocked on IPC recv (event-driven idle with no timer /
  IRQ); reported deterministically rather than as a fatal trap.

---

## 9. Core service chain status

Reached deterministically on `-smp 1` **and** `-smp 2` (secondary still
parked under `-smp 2`):

```text
RISCV_LIVEEEEEEE
RISCV_SYSCALL_ROUNDTRIP_OK nr=15
RISCV_USER_RESUMED tid=2 pc=0x401d56
... supervisor IpcCall/Recv, PM/init scheduling ...
USER_LOG tid=10000 INITRAMFS_SRV_ENTRY ... RESIDENT_WAIT_BEGIN
USER_LOG tid=10001 DEVFS_SRV_ENTRY     ... RESIDENT_WAIT_BEGIN
USER_LOG tid=10002 VFS_SRV_ENTRY       ... VFS_MOUNT_TABLE_READY entries=2
USER_LOG tid=10006 RAMFS_MOUNT_READY prefix=/ram ...
USER_LOG tid=1     VFS_MOUNT_REGISTER_RAMFS_OK prefix=/ram
USER_LOG tid=10007 EXT4_SRV_READY
USER_LOG tid=1     VFS_MOUNT_REGISTER_EXT4_OK prefix=/ext4
RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked
```

Terminal state is **event-driven idle waiting for I/O / timer / IRQ**
(no scope yet), reported deterministically — reached **after** the full
server chain is resident (never before, which would be a stall).

### 9.1 XARCH-SRV-PARITY fix — blocked-waiter delivery on the direct trap path

The RISC-V trap bridge (`yarm_riscv64_trap_bridge` → `handle_trap_entry`)
runs the trap under a raw `&mut KernelState` and, unlike x86_64/aarch64,
does **not** go through `handle_trap_entry_shared`, so it has **no**
post-`with_cpu` `drain_dispatch_post_work` stage. The blocked-waiter
delivery producers (`produce_blocked_waiter_{plain,ordinary_cap,reply_cap}_delivery`)
only stash a *deferred* snapshot when `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu]`
signals that such a drainer will run. That flag is a cross-arch static that
RISC-V does not own; it was observed reading stale-true on RISC-V, so a
producer stashed a snapshot that **no drainer ever executed** — the woken
receiver (e.g. PM after `init`'s SpawnV5 `IPC_CALL`) was left un-enqueued.
The result: `init` blocked on its reply, PM was never re-dispatched to service
the spawn, and the boot stalled at `RISCV_KERNEL_IDLE_WAITING_FOR_IO
reason=no_runnable_task all_services_blocked` **before** any of
initramfs/devfs/vfs/driver_manager/blkcache/virtio_blk spawned.

Fix: `arch/riscv64/trap.rs::handle_trap_entry_with_fault_bookkeeping_mode`
forces `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu] = false` at the top of every
RISC-V trap (its true semantic value — RISC-V has no shared-path drainer), so
the producers take the **legacy inline wake** path, which clears the waiter
slot and wakes the receiver under the same single-dispatcher borrow. The
server chain then loads to parity with x86_64 (`PM_RECV_GOT_MSG opcode=11` ×N,
all `*_SRV_ENTRY`/`*_SRV_READY`, `CROSS_ARCH_LIVE_DONE arch=riscv64 result=ok`),
and `all_services_blocked` idle is reached only afterwards.

### 9.2 Stage 196A — shared trap wrapper + post-lock drain foundation

Stage 196A retires the §9.1 raw-trap workaround by giving RISC-V a **real**
shared trap path, contract-equivalent to x86_64/aarch64
`handle_trap_entry_shared`, while enabling **zero** RISC-V retirement classes.

**Old raw path (retired):** boot ran `Bootstrap::init_static()` and installed a
persistent raw `&'static mut KernelState` pointer
(`install_riscv_trap_kernel_state` / `trap_kernel_state_mut`); the bridge held
that `&mut` across the whole trap and called `handle_trap_entry` directly, with
no post-`with_cpu` drainer, so the active flag was force-cleared (§9.1).

**New shared path:** `run_with_prepared_kernel` now owns `KernelState` through a
boot-constructed `SharedKernel` (`Bootstrap::init_shared_static()` +
`borrow_kernel_for_boot()`, the same Stage-2N pattern x86_64/aarch64 use) and
installs a `SharedKernel` pointer (`install_riscv_trap_shared_kernel` /
`trap_shared_kernel_riscv`). The bridge (`yarm_riscv64_trap_bridge`) no longer
holds a persistent raw `&mut KernelState`: it borrows the `SharedKernel` and
routes through `handle_riscv_trap_entry_shared`, doing every kernel interaction
(current-tid reads, asid lookup) through bounded `with_cpu` callbacks. The
wrapper phases are:

1. **Pre-lock** — the split dispatcher declines every RISC-V syscall
   (`try_split_dispatch_into_frame` is never called;
   `RISCV_SPLIT_DISPATCH_DECLINED_ALL result=inert`). Zero retirement.
2. **Broad-lock (`with_cpu`)** — sets `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu] =
   true` (the RISC-V path now **owns** the flag), then runs the **unchanged**
   canonical `handle_trap_entry_with_fault_bookkeeping_mode`. No raw `&mut
   KernelState` escapes this callback; no nested broad lock.
3. **Post-lock** — after the guard drops, clears the flag and runs
   `drain_dispatch_post_work` (the real drainer that now completes any deferred
   blocked-waiter delivery a producer stashed — so the §9.1 force-false is
   **retired**, replaced by genuine flag ownership + a real drain).

**Active-flag ownership (Part 3).** The flag lifecycle is centralized in the
wrapper: set true immediately before the bounded phase, cleared on every
return, drained after release, never left true across `sret`/idle/fatal.
Structural markers (one-shot latched, first trap only, to avoid per-tick
floods): `RISCV_SHARED_TRAP_ENTRY_BEGIN`, `RISCV_GLOBAL_LOCK_DROP_ACTIVE_SET`,
`RISCV_GLOBAL_LOCK_PHASE_DONE`, `RISCV_GLOBAL_LOCK_DROP_ACTIVE_CLEAR`,
`RISCV_POST_LOCK_DRAIN_BEGIN`, `RISCV_POST_LOCK_DRAIN_DONE result=ok`,
`RISCV_SHARED_TRAP_ENTRY_DONE` (all `cpu=<cpu>`).

**Genuine post-lock drain proof (Part 4).** A default-off oracle
(`yarm.riscv64_post_lock_foundation_oracle=1`) publishes a one-shot post-work
token (the requester tid) during the broad-lock phase, then after the guard
drops consumes it and **re-acquires `with_cpu`** — a real re-acquisition that
would deadlock if the broad guard were still held — before the trap `sret`s
back to the same task. Markers: `RISCV_POST_LOCK_FOUNDATION_ORACLE_{PUBLISH_OK,
LOCK_DROPPED_OK, DRAIN_OK, USER_RETURN_OK, DONE result=ok}`. It mutates no
scheduler / capability / user-copy / task-switch state.

**SATP/sret restore readiness (Part 5).** RISC-V's
`post_switch_restore_arch_thread_state` is no longer a silent `Ok(())` no-op: it
delegates to the documented `restore_arch_thread_state_post_switch` foundation
(frame-side sepc/sstatus/GPR/TLS restore via `resume_current_thread_with_frame`).
It is still uncalled in production (no queue-advancing retirement class is
enabled), but a future switch drain would pair it with the incoming task's
SATP/ASID activation the bridge already performs (`map_kernel_shared_into_asid`
+ `write_satp` on the resumed asid, carrying `sfence.vma` ordering).

**Trap-stack sizing fix (latent bug exposed).** The S-mode trap runs the entire
syscall dispatch on a dedicated `RISCV_TRAP_STACK` (sp←sscratch at vector entry).
It was **16 KiB** — a pre-existing latent overflow: the deepest RISC-V dispatch
chain (IPC cap-transfer / SpawnV5 / fork, large `no_std` on-stack temporaries)
uses between 256 KiB and 1 MiB, so deep traps had been silently clobbering the
`.bss` below the stack, tolerated only because the corrupted bytes landed on
benign statics. Stage 196A's new default-off oracle flag landed in that blast
radius and surfaced as a non-deterministic false→true flip (gone at ≥1 MiB, not
at 256 KiB). The trap stack is now **2 MiB** (`.bss`, NOLOAD, in the gigapage) —
a real RISC-V correctness fix, not just an oracle workaround.

**All RISC-V retirement classes remain disabled (as of 196A).** Zero
`YARM_LOCK_SPLIT_DISPATCH arch=riscv64`, zero `GLOBAL_LOCK_RETIRE_CLASS_DONE
arch=riscv64`, zero `RISCV_FUTEX_WAIT_DISPATCH_*` / `RISCV_YIELD_DISPATCH_*`.
The recommended next class (Stage 196B) is **DebugLog only** — it is a
non-blocking, non-switching syscall that rides the pre-lock split path without
needing the (still-absent) post-`sret` context-switch + SATP/`sfence.vma`
restore proof.

### 9.3 Stage 196B — DebugLog (NR 15) split-dispatch retirement

Stage 196B enables **exactly one** RISC-V split-dispatch retirement class:
**DebugLog (NR 15)** — the first (and only) RISC-V class serviced off the global
lock. Nothing else is retired.

- **Selective gate.** The bridge already imports the syscall ABI into the
  portable frame (a7→nr, a0..a5→args). The shared wrapper
  (`handle_riscv_trap_entry_shared`, Phase 1) gates the split dispatcher behind
  an explicit `frame.syscall_num() == SYSCALL_DEBUG_LOG_NR` check, so the shared
  `try_split_dispatch_into_frame` (which also knows FutexWake / IpcRecv / VmBrk /
  InitramfsReadChunk / ControlPlaneSetCnodeSlots) can **never** service any other
  class on RISC-V. Every non-DebugLog syscall falls through to the unchanged
  broad-lock handler exactly once.
- **Pure read helper.** DebugLog reuses the generic `try_split_debug_log_into_frame`:
  resolve requester tid (bounded `current_tid_authoritative`), copy user bytes via
  the ASID-based split-read seam (`copy_from_user_asid_split_read`), log `USER_LOG`,
  write `set_ok(0,0,0)`. No broad `&mut KernelState`, no scheduler / capability /
  IPC / address-space-switch mutation, no post-lock deferred switch. A
  `UserMemoryFault`/copy-fail follows the canonical global handler (OK, no log —
  never masked as a different success).
- **Same-task sret parity.** A handled DebugLog returns EARLY from Phase 1 —
  before the active flag is set and before `with_cpu` — so no drain is owed and
  nothing is left true across the `sret`. The bridge's existing same-task ecall
  write-back finalizes it: `sepc = sepc+4` (the bridge's sole +4 pre-advance;
  the split path adds no second advance), `sstatus` untouched (preserved),
  a0/a1/a2 = the `set_ok` result lanes, and the same task resumes.
- **Markers.** `RISCV_SPLIT_ABI_IMPORT_OK nr=15`,
  `YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=15 cpu=0 result=ok`,
  `GLOBAL_LOCK_RETIRE_CLASS_BEGIN/DONE arch=riscv64 class=DebugLog result=ok`,
  `RISCV_SPLIT_FINALIZE_OK nr=15 result=ok` (kernel, one-shot latched), plus
  `RISCV_DEBUGLOG_SPLIT_USER_RETURN_OK` — emitted by **init userspace** right
  after `INIT_RUN_ENTER`, proving a subsequent userspace log runs after the split
  DebugLog returns.
- **Still excluded (as of 196B):** FutexWake, FutexWait, Yield, InitramfsReadChunk
  (NR 27, deprecated — NOT ported), D2 recv/send, IpcSend boundary,
  VM/spawn/fork/cap-mint, ReapFaultedTask. The shared-wrapper foundation markers
  and the default-off post-lock oracle are preserved. Recommended next class:
  **FutexWake** (Stage 196C) — waiter/run-queue mutation only, no caller task-switch.

### 9.4 Stage 196C — FutexWake (NR 10) split-dispatch retirement + live oracle

Stage 196C enables the **second** RISC-V split-dispatch class, **FutexWake (NR
10)** — the first RISC-V class that mutates kernel state off the global lock.

- **Selective gate.** The wrapper's Phase 1 gate is extended to
  `nr == SYSCALL_DEBUG_LOG_NR || nr == SYSCALL_FUTEX_WAKE_NR`; every other syscall
  still falls through to the broad-lock handler exactly once. FutexWait (NR 9)
  stays global-lock-only. Markers are nr-templated:
  `RISCV_SPLIT_ABI_IMPORT_OK nr={10|15}`,
  `YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr={10|15}`,
  `RISCV_SPLIT_FINALIZE_OK nr={10|15}`.
- **Lock/rank proof (reused 191B seam).** FutexWake reuses the accepted
  `try_split_futex_wake_into_frame` → `SharedKernel::futex_wake_split_mut`:
  **Phase A** scans matching TCBs under the rank-2 task seam, flips each
  `Blocked(Futex(addr)) → Runnable`, and records the woken tids in a fixed
  bounded buffer, then releases the task seam; **Phase B** enqueues each woken
  tid through the rank-1 scheduler seam. No broad `&mut KernelState`, no new
  lock, no nested task+scheduler lock. The waiter state changes once, the
  scheduler enqueue happens once, the **caller stays `current`** (FutexWake never
  context-switches the caller), and no SATP switch / post-lock switch drain is
  needed.
- **Same-task sret parity.** Like DebugLog, a handled FutexWake returns EARLY
  from Phase 1 (before the active flag / `with_cpu`): the helper writes the wake
  count into a0 via `set_ok(woke,0,0)`, and the bridge's same-task ecall
  write-back finalizes it — `sepc+4` once, `sstatus` preserved, no stale args over
  the result. Markers: `FUTEX_WAKE_SPLIT_BEGIN/DONE arch=riscv64 result=ok
  woke=<count>`, `GLOBAL_LOCK_RETIRE_CLASS_BEGIN/DONE arch=riscv64 class=FutexWake
  result=ok`.
- **Live oracle** (`yarm.riscv64_futex_wake_oracle=1`, default-off; init slot-5
  sentinel = 1). The child blocks through the LEGACY global-lock FutexWait (NR 9);
  the parent (init) uses the **authoritative handshake futex** (not a delay loop)
  to know the child is `Blocked(Futex)`, then wakes it through the SPLIT path
  (count must be 1), wakes again (count must be 0), and the child resumes exactly
  once. Markers: `RISCV_FUTEX_WAKE_LIVE_ORACLE_DONE result=ok first_wake=1
  second_wake=0 waiter_tid=<tid>` and the userspace return proof
  `RISCV_FUTEX_WAKE_USER_RETURN_OK first_wake=1 second_wake=0` (emitted by
  userspace after BOTH split wakes return).
- **Still excluded:** FutexWait, Yield, InitramfsReadChunk (NR 27), D2, IpcSend,
  VM/spawn/fork/cap-mint, ReapFaultedTask (0 `class=<other>` retirement markers,
  0 `RISCV_{FUTEX_WAIT,YIELD}_DISPATCH_*`). DebugLog stays live. 2 MiB trap-stack
  fix + measurement TODO preserved. Recommended next: **Stage 196D** — the RISC-V
  queue-advancing foundation (post-`sret` context-switch drain + SATP/`sfence.vma`
  restore proof) that FutexWait/Yield require.

### 9.5 Stage 196D — queue-advancing context-switch drain FOUNDATION

Stage 196D proves a **genuine RISC-V post-lock context switch** end-to-end —
outgoing userspace task A → in-lock publish → broad lock drops → post-lock drain
dequeues incoming task B → B's SATP/ASID activated with the required
`sfence.vma` → B's saved frame restored → `sret` enters B. It enables **ZERO new
syscall retirement classes**: it is a **separate default-off one-shot deferral**
that reuses Yield (NR 0) only as the trigger and never emits a Yield/FutexWait
retirement marker. FutexWait, Yield, InitramfsReadChunk (NR 27), D2, IpcSend,
VM/spawn/fork/cap-mint, and ReapFaultedTask all stay global-lock-only.

- **In-lock publish (default-off, one-shot, BSP + single-dispatcher + trap-path
  gated).** In `yield_current`, when `riscv_queue_switch_foundation_armed()` (knob
  on AND the one-shot has not fired) and eligible,
  `riscv_queue_switch_foundation_try_defer` claims the one-shot, the outgoing task
  is re-enqueued Runnable exactly once and `current` is cleared via the accepted
  `preempt_reenqueue_current_cpu` seam, the per-CPU deferral is recorded, and the
  in-lock dispatch is SKIPPED (`return Ok(())`). A re-enqueue failure clears the
  deferral and falls straight through to the unchanged legacy in-lock dispatch —
  never a fabricated success. Markers:
  `RISCV_QUEUE_SWITCH_FOUNDATION_{PUBLISH_BEGIN,REENQUEUE_OK}`. Normal Yields
  (knob off) never enter this block.
- **Handler-return bypass (requires a real deferral).** After the broad-lock
  `handle_trap_event_with_fault_bookkeeping_mode` returns, the canonical in-lock
  restore is skipped **only** when `riscv_queue_switch_foundation_is_deferred`
  holds (there is no `current` to restore — restoring would emit stale state or a
  spurious idle). It early-returns cleanly from the bounded `with_cpu` phase; this
  is gated on an ACTUAL pending per-CPU deferral (no generic "skip restore" flag)
  and is inert for every normal syscall. Marker:
  `RISCV_QUEUE_SWITCH_FOUNDATION_HANDLER_RETURN_OK`.
- **Post-lock switch drain (lock genuinely dropped).** After the broad `with_cpu`
  closure returns, the wrapper's drain re-acquires the scheduler seam through the
  SharedKernel via `yield_reverify_ready` — only possible because the broad guard
  was released (a still-held guard would deadlock) — proving `LOCK_DROPPED_OK`.
  It then dequeues B via the rank-1 `yield_dispatch_step_mut` seam (`DEQUEUE_OK`),
  sets B `current` (`CURRENT_SET_OK`), and marks B Running via the rank-2
  `d6_genuine_mark_running_via_task_seam` (`RUNNING_OK`).
- **Real SATP/`sfence.vma` + frame restore (fresh bounded re-acquire).** A brief
  `with_cpu(cpu, …)` re-acquire constructs B's `satp` via
  `riscv64::page_table::cr3_for_asid`, installs the shared kernel gigapage via
  `map_kernel_shared_into_asid`, and writes it via `write_satp` — which executes
  `csrw satp` THEN `sfence.vma x0, x0` (real hardware ops, not markers). NO x86
  CR3 / AArch64 TTBR0 logic. It then restores B's saved sepc/sstatus/GPR frame via
  the shared `restore_arch_thread_state` seam; the bridge propagates it and
  `sret`s into B. Markers: `RISCV_QUEUE_SWITCH_FOUNDATION_{DRAIN_BEGIN,
  LOCK_DROPPED_OK,DEQUEUE_OK,CURRENT_SET_OK,RUNNING_OK,SATP_OK,SFENCE_OK,FRAME_OK,
  SRET_ARMED,DRAIN_DONE}`.
- **No-incoming is an honest FAILURE.** If the drain finds no runnable B (or the
  reverify shows `current` was re-set by an in-lock fallback), it clears the
  deferral and emits `RISCV_QUEUE_SWITCH_FOUNDATION_FAIL reason={no_incoming|
  state_changed}` — it never fabricates an idle task or a `DONE`/success marker.
- **Live oracle** (`yarm.riscv64_queue_switch_foundation_oracle=1`, default-off;
  init slot-5 sentinel = 2, distinct from FutexWake's 1). Init (A) spawns child B
  (via `spawn_thread`, reusing the futex-oracle stack/TLS), then yields; B runs in
  userspace and emits `RISCV_QUEUE_SWITCH_FOUNDATION_INCOMING_USER_OK tid=<btid>`
  then parks (blocks on the park futex → the LEGACY global-lock FutexWait
  re-dispatches A). A resumes and emits
  `RISCV_QUEUE_SWITCH_FOUNDATION_ORACLE_DONE result=ok outgoing=<A> incoming=<B>
  outgoing_resumed=1` — the full round trip (A yields → B runs → A resumes).
- **Trap-stack impact.** The switch drain adds only a brief bounded `with_cpu`
  re-acquire on the existing trap stack (no recursion, no second frame allocation);
  measured boots stay well within the 2 MiB trap stack. The 2 MiB fix +
  measurement TODO are preserved.
- **Still excluded:** FutexWait, Yield, NR 27, D2, IpcSend, VM/spawn/fork/cap-mint,
  ReapFaultedTask (0 `class=<retirement>` markers, 0
  `RISCV_{FUTEX_WAIT,YIELD}_DISPATCH_*`). DebugLog + FutexWake stay live. Counts
  unchanged (SYSCALL_COUNT = 32, VARIANT_COUNT = 23), no new kernel lock, RISC-V AP
  user dispatch still gated. Recommended next: **Stage 196E** — RISC-V FutexWait
  retirement, which builds on this switch-drain foundation to context-switch the
  blocking caller.

### 9.6 Stage 196E — FutexWait (NR 9) queue-advancing RETIREMENT (live controlled oracle)

Stage 196E enables the **first genuine off-global-lock RISC-V syscall retirement
that context-switches the BLOCKING caller** — FutexWait (NR 9) — behind a
**default-off, ONE-SHOT controlled oracle**. It is a live retirement PROOF, not
the default-on production seal (that is Stage 196F). The no-incoming case stays on
the canonical global-lock path.

- **Reused generic machinery.** The in-lock publish, per-CPU deferral
  (`FUTEX_WAIT_DISPATCH_*`), reverify (`futex_wait_reverify_blocked`, un-gated to
  RISC-V), and dequeue (`futex_wait_dispatch_step_mut`, un-gated to RISC-V) are the
  SAME seams x86_64 (192A) and AArch64 (195E/195F) use. The RISC-V arch restore
  reuses the 196D switch machinery (`cr3_for_asid` + `map_kernel_shared_into_asid`
  + `write_satp` + `restore_arch_thread_state`). Nothing is duplicated.
- **Default-off, one-shot mechanism** (`yarm.riscv64_futex_wait_oracle=1`). The
  knob arms the mechanism; a compare_exchange CONSUMED latch guarantees exactly
  ONE eligible FutexWait is retired — every later FutexWait (including the child's
  park) stays on the unchanged legacy path. `armed` = knob-on AND not-consumed.
- **Eligibility (MANDATORY incoming-task-exists).** In `futex_wait_current`, after
  the caller is `Blocked(Futex)` + removed from `current`, the RISC-V block is
  eligible only when: armed, shared trap drain active, single dispatcher, BSP, no
  FutexWait deferral pending, no 196D foundation deferral pending, AND
  `runnable_count_on_cpu(cpu) > 0` (a runnable incoming task already exists). No
  incoming ⇒ emit `RISCV_FUTEX_WAIT_RETIRE_DEFERRED reason=no_incoming`, DO NOT
  consume the one-shot, DO NOT publish — use the canonical legacy dispatch. This is
  not a failure.
- **In-lock publish.** On eligibility: claim the one-shot, record the generic
  FutexWait deferral (`futex_wait_dispatch_try_defer`), and SKIP the in-lock
  dispatch (`return Ok(true)`). Markers
  `RISCV_FUTEX_WAIT_DISPATCH_{DEFER_BEGIN,BLOCK_PUBLISH_OK}`. A publish failure
  after the one-shot consume rolls back: clear the partial deferral, emit
  `..._FALLBACK reason=defer_failed`, fall through to legacy dispatch (the caller
  stays Blocked + not-current — never Blocked-and-current).
- **Handler-return bypass (narrow, independent).** The RISC-V handler bypass is
  extended: `post_lock_bypass = foundation_bypass || futex_wait_bypass`, where
  `futex_wait_bypass = futex_wait_dispatch_is_deferred(cpu)`. A real FutexWait
  deferral skips the canonical in-lock restore and returns cleanly; markers
  `RISCV_FUTEX_WAIT_HANDLER_BYPASS_{BEGIN,DONE}`. Requires an ACTUAL deferral (no
  generic flag); the 196D foundation bypass is unchanged and independent.
- **Post-lock drain.** After the broad guard drops: `futex_wait_reverify_blocked`
  re-acquires the rank-2 task seam through the SharedKernel (impossible under a held
  guard → `LOCK_DROPPED_OK`) AND confirms the waiter is STILL `Blocked(Futex)`
  (guards the FutexWake race); then dequeue B (rank-1), set B current, mark B
  Running (rank-2), and a fresh bounded `with_cpu` re-acquire does the REAL SATP
  write + `sfence.vma` + frame restore + `sret` into B. Markers
  `RISCV_FUTEX_WAIT_DISPATCH_{DRAIN_BEGIN,LOCK_DROPPED_OK,REVERIFY_OK,DEQUEUE_OK,
  CURRENT_SET_OK,RUNNING_OK,SATP_OK,SFENCE_OK,FRAME_OK,SRET_ARMED,DONE}` +
  `GLOBAL_LOCK_RETIRE_CLASS_{BEGIN,DONE} arch=riscv64 class=FutexWait result=ok`.
- **Honest race/failure.** A FutexWake that flips the waiter to Runnable before the
  drain ⇒ `RISCV_FUTEX_WAIT_DISPATCH_DEFERRED reason=state_changed` (clear + decline,
  no stale dispatch, no double-enqueue, no success). Unexpected no-incoming after
  publication ⇒ `RISCV_FUTEX_WAIT_DISPATCH_FAIL reason=no_incoming` (impossible under
  the controlled single-dispatcher gate; never a fabricated idle/success).
- **Live oracle** (slot-5 sentinel = 3). Init (A) spawns B (incoming exists), then
  enters FutexWait NR 9. A's FutexWait is retired → post-lock switch to B (real
  SATP/sfence/frame/sret). B emits `RISCV_FUTEX_WAIT_INCOMING_USER_OK tid=<B>`,
  wakes A through the already-retired split FutexWake NR 10 (count must be 1), then
  parks through the LEGACY path (one-shot consumed) which re-dispatches A. A resumes
  exactly once: `RISCV_FUTEX_WAIT_USER_RETURN_OK tid=<A> wake_count=1` +
  `RISCV_FUTEX_WAIT_LIVE_ORACLE_DONE result=ok blocked_tid=<A> dispatched_tid=<B>
  wake_count=1`.
- **Split gate unchanged.** NR 9 is NOT added to the pre-lock selective split gate
  (that stays NR 15 + NR 10); FutexWait's retirement is via the broad-lock handler +
  post-lock deferral, not `try_split_dispatch_into_frame`.
- **Trap-stack impact.** The FutexWait drain adds only the same brief bounded
  `with_cpu` re-acquire as the 196D switch drain (no recursion, no second frame);
  measured boots stay well within the 2 MiB trap stack. The 2 MiB fix +
  measurement TODO are preserved.
- **Still excluded / not yet claimed:** default-on FutexWait, post-lock idle /
  no-incoming handling (both Stage 196F); Yield, NR 27, D2, IpcSend,
  VM/spawn/fork/cap-mint, ReapFaultedTask, RISC-V AP user dispatch. DebugLog +
  FutexWake stay live; the 196D foundation oracle stays green. Counts unchanged
  (32/23), no new kernel lock.

### 9.7 Stage 196F — FutexWait DEFAULT-ON + post-lock IDLE seal

Stage 196F makes the eligible RISC-V FutexWait retirement **DEFAULT-ON** (no
oracle knob, no one-shot consume latch in the kernel) and adds a genuine
**no-incoming post-lock IDLE outcome**. The switch chain from 196E is byte-preserved.

- **Default-on eligibility.** The in-lock publish (`futex_wait_current`) is now
  gated purely structurally: shared trap drain active, single dispatcher, BSP, no
  FutexWait deferral pending, no 196D foundation deferral pending. The
  `runnable_count_on_cpu > 0` requirement, the `armed()` gate, and the
  `try_consume` one-shot latch are all REMOVED. First exercise emits the one-shot
  informational `RISCV_FUTEX_WAIT_RETIRE_DEFAULT_ON result=ok` (records that the
  PRODUCTION mechanism ran — not that a knob was enabled). Legacy in-lock dispatch
  remains only for genuinely ineligible traps.
- **Two post-lock outcomes.** The drain reverifies the caller is still
  `Blocked(Futex)` (rank-2 seam re-acquire → `LOCK_DROPPED_OK`), then dequeues:
  - **Switch** (incoming exists): the unchanged 196E chain — dequeue → current →
    Running → real `write_satp` + `sfence.vma` + `restore_arch_thread_state` →
    `sret` (`..._DONE result=ok`).
  - **Idle** (no incoming): `RISCV_FUTEX_WAIT_DISPATCH_NO_INCOMING` →
    `POST_LOCK_IDLE_BEGIN` → a fresh `with_cpu` re-acquire confirms `current` is
    None (`POST_LOCK_IDLE_LOCK_DROPPED_OK`) → clear deferral → `..._DONE result=idle`
    → `GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=FutexWait result=ok` →
    `POST_LOCK_IDLE_ENTERED`. NO frame is restored and NO `sret` is attempted: the
    drain returns `Err` with `current == None`, which hands off to the bridge's
    EXISTING proven idle policy (`RISCV_KERNEL_IDLE_WAITING_FOR_IO` + timer/PLIC
    idle-safe-point init + `riscv_trap_halt` wfi). No second idle implementation.
- **Idle interrupt/lock state.** The broad `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE` flag
  is cleared before the drain (never true across the idle handoff or wfi); the
  bridge's `riscv_trap_halt` performs the wfi loop, remaining interrupt-responsive
  so a later timer/external IRQ can dispatch a newly-runnable task. No stale
  `sepc`/`sstatus`/SP/GPR frame is returned.
- **Race preserved.** A FutexWake that flips the caller Runnable before the drain →
  `RISCV_FUTEX_WAIT_DISPATCH_DEFERRED reason=state_changed` (clear + decline; no
  stale dispatch, no double-enqueue, no waiter loss, no success). Genuine kernel
  errors still propagate as `Err` (not converted to idle success).
- **Workload oracles (both default-off).** `yarm.riscv64_futex_wait_oracle` (slot-5
  = 3) runs the two-task SWITCH workload — now under the default-on mechanism (it no
  longer arms retirement); `yarm.riscv64_futex_wait_idle_oracle` (slot-5 = 4) makes
  init (the last runnable task) block on a never-woken futex, driving the IDLE
  outcome and the kernel attestation `RISCV_FUTEX_WAIT_IDLE_ORACLE_DONE result=ok
  lock_dropped=1 current_none=1 outgoing_blocked=1`.
- **Normal core boot** may show ZERO FutexWait retirement markers if no production
  task naturally calls NR 9 — acceptable, since the mechanism is source- and
  test-proven default-on. NR 9 is still NOT in the pre-lock split gate.
- **Trap-stack impact.** The idle branch adds only one bounded `with_cpu` re-acquire
  (the lock-dropped proof) on the existing trap stack — no recursion, no second
  frame, no meaningful new usage. The 2 MiB fix + TODO are preserved.
- **Still excluded:** Yield, NR 27, D2, IpcSend, VM/spawn/fork/cap-mint,
  ReapFaultedTask, RISC-V AP user dispatch. DebugLog + FutexWake stay live; the 196D
  foundation oracle stays green. Counts unchanged (32/23), no new kernel lock.
  Recommended next: **Stage 196G** — RISC-V Yield retirement (re-enqueue + queue-
  advancing switch on the same drain foundation).

---

## 10. Current next target

RISC-V64 is now a regular smoke target (see §11). `--smp 1/2/3/4` are
all live-verified on QEMU `virt` + OpenSBI: nonzero boot harts are
selected and not parked, the binary-FDT `/cpus` walk yields
`present_cpus`/`present_bitmap` matching the platform, the service
chain reaches the idle terminal, and timer / PLIC / external IRQ are
each on an explicit deferred branch with a `reason=` tag.

The next pass is to enable the S-mode timer interrupt (`stimecmp` via
SBI Timer ext, `sstatus.SIE=1`, delegate `STI` in `mideleg`); the trap
vector already saves all GPRs and the bridge already routes
`TrapEvent::TimerInterrupt` through `handle_trap_entry`. After that,
flip the smoke gate from "live OR deferred" to "live required" for the
timer pair, then for PLIC + external IRQ, then unblock RISC-V SMP
scheduling so `online_cpus` can climb past 1.

---

## 10.1 Global-lock retirement portability (Stage 194 audit)

> **Superseded in part by Stage 196A (§9.2).** The Stage 196 step-1 prerequisite
> below is now DONE: the RISC-V trap bridge routes through the shared wrapper
> (`handle_riscv_trap_entry_shared`), the RISC-V path OWNS the active flag (set
> true in the bounded phase, cleared after) with a real `drain_dispatch_post_work`,
> and the §9.1 force-false is retired. Retirement classes are **still all
> disabled**; the "first live slice" recommendation (DebugLog first) stands. The
> Stage-194 text below is retained as the pre-196A audit record.

**RISC-V global-lock retirement paths are inert / global-lock-only today, and RISC-V is
the least ready of the three architectures. No class is retired off the global lock; the
active-flag is force-false; nothing is enabled by flipping a flag.**

- **RISC-V does NOT enter the shared, drain-capable path.** The trap bridge
  (`yarm_riscv64_trap_bridge` → `arch/riscv64/trap.rs::handle_trap_entry`) runs under a raw
  `&mut KernelState`, never through `handle_trap_entry_shared`. Consequently it has neither
  `try_split_dispatch_into_frame` (which needs `&SharedKernel`) nor the post-`with_cpu`
  `drain_dispatch_post_work`. This is the single largest cross-arch retirement gap.
- **Active-flag ownership.** Because `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu]` is a cross-arch
  static and the RISC-V path never sets it, it could read **stale-true** (left set by another
  CPU/earlier boot). The blocked-waiter producers would then stash a snapshot for a drainer
  that never runs → woken receivers left un-enqueued → boot stall at
  `kernel_idle_awaiting_io` (see §9.1). The fix (kept, guarded) is
  `handle_trap_entry_with_fault_bookkeeping_mode` force-storing the flag **false** at the top
  of every RISC-V trap — its true semantic value while RISC-V has no shared-path drainer.
- **Prerequisite before ANY retirement (Stage 196 step 1):** route the RISC-V trap bridge
  through a shared wrapper (`handle_trap_entry_shared` or an equivalent taking `&SharedKernel`
  and draining after the borrow drops), preserving the existing SATP/ASID `sret` restore and
  `SFENCE.VMA` discipline. Only after a real drain exists may the RISC-V path OWN the active
  flag (set true in the wrapper, cleared after) instead of forcing it false.
- **First live slice:** none until the shared-path prerequisite lands; then `DebugLog`
  (pure read, no switch, no drain), then `InitramfsReadChunk`, then `IpcSendPlainEnqueue`.
  Queue-advancing classes (`FutexWait`/`Yield`/`D2`) require a post-`sret` context-switch
  drain + SATP/`SFENCE.VMA` restore proof and are deferred.

See `doc/KERNEL_UNLOCKING.md` §7.1.21 for the Stage 196/197 plans and seal gates.

---

## 11. Smoke commands

RISC-V64 is a **regular** smoke target. The canonical entry points are
the build script and the per-`--smp N` core smoke script; both default
to `build-riscv64/yarm-riscv64.bin` and `build-riscv64/initramfs-core.cpio`.

```sh
# 1. Build artifacts (fails clearly if any required image is missing).
scripts/build-qemu-riscv64-artifacts.sh

# 2. Per-N core smoke. Each call enforces the full marker contract for N.
scripts/qemu-riscv64-core-smoke.sh --smp 1
scripts/qemu-riscv64-core-smoke.sh --smp 2
scripts/qemu-riscv64-core-smoke.sh --smp 3
scripts/qemu-riscv64-core-smoke.sh --smp 4

# 3. Or run the matrix wrapper, which builds once and summarizes 1..4.
scripts/qemu-riscv64-smoke-matrix.sh
```

The per-N gate enforces, for each `--smp N`:

| Axis | Requirement |
|------|-------------|
| Boot entry | `RISCV_BOOT_ENTRY hart=N dtb=0x...` from whichever hart OpenSBI released |
| Boot hart selection | `RISCV_BOOT_HART_SELECTED hart=N` + `RISCV_BOOT_HART_ID_STORED hart=N` |
| DTB `/cpus` scan | `RISCV_DTB_CPU_SCAN_DONE bitmap=0x... count=N` (silent fallback rejected) |
| Topology | `RISCV_HART_TOPOLOGY present_cpus=N` and `YARM_BOOT_OK present_cpus=N present_bitmap=0x{1,3,7,f} online_cpus=1` |
| Scheduler | `RISCV_SCHEDULER_BSP_ONLY online_cpus=1 reason=riscv_smp_scheduler_not_enabled` |
| Boot hart not parked | If `N>1`, `RISCV_SECONDARY_HART_PARK hart=B` must NOT carry the boot-hart id |
| Service chain | `RISCV_LIVEEEEEEE`, `RISCV_SYSCALL_ROUNDTRIP_OK`, `RISCV_USER_RESUMED`, `INITRAMFS_SRV_ENTRY`, `DEVFS_SRV_ENTRY`, `VFS_SRV_ENTRY`, `VFS_MOUNT_TABLE_READY`, `RAMFS_MOUNT_READY`, `VFS_MOUNT_REGISTER_RAMFS_OK`, `EXT4_SRV_READY`, `VFS_MOUNT_REGISTER_EXT4_OK` |
| Idle | `RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked` |
| Timer / PLIC / extirq | Either the live marker (`RISCV_TIMER_SMOKE_OK`, `RISCV_PLIC_INIT_DONE`, `RISCV_EXTIRQ_SMOKE_OK`) **or** the explicit deferred marker (`RISCV_TIMER_DEFERRED`, `RISCV_PLIC_DEFERRED`, `RISCV_EXTIRQ_DEFERRED`) with a `reason=` tag. This pass remains on the deferred branch by design. |

Reject patterns include `RISCV_EARLY_TRAP`, `PANIC`, `FATAL`, `ASSERT`,
`PAGE_FAULT_UNHANDLED`, `TRAP_HANDLE failed`, `Vm(Full)`, `oom`,
`capacity`, `RISCV_DTB_CPU_SCAN_FAILED`, `SPAWN_V5_WRONG_SENDER`, and
any `RISCV_TRAP_HALTED reason=` other than the expected
`kernel_idle_awaiting_io` terminal.

Once RISC-V is in the global kernel-unlocking smoke policy this is the
gate it must continue to satisfy. Live timer IRQ and PLIC external IRQ
remain explicitly deferred until their bring-up lands; tightening the
gate to live-only is the next pass, not this one.

---

## 12. Authoring rule

Future RISC-V64 docs update **this file**. Cross-arch / generic boot
docs update `doc/BOOT.md`.

---

## 13. RISC-V64 port status and TODO (end of stabilization pass 2)

### 13.1 Current accepted status

| Area | Status |
|------|--------|
| OpenSBI handoff | ✅ fixed; `a0`=hartid + `a1`=DTB preserved; `mv a0, s1` correct |
| Boot hart selection | ✅ whichever hart OpenSBI releases to `_start` is the boot hart; no hart-0 assumption, no CAS, no Zaamo dependency |
| Present-hart topology | ✅ binary-FDT `/cpus` walk (`arch::fdt::cpus_hart_id_bitmap`); `RISCV_DTB_CPU_SCAN_DONE bitmap=... count=N` is required, silent fallback rejected |
| `--smp 1/2/3/4` smoke | ✅ live-verified — boot hart never parked; per-N bitmap matches the platform; topology summary `YARM_BOOT_OK present_cpus=N present_bitmap=0x{1,3,7,f} online_cpus=1` |
| `online_cpus=1` (scheduler) | ✅ `RISCV_SCHEDULER_BSP_ONLY online_cpus=1 reason=riscv_smp_scheduler_not_enabled` |
| Secondary harts | ✅ parked via SBI HSM; `RISCV_SECONDARY_HART_PARK hart=N` |
| Real U-mode `sret` | ✅ `RISCV_ENTER_USER_SRET tid=2`; first trap `from_u=1 spp=0` |
| Syscall round-trip | ✅ full `RiscvTrapFrame` save/restore; `+4` ecall PC advance; fail-closed S-mode-fault halt |
| Core service chain | ✅ reaches `RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked`; required markers include `RAMFS_MOUNT_READY`, `VFS_MOUNT_REGISTER_RAMFS_OK`, `EXT4_SRV_READY`, `VFS_MOUNT_REGISTER_EXT4_OK` |
| Timer | ⏸ deferred — audit-stage scaffold landed (`RISCV_TIMER_AUDIT_BEGIN`/`AUDIT_DONE`), defers with canonical `reason=timer_irq_feature_disabled` in default builds; feature-on path defers with `reason=trap_bridge_reentrancy_not_ready` because the trap vector has no kernel-S-mode timer fast path yet |
| PLIC | ⏸ discovery + threshold-address compute live; threshold write skipped under active satp (`RISCV_PLIC_DEFERRED reason=plic_mmio_unmapped_under_active_satp`); base / context / per-source breadcrumbs emit |
| External IRQ | ⏸ `RISCV_EXTIRQ_DEFERRED reason=no_safe_source`; UART0 (sid=10) marked as the candidate via `RISCV_EXTIRQ_SELECT`; no source enabled |
| RISC-V SMP scheduling | ⏸ not implemented; `online_cpus` stays at 1 by design |

### 13.2 Current smoke contract

```sh
scripts/build-qemu-riscv64-artifacts.sh                   # fails clearly if any artifact missing; reports sizes
scripts/qemu-riscv64-core-smoke.sh --smp 1 [--timeout N]  # canonical per-N gate
scripts/qemu-riscv64-core-smoke.sh --smp 2
scripts/qemu-riscv64-core-smoke.sh --smp 3
scripts/qemu-riscv64-core-smoke.sh --smp 4
scripts/qemu-riscv64-smoke-matrix.sh                       # builds once, runs N=1..4, prints summary table
```

Required success markers (see §11 for the full table):

- `YARM_BOOT_OK present_cpus=N present_bitmap=0x{1,3,7,f} online_cpus=1`
- `RISCV_BOOT_ENTRY hart=…`, `RISCV_BOOT_HART_SELECTED hart=…`, `RISCV_BOOT_HART_ID_STORED hart=…`
- `RISCV_DTB_CPU_SCAN_DONE bitmap=… count=N`
- `RISCV_HART_TOPOLOGY present_cpus=…`
- `RISCV_SCHEDULER_BSP_ONLY online_cpus=1 reason=riscv_smp_scheduler_not_enabled`
- `RISCV_LIVEEEEEEE`, `RISCV_SYSCALL_ROUNDTRIP_OK`, `RISCV_USER_RESUMED`
- `INITRAMFS_SRV_ENTRY`, `DEVFS_SRV_ENTRY`, `VFS_SRV_ENTRY`, `VFS_MOUNT_TABLE_READY`
- `RAMFS_MOUNT_READY`, `VFS_MOUNT_REGISTER_RAMFS_OK`, `EXT4_SRV_READY`, `VFS_MOUNT_REGISTER_EXT4_OK`
- `RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked`
- `RISCV_TIMER_AUDIT_BEGIN`, `RISCV_TIMER_AUDIT_DONE sbi_time=… boot_hart=… trap_bridge_reentrant=… feature=…`
- `RISCV_TIMER_INIT_BEGIN`, `RISCV_TIMER_MECHANISM value=…`
- `RISCV_PLIC_BASE value=…`, `RISCV_PLIC_CONTEXT value=…`

Rejected failure markers: `RISCV_EARLY_TRAP`, `PANIC`, `FATAL`,
`ASSERT`, `PAGE_FAULT_UNHANDLED`, `TRAP_HANDLE failed`, `Vm(Full)`,
`oom`, `capacity`, `RISCV_DTB_CPU_SCAN_FAILED`, `SPAWN_V5_WRONG_SENDER`,
`present_cpus=1` under `--smp >1`, boot hart appearing in any
`RISCV_SECONDARY_HART_PARK` line, repeated `source=missing_dtb`, any
`RISCV_TRAP_HALTED reason=` other than `kernel_idle_awaiting_io`, any
`RISCV_TIMER_DEFERRED reason=…` that is not one of the canonical list
below.

Accepted timer-deferred reasons (canonical, kernel + gate must agree):

- `timer_irq_feature_disabled` — default build, cargo feature off (current).
- `trap_bridge_reentrancy_not_ready` — feature on, but trap vector's
  kernel-S-mode timer fast path not yet landed; arming STIE would
  trigger `RISCV_TRAP_UNHANDLED reason=trap_from_s_mode` on the very
  next `wfi`.
- `sbi_time_ext_unavailable` — SBI Timer EID probe returned not-supported.
- `stie_audit_pending` — re-entry case; `init_timer_after_idle_safe_point` already fired.
- `not_boot_hart` — guard against a future caller from a secondary hart.
- `unsafe_under_current_satp` — reserved for a future caller path that
  runs before the kernel-shared gigapage is installed.

Accepted PLIC-deferred reason: `plic_mmio_unmapped_under_active_satp`.

Accepted external-IRQ-deferred reason: `no_safe_source`.

### 13.3 Current limitations

- No RISC-V SMP scheduling. `online_cpus` is permanently 1 in default
  builds; secondaries `wfi` inside `yarm_riscv64_secondary_boot` after
  SBI HSM `hart_start`, and never reach userspace.
- No secondary-hart userspace. The per-hart park path installs a local
  trap vector with interrupts masked; there is no per-CPU state and no
  per-CPU runqueue.
- No broad PLIC source enable. The discovery breadcrumbs enumerate the
  QEMU virt source IDs (virtio-mmio 1..=8, UART0 10) but no source's
  enable register is written.
- No full virtio IRQ routing. Without external-IRQ enable, devices are
  driven exclusively through MMIO polling done by their user-mode
  drivers; that is the steady state today.
- PLIC MMIO is not mapped under the active satp. By the time the
  idle-path init runs, the active page table maps only the
  kernel-shared RAM gigapage; the PLIC's physical MMIO window sits
  below RAM and is never covered, so the threshold write is skipped and
  reported as `plic_mmio_unmapped_under_active_satp` instead of
  faulting. A future change can add a kernel-only / device / NX / not-user
  PLE mapping in every active root once the rest of the IRQ path is in
  place, but doing it without the rest of the path provides no value.
- Timer trap-bridge re-entrancy is unaudited. The current trap bridge
  treats any kernel-S-mode trap as `RISCV_TRAP_UNHANDLED
  reason=trap_from_s_mode` and halts. Enabling STIE before adding a
  kernel-S-mode timer fast path (record tick, disable STIE, sret back
  to `wfi`) would crash the boot, so STIE remains off and the deferral
  reason is the canonical `trap_bridge_reentrancy_not_ready`.

### 13.4 TODO list

**Before global kernel unlocking resumes (RISC-V must satisfy):**

- ✅ RISC-V is accepted as a regular smoke target.
- ✅ `--smp 1/2/3/4` topology smoke passes.
- ✅ Service chain reaches `RISCV_KERNEL_IDLE_WAITING_FOR_IO`.
- ✅ Timer is either live with `RISCV_TIMER_SMOKE_OK ticks=…` **or**
  explicitly deferred with a canonical reason. Default builds defer
  with `timer_irq_feature_disabled`; the gate accepts this.

**After global kernel unlocking resumes (RISC-V follow-up work):**

- ⏳ Trap-vector kernel-S-mode timer fast path: detect `scause` =
  interrupt|`IRQ_SUPERVISOR_TIMER` with `spp=1`, call
  `record_timer_tick`, clear `sie.STIE`, emit
  `RISCV_TIMER_DISABLED_AFTER_ONE_SHOT` + `RISCV_TIMER_SMOKE_OK
  ticks=1`, sret back to the interrupted `wfi`. Once this lands, flip
  `STIE_AUDIT_COMPLETE = true` and enable the `riscv64-timer-irq`
  cargo feature in the smoke gate.
- ⏳ PLIC kernel-only MMIO mapping (device / NX / not-user) installed
  into every active root, so the threshold write can run without
  faulting.
- ⏳ One-source external IRQ proof: UART0 (sid=10) is the marked
  candidate; PLIC claim/complete + a real handler + device interrupt
  ack must all land together. No broad source enable.
- ⏳ virtio IRQ routing once the one-source proof works.
- ⏳ RISC-V SMP scheduler participation, then secondary-hart
  per-CPU state (runqueue lock sharding, percpu records, GS-equivalent
  per-hart pointer via `tp`).
- ⏳ Optional FS strict smoke parity with x86_64 / AArch64 once the
  regular core smoke is in the global gate.

### 13.5 RISC-V64 readiness for global kernel unlocking

**Ready: yes.**

The RISC-V64 port satisfies every "before global unlocking" gate above:
`scripts/qemu-riscv64-smoke-matrix.sh` passes live across `--smp 1/2/3/4`
on QEMU `virt` + OpenSBI; the service chain reaches the idle terminal;
the timer is explicitly deferred with the canonical reason
`timer_irq_feature_disabled`, accepted by the gate. The remaining
items (live timer tick, PLIC mapping, one-source external IRQ, SMP
scheduling) are post-unlocking follow-ups, not unlocking blockers, and
each has a canonical deferred-reason marker today so its absence is
visible at every boot.

The next pass therefore should resume the global kernel unlocking work
with RISC-V included in the smoke matrix, treating its regular core
smoke as the per-arch acceptance gate the same way the x86_64 and
AArch64 core smokes are treated.
