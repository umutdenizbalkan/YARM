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
(no scope yet), reported deterministically.

---

## 10. Current next target

Enable the S-mode timer interrupt (`stimecmp` via SBI Timer ext,
`sstatus.SIE=1`, delegate `STI` in `mideleg`). The trap vector already
saves all GPRs and the bridge already routes
`TrapEvent::TimerInterrupt` through `handle_trap_entry`, so the
scheduler will start ticking and the parked services will be woken on
external IRQs (PLIC). After that, point the official
`scripts/qemu-riscv64-core-smoke.sh` at the production binary path and
confirm it matches the x86_64 / AArch64 marker set.

---

## 11. Smoke commands

```sh
scripts/build-qemu-riscv64-artifacts.sh

# -smp 1
qemu-system-riscv64 -machine virt -m 512M -smp 1 \
  -nographic -monitor none -serial stdio -bios default \
  -kernel build-riscv64/yarm-riscv64.bin \
  -initrd build-riscv64/initramfs-core.cpio \
  -append "console=ttyS0 rdinit=/init"

# -smp 2 (secondary must park)
qemu-system-riscv64 -machine virt -m 512M -smp 2 \
  -nographic -monitor none -serial stdio -bios default \
  -kernel build-riscv64/yarm-riscv64.bin \
  -initrd build-riscv64/initramfs-core.cpio \
  -append "console=ttyS0 rdinit=/init"
```

The official `scripts/qemu-riscv64-core-smoke.sh` exists for
scaffolding; it currently uses a different image base
(`build/yarm-riscv64.bin`). Use the direct commands above (or
`KERNEL_IMAGE=build-riscv64/yarm-riscv64.bin INITRAMFS_IMAGE=...`
overrides) until the script is repointed at the production artifacts.

---

## 12. Authoring rule

Future RISC-V64 docs update **this file**. Cross-arch / generic boot
docs update `doc/BOOT.md`.
