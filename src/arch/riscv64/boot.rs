// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
use core::arch::global_asm;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
global_asm!(
    r#"
    .section .bss.bootstack,"aw",@nobits
    .align 16
boot_stack_riscv64:
    // 16 MiB boot stack, matching x86_64 and AArch64. The previous 16 KiB was
    // grossly undersized: `Bootstrap::init()` returns `KernelState` (~5 KiB)
    // by value, and the init chain holds several `KernelState`-sized copies on
    // the stack simultaneously (the `init()` return slot, the
    // `init_with_capacity_profile` `*state` temporary, the `init_boxed`
    // `MaybeUninit<KernelState>` temporary, and the caller's `kernel` binding),
    // plus the deep run_kernel_boot call chain and core::fmt machinery. With
    // only 16 KiB this overflowed the boot stack downward, corrupting a saved
    // return address so a later `ret` jumped to ~-2 and faulted with an
    // instruction access fault at sepc=0xfffffffffffffffe. x86_64/AArch64 never
    // hit this because their boot stacks are 16 MiB. Lives in .bss (NOLOAD),
    // so it costs RAM only, not image size.
    .skip 0x01000000
boot_stack_riscv64_end:

    .section .text.boot,"ax",@progbits
    .weak _start
    .type _start,@function
_start:
    // OpenSBI enters the boot hart in S-mode with a0=hartid and a1=FDT
    // pointer. Preserve BOTH across early setup: a0/a1 are the only handoff
    // the firmware gives us. We stash them in callee-saved s0/s1 (this is the
    // root of the call tree, so nothing else owns these yet).
    la sp, boot_stack_riscv64_end
    mv s0, a0                       // s0 = hartid (OpenSBI a0)
    mv s1, a1                       // s1 = DTB physical pointer (OpenSBI a1)

    // Install an early S-mode trap vector (direct mode) BEFORE running any
    // kernel code. Until the kernel installs its real trap handler, any
    // early fault (e.g. during bootstrap) must land in a deterministic
    // diagnostic park instead of silently re-entering the payload entry and
    // spinning forever. stvec must be 4-byte aligned; the label below is.
    la t0, yarm_riscv64_early_trap_vector
    csrw stvec, t0

    // Single-BSP selection: only the bootstrap hart (id 0) continues into
    // kernel bootstrap. Any other hart that reaches the cold-boot entry must
    // park in a safe loop BEFORE BSS use, allocator init, cmdline capture, or
    // kernel bootstrap. (On QEMU virt + OpenSBI, secondaries normally wait in
    // firmware for an HSM start, but this guard is defensive and correct even
    // if a secondary arrives here.)
    li t1, 0                        // BOOTSTRAP_CPU_ID
    bne s0, t1, .Lriscv64_secondary_cold_park

    // Boot hart: hand the firmware registers to the Rust primary entry, which
    // emits the early boot markers and then calls the common kernel entry.
    mv a0, s0                       // a0 = hartid
    mv a1, s1                       // a1 = DTB pointer
    call yarm_riscv64_primary_entry // -> ! (does not return)
1:
    wfi
    j 1b

.Lriscv64_secondary_cold_park:
    mv a0, s0                       // a0 = hartid
    call yarm_riscv64_secondary_cold_park // -> ! (does not return)
2:
    wfi
    j 2b

    // Early S-mode trap vector (direct mode). Runs before the kernel installs
    // its own handler. Re-establishes a known-good stack (the faulting sp may
    // be corrupt), reads the trap CSRs, reports them once, and parks. This
    // converts any pre-kernel fault into a single deterministic diagnostic
    // instead of an invisible reset loop.
    .align 4
    .global yarm_riscv64_early_trap_vector
    .type yarm_riscv64_early_trap_vector,@function
yarm_riscv64_early_trap_vector:
    la sp, boot_stack_riscv64_end
    csrr a0, scause
    csrr a1, sepc
    csrr a2, stval
    call yarm_riscv64_early_trap_report // -> ! (does not return)
3:
    wfi
    j 3b

    .global yarm_riscv64_secondary_entry
    .type yarm_riscv64_secondary_entry,@function
yarm_riscv64_secondary_entry:
    // Per the SBI HSM spec, a hart started via sbi_hart_start() begins here
    // with a0 = hartid and a1 = the opaque value. YARM passes a pointer to
    // SecondaryHartHandoff as the opaque value, so the handoff is in a1 (NOT
    // a0). Do not enter _start/yarm_kernel_main.
    beqz a1, 2f
    ld sp, 8(a1)            // stack_top lives at offset 8 of the handoff
    andi sp, sp, -16
    la t0, 3f
    csrw stvec, t0
    li t1, 2
    csrc sstatus, t1
    mv a0, a1              // yarm_riscv64_secondary_boot(handoff_ptr)
    call yarm_riscv64_secondary_boot
2:
    wfi
    j 2b
3:
    wfi
    j 3b
    "#
);

/// Early, allocation-free, lock-free marker writer for the RISC-V boot path.
///
/// Writes a formatted line straight to the SBI legacy console (a single
/// `console_putchar` ecall per byte). Unlike `yarm_log!`, this touches no
/// kernel statics, ring buffers, or locks, so it is safe on any hart at the
/// very earliest boot stage — before BSS use, allocator init, or kernel
/// bootstrap, and from the early trap vector where the kernel state may be
/// unusable. Lines longer than the fixed scratch buffer are truncated.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
struct EarlySbiLine {
    buf: [u8; 160],
    len: usize,
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
impl core::fmt::Write for EarlySbiLine {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &byte in s.as_bytes() {
            if self.len >= self.buf.len() {
                break;
            }
            self.buf[self.len] = byte;
            self.len += 1;
        }
        Ok(())
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub(crate) fn early_sbi_marker(args: core::fmt::Arguments<'_>) {
    use core::fmt::Write;
    let mut line = EarlySbiLine {
        buf: [0; 160],
        len: 0,
    };
    let _ = line.write_fmt(args);
    let text = core::str::from_utf8(&line.buf[..line.len]).unwrap_or("RISCV_EARLY_MARKER_UTF8_ERR");
    crate::arch::riscv64::console::write_line(text);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
macro_rules! early_marker {
    ($($arg:tt)*) => {
        $crate::arch::riscv64::boot::early_sbi_marker(core::format_args!($($arg)*))
    };
}

// The common kernel entry, defined in the `kernel_boot` binary and linked in.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
unsafe extern "C" {
    fn yarm_kernel_main(start_info_ptr: usize) -> !;
}

// Kernel image bounds from the linker script (riscv64-yarm-none.ld), used to
// reserve the firmware window below the kernel during RAM staging.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
unsafe extern "C" {
    static __kernel_start: u8;
}

/// Rust primary (boot-hart) entry, tail-called from `_start` with the
/// preserved OpenSBI handoff registers. Emits the early boot markers, then
/// hands `dtb_ptr` to the common kernel entry exactly as the previous
/// `mv a0, a1; call yarm_kernel_main` did.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_riscv64_primary_entry(hart_id: usize, dtb_ptr: usize) -> ! {
    early_marker!("RISCV_BOOT_ENTRY hart={} dtb=0x{:x}", hart_id, dtb_ptr);
    early_marker!("RISCV_BOOT_HART_SELECTED hart={}", hart_id);
    // Park every non-bootstrap hart in a safe Rust loop BEFORE the boot hart
    // touches BSS, the allocator, cmdline capture, or kernel bootstrap.
    park_secondary_harts_early();
    early_marker!("RISCV_DTB_PTR value=0x{:x}", dtb_ptr);
    unsafe { yarm_kernel_main(dtb_ptr) }
}

/// Cold-boot park for any non-bootstrap hart that reaches `_start`. Emits the
/// park marker and spins in `wfi`, never touching shared boot state.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_riscv64_secondary_cold_park(hart_id: usize) -> ! {
    early_marker!("RISCV_SECONDARY_HART_PARK hart={}", hart_id);
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }
}

