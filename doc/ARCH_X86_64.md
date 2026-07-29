<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM x86_64

> **Ownership rule.** All x86_64-specific boot, trap, syscall, AP/SMP, and
> userspace status documentation lives here. Generic boot flow lives in
> `doc/BOOT.md`. New x86_64 fragment files are forbidden; update this doc
> instead. See `doc/DOCUMENTATION_MAP.md`.

x86_64 is the primary YARM development target. Boot is Xen PVH; QEMU
`q35` is the standard runner. The `-smp 1` core smoke is the accepted
baseline; `-smp 2` is observable-only (APs Rust-online but not scheduler
participants — see §3).

---

## 1. PVH boot path

YARM enters via the **Xen PVH** entry contract:

- The bootloader / QEMU PVH path provides a `PvhStartInfo` pointer in
  the conventional register.
- The entry preserves the `start_info` pointer and passes it to
  `yarm_kernel_main`.

PVH module parsing interprets module entries as **(start, size)**; the
end is computed as `start + size`. PVH `modlist_paddr` and module-payload
addresses are physical; they are accessed through the bootstrap
higher-half alias (`KERNEL_BOOTSTRAP_VIRT_BASE + phys`).

### 1.1 Cmdline capture

`capture_pvh_command_line`:

1. validates the PVH magic,
2. rejects zero or a range outside `KERNEL_PHYS_DIRECT_MAP_BYTES`,
3. translates the physical address with `KERNEL_BOOTSTRAP_VIRT_BASE`
   while the bootstrap direct map is active,
4. reads exactly 2049 bytes (the extra byte distinguishes a 2048-byte
   value from an overlong source), and
5. copies through NUL into kernel-owned fixed storage.

PVH boot data is trusted bootloader input, but the direct-map range
check avoids constructing a slice outside the mapped bootstrap physical
window. The copy is performed during `prepare_arch_boot`, before
ordinary memory allocation can reuse boot data. The separate
`PvhModule.cmdline_paddr` logging remains module metadata and is **not**
used as the kernel command line.

### 1.2 Initrd handoff

Initrd bytes come from the PVH module list (`start_info` module window).
The x86_64 PVH handoff path explicitly reserves the page-aligned initrd
window through `Bootstrap::install_boot_reserved_range(...)` **before**
`install_boot_initrd_bytes(...)`. See `doc/BOOT.md` §3 for the cross-arch
invariant; failing to reserve before allocator init lets allocator reuse
overwrite the initrd bytes.

---

## 2. First-user ABI

x86_64 ring-3 startup ABI lanes:

| Register | Lane |
|----------|------|
| `rdi` | arg0 |
| `rsi` | arg1 |
| `rdx` | arg2 |
| `rcx` | mapped startup-args block VA |
| `r8`  | startup-args count |
| `r9`  | reserved |

The startup-args block is copied into user-mapped memory before ring-3
entry.

First-user image selection prefers `/init` from the initramfs CPIO;
synthetic ELF is fallback-only.

---

## 3. SMP — `-smp 1` accepted baseline + AP Rust-entry status (outcome A)

### 3.1 `-smp 1` is the accepted baseline

Core smoke is pinned `QEMU_SMP=1`. `-smp 1` runs the production scheduler
on the BSP only and exercises every YARM live path (D1/D2/D5/D3.1/D6.1
splits; see `doc/KERNEL_UNLOCKING.md`).

### 3.2 AP Rust online (Milestone 2 Pass 2 / Stage 109)

`yarm.x86_ap_rust=1` (boot cmdline) enables a live AP path: the AP leaves
the trampoline, enters the higher-half Rust AP entry function, publishes
its online status to the BSP, and parks in a Rust-controlled `cli;hlt`
loop. **Production scheduler participation remains BSP-only.**

What ships (outcome A):

- Trampoline tail (`arch/x86_64/smp_trampoline.rs`) publishes
  `ready_word = 2` ("Rust online") from low-RIP asm immediately before
  `movabs rax, OFFSET yarm_x86_64_ap_entry; jmp rax`.
