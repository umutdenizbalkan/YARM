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
  `X86_AP_TRAMPOLINE_REACHED`, `X86_AP_ENTER_RUST`,
  `X86_AP_GDT_TSS_READY`, `X86_AP_IDT_READY`, `X86_AP_GS_READY`,
  `X86_AP_CPU_LOCAL_READY`, `X86_AP_ONLINE`, `X86_AP_RUST_PARK`, then
  once `X86_SMP_STARTUP started_secondary=N online_cpus=1
  present_cpus=M` and `X86_SMP_OBSERVATION_OK rust_aps=N
  scheduler_aps=0`.
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

## 7. Authoring rule

Future x86_64 docs update **this file**. Cross-arch / generic boot docs
update `doc/BOOT.md`.