/// Current kernel-bootstrap step breadcrumb, published by
/// [`riscv64_set_bootstrap_step`] and read by the early trap reporter so a
/// fault during `Bootstrap::init` can be attributed to a named step even
/// though the boot hart has no kernel logging context yet. We store a
/// `&'static str` as (ptr,len) in two atomics; the strings are static, so the
/// pointer stays valid for the whole boot.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static BOOTSTRAP_STEP_PTR: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static BOOTSTRAP_STEP_LEN: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// Records the current bootstrap step and emits a `RISCV_BOOTSTRAP_BEFORE_*`
/// breadcrumb. Called from the (arch-agnostic) kernel bootstrap via the
/// `arch::boot_entry::bootstrap_step` facade; a no-op on other arches.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn riscv64_set_bootstrap_step(name: &'static str) {
    use core::sync::atomic::Ordering;
    BOOTSTRAP_STEP_PTR.store(name.as_ptr() as usize, Ordering::Relaxed);
    BOOTSTRAP_STEP_LEN.store(name.len(), Ordering::Relaxed);
    early_marker!("RISCV_BOOTSTRAP_BEFORE_{}", name);
}

/// Returns the current bootstrap step name, if any (used by the trap reporter).
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn current_bootstrap_step() -> Option<&'static str> {
    use core::sync::atomic::Ordering;
    let ptr = BOOTSTRAP_STEP_PTR.load(Ordering::Relaxed);
    let len = BOOTSTRAP_STEP_LEN.load(Ordering::Relaxed);
    if ptr == 0 || len == 0 || len > 64 {
        return None;
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
    core::str::from_utf8(bytes).ok()
}

/// Early trap reporter, tail-called from the early S-mode trap vector. Reports
/// the trap CSRs once (plus the named bootstrap step, if known) and parks,
/// converting any pre-kernel fault into a single deterministic diagnostic
/// instead of an invisible payload-reset loop.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_riscv64_early_trap_report(scause: usize, sepc: usize, stval: usize) -> ! {
    early_marker!(
        "RISCV_EARLY_TRAP scause=0x{:x} sepc=0x{:x} stval=0x{:x}",
        scause,
        sepc,
        stval
    );
    if let Some(step) = current_bootstrap_step() {
        early_marker!("RISCV_BOOTSTRAP_TRAP_STEP name={}", step);
    }
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }
}