- `yarm_x86_64_ap_entry` emits a `@` COM1 breadcrumb (Rust-entered
  proof) and parks forever in `cli;hlt;jmp 2b`. Body is 100% inline asm
  so the compiler cannot insert SSE-typed prologue/epilogue that the
  AP's CR4 (only PAE set) couldn't dispatch.
- Online publication is from **low-RIP asm**. A prior attempt that
  published online (`[rdi+32]=2`) from Rust reached `@` but never
  completed the store — likely a compiler-emitted Rust prolog faulting
  before the inline-asm store. Publishing from low-RIP uses the same
  write site already proven for the `=1` store.
- The BSP polling site emits the full marker sequence per AP:
  `X86_AP_INIT_SENT`, `X86_AP_STARTUP_SENT`,
  `X86_AP_TRAMPOLINE_REACHED`, `X86_AP_ENTER_RUST`, the per-CPU record +
  GS scaffold (`X86_AP_PERCPU_BEGIN` .. `X86_AP_GS_INIT_BY_AP
  reason=wrmsr_in_ap_entry_graded_by_admit_poll` .. `X86_AP_PERCPU_READY`),
  the env scaffold (`X86_AP_ENV_BEGIN` .. `X86_AP_ENV_READY`),
  `X86_AP_GDT_TSS_READY`, `X86_AP_IDT_READY`,
  `X86_AP_CPU_LOCAL_READY`, `X86_AP_ONLINE`, then the Stage 183 admit
  poll (`X86_AP_SCHED_ADMIT_BEGIN`, `X86_AP_GS_OK`,
  `X86_AP_KERNEL_CR3_OK`, `X86_AP_GDT_LOCAL_OK`, `X86_AP_TSS_OK`,
  `X86_AP_LAPIC_OK`, `X86_AP_LAPIC_TIMER_DEFERRED`,
  `X86_AP_IDLE_TASK_READY`, `X86_AP_IDLE_CONTEXT_OK`,
  `X86_AP_SCHED_PREREQ_OK`, `X86_AP_IDLE_ENTER`,
  `X86_AP_SCHED_ADMIT_DONE`), then once `X86_SMP_STARTUP
  started_secondary=N online_cpus=1 present_cpus=M` and
  `X86_SMP_OBSERVATION_OK rust_aps=N scheduler_aps=0`.
  Note: since Stage 183 inc.2/3 the AP itself performs the
  `WRMSR IA32_GS_BASE` + rdmsr readback (graded `X86_AP_GS_OK`/`_BAD`),
  the kernel-CR3 reload, per-AP lgdt/ltr, and the LAPIC ID readback; the
  BSP grades each from AP-written results — no fake "READY" markers.
- The `yarm.x86_ap_rust=` knob (`kernel/boot_command_line.rs`) flips
  `arch::x86_64::smp::set_ap_rust_entry_enabled`; the knob emits
  `YARM_X86_AP_RUST_SET enabled=true|false`. `1`, `true`, `yes`, `on` →
  `Some(true)`; `0`, `false`, `no`, `off` → `Some(false)`.

### 3.3 Safety fences (must not be violated by any AP change)

- **APs do NOT enter userspace.** The Rust AP entry is `extern "C" fn
  ... -> !` whose only operations are `cli`, one COM1 byte, and the
  `cli;hlt;jmp` park loop. No syscall-return path, no scheduler
  dispatch.
- **APs do NOT participate in production scheduling.**
  `start_secondary_cpus` intentionally does NOT invoke the scheduler
  bring-up entry point for APs. `online_cpu_count()` stays at 1 (BSP).
  Rust-online count is reported separately as `started_secondary` in
  `X86_SMP_STARTUP`.
- **APs do NOT take timer interrupts.** No AP IDT installed; `cli` stays
  set across the entire Rust park loop.
- **APs do NOT participate in cross-CPU wake / runqueue sharding.**

### 3.4 Acceptance evidence (Stage 109)

| Smoke | Result | Notes |
|-------|--------|-------|
| x86_64 `-smp 1` core | PASS | all 6 service entries present exactly once |
| x86_64 `-smp 1` optional-FS strict | PASS | `INIT_FAT_SPAWN_SKIPPED=1` |
| AArch64 core | PASS | boot markers detected, no boot blockers |
| AArch64 optional-FS strict | PASS | `INIT_FAT_SPAWN_SKIPPED=1` |
| x86_64 `-smp 2` + `yarm.x86_ap_rust=1` | **PASS (AP Rust online)** | `X86_SMP_STARTUP started_secondary=1 online_cpus=1 present_cpus=2`; COM1 breadcrumbs `sSR2@` prove asm published online (2) and AP entered Rust (@) |

---

## 4. Current next target — AP per-CPU environment

Before APs can participate in production scheduling, the following must
land (in order):

1. **Per-CPU GDT/IDT/TSS + GS base + AP-safe printk**, behind a
   default-off knob.
2. **`bring_up_cpu(cpu)`** integration so APs join the production
   scheduler.
3. **Lock-free `await_tlb_shootdown_ack`** for multi-CPU D3.
4. **Per-CPU runqueue lock sharding (D6)** once `-smp ≥ 2`
   scheduler-online smoke exists.
5. **D4 continuation:** `syscall/recv_shared_v3.rs`, then
   `syscall/process.rs`.

Until items 1–2 land, `-smp 1` remains the accepted baseline and the
core smoke stays pinned `QEMU_SMP=1`. No fake SMP acceptance.

---

## 5. BT2 — LAPIC timer arming discipline

The BSP LAPIC timer is armed **exactly once** via
`start_bsp_periodic_timer(kernel)` in `run_scheduler_loop()`, **after**
`signal_bootstrap_scheduler_ready()`. The early arming in
`init_lapic_mmio_base()` was removed. Do **not** re-introduce early
timer arming — see `doc/KERNEL_UNLOCKING.md` §4.

---

## 6. Pointers to current smoke commands

```sh
scripts/build-qemu-x86_64-artifacts.sh
scripts/qemu-x86_64-core-smoke.sh
QEMU_SMOKE_STRICT=1 scripts/qemu-x86_64-core-smoke.sh
QEMU_SMOKE_STRICT=1 scripts/qemu-x86_64-optional-fs-smoke.sh

# Override artifact paths
KERNEL_IMAGE=build-x86_64/kernel_boot.elf \
  INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio \
  scripts/qemu-x86_64-core-smoke.sh

# AP Rust-online observation
QEMU_SMP=2 ... -append "console=ttyS0 yarm.x86_ap_rust=1"
```

See `doc/BOOT.md` §4.1 for the full marker contract and
`doc/KERNEL_UNLOCKING.md` for the optional-FS marker invariants.

---

## 6.1 Return contract, scheduling and live proof (kernel unlocking)

Census and cross-architecture matrix: `doc/KERNEL_UNLOCK_AUDIT.md` §2. Roadmap:
`doc/KERNEL_UNLOCKING.md` §0.

### Off-lock position

x86_64 is the **only** architecture with off-lock authoritative dispatch.
`d6_genuine_enabled()` (`src/kernel/boot/mod.rs:766`) is
`cfg!(target_arch = "x86_64") && !d6_controlled_switch_proof_enabled() && !d6_switch_a_enabled()`
— compile-time true in production, with **no opt-out back to the old global-lock path**.

Ungated off-lock syscall classes: **5** — NR 15 `DebugLog`, NR 10 `FutexWake`,
NR 8 `ControlPlaneSetCnodeSlots`, NR 2 `IpcRecv` (kernel-task queued-plain only),
NR 14 `VmBrk` (page-crossing shrink only). NR 6 / NR 7 direct IPC are implemented and
live-proven but **proof-gated, default-OFF**.

### Post-lock drain chain

After `with_cpu` returns in `handle_trap_entry_shared`:
`drain_dispatch_post_work` → D2-send → D2-recv → FutexWait (192A) → Yield (192B) →
D6-genuine mutating dispatch → Stage 117 switch-plan stash →
`drain_reply_timeout_post_work` → `drain_server_death_post_work`.

Each drain re-verifies its subject before acting and falls back to the broad path with
`reason=state_changed` if the state moved while the guard was down.

### Restore-owner contract — the x86_64-specific hazard

x86_64 resolves the restore owner **in-lock** (Stage 200D-0B3), which is correct for
identity coherence but happens strictly **before** the post-lock drains — and the drains
are exactly where wakes are published. AArch64 and RISC-V consume the disposition
**after** the drains, so they never had this gap.