// ── RISC-V S-mode -> U-mode entry + S-mode trap vector ─────────────────────
//
// `yarm_riscv64_enter_user` performs the real `sret` into U-mode. It loads the
// per-task `satp` (a user page table that also carries the kernel-shared
// gigapage so the kernel/trap-vector keep executing across the switch), sets
// `sscratch` to a kernel trap stack, installs the S-mode trap vector, programs
// `sstatus` for U-mode (SPP=0, SPIE=0 — interrupts stay masked in user), loads
// the user GPRs/sp/sepc, and `sret`s.
//
// `yarm_riscv64_trap_vector` is the S-mode trap entry. It swaps to the kernel
// trap stack via `sscratch`, snapshots the trap CSRs and the syscall number,
// and hands off to the Rust reporter. It lives in kernel text (covered by the
// gigapage) so it is reachable with a user `satp` active.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
core::arch::global_asm!(
    r#"
    .section .text, "ax", @progbits

    .global yarm_riscv64_enter_user
    .type yarm_riscv64_enter_user, @function
yarm_riscv64_enter_user:
    // a0 = *const RiscvEnterUserCtx (kernel addr, mapped by gigapage)
    mv t0, a0
    ld t1, 24(t0)              // kernel_sp
    csrw sscratch, t1
    la t2, yarm_riscv64_trap_vector
    csrw stvec, t2
    ld t3, 8(t0)              // sepc (user entry)
    csrw sepc, t3
    // sstatus: clear SPP (bit 8) -> return to U-mode after sret; clear SPIE
    // (bit 5) so interrupts stay disabled in U-mode (we have no timer/IRQ
    // path yet); set SUM (bit 18) so the kernel can still touch U-pages from
    // S-mode for trap bookkeeping if/when we resume there. We use csrrc/csrrs
    // (atomic clear/set with mask), the canonical way to flip individual
    // status bits without read-modify-write hazards.
    li t5, 0x120              // SPP | SPIE
    csrc sstatus, t5
    li t5, 0x40000            // SUM
    csrs sstatus, t5
    // user GPRs
    ld a0, 32(t0)
    ld a1, 40(t0)
    ld a2, 48(t0)
    ld a3, 56(t0)
    ld a4, 64(t0)
    ld a5, 72(t0)
    ld tp, 80(t0)
    ld sp, 16(t0)            // user sp
    ld t1, 0(t0)             // satp
    csrw satp, t1
    sfence.vma x0, x0
    sret

    .align 4
    .global yarm_riscv64_trap_vector
    .type yarm_riscv64_trap_vector, @function
yarm_riscv64_trap_vector:
    // Entered on a trap. sp is the user sp; swap in the kernel trap stack.
    csrrw sp, sscratch, sp     // sp = kernel trap stack; sscratch = user sp
    addi sp, sp, -64
    sd ra, 0(sp)
    sd a7, 8(sp)               // user a7 (syscall number)
    csrr a0, scause
    csrr a1, sepc
    csrr a2, stval
    ld a3, 8(sp)               // a7
    csrr a4, sstatus
    csrr a5, sscratch          // user sp at trap time
    call yarm_riscv64_user_trap_report
1:
    wfi
    j 1b
"#
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[repr(C)]
struct RiscvEnterUserCtx {
    satp: u64,      // 0
    sepc: u64,      // 8
    user_sp: u64,   // 16
    kernel_sp: u64, // 24
    a0: u64,        // 32
    a1: u64,        // 40
    a2: u64,        // 48
    a3: u64,        // 56
    a4: u64,        // 64
    a5: u64,        // 72
    tp: u64,        // 80
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
unsafe extern "C" {
    fn yarm_riscv64_enter_user(ctx: *const RiscvEnterUserCtx) -> !;
    static yarm_riscv64_trap_vector: u8;
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[repr(align(16))]
struct RiscvTrapStack([u8; 16 * 1024]);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static mut RISCV_TRAP_STACK: RiscvTrapStack = RiscvTrapStack([0; 16 * 1024]);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn riscv_trap_stack_top() -> u64 {
    let base = core::ptr::addr_of!(RISCV_TRAP_STACK) as u64;
    (base + (16 * 1024)) & !0xf
}

/// S-mode trap reporter for U-mode traps. For this bring-up stage it is a
/// black-box recorder: it captures the trap CSRs and the syscall number, emits
/// the required markers, and halts deterministically. (The full handle +
/// `sret` round-trip is the next stage.)
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_riscv64_user_trap_report(
    scause: usize,
    sepc: usize,
    stval: usize,
    a7: usize,
    sstatus: usize,
    user_sp: usize,
) -> ! {
    const EXC_USER_ECALL: usize = 8;
    // sstatus.SPP (bit 8): 0 = trap was taken from U-mode (sret really
    // returned to U), 1 = trap was taken from S-mode (sret never reached U).
    let spp = (sstatus >> 8) & 1;
    let trap_from_u = spp == 0;
    early_marker!(
        "RISCV_TRAP_ENTER scause=0x{:x} sepc=0x{:x} stval=0x{:x} sstatus=0x{:x} spp={} from_u={} user_sp=0x{:x}",
        scause,
        sepc,
        stval,
        sstatus,
        spp,
        trap_from_u as u8,
        user_sp
    );
    early_marker!(
        "RISCV_FIRST_USER_TRAP scause=0x{:x} sepc=0x{:x} stval=0x{:x}",
        scause,
        sepc,
        stval
    );
    if scause == EXC_USER_ECALL {
        early_marker!("RISCV_FIRST_USER_SYSCALL nr={}", a7);
        early_marker!("RISCV_TRAP_HALTED reason=first_user_syscall_captured");
    } else if trap_from_u {
        early_marker!(
            "RISCV_TRAP_HALTED reason=first_user_trap_captured_from_u scause=0x{:x}",
            scause
        );
    } else {
        early_marker!(
            "RISCV_TRAP_HALTED reason=sret_failed_or_kernel_fault scause=0x{:x}",
            scause
        );
    }
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }
}

/// Proves the kernel keeps executing after a `satp` switch into a user page
/// table: installs `satp`, runs real kernel code (the marker path touches
/// kernel .rodata/.data/.bss in the gigapage), then restores the prior `satp`.
/// If the gigapage were wrong this would fault into the early trap vector
/// (also gigapage-mapped) rather than dying silently.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn riscv64_probe_satp_alive(satp: u64) {
    let prev: u64;
    unsafe {
        core::arch::asm!("csrr {0}, satp", out(reg) prev, options(nostack, preserves_flags));
    }
    crate::arch::riscv64::page_table::write_satp(satp);
    early_marker!("RISCV_SATP_KERNEL_ALIVE_OK satp=0x{:x}", satp);
    crate::arch::riscv64::page_table::write_satp(prev);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const RING3_INIT_SERVER_TID: u64 = 1;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const RING3_SUPERVISOR_TID: u64 = 2;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const RING3_PM_SERVER_TID: u64 = 3;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const INITRAMFS_HELLO_WORLD_IMAGE_ID: u64 = 0x494E_4954_5256_484C; // "INITRVHL"

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn initramfs_static_hello_world_elf() -> [u8; 256] {
    let mut image = [0u8; 256];
    // ELF header.
    image[..4].copy_from_slice(b"\x7FELF");
    image[4] = 2; // ELFCLASS64
    image[5] = 1; // little-endian
    image[6] = 1; // EV_CURRENT
    image[7] = 0; // SYSV ABI
    image[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    image[18..20].copy_from_slice(&0xF3u16.to_le_bytes()); // EM_RISCV
    image[20..24].copy_from_slice(&1u32.to_le_bytes()); // EV_CURRENT
    let entry = 0x0040_1000u64;
    image[24..32].copy_from_slice(&entry.to_le_bytes());
    image[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
    image[52..54].copy_from_slice(&(64u16).to_le_bytes()); // e_ehsize
    image[54..56].copy_from_slice(&(56u16).to_le_bytes()); // e_phentsize
    image[56..58].copy_from_slice(&(1u16).to_le_bytes()); // e_phnum

    // Single PT_LOAD segment.
    let ph = 64usize;
    image[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    image[ph + 4..ph + 8].copy_from_slice(&5u32.to_le_bytes()); // RX
    image[ph + 8..ph + 16].copy_from_slice(&128u64.to_le_bytes()); // p_offset
    image[ph + 16..ph + 24].copy_from_slice(&(entry & !0xFFF).to_le_bytes()); // p_vaddr
    image[ph + 24..ph + 32].copy_from_slice(&0u64.to_le_bytes()); // p_paddr
    image[ph + 32..ph + 40].copy_from_slice(&12u64.to_le_bytes()); // p_filesz
    image[ph + 40..ph + 48].copy_from_slice(&16u64.to_le_bytes()); // p_memsz
    image[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes()); // p_align

    // li a7, SYSCALL_YIELD_NR; ecall; j ecall.
    image[128..140].copy_from_slice(&[
        0x93, 0x08, 0x00, 0x00, // addi a7, x0, 0
        0x73, 0x00, 0x00, 0x00, // ecall
        0x6F, 0xF0, 0xDF, 0xFF, // jal x0, -4
    ]);
    image
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const INITRD_INIT_ELF_MAX_SIZE: usize = 16 * 1024 * 1024;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn load_init_elf_from_initramfs_vfs() -> Option<alloc::vec::Vec<u8>> {
    let bytes = crate::kernel::boot::Bootstrap::boot_initrd_bytes()?;
    let entry = yarm_srv_common::cpio::CpioArchive::new(bytes)
        .find("/init")
        .ok()
        .flatten()
        .or_else(|| {
            yarm_srv_common::cpio::CpioArchive::new(bytes)
                .find("init")
                .ok()
                .flatten()
        })?;
    let file_data = entry.file_data();
    if file_data.len() > INITRD_INIT_ELF_MAX_SIZE {
        crate::yarm_log!(
            "YARM_INITRD_INIT_TOO_LARGE len={} cap={}",
            file_data.len(),
            INITRD_INIT_ELF_MAX_SIZE
        );
        return None;
    }
    Some(alloc::vec::Vec::from(file_data))
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn load_supervisor_elf_from_initramfs_vfs() -> Option<alloc::vec::Vec<u8>> {
    let bytes = crate::kernel::boot::Bootstrap::boot_initrd_bytes()?;
    let entry = yarm_srv_common::cpio::CpioArchive::new(bytes)
        .find("sbin/supervisor")
        .ok()
        .flatten()?;
    let file_data = entry.file_data();
    if file_data.len() > INITRD_INIT_ELF_MAX_SIZE {
        return None;
    }
    Some(alloc::vec::Vec::from(file_data))
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn load_pm_elf_from_initramfs_vfs() -> Option<alloc::vec::Vec<u8>> {
    let bytes = crate::kernel::boot::Bootstrap::boot_initrd_bytes()?;
    let entry = yarm_srv_common::cpio::CpioArchive::new(bytes)
        .find("sbin/process_manager")
        .ok()
        .flatten()?;
    let file_data = entry.file_data();
    if file_data.len() > INITRD_INIT_ELF_MAX_SIZE {
        return None;
    }
    Some(alloc::vec::Vec::from(file_data))
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn bootstrap_first_user_task(
    kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    use crate::kernel::boot::UserImageSpec;
    use crate::kernel::task::TaskClass;

    if kernel.task_asid(RING3_INIT_SERVER_TID).is_some() {
        return Ok(());
    }

    let (init_asid, _) = kernel.create_user_address_space()?;
    let init_image = load_init_elf_from_initramfs_vfs();
    let init_fallback = initramfs_static_hello_world_elf();
    let (init_bytes, init_source): (&[u8], &str) = match init_image.as_deref() {
        Some(img) => (img, "initrd"),
        None => (&init_fallback, "synthetic"),
    };
    let init_elf_info = yarm_srv_common::elf::ElfImageInfo::parse(0, init_bytes)
        .map_err(|_| crate::kernel::boot::KernelError::WrongObject)?;
    let (_, init_first_pt_load, init_heap) =
        kernel.load_elf_pt_load_segments(init_asid, init_bytes)?;
    let init_entry = init_elf_info.entry as usize;
    crate::yarm_log!("INIT_ELF_HEADER_ENTRY value={:#x}", init_elf_info.entry);
    crate::yarm_log!("INIT_FIRST_PT_LOAD_VADDR value={:#x}", init_first_pt_load);
    crate::yarm_log!("INIT_SELECTED_ENTRY value={:#x}", init_entry);
    if init_entry == init_first_pt_load {
        crate::yarm_log!(
            "INIT_ENTRY_EQUALS_FIRST_PT_LOAD_WARN: ELF e_entry matches first PT_LOAD base; entry may be wrong"
        );
    }
    crate::yarm_log!(
        "YARM_INITRD_INIT_ELF_SELECTED entry={:#x} source={}",
        init_entry,
        init_source
    );

    let supervisor_image = load_supervisor_elf_from_initramfs_vfs();
    let supervisor_aei: Option<(_, usize, usize)> = if let Some(ref sup_bytes) = supervisor_image {
        let sup_elf_info = yarm_srv_common::elf::ElfImageInfo::parse(1, sup_bytes)
            .map_err(|_| crate::kernel::boot::KernelError::WrongObject)?;
        let (sup_asid, _) = kernel.create_user_address_space()?;
        let (_, sup_first_pt_load, sup_heap) =
            kernel.load_elf_pt_load_segments(sup_asid, sup_bytes)?;
        let sup_entry = sup_elf_info.entry as usize;
        crate::yarm_log!(
            "SUPERVISOR_ELF_HEADER_ENTRY value={:#x}",
            sup_elf_info.entry
        );
        crate::yarm_log!(
            "SUPERVISOR_FIRST_PT_LOAD_VADDR value={:#x}",
            sup_first_pt_load
        );
        crate::yarm_log!("SUPERVISOR_SELECTED_ENTRY value={:#x}", sup_entry);
        if sup_entry == sup_first_pt_load {
            crate::yarm_log!(
                "SUPERVISOR_ENTRY_EQUALS_FIRST_PT_LOAD_WARN: ELF e_entry matches first PT_LOAD base; entry may be wrong"
            );
        }
        Some((sup_asid, sup_entry, sup_heap))
    } else {
        crate::yarm_log!("YARM_SUPERVISOR_ELF_MISSING path=sbin/supervisor");
        return Err(crate::kernel::boot::KernelError::MemoryObjectMissing);
    };

    let pm_image = load_pm_elf_from_initramfs_vfs();
    let pm_aei: Option<(_, usize, usize)> = if let Some(ref pm_bytes) = pm_image {
        let pm_elf_info = yarm_srv_common::elf::ElfImageInfo::parse(2, pm_bytes)
            .map_err(|_| crate::kernel::boot::KernelError::WrongObject)?;
        let (pm_asid, _) = kernel.create_user_address_space()?;
        let (_, pm_first_pt_load, pm_heap) = kernel.load_elf_pt_load_segments(pm_asid, pm_bytes)?;
        let pm_entry = pm_elf_info.entry as usize;
        crate::yarm_log!("PM_ELF_HEADER_ENTRY value={:#x}", pm_elf_info.entry);
        crate::yarm_log!("PM_FIRST_PT_LOAD_VADDR value={:#x}", pm_first_pt_load);
        crate::yarm_log!("PM_SELECTED_ENTRY value={:#x}", pm_entry);
        if pm_entry == pm_first_pt_load {
            crate::yarm_log!(
                "PM_ENTRY_EQUALS_FIRST_PT_LOAD_WARN: ELF e_entry matches first PT_LOAD base; entry may be wrong"
            );
        }
        Some((pm_asid, pm_entry, pm_heap))
    } else {
        crate::yarm_log!("YARM_PM_ELF_MISSING path=sbin/process_manager");
        return Err(crate::kernel::boot::KernelError::MemoryObjectMissing);
    };

    if supervisor_aei.is_some() {
        kernel.register_task_with_class(RING3_SUPERVISOR_TID, TaskClass::SystemServer)?;
    }
    if pm_aei.is_some() {
        kernel.register_task_with_class(RING3_PM_SERVER_TID, TaskClass::SystemServer)?;
    }
    kernel.register_task_with_class(RING3_INIT_SERVER_TID, TaskClass::SystemServer)?;

    let (_, pm_inbound_send_root, pm_inbound_recv_root) = kernel.create_endpoint(16)?;
    let pm_inbound_send_init = kernel.grant_capability_task_to_task_with_rights(
        0,
        pm_inbound_send_root,
        RING3_INIT_SERVER_TID,
        crate::kernel::capabilities::CapRights::SEND,
    )?;
    crate::yarm_log!(
        "CAP_GRANT_BOOT dst_tid={} slot=1 cap={} rights=SEND result=ok",
        RING3_INIT_SERVER_TID,
        pm_inbound_send_init.0
    );
    let pm_inbound_send_sup = if supervisor_aei.is_some() {
        let c = kernel.grant_capability_task_to_task_with_rights(
            0,
            pm_inbound_send_root,
            RING3_SUPERVISOR_TID,
            crate::kernel::capabilities::CapRights::SEND,
        )?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=1 cap={} rights=SEND result=ok",
            RING3_SUPERVISOR_TID,
            c.0
        );
        Some(c)
    } else {
        None
    };
    let pm_inbound_recv_pm = if pm_aei.is_some() {
        let c = kernel.grant_capability_task_to_task_with_rights(
            0,
            pm_inbound_recv_root,
            RING3_PM_SERVER_TID,
            crate::kernel::capabilities::CapRights::RECEIVE,
        )?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=17 cap={} rights=RECEIVE result=ok",
            RING3_PM_SERVER_TID,
            c.0
        );
        Some(c)
    } else {
        None
    };

    let (_, _, init_reply_recv_root) = kernel.create_endpoint(8)?;
    let init_reply_recv_init = kernel.grant_capability_task_to_task_with_rights(
        0,
        init_reply_recv_root,
        RING3_INIT_SERVER_TID,
        crate::kernel::capabilities::CapRights::RECEIVE,
    )?;
    crate::yarm_log!(
        "CAP_GRANT_BOOT dst_tid={} slot=2 cap={} rights=RECEIVE result=ok",
        RING3_INIT_SERVER_TID,
        init_reply_recv_init.0
    );

    // Dedicated PM outbound reply endpoint: PM receives on startup slot 2.
    let (_, _, pm_outbound_reply_recv_root) = kernel.create_endpoint(8)?;
    let pm_outbound_reply_recv_pm = if pm_aei.is_some() {
        let c = kernel.grant_capability_task_to_task_with_rights(
            0,
            pm_outbound_reply_recv_root,
            RING3_PM_SERVER_TID,
            crate::kernel::capabilities::CapRights::RECEIVE,
        )?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=2 cap={} rights=RECEIVE result=ok",
            RING3_PM_SERVER_TID,
            c.0
        );
        Some(c)
    } else {
        None
    };

    let (_, _, sup_fault_recv_root) = kernel.create_endpoint(8)?;
    let sup_fault_recv_sup = if supervisor_aei.is_some() {
        let c = kernel.grant_capability_task_to_task_with_rights(
            0,
            sup_fault_recv_root,
            RING3_SUPERVISOR_TID,
            crate::kernel::capabilities::CapRights::RECEIVE,
        )?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=3 cap={} rights=RECEIVE result=ok",
            RING3_SUPERVISOR_TID,
            c.0
        );
        Some(c)
    } else {
        None
    };

    // EP4: Supervisor control — supervisor SEND (slot 4), supervisor RECV (slot 5).
    let (_, sup_ctrl_send_root, sup_ctrl_recv_root) = kernel.create_endpoint(8)?;
    let sup_ctrl_send_sup = if supervisor_aei.is_some() {
        let c = kernel.grant_capability_task_to_task_with_rights(
            0,
            sup_ctrl_send_root,
            RING3_SUPERVISOR_TID,
            crate::kernel::capabilities::CapRights::SEND,
        )?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=4 cap={} rights=SEND result=ok",
            RING3_SUPERVISOR_TID,
            c.0
        );
        Some(c)
    } else {
        None
    };
    let sup_ctrl_recv_sup = if supervisor_aei.is_some() {
        let c = kernel.grant_capability_task_to_task_with_rights(
            0,
            sup_ctrl_recv_root,
            RING3_SUPERVISOR_TID,
            crate::kernel::capabilities::CapRights::RECEIVE,
        )?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=5 cap={} rights=RECEIVE result=ok",
            RING3_SUPERVISOR_TID,
            c.0
        );
        Some(c)
    } else {
        None
    };

    // EP5: Supervisor PM reply — supervisor gets RECV (slot 2); distinct from init's EP2.
    let (_, _, sup_pm_reply_recv_root) = kernel.create_endpoint(8)?;
    let sup_pm_reply_recv_sup = if supervisor_aei.is_some() {
        let c = kernel.grant_capability_task_to_task_with_rights(
            0,
            sup_pm_reply_recv_root,
            RING3_SUPERVISOR_TID,
            crate::kernel::capabilities::CapRights::RECEIVE,
        )?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=2 cap={} rights=RECEIVE result=ok",
            RING3_SUPERVISOR_TID,
            c.0
        );
        Some(c)
    } else {
        None
    };

    if let Some(fault_cap) = sup_fault_recv_sup {
        kernel.set_supervisor_endpoint_for_task(RING3_SUPERVISOR_TID, fault_cap)?;
    }

    if let Some((sup_asid, sup_entry, sup_heap)) = supervisor_aei {
        let mut sup_args = UserImageSpec::DEFAULT_STARTUP_ARGS;
        sup_args[0] = RING3_SUPERVISOR_TID;
        if let Some(c) = pm_inbound_send_sup {
            sup_args[1] = c.0;
        }
        if let Some(c) = sup_pm_reply_recv_sup {
            sup_args[2] = c.0;
        }
        if let Some(c) = sup_fault_recv_sup {
            sup_args[3] = c.0;
        }
        if let Some(c) = sup_ctrl_send_sup {
            sup_args[4] = c.0;
        }
        if let Some(c) = sup_ctrl_recv_sup {
            sup_args[5] = c.0;
        }
        sup_args[8] = RING3_INIT_SERVER_TID;
        for n in 0..10usize {
            crate::yarm_log!("SUP_STARTUP_SLOT slot={} value={}", n, sup_args[n]);
        }
        kernel.spawn_user_task_from_image(UserImageSpec {
            tid: RING3_SUPERVISOR_TID,
            entry: sup_entry,
            asid: Some(sup_asid),
            class: TaskClass::SystemServer,
            startup_args: sup_args,
            ..Default::default()
        })?;
        kernel.set_task_brk_bounds(RING3_SUPERVISOR_TID, sup_heap, sup_heap)?;
        crate::yarm_log!("YARM_SUPERVISOR_TID2_SPAWNED tid={}", RING3_SUPERVISOR_TID);
    }

    if let Some((pm_asid, pm_entry, pm_heap)) = pm_aei {
        let mut pm_args = UserImageSpec::DEFAULT_STARTUP_ARGS;
        pm_args[0] = RING3_PM_SERVER_TID;
        if let Some(c) = pm_outbound_reply_recv_pm {
            pm_args[2] = c.0;
        }
        if let Some(c) = pm_inbound_recv_pm {
            pm_args[17] = c.0;
        }
        kernel.spawn_user_task_from_image(UserImageSpec {
            tid: RING3_PM_SERVER_TID,
            entry: pm_entry,
            asid: Some(pm_asid),
            class: TaskClass::SystemServer,
            startup_args: pm_args,
            ..Default::default()
        })?;
        kernel.set_task_brk_bounds(RING3_PM_SERVER_TID, pm_heap, pm_heap)?;
        crate::yarm_log!("YARM_PM_TID3_SPAWNED tid={}", RING3_PM_SERVER_TID);
    }

    let mut init_args = UserImageSpec::DEFAULT_STARTUP_ARGS;
    init_args[0] = RING3_INIT_SERVER_TID;
    init_args[1] = pm_inbound_send_init.0;
    init_args[2] = init_reply_recv_init.0;
    init_args[9] = RING3_SUPERVISOR_TID;
    crate::yarm_log!(
        "YARM_FIRST_USER_STARTUP_ARGS tid={} arg0={} arg1={} arg2={} arg3={}",
        RING3_INIT_SERVER_TID,
        init_args[0],
        init_args[1],
        init_args[2],
        init_args[3]
    );
    kernel.spawn_user_task_from_image(UserImageSpec {
        tid: RING3_INIT_SERVER_TID,
        entry: init_entry,
        asid: Some(init_asid),
        class: TaskClass::SystemServer,
        startup_args: init_args,
        ..Default::default()
    })?;
    kernel.set_task_brk_bounds(RING3_INIT_SERVER_TID, init_heap, init_heap)?;
    crate::yarm_log!(
        "YARM_INIT_DONE arch=riscv64 phase=kernel_static_init_elf image_id=0x{:x} seeded=0 initramfs_handled=1 devfs_handled=0",
        INITRAMFS_HELLO_WORLD_IMAGE_ID
    );
    Ok(())
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "riscv64")))]
pub fn bootstrap_first_user_task(
    _kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    Ok(())
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const QEMU_VIRT_HSM_SECONDARY_HART_LIMIT: usize = 8;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const RISCV64_SECONDARY_STACK_BYTES: usize = 4096;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const RISCV64_SECONDARY_ACK_EMPTY: usize = 0;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const RISCV64_SECONDARY_ACK_START_REQUESTED: usize = 1;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const RISCV64_SECONDARY_ACK_PARKED: usize = 2;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const RISCV64_SECONDARY_ACK_POLL_ITERS: usize = 100_000;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[repr(C)]
#[derive(Clone, Copy)]
struct SecondaryHartHandoff {
    hart_id: usize,
    stack_top: usize,
    ack: usize,
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
impl SecondaryHartHandoff {
    const fn empty() -> Self {
        Self {
            hart_id: usize::MAX,
            stack_top: 0,
            ack: RISCV64_SECONDARY_ACK_EMPTY,
        }
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[repr(align(16))]
#[derive(Clone, Copy)]
struct SecondaryHartStack([u8; RISCV64_SECONDARY_STACK_BYTES]);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static mut RISCV64_SECONDARY_HANDOFFS: [SecondaryHartHandoff; QEMU_VIRT_HSM_SECONDARY_HART_LIMIT] =
    [SecondaryHartHandoff::empty(); QEMU_VIRT_HSM_SECONDARY_HART_LIMIT];
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static mut RISCV64_SECONDARY_STACKS: [SecondaryHartStack; QEMU_VIRT_HSM_SECONDARY_HART_LIMIT] =
    [SecondaryHartStack([0; RISCV64_SECONDARY_STACK_BYTES]); QEMU_VIRT_HSM_SECONDARY_HART_LIMIT];

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
unsafe extern "C" {
    fn yarm_riscv64_secondary_entry() -> !;
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_riscv64_secondary_boot(handoff_ptr: usize) -> ! {
    let mut hart_id = usize::MAX;
    if handoff_ptr != 0 {
        let handoff = handoff_ptr as *mut SecondaryHartHandoff;
        unsafe {
            hart_id = core::ptr::read_volatile(core::ptr::addr_of!((*handoff).hart_id));
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*handoff).ack),
                RISCV64_SECONDARY_ACK_PARKED,
            );
        }
    }
    // Required early-boot marker: a non-bootstrap hart has reached a safe,
    // Rust-controlled park. This runs on the secondary's own per-hart stack,
    // with interrupts masked, before it could ever touch kernel bootstrap.
    early_marker!("RISCV_SECONDARY_HART_PARK hart={}", hart_id);
    crate::arch::riscv64::console::write_line("YARM_RISCV64_SMP_SECONDARY_PARKED");
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn secondary_stack_top(slot: usize) -> usize {
    let stacks = core::ptr::addr_of_mut!(RISCV64_SECONDARY_STACKS) as *mut SecondaryHartStack;
    unsafe {
        stacks
            .add(slot)
            .cast::<u8>()
            .add(RISCV64_SECONDARY_STACK_BYTES) as usize
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn prepare_secondary_handoff(slot: usize, hart_id: usize) -> usize {
    let handoffs = core::ptr::addr_of_mut!(RISCV64_SECONDARY_HANDOFFS) as *mut SecondaryHartHandoff;
    let handoff = unsafe { handoffs.add(slot) };
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*handoff).hart_id), hart_id);
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!((*handoff).stack_top),
            secondary_stack_top(slot),
        );
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!((*handoff).ack),
            RISCV64_SECONDARY_ACK_START_REQUESTED,
        );
    }
    handoff as usize
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn secondary_ack(slot: usize) -> usize {
    let handoffs = core::ptr::addr_of!(RISCV64_SECONDARY_HANDOFFS) as *const SecondaryHartHandoff;
    let handoff = unsafe { handoffs.add(slot) };
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*handoff).ack)) }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn wait_for_secondary_ack(slot: usize) -> bool {
    for _ in 0..RISCV64_SECONDARY_ACK_POLL_ITERS {
        if secondary_ack(slot) == RISCV64_SECONDARY_ACK_PARKED {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Ensures the QEMU-virt secondary harts are brought to a Rust-controlled
/// park exactly once. On QEMU virt + OpenSBI, non-boot harts wait in firmware
/// for an HSM `hart_start`; this drives that start so each secondary lands in
/// `yarm_riscv64_secondary_boot`, emits `RISCV_SECONDARY_HART_PARK hart=N`,
/// and spins in `wfi`. The `swap` guard makes this idempotent so the early
/// (pre-bootstrap) call and the legacy post-bootstrap call cannot double-start
/// a hart (`SBI_ERR_ALREADY_*`).
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static RISCV64_SECONDARIES_PARKED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn park_qemu_virt_secondaries_once(context: &str) {
    if RISCV64_SECONDARIES_PARKED.swap(true, core::sync::atomic::Ordering::AcqRel) {
        return;
    }

    let hsm_available =
        match crate::arch::riscv64::sbi::probe_extension(crate::arch::riscv64::sbi::SBI_EXT_HSM) {
            Ok(0) => false,
            Ok(_) => true,
            Err(_) => false,
        };
    if !hsm_available {
        early_marker!(
            "RISCV_SECONDARY_PARK_SKIPPED reason=no_hsm context={}",
            context
        );
        return;
    }

    let entry_addr = (yarm_riscv64_secondary_entry as *const () as usize)
        .saturating_sub(crate::arch::platform_constants::KERNEL_LINK_VIRT_BASE as usize);
    let boot_hart = crate::arch::platform_constants::BOOTSTRAP_CPU_ID as usize;
    for hart_id in 0..QEMU_VIRT_HSM_SECONDARY_HART_LIMIT {
        if hart_id == boot_hart {
            continue;
        }
        let slot = hart_id;
        let handoff_ptr = prepare_secondary_handoff(slot, hart_id);
        match crate::arch::riscv64::sbi::hsm_hart_start(hart_id, entry_addr, handoff_ptr) {
            Ok(()) => {
                let acked = wait_for_secondary_ack(slot);
                crate::yarm_log!(
                    "YARM_RISCV64_SMP_HART_START hart={} ret=0 ack={} state=parked_not_online entry=0x{:x} handoff=0x{:x} context={}",
                    hart_id,
                    acked as u8,
                    entry_addr,
                    handoff_ptr,
                    context
                );
            }
            // Absent harts (e.g. all slots beyond `-smp N`) return an HSM
            // error; that is expected, so stay silent to avoid log spam.
            Err(_) => {}
        }
    }
}

/// Parks the secondary harts early, before kernel bootstrap. Called from the
/// boot-hart primary entry so non-boot harts are in a safe Rust-controlled
/// `wfi` loop before any BSS use, allocator init, cmdline capture, or kernel
/// bootstrap on the boot hart.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn park_secondary_harts_early() {
    park_qemu_virt_secondaries_once("early_primary");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn release_secondary_cpus_after_bootstrap() {
    // Idempotent: a no-op if the secondaries were already parked early.
    park_qemu_virt_secondaries_once("post_bootstrap");
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "riscv64")))]
pub fn release_secondary_cpus_after_bootstrap() {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn enter_dispatched_user_task_if_available(
    kernel: &crate::kernel::boot::KernelState,
    dispatched_tid: Option<u64>,
) {
    let Some(tid) = dispatched_tid else {
        return;
    };
    early_marker!("RISCV_FIRST_USER_PREP_BEGIN tid={}", tid);

    let Some(context) = kernel.thread_user_context(tid) else {
        early_marker!(
            "RISCV_USERSPACE_DEFERRED reason=no_user_context tid={}",
            tid
        );
        return;
    };
    if context.instruction_ptr.0 == 0 || context.stack_ptr.0 == 0 {
        early_marker!(
            "RISCV_USERSPACE_DEFERRED reason=empty_user_context tid={} pc=0x{:x} sp=0x{:x}",
            tid,
            context.instruction_ptr.0,
            context.stack_ptr.0
        );
        return;
    }
    let Some(asid) = kernel.task_asid(tid) else {
        early_marker!("RISCV_USERSPACE_DEFERRED reason=no_asid tid={}", tid);
        return;
    };

    // Phase 1: Sv39 plan — install the kernel-shared gigapage into the user
    // root so the kernel/trap path survives the satp switch.
    early_marker!("RISCV_SV39_PLAN_BEGIN");
    let (kstart, kend) = match crate::arch::riscv64::page_table::map_kernel_shared_into_asid(asid) {
        Ok(range) => range,
        Err(err) => {
            early_marker!(
                "RISCV_USERSPACE_DEFERRED reason=gigapage_install_failed err={:?}",
                err
            );
            return;
        }
    };
    early_marker!(
        "RISCV_SV39_MAP_KERNEL start=0x{:x} end=0x{:x}",
        kstart,
        kend
    );
    let trap_vector = unsafe { core::ptr::addr_of!(yarm_riscv64_trap_vector) as u64 };
    early_marker!(
        "RISCV_SV39_MAP_TRAP_VECTOR va=0x{:x} pa=0x{:x}",
        trap_vector,
        trap_vector
    );
    let kernel_sp = riscv_trap_stack_top();
    early_marker!(
        "RISCV_SV39_MAP_KERNEL_STACK va=0x{:x} pa=0x{:x}",
        kernel_sp,
        kernel_sp
    );
    // The RISC-V console is SBI (ecall to M-mode), so no UART MMIO mapping is
    // required while a user address space is active.
    early_marker!("RISCV_SV39_MAP_UART va=0x0 pa=0x0 note=sbi_console_no_mmio_needed");
    early_marker!("RISCV_SV39_USER_ROOT_READY tid={}", tid);
    early_marker!("RISCV_SV39_PLAN_DONE");

    let Some(satp) = crate::arch::riscv64::page_table::cr3_for_asid(asid) else {
        early_marker!("RISCV_USERSPACE_DEFERRED reason=no_satp tid={}", tid);
        return;
    };

    // Phase 1 proof: switch to the user satp, run kernel code, switch back.
    early_marker!("RISCV_SATP_INSTALL_BEGIN root=0x{:x}", satp);
    riscv64_probe_satp_alive(satp);
    early_marker!("RISCV_SATP_INSTALL_DONE value=0x{:x}", satp);

    // Phase 2: the real S-mode trap vector (installed by the enter-user asm via
    // `csrw stvec` just before sret).
    early_marker!("RISCV_TRAP_VECTOR_INSTALL_BEGIN");
    early_marker!("RISCV_TRAP_VECTOR_INSTALL_DONE base=0x{:x}", trap_vector);

    // Phase 3: build the user context and sret into U-mode.
    early_marker!(
        "RISCV_FIRST_USER_ELF_OK tid={} entry=0x{:x}",
        tid,
        context.instruction_ptr.0
    );
    early_marker!(
        "RISCV_FIRST_USER_STACK_OK tid={} sp=0x{:x}",
        tid,
        context.stack_ptr.0
    );
    let tls = kernel.thread_tls_base(tid).unwrap_or(0) as u64;
    let ctx = RiscvEnterUserCtx {
        satp,
        sepc: context.instruction_ptr.0,
        user_sp: context.stack_ptr.0,
        kernel_sp,
        a0: context.arg0 as u64,
        a1: context.arg1 as u64,
        a2: context.arg2 as u64,
        a3: context.arg3 as u64,
        a4: context.arg4 as u64,
        a5: context.arg5 as u64,
        tp: tls,
    };
    early_marker!(
        "RISCV_FIRST_USER_CONTEXT_OK tid={} pc=0x{:x} sp=0x{:x}",
        tid,
        ctx.sepc,
        ctx.user_sp
    );
    early_marker!("RISCV_ENTER_USER_ATTEMPT tid={}", tid);
    early_marker!("RISCV_ENTER_USER_SRET tid={}", tid);
    unsafe {
        yarm_riscv64_enter_user(&ctx as *const RiscvEnterUserCtx);
    }
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "riscv64")))]
pub fn enter_dispatched_user_task_if_available(
    _kernel: &crate::kernel::boot::KernelState,
    _dispatched_tid: Option<u64>,
) {
}

pub fn run_with_prepared_kernel(run: fn(&mut crate::kernel::boot::KernelState)) {
    // Use the in-place static initializer (writes BOOTSTRAP_KERNEL_STATE in
    // .bss and returns a &'static mut), exactly like x86_64/AArch64. The
    // boxed `Bootstrap::init()` path would `Box::new` the ~4.27 MiB bare-metal
    // KernelState out of the 1 MiB page-table frame pool and OOM (silent
    // panic-loop); init_static avoids the heap entirely.
    let kernel = crate::kernel::boot::Bootstrap::init_static().expect("kernel init");
    crate::yarm_log!(
        "YARM_BOOT_OK present_cpus={} present_bitmap=0x{:x} online_cpus={}",
        kernel.present_cpu_count(),
        kernel.present_cpu_bitmap(),
        kernel.online_cpu_count()
    );
    crate::yarm_log!("RISCV_KERNEL_BOOT_OK");
    run(kernel);
}

/// Tracks whether the RISC-V boot command line has already been captured.
///
/// The boot command line is owned by the firmware-provided DTB and must be
/// captured exactly once, from the boot hart, using the OpenSBI `a1` pointer.
/// If this function is ever re-entered (e.g. a pre-kernel fault restarts the
/// payload entry before the early trap vector is in force), a later
/// missing-DTB capture must NOT clobber the already-valid command line.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static RISCV64_CMDLINE_CAPTURED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn prepare_arch_boot(start_info_ptr: usize) {
    // Monotonic, capture-once guard: the first call wins and records the
    // command line; any subsequent call is a no-op that preserves it. This is
    // the single source of truth for "the cmdline is captured", so a re-entry
    // with a missing/garbage DTB pointer can never replace a valid cmdline
    // with an empty one (requirement: no missing-DTB overwrite loop).
    if RISCV64_CMDLINE_CAPTURED.swap(true, core::sync::atomic::Ordering::AcqRel) {
        early_marker!("RISCV_CMDLINE_PRESERVED reason=missing_dtb_after_valid");
        return;
    }

    let Some(dtb) = dtb_slice_from_start_info(start_info_ptr) else {
        // DTB parse failed on the one-and-only capture. Record an empty
        // command line once and report the precise reason; the boot hart can
        // still proceed (the QEMU `-append` may be absent).
        early_marker!(
            "RISCV_DTB_PARSE_FAILED reason=bad_magic_or_size ptr=0x{:x}",
            start_info_ptr
        );
        let captured = crate::kernel::boot_command_line::set_raw_cmdline_from_bytes_monotonic(&[]);
        crate::yarm_log!(
            "YARM_BOOT_CMDLINE_CAPTURE arch=riscv64 len={} truncated={} source=missing_dtb",
            captured.raw_cmdline().len(),
            captured.cmdline_was_truncated() as u8
        );
        early_marker!(
            "RISCV_CMDLINE_CAPTURE_ONCE len={}",
            captured.raw_cmdline().len()
        );
        return;
    };
    let captured = crate::kernel::boot_command_line::set_raw_cmdline_from_bytes_monotonic(
        crate::arch::fdt::chosen_bootargs(dtb).unwrap_or(&[]),
    );
    crate::yarm_log!(
        "YARM_BOOT_CMDLINE_CAPTURE arch=riscv64 len={} truncated={}",
        captured.raw_cmdline().len(),
        captured.cmdline_was_truncated() as u8
    );
    early_marker!(
        "RISCV_CMDLINE_CAPTURE_ONCE len={}",
        captured.raw_cmdline().len()
    );

    // Stage the real RAM window and reserve firmware/DTB/initrd so the frame
    // allocator never hands out MMIO or firmware memory. Without this the
    // common fallback memory map (NEXT_ANON_PHYS_BASE = 0x1000_0000, the QEMU
    // virt UART region, which is BELOW RAM at 0x8000_0000) seeded the
    // allocators with MMIO addresses, producing a store access fault when the
    // first frame was written.
    stage_riscv64_boot_memory(dtb, start_info_ptr);
}