The live consequence (fourth ServerDies attempt): the server-death drain made a caller
runnable and enqueued it, the epilogue committed the earlier `owner=idle` decision
anyway, and the CPU halted holding an idle frame while a runnable task existed —
re-idling on every timer tick, 220 times, until the boot timed out.

**Invariant:** *a restore-owner decision taken before the post-lock drains must be
re-validated after them.* Any drain that makes a task runnable hits this; the
reply-timeout collector and the server-death drain both can.

The repair is `SharedKernel::revalidate_idle_owner_after_drains` (`src/runtime.rs:665`),
wired in the trap epilogue at `src/arch/x86_64/descriptor_tables.rs:1324`. It runs with
the broad guard dropped and every drain complete, but before any frame is committed. It
uses the existing `dispatch_next_on_cpu` (exactly one queue advance, through the same
authority), is CPU-local, and is gated on the prepared owner being idle so a prepared
replacement is never displaced.

Because `dispatch_next_on_cpu` **commits** its selection as the CPU's `current` before
the arch restore is attempted, the outcome is typed rather than `Option<u64>`:

```rust
pub(crate) enum OwnerRevalidation {
    Idle,                                          // nothing committed
    Replacement(u64),                              // committed AND restored
    RestoreFailed { tid: u64, rolled_back: bool }, // committed, NOT restored
}
pub(crate) enum OwnerCommit { Idle, Replacement(u64), FailClosed(u64) }
```

`OwnerRevalidation::disposition()` is a pure function, so the fail-closed rule is one
testable rule rather than the incidental shape of a `match`. Only
`RestoreFailed { rolled_back: false }` maps to `FailClosed`, which takes the **existing**
fatal path (`fatal_trap_read_snapshot` → `log_decoded_fatal_trap_from_snapshot` →
`debug_uart_trap_breadcrumb` → `halt_forever`), not a second policy; `halt_forever`
diverges, so that arm cannot fall through to the frame commit.

Rollback undoes the seam's own advance, CPU-locally: `block_current_on_cpu` clears
`current` (and scheduler membership, so the re-enqueue cannot hit `AlreadyQueued`), and
the task is re-enqueued **only if it is still live** — a task whose TCB is gone is not
resurrected into a run queue, but `current` is still cleared, which is what the idle path
depends on. `rolled_back` requires **both** halves.

A silent-success hole was closed along the way: `restore_arch_thread_state` maps
`KernelError::TaskMissing` to `Ok(())` so early boot restores nothing and still returns
cleanly. That is correct for its other callers and **wrong** at this call site — a task
still in a run queue whose TCB has been reaped would be reported as a successful
replacement.

> ⚠️ **`revalidate_idle_owner_after_drains` has never run in QEMU.** It is
> hosted-proven only (9 + 11 cases, 7 + 10 mutation guards killed). The `FailClosed` arm
> is a currently-unreachable backstop: with today's code `block_current_on_cpu` always
> returns the task just dispatched and the only restore failure is the unrestorable one,
> which is not requeued, so `rolled_back` is always `true`.

### Live-proof status

* `ExitCurrentTask` NR 16 — live cell sealed at `0b5e98f`
  (`STAGE_200D0B3_X86_EXIT_CURRENT_TASK_REFREEZE_SEAL`).
* Reply timeout — `scan_broad_lock=0`, `completion_transaction_narrow=1`; two matrix cells.
* Direct IPC NR 6 / NR 7 at SMP=2 — full cross-CPU request **and** reply proven live,
  frozen by `STAGE_199_X86_DIRECT_IPC_FINAL_SEAL … result=ok`; all knob-gated.
* AP scheduling — proof-gated live (generic fresh entry, saved-frame resume, recv-v2
  block, cross-CPU IPI both directions). The **production** scheduler remains BSP-only.
* **ServerDies — no live cell.** Blocked on the link-accounting defect (canonical Stage
  202D); see `doc/IPC.md` §8.5.

---

## 7. Authoring rule

Future x86_64 docs update **this file**. Cross-arch / generic boot docs
update `doc/BOOT.md`.