/// Stages the real RAM window from the DTB and reserves the firmware, DTB, and
/// initramfs regions for the frame allocator. RISC-V-specific; mirrors what the
/// PVH (x86_64) and DTB (AArch64) paths already do for their allocators.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn stage_riscv64_boot_memory(dtb: &'static [u8], dtb_ptr: usize) {
    let page = crate::kernel::vm::PAGE_SIZE as u64;

    let Some((ram_base, ram_size)) = crate::arch::fdt::memory_reg(dtb) else {
        // Fail closed: without a trustworthy RAM window we must not guess, or
        // the allocator could stomp MMIO/firmware. The kernel image range is
        // still reserved by default_reserved_ranges; bootstrap will then fail
        // deterministically rather than corrupt memory.
        early_marker!("RISCV_MEM_PARSE_FAILED reason=no_memory_reg");
        return;
    };
    let ram_end = ram_base.saturating_add(ram_size);
    early_marker!(
        "RISCV_MEM_RAM base=0x{:x} size=0x{:x} end=0x{:x}",
        ram_base,
        ram_size,
        ram_end
    );
    let _ = crate::arch::boot_entry::stage_detected_ram_for_bootstrap(&[
        crate::kernel::frame_allocator::MemoryRegion {
            start: ram_base,
            len: ram_size,
            usable: true,
        },
    ]);

    let mut reserved: [(u64, u64); MAX_BOOT_EXTRA_RESERVED_RISCV] =
        [(0, 0); MAX_BOOT_EXTRA_RESERVED_RISCV];
    let mut reserved_len = 0usize;

    // Firmware (OpenSBI) occupies RAM from ram_base up to the kernel image.
    let kernel_start = (core::ptr::addr_of!(__kernel_start) as u64) & !(page - 1);
    if kernel_start > ram_base {
        reserved[reserved_len] = (ram_base, kernel_start);
        reserved_len += 1;
        early_marker!(
            "RISCV_MEM_RESERVE_FIRMWARE start=0x{:x} end=0x{:x}",
            ram_base,
            kernel_start
        );
    }

    // The DTB blob itself (we are still reading it; reserve so it survives).
    let dtb_start = (dtb_ptr as u64) & !(page - 1);
    let dtb_end = ((dtb_ptr as u64).saturating_add(dtb.len() as u64) + (page - 1)) & !(page - 1);
    if dtb_end > dtb_start && reserved_len < reserved.len() {
        reserved[reserved_len] = (dtb_start, dtb_end);
        reserved_len += 1;
        early_marker!(
            "RISCV_MEM_RESERVE_DTB start=0x{:x} end=0x{:x}",
            dtb_start,
            dtb_end
        );
    }

    // The initramfs, located via /chosen. Register it for later ELF loading and
    // reserve its frames.
    if let Some((initrd_start, initrd_end)) = crate::arch::fdt::chosen_initrd(dtb) {
        let initrd_len = initrd_end.saturating_sub(initrd_start) as usize;
        if initrd_len > 0 {
            // SAFETY: the DTB-provided initrd window is immutable boot memory
            // inside the RAM region staged above.
            let bytes =
                unsafe { core::slice::from_raw_parts(initrd_start as *const u8, initrd_len) };
            crate::kernel::boot::Bootstrap::install_boot_initrd_bytes(bytes);
        }
        let initrd_pa_start = initrd_start & !(page - 1);
        let initrd_pa_end = (initrd_end + (page - 1)) & !(page - 1);
        if initrd_pa_end > initrd_pa_start && reserved_len < reserved.len() {
            reserved[reserved_len] = (initrd_pa_start, initrd_pa_end);
            reserved_len += 1;
        }
        early_marker!(
            "RISCV_MEM_RESERVE_INITRD start=0x{:x} end=0x{:x} len=0x{:x}",
            initrd_start,
            initrd_end,
            initrd_len
        );
    } else {
        early_marker!("RISCV_INITRD_ABSENT reason=no_chosen_initrd");
    }

    crate::kernel::boot::Bootstrap::install_boot_extra_reserved_ranges(&reserved[..reserved_len]);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const MAX_BOOT_EXTRA_RESERVED_RISCV: usize = 4;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn dtb_slice_from_start_info(start_info_ptr: usize) -> Option<&'static [u8]> {
    if start_info_ptr == 0 {
        return None;
    }
    let magic_be = unsafe { core::ptr::read_unaligned(start_info_ptr as *const u32) };
    if u32::from_be(magic_be) != 0xd00dfeed {
        return None;
    }
    let total_size_be = unsafe { core::ptr::read_unaligned((start_info_ptr + 4) as *const u32) };
    let total_size = u32::from_be(total_size_be) as usize;
    if !(40..=2 * 1024 * 1024).contains(&total_size) {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(start_info_ptr as *const u8, total_size) })
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "riscv64")))]
pub fn prepare_arch_boot(_start_info_ptr: usize) {}

pub fn emit_panic(_info: &core::panic::PanicInfo<'_>) {}
